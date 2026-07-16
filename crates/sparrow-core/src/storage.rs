use serde::Serialize;
use sqlx::{types::Json, PgPool};

use crate::collector::ValueType;
use crate::transport::DataBatch;

/// One row from `hosts`, as returned to API clients.
///
/// `last_seen_ms` is epoch milliseconds, not a native timestamp type —
/// consistent with every other wire/storage timestamp in this codebase
/// (`MetricItem::timestamp_ms`, `HeartbeatMessage::timestamp_ms`,
/// `sparrow_core::time::now_ms`), rather than introducing `sqlx`'s
/// `chrono`/`time` feature (not currently enabled) just for this one field.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct HostRow {
    pub host_id: String,
    pub hostname: String,
    pub online: bool,
    pub last_seen_ms: i64,
}

/// Low-frequency persistence for agent registration and liveness.
///
/// These operations deliberately use direct SQL rather than
/// `nest_data::AsyncRepository`: they are narrow state transitions, not a
/// general-purpose per-row CRUD surface.
///
/// `Clone` (cheap — `PgPool` is an `Arc`-backed handle) so HTTP route
/// closures can each hold their own owned copy per request, since
/// `nest-http-serve` handlers have no other way to reach shared state (see
/// `api/hosts.rs`'s `routes()`).
#[derive(Clone)]
pub struct HostRegistry {
    pool: PgPool,
}

impl HostRegistry {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert_on_register(&self, host_id: &str, hostname: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO hosts (host_id, hostname, last_seen, online)
             VALUES ($1, $2, NOW(), true)
             ON CONFLICT (host_id) DO UPDATE
             SET hostname = $2, last_seen = NOW(), online = true",
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

    /// Marks every currently-online host whose `last_seen` is older than
    /// `stale_after_secs` as offline. Returns the number of hosts actually
    /// updated (0 if none are stale) — used by `sparrow-server`'s periodic
    /// backstop sweep (`offline_watch.rs`) to detect agents that hang without
    /// ever triggering a disconnect (so MQTT's LWT never fires).
    pub async fn mark_stale_offline(&self, stale_after_secs: i64) -> sqlx::Result<u64> {
        let result = sqlx::query(
            "UPDATE hosts
             SET online = false
             WHERE online = true
               AND last_seen < NOW() - ($1 * INTERVAL '1 second')",
        )
        .bind(stale_after_secs)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Lists every host, for `GET /hosts`.
    pub async fn list(&self) -> sqlx::Result<Vec<HostRow>> {
        sqlx::query_as::<_, HostRow>(
            "SELECT host_id, hostname, online,
                    (EXTRACT(EPOCH FROM last_seen) * 1000)::BIGINT AS last_seen_ms
             FROM hosts
             ORDER BY host_id",
        )
        .fetch_all(&self.pool)
        .await
    }
}

/// One row from `metric_history`, as returned to API clients — the raw
/// stored `value_type` string ("float"/"integer"/"text"), not
/// `crate::collector::ValueType`: this is a read path back out to JSON, not
/// a round-trip through the same typed wire contract `DataBatch` uses, and
/// adding an `sqlx::Type` mapping for `ValueType` just for this one read
/// isn't worth it over exposing the same string already sitting in the row.
///
/// Shared by both `latest_items` (one row per key) and `history` (a
/// timestamp-ordered range for one key) — same five columns either way, so
/// one row type for both rather than two structurally-identical ones.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct MetricHistoryRow {
    pub collector: String,
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub tags: serde_json::Value,
    pub ts: i64,
}

/// High-frequency metric persistence using one multi-row insert per batch.
///
/// The migration stores `value_type` as `TEXT` and `tags` as `JSONB`. Values
/// are therefore encoded as stable snake-case labels and SQLx's typed JSON
/// wrapper rather than debug output or an intermediate `serde_json::Value`.
///
/// `Clone` for the same reason as `HostRegistry` — see its doc comment.
#[derive(Clone)]
pub struct MetricHistory {
    pool: PgPool,
}

impl MetricHistory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_batch(&self, host_id: &str, batch: &DataBatch) -> sqlx::Result<()> {
        if batch.items.is_empty() {
            return Ok(());
        }

        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO metric_history (host_id, collector, key, value, value_type, tags, ts) ",
        );
        builder.push_values(&batch.items, |mut row, item| {
            row.push_bind(host_id)
                .push_bind(&batch.collector)
                .push_bind(&item.key)
                .push_bind(&item.value)
                .push_bind(value_type_storage_value(item.value_type))
                .push_bind(Json(&item.tags))
                .push_bind(item.timestamp_ms);
        });
        builder.build().execute(&self.pool).await?;
        Ok(())
    }

    /// Returns the most recent row per distinct `key` under `host_id`, for
    /// `GET /hosts/:id/items`. A single `DISTINCT ON` query, not a fetch-all
    /// followed by filtering in Rust — `metric_history` can be arbitrarily
    /// large per host, and this only ever needs the latest row per key.
    pub async fn latest_items(&self, host_id: &str) -> sqlx::Result<Vec<MetricHistoryRow>> {
        sqlx::query_as::<_, MetricHistoryRow>(
            "SELECT DISTINCT ON (key) collector, key, value, value_type, tags, ts
             FROM metric_history
             WHERE host_id = $1
             ORDER BY key, ts DESC",
        )
        .bind(host_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Returns rows for one `(host_id, key)`, newest first, optionally
    /// bounded by `from_ms`/`to_ms` (inclusive) and always capped at
    /// `limit` — the caller (`api/history.rs`) is responsible for
    /// defaulting/clamping `limit` to a sane bound before calling this; this
    /// method just executes whatever it's given, matching every other
    /// method here trusting its caller rather than re-validating.
    ///
    /// Matches `idx_metric_history_host_key_ts (host_id, key, ts DESC)`
    /// exactly: equality on `host_id`/`key`, a range on `ts`, ordered by
    /// `ts DESC` — no new index needed.
    pub async fn history(
        &self,
        host_id: &str,
        key: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: i64,
    ) -> sqlx::Result<Vec<MetricHistoryRow>> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT collector, key, value, value_type, tags, ts FROM metric_history WHERE host_id = ",
        );
        builder.push_bind(host_id);
        builder.push(" AND key = ");
        builder.push_bind(key);

        if let Some(from_ms) = from_ms {
            builder.push(" AND ts >= ");
            builder.push_bind(from_ms);
        }
        if let Some(to_ms) = to_ms {
            builder.push(" AND ts <= ");
            builder.push_bind(to_ms);
        }

        builder.push(" ORDER BY ts DESC LIMIT ");
        builder.push_bind(limit);

        builder
            .build_query_as::<MetricHistoryRow>()
            .fetch_all(&self.pool)
            .await
    }
}

