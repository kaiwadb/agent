# Tunnel wire protocol — scan extension

Versioned: tunnel `0.7.0+`. Backwards-compatible additive change on top of the existing query protocol.

The tunnel speaks JSON over a single WebSocket. The connection is initiated by the tunnel; the server then drives all activity by sending **commands**, each tagged with a numeric `id` (the originating row in the server's `tunnel_commands` table). The tunnel echoes that `id` back in every reply so the server can correlate without an in-process routing table.

This document covers the **`scan` command**, used to ask the tunnel to discover candidate databases reachable from its deployment vantage point. Designed for onboarding UX: ship the tunnel + auth token, the server requests one scan, the UI fills with candidate endpoints, an operator only has to enter credentials.

---

## Transport

WebSocket text frames. JSON only. One JSON value per frame.

Headers on the WebSocket upgrade request:

```
Authorization: Bearer <token>
```

Server may evict an older connection by sending close code `4001`. The tunnel reconnects with a 30s backoff in that case (existing behaviour, unchanged).

Server pings via a periodic command of its choice are unnecessary: the tunnel sends application-level pings every 15s (existing behaviour).

---

## Frame shapes (server → tunnel)

```json
{
  "type": "command",
  "id": 42,
  "request": { "<Variant>": <variant-payload> }
}
```

`request` is an externally-tagged Rust enum. Existing variant:

- `Query` — execute a SQL/Mongo statement against a configured connection.

New variant (this doc):

- `Scan` — discover reachable databases.

Unknown variants are not silently ignored; the tunnel currently rejects unparseable messages with a warning log. Server **must not** send a variant unsupported by the tunnel version it negotiated with. The tunnel advertises its semver in the `User-Agent` header on the WebSocket upgrade — version-gate scan dispatch on that.

### `Scan` command

```json
{
  "type": "command",
  "id": 42,
  "request": {
    "Scan": {
      "timeout_ms": 30000,
      "max_prefix": 22,
      "extra_cidrs": [],
      "include_dns_labels": null,
      "include_search_domains": null
    }
  }
}
```

All fields **optional**. Defaults shown below.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `timeout_ms` | `u32` | `30000` | Hard wall-clock budget for the entire scan. On timeout the tunnel returns whatever it has so far with the timed-out flag set. Clamped to `[1000, 120000]`. |
| `max_prefix` | `u8` | `22` | Smallest IPv4 prefix length the tunnel will fully enumerate (i.e. `/22` = 1024 hosts). Subnets with a smaller prefix (larger host count) are reported in `subnets_skipped`. Clamped to `[16, 32]`. |
| `extra_cidrs` | `string[]` | `[]` | Operator-supplied CIDRs to scan in addition to NIC-bound subnets. Same `max_prefix` cap applies. IPv4 only; IPv6 entries are rejected with an error in the report. |
| `include_dns_labels` | `string[]` \| `null` | built-in list | Extra DNS labels to probe against search domains (see L1 below). `null` means use the default list. |
| `include_search_domains` | `string[]` \| `null` | from `/etc/resolv.conf` | Override search domains for DNS hint phase. `null` means auto-detect. |

---

## Frame shapes (tunnel → server)

Two terminal replies per command. **Always one or the other**, never both.

### Success

```json
{
  "type": "result",
  "id": 42,
  "payload": { "ScanResult": <ScanReport> }
}
```

### Failure

```json
{
  "type": "error",
  "id": 42,
  "error": "scan disabled by operator"
}
```

`error` is a human-readable string. Use specific prefixes the server can match on if needed:

- `scan disabled by operator` — tunnel was launched with `--no-scan` or `KAIWADB_TUNNEL_DISABLE_SCAN=1`.
- `scan timed out` — wall-clock budget hit *before any candidate found*. (If at least one candidate was found, the tunnel returns `ScanResult` with `timed_out: true` instead of an error.)
- Anything else — unexpected internal failure.

---

## `ScanReport`

```json
{
  "candidates": [
    {
      "ip": "10.0.4.23",
      "hostname": "db-prod.internal",
      "port": 5432,
      "engine": "postgres",
      "version_hint": "15.4",
      "sources": ["subnet_scan", "arp"]
    }
  ],
  "subnets_scanned": ["10.0.4.0/24", "192.168.1.0/24"],
  "subnets_skipped": [
    { "cidr": "10.0.0.0/8", "reason": "too_large", "prefix": 8 }
  ],
  "search_domains": ["corp.internal", "example.com"],
  "interfaces": [
    { "name": "eth0", "ip": "10.0.4.5", "prefix": 24, "kind": "private" }
  ],
  "duration_ms": 12345,
  "timed_out": false
}
```

### `Candidate`

| Field | Type | Notes |
|---|---|---|
| `ip` | string | IPv4 or IPv6 (currently always IPv4 since enumeration is IPv4-only). |
| `hostname` | string \| null | Reverse-DNS of `ip` if it resolves within budget; else `null`. |
| `port` | u16 | The TCP port the engine answered on. |
| `engine` | string | One of `postgres`, `mysql`, `mssql`, `mongo`, `clickhouse`, `unknown`. `unknown` is only emitted for known-unfingerprintable ports — currently ClickHouse's native binary protocol on `9000`/`9440`. Any other open port that fails the per-engine handshake is dropped (it's almost always an unrelated service answering on a coincidentally-shared port, not a misconfigured DB). |
| `version_hint` | string \| null | Server-version string from the handshake (e.g. `"15.4"`, `"8.0.36"`). `null` if the engine doesn't volunteer one or fingerprinting failed. |
| `sources` | string[] | Where the tunnel learned about this candidate. Multi-valued (a candidate often comes from several signals). See "Source labels" below. |

