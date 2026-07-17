//! `AlertingTask`: the periodic rule-evaluation loop that opens and resolves
//! [`Problem`]s.
//!
//! Same interval-loop-with-cancel-poll pattern as `sparrow-agent`'s
//! `CollectorTask`/`HeartbeatTask` (Phase 6) and `sparrow-server`'s own
//! `OfflineWatch` (Phase 7) — this is the third use of that shape. Per the
//! issue's own instruction: if a fourth use shows up in a later phase,
//! that's the trigger to factor it into a shared helper in `sparrow-core`,
//! not before (`sparrow_core::interval_task::run_on_interval` already is
//! that shared helper for the loop shape itself; what's "the third use" here
//! is the broader pattern of a `Task` wrapping it).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nest_error::{NestError, NestResult};
use nest_task::{Task, TaskContext};
use sparrow_core::interval_task::run_on_interval;
use sparrow_core::time::now_ms;
use sparrow_core::trigger::{Problem, Rule};
use sqlx::PgPool;

/// Periodically evaluates every enabled [`Rule`] against each applicable
/// host's latest metric value, opening/updating/resolving [`Problem`]s.
///
/// **`sustained_for_secs` tracking is in-memory only** (`condition_since`
/// below), not persisted to a `condition_since` column — a deliberate v1
/// choice, not an oversight: it's simpler, and matches how most simple
/// monitoring tools behave. The real cost is that a rule sitting 20s into a
/// 30s sustain window resets to 0 on server restart (it has to
/// re-accumulate the full window from scratch). Acceptable for v1; if that
/// ever becomes a real problem, the fix is a persisted column read back on
/// startup, not a rewrite of the tracking logic itself.
pub struct AlertingTask {
    pool: PgPool,
    /// Evaluation cadence — independent of any collector's interval, any
    /// individual rule's `sustained_for_secs`, or the agent's heartbeat.
    interval: Duration,
    /// When each `(rule_id, host_id)` pair's condition *first* became true,
    /// for rules with `sustained_for_secs > 0` that haven't opened a
    /// `Problem` yet. Removed the moment the condition goes false again (so
    /// a later re-trip starts a fresh window, not a stale one) or the
    /// instant a `Problem` actually opens (no longer needed once `Open` is
    /// the source of truth).
    condition_since: Mutex<HashMap<(i64, String), Instant>>,
}

impl AlertingTask {
    pub fn new(pool: PgPool, interval: Duration) -> Self {
        Self {
            pool,
            interval,
            condition_since: Mutex::new(HashMap::new()),
        }
    }

