//! Database discovery.
//!
//! Layered approach. Each layer contributes IP/host candidates; a single
//! probe pass then TCP-connects each (candidate × engine-default-port) tuple
//! and fingerprints whatever answers. The whole thing is wall-clock-budgeted
//! and concurrency-capped so it stays bounded in any deployment.
//!
//! Layers:
//!
//! - L0 — free local signals (NICs, /etc/hosts, /proc/net, env, gateway).
//! - L1 — DNS hints (search domain × known DB labels). Catches managed DBs
//!   that are pure DNS endpoints (RDS, Atlas, ...).
//! - L2 — TCP probe targeted at the L0+L1 hosts × DB-default ports.
//! - L3 — full subnet enumeration on NIC subnets within `max_prefix`.
//! - L4 — protocol fingerprint on every open port.

mod fingerprint;
mod hints;
mod interfaces;
mod probe;
mod resolver;

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::Semaphore;
use tokio::time::{Duration, timeout};
use tracing::{debug, info, warn};

use crate::communication::{ScanRequest, ScanReport};

pub use interfaces::{InterfaceInfo, SkippedSubnet};

const DEFAULT_TIMEOUT_MS: u32 = 30_000;
const MIN_TIMEOUT_MS: u32 = 1_000;
const MAX_TIMEOUT_MS: u32 = 120_000;

const DEFAULT_MAX_PREFIX: u8 = 22;
const MIN_MAX_PREFIX: u8 = 16;
const MAX_MAX_PREFIX: u8 = 32;

const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const CONCURRENCY: usize = 256;

/// Default DNS labels probed against each search domain. Carefully tuned to
/// minimise false positives — labels here are ones a sysadmin would only ever
/// point at a database.
const DEFAULT_DNS_LABELS: &[&str] = &[
    "db",
    "database",
    "postgres",
    "postgresql",
    "pg",
    "mysql",
    "mariadb",
    "mssql",
    "sqlserver",
    "mongo",
    "mongodb",
    "clickhouse",
    "db-primary",
    "db-master",
    "db-prod",
    "db-staging",
];

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub ip: String,
    pub hostname: Option<String>,
    pub port: u16,
    pub engine: String,
    pub version_hint: Option<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Arp,
    EtcHosts,
    Dns,
    ProcNet,
    Gateway,
    SubnetScan,
    Env,
    ExtraCidr,
    Routed,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Arp => "arp",
            Source::EtcHosts => "etc_hosts",
            Source::Dns => "dns",
            Source::ProcNet => "proc_net",
            Source::Gateway => "gateway",
            Source::SubnetScan => "subnet_scan",
            Source::Env => "env",
            Source::ExtraCidr => "extra_cidr",
            Source::Routed => "routed",
        }
    }
}

/// (engine, port) pairs probed on every candidate IP.
pub(crate) const ENGINE_PORTS: &[(&str, u16)] = &[
    ("postgres", 5432),
    ("mysql", 3306),
    ("mysql", 3307),
    ("mssql", 1433),
    ("mongo", 27017),
    ("mongo", 27018),
    ("mongo", 27019),
    ("clickhouse", 8123),
    ("clickhouse", 8443),
    ("clickhouse", 9000),
    ("clickhouse", 9440),
];

pub async fn scan(req: ScanRequest) -> ScanReport {
    let started = Instant::now();
    let timeout_ms = req
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
    let max_prefix = req
        .max_prefix
        .unwrap_or(DEFAULT_MAX_PREFIX)
        .clamp(MIN_MAX_PREFIX, MAX_MAX_PREFIX);

    let budget = Duration::from_millis(timeout_ms as u64);
    info!(
        timeout_ms,
        max_prefix,
        extra_cidrs = req.extra_cidrs.len(),
        "starting discovery scan"
    );

    let work = run_scan(req, max_prefix);
    let (report, timed_out) = match timeout(budget, work).await {
        Ok(report) => (report, false),
        Err(_) => {
            warn!("scan exceeded wall-clock budget");
            (ScanReport::empty(), true)
        }
    };

    let mut report = report;
    report.duration_ms = started.elapsed().as_millis() as u64;
    report.timed_out = timed_out;

    info!(
        candidates = report.candidates.len(),
        subnets_scanned = report.subnets_scanned.len(),
        subnets_skipped = report.subnets_skipped.len(),
        duration_ms = report.duration_ms,
        timed_out = report.timed_out,
        "scan complete"
    );
    report
}