### `SkippedSubnet`

| Field | Type | Notes |
|---|---|---|
| `cidr` | string | The CIDR that was skipped. |
| `reason` | string | `too_large`, `link_local`, `loopback`, `ipv6_unsupported`, `invalid`, `public` (public ranges never enumerated). |
| `prefix` | u8 | Prefix length, redundant with `cidr` for convenience. |

### `InterfaceInfo`

| Field | Type | Notes |
|---|---|---|
| `name` | string | OS-reported NIC name (`eth0`, `en0`, ...). |
| `ip` | string | The NIC's own IP. |
| `prefix` | u8 | Prefix length. |
| `kind` | string | `private`, `loopback`, `link_local`, `public`, `ipv6`. |

### Source labels

The tunnel may attribute a candidate to any of:

- `arp` — `/proc/net/arp` cache had this IP.
- `etc_hosts` — `/etc/hosts` mapped a DB-like label to this IP.
- `dns` — DNS search-domain × known-label probe resolved here.
- `proc_net` — local TCP listener found on a DB default port.
- `gateway` — derived from the default route.
- `subnet_scan` — found by enumerating a NIC-bound subnet.
- `routed` — found by enumerating a non-NIC subnet from the routing table (typically WireGuard/OpenVPN/tunnel `AllowedIPs`).
- `env` — appeared in `DATABASE_URL`/`PGHOST`/etc. environment vars.
- `extra_cidr` — server-supplied CIDR in `extra_cidrs`.

---

## Semantics

- The tunnel **never** initiates a scan on its own. Scanning happens only in response to a `Scan` command. The server is the only entity that decides when to scan, what to skip, and how to surface results to the UI. The tunnel just executes.
- The scan is **non-authenticated**: the tunnel only performs unauthenticated TCP connect + protocol handshake. No credentials, no SQL. A candidate appearing in the report does **not** mean the tunnel has access to it; that's a separate cred-validation step driven by a future `Query` command.
- The scan is **read-only** with respect to the customer's environment: it opens TCP connections (some short-lived) and may show up as port-scan noise in SIEM. The tunnel logs a scan plan + summary at `info` level so operators can audit.
- The scan is **bounded**: hard wall-clock budget, hard concurrency cap (256 in-flight sockets), per-probe timeout (400ms). The server can rely on the command returning within `timeout_ms + ~1s`.
- The scan is **idempotent** and may be re-issued any time. Subsequent scans don't preserve state between commands.
- A candidate is **dedup'd** on `(ip, port)`. Multiple signals collapse into one row with merged `sources`.
- Hostnames: the tunnel does reverse-DNS lookups but does *not* let them block the scan; reverse-DNS failures simply leave `hostname` as `null`.

## Ports probed by default

| Engine | Ports |
|---|---|
| Postgres | 5432 |
| MySQL | 3306, 3307 |
| MSSQL | 1433 |
| MongoDB | 27017, 27018, 27019 |
| ClickHouse | 8123 (HTTP), 9000 (native), 8443 (HTTP TLS), 9440 (native TLS) |

ClickHouse fingerprinting uses the HTTP endpoint; ports 9000/9440 are reported as `engine: unknown` for the operator to confirm.

## Operator opt-out

If the tunnel binary is launched with `--no-scan` or `KAIWADB_TUNNEL_DISABLE_SCAN=1`, any `Scan` command returns:

```json
{ "type": "error", "id": <id>, "error": "scan disabled by operator" }
```

The server should surface this in the UI ("scanning has been disabled by the operator of this tunnel") rather than treating it as a transient failure.

## Backwards compatibility

- Existing `Query` command is unchanged.
- Existing `QueryResult` payload shape is unchanged.
- Servers running an older protocol that never send `Scan` will see no behaviour change in the tunnel.
- A tunnel running this version that receives an unknown command variant still logs a warning and drops the frame (existing behaviour). Future server-side variants can be added the same way.

## Example: full round-trip

Server:

```json
{
  "type": "command",
  "id": 1,
  "request": {
    "Scan": { "timeout_ms": 20000, "max_prefix": 22 }
  }
}
```

Tunnel (after ~7s):

```json
{
  "type": "result",
  "id": 1,
  "payload": {
    "ScanResult": {
      "candidates": [
        {
          "ip": "10.0.4.23",
          "hostname": "db-prod.internal",
          "port": 5432,
          "engine": "postgres",
          "version_hint": "15.4",
          "sources": ["subnet_scan", "arp"]
        },
        {
          "ip": "10.0.4.24",
          "hostname": null,
          "port": 27017,
          "engine": "mongo",
          "version_hint": "7.0.5",
          "sources": ["subnet_scan"]
        },
        {
          "ip": "10.0.4.40",
          "hostname": "analytics.internal",
          "port": 8123,
          "engine": "clickhouse",
          "version_hint": "24.3.2.23",
          "sources": ["dns", "subnet_scan"]
        }
      ],
      "subnets_scanned": ["10.0.4.0/24"],
      "subnets_skipped": [
        { "cidr": "10.0.0.0/8", "reason": "too_large", "prefix": 8 }
      ],
      "search_domains": ["corp.internal"],
      "interfaces": [
        { "name": "eth0", "ip": "10.0.4.5", "prefix": 24, "kind": "private" }
      ],
      "duration_ms": 6873,
      "timed_out": false
    }
  }
}
```
