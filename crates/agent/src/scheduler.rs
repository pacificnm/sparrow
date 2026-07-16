use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nest_error::NestResult;
use nest_task::{Task, TaskContext};
use sparrow_core::collector::{Collector, MetricItem};
use tokio::sync::Mutex;

use crate::interval_task::run_on_interval;

/// The seam [`CollectorTask`] publishes through.
///
/// The spec's sketch names a concrete `crate::publisher::Publisher` field,
/// but that type (Issue 6.3, with its own bounded outage-buffering design)
/// doesn't exist in this crate yet. This trait captures exactly the call
/// contract the sketch uses (`publish_batch(collector_name, items)`) without
/// guessing at `Publisher`'s internals — Issue 6.3 implements it for the
/// real MQTT-backed `Publisher`.
#[async_trait]
pub trait BatchSink: Send + Sync {
    /// Publishes one collector's batch of items.
    async fn publish_batch(
        &self,
        collector: &'static str,
        items: Vec<MetricItem>,
    ) -> NestResult<()>;
}

/// A long-running task that owns one [`Collector`] and runs it on its own
/// interval until cancelled.
///
/// `nest-task`'s `TaskManager::spawn` runs a `Task` exactly once — there is
/// no built-in interval scheduling — so the interval loop lives inside
/// `run` itself, one long-lived `CollectorTask` per collector.
pub struct CollectorTask {
    collector: Mutex<Box<dyn Collector>>,
    // Task::name(&self) -> &'static str is synchronous, so it can't lock the
    // async Mutex above to read Collector::name() at call time. Cached once
    // at construction instead — Collector::name() is stable for the
    // collector's lifetime (Phase 4), so this loses nothing.
    name: &'static str,
    interval: Duration,
    sink: Arc<dyn BatchSink>,
}

impl CollectorTask {
    /// Creates a task that runs `collector` every `interval` until
    /// cancelled, publishing each batch through `sink`.
    pub fn new(
        collector: Box<dyn Collector>,
        interval: Duration,
        sink: Arc<dyn BatchSink>,
    ) -> Self {
        let name = collector.name();
        Self {
            collector: Mutex::new(collector),
            name,
            interval,
            sink,
        }
    }
}

#[async_trait]
impl Task for CollectorTask {
    type Output = ();

    fn name(&self) -> &'static str {
        self.name
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        run_on_interval(self.interval, ctx.cancel_token(), || async {
            let collected = {
                let mut collector = self.collector.lock().await;
                collector.collect()
            };

            match collected {
                Ok(items) => {
                    if let Err(err) = self.sink.publish_batch(self.name, items).await {
                        tracing::warn!(error = %err, collector = self.name, "failed to publish batch");
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, collector = self.name, "collector failed");
                }
            }
        })
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nest_task::{CancelToken, ProgressReporter, TaskId};
    use sparrow_core::collector::{CollectorError, ValueType};

    use super::*;

    struct CountingCollector {
        calls: Arc<AtomicUsize>,
    }

    impl Collector for CountingCollector {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn default_interval_secs(&self) -> u64 {
            1
        }

        fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![MetricItem {
                key: "fake.count".to_string(),
                value_type: ValueType::Integer,
                value: "1".to_string(),
                tags: Default::default(),
                timestamp_ms: 0,
            }])
        }
    }

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

    fn test_context(cancel: CancelToken) -> TaskContext {
        let app = nest_core::AppBuilder::new()
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
    async fn run_collects_roughly_once_per_interval_until_cancelled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let collector = CountingCollector {
            calls: Arc::clone(&calls),
        };
        let task = Arc::new(CollectorTask::new(
            Box::new(collector),
            Duration::from_secs(1),
            Arc::new(NoopSink),
        ));

        let cancel = CancelToken::new();
        let ctx = test_context(cancel.clone());

        let running = tokio::spawn({
            let task = Arc::clone(&task);
            async move { task.run(ctx).await }
        });

        tokio::time::sleep(Duration::from_millis(3300)).await;
        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("task should exit shortly after cancellation")
            .expect("task should not panic")
            .expect("run should return Ok");

        let count = calls.load(Ordering::SeqCst);
        assert!(
            (2..=5).contains(&count),
            "expected roughly 3 collect() calls in ~3.3s at a 1s interval, got {count}"
        );
    }
}
