#![allow(clippy::result_large_err)]

mod config;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use clap::{ArgMatches, Command};
use nest_ai::AiService;
use nest_ai_ollama::{OllamaConfig, OllamaProvider};
use nest_cli::{AsyncCliCommand, CliApp};
use nest_config::ConfigService;
use nest_core::AppContext;
use nest_data_postgres::{migration::apply_migrations, PostgresConfig, PostgresConnection};
use nest_error::{NestError, NestResult};
use nest_http_client::{HttpClientConfig, HttpClientService};
use nest_http_serve::HttpServer;
use nest_mqtt::{MqttClient, MqttConfig, TlsConfig};
use nest_task::TaskManager;
use nest_task_runtime::{RuntimeConfig, TaskManagerConfig, TaskManagerService, TaskRuntimeModule};

use config::ServerConfig;
use sparrow_core::analyst::embedder::{Embedder, OllamaEmbedder};
use sparrow_core::storage::{HostRegistry, MetricHistory};
use sparrow_server::alerting::{AlertingTask, LogSink};
use sparrow_server::api;
use sparrow_server::ingest;
use sparrow_server::offline_watch::OfflineWatch;

/// This crate had no runnable binary at all before Issue 13.2 — every prior
/// phase (7 through 11) added library functions (`ingest.rs`,
/// `alerting.rs`, every `api/*.rs` route builder) but none of them ever got
/// wired into a `main.rs`, unlike `crates/agent` (Issue 6.6). This is that
/// wiring, built as part of the deploy/Dockerfile work since a Dockerfile
/// can't package a binary that doesn't exist.
///
/// Persistent tasks here: two ingest loops that don't fit `nest-task`'s
/// `Task` trait (`ingest.rs`'s functions are plain infinite loops over an
/// MQTT subscription stream, not `Task::run`-shaped — a third,
/// `run_register_ingest`, is one-shot-ish but still loop-shaped the same
/// way), spawned directly via `tokio::spawn`; `offline_watch`/`alerting`
/// *do* implement `Task`, spawned via `TaskManagerService` like
/// `crates/agent`'s tasks. Same reasoning as that crate's own
/// `MAX_CONCURRENT_TASKS` sizing: this needs more than the default
/// `TaskManagerConfig`'s `max_concurrent: 4` for its two persistent
/// `Task`-trait tasks (offline_watch, alerting) to actually run rather than
/// queue forever — sized generously above the real count, same pattern.
const MAX_CONCURRENT_TASKS: usize = 16;

fn main() -> NestResult<()> {
    CliApp::new("sparrow-server")
        .module(
            TaskRuntimeModule::owned(RuntimeConfig::default())?.with_manager_config(
                TaskManagerConfig {
                    max_concurrent: MAX_CONCURRENT_TASKS,
                },
            ),
        )
        .async_command(RunCommand)
        .run()
}

struct RunCommand;

