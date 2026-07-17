use nest_data::Migration;
use nest_data::SqlMigration;

/// Returns Sparrow's database migrations in application order.
///
/// The server host registers these with
/// `PostgresDataModule::new(config).with_migrations(all_migrations())`.
/// Agents do not need local database access in the current plan, so this core
/// crate only exposes the migration list and leaves host wiring to the server
/// crate when that crate is introduced.
pub fn all_migrations() -> Vec<Box<dyn Migration>> {
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
                value_type TEXT NOT NULL CHECK (value_type IN ('float', 'integer', 'text')),
                tags JSONB NOT NULL DEFAULT '{}',
                ts BIGINT NOT NULL
            );
            CREATE INDEX idx_metric_history_host_key_ts ON metric_history (host_id, key, ts DESC);",
            "DROP TABLE metric_history",
        )),
        Box::new(SqlMigration::new(
            "003_create_rules",
            "CREATE TABLE rules (
                id BIGSERIAL PRIMARY KEY,
                host_id TEXT REFERENCES hosts(host_id),
                item_key TEXT NOT NULL,
                operator TEXT NOT NULL,
                threshold DOUBLE PRECISION NOT NULL,
                severity TEXT NOT NULL,
                sustained_for_secs BIGINT NOT NULL DEFAULT 0,
                enabled BOOLEAN NOT NULL DEFAULT true
            )",
            "DROP TABLE rules",
        )),
        Box::new(SqlMigration::new(
            "004_create_problems",
            "CREATE TABLE problems (
                id BIGSERIAL PRIMARY KEY,
                rule_id BIGINT NOT NULL REFERENCES rules(id),
                host_id TEXT NOT NULL REFERENCES hosts(host_id),
                status TEXT NOT NULL,
                opened_at BIGINT NOT NULL,
                resolved_at BIGINT,
                last_value DOUBLE PRECISION NOT NULL
            );
            -- Only one OPEN problem per (rule_id, host_id) at a time. Enforced
            -- at the application level too (Issue 8.3's evaluation loop), but
            -- keep this index regardless: it turns \"the evaluation loop has a
            -- bug and double-opens a Problem\" from a silent data-quality issue
            -- into a loud constraint-violation error during testing.
            CREATE UNIQUE INDEX idx_one_open_problem_per_rule_host
                ON problems (rule_id, host_id) WHERE status = 'open';",
            "DROP TABLE problems",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use sqlx::PgPool;
    use testcontainers_modules::postgres::Postgres as PostgresImage;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::testcontainers::ContainerAsync;

    struct TestDb {
        _container: ContainerAsync<PostgresImage>,
        url: String,
    }

    async fn start_postgres() -> Result<TestDb, String> {
        let container = PostgresImage::default()
            .start()
            .await
            .map_err(|error| format!("failed to start postgres testcontainer: {error}"))?;
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container port");

        Ok(TestDb {
            _container: container,
            url: format!("postgres://postgres:postgres@{host}:{port}/postgres"),
        })
    }

    #[test]
    fn migrations_are_ordered_and_named() {
        let migrations = all_migrations();
        let ids: Vec<_> = migrations.iter().map(|migration| migration.id()).collect();

        assert_eq!(
            ids,
            vec![
                "001_create_hosts",
                "002_create_metric_history",
                "003_create_rules",
                "004_create_problems",
            ]
        );
    }

    #[test]
    fn metric_history_schema_matches_storage_encoding() {
        let migrations = all_migrations();
        let metric_history = migrations
            .iter()
            .find(|migration| migration.id() == "002_create_metric_history")
            .expect("metric_history migration");
        let up_sql = metric_history.up_sql();

        assert!(up_sql.contains(
            "value_type TEXT NOT NULL CHECK (value_type IN ('float', 'integer', 'text'))"
        ));
        assert!(up_sql.contains("tags JSONB NOT NULL DEFAULT '{}'"));
        assert!(up_sql.contains(
            "CREATE INDEX idx_metric_history_host_key_ts ON metric_history (host_id, key, ts DESC)"
        ));
    }

    #[test]
    fn problems_migration_keeps_the_partial_unique_index() {
        let migrations = all_migrations();
        let problems = migrations
            .iter()
            .find(|migration| migration.id() == "004_create_problems")
            .expect("problems migration");
        let up_sql = problems.up_sql();

        // Not "redundant" with Issue 8.3's application-level check — this is
        // what turns a double-open bug in the evaluation loop into a loud
        // constraint violation during testing instead of a silent
        // data-quality issue. Must not get "simplified" away.
        assert!(up_sql.contains(
            "CREATE UNIQUE INDEX idx_one_open_problem_per_rule_host\n                ON problems (rule_id, host_id) WHERE status = 'open';"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_data_module_applies_migrations_to_fresh_postgres() {
        let db = match start_postgres().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("skipping PostgreSQL migration integration test: {error}");
                return;
            }
        };

        let built = nest_core::AppBuilder::new()
            .module(DataModule)
            .module(
                PostgresDataModule::new(PostgresConfig::new(db.url.clone()))
                    .with_migrations(all_migrations()),
            )
            .build()
            .expect("app with postgres migrations");

        let pool = PgPool::connect(&db.url).await.expect("fresh postgres pool");
        let applied: Vec<String> =
            sqlx::query_scalar("SELECT id FROM _nest_migrations ORDER BY applied_at ASC, id ASC")
                .fetch_all(&pool)
                .await
                .expect("applied migration rows");

        assert_eq!(
            applied,
            vec![
                "001_create_hosts",
                "002_create_metric_history",
                "003_create_rules",
                "004_create_problems",
            ]
        );

        let host_columns: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'hosts'")
                .fetch_one(&pool)
                .await
                .expect("host column count");
        let metric_columns: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'metric_history'")
                .fetch_one(&pool)
                .await
                .expect("metric column count");
        let rules_columns: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'rules'")
                .fetch_one(&pool)
                .await
                .expect("rules column count");
        let problems_columns: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'problems'")
                .fetch_one(&pool)
                .await
                .expect("problems column count");

        assert_eq!(host_columns, 4);
        assert_eq!(metric_columns, 8);
        assert_eq!(rules_columns, 8);
        assert_eq!(problems_columns, 7);

        // The partial unique index this migration exists for — confirm it's
        // actually created in Postgres, not just present in the SQL string.
        let index_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_one_open_problem_per_rule_host')",
        )
        .fetch_one(&pool)
        .await
        .expect("index existence check");
        assert!(
            index_exists,
            "idx_one_open_problem_per_rule_host should exist after migrating"
        );

        // The index must actually enforce one-open-problem-per-rule-host —
        // insert a rule, open two problems for the same (rule_id, host_id)
        // pair, and confirm the second one is rejected, not silently
        // allowed. This is the actual behavior the migration exists for.
        crate::storage::HostRegistry::new(pool.clone())
            .upsert_on_register("problems-index-test-host", "test-host")
            .await
            .expect("seed host for the FK reference");
        let rule_id: i64 = sqlx::query_scalar(
            "INSERT INTO rules (host_id, item_key, operator, threshold, severity)
             VALUES ($1, 'cpu.usage_percent', 'greater_than', 90.0, 'warning')
             RETURNING id",
        )
        .bind("problems-index-test-host")
        .fetch_one(&pool)
        .await
        .expect("insert rule");

        sqlx::query(
            "INSERT INTO problems (rule_id, host_id, status, opened_at, last_value)
             VALUES ($1, $2, 'open', 0, 95.0)",
        )
        .bind(rule_id)
        .bind("problems-index-test-host")
        .execute(&pool)
        .await
        .expect("first open problem should insert cleanly");

        let second_insert = sqlx::query(
            "INSERT INTO problems (rule_id, host_id, status, opened_at, last_value)
             VALUES ($1, $2, 'open', 1, 96.0)",
        )
        .bind(rule_id)
        .bind("problems-index-test-host")
        .execute(&pool)
        .await;
        assert!(
            second_insert.is_err(),
            "a second OPEN problem for the same (rule_id, host_id) must be rejected by the partial unique index"
        );

        drop(built);
    }
}
