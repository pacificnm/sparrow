use sqlx::{types::Json, PgPool};

use crate::collector::ValueType;
use crate::transport::DataBatch;

/// Low-frequency persistence for agent registration and liveness.
///
/// These operations deliberately use direct SQL rather than
/// `nest_data::AsyncRepository`: they are narrow state transitions, not a
/// general-purpose per-row CRUD surface.
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
}

/// High-frequency metric persistence using one multi-row insert per batch.
///
/// The migration stores `value_type` as `TEXT` and `tags` as `JSONB`. Values
/// are therefore encoded as stable snake-case labels and SQLx's typed JSON
/// wrapper rather than debug output or an intermediate `serde_json::Value`.
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
    use super::*;

    #[test]
    fn value_type_storage_values_match_the_wire_encoding() {
        assert_eq!(value_type_storage_value(ValueType::Float), "float");
        assert_eq!(value_type_storage_value(ValueType::Integer), "integer");
        assert_eq!(value_type_storage_value(ValueType::Text), "text");
    }
}