async fn run_scan(req: ScanRequest, max_prefix: u8) -> ScanReport {
    let labels: Vec<String> = req
        .include_dns_labels
        .unwrap_or_else(|| DEFAULT_DNS_LABELS.iter().map(|s| s.to_string()).collect());

    // L0 — interface enumeration + classification.
    let mut nic_view = interfaces::scan_interfaces(max_prefix, &req.extra_cidrs);

    // L0 — routed subnets (wg-prod, tun0, ...). Subtract anything already
    // covered by a NIC subnet so we don't double-count.
    let routed = hints::routed_subnets();
    let routed_nets: Vec<_> = routed.iter().map(|(net, _)| *net).collect();
    let routed_ips = interfaces::expand_routed(
        &routed_nets,
        &nic_view.nic_subnets,
        max_prefix,
        &mut nic_view.subnets_scanned,
        &mut nic_view.subnets_skipped,
    );

    // L0 — local hints (proc, arp, hosts, env, gateway). All cheap, run sync.
    let mut hint_set: BTreeMap<IpAddr, Vec<Source>> = BTreeMap::new();
    let local_listeners = hints::proc_net_listeners();
    for ip in hints::arp_neighbours() {
        hint_set.entry(ip).or_default().push(Source::Arp);
    }
    for ip in hints::etc_hosts_db_labels() {
        hint_set.entry(ip).or_default().push(Source::EtcHosts);
    }
    for ip in hints::gateway_ips() {
        hint_set.entry(ip).or_default().push(Source::Gateway);
    }
    for ip in hints::env_db_hosts() {
        hint_set.entry(ip).or_default().push(Source::Env);
    }

    let search_domains = req
        .include_search_domains
        .unwrap_or_else(hints::resolv_search_domains);

    // L1 — DNS hints.
    for ip in hints::dns_label_resolve(&labels, &search_domains).await {
        hint_set.entry(ip).or_default().push(Source::Dns);
    }

    // L3 — subnet enumeration. NIC subnets, routed (wg/vpn) subnets, and
    // operator-supplied extra CIDRs, all already cap-checked above.
    for ip in &nic_view.enumerated_ips {
        hint_set
            .entry(IpAddr::V4(*ip))
            .or_default()
            .push(Source::SubnetScan);
    }
    for ip in &routed_ips {
        hint_set
            .entry(IpAddr::V4(*ip))
            .or_default()
            .push(Source::Routed);
    }
    for ip in &nic_view.extra_cidr_ips {
        hint_set
            .entry(IpAddr::V4(*ip))
            .or_default()
            .push(Source::ExtraCidr);
    }

    debug!(
        unique_hosts = hint_set.len(),
        listeners = local_listeners.len(),
        "discovery targets compiled"
    );

    // L2 + L4 — TCP probe + fingerprint, concurrency-bounded.
    let sem = Arc::new(Semaphore::new(CONCURRENCY));
    let mut tasks = Vec::new();
    for (ip, sources) in hint_set {
        for (engine, port) in ENGINE_PORTS {
            let sem = sem.clone();
            let sources = sources.clone();
            let engine = (*engine).to_string();
            let port = *port;
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                probe::probe_endpoint(ip, port, &engine, &sources).await
            }));
        }
    }

    // Also probe localhost on the engine ports — picks up local DBs that
    // didn't show up via /proc/net/tcp parse (e.g. when running unprivileged).
    for (engine, port) in ENGINE_PORTS {
        let sem = sem.clone();
        let engine = (*engine).to_string();
        let port = *port;
        let sources = vec![Source::ProcNet];
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            probe::probe_endpoint(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port, &engine, &sources).await
        }));
    }

    // Pull out the also-listening info from proc and pre-seed: makes the
    // common "tunnel runs on the DB host" case zero-latency.
    for (port, engine) in local_listeners {
        let sem = sem.clone();
        let engine = engine.to_string();
        let sources = vec![Source::ProcNet];
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            probe::probe_endpoint(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port, &engine, &sources).await
        }));
    }

    let mut by_key: BTreeMap<(String, u16), Candidate> = BTreeMap::new();
    for task in tasks {
        let Ok(Some(found)) = task.await else { continue };
        let key = (found.ip.clone(), found.port);
        by_key
            .entry(key)
            .and_modify(|c| {
                for s in &found.sources {
                    if !c.sources.contains(s) {
                        c.sources.push(s.clone());
                    }
                }
                if c.version_hint.is_none() && found.version_hint.is_some() {
                    c.version_hint = found.version_hint.clone();
                }
                if c.hostname.is_none() && found.hostname.is_some() {
                    c.hostname = found.hostname.clone();
                }
                if c.engine == "unknown" && found.engine != "unknown" {
                    c.engine = found.engine.clone();
                }
            })
            .or_insert(found);
    }

    let mut candidates: Vec<Candidate> = by_key.into_values().collect();

    // Safety net: a 9440 hit is the ClickHouse native+TLS port which we
    // don't fingerprint. If we already confirmed ClickHouse on the same IP
    // via HTTP (8443/8123) or native (9000), drop the unknown entry —
    // operator already knows the host runs ClickHouse and can pick whichever
    // port their client needs.
    let confirmed_ch: std::collections::HashSet<String> = candidates
        .iter()
        .filter(|c| c.engine == "clickhouse")
        .map(|c| c.ip.clone())
        .collect();
    candidates.retain(|c| {
        !(c.engine == "unknown" && c.port == 9440 && confirmed_ch.contains(&c.ip))
    });

    candidates.sort_by(|a, b| (a.ip.clone(), a.port).cmp(&(b.ip.clone(), b.port)));

    ScanReport {
        candidates,
        subnets_scanned: nic_view.subnets_scanned,
        subnets_skipped: nic_view.subnets_skipped,
        search_domains,
        interfaces: nic_view.interfaces,
        duration_ms: 0, // filled by caller
        timed_out: false,
    }
}

impl ScanReport {
    fn empty() -> Self {
        ScanReport {
            candidates: vec![],
            subnets_scanned: vec![],
            subnets_skipped: vec![],
            search_domains: vec![],
            interfaces: vec![],
            duration_ms: 0,
            timed_out: false,
        }
    }
}