fn value_type_storage_value(value_type: ValueType) -> &'static str {
    match value_type {
        ValueType::Float => "float",
        ValueType::Integer => "integer",
        ValueType::Text => "text",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use sqlx::Row;
    use testcontainers_modules::postgres::Postgres as PostgresImage;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::testcontainers::ContainerAsync;

    use crate::collector::MetricItem;
    use crate::transport::DataBatch;

    use super::*;

    #[test]
    fn value_type_storage_values_match_the_wire_encoding() {
        assert_eq!(value_type_storage_value(ValueType::Float), "float");
        assert_eq!(value_type_storage_value(ValueType::Integer), "integer");
        assert_eq!(value_type_storage_value(ValueType::Text), "text");
    }

    /// Holds a running container alive for the test's duration; dropping it stops it.
    /// Same testcontainers-rs convention as `pacificnm/nest`'s Phase 1-2 test support.
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
                    .with_migrations(crate::migrations::all_migrations()),
            )
            .build()
            .expect("app with postgres migrations");

        let pool = PgPool::connect(&url).await.expect("fresh postgres pool");

        TestDb {
            _container: container,
            pool,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn host_registry_upsert_heartbeat_offline_round_trip() {
        let db = start_postgres_with_schema().await;
        let registry = HostRegistry::new(db.pool.clone());
        let host_id = "host-registry-round-trip";

        registry
            .upsert_on_register(host_id, "sparrow-host")
            .await
            .expect("upsert_on_register should succeed");

        let row = sqlx::query(
            "SELECT hostname, online, last_seen IS NOT NULL AS has_last_seen \
             FROM hosts WHERE host_id = $1",
        )
        .bind(host_id)
        .fetch_one(&db.pool)
        .await
        .expect("host row after register");
        assert_eq!(row.get::<String, _>("hostname"), "sparrow-host");
        assert!(row.get::<bool, _>("online"));
        assert!(row.get::<bool, _>("has_last_seen"));

        // Re-registering the same host_id must update the row, not duplicate it.
        registry
            .upsert_on_register(host_id, "sparrow-host-renamed")
            .await
            .expect("re-register should succeed");
        let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hosts WHERE host_id = $1")
            .bind(host_id)
            .fetch_one(&db.pool)
            .await
            .expect("host row count");
        assert_eq!(row_count, 1);
        let hostname: String = sqlx::query_scalar("SELECT hostname FROM hosts WHERE host_id = $1")
            .bind(host_id)
            .fetch_one(&db.pool)
            .await
            .expect("updated hostname");
        assert_eq!(hostname, "sparrow-host-renamed");

        registry
            .mark_offline(host_id)
            .await
            .expect("mark_offline should succeed");
        let online: bool = sqlx::query_scalar("SELECT online FROM hosts WHERE host_id = $1")
            .bind(host_id)
            .fetch_one(&db.pool)
            .await
            .expect("online after mark_offline");
        assert!(!online);

        registry
            .touch_heartbeat(host_id)
            .await
            .expect("touch_heartbeat should succeed");
        let online: bool = sqlx::query_scalar("SELECT online FROM hosts WHERE host_id = $1")
            .bind(host_id)
            .fetch_one(&db.pool)
            .await
            .expect("online after touch_heartbeat");
        assert!(online);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mark_stale_offline_marks_only_hosts_past_the_threshold() {
        let db = start_postgres_with_schema().await;
        let registry = HostRegistry::new(db.pool.clone());

        let stale_host = "host-stale";
        let fresh_host = "host-fresh";
        registry
            .upsert_on_register(stale_host, "stale-host")
            .await
            .expect("register stale host");
        registry
            .upsert_on_register(fresh_host, "fresh-host")
            .await
            .expect("register fresh host");

        // Backdate only the stale host's last_seen — upsert_on_register
        // always sets it to NOW(), so there's no HostRegistry method for
        // seeding an arbitrary timestamp; this is test-only setup.
        sqlx::query("UPDATE hosts SET last_seen = NOW() - INTERVAL '1 hour' WHERE host_id = $1")
            .bind(stale_host)
            .execute(&db.pool)
            .await
            .expect("backdate stale host's last_seen");

        let updated = registry
            .mark_stale_offline(45)
            .await
            .expect("mark_stale_offline should succeed");
        assert_eq!(updated, 1, "exactly one host should have been stale");

        let stale_online: bool = sqlx::query_scalar("SELECT online FROM hosts WHERE host_id = $1")
            .bind(stale_host)
            .fetch_one(&db.pool)
            .await
            .expect("stale host online status");
        assert!(!stale_online, "the stale host should be marked offline");

        let fresh_online: bool = sqlx::query_scalar("SELECT online FROM hosts WHERE host_id = $1")
            .bind(fresh_host)
            .fetch_one(&db.pool)
            .await
            .expect("fresh host online status");
        assert!(fresh_online, "the fresh host should be left untouched");

        // Re-running the sweep with nothing newly stale should be a no-op.
        let updated_again = registry
            .mark_stale_offline(45)
            .await
            .expect("second sweep should succeed");
        assert_eq!(
            updated_again, 0,
            "already-offline hosts should not be counted again"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_returns_every_host_ordered_by_host_id() {
        let db = start_postgres_with_schema().await;
        let registry = HostRegistry::new(db.pool.clone());

        registry
            .upsert_on_register("host-b", "second")
            .await
            .expect("register host-b");
        registry
            .upsert_on_register("host-a", "first")
            .await
            .expect("register host-a");
        registry
            .mark_offline("host-b")
            .await
            .expect("mark host-b offline");

        let hosts = registry.list().await.expect("list should succeed");

        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].host_id, "host-a");
        assert_eq!(hosts[0].hostname, "first");
        assert!(hosts[0].online);
        assert!(hosts[0].last_seen_ms > 0);
        assert_eq!(hosts[1].host_id, "host-b");
        assert!(!hosts[1].online);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn metric_history_insert_batch_round_trip() {
        let db = start_postgres_with_schema().await;
        let host_id = "host-metric-history";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "sparrow-host")
            .await
            .expect("host must exist before inserting metrics (FK constraint)");

        let batch = DataBatch {
            host_id: host_id.to_string(),
            collector: "cpu".to_string(),
            items: vec![
                MetricItem {
                    key: "cpu.usage_percent".to_string(),
                    value_type: ValueType::Float,
                    value: "42.5".to_string(),
                    tags: BTreeMap::from([("core".to_string(), "0".to_string())]),
                    timestamp_ms: 1_700_000_000_000,
                },
                MetricItem {
                    key: "cpu.core_count".to_string(),
                    value_type: ValueType::Integer,
                    value: "8".to_string(),
                    tags: BTreeMap::new(),
                    timestamp_ms: 1_700_000_000_500,
                },
                MetricItem {
                    key: "cpu.governor".to_string(),
                    value_type: ValueType::Text,
                    value: "performance".to_string(),
                    tags: BTreeMap::from([("core".to_string(), "1".to_string())]),
                    timestamp_ms: 1_700_000_001_000,
                },
            ],
        };

        MetricHistory::new(db.pool.clone())
            .insert_batch(host_id, &batch)
            .await
            .expect("insert_batch should succeed");

        let row_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM metric_history WHERE host_id = $1")
                .bind(host_id)
                .fetch_one(&db.pool)
                .await
                .expect("metric_history row count");
        assert_eq!(row_count, batch.items.len() as i64);

        let rows = sqlx::query(
            "SELECT collector, key, value, value_type, tags, ts \
             FROM metric_history WHERE host_id = $1 ORDER BY key",
        )
        .bind(host_id)
        .fetch_all(&db.pool)
        .await
        .expect("metric_history rows");

        let mut expected = batch.items.clone();
        expected.sort_by(|a, b| a.key.cmp(&b.key));

        assert_eq!(rows.len(), expected.len());
        for (row, item) in rows.iter().zip(expected.iter()) {
            assert_eq!(row.get::<String, _>("collector"), batch.collector);
            assert_eq!(row.get::<String, _>("key"), item.key);
            assert_eq!(row.get::<String, _>("value"), item.value);
            assert_eq!(
                row.get::<String, _>("value_type"),
                value_type_storage_value(item.value_type)
            );
            assert_eq!(
                row.get::<serde_json::Value, _>("tags"),
                serde_json::to_value(&item.tags).expect("tags should serialize")
            );
            assert_eq!(row.get::<i64, _>("ts"), item.timestamp_ms);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn latest_items_returns_only_the_newest_row_per_key() {
        let db = start_postgres_with_schema().await;
        let host_id = "host-latest-items";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "sparrow-host")
            .await
            .expect("host must exist before inserting metrics (FK constraint)");
        let history = MetricHistory::new(db.pool.clone());

        // Two batches for cpu.usage_percent, an older and a newer value —
        // only the newer one should come back. memory.used_bytes has just
        // one row, and disk never reports at all in this test.
        history
            .insert_batch(
                host_id,
                &DataBatch {
                    host_id: host_id.to_string(),
                    collector: "cpu".to_string(),
                    items: vec![MetricItem {
                        key: "cpu.usage_percent".to_string(),
                        value_type: ValueType::Float,
                        value: "10.0".to_string(),
                        tags: BTreeMap::new(),
                        timestamp_ms: 1_700_000_000_000,
                    }],
                },
            )
            .await
            .expect("insert older cpu batch");
        history
            .insert_batch(
                host_id,
                &DataBatch {
                    host_id: host_id.to_string(),
                    collector: "cpu".to_string(),
                    items: vec![MetricItem {
                        key: "cpu.usage_percent".to_string(),
                        value_type: ValueType::Float,
                        value: "55.5".to_string(),
                        tags: BTreeMap::new(),
                        timestamp_ms: 1_700_000_002_000,
                    }],
                },
            )
            .await
            .expect("insert newer cpu batch");
        history
            .insert_batch(
                host_id,
                &DataBatch {
                    host_id: host_id.to_string(),
                    collector: "memory".to_string(),
                    items: vec![MetricItem {
                        key: "memory.used_bytes".to_string(),
                        value_type: ValueType::Integer,
                        value: "12345".to_string(),
                        tags: BTreeMap::new(),
                        timestamp_ms: 1_700_000_001_000,
                    }],
                },
            )
            .await
            .expect("insert memory batch");

        let items = history
            .latest_items(host_id)
            .await
            .expect("latest_items should succeed");

        assert_eq!(items.len(), 2, "one row per distinct key");
        let cpu = items
            .iter()
            .find(|item| item.key == "cpu.usage_percent")
            .expect("cpu.usage_percent present");
        assert_eq!(cpu.value, "55.5", "the newer cpu value should win");
        assert_eq!(cpu.ts, 1_700_000_002_000);
        let memory = items
            .iter()
            .find(|item| item.key == "memory.used_bytes")
            .expect("memory.used_bytes present");
        assert_eq!(memory.value, "12345");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn history_respects_range_and_limit_and_orders_newest_first() {
        let db = start_postgres_with_schema().await;
        let host_id = "host-history";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "sparrow-host")
            .await
            .expect("host must exist before inserting metrics (FK constraint)");
        let history = MetricHistory::new(db.pool.clone());

        // Five points 1000ms apart, plus one unrelated key that must never
        // appear in cpu.usage_percent's history.
        for (i, value) in ["1", "2", "3", "4", "5"].iter().enumerate() {
            history
                .insert_batch(
                    host_id,
                    &DataBatch {
                        host_id: host_id.to_string(),
                        collector: "cpu".to_string(),
                        items: vec![MetricItem {
                            key: "cpu.usage_percent".to_string(),
                            value_type: ValueType::Float,
                            value: value.to_string(),
                            tags: BTreeMap::new(),
                            timestamp_ms: 1_700_000_000_000 + (i as i64) * 1000,
                        }],
                    },
                )
                .await
                .expect("insert cpu point");
        }
        history
            .insert_batch(
                host_id,
                &DataBatch {
                    host_id: host_id.to_string(),
                    collector: "memory".to_string(),
                    items: vec![MetricItem {
                        key: "memory.used_bytes".to_string(),
                        value_type: ValueType::Integer,
                        value: "999".to_string(),
                        tags: BTreeMap::new(),
                        timestamp_ms: 1_700_000_002_500,
                    }],
                },
            )
            .await
            .expect("insert unrelated memory point");

        // No bounds, generous limit: all 5 cpu points, newest first.
        let all = history
            .history(host_id, "cpu.usage_percent", None, None, 100)
            .await
            .expect("history should succeed");
        assert_eq!(
            all.iter().map(|row| row.value.as_str()).collect::<Vec<_>>(),
            vec!["5", "4", "3", "2", "1"],
            "should be ordered newest first and exclude other keys"
        );

        // Range bounds: only points 2 (ts=...002000) through 4
        // (ts=...004000) inclusive.
        let ranged = history
            .history(
                host_id,
                "cpu.usage_percent",
                Some(1_700_000_002_000),
                Some(1_700_000_004_000),
                100,
            )
            .await
            .expect("ranged history should succeed");
        assert_eq!(
            ranged
                .iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["4", "3", "2"]
        );

        // Limit caps the result even when more rows match.
        let limited = history
            .history(host_id, "cpu.usage_percent", None, None, 2)
            .await
            .expect("limited history should succeed");
        assert_eq!(
            limited
                .iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["5", "4"]
        );
    }

    #[tokio::test]
    async fn insert_batch_with_no_items_is_a_no_op_and_needs_no_connection() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        let batch = DataBatch {
            host_id: "host-empty".to_string(),
            collector: "cpu".to_string(),
            items: Vec::new(),
        };

        MetricHistory::new(pool)
            .insert_batch("host-empty", &batch)
            .await
            .expect("empty batch should short-circuit before touching the pool");
    }
}
