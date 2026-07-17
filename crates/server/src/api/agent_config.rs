//! `GET /hosts/:id/config` and `PUT /hosts/:id/config`.
//!
//! Same `RequestContext` ground truth as `api/hosts.rs`/`api/history.rs`
//! (verified against `nest-http-serve/src/context.rs` directly): no
//! service-access method exists, so `PgPool`/`MqttClient` are captured by
//! the route closures `routes()` builds, not fetched from inside the
//! handler. `ctx.json::<T>()` (also verified there) deserializes the PUT
//! body, rejecting an empty body or invalid JSON with a `ServeError` rather
//! than panicking.

use std::collections::BTreeMap;

use nest_error::NestError;
use nest_http_serve::{HttpResult, Json, RequestContext, RouteGroup, ServeError};
use nest_mqtt::{MqttClient, MqttQos};
use sparrow_core::config::AgentConfigOverride;
use sparrow_core::transport::Topics;
use sqlx::types::Json as SqlxJson;
use sqlx::PgPool;

/// Builds the `/api/hosts/:id/config` routes.
pub fn routes(pool: PgPool, mqtt: MqttClient) -> RouteGroup {
    let get_pool = pool.clone();
    RouteGroup::new("/api")
        .get("/hosts/:id/config", move |ctx| {
            let pool = get_pool.clone();
            async move { get_agent_config(ctx, pool).await }
        })
        .put("/hosts/:id/config", move |ctx| {
            let pool = pool.clone();
            let mqtt = mqtt.clone();
            async move { update_agent_config(ctx, pool, mqtt).await }
        })
}

async fn get_agent_config(ctx: RequestContext, pool: PgPool) -> HttpResult {
    let host_id = ctx.param("id")?;
    let config = fetch_agent_config(&pool, host_id)
        .await
        .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;
    Json(config).into_response()
}

async fn update_agent_config(ctx: RequestContext, pool: PgPool, mqtt: MqttClient) -> HttpResult {
    let host_id = ctx.param("id")?;
    let override_: AgentConfigOverride = ctx.json()?;

    upsert_agent_config(&pool, host_id, &override_)
        .await
        .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;

    // Published retained (not fire-and-forget like a status update): an
    // agent that connects or reconnects *after* this publish still
    // receives it immediately on subscribe — no polling, no "agent asks
    // server for its config on startup" round trip needed. This is
    // precisely why Phase 2 called out MQTT retained messages as
    // "config-push almost for free."
    mqtt.publish(
        &Topics::config(host_id),
        override_.to_payload(),
        MqttQos::AtLeastOnce,
        true,
    )
    .await
    .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;

    Json(override_).into_response()
}

#[derive(sqlx::FromRow)]
struct AgentConfigRow {
    disabled_collectors: SqlxJson<Vec<String>>,
    collector_intervals: SqlxJson<BTreeMap<String, u64>>,
}

/// Returns `host_id`'s stored override, or [`AgentConfigOverride::default`]
/// (everything enabled, default intervals) if it has no row — Issue 9.1's
/// explicit requirement: a missing row is not an error condition anywhere.
async fn fetch_agent_config(pool: &PgPool, host_id: &str) -> sqlx::Result<AgentConfigOverride> {
    let row = sqlx::query_as::<_, AgentConfigRow>(
        "SELECT disabled_collectors, collector_intervals FROM agent_configs WHERE host_id = $1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(row) => AgentConfigOverride {
            disabled_collectors: row.disabled_collectors.0,
            collector_intervals: row.collector_intervals.0,
        },
        None => AgentConfigOverride::default(),
    })
}

