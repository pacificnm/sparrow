use std::collections::VecDeque;

use nest_error::NestResult;
use nest_mqtt::{MqttClient, MqttQos};
use sparrow_core::collector::MetricItem;
use sparrow_core::transport::{DataBatch, Topics};
use tokio::sync::Mutex;

use crate::config::AgentConfig;

/// Cap on batches held while the broker is unreachable. Oldest batches are
/// dropped first once the buffer is full — bounded memory over completeness.
const MAX_BUFFERED_BATCHES: usize = 100;

/// Publishes `DataBatch`es for a host, buffering through broker outages.
///
/// If `publish` fails, the batch is buffered (rather than dropped or retried
/// inline) and buffered batches are flushed opportunistically on the next
/// call, before the new batch is sent. This is the only place in the agent
/// that buffers for outages — `CollectorTask` does not retry on its own.
pub struct Publisher {
    client: MqttClient,
    host_id: String,
    buffer: Mutex<VecDeque<DataBatch>>,
}

impl Publisher {
    pub fn new(client: MqttClient, config: &AgentConfig) -> Self {
        Self {
            client,
            host_id: config.host_id.clone(),
            buffer: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn publish_batch(&self, collector: &str, items: Vec<MetricItem>) -> NestResult<()> {
        let batch = DataBatch {
            host_id: self.host_id.clone(),
            collector: collector.to_string(),
            items,
        };

        let mut buffer = self.buffer.lock().await;
        self.flush_locked(&mut buffer).await;

        match self.send(&batch).await {
            Ok(()) => Ok(()),
            Err(err) => {
                Self::push_bounded(&mut buffer, batch);
                Err(err)
            }
        }
    }

    /// Drains buffered batches in order, oldest first, stopping at the first
    /// failure (and putting it back) so ordering and at-least-once delivery
    /// are preserved rather than skipping ahead past a still-unreachable broker.
    async fn flush_locked(&self, buffer: &mut VecDeque<DataBatch>) {
        while let Some(batch) = buffer.pop_front() {
            if self.send(&batch).await.is_err() {
                buffer.push_front(batch);
                break;
            }
        }
    }

    fn push_bounded(buffer: &mut VecDeque<DataBatch>, batch: DataBatch) {
        if buffer.len() >= MAX_BUFFERED_BATCHES {
            buffer.pop_front();
        }
        buffer.push_back(batch);
    }

    async fn send(&self, batch: &DataBatch) -> NestResult<()> {
        let topic = Topics::data(&self.host_id);
        self.client
            .publish(&topic, batch.to_payload(), MqttQos::AtLeastOnce, false)
            .await
    }
}
