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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nest_error::{NestError, NestResult};
use nest_http_client::{HttpClientService, HttpRequest};
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
    /// Notified (via `NotificationSink::notify`) whenever `open_problem`
    /// actually opens a new `Problem` — tied to that specific state
    /// transition, not to every evaluation pass (see `open_problem`).
    sinks: Vec<Arc<dyn NotificationSink>>,
}

impl AlertingTask {
    pub fn new(pool: PgPool, interval: Duration, sinks: Vec<Arc<dyn NotificationSink>>) -> Self {
        Self {
            pool,
            interval,
            condition_since: Mutex::new(HashMap::new()),
            sinks,
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
                open_problem(&self.pool, &self.sinks, rule, host_id, value).await?;
            }
            (true, None) => {
                let first_true_at = self.mark_condition_since(rule.id, host_id);
                let sustain = Duration::from_secs(rule.sustained_for_secs as u64);
                if first_true_at.elapsed() >= sustain {
                    self.clear_condition_since(rule.id, host_id);
                    open_problem(&self.pool, &self.sinks, rule, host_id, value).await?;
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

/// Inserts the new `Problem` row and notifies `sinks` — notification is
/// tied to this specific state transition (a `Problem` actually opening),
/// not to every evaluation pass, per the issue's explicit instruction.
async fn open_problem(
    pool: &PgPool,
    sinks: &[Arc<dyn NotificationSink>],
    rule: &Rule,
    host_id: &str,
    value: f64,
) -> NestResult<Problem> {
    let problem = sqlx::query_as::<_, Problem>(
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
    .map_err(|error| db_error("failed to open problem", error))?;

    for sink in sinks {
        sink.notify(&problem, rule);
    }

    Ok(problem)
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

/// Notified when a [`Problem`] opens (see `open_problem`).
///
/// Synchronous by design, not `async` — this keeps `dyn NotificationSink`
/// usable as a plain trait object (`Arc<dyn NotificationSink>`) without
/// `async_trait`'s boxing, and it matches how a sink is actually meant to
/// behave: a notification is fired off, not awaited as part of the
/// transition that triggered it. A sink that needs to do async work (like
/// [`WebhookSink`]) is responsible for spawning it, not blocking `notify`.
pub trait NotificationSink: Send + Sync {
    fn notify(&self, problem: &Problem, rule: &Rule);
}

/// Logs every opened `Problem` at `warn` level. The simplest possible sink —
/// mainly useful as a default/fallback and for tests.
pub struct LogSink;

impl NotificationSink for LogSink {
    fn notify(&self, problem: &Problem, rule: &Rule) {
        tracing::warn!(
            problem_id = problem.id,
            host_id = %problem.host_id,
            key = %rule.item_key,
            severity = ?rule.severity,
            "problem opened"
        );
    }
}

/// JSON body `WebhookSink` posts — a stub shape (this is the "Notification
/// stub" the phase doc names it), not a stable external contract yet.
#[derive(serde::Serialize)]
struct WebhookPayload<'a> {
    problem: &'a Problem,
    rule: &'a Rule,
}

/// Fire-and-forget `POST`s a JSON `{problem, rule}` payload to `url` whenever
/// a `Problem` opens.
///
/// "Fire-and-forget" is why `notify` (a sync fn — see [`NotificationSink`])
/// spawns the actual request rather than awaiting it inline: the caller
/// (`open_problem`) gets no feedback either way, by design. Failures
/// (connection refused, non-2xx status, timeout — anything
/// `HttpClientService::send` surfaces) are logged and dropped, never
/// propagated — a broken webhook endpoint must never take down the
/// alerting loop, per the issue's explicit instruction.
pub struct WebhookSink {
    url: String,
    http: HttpClientService,
}

impl WebhookSink {
    pub fn new(url: impl Into<String>, http: HttpClientService) -> Self {
        Self {
            url: url.into(),
            http,
        }
    }
}

impl NotificationSink for WebhookSink {
    fn notify(&self, problem: &Problem, rule: &Rule) {
        let url = self.url.clone();
        let http = self.http.clone();
        let payload = match serde_json::to_vec(&WebhookPayload { problem, rule }) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(error = %error, url = %url, "failed to encode webhook payload");
                return;
            }
        };

        tokio::spawn(async move {
            let request = HttpRequest::post(&url)
                .with_header("content-type", "application/json")
                .with_body(payload);
            if let Err(error) = http.send(request).await {
                tracing::warn!(error = %error, url = %url, "webhook notification failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use sparrow_core::trigger::{Operator, ProblemStatus, Severity};

    use super::*;

    fn test_task() -> AlertingTask {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        AlertingTask::new(pool, Duration::from_secs(10), Vec::new())
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

    fn test_rule() -> Rule {
        Rule {
            id: 1,
            host_id: Some("test-host".to_string()),
            item_key: "cpu.usage_percent".to_string(),
            operator: Operator::GreaterThan,
            threshold: 90.0,
            severity: Severity::Warning,
            sustained_for_secs: 0,
            enabled: true,
        }
    }

    fn test_problem() -> Problem {
        Problem {
            id: 42,
            rule_id: 1,
            host_id: "test-host".to_string(),
            status: ProblemStatus::Open,
            opened_at: now_ms(),
            resolved_at: None,
            last_value: 95.5,
        }
    }

    #[test]
    fn log_sink_does_not_panic() {
        // Nothing observable to assert on tracing output here (no
        // subscriber capture in this test), but this at least confirms
        // notify() doesn't panic given real Problem/Rule values.
        LogSink.notify(&test_problem(), &test_rule());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn webhook_sink_posts_the_problem_and_rule_as_json() {
        use std::sync::Mutex as StdMutex;

        use nest_http_client::HttpClientConfig;
        use nest_http_serve::{HttpServer, Json, RequestContext, RouteGroup};

        let received: Arc<StdMutex<Option<Vec<u8>>>> = Arc::new(StdMutex::new(None));

        let route_received = Arc::clone(&received);
        let server = HttpServer::builder()
            .routes(
                RouteGroup::new("").post("/webhook", move |ctx: RequestContext| {
                    let received = Arc::clone(&route_received);
                    async move {
                        *received.lock().unwrap() = Some(ctx.body().to_vec());
                        Json(serde_json::json!({ "ok": true })).into_response()
                    }
                }),
            )
            .spawn()
            .await
            .expect("test server should spawn");

        let http =
            HttpClientService::new(HttpClientConfig::default()).expect("http client should build");
        let sink = WebhookSink::new(format!("{}/webhook", server.base_url()), http);

        let rule = test_rule();
        let problem = test_problem();
        sink.notify(&problem, &rule);

        // Fire-and-forget: the POST happens in a spawned task, so poll for
        // it rather than assuming it lands before the next line runs.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let body = loop {
            if let Some(body) = received.lock().unwrap().clone() {
                break body;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("webhook POST did not arrive within 5s");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("webhook body should be valid JSON");
        assert_eq!(value["problem"]["id"], problem.id);
        assert_eq!(value["problem"]["host_id"], problem.host_id);
        assert_eq!(value["rule"]["item_key"], rule.item_key);
        assert_eq!(value["rule"]["operator"], "greater_than");

        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn webhook_sink_failure_does_not_panic_or_propagate() {
        use nest_http_client::HttpClientConfig;

        // Nothing listens on this port — the POST will fail to connect.
        // notify() must swallow that, not panic, matching NotificationSink's
        // "never propagate" contract.
        let http =
            HttpClientService::new(HttpClientConfig::default()).expect("http client should build");
        let sink = WebhookSink::new("http://127.0.0.1:1", http);

        sink.notify(&test_problem(), &test_rule());

        // Give the spawned task a moment to actually run and fail; the only
        // thing under test is that this doesn't panic or hang.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // --- Docker-backed tests below: evaluate_rule_for_host's four quadrants,
    // sustained-duration, and an end-to-end evaluate_once() pass against a
    // real testcontainers Postgres. Issue 8.5's acceptance criterion is
    // `cargo test -p sparrow-server alerting::` passing with Docker running.

    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use sparrow_core::storage::HostRegistry;
    use testcontainers_modules::postgres::Postgres as PostgresImage;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::testcontainers::ContainerAsync;

    /// Holds a running Postgres container (with Sparrow's migrations already
    /// applied) alive for the test's duration. Same recipe as
    /// `sparrow-core/src/storage.rs`'s own test module (duplicated, not
    /// imported — that one is private to `sparrow-core`'s tests).
    struct TestDb {
        _container: ContainerAsync<PostgresImage>,
        pool: PgPool,
    }

    async fn start_postgres_with_schema() -> TestDb {
        let container = PostgresImage::default()
            .start()
            .await
            .expect("failed to start postgres testcontainer");
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

        nest_core::AppBuilder::new()
            .module(DataModule)
            .module(
                PostgresDataModule::new(PostgresConfig::new(url.clone()))
                    .with_migrations(sparrow_core::migrations::all_migrations()),
            )
            .build()
            .expect("app with postgres migrations");

        let pool = PgPool::connect(&url).await.expect("fresh postgres pool");

        TestDb {
            _container: container,
            pool,
        }
    }

    /// Registers `host_id` and inserts a matching `rules` row (both FK
    /// targets `problems`/`rules` need), returning a `Rule` whose `id`
    /// matches the real inserted row — `evaluate_rule_for_host` opens/
    /// resolves real `problems` rows referencing this id, so it can't be a
    /// made-up value like the non-Docker unit tests above use.
    async fn seed_rule(pool: &PgPool, host_id: &str, sustained_for_secs: i64) -> Rule {
        HostRegistry::new(pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host for the FK reference");

        let id: i64 = sqlx::query_scalar(
            "INSERT INTO rules (host_id, item_key, operator, threshold, severity, sustained_for_secs)
             VALUES ($1, 'cpu.usage_percent', 'greater_than', 90.0, 'warning', $2)
             RETURNING id",
        )
        .bind(host_id)
        .bind(sustained_for_secs)
        .fetch_one(pool)
        .await
        .expect("insert rule");

        Rule {
            id,
            host_id: Some(host_id.to_string()),
            item_key: "cpu.usage_percent".to_string(),
            operator: Operator::GreaterThan,
            threshold: 90.0,
            severity: Severity::Warning,
            sustained_for_secs,
            enabled: true,
        }
    }

    async fn open_problem_count(pool: &PgPool, rule_id: i64, host_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM problems WHERE rule_id = $1 AND host_id = $2 AND status = 'open'",
        )
        .bind(rule_id)
        .bind(host_id)
        .fetch_one(pool)
        .await
        .expect("open problem count")
    }

    fn task_for(pool: PgPool) -> AlertingTask {
        AlertingTask::new(pool, Duration::from_secs(10), Vec::new())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evaluate_rule_for_host_opens_a_problem_when_condition_becomes_true() {
        let db = start_postgres_with_schema().await;
        let rule = seed_rule(&db.pool, "quadrant-open", 0).await;
        let task = task_for(db.pool.clone());

        task.evaluate_rule_for_host(&rule, "quadrant-open", 95.0)
            .await
            .expect("evaluate_rule_for_host should succeed");

        let problem = fetch_open_problem(&db.pool, rule.id, "quadrant-open")
            .await
            .expect("fetch_open_problem should succeed")
            .expect("a problem should have opened");
        assert_eq!(problem.last_value, 95.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evaluate_rule_for_host_updates_last_value_without_opening_a_duplicate() {
        let db = start_postgres_with_schema().await;
        let rule = seed_rule(&db.pool, "quadrant-update", 0).await;
        let task = task_for(db.pool.clone());

        task.evaluate_rule_for_host(&rule, "quadrant-update", 95.0)
            .await
            .expect("first evaluation should open a problem");
        task.evaluate_rule_for_host(&rule, "quadrant-update", 97.0)
            .await
            .expect("second evaluation should update, not duplicate");

        assert_eq!(
            open_problem_count(&db.pool, rule.id, "quadrant-update").await,
            1,
            "a second true evaluation must not open a duplicate problem — the \
             partial unique index from Issue 8.2 backs this up"
        );
        let problem = fetch_open_problem(&db.pool, rule.id, "quadrant-update")
            .await
            .expect("fetch_open_problem should succeed")
            .expect("the problem should still be open");
        assert_eq!(problem.last_value, 97.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evaluate_rule_for_host_resolves_an_open_problem_when_condition_goes_false() {
        let db = start_postgres_with_schema().await;
        let rule = seed_rule(&db.pool, "quadrant-resolve", 0).await;
        let task = task_for(db.pool.clone());

        task.evaluate_rule_for_host(&rule, "quadrant-resolve", 95.0)
            .await
            .expect("first evaluation should open a problem");
        task.evaluate_rule_for_host(&rule, "quadrant-resolve", 10.0)
            .await
            .expect("second evaluation should resolve the open problem");

        assert!(
            fetch_open_problem(&db.pool, rule.id, "quadrant-resolve")
                .await
                .expect("fetch_open_problem should succeed")
                .is_none(),
            "the problem should no longer be open"
        );
        let status: String =
            sqlx::query_scalar("SELECT status FROM problems WHERE rule_id = $1 AND host_id = $2")
                .bind(rule.id)
                .bind("quadrant-resolve")
                .fetch_one(&db.pool)
                .await
                .expect("problem row should still exist, now resolved");
        assert_eq!(status, "resolved");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evaluate_rule_for_host_is_a_no_op_when_condition_false_and_nothing_open() {
        let db = start_postgres_with_schema().await;
        let rule = seed_rule(&db.pool, "quadrant-noop", 0).await;
        let task = task_for(db.pool.clone());

        task.evaluate_rule_for_host(&rule, "quadrant-noop", 10.0)
            .await
            .expect("evaluate_rule_for_host should succeed");

        assert_eq!(
            open_problem_count(&db.pool, rule.id, "quadrant-noop").await,
            0,
            "a never-true condition must never open a problem"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evaluate_rule_for_host_waits_out_the_sustain_window_before_opening() {
        let db = start_postgres_with_schema().await;
        let rule = seed_rule(&db.pool, "quadrant-sustain", 1).await;
        let task = task_for(db.pool.clone());

        task.evaluate_rule_for_host(&rule, "quadrant-sustain", 95.0)
            .await
            .expect("first evaluation should succeed");
        assert_eq!(
            open_problem_count(&db.pool, rule.id, "quadrant-sustain").await,
            0,
            "condition true for less than the sustain window must not open a problem yet"
        );

        tokio::time::sleep(Duration::from_millis(1100)).await;
        task.evaluate_rule_for_host(&rule, "quadrant-sustain", 95.0)
            .await
            .expect("second evaluation, past the sustain window, should succeed");
        assert_eq!(
            open_problem_count(&db.pool, rule.id, "quadrant-sustain").await,
            1,
            "condition true for longer than the sustain window must open a problem"
        );
    }

    /// End-to-end acceptance case: seed a rule, drive a value past threshold
    /// by inserting directly into `metric_history` (no need for a real
    /// loaded host), confirm a Problem opens and later resolves when the
    /// value drops back — via the real `evaluate_once` entry point
    /// (`Task::run` calls exactly this each tick), not `evaluate_rule_for_host`
    /// directly like the quadrant tests above.
    #[tokio::test(flavor = "multi_thread")]
    async fn evaluate_once_drives_the_full_problem_lifecycle_from_metric_history() {
        let db = start_postgres_with_schema().await;
        let host_id = "e2e-host";
        let rule = seed_rule(&db.pool, host_id, 0).await;
        let task = task_for(db.pool.clone());

        insert_metric(&db.pool, host_id, "cpu.usage_percent", 95.0, 1).await;
        task.evaluate_once()
            .await
            .expect("first pass should succeed");
        assert!(
            fetch_open_problem(&db.pool, rule.id, host_id)
                .await
                .expect("fetch_open_problem should succeed")
                .is_some(),
            "a problem should have opened once the metric crossed the threshold"
        );

        insert_metric(&db.pool, host_id, "cpu.usage_percent", 10.0, 2).await;
        task.evaluate_once()
            .await
            .expect("second pass should succeed");
        assert!(
            fetch_open_problem(&db.pool, rule.id, host_id)
                .await
                .expect("fetch_open_problem should succeed")
                .is_none(),
            "the problem should resolve once the metric drops back below threshold"
        );
    }

    async fn insert_metric(pool: &PgPool, host_id: &str, key: &str, value: f64, ts: i64) {
        sqlx::query(
            "INSERT INTO metric_history (host_id, collector, key, value, value_type, ts)
             VALUES ($1, 'test-collector', $2, $3, 'float', $4)",
        )
        .bind(host_id)
        .bind(key)
        .bind(value.to_string())
        .bind(ts)
        .execute(pool)
        .await
        .expect("insert metric_history row");
    }
}
