use std::time::Duration;

use async_trait::async_trait;
use nest_error::NestResult;
use nest_task::{Task, TaskContext};
use sparrow_core::interval_task::run_on_interval;
use sparrow_core::storage::HostRegistry;

/// How often the sweep runs.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// A host is considered stale (and gets marked offline) once its `last_seen`
/// is older than this. ~3x Phase 6's 15s heartbeat interval — generous
/// enough to tolerate a missed heartbeat or two from ordinary network
/// jitter without falsely marking a healthy host offline.
const STALE_AFTER_SECS: i64 = 45;

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
}

impl OfflineWatch {
    /// Creates a sweep that marks stale hosts offline via `registry` every
    /// [`SWEEP_INTERVAL`].
    pub fn new(registry: HostRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Task for OfflineWatch {
    type Output = ();

    fn name(&self) -> &'static str {
        "offline_watch"
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        run_on_interval(SWEEP_INTERVAL, ctx.cancel_token(), || async {
            match self.registry.mark_stale_offline(STALE_AFTER_SECS).await {
                Ok(0) => {}
                Ok(marked) => tracing::info!(marked, "offline sweep marked stale hosts offline"),
                Err(err) => tracing::warn!(error = %err, "offline sweep failed"),
            }
        })
        .await;

        Ok(())
    }
}