    async fn evaluate_once(&self) -> NestResult<()> {
        let rules = fetch_enabled_rules(&self.pool).await?;

        for rule in rules {
            let hosts = match &rule.host_id {
                Some(host_id) => vec![host_id.clone()],
                None => fetch_all_host_ids(&self.pool).await?,
            };

            for host_id in hosts {
                match fetch_latest_value(&self.pool, &host_id, &rule.item_key).await {
                    Ok(Some(latest)) => {
                        if let Err(err) = self.evaluate_rule_for_host(&rule, &host_id, latest).await
                        {
                            tracing::warn!(
                                error = %err,
                                rule_id = rule.id,
                                host_id = %host_id,
                                "failed to evaluate rule for host"
                            );
                        }
                    }
                    // No data for this (host, item_key) yet — nothing to
                    // evaluate against, not an error.
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            rule_id = rule.id,
                            host_id = %host_id,
                            item_key = %rule.item_key,
                            "failed to fetch latest value for rule"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    async fn evaluate_rule_for_host(
        &self,
        rule: &Rule,
        host_id: &str,
        value: f64,
    ) -> NestResult<()> {
        let condition_true = rule.operator.evaluate(value, rule.threshold);
        let existing_open = fetch_open_problem(&self.pool, rule.id, host_id).await?;

        match (condition_true, existing_open) {
            (true, None) if rule.sustained_for_secs <= 0 => {
                self.clear_condition_since(rule.id, host_id);
                open_problem(&self.pool, rule, host_id, value).await?;
            }
            (true, None) => {
                let first_true_at = self.mark_condition_since(rule.id, host_id);
                let sustain = Duration::from_secs(rule.sustained_for_secs as u64);
                if first_true_at.elapsed() >= sustain {
                    self.clear_condition_since(rule.id, host_id);
                    open_problem(&self.pool, rule, host_id, value).await?;
                }
                // Not sustained long enough yet — nothing to do this pass.
            }
            (false, Some(problem)) => {
                // Condition no longer true: reset any in-progress sustain
                // tracking (irrelevant now that there's an open Problem to
                // resolve, but also correct if this fires for a rule that
                // never actually finished its sustain window elsewhere).
                self.clear_condition_since(rule.id, host_id);
                resolve_problem(&self.pool, problem.id).await?;
            }
            (true, Some(problem)) => {
                // Already open — no duplicate Problem (the partial unique
                // index from Issue 8.2 backs this up), just refresh the
                // observed value.
                update_last_value(&self.pool, problem.id, value).await?;
            }
            (false, None) => {
                // Nothing open and the condition isn't true. Still clear:
                // covers a rule that was partway through its sustain window
                // (condition_since set) and then went false before ever
                // opening a Problem.
                self.clear_condition_since(rule.id, host_id);
            }
        }

        Ok(())
    }

    /// Returns the instant `(rule_id, host_id)`'s condition first became
    /// true, recording it now if this is the first time. Calling this
    /// repeatedly while the condition stays true returns the *same*
    /// instant every time — it does not reset the clock on each evaluation
    /// pass, which is the whole point of tracking wall-clock duration
    /// instead of "the last N evaluations were true."
    fn mark_condition_since(&self, rule_id: i64, host_id: &str) -> Instant {
        let mut map = self
            .condition_since
            .lock()
            .expect("condition_since mutex poisoned");
        *map.entry((rule_id, host_id.to_string()))
            .or_insert_with(Instant::now)
    }

    fn clear_condition_since(&self, rule_id: i64, host_id: &str) {
        let mut map = self
            .condition_since
            .lock()
            .expect("condition_since mutex poisoned");
        map.remove(&(rule_id, host_id.to_string()));
    }
}

#[async_trait]
impl Task for AlertingTask {
    type Output = ();

    fn name(&self) -> &'static str {
        "alerting"
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        run_on_interval(self.interval, ctx.cancel_token(), || async {
            if let Err(err) = self.evaluate_once().await {
                // A whole-pass failure (e.g. the DB is unreachable) — log
                // and let the next tick retry, rather than let one bad pass
                // kill the task permanently.
                tracing::warn!(error = %err, "alerting evaluation pass failed");
            }
        })
        .await;

        Ok(())
    }
}

fn db_error(context: &str, error: sqlx::Error) -> NestError {
    NestError::unknown(format!("{context}: {error}"))
}

async fn fetch_enabled_rules(pool: &PgPool) -> NestResult<Vec<Rule>> {
    sqlx::query_as::<_, Rule>(
        "SELECT id, host_id, item_key, operator, threshold, severity, sustained_for_secs, enabled
         FROM rules
         WHERE enabled = true",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| db_error("failed to fetch enabled rules", error))
}

async fn fetch_all_host_ids(pool: &PgPool) -> NestResult<Vec<String>> {
    sqlx::query_scalar("SELECT host_id FROM hosts")
        .fetch_all(pool)
        .await
        .map_err(|error| db_error("failed to fetch host ids", error))
}

/// Returns the most recent value for `(host_id, item_key)`, parsed as
/// `f64`, or `None` if no data exists yet. `metric_history.value` is
/// stored as `TEXT` (see `sparrow_core::storage`'s doc comment on why), so
/// a rule pointed at a non-numeric key (e.g. `cpu.governor`, a `Text`-typed
/// item) produces a parse error here — that's a per-rule misconfiguration,
/// not a DB failure, and the caller (`evaluate_once`) logs and skips it
/// rather than letting it abort every other rule's evaluation this pass.
async fn fetch_latest_value(
    pool: &PgPool,
    host_id: &str,
    item_key: &str,
) -> NestResult<Option<f64>> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value FROM metric_history
         WHERE host_id = $1 AND key = $2
         ORDER BY ts DESC
         LIMIT 1",
    )
    .bind(host_id)
    .bind(item_key)
    .fetch_optional(pool)
    .await
    .map_err(|error| db_error("failed to fetch latest metric value", error))?;

    match raw {
        Some(raw) => raw.parse::<f64>().map(Some).map_err(|error| {
            NestError::unknown(format!("non-numeric metric value {raw:?}: {error}"))
        }),
        None => Ok(None),
    }
}

async fn fetch_open_problem(
    pool: &PgPool,
    rule_id: i64,
    host_id: &str,
) -> NestResult<Option<Problem>> {
    sqlx::query_as::<_, Problem>(
        "SELECT id, rule_id, host_id, status, opened_at, resolved_at, last_value
         FROM problems
         WHERE rule_id = $1 AND host_id = $2 AND status = 'open'",
    )
    .bind(rule_id)
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| db_error("failed to fetch open problem", error))
}

async fn open_problem(
    pool: &PgPool,
    rule: &Rule,
    host_id: &str,
    value: f64,
) -> NestResult<Problem> {
    sqlx::query_as::<_, Problem>(
        "INSERT INTO problems (rule_id, host_id, status, opened_at, last_value)
         VALUES ($1, $2, 'open', $3, $4)
         RETURNING id, rule_id, host_id, status, opened_at, resolved_at, last_value",
    )
    .bind(rule.id)
    .bind(host_id)
    .bind(now_ms())
    .bind(value)
    .fetch_one(pool)
    .await
    .map_err(|error| db_error("failed to open problem", error))
}

async fn resolve_problem(pool: &PgPool, problem_id: i64) -> NestResult<()> {
    sqlx::query("UPDATE problems SET status = 'resolved', resolved_at = $1 WHERE id = $2")
        .bind(now_ms())
        .bind(problem_id)
        .execute(pool)
        .await
        .map_err(|error| db_error("failed to resolve problem", error))?;
    Ok(())
}

async fn update_last_value(pool: &PgPool, problem_id: i64, value: f64) -> NestResult<()> {
    sqlx::query("UPDATE problems SET last_value = $1 WHERE id = $2")
        .bind(value)
        .bind(problem_id)
        .execute(pool)
        .await
        .map_err(|error| db_error("failed to update problem's last_value", error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_task() -> AlertingTask {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        AlertingTask::new(pool, Duration::from_secs(10))
    }

    #[tokio::test]
    async fn mark_condition_since_does_not_reset_on_repeated_calls() {
        let task = test_task();

        let first = task.mark_condition_since(1, "host-a");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = task.mark_condition_since(1, "host-a");

        assert_eq!(
            first, second,
            "repeated true evaluations must not reset the sustain-window clock"
        );
    }

    #[tokio::test]
    async fn mark_condition_since_tracks_rule_host_pairs_independently() {
        let task = test_task();

        let rule1_host_a = task.mark_condition_since(1, "host-a");
        let rule2_host_a = task.mark_condition_since(2, "host-a");
        let rule1_host_b = task.mark_condition_since(1, "host-b");

        // Different keys entirely, so no reason to expect equality, but the
        // real assertion is that setting one didn't clobber the others.
        assert_eq!(task.mark_condition_since(1, "host-a"), rule1_host_a);
        assert_eq!(task.mark_condition_since(2, "host-a"), rule2_host_a);
        assert_eq!(task.mark_condition_since(1, "host-b"), rule1_host_b);
    }

    #[tokio::test]
    async fn clear_condition_since_starts_a_fresh_window_on_next_mark() {
        let task = test_task();

        let first = task.mark_condition_since(1, "host-a");
        task.clear_condition_since(1, "host-a");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let after_clear = task.mark_condition_since(1, "host-a");

        assert!(
            after_clear > first,
            "clearing then re-marking must start a fresh window, not reuse the old timestamp \
             (this is what makes a condition that flips false before sustaining, then true \
             again, require a full fresh sustain window rather than resuming a stale one)"
        );
    }

    #[tokio::test]
    async fn clear_condition_since_on_an_untracked_pair_is_a_no_op() {
        let task = test_task();
        // Must not panic when clearing something that was never marked.
        task.clear_condition_since(999, "never-seen-host");
    }
}
