use serde::{Deserialize, Serialize};

/// The config override an agent receives on `Topics::config(host_id)`.
///
/// Lives here (not in `crates/agent`) because both the agent and the server
/// need it: the agent deserializes it from the retained MQTT message, the
/// server constructs and publishes it (Phase 9). It deliberately excludes
/// `host_id`/`broker_host`/`broker_port` — those are agent-local concerns
/// the server never sets — see `crates/agent`'s `AgentConfig` for the full
/// local config those fields belong to.
///
/// This is the one payload that must agree byte-for-byte between agent and
/// server; if a future phase's server-side type drifts from this, that's a
/// bug in the server, not a reason to add a second shape here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentConfigOverride {
    #[serde(default)]
    pub disabled_collectors: Vec<String>,
    #[serde(default)]
    pub collector_intervals: std::collections::BTreeMap<String, u64>,
}

impl AgentConfigOverride {
    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialization should not fail")
    }

    pub fn from_payload(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_everything_enabled_with_no_overrides() {
        let config = AgentConfigOverride::default();
        assert!(config.disabled_collectors.is_empty());
        assert!(config.collector_intervals.is_empty());
    }

    #[test]
    fn payload_round_trips() {
        let config = AgentConfigOverride {
            disabled_collectors: vec!["disk".to_string()],
            collector_intervals: std::collections::BTreeMap::from([("cpu".to_string(), 10)]),
        };

        let decoded =
            AgentConfigOverride::from_payload(&config.to_payload()).expect("valid override");

        assert_eq!(decoded, config);
    }

    #[test]
    fn missing_fields_default_to_empty() {
        let decoded = AgentConfigOverride::from_payload(b"{}").expect("valid empty object");
        assert_eq!(decoded, AgentConfigOverride::default());
    }
}
