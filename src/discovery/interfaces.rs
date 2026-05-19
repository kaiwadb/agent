//! NIC enumeration, subnet classification, host expansion.
//!
//! Only IPv4 subnets are enumerated. IPv6 globals are recorded for the report
//! but never enumerated (2^64 hosts make brute scanning meaningless). IPv6
//! peers can still be discovered via ARP/NDP cache parsing in `hints.rs`.

use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use serde::Serialize;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub ip: String,
    pub prefix: u8,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedSubnet {
    pub cidr: String,
    pub reason: String,
    pub prefix: u8,
}

pub struct NicView {
    pub interfaces: Vec<InterfaceInfo>,
    pub subnets_scanned: Vec<String>,
    pub subnets_skipped: Vec<SkippedSubnet>,
    pub enumerated_ips: Vec<Ipv4Addr>,
    pub extra_cidr_ips: Vec<Ipv4Addr>,
    /// All NIC-bound IPv4 nets we *saw*, regardless of whether we enumerated
    /// them. Used by the orchestrator to dedup routes from the routing table
    /// that already point at a NIC-known subnet.
    pub nic_subnets: Vec<Ipv4Net>,
}

/// Expand an additional set of IPv4 subnets (e.g. discovered via the routing
/// table) into host IPs, applying the same `max_prefix` cap as NIC subnets.
/// Subnets that exceed the cap are recorded in `skipped`; subnets already
/// fully covered by `existing` are silently dropped.
pub fn expand_routed(
    nets: &[Ipv4Net],
    existing: &[Ipv4Net],
    max_prefix: u8,
    scanned: &mut Vec<String>,
    skipped: &mut Vec<SkippedSubnet>,
) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for net in nets {
        if existing.iter().any(|e| e.contains(net)) {
            continue;
        }
        if net.prefix_len() < max_prefix {
            skipped.push(SkippedSubnet {
                cidr: net.to_string(),
                reason: "too_large".to_string(),
                prefix: net.prefix_len(),
            });
            continue;
        }
        scanned.push(net.to_string());
        for host in net.hosts() {
            out.push(host);
        }
    }
    out
}

pub fn scan_interfaces(max_prefix: u8, extra_cidrs: &[String]) -> NicView {
    let mut interfaces = Vec::new();
    let mut subnets_scanned = Vec::new();
    let mut subnets_skipped = Vec::new();
    let mut enumerated_ips: Vec<Ipv4Addr> = Vec::new();
    let mut nic_subnets: Vec<Ipv4Net> = Vec::new();

    let raw = match if_addrs::get_if_addrs() {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to enumerate network interfaces");
            let extra_cidr_ips =
                expand_extra_cidrs(extra_cidrs, max_prefix, &mut subnets_skipped);
            return NicView {
                interfaces,
                subnets_scanned,
                subnets_skipped,
                enumerated_ips,
                extra_cidr_ips,
                nic_subnets,
            };
        }
    };

    for iface in raw {
        match iface.addr {
            if_addrs::IfAddr::V4(addr) => {
                let prefix = ipv4_netmask_to_prefix(addr.netmask);
                let kind = classify_v4(&addr.ip);
                interfaces.push(InterfaceInfo {
                    name: iface.name.clone(),
                    ip: addr.ip.to_string(),
                    prefix,
                    kind: kind.label().to_string(),
                });

                if !matches!(kind, V4Kind::Private) {
                    if matches!(kind, V4Kind::Loopback | V4Kind::LinkLocal | V4Kind::Public) {
                        let cidr = format!("{}/{}", network_address(addr.ip, prefix), prefix);
                        subnets_skipped.push(SkippedSubnet {
                            cidr,
                            reason: kind.label().to_string(),
                            prefix,
                        });
                    }
                    continue;
                }

                let net = match Ipv4Net::new(addr.ip, prefix) {
                    Ok(n) => n.trunc(),
                    Err(e) => {
                        warn!(error = %e, "invalid IPv4 net derived from interface");
                        continue;
                    }
                };
                nic_subnets.push(net);

                if prefix < max_prefix {
                    subnets_skipped.push(SkippedSubnet {
                        cidr: net.to_string(),
                        reason: "too_large".to_string(),
                        prefix,
                    });
                    continue;
                }

                subnets_scanned.push(net.to_string());
                for host in net.hosts() {
                    enumerated_ips.push(host);
                }
            }
            if_addrs::IfAddr::V6(addr) => {
                interfaces.push(InterfaceInfo {
                    name: iface.name.clone(),
                    ip: addr.ip.to_string(),
                    prefix: 0,
                    kind: "ipv6".to_string(),
                });
                // Never enumerate v6.
            }
        }
    }

    let extra_cidr_ips = expand_extra_cidrs(extra_cidrs, max_prefix, &mut subnets_skipped);
    debug!(
        ifaces = interfaces.len(),
        scanned = subnets_scanned.len(),
        skipped = subnets_skipped.len(),
        enumerated = enumerated_ips.len(),
        extras = extra_cidr_ips.len(),
        "interface scan summary"
    );

    NicView {
        interfaces,
        subnets_scanned,
        subnets_skipped,
        enumerated_ips,
        extra_cidr_ips,
        nic_subnets,
    }
}

