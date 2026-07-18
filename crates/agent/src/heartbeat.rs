use std::time::Duration;

use async_trait::async_trait;
use nest_error::NestResult;
use nest_mqtt::{MqttClient, MqttQos};
use nest_task::{Task, TaskContext};
use sparrow_core::interval_task::run_on_interval;
use sparrow_core::time::now_ms;
use sparrow_core::transport::{HeartbeatMessage, Topics};

use crate::config::AgentConfig;

/// Heartbeat cadence — hardcoded per spec, not collector-configurable.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// A long-running task that publishes a [`HeartbeatMessage`] on
/// [`Topics::heartbeat`] every [`HEARTBEAT_INTERVAL`] until cancelled.
///
/// Simpler than [`crate::scheduler::CollectorTask`]: no collector, and no
/// outage buffering — a stale, later-resent heartbeat timestamp would be
/// actively misleading (unlike a delayed metric batch, which is still
/// correct data), so a publish failure is just logged and the next tick
/// carries a fresh timestamp on its own. Shares `CollectorTask`'s interval-
/// loop-with-cancel-poll shape via [`run_on_interval`] rather than a second,
/// slightly different copy of that loop.
pub struct HeartbeatTask {
    client: MqttClient,
    host_id: String,
}

impl HeartbeatTask {
    /// Creates a task that publishes `config.host_id`'s heartbeat over
    /// `client` every [`HEARTBEAT_INTERVAL`] until cancelled.
    pub fn new(client: MqttClient, config: &AgentConfig) -> Self {
        Self {
            client,
            host_id: config.host_id.clone(),
        }
    }
}

#[async_trait]
impl Task for HeartbeatTask {
    type Output = ();

    fn name(&self) -> &'static str {
        "heartbeat"
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        run_on_interval(HEARTBEAT_INTERVAL, ctx.cancel_token(), || async {
            let message = HeartbeatMessage {
                host_id: self.host_id.clone(),
                timestamp_ms: now_ms(),
            };

            if let Err(err) = self
                .client
                .publish(
                    &Topics::heartbeat(&self.host_id),
                    message.to_payload(),
                    MqttQos::AtLeastOnce,
                    false,
                )
                .await
            {
                tracing::warn!(error = %err, host_id = %self.host_id, "failed to publish heartbeat");
            }
        })
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use nest_core::AppBuilder;
    use nest_mqtt::{MqttClient, MqttConfig};
    use nest_task::{CancelToken, ProgressReporter, TaskId};

    use super::*;

    fn test_config() -> AgentConfig {
        AgentConfig {
            host_id: "heartbeat-test-host".to_string(),
            broker_host: "127.0.0.1".to_string(),
            broker_port: 1883,
            collector_intervals: Default::default(),
            disabled_collectors: Vec::new(),
        }
    }

    fn test_context(cancel: CancelToken) -> TaskContext {
        let app = AppBuilder::new()
            .build()
            .expect("empty app context")
            .context;
        TaskContext::new(
            TaskId::new(),
            app,
            cancel,
            ProgressReporter::new(Arc::new(|_progress| {})),
            tracing::Span::none(),
        )
    }

    #[tokio::test]
    async fn run_exits_promptly_after_cancellation() {
        // No broker is running in this test — publish() will fail and be
        // logged, which is exactly the "no buffering" behavior under test.
        // What matters here is the interval-loop's cancel responsiveness,
        // shared with CollectorTask and already timing-tested there.
        let config = test_config();
        let client = MqttClient::connect(&MqttConfig {
            client_id: "heartbeat-test".to_string(),
            broker_host: config.broker_host.clone(),
            broker_port: config.broker_port,
            keep_alive_secs: 5,
            username: None,
            password: None,
            last_will: None,
            capacity: 16,
            tls: None,
        })
        .await
        .expect("client construction does not require a live connection");

        let task = Arc::new(HeartbeatTask::new(client, &config));
        assert_eq!(task.name(), "heartbeat");

        let cancel = CancelToken::new();
        let ctx = test_context(cancel.clone());

        let running = tokio::spawn({
            let task = Arc::clone(&task);
            async move { task.run(ctx).await }
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("task should exit shortly after cancellation")
            .expect("task should not panic")
            .expect("run should return Ok");
    }
}
