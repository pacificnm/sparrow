use std::time::Duration;

use async_trait::async_trait;
use nest_error::NestResult;
use nest_task::{Task, TaskContext};
use sparrow_core::interval_task::run_on_interval;
use sparrow_core::storage::HostRegistry;

/// Production sweep cadence.
const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Production staleness threshold: ~3x Phase 6's 15s heartbeat interval —
/// generous enough to tolerate a missed heartbeat or two from ordinary
/// network jitter without falsely marking a healthy host offline.
const DEFAULT_STALE_AFTER_SECS: i64 = 45;

/// Periodic backstop that marks hosts offline once their `last_seen` goes
/// stale.
///
/// **This is not the primary offline-detection mechanism.** MQTT's LWT
/// (configured on the agent's `MqttConfig`, Phase 6 —
/// `crates/agent/src/main.rs`'s `build_mqtt_config`) marks a host offline
/// near-instantly the moment its TCP connection actually dies, whether that
/// disconnect was clean or unclean. This sweep exists for the one case LWT
/// cannot cover: an agent process that hangs *without its connection ever
/// dying at all* — e.g. a frozen process still holding an open, idle TCP
/// socket. No disconnect ever fires in that case, so the broker has no
/// reason to publish the Will. Polling `last_seen` on a fixed cadence and
/// comparing it against a stale threshold catches exactly that gap; every
/// other disconnect scenario is already handled near-instantly by LWT
/// before this sweep would ever run.
pub struct OfflineWatch {
    registry: HostRegistry,
    sweep_interval: Duration,
    stale_after_secs: i64,
}

impl OfflineWatch {
    /// Creates a sweep that marks stale hosts offline via `registry` every
    /// `sweep_interval`, once `last_seen` exceeds `stale_after_secs`.
    ///
    /// Cadence and threshold are constructor parameters, not hardcoded —
    /// same reason `CollectorTask`/`HeartbeatTask` (Phase 6) take their
    /// interval this way rather than baking it into the type: a
    /// milestone-closing test proving this sweep's `Task::run` loop
    /// actually fires and calls through to `mark_stale_offline` shouldn't
    /// have to wait out a real 30s production interval to do it.
    pub fn new(registry: HostRegistry, sweep_interval: Duration, stale_after_secs: i64) -> Self {
        Self {
            registry,
            sweep_interval,
            stale_after_secs,
        }
    }

    /// Creates a sweep using Sparrow's production cadence/threshold
    /// ([`DEFAULT_SWEEP_INTERVAL`], [`DEFAULT_STALE_AFTER_SECS`]).
    pub fn with_defaults(registry: HostRegistry) -> Self {
        Self::new(registry, DEFAULT_SWEEP_INTERVAL, DEFAULT_STALE_AFTER_SECS)
    }
}

#[async_trait]
impl Task for OfflineWatch {
    type Output = ();

    fn name(&self) -> &'static str {
        "offline_watch"
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        run_on_interval(self.sweep_interval, ctx.cancel_token(), || async {
            match self
                .registry
                .mark_stale_offline(self.stale_after_secs)
                .await
            {
                Ok(0) => {}
                Ok(marked) => tracing::info!(marked, "offline sweep marked stale hosts offline"),
                Err(err) => tracing::warn!(error = %err, "offline sweep failed"),
            }
        })
        .await;

        Ok(())
    }
}
