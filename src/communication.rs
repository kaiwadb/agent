use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::discovery::{Candidate, InterfaceInfo, SkippedSubnet};
use crate::query::Query;

/// Server -> tunnel frame. The id is the originating
/// `tunnel_commands.id` row in the server's database, used to correlate
/// responses without any in-process routing table.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Command { id: i64, request: ServerCommand },
}

/// Tunnel -> server frame. The id always echoes back the originating
/// command's id.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TunnelMessage {
    Result { id: i64, payload: TunnelResult },
    Error { id: i64, error: String },
}

#[derive(Debug, Deserialize)]
pub enum ServerCommand {
    Query(Query),
    Scan(ScanRequest),
}

#[derive(Debug, Serialize)]
pub enum TunnelResult {
    QueryResult(Value),
    ScanResult(ScanReport),
}

#[derive(Debug, Default, Deserialize)]
pub struct ScanRequest {
    #[serde(default)]
    pub timeout_ms: Option<u32>,
    #[serde(default)]
    pub max_prefix: Option<u8>,
    #[serde(default)]
    pub extra_cidrs: Vec<String>,
    #[serde(default)]
    pub include_dns_labels: Option<Vec<String>>,
    #[serde(default)]
    pub include_search_domains: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub candidates: Vec<Candidate>,
    pub subnets_scanned: Vec<String>,
    pub subnets_skipped: Vec<SkippedSubnet>,
    pub search_domains: Vec<String>,
    pub interfaces: Vec<InterfaceInfo>,
    pub duration_ms: u64,
    pub timed_out: bool,
}