/// Upserts `host_id`'s full override — a `PUT`, not a partial patch: the
/// stored row becomes exactly `override_`, replacing whatever was there
/// before rather than merging with it field-by-field.
async fn upsert_agent_config(
    pool: &PgPool,
    host_id: &str,
    override_: &AgentConfigOverride,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO agent_configs (host_id, disabled_collectors, collector_intervals, updated_at)
         VALUES ($1, $2, $3, NOW())
         ON CONFLICT (host_id) DO UPDATE SET
             disabled_collectors = EXCLUDED.disabled_collectors,
             collector_intervals = EXCLUDED.collector_intervals,
             updated_at = EXCLUDED.updated_at",
    )
    .bind(host_id)
    .bind(SqlxJson(&override_.disabled_collectors))
    .bind(SqlxJson(&override_.collector_intervals))
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use nest_http_serve::{HttpMethod, RouteRegistry};
    use nest_mqtt::MqttConfig;

    use super::*;

    /// Confirms `routes()` wires up the two expected patterns/methods, the
    /// same non-Docker check `api/hosts.rs`/`api/history.rs`'s own tests
    /// do. `MqttClient::connect` doesn't require a live broker (only its
    /// background event-loop poll task would fail to reach one — confirmed
    /// by `config_reload.rs`'s own equivalent test), so this needs no
    /// Docker either.
    #[tokio::test]
    async fn routes_registers_the_expected_patterns_and_methods() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        let mqtt = MqttClient::connect(&MqttConfig {
            client_id: "agent-config-routes-test".to_string(),
            broker_host: "127.0.0.1".to_string(),
            broker_port: 1,
            keep_alive_secs: 5,
            username: None,
            password: None,
            last_will: None,
            capacity: 16,
        })
        .await
        .expect("client construction does not require a live connection");

        let mut registry = RouteRegistry::new();
        registry.add_group(routes(pool, mqtt));

        let mut found: Vec<(HttpMethod, &str)> = registry
            .routes()
            .iter()
            .map(|route| (route.method, route.pattern.as_str()))
            .collect();
        found.sort_by_key(|(method, _)| format!("{method:?}"));

        assert_eq!(
            found,
            vec![
                (HttpMethod::Get, "/api/hosts/:id/config"),
                (HttpMethod::Put, "/api/hosts/:id/config"),
            ]
        );
    }

    // --- Docker-backed tests below: the storage half of this endpoint
    // (upsert/default-read round trip), against a real testcontainers
    // Postgres — no MQTT needed for this part, per the phase-9 spec's own
    // "Tests" section. The full HTTP+MQTT-publish path (including the
    // "freshly connecting agent still gets the retained config" case) is
    // Issue 9.4's job.

    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use sparrow_core::storage::HostRegistry;
    use testcontainers_modules::postgres::Postgres as PostgresImage;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::testcontainers::ContainerAsync;

    /// Holds a running Postgres container (with Sparrow's migrations
    /// already applied) alive for the test's duration. Same recipe as
    /// `sparrow-core/src/storage.rs`'s and `alerting.rs`'s own test
    /// modules (duplicated, not imported — private to each module's tests).
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

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_agent_config_defaults_when_no_row_exists() {
        let db = start_postgres_with_schema().await;
        HostRegistry::new(db.pool.clone())
            .upsert_on_register("agent-config-no-row-host", "test-host")
            .await
            .expect("seed host for the FK reference");

        let config = fetch_agent_config(&db.pool, "agent-config-no-row-host")
            .await
            .expect("fetch_agent_config should succeed");

        assert_eq!(
            config,
            AgentConfigOverride::default(),
            "a host with no agent_configs row must read back as everything \
             enabled, default intervals — not an error"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_agent_config_then_fetch_round_trips() {
        let db = start_postgres_with_schema().await;
        let host_id = "agent-config-round-trip-host";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host for the FK reference");

        let override_ = AgentConfigOverride {
            disabled_collectors: vec!["disk".to_string()],
            collector_intervals: BTreeMap::from([("cpu".to_string(), 5)]),
        };
        upsert_agent_config(&db.pool, host_id, &override_)
            .await
            .expect("upsert should succeed");

        let fetched = fetch_agent_config(&db.pool, host_id)
            .await
            .expect("fetch_agent_config should succeed");
        assert_eq!(fetched, override_);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_agent_config_replaces_rather_than_merges() {
        let db = start_postgres_with_schema().await;
        let host_id = "agent-config-replace-host";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host for the FK reference");

        upsert_agent_config(
            &db.pool,
            host_id,
            &AgentConfigOverride {
                disabled_collectors: vec!["disk".to_string(), "memory".to_string()],
                collector_intervals: BTreeMap::from([("cpu".to_string(), 5)]),
            },
        )
        .await
        .expect("first upsert should succeed");

        // A second PUT that omits both fields (defaults) must fully
        // replace the first, not leave its values in place merged in.
        upsert_agent_config(&db.pool, host_id, &AgentConfigOverride::default())
            .await
            .expect("second upsert should succeed");

        let fetched = fetch_agent_config(&db.pool, host_id)
            .await
            .expect("fetch_agent_config should succeed");
        assert_eq!(
            fetched,
            AgentConfigOverride::default(),
            "a PUT must fully replace the stored override, not merge with the previous one"
        );
    }
}
