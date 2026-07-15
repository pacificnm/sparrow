use sqlx::PgPool;
use crate::transport::DataBatch;

pub struct HostRegistry {
    pool: PgPool,
}

impl HostRegistry {
    pub fn new(pool: PgPool) -> Self { Self { pool } }
    pub async fn upsert_on_register(&self, host_id: &str, hostname: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO hosts (host_id, hostname, last_seen, online) VALUES ($1, $2, NOW(), true) ON CONFLICT (host_id) DO UPDATE SET hostname = $2, last_seen = NOW(), online = true"
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

pub struct MetricHistory {
    pool: PgPool,
}

impl MetricHistory {
    pub fn new(pool: PgPool) -> Self { Self { pool } }
    pub async fn insert_batch(&self, host_id: &str, batch: &DataBatch) -> sqlx::Result<()> {
        if batch.items.is_empty() {
            return Ok(());
        }
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO metric_history (host_id, collector, key, value, value_type, tags, ts)"
        );
        builder.push_values(&batch.items, |mut row, item| {
            row.push_bind(host_id)
                .push_bind(&batch.collector)
                .push_bind(&item.key)
                .push_bind(&item.value)
                .push_bind(format!("{:?}", item.value_type))
                .push_bind(serde_json::to_value(&item.tags).unwrap_or_default())
                .push_bind(item.timestamp_ms);
        });
        builder.build().execute(&self.pool).await?;
        Ok(())
    }
}
