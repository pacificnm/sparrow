# Phase 8 Task Spec — Trigger / Alerting Engine

**Repo:** `pacificnm/sparrow`
**Crate:** `crates/core/src/trigger.rs` (rule model, evaluation logic) + `crates/server/src/alerting.rs` (the running loop)
**Prerequisite:** Phase 4 (storage), Phase 7 (server, ingest pipeline writing to `metric_history`).

## Design

### Rule model (`crates/core/src/trigger.rs`)

```rust
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Rule {
    pub id: i64,
    pub host_id: Option<String>, // None = applies to all hosts
    pub item_key: String,
    pub operator: Operator,
    pub threshold: f64,
    pub severity: Severity,
    pub sustained_for_secs: i64, // 0 = trip immediately, matches Zabbix's trigger-duration concept
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Operator {
    GreaterThan,
    LessThan,
    Equal,
}

#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Operator {
    pub fn evaluate(self, value: f64, threshold: f64) -> bool {
        match self {
            Operator::GreaterThan => value > threshold,
            Operator::LessThan => value < threshold,
            Operator::Equal => (value - threshold).abs() < f64::EPSILON,
        }
    }
}
```

**`sustained_for_secs` note:** a rule with `sustained_for_secs > 0` must stay
true across that whole window before a Problem is raised, not just on a
single evaluation — this is the "for 30s" language from the plan's original
acceptance example ("cpu.usage > 90 for 30s"). Implement this by tracking,
per rule-per-host, the timestamp when the condition *first* became true; only
raise the Problem once `now - first_true_at >= sustained_for_secs`. Do not
implement this as "check the last N evaluations were all true" — that
couples correctness to the evaluation loop's own interval in a fragile way;
track wall-clock duration instead.

### Problem state (`crates/core/src/trigger.rs`, continued)

```rust
#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ProblemStatus {
    Open,
    Resolved,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Problem {
    pub id: i64,
    pub rule_id: i64,
    pub host_id: String,
    pub status: ProblemStatus,
    pub opened_at: i64, // unix millis
    pub resolved_at: Option<i64>,
    pub last_value: f64,
}
```

### Migrations (add to Phase 4's `migrations.rs`)

```sql
CREATE TABLE rules (
    id BIGSERIAL PRIMARY KEY,
    host_id TEXT REFERENCES hosts(host_id),
    item_key TEXT NOT NULL,
    operator TEXT NOT NULL,
    threshold DOUBLE PRECISION NOT NULL,
    severity TEXT NOT NULL,
    sustained_for_secs BIGINT NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT true
);

CREATE TABLE problems (
    id BIGSERIAL PRIMARY KEY,
    rule_id BIGINT NOT NULL REFERENCES rules(id),
    host_id TEXT NOT NULL REFERENCES hosts(host_id),
    status TEXT NOT NULL,
    opened_at BIGINT NOT NULL,
    resolved_at BIGINT,
    last_value DOUBLE PRECISION NOT NULL
);
-- Only one OPEN problem per (rule_id, host_id) at a time — enforce this at the
-- application level in the evaluation loop below, a partial unique index is the
-- stricter option if you want the DB to also guarantee it:
CREATE UNIQUE INDEX idx_one_open_problem_per_rule_host
    ON problems (rule_id, host_id) WHERE status = 'open';
```

The partial unique index is worth keeping — it turns "the evaluation loop
has a bug and double-opens a Problem" from a silent data-quality issue into a
loud constraint-violation error during testing.

### Evaluation loop (`crates/server/src/alerting.rs`)

Same interval-loop-with-cancel-poll pattern from Phase 6/7 (reuse it — this
is the third use of that shape; if a fourth comes up in a later phase,
that's the trigger to actually factor it into a shared helper in
`crates/core`, not before).

