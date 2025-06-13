use super::query::Query;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub enum ServerCommand {
    Query(Query),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum AgentResult {
    QueryResult(Value),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerCommandMsg {
    pub channel: String,
    pub payload: ServerCommand,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentResultMsg {
    pub channel: String,
    pub payload: AgentResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentNotification {}

#[derive(Debug, Serialize, Deserialize)]
pub enum PayloadFromAgent {
    Result(AgentResultMsg),
    Notification(AgentNotification),
}
