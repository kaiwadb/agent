//! Per-engine handshake fingerprint.
//!
//! Each engine's earliest unauthenticated exchange is enough to confirm "yes
//! this is engine X" and often to grab a version string for free.
//!
//! - **Postgres**: send SSLRequest, server responds with one byte `'S'`,
//!   `'N'`, or an ErrorResponse. None of those identifies postgres uniquely
//!   on the byte itself, but they collectively distinguish postgres from
//!   anything else listening on 5432. Version comes from a second exchange
//!   (StartupMessage with bogus creds → ErrorResponse carries version).
//!   For speed and to avoid noisy auth logs we only do the SSLRequest probe
//!   and leave `version_hint` empty.
//! - **MySQL / MariaDB**: server sends a greeting unsolicited on connect. The
//!   protocol version byte (`0x0a`) and a null-terminated version string land
//!   in the first few hundred bytes. MariaDB advertises itself in the version
//!   string (e.g. `"5.5.5-10.11.7-MariaDB-…"`), so a single handshake
//!   distinguishes the two. Free fingerprint + version.
//! - **MSSQL**: send a TDS prelogin packet. Response includes version bytes
//!   in the PRELOGIN_VERSION token.
//! - **MongoDB**: send `OP_MSG` with a `hello` command. Response is a BSON
//!   doc with the `version` field.
//! - **ClickHouse**: HTTP GET `/?query=SELECT%20version()`. Body is the
//!   version. Native protocol on 9000 is not fingerprinted (the operator
//!   confirms manually).

use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::trace;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(800);

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(HANDSHAKE_TIMEOUT)
        .build()
        .expect("failed to build fingerprint HTTP client")
});

pub async fn fingerprint(
    stream: TcpStream,
    ip: IpAddr,
    port: u16,
    expected_engine: &str,
) -> (&'static str, Option<String>) {
    let result = timeout(
        HANDSHAKE_TIMEOUT,
        dispatch(stream, ip, port, expected_engine),
    )
    .await;
    match result {
        Ok(Ok(v)) => v,
        _ => (engine_static_name(expected_engine), None),
    }
}

async fn dispatch(
    stream: TcpStream,
    ip: IpAddr,
    port: u16,
    expected_engine: &str,
) -> std::io::Result<(&'static str, Option<String>)> {
    match expected_engine {
        "postgres" => postgres(stream).await,
        // MySQL and MariaDB share the wire protocol — one handler parses
        // both, then reports whichever the server actually is.
        "mysql" | "mariadb" => mysql(stream).await,
        "mssql" => mssql(stream).await,
        "mongo" => mongo(stream).await,
        "clickhouse" => clickhouse(stream, ip, port).await,
        _ => Ok(("unknown", None)),
    }
}

fn engine_static_name(s: &str) -> &'static str {
    match s {
        "postgres" => "postgres",
        "mysql" => "mysql",
        "mariadb" => "mariadb",
        "mssql" => "mssql",
        "mongo" => "mongo",
        "clickhouse" => "clickhouse",
        _ => "unknown",
    }
}

/// Postgres SSLRequest packet: `[length=8][code=80877103]`, both big-endian.
async fn postgres(mut s: TcpStream) -> std::io::Result<(&'static str, Option<String>)> {
    let mut pkt = [0u8; 8];
    pkt[0..4].copy_from_slice(&8u32.to_be_bytes());
    pkt[4..8].copy_from_slice(&80877103u32.to_be_bytes());
    s.write_all(&pkt).await?;
    let mut resp = [0u8; 1];
    s.read_exact(&mut resp).await?;
    match resp[0] {
        b'S' | b'N' | b'E' => Ok(("postgres", None)),
        _ => Ok(("unknown", None)),
    }
}

async fn mysql(mut s: TcpStream) -> std::io::Result<(&'static str, Option<String>)> {
    // Read up to 256 bytes of the greeting. Layout: [3 bytes length][1 byte
    // seq id][payload]. Payload starts with protocol version byte (0x0a for
    // protocol 10), then a null-terminated server version string. MariaDB
    // ≥ 10.2 prefixes the version with `5.5.5-` for compat with old MySQL
    // clients that hard-cap at 5.x; the real version + `-MariaDB` suffix
    // sits in the second segment (`5.5.5-10.11.7-MariaDB-…`), so a
    // case-insensitive substring check on the greeting is enough.
    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).await?;
    if n < 6 {
        return Ok(("unknown", None));
    }
    let payload_start = 4;
    if buf[payload_start] != 0x0a {
        return Ok(("unknown", None));
    }
    let vs = &buf[payload_start + 1..n];
    let end = vs.iter().position(|b| *b == 0).unwrap_or(vs.len());
    let version = std::str::from_utf8(&vs[..end]).ok().map(|v| v.to_string());
    let engine = classify_mysql_family(version.as_deref());
    trace!(?version, engine, "mysql-family greeting parsed");
    Ok((engine, version))
}

