//! Shared DNS resolver. One `TokioAsyncResolver` instance is reused for both
//! forward (DNS hint phase) and reverse (hostname-on-candidate) lookups so
//! initialization cost and connection caches are amortised.

use std::net::IpAddr;
use std::sync::OnceLock;

use hickory_resolver::TokioAsyncResolver;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use tokio::time::{Duration, timeout};
use tracing::debug;

const REVERSE_TIMEOUT: Duration = Duration::from_millis(500);
const FORWARD_TIMEOUT: Duration = Duration::from_millis(800);

static RESOLVER: OnceLock<Option<TokioAsyncResolver>> = OnceLock::new();

fn resolver() -> Option<&'static TokioAsyncResolver> {
    RESOLVER
        .get_or_init(|| match TokioAsyncResolver::tokio_from_system_conf() {
            Ok(r) => Some(r),
            Err(e) => {
                debug!(error = %e, "falling back to cloudflare resolver");
                Some(TokioAsyncResolver::tokio(
                    ResolverConfig::cloudflare(),
                    ResolverOpts::default(),
                ))
            }
        })
        .as_ref()
}

pub async fn reverse(ip: IpAddr) -> Option<String> {
    let r = resolver()?;
    let response = timeout(REVERSE_TIMEOUT, r.reverse_lookup(ip)).await.ok()?.ok()?;
    let name = response.iter().next()?.to_string();
    let name = name.trim_end_matches('.').to_string();
    if name.is_empty() { None } else { Some(name) }
}

pub async fn forward(name: &str) -> Vec<IpAddr> {
    let Some(r) = resolver() else { return vec![] };
    let response = match timeout(FORWARD_TIMEOUT, r.lookup_ip(name)).await {
        Ok(Ok(r)) => r,
        _ => return vec![],
    };
    response.iter().collect()
}