#[async_trait]
impl AsyncCliCommand for RunCommand {
    fn name(&self) -> &'static str {
        "run"
    }

    fn about(&self) -> &'static str {
        "Run the Sparrow server: HTTP API, MQTT ingest, alerting"
    }

    fn configure(&self, cmd: Command) -> Command {
        cmd
    }

    async fn run_async(&self, ctx: &AppContext, _matches: &ArgMatches) -> NestResult<()> {
        let config_service = ctx.service::<ConfigService>()?;
        let config = ServerConfig::from_config_service(config_service)?;

        // PostgresConnection::connect retries with backoff internally
        // (nest-data-postgres's own Phase 1 behavior) - this, plus
        // MqttClient's event loop reconnecting forever on its own, is what
        // makes docker-compose's `depends_on` weak "container started, not
        // ready" guarantee safe to rely on (see deploy/README.md's
        // "Startup ordering" section).
        let database_url = config.resolved_database_url()?;
        let pg = PostgresConnection::connect(&PostgresConfig::new(&database_url)).await?;
        let pool = pg.pool().clone();
        apply_migrations(&pool, &sparrow_core::migrations::all_migrations()).await?;

        let mut mqtt_config = MqttConfig::new(
            &config.mqtt_broker_host,
            config.mqtt_broker_port,
            "sparrow-server",
        );
        if let Some(password) = &config.mqtt_password {
            mqtt_config = mqtt_config.with_credentials("sparrow-server", password);
        }
        if let Some(ca_file) = &config.mqtt_tls_ca_file {
            let tls = TlsConfig::from_ca_file(ca_file).map_err(|error| {
                NestError::unknown(format!("failed to read mqtt_tls_ca_file: {error}"))
            })?;
            mqtt_config = mqtt_config.with_tls(tls);
        }
        let mqtt = MqttClient::connect(&mqtt_config).await?;

        let registry = HostRegistry::new(pool.clone());
        let history = MetricHistory::new(pool.clone());

        // ingest.rs's three loops aren't `Task`-shaped (see module doc) -
        // spawned directly, each logging and exiting its own task on a
        // fatal subscribe error rather than taking the whole server down.
        tokio::spawn(run_ingest_loop("register", {
            let mqtt = mqtt.clone();
            let registry = registry.clone();
            async move { ingest::run_register_ingest(mqtt, registry).await }
        }));
        tokio::spawn(run_ingest_loop("heartbeat", {
            let mqtt = mqtt.clone();
            let registry = registry.clone();
            async move { ingest::run_heartbeat_ingest(mqtt, registry).await }
        }));
        tokio::spawn(run_ingest_loop("data", {
            let mqtt = mqtt.clone();
            let history = history.clone();
            async move { ingest::run_data_ingest(mqtt, history).await }
        }));

        let manager = ctx.service::<TaskManagerService>()?.clone();
        manager
            .spawn(OfflineWatch::with_defaults(registry.clone()))
            .await?;

        let http_client = HttpClientService::new(HttpClientConfig::default())?;
        let embedder: Arc<dyn Embedder> = Arc::new(OllamaEmbedder::new(
            &config.ollama_base_url,
            &config.ollama_embedding_model,
            http_client.clone(),
        ));
        let ai_provider = OllamaProvider::new(OllamaConfig::new(
            &config.ollama_base_url,
            &config.ollama_completion_model,
        ))?;
        let ai_service = AiService::new(Arc::new(ai_provider));

        manager
            .spawn(AlertingTask::new(
                pool.clone(),
                Duration::from_secs(config.alerting_interval_secs),
                vec![Arc::new(LogSink)],
                Arc::clone(&embedder),
            ))
            .await?;

        tracing::info!(bind = %config.http_bind, "sparrow-server running");

        HttpServer::builder()
            .name("sparrow-server")
            .bind(&config.http_bind)
            .routes(api::hosts::routes(registry.clone(), history.clone()))
            .routes(api::history::routes(history))
            .routes(api::problems::routes(pool.clone()))
            .routes(api::agent_config::routes(pool.clone(), mqtt))
            .routes(api::analyst::routes(ai_service, pool, embedder))
            .run()
            .await
            // ServeError is Debug, not Display (checked its source rather
            // than assuming `.to_string()` would work).
            .map_err(|error| NestError::unknown(format!("{error:?}")))?;

        Ok(())
    }
}

/// Runs an ingest loop to completion (it only returns on a fatal
/// subscribe/stream error, never on a single malformed message - see
/// `ingest.rs`'s own doc comment), logging the failure instead of letting
/// `tokio::spawn`'s dropped `JoinHandle` swallow it silently.
async fn run_ingest_loop(
    name: &'static str,
    fut: impl std::future::Future<Output = NestResult<()>>,
) {
    if let Err(error) = fut.await {
        tracing::error!(loop_name = name, %error, "ingest loop exited with an error");
    }
}
