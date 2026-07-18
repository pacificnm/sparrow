//! Server-wide configuration (Issue 13.2 — this crate had no runnable
//! bootstrap before this issue, so this config type didn't exist either).

use nest_config::ConfigService;
use nest_error::{NestError, NestResult};
use serde::Deserialize;

/// Default HTTP bind address.
const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8080";
/// Default alerting evaluation cadence.
const DEFAULT_ALERTING_INTERVAL_SECS: u64 = 30;

/// Configuration for the Sparrow server, read from the `[server]` TOML
/// section — same shape convention as `crates/agent`'s `AgentConfig`.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// PostgreSQL connection URL, **without** a password (e.g.
    /// `postgresql://sparrow@postgres/sparrow`) — the password comes from
    /// `database_password` or `database_password_file` below, never
    /// inlined here, per Issue 13.2's explicit instruction (matching the
    /// same posture Issue 12's TLS/ACL work established: no secrets
    /// committed alongside config).
    pub database_url: String,
    /// Postgres password, inline — convenient for local dev
    /// (`config.toml`, gitignored), not meant for a shared/committed file.
    pub database_password: Option<String>,
    /// Path to a file containing the Postgres password (its entire
    /// content, trimmed) — the Docker Compose path, matching Postgres's
    /// own `POSTGRES_PASSWORD_FILE` convention and `deploy/docker-
    /// compose.yml`'s `secrets: postgres_password`. Takes precedence over
    /// `database_password` if both are set.
    pub database_password_file: Option<String>,

    /// Address of the Mosquitto broker.
    pub mqtt_broker_host: String,
    /// Port of the Mosquitto broker.
    pub mqtt_broker_port: u16,
    /// MQTT password for the dedicated `sparrow-server` broker user (Issue
    /// 12.2's ACL rules grant that exact username broader read/write
    /// access than any individual agent). `None` connects without
    /// credentials — only viable against a broker with `allow_anonymous
    /// true`.
    pub mqtt_password: Option<String>,
    /// Path to a PEM-encoded CA certificate, enabling TLS (Issue 12.1) when
    /// present.
    pub mqtt_tls_ca_file: Option<String>,

    /// HTTP API bind address (`host:port`).
    #[serde(default = "default_http_bind")]
    pub http_bind: String,

    /// Ollama's HTTP base URL — used for both completions
    /// (`nest_ai_ollama::OllamaProvider`) and embeddings
    /// (`sparrow_core::analyst::embedder::OllamaEmbedder`; Issue 10.1's
    /// research spike found neither `nest_ai` nor `nest-ai-ollama` expose
    /// embeddings, so the embedder calls Ollama directly instead of going
    /// through the completions provider).
    pub ollama_base_url: String,
    /// Model used for the AI Health Analyst's completions.
    pub ollama_completion_model: String,
    /// Model used for `search_similar_incidents`' embeddings — Issue 10.1
    /// confirmed `nomic-embed-text` empirically (768-dimensional), not
    /// assumed; a different model here must match
    /// `sparrow_core::analyst::embedder::EMBEDDING_DIMENSION`.
    pub ollama_embedding_model: String,

    /// `AlertingTask`'s rule-evaluation cadence, in seconds.
    #[serde(default = "default_alerting_interval_secs")]
    pub alerting_interval_secs: u64,
}

impl ServerConfig {
    /// Deserializes from the `[server]` section of a [`ConfigService`].
    pub fn from_config_service(cs: &ConfigService) -> NestResult<Self> {
        cs.section("server")
    }

    /// Resolves the final, connectable Postgres URL: `database_url` with
    /// the password (from `database_password_file` if set, else
    /// `database_password`, else none) injected into the userinfo
    /// component. Returns `database_url` unchanged if neither password
    /// source is set (a passwordless/trust-auth Postgres, viable for local
    /// dev, never for the Docker Compose deployment).
    pub fn resolved_database_url(&self) -> NestResult<String> {
        let password = match (&self.database_password_file, &self.database_password) {
            (Some(path), _) => Some(std::fs::read_to_string(path).map_err(|error| {
                NestError::unknown(format!(
                    "failed to read database_password_file {path}: {error}"
                ))
            })?),
            (None, Some(password)) => Some(password.clone()),
            (None, None) => None,
        };

        Ok(match password {
            Some(password) => inject_password(&self.database_url, password.trim()),
            None => self.database_url.clone(),
        })
    }
}

/// Injects `password` into a `scheme://[user[:oldpass]]@host/...` URL's
/// userinfo, replacing any existing password. Plain string manipulation,
/// not a full URL parser — Postgres connection URLs have a simple, fixed
/// shape and this crate has no other need for a `url`-crate dependency.
fn inject_password(url: &str, password: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let Some((userinfo, host_and_path)) = rest.split_once('@') else {
        return url.to_string();
    };
    let username = userinfo.split_once(':').map_or(userinfo, |(user, _)| user);
    format!("{scheme}://{username}:{password}@{host_and_path}")
}

