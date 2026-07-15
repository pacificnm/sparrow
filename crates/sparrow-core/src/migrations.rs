use nest_data::SqlMigration;
use nest_data::Migration;

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
                value_type TEXT NOT NULL,
                tags JSONB NOT NULL DEFAULT '{}',
                ts BIGINT NOT NULL
            );
            CREATE INDEX idx_metric_history_host_key_ts ON metric_history (host_id, key, ts DESC);",
            "DROP TABLE metric_history",
        )),
    ]
}