fn classify_mysql_family(version: Option<&str>) -> &'static str {
    match version {
        Some(v) if v.to_ascii_lowercase().contains("mariadb") => "mariadb",
        _ => "mysql",
    }
}

async fn mssql(mut s: TcpStream) -> std::io::Result<(&'static str, Option<String>)> {
    // Minimal TDS prelogin packet. Headers + a single option (VERSION, type 0)
    // pointing at 6 bytes of zeros, then a TERMINATOR (0xff).
    //
    //  byte 0    : packet type   (0x12 = prelogin)
    //  byte 1    : status        (0x01 = EOM)
    //  bytes 2-3 : length        (big-endian total packet length)
    //  bytes 4-5 : SPID          (0)
    //  byte  6   : packet id     (1)
    //  byte  7   : window        (0)
    //  then options stream: VERSION(0), TERMINATOR(0xff)
    //
    // We send just enough to elicit a prelogin response which carries the
    // server's own VERSION token (4 bytes major.minor.build + 2 bytes
    // sub-build).
    let body: &[u8] = &[
        0x00, // VERSION option type
        0x00, 0x06, // offset (relative to start of option data) - we'll fix
        0x00, 0x06, // length
        0xff, // TERMINATOR
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 6 bytes VERSION payload (all zero)
    ];
    let total_len = 8 + body.len();
    let mut pkt: Vec<u8> = Vec::with_capacity(total_len);
    pkt.push(0x12);
    pkt.push(0x01);
    pkt.extend_from_slice(&(total_len as u16).to_be_bytes());
    pkt.extend_from_slice(&[0, 0, 1, 0]);
    pkt.extend_from_slice(body);
    // Fix the option offset: data sits after the option stream (after the
    // TERMINATOR at index 5 of body). Header is 8 bytes; option-data offset
    // is from packet-start... wait, TDS spec says offset is from the start
    // of the prelogin *option stream*, not the packet. Option stream starts
    // right after the 8-byte header. Our option stream is:
    //   [VERSION][off=0x0006][len=0x0006][TERMINATOR][6 bytes data]
    // i.e. 5 bytes of options metadata, then 6 bytes data. Offset 6 (set
    // above) is wrong — should be 5. Patch:
    let _ = body; // (the immutable slice above)
    pkt[8 + 2] = 0x00;
    pkt[8 + 1] = 0x00;
    // re-write the offset bytes at body index 1..3 (overall packet 9..11):
    pkt[9] = 0x00;
    pkt[10] = 0x05;
    s.write_all(&pkt).await?;

    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).await?;
    if n < 8 || buf[0] != 0x04 {
        // 0x04 is the prelogin response packet type
        return Ok(("unknown", None));
    }
    // Walk the option stream to find VERSION (token 0x00).
    let mut i = 8;
    let mut version_off = None;
    let mut version_len = None;
    while i + 5 <= n {
        let token = buf[i];
        if token == 0xff {
            break;
        }
        let off = u16::from_be_bytes([buf[i + 1], buf[i + 2]]) as usize;
        let len = u16::from_be_bytes([buf[i + 3], buf[i + 4]]) as usize;
        if token == 0x00 {
            version_off = Some(off);
            version_len = Some(len);
        }
        i += 5;
    }
    let opt_stream_start = 8;
    if let (Some(off), Some(len)) = (version_off, version_len) {
        let data_start = opt_stream_start + off;
        if data_start + len <= n && len >= 6 {
            let v = &buf[data_start..data_start + 6];
            let major = v[0];
            let minor = v[1];
            let build = u16::from_be_bytes([v[2], v[3]]);
            let sub = u16::from_be_bytes([v[4], v[5]]);
            return Ok((
                "mssql",
                Some(format!("{major}.{minor}.{build}.{sub}")),
            ));
        }
    }
    Ok(("mssql", None))
}

