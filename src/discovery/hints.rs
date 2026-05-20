//! Cheap local signals: NIC-adjacent files in /proc and /etc, environment
//! variables, DNS lookups.
//!
//! Linux is the primary deployment target. The /proc-backed helpers silently
//! return empty on non-Linux — the rest of discovery still works.

use std::collections::HashSet;
use std::net::IpAddr;
#[cfg(target_os = "linux")]
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use tracing::debug;

#[cfg(target_os = "linux")]
use super::ENGINE_PORTS;
use super::resolver;

/// Read `/etc/hosts`, return IPs whose hostname or alias contains a DB-ish
/// substring. Matching is loose because hostnames vary wildly across customer
/// environments; the operator confirms the candidate later via the UI.
pub fn etc_hosts_db_labels() -> Vec<IpAddr> {
    let Ok(text) = std::fs::read_to_string("/etc/hosts") else {
        return vec![];
    };
    let mut out = Vec::new();
    let needles = db_name_needles();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(ip_str) = parts.next() else { continue };
        let names: Vec<&str> = parts.collect();
        if !names.iter().any(|n| name_matches(n, &needles)) {
            continue;
        }
        if let Ok(ip) = ip_str.parse::<IpAddr>() {
            out.push(ip);
        }
    }
    debug!(count = out.len(), "/etc/hosts DB-like entries");
    out
}

/// Parse `/etc/resolv.conf` for search domains via the `resolv-conf` crate.
pub fn resolv_search_domains() -> Vec<String> {
    let Ok(bytes) = std::fs::read("/etc/resolv.conf") else {
        return vec![];
    };
    let Ok(cfg) = resolv_conf::Config::parse(&bytes) else {
        return vec![];
    };
    let out: Vec<String> = cfg
        .get_search()
        .map(|list| list.iter().map(|d| d.to_string()).collect())
        .unwrap_or_default();
    debug!(count = out.len(), "resolv.conf search domains");
    out
}

/// Resolve `<label>.<domain>` for every (label, domain) combination via the
/// shared hickory resolver.
pub async fn dns_label_resolve(labels: &[String], domains: &[String]) -> Vec<IpAddr> {
    let mut seen: HashSet<IpAddr> = HashSet::new();
    let mut tasks = Vec::new();
    for label in labels {
        for domain in domains {
            let name = format!("{label}.{domain}");
            tasks.push(tokio::spawn(async move { resolver::forward(&name).await }));
        }
        // Also probe the bare label — covers single-label search resolution.
        let bare = label.clone();
        tasks.push(tokio::spawn(async move { resolver::forward(&bare).await }));
    }
    for t in tasks {
        if let Ok(ips) = t.await {
            for ip in ips {
                if !ip_is_uninteresting(&ip) {
                    seen.insert(ip);
                }
            }
        }
    }
    let out: Vec<IpAddr> = seen.into_iter().collect();
    debug!(count = out.len(), "dns hint resolves");
    out
}

