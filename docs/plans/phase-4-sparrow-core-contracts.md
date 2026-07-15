# Phase 4 Task Spec — Sparrow Core Contracts

**Repo:** `pacificnm/sparrow` (product repo, checked out locally at `nest/apps/sparrow/`)
**Crate:** `crates/core`
**Prerequisite:** Phases 1–3 merged (`nest-data-postgres` hardened, `nest-mqtt` and `nest-ai-claude` built) — this phase is the first Sparrow-repo work and is blocked on all three.

## Ground truth from the framework (confirmed against real source in Phases 1–3)

- `nest_data::AsyncRepository<TEntity, TId>` is a **per-row** CRUD trait (`get/list/insert/update/delete`) — fine for the host registry and Problems, **wrong tool** for high-frequency metric writes. Use `PostgresConnection::pool()` (raw `sqlx::PgPool`) directly for batch metric inserts, per the batch-write note from Phase 1.
- `nest_data::Migration` trait: `id() -> &str`, `up_sql() -> &str`, `down_sql() -> &str`. `SqlMigration::new(id, up_sql, down_sql)` is the ready-made implementation — use it, don't write a custom `Migration` impl unless there's a real reason to.
- `nest_mqtt::MqttClient` (Phase 2): `connect(&MqttConfig)`, `publish(topic, payload, qos, retain)`, `subscribe(topic_filter, qos) -> impl Stream<Item = MqttMessage>`, `MqttMessage { topic, payload, retained }`.
- `nest_core::Module`/`AppBuilder::register_service` — `crates/core` itself is not a `Module`; it's a plain library crate that `crates/agent` and `crates/server` both depend on and wire into their own module registration.

---

## Design

```
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── collector.rs   # Collector trait, MetricItem, ValueType
    ├── transport.rs    # Sparrow's topic taxonomy on top of nest-mqtt
    ├── storage.rs       # host registry + batch metric-history on nest-data-postgres
    └── migrations.rs     # SqlMigration list: hosts, metric_history, (problems table stubbed for Phase 8)
```

### `collector.rs`

```rust
use serde::{Deserialize, Serialize};

/// The type of a metric value — mirrors what the storage schema needs to
/// pick a column type; keep this small and closed, do not make it open-ended.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Float,
    Integer,
    Text,
}

/// One data point produced by a [`Collector`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricItem {
    /// Dotted key, e.g. `cpu.usage_percent`, `disk.used_bytes`.
    pub key: String,
    pub value_type: ValueType,
    /// Always populated regardless of `value_type` — store as text, cast on
    /// read/write at the storage boundary; keeps this struct simple and
    /// avoids a serde-untagged-enum footgun for a low-cost model to debug.
    pub value: String,
    /// Free-form tags (e.g. `{"mount": "/data"}` for a disk item, `{"core": "0"}`
    /// for a per-core CPU item) — optional, empty map if unused.
    #[serde(default)]
    pub tags: std::collections::BTreeMap<String, String>,
    /// Unix millis, set by the collector at read time.
    pub timestamp_ms: i64,
}

/// A modular metric producer. No knowledge of the broker, the agent's
/// scheduling, or storage — a `Collector` is pure: given a call to `collect`,
/// return the current readings.
pub trait Collector: Send + Sync {
    /// Stable name, used as the topic segment and the `collector` tag in storage.
    /// Must be a valid MQTT topic segment: no `/`, `+`, `#`, or whitespace —
    /// validate this in `Collector::name`'s implementations, not centrally.
    fn name(&self) -> &'static str;

    /// Default interval when the agent's config doesn't override it.
    fn default_interval_secs(&self) -> u64;

    /// Reads current values. Implementations that need persistent state
    /// between calls (see Phase 5's `sysinfo` CPU-usage note) should hold
    /// that state as `&mut self` — this trait takes `&mut self` deliberately,
    /// not `&self`, for exactly that reason.
    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError>;
}

