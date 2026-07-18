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
            tls: None,
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
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    /// Holds a running Postgres container (with Sparrow's migrations
    /// already applied) alive for the test's duration. Same recipe as
    /// `sparrow-core/src/storage.rs`'s and `alerting.rs`'s own test
    /// modules (duplicated, not imported — private to each module's tests).
    /// `pgvector/pgvector:pg16`, not the plain `postgres` image — Issue
    /// 10.4's `resolved_incidents` migration needs the `vector` extension
    /// installable.
    struct TestDb {
        _container: ContainerAsync<GenericImage>,
        pool: PgPool,
    }

    async fn start_postgres_with_schema() -> TestDb {
        let container = GenericImage::new("pgvector/pgvector", "pg16")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
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

    // --- Milestone-closing end-to-end tests (Issue 9.4): a real Postgres +
    // Mosquitto + HTTP server + real sparrow-agent components (the same
    // ones main.rs wires up), driven through the actual PUT endpoint —
    // not by publishing to MQTT directly like config_reload_live.rs's own
    // (issue #15) test does. Case (c) below is the one the phase-9 spec's
    // "do not skip" list calls out explicitly: it's the only test that
    // actually proves retained-message semantics work, not just live
    // pub/sub.

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use nest_http_serve::HttpServer;
    use nest_task::TaskManager;
    use nest_task_runtime::{TaskManagerConfig, TaskManagerService};
    use sparrow_agent::config::AgentConfig;
    use sparrow_agent::config_reload::ConfigReload;
    use sparrow_agent::publisher::Publisher;
    use sparrow_agent::scheduler::BatchSink;
    use sparrow_core::transport::DataBatch;
    use testcontainers::ContainerAsync as MosquittoContainer;

    /// Holds a running Mosquitto container alive for the test's duration.
    /// Same recipe as `crates/agent/tests/support/mod.rs` and
    /// `crates/server/tests/support/mod.rs`'s own `start_broker` — every
    /// `tests/*.rs`/`#[cfg(test)] mod` compiles separately, so this can't
    /// be shared, only duplicated.
    struct TestBroker {
        #[allow(dead_code)]
        container: MosquittoContainer<GenericImage>,
        host: String,
        port: u16,
    }

    async fn start_broker() -> TestBroker {
        let container = GenericImage::new("eclipse-mosquitto", "2")
            .with_exposed_port(1883.tcp())
            .with_wait_for(WaitFor::message_on_stderr("running"))
            .start()
            .await
            .expect("failed to start mosquitto testcontainer");
        let host = container
            .get_host()
            .await
            .expect("container host")
            .to_string();
        let port = container
            .get_host_port_ipv4(1883)
            .await
            .expect("container port");
        TestBroker {
            container,
            host,
            port,
        }
    }

    /// Builds a real `AgentConfig` with every collector's interval
    /// overridden to 1s, so the test doesn't wait out `disk`'s real 60s
    /// default — same reasoning as `crates/agent/tests/support/mod.rs`'s
    /// `test_agent_config`, duplicated rather than imported (private to
    /// that crate's own tests).
    fn e2e_agent_config(host_id: &str, broker: &TestBroker) -> AgentConfig {
        AgentConfig {
            host_id: host_id.to_string(),
            broker_host: broker.host.clone(),
            broker_port: broker.port,
            collector_intervals: BTreeMap::from([
                ("cpu".to_string(), 1),
                ("memory".to_string(), 1),
                ("disk".to_string(), 1),
            ]),
            disabled_collectors: Vec::new(),
            mqtt_password: None,
        }
    }

    fn e2e_task_manager() -> TaskManagerService {
        let app = nest_core::AppBuilder::new()
            .build()
            .expect("empty app context")
            .context;
        let manager = TaskManagerService::new(
            tokio::runtime::Handle::current(),
            TaskManagerConfig::default(),
        );
        manager.set_context(app);
        manager
    }

    /// Polls `predicate` until it's `true` or `timeout` elapses. Duplicated
    /// from `crates/agent/tests/support/mod.rs`'s `wait_until` (private to
    /// that crate's tests).
    async fn wait_until(
        timeout: Duration,
        poll_interval: Duration,
        mut predicate: impl FnMut() -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn put_agent_config_is_retained_and_a_running_agent_picks_it_up() {
        let db = start_postgres_with_schema().await;
        let broker = start_broker().await;
        let host_id = "agent-config-e2e-running-host";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host for the FK reference");

        let server_mqtt =
            MqttClient::connect(&MqttConfig::new(&broker.host, broker.port, "server"))
                .await
                .expect("server mqtt client should connect");
        let http_server = HttpServer::builder()
            .routes(routes(db.pool.clone(), server_mqtt))
            .spawn()
            .await
            .expect("http server should spawn");

        // The already-running agent: ConfigReload spawns its local
        // (nothing-disabled) collector set itself at startup, same as
        // main.rs's real bootstrap.
        let agent_config = e2e_agent_config(host_id, &broker);
        let agent_client = MqttClient::connect(&MqttConfig::new(
            &agent_config.broker_host,
            agent_config.broker_port,
            &agent_config.host_id,
        ))
        .await
        .expect("agent mqtt client should connect");
        let sink: Arc<dyn BatchSink> =
            Arc::new(Publisher::new(agent_client.clone(), &agent_config));
        let manager = e2e_task_manager();
        manager
            .spawn(ConfigReload::new(
                agent_client.clone(),
                &agent_config,
                Arc::clone(&sink),
                manager.clone(),
            ))
            .await
            .expect("config_reload should spawn");

        // Observes Topics::config(host_id) directly — confirms (a) "the
        // retained message lands on the topic", independent of whether the
        // agent reacts correctly to it.
        let config_observer = MqttClient::connect(&MqttConfig::new(
            &broker.host,
            broker.port,
            "config-topic-observer",
        ))
        .await
        .expect("config observer should connect");
        let config_stream = config_observer
            .subscribe(&Topics::config(host_id), MqttQos::AtLeastOnce)
            .await
            .expect("config observer should subscribe");
        tokio::pin!(config_stream);

        // Observes Topics::data(host_id) — confirms (b): the running
        // agent's own collector set actually reacts.
        let data_observer = MqttClient::connect(&MqttConfig::new(
            &broker.host,
            broker.port,
            "data-topic-observer",
        ))
        .await
        .expect("data observer should connect");
        let data_stream = data_observer
            .subscribe(&Topics::data(host_id), MqttQos::AtLeastOnce)
            .await
            .expect("data observer should subscribe");
        tokio::time::sleep(Duration::from_millis(300)).await;

        let cpu_seen = Arc::new(AtomicBool::new(false));
        let disk_seen = Arc::new(AtomicBool::new(false));
        {
            let cpu_seen = Arc::clone(&cpu_seen);
            let disk_seen = Arc::clone(&disk_seen);
            tokio::spawn(async move {
                tokio::pin!(data_stream);
                while let Some(message) = data_stream.next().await {
                    if let Ok(batch) = DataBatch::from_payload(&message.payload) {
                        match batch.collector.as_str() {
                            "cpu" => cpu_seen.store(true, Ordering::SeqCst),
                            "disk" => disk_seen.store(true, Ordering::SeqCst),
                            _ => {}
                        }
                    }
                }
            });
        }

        assert!(
            wait_until(Duration::from_secs(10), Duration::from_millis(200), || {
                cpu_seen.load(Ordering::SeqCst) && disk_seen.load(Ordering::SeqCst)
            })
            .await,
            "expected cpu and disk data within 10s of the baseline (nothing-disabled) config"
        );

        // The actual thing under test: PUT through the real HTTP endpoint,
        // not a raw MQTT publish.
        let http_client = reqwest::Client::new();
        let response = http_client
            .put(format!(
                "{}/api/hosts/{host_id}/config",
                http_server.base_url()
            ))
            .json(&AgentConfigOverride {
                disabled_collectors: vec!["disk".to_string()],
                collector_intervals: BTreeMap::new(),
            })
            .send()
            .await
            .expect("PUT should succeed");
        assert!(response.status().is_success(), "PUT should return 2xx");
        let applied: AgentConfigOverride = response.json().await.expect("PUT body should be JSON");
        assert_eq!(applied.disabled_collectors, vec!["disk".to_string()]);

        // (a) the retained message lands on the topic.
        let landed = tokio::time::timeout(Duration::from_secs(5), config_stream.next())
            .await
            .expect("config message should arrive within 5s")
            .expect("config stream should not end");
        let landed_override = AgentConfigOverride::from_payload(&landed.payload)
            .expect("retained message should decode as AgentConfigOverride");
        assert_eq!(
            landed_override.disabled_collectors,
            vec!["disk".to_string()]
        );

        // (b) the running agent picks it up within one cancel-poll cycle.
        cpu_seen.store(false, Ordering::SeqCst);
        disk_seen.store(false, Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_secs(5), Duration::from_millis(200), || {
                cpu_seen.load(Ordering::SeqCst)
            })
            .await,
            "cpu should keep publishing after disk is disabled"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !disk_seen.load(Ordering::SeqCst),
            "the running agent should have stopped publishing disk.* items"
        );

        http_server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn put_agent_config_reaches_a_freshly_connecting_agent() {
        // Case (c) from the phase-9 spec's "do not skip" list: the only
        // test that actually proves retained-message semantics work, not
        // just live pub/sub. A PUT happens *before* any agent for this
        // host exists; the agent that starts afterward must still come up
        // with disk disabled from the start, purely from the retained
        // message it receives on its very first subscribe.
        let db = start_postgres_with_schema().await;
        let broker = start_broker().await;
        let host_id = "agent-config-e2e-fresh-host";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host for the FK reference");

        let server_mqtt =
            MqttClient::connect(&MqttConfig::new(&broker.host, broker.port, "server"))
                .await
                .expect("server mqtt client should connect");
        let http_server = HttpServer::builder()
            .routes(routes(db.pool.clone(), server_mqtt))
            .spawn()
            .await
            .expect("http server should spawn");

        let http_client = reqwest::Client::new();
        let response = http_client
            .put(format!(
                "{}/api/hosts/{host_id}/config",
                http_server.base_url()
            ))
            .json(&AgentConfigOverride {
                disabled_collectors: vec!["disk".to_string()],
                collector_intervals: BTreeMap::new(),
            })
            .send()
            .await
            .expect("PUT should succeed");
        assert!(response.status().is_success(), "PUT should return 2xx");

        // Only now does the agent for this host come into existence — same
        // bootstrap as main.rs: ConfigReload spawns its (plain,
        // nothing-disabled) local-defaults collector set at startup, then
        // immediately reconciles against the already-retained override on
        // its very first subscribe.
        let agent_config = e2e_agent_config(host_id, &broker);
        let agent_client = MqttClient::connect(&MqttConfig::new(
            &agent_config.broker_host,
            agent_config.broker_port,
            &agent_config.host_id,
        ))
        .await
        .expect("agent mqtt client should connect");
        let sink: Arc<dyn BatchSink> =
            Arc::new(Publisher::new(agent_client.clone(), &agent_config));
        let manager = e2e_task_manager();
        manager
            .spawn(ConfigReload::new(
                agent_client.clone(),
                &agent_config,
                Arc::clone(&sink),
                manager.clone(),
            ))
            .await
            .expect("config_reload should spawn");

        let data_observer = MqttClient::connect(&MqttConfig::new(
            &broker.host,
            broker.port,
            "fresh-agent-data-observer",
        ))
        .await
        .expect("data observer should connect");
        let data_stream = data_observer
            .subscribe(&Topics::data(host_id), MqttQos::AtLeastOnce)
            .await
            .expect("data observer should subscribe");

        let cpu_seen = Arc::new(AtomicBool::new(false));
        let disk_seen = Arc::new(AtomicBool::new(false));
        {
            let cpu_seen = Arc::clone(&cpu_seen);
            let disk_seen = Arc::clone(&disk_seen);
            tokio::spawn(async move {
                tokio::pin!(data_stream);
                while let Some(message) = data_stream.next().await {
                    if let Ok(batch) = DataBatch::from_payload(&message.payload) {
                        match batch.collector.as_str() {
                            "cpu" => cpu_seen.store(true, Ordering::SeqCst),
                            "disk" => disk_seen.store(true, Ordering::SeqCst),
                            _ => {}
                        }
                    }
                }
            });
        }

        assert!(
            wait_until(Duration::from_secs(10), Duration::from_millis(200), || {
                cpu_seen.load(Ordering::SeqCst)
            })
            .await,
            "cpu should still publish — only disk was disabled"
        );
        // Give disk every opportunity to have published at least once if
        // the retained-message reconciliation didn't actually take effect
        // before its first tick.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !disk_seen.load(Ordering::SeqCst),
            "a freshly connecting agent must come up with disk already disabled, proving the \
             retained config was applied from its very first subscribe — not observed live"
        );

        http_server.shutdown().await;
    }
}
