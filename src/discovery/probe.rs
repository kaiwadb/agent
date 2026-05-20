//! TCP connect + handshake fingerprint.
//!
//! `probe_endpoint` is the unit of work scheduled by the orchestrator. It:
//!   1. Tries a TCP connect with a short timeout. Closed/filtered ports return
//!      `None`.
//!   2. Runs the per-engine handshake dispatcher in `fingerprint`.
//!   3. Reverse-DNSes the IP best-effort (with its own timeout).

use std::net::{IpAddr, SocketAddr};

use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::trace;

use super::fingerprint;
use super::resolver;
use super::{Candidate, Source};

pub async fn probe_endpoint(
    ip: IpAddr,
    port: u16,
    engine: &str,
    sources: &[Source],
) -> Option<Candidate> {
    let addr = SocketAddr::new(ip, port);
    let connect = TcpStream::connect(addr);
    let stream = match timeout(super::PROBE_TIMEOUT, connect).await {
        Ok(Ok(s)) => s,
        _ => return None,
    };
    trace!(%addr, %engine, "tcp open");

    let (resolved_engine, version_hint) =
        fingerprint::fingerprint(stream, ip, port, engine).await;

    // Drop false-positives: an open port on a "DB default" that didn't speak
    // the expected protocol is almost always something else (admin panel on
    // 8443, random service on 3307, etc.). Keep `unknown` only on ports we
    // never even attempt to fingerprint — currently the ClickHouse native
    // binary ports — so the operator can still confirm those manually.
    if resolved_engine == "unknown" && !is_unfingerprintable_port(port) {
        return None;
    }

    let hostname = resolver::reverse(ip).await;

    Some(Candidate {
        ip: ip.to_string(),
        hostname,
        port,
        engine: resolved_engine.to_string(),
        version_hint,
        sources: sources.iter().map(|s| s.as_str().to_string()).collect(),
    })
}

fn is_unfingerprintable_port(port: u16) -> bool {
    matches!(port, 9000 | 9440)
}