#[derive(Debug, thiserror::Error)]
#[error("collector `{collector}` failed: {message}")]
pub struct CollectorError {
    pub collector: String,
    pub message: String,
}
```

**Explicit design note, keep it in a doc comment on `Collector`:** `collect`
takes `&mut self`, not `&self` — this is a deliberate deviation from the
"pure function" framing above, made necessary by collectors like `cpu` that
need a persistent `sysinfo::System` instance to compute usage deltas (Phase
5). Don't "fix" this to `&self` later without re-reading Phase 5's note.

### `transport.rs`

```rust
/// Sparrow's MQTT topic taxonomy. Centralize every topic string here —
/// nowhere else in the codebase should hand-format a topic string.
pub struct Topics;

impl Topics {
    pub fn register(host_id: &str) -> String { format!("sparrow/agents/{host_id}/register") }
    pub fn heartbeat(host_id: &str) -> String { format!("sparrow/agents/{host_id}/heartbeat") }
    pub fn data(host_id: &str) -> String { format!("sparrow/agents/{host_id}/data") }
    pub fn config(host_id: &str) -> String { format!("sparrow/agents/{host_id}/config") }
    pub fn command(host_id: &str) -> String { format!("sparrow/agents/{host_id}/command") }
    /// Wildcard filter for the server to subscribe to all agents' data at once.
    pub fn all_data() -> &'static str { "sparrow/agents/+/data" }
    pub fn all_register() -> &'static str { "sparrow/agents/+/register" }
    pub fn all_heartbeat() -> &'static str { "sparrow/agents/+/heartbeat" }
}

/// Wire payload published on `data` topics — a batch, not one message per item,
/// to keep MQTT message counts sane at high collector frequency.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataBatch {
    pub host_id: String,
    pub collector: String,
    pub items: Vec<crate::collector::MetricItem>,
}

/// Wire payload published on `register`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegisterMessage {
    pub host_id: String,
    pub hostname: String,
    pub agent_version: String,
}

/// Wire payload published on `heartbeat` — kept minimal on purpose; presence
/// alone (plus LWT for absence) carries most of the signal.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HeartbeatMessage {
    pub host_id: String,
    pub timestamp_ms: i64,
}
```

Add `serde_json` (de)serialization helpers (`DataBatch::to_payload() -> Vec<u8>`, `DataBatch::from_payload(&[u8]) -> Result<Self, serde_json::Error>`) on each of the three message types — every publish/subscribe call site should go through these, not call `serde_json::to_vec`/`from_slice` ad hoc.

### `storage.rs`

Two distinct access patterns, do not blur them into one generic layer:

```rust
use sqlx::PgPool;

/// Host registry — low-frequency writes (register/heartbeat/offline), fine
/// to go through ordinary `sqlx::query` calls directly (not `AsyncRepository`
/// — that trait's per-row model adds no value here and Sparrow doesn't need
/// pluggable storage backends the way a generic framework module would).
pub struct HostRegistry {
    pool: PgPool,
}

impl HostRegistry {
    pub fn new(pool: PgPool) -> Self { Self { pool } }