async fn mongo(mut s: TcpStream) -> std::io::Result<(&'static str, Option<String>)> {
    // OP_MSG (opcode 2013) carrying a section-kind-0 BSON doc `{hello:1, $db:"admin"}`.
    use std::io::Cursor;

    // Hand-rolled BSON for `{"hello": 1, "$db": "admin"}`.
    // BSON element: <type byte><cstring name><value>
    //   double=0x01 ... int32=0x10 ... string=0x02
    // Easier here to write a tiny BSON encoder for two known fields:
    let bson = {
        let mut body: Vec<u8> = Vec::new();
        // int32 "hello" = 1
        body.push(0x10);
        body.extend_from_slice(b"hello\0");
        body.extend_from_slice(&1i32.to_le_bytes());
        // string "$db" = "admin"
        body.push(0x02);
        body.extend_from_slice(b"$db\0");
        let db_val = "admin\0";
        body.extend_from_slice(&((db_val.len()) as i32).to_le_bytes());
        body.extend_from_slice(db_val.as_bytes());
        // terminator
        body.push(0x00);

        // Prepend the total doc length.
        let total = (body.len() + 4) as i32;
        let mut doc: Vec<u8> = Vec::with_capacity(total as usize);
        doc.extend_from_slice(&total.to_le_bytes());
        doc.extend_from_slice(&body);
        doc
    };

    // OP_MSG layout:
    //   int32 messageLength
    //   int32 requestID
    //   int32 responseTo
    //   int32 opCode (2013)
    //   uint32 flagBits (0)
    //   section: kind=0, body=bson
    let mut msg: Vec<u8> = Vec::new();
    let header_and_flags_len = 4 * 4 + 4; // 20
    let section_len = 1 + bson.len();
    let total_len = (header_and_flags_len + section_len) as i32;
    msg.extend_from_slice(&total_len.to_le_bytes());
    msg.extend_from_slice(&1i32.to_le_bytes()); // requestID
    msg.extend_from_slice(&0i32.to_le_bytes()); // responseTo
    msg.extend_from_slice(&2013i32.to_le_bytes());
    msg.extend_from_slice(&0u32.to_le_bytes()); // flags
    msg.push(0x00); // section kind
    msg.extend_from_slice(&bson);

    s.write_all(&msg).await?;

    // Read header (16 bytes), then payload of declared length.
    let mut hdr = [0u8; 16];
    s.read_exact(&mut hdr).await?;
    let reply_len = i32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
    if reply_len < 16 || reply_len > 1024 * 1024 {
        return Ok(("unknown", None));
    }
    let mut rest = vec![0u8; reply_len - 16];
    s.read_exact(&mut rest).await?;

    // Find string element "version" in the returned BSON section. We don't
    // need a full BSON parser — scan for the byte signature 0x02 "version\0".
    let needle: &[u8] = b"\x02version\0";
    let Some(pos) = find_subsequence(&rest, needle) else {
        return Ok(("mongo", None));
    };
    let str_start = pos + needle.len();
    if str_start + 4 > rest.len() {
        return Ok(("mongo", None));
    }
    let cur = Cursor::new(&rest[str_start..]);
    let mut bytes = cur.into_inner().iter().copied();
    let mut len_bytes = [0u8; 4];
    for slot in &mut len_bytes {
        *slot = bytes.next().unwrap_or(0);
    }
    let str_len = i32::from_le_bytes(len_bytes) as usize;
    if str_len == 0 || str_start + 4 + str_len > rest.len() {
        return Ok(("mongo", None));
    }
    let raw = &rest[str_start + 4..str_start + 4 + str_len];
    // Strip trailing NUL.
    let raw = raw.strip_suffix(b"\0").unwrap_or(raw);
    let version = std::str::from_utf8(raw).ok().map(|s| s.to_string());
    Ok(("mongo", version))
}

fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn clickhouse(
    s: TcpStream,
    ip: IpAddr,
    port: u16,
) -> std::io::Result<(&'static str, Option<String>)> {
    // Native plaintext: speak the Hello/Hello handshake directly.
    if port == 9000 {
        return clickhouse_native(s).await;
    }
    // Native TLS — would need a TLS handshake first. Leave as unknown;
    // the orchestrator dedups against any 8443 HTTP confirmation on the
    // same IP.
    if port == 9440 {
        drop(s);
        return Ok(("unknown", None));
    }

    drop(s);

    let scheme = if port == 8443 { "https" } else { "http" };
    let host = match ip {
        IpAddr::V4(v) => v.to_string(),
        IpAddr::V6(v) => format!("[{v}]"),
    };
    // /ping returns "Ok.\n" on any ClickHouse, regardless of auth config.
    let ping_url = format!("{scheme}://{host}:{port}/ping");
    let ping = HTTP.get(&ping_url).send().await;
    let is_clickhouse = match &ping {
        Ok(resp) => {
            resp.status().is_success()
                || resp
                    .headers()
                    .get("X-ClickHouse-Server-Display-Name")
                    .is_some()
        }
        Err(_) => false,
    };
    if !is_clickhouse {
        return Ok(("unknown", None));
    }

    // Try to grab a version via SELECT version(). On secured clusters this
    // will 401; that's fine — we still know it's ClickHouse.
    let q_url = format!("{scheme}://{host}:{port}/?query=SELECT%20version()");
    let version = match HTTP.get(&q_url).send().await {
        Ok(resp) if resp.status().is_success() => resp.text().await.ok().map(|t| t.trim().to_string()),
        _ => None,
    };
    Ok(("clickhouse", version))
}

