#![allow(clippy::result_large_err)]

use std::sync::Arc;

use async_trait::async_trait;
use clap::{ArgMatches, Command};
use nest_cli::{AsyncCliCommand, CliApp};
use nest_config::ConfigService;
use nest_core::AppContext;
use nest_error::NestResult;
use nest_mqtt::{LastWillConfig, MqttClient, MqttConfig, MqttQos};
use nest_task::TaskManager;
use nest_task_runtime::{RuntimeConfig, TaskManagerConfig, TaskManagerService, TaskRuntimeModule};
use sparrow_agent::config::AgentConfig;
use sparrow_agent::config_reload::ConfigReload;
use sparrow_agent::heartbeat::HeartbeatTask;
use sparrow_agent::publisher::Publisher;
use sparrow_agent::scheduler::BatchSink;
use sparrow_core::transport::{RegisterMessage, Topics};

/// Persistent tasks — one per enabled collector (up to 3: cpu/memory/disk),
/// plus heartbeat, plus config_reload — each hold their
/// `TaskManagerService` semaphore permit for their *entire* (indefinite)
/// lifetime: `nest-task-runtime`'s `spawn` acquires a permit before
/// `Task::run` and only releases it once that call returns, which for these
/// tasks is "never, until cancelled" (verified by reading
/// `nest-task-runtime/src/manager.rs` directly, not assumed). The default
/// `TaskManagerConfig` (`max_concurrent: 4`) is sized for bursts of
/// short-lived tasks, not this — with it, the 5th persistent task spawned
/// would sit `Queued` forever, never actually running, since no permit ever
/// frees up. Set generously above the actual task count instead.
const MAX_CONCURRENT_TASKS: usize = 16;

fn main() -> NestResult<()> {
    CliApp::new("sparrow-agent")
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
        "Collect host metrics and publish them over MQTT until stopped"
    }

    fn configure(&self, cmd: Command) -> Command {
        cmd
    }

    async fn run_async(&self, ctx: &AppContext, _matches: &ArgMatches) -> NestResult<()> {
        let config_service = ctx.service::<ConfigService>()?;
        let agent_config = AgentConfig::from_config_service(config_service)?;

        let client = MqttClient::connect(&build_mqtt_config(&agent_config)).await?;

        publish_register_message(&client, &agent_config).await?;

        let sink: Arc<dyn BatchSink> = Arc::new(Publisher::new(client.clone(), &agent_config));
        let manager = ctx.service::<TaskManagerService>()?.clone();

        manager
            .spawn(HeartbeatTask::new(client.clone(), &agent_config))
            .await?;

        // ConfigReload spawns the local-defaults CollectorTask set itself
        // (at Task::run start, before subscribing) — not a separate loop
        // here — so its own bookkeeping of what's running stays accurate
        // from the very first retained config message onward. See
        // ConfigReload's doc comment for why splitting this across main.rs
        // and ConfigReload used to be a real bug.
        manager
            .spawn(ConfigReload::new(
                client.clone(),
                &agent_config,
                Arc::clone(&sink),
                manager.clone(),
            ))
            .await?;

        tracing::info!(
            host_id = %agent_config.host_id,
            "sparrow-agent running: collectors (via config_reload), heartbeat, config_reload"
        );

        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received, stopping");

        // Task cancellation on shutdown is handled by nest-task-runtime's own
        // `TaskManagerLifecycle::on_shutdown` (calls `cancel_all()`), which
        // the framework runs automatically after this returns — no need to
        // cancel anything here directly.
        Ok(())
    }
}

/// Builds the agent's `MqttConfig`, including its Last-Will-and-Testament.
///
/// The LWT publishes an **empty** retained payload on the same topic normal
/// heartbeats use (`Topics::heartbeat(host_id)`) — deliberately not a second
/// field bolted onto `HeartbeatMessage` for an "online"/"offline" flag,
/// since an empty payload is already an unambiguous, standard MQTT idiom for
/// "nothing here" that a future ingest consumer can check for without
/// needing to deserialize anything. Ground truth: `docs/plans/phase-7-server.md`'s
/// `offline_watch.rs` section says this LWT "should mark a host offline
/// near-instantly on an unclean disconnect" but Phase 7 doesn't exist yet in
/// this codebase to confirm the exact payload contract against — this is
/// the simplest choice consistent with that description, not a guess backed
/// by Phase 7 source.
fn build_mqtt_config(config: &AgentConfig) -> MqttConfig {
    MqttConfig::new(&config.broker_host, config.broker_port, &config.host_id).with_last_will(
        LastWillConfig {
            topic: Topics::heartbeat(&config.host_id),
            payload: Vec::new(),
            qos: MqttQos::AtLeastOnce,
            retain: true,
        },
    )
}

/// Publishes a one-time `RegisterMessage` on `Topics::register(host_id)`.
///
/// Not mentioned anywhere in `docs/plans/phase-6-agent.md` (grepped the
/// whole file for "register" — zero hits), but `phase-7-server.md`'s
/// `ingest.rs` section explicitly subscribes to `Topics::all_register()`
/// and calls `HostRegistry::upsert_on_register` on each message — the only
/// plausible publisher for that is the agent, and `main.rs` is the only
/// place in this crate positioned to do it. Retained, so a server that
/// (re)starts after this agent registered still receives its identity
/// without the agent needing to notice and re-publish.
async fn publish_register_message(client: &MqttClient, config: &AgentConfig) -> NestResult<()> {
    let hostname = sysinfo::System::host_name().unwrap_or_else(|| config.host_id.clone());
    let message = RegisterMessage {
        host_id: config.host_id.clone(),
        hostname,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    client
        .publish(
            &Topics::register(&config.host_id),
            message.to_payload(),
            MqttQos::AtLeastOnce,
            true,
        )
        .await
}

/// Waits for Ctrl+C or (on Unix) SIGTERM. Mirrors `nest-http-serve`'s
/// `shutdown_signal` (`server.rs`) rather than inventing a second version of
/// the same idiom.
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};

        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