fn default_http_bind() -> String {
    DEFAULT_HTTP_BIND.to_string()
}

fn default_alerting_interval_secs() -> u64 {
    DEFAULT_ALERTING_INTERVAL_SECS
}

#[cfg(test)]
mod tests {
    use nest_config::{ConfigDocument, ConfigSource, LoadedConfig};

    use super::*;

    fn config_service(input: &str) -> ConfigService {
        let document = ConfigDocument::parse_toml(input).expect("valid toml");
        let loaded = LoadedConfig {
            document,
            source: ConfigSource::SearchDefaults,
            path: None,
        };
        ConfigService::new(loaded)
    }

    const SAMPLE: &str = r#"
[server]
database_url = "postgresql://sparrow@localhost/sparrow"
mqtt_broker_host = "localhost"
mqtt_broker_port = 1883
ollama_base_url = "http://127.0.0.1:11434"
ollama_completion_model = "llama3.1"
ollama_embedding_model = "nomic-embed-text"
"#;

    #[test]
    fn deserialize_sample_produces_valid_config_with_defaults() {
        let cfg = ServerConfig::from_config_service(&config_service(SAMPLE)).expect("parse failed");

        assert_eq!(cfg.database_url, "postgresql://sparrow@localhost/sparrow");
        assert_eq!(cfg.database_password, None);
        assert_eq!(cfg.database_password_file, None);
        assert_eq!(cfg.mqtt_broker_host, "localhost");
        assert_eq!(cfg.mqtt_broker_port, 1883);
        assert_eq!(cfg.mqtt_password, None);
        assert_eq!(cfg.mqtt_tls_ca_file, None);
        assert_eq!(cfg.http_bind, "0.0.0.0:8080");
        assert_eq!(cfg.alerting_interval_secs, 30);
        assert_eq!(cfg.ollama_completion_model, "llama3.1");
        assert_eq!(cfg.ollama_embedding_model, "nomic-embed-text");
    }

    #[test]
    fn overrides_are_respected() {
        let cfg = ServerConfig::from_config_service(&config_service(
            r#"
[server]
database_url = "postgresql://sparrow@db/sparrow"
mqtt_broker_host = "mosquitto"
mqtt_broker_port = 8883
mqtt_password = "s3cret"
mqtt_tls_ca_file = "/etc/sparrow/ca.crt"
http_bind = "127.0.0.1:9090"
ollama_base_url = "http://ollama:11434"
ollama_completion_model = "llama3.1"
ollama_embedding_model = "nomic-embed-text"
alerting_interval_secs = 5
"#,
        ))
        .expect("parse failed");

        assert_eq!(cfg.mqtt_broker_port, 8883);
        assert_eq!(cfg.mqtt_password, Some("s3cret".to_string()));
        assert_eq!(
            cfg.mqtt_tls_ca_file,
            Some("/etc/sparrow/ca.crt".to_string())
        );
        assert_eq!(cfg.http_bind, "127.0.0.1:9090");
        assert_eq!(cfg.alerting_interval_secs, 5);
    }

    #[test]
    fn resolved_database_url_is_unchanged_when_no_password_source_is_set() {
        let cfg = ServerConfig::from_config_service(&config_service(SAMPLE)).expect("parse failed");
        assert_eq!(
            cfg.resolved_database_url().expect("resolve"),
            "postgresql://sparrow@localhost/sparrow"
        );
    }

    #[test]
    fn resolved_database_url_injects_the_inline_password() {
        let mut cfg =
            ServerConfig::from_config_service(&config_service(SAMPLE)).expect("parse failed");
        cfg.database_password = Some("s3cret".to_string());
        assert_eq!(
            cfg.resolved_database_url().expect("resolve"),
            "postgresql://sparrow:s3cret@localhost/sparrow"
        );
    }

    #[test]
    fn resolved_database_url_prefers_the_password_file_over_the_inline_password() {
        let dir =
            std::env::temp_dir().join(format!("sparrow-server-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("postgres_password.txt");
        std::fs::write(&path, "from-file-secret\n").expect("write temp secret file");

        let mut cfg =
            ServerConfig::from_config_service(&config_service(SAMPLE)).expect("parse failed");
        cfg.database_password = Some("inline-should-be-ignored".to_string());
        cfg.database_password_file = Some(path.to_str().expect("utf8 path").to_string());

        assert_eq!(
            cfg.resolved_database_url().expect("resolve"),
            "postgresql://sparrow:from-file-secret@localhost/sparrow",
            "the trailing newline in the file must be trimmed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolved_database_url_errors_when_the_password_file_does_not_exist() {
        let mut cfg =
            ServerConfig::from_config_service(&config_service(SAMPLE)).expect("parse failed");
        cfg.database_password_file = Some("/nonexistent/postgres_password.txt".to_string());

        let error = cfg
            .resolved_database_url()
            .expect_err("a missing password file should be an error, not silently ignored");
        assert!(error.to_string().contains("database_password_file"));
    }
}
