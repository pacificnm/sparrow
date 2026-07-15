# Phase 6 Task Spec — Agent (`crates/agent`, `nest-cli`)

**Repo:** `pacificnm/sparrow`
**Crate:** `crates/agent`
**Prerequisite:** Phase 4 (core contracts), Phase 5 (collectors), `nest-mqtt` (Phase 2).

## Ground truth — confirmed against real framework source

- **`nest-task-runtime` has no recurring/interval scheduling primitive.** `TaskManager::spawn<T: Task>(&self, task: T) -> NestResult<TaskHandle<T::Output>>` spawns a task **once**. `Task` is `{ fn name(&self) -> &'static str; async fn run(&self, ctx: TaskContext) -> NestResult<Self::Output>; }`. "Run each collector on its own interval" must be built as a **long-running `Task` with an internal loop**, one per collector, not as repeated `spawn` calls from an external scheduler.
- **`CancelToken::is_cancelled()` is a poll, not an awaitable future.** There is no `cancelled().await` to race against in a `tokio::select!`. This means a collector loop that does `interval.tick().await` (say, every 60s for the `disk` collector) will not notice a shutdown request until that tick fires — up to 60s of shutdown latency. Fix: poll `is_cancelled()` on a **short, fixed cadence** (e.g. every 1s) inside the loop, and only run `collect()`/publish when the collector's own interval has elapsed, tracked via `Instant` — not `tokio::time::interval` set to the collector's full interval.
- `nest_cli::CliApp::new("sparrow-agent")` — standard bootstrap (config → logging → `AppContext` → modules → run), per the app standard.
- `nest_mqtt::MqttClient` (Phase 2): `connect`, `publish`, `subscribe` (returns a `Stream<Item = MqttMessage>`), LWT configured at connect time via `MqttConfig::last_will`.

---

## Design

```
crates/agent/
├── Cargo.toml
└── src/
    ├── main.rs        # CliApp::new("sparrow-agent")...run()
    ├── config.rs        # AgentConfig: broker address, host_id, enabled collectors + intervals
    ├── scheduler.rs      # CollectorTask: the Task impl with the interval-loop-with-cancel-poll design
    ├── publisher.rs       # batches CollectorTask output, publishes DataBatch
    ├── heartbeat.rs        # heartbeat Task (same interval-loop pattern, simpler)
    └── config_reload.rs     # subscribes to the agent's own retained config topic, applies live
```

### `config.rs`

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentConfig {
    pub host_id: String,
    pub broker_host: String,
    pub broker_port: u16,
    /// Per-collector interval overrides; absent entries use the collector's
    /// own `default_interval_secs()`.
    #[serde(default)]
    pub collector_intervals: std::collections::BTreeMap<String, u64>,
    /// Collectors explicitly disabled — everything in `default_collectors()`
    /// runs unless named here.
    #[serde(default)]
    pub disabled_collectors: Vec<String>,
}
```

Load via `nest-config`'s `[agent]` TOML section, following the same `from_config_service` pattern seen in every other module in Phases 1–3 — check `nest-config`'s `ConfigDocument::section("agent")` usage in an existing example (e.g. `nest-claude`'s `config.rs`) before writing this, don't invent a different loading idiom.

### `scheduler.rs` — the core design

```rust
use std::time::{Duration, Instant};
use async_trait::async_trait;
use nest_task::{Task, TaskContext};
use nest_error::NestResult;
use sparrow_core::collector::Collector;

/// A long-running task that owns one [`Collector`] and runs it on its own
/// interval until cancelled. This is Sparrow's answer to nest-task-runtime
/// having no built-in interval scheduling — see Phase 6's ground-truth note.
pub struct CollectorTask {
    collector: Box<dyn Collector>,
    interval: Duration,
    publisher: crate::publisher::Publisher,
}

impl CollectorTask {
    pub fn new(collector: Box<dyn Collector>, interval: Duration, publisher: crate::publisher::Publisher) -> Self {
        Self { collector, interval, publisher }
    }
}

const CANCEL_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[async_trait]
impl Task for CollectorTask {
    type Output = ();

    fn name(&self) -> &'static str {
        // NOTE: Task::name returns &'static str but each CollectorTask wraps a
        // dynamically-chosen collector — CHECK whether nest-task's Task trait
        // actually requires 'static here or whether TaskContext carries the name
        // separately; if it's truly 'static-only, this needs a match over the known
        // collector names (cpu/memory/disk) rather than a stored String, since you
        // can't leak a String to get a 'static str cleanly. Do not `Box::leak` as a
        // shortcut — resolve this properly, e.g. `self.collector.name()` if that
        // itself returns `&'static str` (Phase 4's `Collector::name` does), in which
        // case this is a non-issue — verify before treating it as a problem.
        self.collector.name()
    }

    async fn run(&self, ctx: TaskContext) -> NestResult<()> {
        let mut last_run = Instant::now() - self.interval; // run immediately on first loop
        loop {
            if ctx.cancel_token().is_cancelled() {
                return Ok(());
            }

            if last_run.elapsed() >= self.interval {
                // `Collector::collect` takes `&mut self` (Phase 4) but `Task::run` here
                // takes `&self` on `CollectorTask` — CHECK: this needs interior mutability
                // (`Mutex<Box<dyn Collector>>` or similar) since `collector` can't be
                // mutated through `&self`. Resolve this in the actual struct definition,
                // don't leave `collector: Box<dyn Collector>` as a plain field if `run`
                // only gets `&self` — that won't compile as sketched above.
                match self.collector_collect_somehow() {
                    Ok(items) => {
                        if let Err(err) = self.publisher.publish_batch(self.collector.name(), items).await {
                            tracing::warn!(error = %err, collector = self.collector.name(), "failed to publish batch");
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, collector = self.collector.name(), "collector failed");
                    }
                }
                last_run = Instant::now();
            }

            tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
        }
    }
}
```

**Explicit unresolved item, flagged deliberately in the sketch above rather
than silently worked around:** `Task::run(&self, ...)` takes `&self`, but
`Collector::collect(&mut self, ...)` needs `&mut self`. The sketch's
`collector_collect_somehow()` is a placeholder naming the problem, not a
real method — resolve it with interior mutability (`tokio::sync::Mutex<Box<dyn
Collector>>` around the collector field, `.lock().await` inside `run`) before
writing the real implementation. Do not use a synchronous `std::sync::Mutex`
across an `.await` point — that will not compile cleanly / will trigger a
`Send` bound issue with the async trait; use the Tokio async mutex.

Also verify the `Task::name() -> &'static str` signature concern noted
inline — if `Collector::name()` already returns `&'static str` (it does, per
Phase 4), then `self.collector.name()` satisfies `Task::name` with no
further work, and the inline warning about `Box::leak` is moot. Confirm this
before treating it as a real blocker.