fn expand_extra_cidrs(
    cidrs: &[String],
    max_prefix: u8,
    skipped: &mut Vec<SkippedSubnet>,
) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for raw in cidrs {
        match raw.parse::<Ipv4Net>() {
            Ok(net) => {
                let net = net.trunc();
                if net.prefix_len() < max_prefix {
                    skipped.push(SkippedSubnet {
                        cidr: net.to_string(),
                        reason: "too_large".to_string(),
                        prefix: net.prefix_len(),
                    });
                    continue;
                }
                for host in net.hosts() {
                    out.push(host);
                }
            }
            Err(e) => {
                warn!(cidr = %raw, error = %e, "skipping invalid extra_cidr");
                skipped.push(SkippedSubnet {
                    cidr: raw.clone(),
                    reason: "invalid".to_string(),
                    prefix: 0,
                });
            }
        }
    }
    out
}

enum V4Kind {
    Loopback,
    LinkLocal,
    Private,
    Public,
}

impl V4Kind {
    fn label(&self) -> &'static str {
        match self {
            V4Kind::Loopback => "loopback",
            V4Kind::LinkLocal => "link_local",
            V4Kind::Private => "private",
            V4Kind::Public => "public",
        }
    }
}

fn classify_v4(ip: &Ipv4Addr) -> V4Kind {
    if ip.is_loopback() {
        V4Kind::Loopback
    } else if ip.is_link_local() {
        V4Kind::LinkLocal
    } else if ip.is_private() {
        V4Kind::Private
    } else {
        V4Kind::Public
    }
}

fn ipv4_netmask_to_prefix(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

fn network_address(ip: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    if prefix == 0 {
        return Ipv4Addr::new(0, 0, 0, 0);
    }
    let bits = !0u32 << (32 - prefix);
    Ipv4Addr::from(u32::from(ip) & bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basic() {
        assert!(matches!(classify_v4(&Ipv4Addr::new(127, 0, 0, 1)), V4Kind::Loopback));
        assert!(matches!(classify_v4(&Ipv4Addr::new(169, 254, 1, 1)), V4Kind::LinkLocal));
        assert!(matches!(classify_v4(&Ipv4Addr::new(10, 1, 2, 3)), V4Kind::Private));
        assert!(matches!(classify_v4(&Ipv4Addr::new(192, 168, 0, 1)), V4Kind::Private));
        assert!(matches!(classify_v4(&Ipv4Addr::new(172, 16, 0, 1)), V4Kind::Private));
        assert!(matches!(classify_v4(&Ipv4Addr::new(8, 8, 8, 8)), V4Kind::Public));
    }

    #[test]
    fn netmask_to_prefix() {
        assert_eq!(ipv4_netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 0)), 24);
        assert_eq!(ipv4_netmask_to_prefix(Ipv4Addr::new(255, 255, 0, 0)), 16);
        assert_eq!(ipv4_netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 255)), 32);
        assert_eq!(ipv4_netmask_to_prefix(Ipv4Addr::new(0, 0, 0, 0)), 0);
    }

    #[test]
    fn network_addr() {
        assert_eq!(
            network_address(Ipv4Addr::new(10, 0, 4, 23), 24),
            Ipv4Addr::new(10, 0, 4, 0)
        );
        assert_eq!(
            network_address(Ipv4Addr::new(172, 16, 5, 9), 12),
            Ipv4Addr::new(172, 16, 0, 0)
        );
    }

    #[test]
    fn skip_too_large() {
        let mut skipped = Vec::new();
        let ips = expand_extra_cidrs(&["10.0.0.0/8".into()], 22, &mut skipped);
        assert!(ips.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "too_large");
    }

    #[test]
    fn expand_small_cidr() {
        let mut skipped = Vec::new();
        let ips = expand_extra_cidrs(&["192.168.1.0/30".into()], 22, &mut skipped);
        // /30 has 2 usable hosts via ipnet::hosts (network/broadcast excluded).
        assert_eq!(ips.len(), 2);
        assert!(skipped.is_empty());
    }
}
