use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use nest_error::NestResult;
use nest_mqtt::{MqttClient, MqttQos};
use nest_task::{Task, TaskContext, TaskHandle, TaskManager};
use nest_task_runtime::TaskManagerService;
use sparrow_core::collector::Collector;
use sparrow_core::collectors::default_collectors;
use sparrow_core::config::AgentConfigOverride;
use sparrow_core::transport::Topics;
use tokio::sync::Mutex;

use crate::config::AgentConfig;
use crate::scheduler::{BatchSink, CollectorTask};

/// Cadence at which [`ConfigReload::run`] polls for cancellation between
/// incoming config messages. Same bound as `interval_task`'s cancel-poll,
/// duplicated as a plain constant rather than imported: this loop is driven
/// by an incoming message stream, not a fixed tick, so `run_on_interval`'s
/// shape doesn't fit here — only the ~1s responsiveness bound is shared.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// One collector's currently-running `CollectorTask`, tracked so a later
/// config change can be diffed against it.
struct RunningCollector {
    handle: TaskHandle<()>,
    interval_secs: u64,
}

/// Subscribes once (at [`Task::run`] start) to `Topics::config(host_id)` — a
/// **retained** topic, so a message arrives immediately even if it was
/// published while this agent was offline — and applies each
/// [`AgentConfigOverride`] it receives to the live `CollectorTask` set:
///
/// - Collectors newly in `disabled_collectors` are cancelled.
/// - Collectors newly absent from `disabled_collectors` are started.
/// - Collectors whose effective interval changed are cancelled and
///   restarted with the new interval — their interval is not mutated in
///   place (meaningfully more complex for marginal benefit at this scale).
/// - Collectors whose desired state didn't change are left completely
///   alone, including whatever persistent state their `Collector` instance
///   holds (e.g. `cpu`'s `sysinfo::System`).
///
/// Dynamically-started collectors are spawned through the same
/// [`TaskManagerService`] `main.rs` uses for the initial task set (not a raw
/// `tokio::spawn`) — that's what gives them a real `TaskContext` (built from
/// the app's actual `Arc<AppContext>`, which isn't otherwise reachable from
/// here) and makes them show up in the same registry/cancellation path as
/// every other task in the process, not a separate untracked set.
pub struct ConfigReload {
    client: MqttClient,
    host_id: String,
    sink: Arc<dyn BatchSink>,
    manager: TaskManagerService,
    running: Mutex<HashMap<&'static str, RunningCollector>>,
}