```rust
pub struct AlertingTask {
    pool: sqlx::PgPool,
    interval: std::time::Duration, // e.g. 10s — independent of any collector's interval
}

impl AlertingTask {
    async fn evaluate_once(&self) -> NestResult<()> {
        let rules = fetch_enabled_rules(&self.pool).await?;
        for rule in rules {
            let hosts = if let Some(host_id) = &rule.host_id {
                vec![host_id.clone()]
            } else {
                fetch_all_host_ids(&self.pool).await?
            };
            for host_id in hosts {
                if let Some(latest) = fetch_latest_value(&self.pool, &host_id, &rule.item_key).await? {
                    self.evaluate_rule_for_host(&rule, &host_id, latest).await?;
                }
            }
        }
        Ok(())
    }

    async fn evaluate_rule_for_host(&self, rule: &sparrow_core::trigger::Rule, host_id: &str, value: f64) -> NestResult<()> {
        let condition_true = rule.operator.evaluate(value, rule.threshold);
        let existing_open = fetch_open_problem(&self.pool, rule.id, host_id).await?;

        match (condition_true, existing_open) {
            (true, None) if rule.sustained_for_secs == 0 => {
                open_problem(&self.pool, rule, host_id, value).await?;
            }
            (true, None) => {
                // CHECK: sustained_for_secs > 0 needs a "condition first became true at"
                // tracker that survives across evaluate_once() calls — an in-memory
                // HashMap<(rule_id, host_id), Instant> on AlertingTask works but is lost
                // on server restart (a rule that was 20s into a 30s sustain window resets
                // to 0 after a restart). Decide whether that's acceptable for v1 (probably
                // yes — matches how most simple monitoring tools behave) or whether this
                // needs a persisted "condition_since" column instead. Make this decision
                // explicitly, don't let it happen by accident because the in-memory map
                // was the easiest thing to write first.
                todo!("sustained-duration tracking — see note above")
            }
            (false, Some(problem)) => {
                resolve_problem(&self.pool, problem.id).await?;
            }
            (true, Some(problem)) => {
                update_last_value(&self.pool, problem.id, value).await?;
            }
            (false, None) => {} // nothing to do
        }
        Ok(())
    }
}
```

### Notification stub

```rust
pub trait NotificationSink: Send + Sync {
    fn notify(&self, problem: &sparrow_core::trigger::Problem, rule: &sparrow_core::trigger::Rule);
}

pub struct LogSink;
impl NotificationSink for LogSink {
    fn notify(&self, problem: &sparrow_core::trigger::Problem, rule: &sparrow_core::trigger::Rule) {
        tracing::warn!(
            problem_id = problem.id, host_id = %problem.host_id,
            key = %rule.item_key, severity = ?rule.severity,
            "problem opened"
        );
    }
}

pub struct WebhookSink {
    url: String,
    http: nest_http_client::HttpClientService,
}
// implement notify() as a fire-and-forget POST of a JSON payload; log (don't propagate)
// failures — a broken webhook endpoint must never take down the alerting loop.
```

Call `NotificationSink::notify` from `open_problem`, not from the loop's
caller — keeps notification tied to the actual state transition, not to
every evaluation pass.

### API (`crates/server/src/api/problems.rs`)

```rust
pub fn routes() -> nest_http_serve::RouteGroup {
    nest_http_serve::RouteGroup::new("/api")
        .get("/problems", list_problems)
}

async fn list_problems(ctx: nest_http_serve::RequestContext) -> nest_http_serve::HttpResult {
    // Optional `?host_id=` query filter — omitted means "all currently OPEN
    // problems across all hosts." Same "verify RequestContext's query-param
    // accessor against context.rs before writing this, don't guess the exact
    // method name" instruction as Phase 7's api/hosts.rs and api/history.rs.
    todo!("fetch open problems (optionally filtered by host_id), return JSON")
}
```

Specified here rather than in Phase 7 because it depends on this phase's
`Problem`/`problems` table — Phase 7's `api/` design predates Problems
existing. Register this route group alongside Phase 7's `hosts`/`history`
groups in the server's route registry. This is the endpoint Phase 11's
`ProblemsPanel.tsx` consumes; implementation and its test are tracked under
Milestone 11 (Issue 11.2) since that's when the desktop dashboard first
needs it, but the contract lives here so it isn't an undocumented
Phase-11-only assumption.

---

## Tests

- `Operator::evaluate` — simple unit tests per variant, including the float-equality edge case for `Equal`.
- `evaluate_rule_for_host` — the four-quadrant match above, each tested independently against a `testcontainers` Postgres: condition newly true → problem opens; condition still true → `last_value` updates, no duplicate problem (this is the test the partial unique index backs up); condition false with an open problem → resolves; condition false with no problem → no-op.
- Sustained-duration behavior — once the design decision above is made, write the test that actually exercises it (condition true for less than the sustain window → no problem yet; true for longer → problem opens).

**Acceptance:** `cargo test -p sparrow-server alerting::` and `cargo test -p sparrow-core trigger::` pass with Docker running. End-to-end: seed a rule, drive a collector's value past threshold (can fake this by inserting directly into `metric_history` rather than needing a real loaded host), confirm a Problem opens and later resolves when the value drops back.

## Explicit "do not" list

- Do not implement sustained-duration tracking without first deciding (and documenting) whether it's in-memory-only or persisted — flagged as a real decision above, not a guess.
- Do not let a `WebhookSink` failure propagate up and stop the evaluation loop.
- Do not open a second `OPEN` problem for a rule/host pair that already has one — the partial unique index exists specifically to catch this in testing if the application-level check has a bug.
