use std::collections::BTreeMap;

use nest_config::ConfigService;
use serde::Deserialize;

/// Configuration for the Sparrow agent read from the `[agent]` TOML section.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Stable identifier published to MQTT topics.
    pub host_id: String,

    /// Address of the Mosquitto broker.
    pub broker_host: String,

    /// Port of the Mosquitto broker.
    pub broker_port: u16,

    /// MQTT password. When present, the agent authenticates with username
    /// `host_id` (Issue 12.2's ACL rules match on the connecting username
    /// via `%u`, not client_id) and this password. `None` connects without
    /// credentials — only viable against a broker with `allow_anonymous
    /// true`; a broker configured per `deploy/mosquitto/mosquitto.conf`
    /// (Issue 12.2) will reject it.
    pub mqtt_password: Option<String>,

    /// Per-collector interval overrides; absent entries use the collector's
    /// own `default_interval_secs()`.
    #[serde(default)]
    pub collector_intervals: BTreeMap<String, u64>,

    /// Collectors explicitly disabled — everything in `default_collectors()`
    /// runs unless named here.
    #[serde(default)]
    pub disabled_collectors: Vec<String>,
}

impl AgentConfig {
    /// Deserialize from the `[agent]` section of a [`ConfigService`].
    pub fn from_config_service(cs: &ConfigService) -> nest_error::NestResult<Self> {
        cs.section("agent")
    }
}

#[cfg(test)]
mod tests {
    use crate::config::AgentConfig;
    use nest_config::{ConfigDocument, ConfigService};

    const SAMPLE: &str = r#"
[agent]
host_id = "my-test-host"
broker_host = "localhost"
broker_port = 1883
"#;

    fn config_service(input: &str) -> ConfigService {
        use nest_config::LoadedConfig;

        let document = ConfigDocument::parse_toml(input).expect("valid toml");
        let loaded = LoadedConfig {
            document,
            source: nest_config::ConfigSource::SearchDefaults,
            path: None,
        };
        ConfigService::new(loaded)
    }

    #[test]
    fn deserialize_sample_produces_valid_config() {
        let cs = config_service(SAMPLE);
        let cfg = AgentConfig::from_config_service(&cs).expect("parse failed");

        assert_eq!(cfg.host_id, "my-test-host");
        assert_eq!(cfg.broker_host, "localhost");
        assert_eq!(cfg.broker_port, 1883);

        // No overrides in the sample — absent entries fall back to each
        // collector's own default_interval_secs() at the scheduler, not here.
        assert!(cfg.collector_intervals.is_empty());
        assert_eq!(cfg.disabled_collectors, Vec::<String>::new());
    }

    #[test]
    fn custom_intervals_and_disabled_are_respected() {
        let cs = config_service(
            r#"[agent]
host_id = "custom"
broker_host = "remote.mosquitto.org"
broker_port = 8883

[agent.collector_intervals]
cpu = 10
disk = 120
"#,
        );

        let cfg = AgentConfig::from_config_service(&cs).expect("parse failed");

        assert_eq!(cfg.host_id, "custom");
        assert_eq!(cfg.broker_host, "remote.mosquitto.org");
        assert_eq!(cfg.broker_port, 8883);
        // When the table is overridden in TOML, only the specified keys remain.
        assert_eq!(cfg.collector_intervals.get("cpu"), Some(&10));
        assert_eq!(cfg.collector_intervals.get("disk"), Some(&120));

        let cs2 = config_service(
            r#"[agent]
host_id = "minimal"
broker_host = "127.0.0.1"
broker_port = 1884
disabled_collectors = ["network", "disk"]
"#,
        );

        let cfg2 = AgentConfig::from_config_service(&cs2).expect("parse failed");
        assert_eq!(cfg2.host_id, "minimal");
        assert!(cfg2.disabled_collectors.contains(&"network".to_string()));
        assert!(cfg2.disabled_collectors.contains(&"disk".to_string()));
        // No [agent.collector_intervals] table given — stays empty.
        assert!(cfg2.collector_intervals.is_empty());
    }

    #[test]
    fn mqtt_password_defaults_to_none_and_round_trips_when_present() {
        let cfg = AgentConfig::from_config_service(&config_service(SAMPLE)).expect("parse failed");
        assert_eq!(cfg.mqtt_password, None);

        let cfg_with_password = AgentConfig::from_config_service(&config_service(
            r#"[agent]
host_id = "my-test-host"
broker_host = "localhost"
broker_port = 8883
mqtt_password = "s3cret"
"#,
        ))
        .expect("parse failed");
        assert_eq!(cfg_with_password.mqtt_password, Some("s3cret".to_string()));
    }
}
