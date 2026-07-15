use serde::{Deserialize, Serialize};

/// Sparrow's MQTT topic taxonomy. Centralize every topic string here —
/// nowhere else in the codebase should hand-format a topic string.
pub struct Topics;

impl Topics {
    pub fn register(host_id: &str) -> String {
        format!("sparrow/agents/{host_id}/register")
    }

    pub fn heartbeat(host_id: &str) -> String {
        format!("sparrow/agents/{host_id}/heartbeat")
    }

    pub fn data(host_id: &str) -> String {
        format!("sparrow/agents/{host_id}/data")
    }

    pub fn config(host_id: &str) -> String {
        format!("sparrow/agents/{host_id}/config")
    }

    pub fn command(host_id: &str) -> String {
        format!("sparrow/agents/{host_id}/command")
    }

    /// Wildcard filter for the server to subscribe to all agents' data at once.
    pub fn all_data() -> &'static str {
        "sparrow/agents/+/data"
    }

    pub fn all_register() -> &'static str {
        "sparrow/agents/+/register"
    }

    pub fn all_heartbeat() -> &'static str {
        "sparrow/agents/+/heartbeat"
    }
}

/// Wire payload published on `data` topics — a batch, not one message per item,
/// to keep MQTT message counts sane at high collector frequency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataBatch {
    pub host_id: String,
    pub collector: String,
    pub items: Vec<crate::collector::MetricItem>,
}

/// Wire payload published on `register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterMessage {
    pub host_id: String,
    pub hostname: String,
    pub agent_version: String,
}

/// Wire payload published on `heartbeat` — kept minimal on purpose; presence
/// alone (plus LWT for absence) carries most of the signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatMessage {
    pub host_id: String,
    pub timestamp_ms: i64,
}

impl DataBatch {
    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialization should not fail")
    }

    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

impl RegisterMessage {
    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialization should not fail")
    }

    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

impl HeartbeatMessage {
    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialization should not fail")
    }

    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::collector::{MetricItem, ValueType};

    use super::*;

    #[test]
    fn topics_match_the_wire_contract() {
        let host_id = "host-123";

        assert_eq!(
            Topics::register(host_id),
            "sparrow/agents/host-123/register"
        );
        assert_eq!(
            Topics::heartbeat(host_id),
            "sparrow/agents/host-123/heartbeat"
        );
        assert_eq!(Topics::data(host_id), "sparrow/agents/host-123/data");
        assert_eq!(Topics::config(host_id), "sparrow/agents/host-123/config");
        assert_eq!(Topics::command(host_id), "sparrow/agents/host-123/command");
        assert_eq!(Topics::all_data(), "sparrow/agents/+/data");
        assert_eq!(Topics::all_register(), "sparrow/agents/+/register");
        assert_eq!(Topics::all_heartbeat(), "sparrow/agents/+/heartbeat");
    }

    #[test]
    fn data_batch_payload_round_trips() {
        let batch = DataBatch {
            host_id: "host-123".to_string(),
            collector: "cpu".to_string(),
            items: vec![MetricItem {
                key: "cpu.usage_percent".to_string(),
                value_type: ValueType::Float,
                value: "42.5".to_string(),
                tags: BTreeMap::from([("core".to_string(), "0".to_string())]),
                timestamp_ms: 1_700_000_000_000,
            }],
        };

        let decoded = DataBatch::from_payload(&batch.to_payload()).expect("valid data batch");

        assert_eq!(decoded.host_id, batch.host_id);
        assert_eq!(decoded.collector, batch.collector);
        assert_eq!(decoded.items.len(), 1);
        assert_eq!(decoded.items[0].key, batch.items[0].key);
        assert_eq!(decoded.items[0].value_type, batch.items[0].value_type);
        assert_eq!(decoded.items[0].value, batch.items[0].value);
        assert_eq!(decoded.items[0].tags, batch.items[0].tags);
        assert_eq!(decoded.items[0].timestamp_ms, batch.items[0].timestamp_ms);
    }

    #[test]
    fn register_message_payload_round_trips() {
        let message = RegisterMessage {
            host_id: "host-123".to_string(),
            hostname: "sparrow-agent".to_string(),
            agent_version: "0.1.0".to_string(),
        };

        let decoded =
            RegisterMessage::from_payload(&message.to_payload()).expect("valid register message");

        assert_eq!(decoded.host_id, message.host_id);
        assert_eq!(decoded.hostname, message.hostname);
        assert_eq!(decoded.agent_version, message.agent_version);
    }

    #[test]
    fn heartbeat_message_payload_round_trips() {
        let message = HeartbeatMessage {
            host_id: "host-123".to_string(),
            timestamp_ms: 1_700_000_000_000,
        };

        let decoded =
            HeartbeatMessage::from_payload(&message.to_payload()).expect("valid heartbeat message");

        assert_eq!(decoded.host_id, message.host_id);
        assert_eq!(decoded.timestamp_ms, message.timestamp_ms);
    }
}