/// ClickHouse native protocol Hello/Hello handshake.
///
/// Wire format is LEB128-style varuints for ints and (varuint length, bytes)
/// for strings. Client Hello carries name/version/revision/db/user/password;
/// server responds with packet type 0 (Hello) carrying its own name and
/// version. A non-zero packet type means we're talking to something else.
async fn clickhouse_native(mut s: TcpStream) -> std::io::Result<(&'static str, Option<String>)> {
    // Conservative protocol revision known to be supported widely. The
    // server advertises its own revision back; we don't actually use this
    // value beyond passing the handshake.
    const CLIENT_REVISION: u64 = 54429;

    let mut req: Vec<u8> = Vec::new();
    write_varuint(&mut req, 0); // packet: Hello
    write_string(&mut req, "kaiwadb-tunnel-scan");
    write_varuint(&mut req, 1); // client major
    write_varuint(&mut req, 0); // client minor
    write_varuint(&mut req, CLIENT_REVISION);
    write_string(&mut req, ""); // default db
    write_string(&mut req, "default"); // user
    write_string(&mut req, ""); // password
    s.write_all(&req).await?;

    let mut buf = vec![0u8; 1024];
    let n = s.read(&mut buf).await?;
    if n == 0 {
        return Ok(("unknown", None));
    }
    buf.truncate(n);

    let mut cur = std::io::Cursor::new(&buf[..]);
    let packet_type = match read_varuint(&mut cur) {
        Ok(v) => v,
        Err(_) => return Ok(("unknown", None)),
    };
    if packet_type != 0 {
        return Ok(("unknown", None));
    }
    let server_name = read_string(&mut cur).unwrap_or_default();
    if !server_name.to_ascii_lowercase().contains("clickhouse") {
        return Ok(("unknown", None));
    }
    let major = read_varuint(&mut cur).unwrap_or(0);
    let minor = read_varuint(&mut cur).unwrap_or(0);
    let _revision = read_varuint(&mut cur).unwrap_or(0);
    // Optional fields: timezone (since rev 54058), display name (54372),
    // version_patch (54401). Try to read version_patch best-effort.
    let _ = read_string(&mut cur);
    let _ = read_string(&mut cur);
    let patch = read_varuint(&mut cur).ok();

    let version = match patch {
        Some(p) => format!("{major}.{minor}.{p}"),
        None => format!("{major}.{minor}"),
    };
    Ok(("clickhouse", Some(version)))
}

fn write_varuint(out: &mut Vec<u8>, mut val: u64) {
    while val >= 0x80 {
        out.push(((val as u8) & 0x7f) | 0x80);
        val >>= 7;
    }
    out.push(val as u8);
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    write_varuint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn read_varuint(cur: &mut std::io::Cursor<&[u8]>) -> std::io::Result<u64> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        let mut byte = [0u8; 1];
        std::io::Read::read_exact(cur, &mut byte)?;
        let b = byte[0];
        result |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "varuint too large",
            ));
        }
    }
}

fn read_string(cur: &mut std::io::Cursor<&[u8]>) -> std::io::Result<String> {
    let len = read_varuint(cur)? as usize;
    if len > 4096 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "string too long",
        ));
    }
    let mut buf = vec![0u8; len];
    std::io::Read::read_exact(cur, &mut buf)?;
    String::from_utf8(buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::classify_mysql_family;

    #[test]
    fn mariadb_greeting_disambiguates() {
        // Real 10.11 greeting shape: leading `5.5.5-` compat prefix, then the
        // actual version + `-MariaDB-<suffix>`.
        assert_eq!(
            classify_mysql_family(Some("5.5.5-10.11.7-MariaDB-1:10.11.7+maria~ubu2204")),
            "mariadb",
        );
        assert_eq!(classify_mysql_family(Some("10.4.28-MariaDB")), "mariadb");
    }

    #[test]
    fn mysql_greeting_stays_mysql() {
        assert_eq!(classify_mysql_family(Some("8.0.34")), "mysql");
        assert_eq!(classify_mysql_family(Some("5.7.42-log")), "mysql");
    }

    #[test]
    fn empty_greeting_falls_back_to_mysql() {
        assert_eq!(classify_mysql_family(None), "mysql");
        assert_eq!(classify_mysql_family(Some("")), "mysql");
    }
}