/// Read TCP listening sockets via `procfs`. Return (port, engine) for every
/// listener that matches one of our supported engine ports.
///
/// Linux-only. On other targets returns an empty vec; the orchestrator's
/// explicit localhost probe loop still catches local DBs.
#[cfg(target_os = "linux")]
pub fn proc_net_listeners() -> Vec<(u16, &'static str)> {
    use procfs::net::TcpState;
    let mut ports: HashSet<u16> = HashSet::new();
    for table in [procfs::net::tcp(), procfs::net::tcp6()] {
        let Ok(entries) = table else { continue };
        for entry in entries {
            if entry.state == TcpState::Listen {
                ports.insert(entry.local_address.port());
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (engine, port) in ENGINE_PORTS {
        if ports.contains(port) && seen.insert((*engine, *port)) {
            out.push((*port, *engine));
        }
    }
    debug!(count = out.len(), "proc_net DB-port listeners");
    out
}

#[cfg(not(target_os = "linux"))]
pub fn proc_net_listeners() -> Vec<(u16, &'static str)> {
    Vec::new()
}

/// ARP neighbours via `procfs`. Incomplete entries (no flags) are skipped.
/// Linux-only; macOS/Windows return an empty vec.
#[cfg(target_os = "linux")]
pub fn arp_neighbours() -> Vec<IpAddr> {
    let Ok(entries) = procfs::net::arp() else {
        return vec![];
    };
    let out: Vec<IpAddr> = entries
        .into_iter()
        .filter(|e| !e.flags.is_empty())
        .map(|e| IpAddr::V4(e.ip_address))
        .collect();
    debug!(count = out.len(), "arp neighbours");
    out
}

#[cfg(not(target_os = "linux"))]
pub fn arp_neighbours() -> Vec<IpAddr> {
    Vec::new()
}

/// Default-gateway IP plus a handful of fixed offsets (operators routinely
/// place the primary DB at `.2` or similar). Linux-only.
#[cfg(target_os = "linux")]
pub fn gateway_ips() -> Vec<IpAddr> {
    let Ok(routes) = procfs::net::route() else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in routes {
        if r.destination != Ipv4Addr::UNSPECIFIED {
            continue;
        }
        if r.gateway == Ipv4Addr::UNSPECIFIED {
            continue;
        }
        out.push(IpAddr::V4(r.gateway));
        let n = u32::from(r.gateway);
        for delta in [1u32, 2, 10, 100] {
            out.push(IpAddr::V4(Ipv4Addr::from(n.wrapping_add(delta))));
        }
    }
    debug!(count = out.len(), "gateway-derived IPs");
    out
}

#[cfg(not(target_os = "linux"))]
pub fn gateway_ips() -> Vec<IpAddr> {
    Vec::new()
}

/// Non-default routes that point at subnets not bound directly to any NIC —
/// typically WireGuard/OpenVPN/tunnel interfaces whose AllowedIPs only show
/// up here. Linux-only.
#[cfg(target_os = "linux")]
pub fn routed_subnets() -> Vec<(Ipv4Net, String)> {
    let Ok(routes) = procfs::net::route() else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in routes {
        // Default route — handled by gateway_ips.
        if r.destination == Ipv4Addr::UNSPECIFIED {
            continue;
        }
        let prefix = u32::from(r.mask).count_ones() as u8;
        // /32 host routes aren't a subnet to enumerate.
        if prefix == 0 || prefix >= 32 {
            continue;
        }
        let Ok(net) = Ipv4Net::new(r.destination, prefix) else {
            continue;
        };
        out.push((net.trunc(), r.iface));
    }
    debug!(count = out.len(), "routed (non-default) subnets");
    out
}

#[cfg(not(target_os = "linux"))]
pub fn routed_subnets() -> Vec<(Ipv4Net, String)> {
    Vec::new()
}

/// Inspect environment variables that conventionally point at a DB. Best-
/// effort URL parsing; non-URL values are tried as bare hostnames.
pub fn env_db_hosts() -> Vec<IpAddr> {
    const VARS: &[&str] = &[
        "DATABASE_URL",
        "POSTGRES_HOST",
        "PGHOST",
        "MYSQL_HOST",
        "MARIADB_HOST",
        "MSSQL_HOST",
        "MONGO_HOST",
        "MONGODB_URI",
        "CLICKHOUSE_HOST",
        "CLICKHOUSE_URL",
    ];
    let mut out = Vec::new();
    for var in VARS {
        let Ok(val) = std::env::var(var) else { continue };
        if let Some(host) = extract_host(&val) {
            if let Ok(ip) = host.parse::<IpAddr>() {
                out.push(ip);
            }
            // DNS names are caught by the L1 DNS-label pass if they match
            // a well-known label; we don't auto-resolve arbitrary env-var
            // hostnames here because the budget cost-to-yield ratio is bad.
        }
    }
    debug!(count = out.len(), "env-derived IPs");
    out
}

fn extract_host(val: &str) -> Option<String> {
    if let Ok(url) = url::Url::parse(val) {
        return url.host_str().map(|s| s.to_string());
    }
    if let Some((host, _)) = val.split_once(':') {
        if !host.is_empty() {
            return Some(host.to_string());
        }
    }
    if !val.is_empty() {
        return Some(val.to_string());
    }
    None
}

fn db_name_needles() -> &'static [&'static str] {
    &[
        "db",
        "database",
        "postgres",
        "mysql",
        "mariadb",
        "mssql",
        "sqlserver",
        "mongo",
        "clickhouse",
    ]
}

fn name_matches(name: &str, needles: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    needles.iter().any(|n| lower.contains(n))
}

fn ip_is_uninteresting(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback() || v.is_unspecified(),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_host_url() {
        assert_eq!(
            extract_host("postgres://u:p@db.internal:5432/app"),
            Some("db.internal".to_string())
        );
    }

    #[test]
    fn extract_host_hostport() {
        assert_eq!(extract_host("10.0.0.5:5432"), Some("10.0.0.5".to_string()));
    }

    #[test]
    fn extract_host_bare() {
        assert_eq!(extract_host("dbhost"), Some("dbhost".to_string()));
    }

    #[test]
    fn name_matching() {
        let needles = db_name_needles();
        assert!(name_matches("postgres-prod", needles));
        assert!(name_matches("DB-Master", needles));
        assert!(!name_matches("web-frontend", needles));
    }
}
