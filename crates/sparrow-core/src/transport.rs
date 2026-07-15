use serde::{Deserialize, Serialize};

pub struct Topics;

impl Topics {
    pub fn register(host_id: &str) -> String { format!("sparrow/agents/{host_id}/register") }
    pub fn heartbeat(host_id: &str) -> String { format!("sparrow/agents/{host_id}/heartbeat") }
    pub fn data(host_id: &str) -> String { format!("sparrow/agents/{host_id}/data") }
    pub fn config(host_id: &str) -> String { format!("sparrow/agents/{host_id}/config") }
    pub fn command(host_id: &str) -> String { format!("sparrow/agents/{host_id}/command") }
    pub fn all_data() -> &'static str { "sparrow/agents/+/data" }
    pub fn all_register() -> &'static str { "sparrow/agents/+/register" }
    pub fn all_heartbeat() -> &'static str { "sparrow/agents/+/heartbeat" }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataBatch {
    pub host_id: String,
    pub collector: String,
    pub items: Vec<crate::collector::MetricItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterMessage {
    pub host_id: String,
    pub hostname: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatMessage {
    pub host_id: String,
    pub timestamp_ms: i64,
}

impl DataBatch {
    pub fn to_payload(&self) -> Vec<u8> { serde_json::to_vec(self).expect("serialization should not fail") }
    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> { serde_json::from_slice(data) }
}

impl RegisterMessage {
    pub fn to_payload(&self) -> Vec<u8> { serde_json::to_vec(self).expect("serialization should not fail") }
    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> { serde_json::from_slice(data) }
}

impl HeartbeatMessage {
    pub fn to_payload(&self) -> Vec<u8> { serde_json::to_vec(self).expect("serialization should not fail") }
    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> { serde_json::from_slice(data) }
}