    pub async fn upsert_on_register(&self, host_id: &str, hostname: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO hosts (host_id, hostname, last_seen, online)
             VALUES ($1, $2, NOW(), true)
             ON CONFLICT (host_id) DO UPDATE SET hostname = $2, last_seen = NOW(), online = true",
        )
        .bind(host_id)
        .bind(hostname)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_heartbeat(&self, host_id: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE hosts SET last_seen = NOW(), online = true WHERE host_id = $1")
            .bind(host_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_offline(&self, host_id: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE hosts SET online = false WHERE host_id = $1")
            .bind(host_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Metric history — the actual high-frequency write path. Batch inserts via
/// `sqlx::QueryBuilder`, one round-trip per `DataBatch`, not one INSERT per item.
pub struct MetricHistory {
    pool: PgPool,
}

impl MetricHistory {
    pub fn new(pool: PgPool) -> Self { Self { pool } }

    pub async fn insert_batch(&self, host_id: &str, batch: &crate::transport::DataBatch) -> sqlx::Result<()> {
        if batch.items.is_empty() {
            return Ok(());
        }
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO metric_history (host_id, collector, key, value, value_type, tags, ts)",
        );
        builder.push_values(&batch.items, |mut row, item| {
            row.push_bind(host_id)
                .push_bind(&batch.collector)
                .push_bind(&item.key)
                .push_bind(&item.value)
                .push_bind(format!("{:?}", item.value_type)) // CHECK: confirm the actual
                    // column type/encoding decided in migrations.rs before writing this —
                    // `{:?}` is a placeholder, not a real serialization choice, don't ship it as-is.
                .push_bind(serde_json::to_value(&item.tags).unwrap_or_default())
                .push_bind(item.timestamp_ms);
        });
        builder.build().execute(&self.pool).await?;
        Ok(())
    }
}
```

**Flagged for a real decision, not a guess:** the `value_type`/`tags` column
encoding above (`format!("{:?}", ...)`, raw JSON) is a placeholder. Decide
the actual Postgres column types in `migrations.rs` first (likely
`value_type` as a Postgres `TEXT` with a `CHECK` constraint or a proper enum
type, `tags` as `JSONB`), then make `insert_batch` match — don't let the
migration and the insert code disagree.

### `migrations.rs`

```rust
use nest_data::SqlMigration;

pub fn all_migrations() -> Vec<Box<dyn nest_data::Migration>> {
    vec![
        Box::new(SqlMigration::new(
            "001_create_hosts",
            "CREATE TABLE hosts (
                host_id TEXT PRIMARY KEY,
                hostname TEXT NOT NULL,
                last_seen TIMESTAMPTZ NOT NULL,
                online BOOLEAN NOT NULL DEFAULT false
            )",
            "DROP TABLE hosts",
        )),
        Box::new(SqlMigration::new(
            "002_create_metric_history",
            "CREATE TABLE metric_history (
                id BIGSERIAL PRIMARY KEY,
                host_id TEXT NOT NULL REFERENCES hosts(host_id),
                collector TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                value_type TEXT NOT NULL,
                tags JSONB NOT NULL DEFAULT '{}',
                ts BIGINT NOT NULL
            );
            CREATE INDEX idx_metric_history_host_key_ts ON metric_history (host_id, key, ts DESC);",
            "DROP TABLE metric_history",
        )),
    ]
}
```

Register via `PostgresDataModule::new(config).with_migrations(sparrow_core::migrations::all_migrations())` in both `crates/agent` (if it needs local read access — likely not) and `crates/server` (definitely).

---

## Tests

- `collector.rs`: a `FakeCollector` test double producing known `MetricItem`s, assert `Collector` trait usage compiles and round-trips through serde.
- `transport.rs`: `DataBatch`/`RegisterMessage`/`HeartbeatMessage` serde round-trip tests; `Topics::*` format assertions (exact string match — these strings are a cross-service contract, a typo here breaks agent/server communication silently).
- `storage.rs`: integration tests via `testcontainers-rs` (same convention as Phases 1–2) — `HostRegistry` upsert/heartbeat/offline round-trip; `MetricHistory::insert_batch` with a multi-item batch, assert row count and values match.

**Acceptance:** `cargo test -p sparrow-core` passes with Docker running for the storage integration tests; no manual setup steps.

## Explicit "do not" list

- Do not route metric-history writes through `nest_data::AsyncRepository` — established above as the wrong tool for this access pattern.
- Do not hand-format topic strings anywhere outside `Topics` — every publish/subscribe call site uses these helpers.
- Do not ship `insert_batch`'s placeholder `value_type`/`tags` encoding without first deciding the real column types in `migrations.rs`.
