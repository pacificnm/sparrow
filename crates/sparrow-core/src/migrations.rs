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

        assert_eq!(ids, vec!["001_create_hosts", "002_create_metric_history"]);
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
            vec!["001_create_hosts", "002_create_metric_history"]
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

        assert_eq!(host_columns, 4);
        assert_eq!(metric_columns, 8);

        drop(built);
    }
}