impl ConfigReload {
    /// Creates a task that reconciles the running `CollectorTask` set
    /// against `Topics::config(config.host_id)`, publishing batches through
    /// `sink` and spawning each replacement `CollectorTask` through `manager`.
    pub fn new(
        client: MqttClient,
        config: &AgentConfig,
        sink: Arc<dyn BatchSink>,
        manager: TaskManagerService,
    ) -> Self {
        Self {
            client,
            host_id: config.host_id.clone(),
            sink,
            manager,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Reconciles the running `CollectorTask` set against `override_`,
    /// leaving unchanged collectors' tasks untouched.
    async fn apply(&self, override_: &AgentConfigOverride) {
        let mut running = self.running.lock().await;

        let mut desired: HashMap<&'static str, (Box<dyn Collector>, u64)> = HashMap::new();
        for collector in default_collectors() {
            let name = collector.name();
            if override_
                .disabled_collectors
                .iter()
                .any(|disabled| disabled == name)
            {
                continue;
            }

            let interval = override_
                .collector_intervals
                .get(name)
                .copied()
                .unwrap_or_else(|| collector.default_interval_secs());

            desired.insert(name, (collector, interval));
        }

        // Stop anything no longer desired, or whose interval changed (it
        // gets a fresh task below with the new interval).
        let to_stop: Vec<&'static str> = running
            .iter()
            .filter(|(name, existing)| match desired.get(*name) {
                None => true,
                Some((_, interval)) => *interval != existing.interval_secs,
            })
            .map(|(name, _)| *name)
            .collect();

        for name in to_stop {
            if let Some(existing) = running.remove(name) {
                existing.handle.cancel();
            }
        }

        // Start anything newly desired: brand new, or just stopped above
        // because its interval changed.
        for (name, (collector, interval)) in desired {
            if running.contains_key(name) {
                continue;
            }

            let task = CollectorTask::new(
                collector,
                Duration::from_secs(interval),
                Arc::clone(&self.sink),
            );

            match self.manager.spawn(task).await {
                Ok(handle) => {
                    running.insert(
                        name,
                        RunningCollector {
                            handle,
                            interval_secs: interval,
                        },
                    );
                }
                Err(err) => {
                    tracing::warn!(error = %err, collector = name, "failed to spawn collector task");
                }
            }
        }
    }
}

#[async_trait]
impl Task for ConfigReload {
    type Output = ();

    fn name(&self) -> &'static str {
        "config_reload"
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        let messages = self
            .client
            .subscribe(&Topics::config(&self.host_id), MqttQos::AtLeastOnce)
            .await?;
        // The stream nest-mqtt returns isn't Unpin (its filter_map wraps a
        // non-Unpin async block), but `StreamExt::next()` requires `Unpin` —
        // pin it in place here rather than boxing.
        tokio::pin!(messages);

        loop {
            if ctx.cancel_token().is_cancelled() {
                return Ok(());
            }

            match tokio::time::timeout(CANCEL_POLL_INTERVAL, messages.next()).await {
                Ok(Some(message)) => match AgentConfigOverride::from_payload(&message.payload) {
                    Ok(override_) => self.apply(&override_).await,
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to parse config message; ignoring")
                    }
                },
                // Subscription stream ended (client dropped) — nothing left to react to.
                Ok(None) => return Ok(()),
                // No message this tick; loop back and re-check cancellation.
                Err(_timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nest_core::AppBuilder;
    use nest_error::NestResult;
    use nest_mqtt::MqttConfig;
    use nest_task::TaskStatus;
    use nest_task_runtime::TaskManagerConfig;
    use sparrow_core::collector::MetricItem;

    use super::*;

    struct NoopSink;

    #[async_trait]
    impl BatchSink for NoopSink {
        async fn publish_batch(
            &self,
            _collector: &'static str,
            _items: Vec<MetricItem>,
        ) -> NestResult<()> {
            Ok(())
        }
    }

    async fn test_reload() -> ConfigReload {
        let client = MqttClient::connect(&MqttConfig {
            client_id: "config-reload-test".to_string(),
            broker_host: "127.0.0.1".to_string(),
            broker_port: 1883,
            keep_alive_secs: 5,
            username: None,
            password: None,
            last_will: None,
            capacity: 16,
        })
        .await
        .expect("client construction does not require a live connection");

        let config = AgentConfig {
            host_id: "config-reload-test-host".to_string(),
            broker_host: "127.0.0.1".to_string(),
            broker_port: 1883,
            collector_intervals: Default::default(),
            disabled_collectors: Vec::new(),
        };

        let app = AppBuilder::new()
            .build()
            .expect("empty app context")
            .context;
        let manager = TaskManagerService::new(
            tokio::runtime::Handle::current(),
            TaskManagerConfig::default(),
        );
        manager.set_context(app);

        ConfigReload::new(client, &config, Arc::new(NoopSink), manager)
    }

    #[tokio::test]
    async fn apply_starts_enabled_collectors_with_effective_intervals() {
        let reload = test_reload().await;

        reload
            .apply(&AgentConfigOverride {
                disabled_collectors: vec!["disk".to_string()],
                collector_intervals: BTreeMap::from([("cpu".to_string(), 5)]),
            })
            .await;

        let running = reload.running.lock().await;
        assert!(!running.contains_key("disk"), "disk was disabled");
        assert_eq!(
            running
                .get("cpu")
                .expect("cpu should be running")
                .interval_secs,
            5
        );
        assert!(
            running.contains_key("memory"),
            "memory should use its default interval"
        );
    }

    #[tokio::test]
    async fn apply_stops_newly_disabled_and_leaves_others_running() {
        let reload = test_reload().await;

        reload.apply(&AgentConfigOverride::default()).await;

        let cpu_id_before = {
            let running = reload.running.lock().await;
            assert!(running.contains_key("memory"));
            running.get("cpu").expect("cpu running").handle.id()
        };

        reload
            .apply(&AgentConfigOverride {
                disabled_collectors: vec!["memory".to_string()],
                collector_intervals: BTreeMap::new(),
            })
            .await;

        let running = reload.running.lock().await;
        assert!(
            !running.contains_key("memory"),
            "memory should have been stopped"
        );
        let cpu = running.get("cpu").expect("cpu should still be running");
        assert_eq!(
            cpu.handle.id(),
            cpu_id_before,
            "cpu's original task should not have been touched by an unrelated change"
        );
        assert_ne!(cpu.handle.status(), TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn apply_restarts_a_collector_when_its_interval_changes() {
        let reload = test_reload().await;

        reload.apply(&AgentConfigOverride::default()).await;

        let cpu_id_before = {
            let running = reload.running.lock().await;
            running.get("cpu").expect("cpu running").handle.id()
        };

        reload
            .apply(&AgentConfigOverride {
                disabled_collectors: Vec::new(),
                collector_intervals: BTreeMap::from([("cpu".to_string(), 99)]),
            })
            .await;

        let running = reload.running.lock().await;
        let cpu_after = running.get("cpu").expect("cpu should still be running");
        assert_eq!(cpu_after.interval_secs, 99);
        assert_ne!(
            cpu_after.handle.id(),
            cpu_id_before,
            "the old cpu task should have been replaced, not mutated in place"
        );
        assert_ne!(
            cpu_after.handle.status(),
            TaskStatus::Cancelled,
            "the new cpu task should be running"
        );
    }

    #[tokio::test]
    async fn unchanged_collectors_are_not_touched_across_repeated_apply_calls() {
        let reload = test_reload().await;

        reload.apply(&AgentConfigOverride::default()).await;
        let cpu_id_first = {
            let running = reload.running.lock().await;
            running.get("cpu").expect("cpu running").handle.id()
        };

        // Same config applied again — nothing should be stopped or restarted.
        reload.apply(&AgentConfigOverride::default()).await;

        let running = reload.running.lock().await;
        assert_eq!(
            running.get("cpu").expect("cpu running").handle.id(),
            cpu_id_first,
            "re-applying an unchanged config must not disturb already-running collectors"
        );
        assert_eq!(
            running.len(),
            3,
            "cpu, memory, and disk should all still be running"
        );
    }
}
