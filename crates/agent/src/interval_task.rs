//! Shared interval-loop-with-cancel-poll driver.
//!
//! `nest-task`'s `TaskManager::spawn` runs a `Task` exactly once — there is
//! no built-in interval scheduling — so every long-running, periodic task in
//! this crate ([`crate::scheduler::CollectorTask`], [`crate::heartbeat::HeartbeatTask`])
//! drives its own interval loop. Rather than each hand-rolling one, both call
//! [`run_on_interval`] so the cancel-poll shape exists in exactly one place.

use std::future::Future;
use std::time::{Duration, Instant};

use nest_task::CancelToken;

/// Cadence at which cancellation is polled, independent of the wrapped
/// task's own interval.
///
/// `CancelToken::is_cancelled()` is a poll, not an awaitable future, so a
/// loop that only checks it once per tick interval (e.g. `disk`'s 60s) would
/// leave shutdown waiting up to that long. Polling on a short, fixed cadence
/// independent of `interval` bounds that latency to ~1s regardless of how
/// long any given task's interval is.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Runs `tick` once every `interval` — the first tick fires immediately,
/// without waiting a full interval first — until `cancel` is cancelled,
/// polling cancellation on [`CANCEL_POLL_INTERVAL`] independent of `interval`.
pub async fn run_on_interval<F, Fut>(interval: Duration, cancel: &CancelToken, mut tick: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut last_run = Instant::now() - interval;

    loop {
        if cancel.is_cancelled() {
            return;
        }

        if last_run.elapsed() >= interval {
            tick().await;
            last_run = Instant::now();
        }

        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn ticks_roughly_once_per_interval_until_cancelled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();

        let running = tokio::spawn({
            let calls = Arc::clone(&calls);
            let cancel = cancel.clone();
            async move {
                run_on_interval(Duration::from_secs(1), &cancel, || {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            }
        });

        tokio::time::sleep(Duration::from_millis(3300)).await;
        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("loop should exit shortly after cancellation")
            .expect("loop should not panic");

        let count = calls.load(Ordering::SeqCst);
        assert!(
            (2..=5).contains(&count),
            "expected roughly 3 ticks in ~3.3s at a 1s interval, got {count}"
        );
    }

    #[tokio::test]
    async fn first_tick_fires_immediately_not_after_a_full_interval() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();

        let running = tokio::spawn({
            let calls = Arc::clone(&calls);
            let cancel = cancel.clone();
            async move {
                run_on_interval(Duration::from_secs(60), &cancel, || {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            }
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("loop should exit shortly after cancellation")
            .expect("loop should not panic");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "first tick should fire immediately, not after waiting a full 60s interval"
        );
    }
}