### `publisher.rs`

```rust
pub struct Publisher {
    client: nest_mqtt::MqttClient,
    host_id: String,
}

impl Publisher {
    pub async fn publish_batch(&self, collector: &str, items: Vec<sparrow_core::collector::MetricItem>) -> NestResult<()> {
        let batch = sparrow_core::transport::DataBatch {
            host_id: self.host_id.clone(),
            collector: collector.to_string(),
            items,
        };
        let topic = sparrow_core::transport::Topics::data(&self.host_id);
        self.client
            .publish(&topic, batch.to_payload(), nest_mqtt::MqttQos::AtLeastOnce, false)
            .await
    }
}
```

### `heartbeat.rs`

Same `CollectorTask`-style interval-loop pattern (reuse the cancel-poll design, do not write a third variant of the loop), but simpler — no collector, just publishes `HeartbeatMessage` on `Topics::heartbeat(host_id)` every 15s (hardcode this interval, it's not collector-configurable).

**Bounded buffering for broker outages:** if `publish` fails (broker
unreachable), do not drop the batch silently and do not block indefinitely
retrying — buffer up to N recent batches (e.g. `VecDeque` capped at 100
entries, drop oldest on overflow) in the `Publisher`, and flush the buffer
opportunistically on the next successful publish. Implement this inside
`Publisher`, not in each `CollectorTask` — keeps the buffering logic in one
place.

### `config_reload.rs`

Subscribe to `Topics::config(host_id)` once at startup (retained message —
the agent gets the last-published config immediately on subscribe, even if
it was published while the agent was offline, per MQTT retained-message
semantics confirmed in Phase 2's design). On each message, parse the new
`AgentConfig`, diff against the currently-running set of `CollectorTask`s,
and:
- Stop (cancel) tasks for collectors newly in `disabled_collectors`.
- Start new `CollectorTask`s for collectors newly enabled.
- For interval changes on already-running collectors, the simplest correct
  approach is: cancel the existing task, spawn a new one with the new
  interval — do not try to mutate a running task's interval in place, that's
  meaningfully more complex for marginal benefit at Sparrow's scale.

### `main.rs`

```rust
fn main() -> NestResult<()> {
    nest_cli::CliApp::new("sparrow-agent")
        .module(/* MqttModule::new(mqtt_config) from Phase 2 */)
        // CHECK: does the agent need PostgresDataModule at all? Per the plan, the
        // agent publishes over MQTT and has no direct DB access — confirm this
        // assumption before wiring PostgresDataModule in here; if the agent truly
        // has zero DB needs, don't add the dependency just because the server has it.
        .run()
}
```

Wire the actual task spawning (one `CollectorTask` per enabled collector,
plus the heartbeat task, plus `config_reload`'s subscriber loop) through
`TaskManagerService::spawn` inside the app's startup — the exact wiring point
(a `Lifecycle::on_startup` impl, most likely, following Phase 2's
`MqttModule` pattern for where async startup work happens via `block_on`)
needs to be confirmed against `nest-cli`'s actual `CliApp::run()` internals
before finalizing — flagged as a check item, not guessed here.

---

## Tests

- `scheduler.rs`: a `FakeCollector` with a counter, run `CollectorTask` for a short duration (e.g. 3 seconds with a 1-second interval) via a test-only cancel token cancelled after that duration, assert `collect()` was called roughly 3 times (allow slack — this is a timing test, not exact-count).
- `config_reload.rs`: publish a config change to a local Mosquitto (`testcontainers`), assert the agent's running task set changes accordingly (disable → task stops, re-enable → task restarts).
- Full integration: agent + local broker (`testcontainers`), assert real `cpu`/`memory`/`disk` data appears on `Topics::data(host_id)` within one interval period.

**Acceptance:** `cargo test -p sparrow-agent` passes with Docker running; `./build run` against a local Mosquitto shows live data; killing and restarting the broker mid-run does not crash the agent and data resumes flowing after reconnect (Phase 2's `MqttClient` handles the reconnect at the transport level — this test confirms the agent doesn't do anything on top that breaks that).

## Explicit "do not" list

- Do not use `tokio::time::interval` set to the collector's full interval as the loop's only wait — that's the shutdown-latency bug identified above.
- Do not use a synchronous `Mutex` around the collector across an `.await` point.
- Do not write three different variants of the interval-loop-with-cancel-poll pattern (`CollectorTask`, heartbeat, anything else) — factor the loop shape into one reusable piece if a third use case shows up.
- Do not silently drop batches on publish failure — buffer per the design above.
