//! Wire shapes between the tunnel and the kaiwadb server.
//!
//! Server → tunnel (text JSON):
//!   `{"type":"command","id":<i64>,"request":{...ServerCommand...}}`
//!
//! Tunnel → server:
//!   Text:   `{"type":"begin","id":<i64>}`
//!   Binary: `[8-byte BE i64 cmd_id][payload bytes]`
//!   Text:   `{"type":"end","id":<i64>,"error":null|"..."}`
//!
//! `begin` opens a response stream; binary frames carry chunks tagged with
//! the cmd_id so multiple in-flight commands can interleave; `end` closes
//! it. If `error` is non-null the response is considered failed regardless
//! of any chunks already streamed.

use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Bytes;

use crate::discovery::{Candidate, InterfaceInfo, SkippedSubnet};
use crate::query::Query;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Command { id: i64, request: ServerCommand },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TunnelControl {
    Begin { id: i64 },
    End { id: i64, error: Option<String> },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerCommand {
    Query(Query),
    Scan(ScanRequest),
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

pub const BIN_HDR: usize = 8;

/// Wrap `payload` with an 8-byte cmd_id header. Returns a fresh allocation
/// suitable for `Message::Binary`.
pub fn frame_chunk(id: i64, payload: &[u8]) -> Bytes {
    let mut buf = Vec::with_capacity(BIN_HDR + payload.len());
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(payload);
    Bytes::from(buf)
}
