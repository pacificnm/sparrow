//! Issue 14.1 — the one end-to-end test exercising the whole system
//! together (every prior phase tested its own slice in isolation).
//!
//! Assembles, via `testcontainers`: a real Postgres (pgvector), a real
//! Mosquitto broker, `crates/server`'s own logic (ingest loops,
//! `offline_watch`, `alerting`, the full HTTP API) running in-process
//! against them, N **fake agents** (lightweight MQTT publishers — not real
//! `crates/agent` binaries, faster and more deterministic), and a **mocked**
//! `AiProvider` (scripted response, no network/model access).
//!
//! Per the issue's own instruction: a failure here is signal that two
//! phases' specs disagreed with each other, not just a bug to patch
//! locally — this is the test that exercises the actual interfaces between
//! phases (ingest → alerting → analyst → HTTP), not any one phase's own
//! mocked-out unit tests.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nest_ai::{AiProvider, AiResult, AiService, CompletionRequest, CompletionResponse};
use nest_core::AppBuilder;
use nest_data::DataModule;
use nest_data_postgres::{PostgresConfig, PostgresDataModule};
use nest_http_serve::HttpServer;
use nest_mqtt::{MqttClient, MqttConfig, MqttQos};
use nest_task_runtime::{TaskManager, TaskManagerConfig, TaskManagerService};
use sparrow_core::analyst::embedder::{Embedder, EMBEDDING_DIMENSION};
use sparrow_core::collector::{MetricItem, ValueType};
use sparrow_core::storage::{HostRegistry, MetricHistory};
use sparrow_core::transport::{DataBatch, HeartbeatMessage, RegisterMessage, Topics};
use sparrow_server::alerting::{AlertingTask, LogSink};
use sparrow_server::api;
use sparrow_server::ingest;
use sparrow_server::offline_watch::OfflineWatch;
use sqlx::PgPool;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const N_FAKE_AGENTS: usize = 3;

// --- Test infrastructure (each duplicated rather than imported, same
// reasoning every other crates/server test module gives: private to each
// module's own tests). ---

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

struct TestBroker {
    _container: ContainerAsync<GenericImage>,
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
        _container: container,
        host,
        port,
    }
}

/// Same shape as `crates/agent/tests/support/mod.rs`'s own helper: a
/// `TaskManagerService` with its `AppContext` already attached, so
/// `OfflineWatch`/`AlertingTask` (real `Task` implementors) have a working
/// manager to spawn onto.
fn test_task_manager() -> TaskManagerService {
    let app = AppBuilder::new()
        .build()
        .expect("empty app context")
        .context;
    let manager = TaskManagerService::new(
        tokio::runtime::Handle::current(),
        TaskManagerConfig { max_concurrent: 16 },
    );
    manager.set_context(app);
    manager
}

/// A scripted `AiProvider` — never calls a real model, never touches the
/// network. Always returns `SCRIPTED_RESPONSE` with no tool calls, so
/// `run_analysis` (`sparrow_core::analyst::loop.rs`) returns it immediately
/// on the first round, same as `analyst/loop.rs`'s own `FakeProvider`
/// (`tool_rounds: 0` case).
const SCRIPTED_RESPONSE: &str = "Disk usage on this host is critically high.";

struct ScriptedAiProvider {
    calls: AtomicUsize,
}

impl ScriptedAiProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl AiProvider for ScriptedAiProvider {
    fn provider_id(&self) -> &'static str {
        "scripted-test-double"
    }

    async fn complete(&self, _request: CompletionRequest) -> AiResult<CompletionResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            model: "scripted-test-double".to_string(),
            content: SCRIPTED_RESPONSE.to_string(),
            done: true,
            tool_calls: Vec::new(),
            metrics: None,
        })
    }
}

/// Never actually invoked: `ScriptedAiProvider` never requests a tool call,
/// so `execute_tool`'s `search_similar_incidents` branch (the only thing
/// that touches `Embedder`) is never reached — same reasoning as
/// `analyst/loop.rs`'s own `UnusedEmbedder`.
struct UnusedEmbedder;

#[async_trait::async_trait]
impl Embedder for UnusedEmbedder {
    async fn embed(&self, _text: &str) -> nest_error::NestResult<Vec<f32>> {
        Ok(vec![0.0; EMBEDDING_DIMENSION])
    }
}

/// A lightweight test double standing in for a real `crates/agent`
/// process — connects its own `MqttClient` and publishes synthetic wire
/// payloads directly, per the issue's own instruction ("not real
/// `crates/agent` binaries").
struct FakeAgent {
    client: MqttClient,
    host_id: String,
}

impl FakeAgent {
    async fn connect(broker: &TestBroker, host_id: &str) -> Self {
        let config = MqttConfig::new(&broker.host, broker.port, host_id);
        let client = MqttClient::connect(&config)
            .await
            .expect("fake agent client construction does not require a live connection");
        Self {
            client,
            host_id: host_id.to_string(),
        }
    }

    async fn register(&self) {
        let message = RegisterMessage {
            host_id: self.host_id.clone(),
            hostname: format!("{}-hostname", self.host_id),
            agent_version: "0.0.0-fake".to_string(),
        };
        self.client
            .publish(
                &Topics::register(&self.host_id),
                message.to_payload(),
                MqttQos::AtLeastOnce,
                true,
            )
            .await
            .expect("fake agent register publish");
    }

    async fn heartbeat(&self) {
        let message = HeartbeatMessage {
            host_id: self.host_id.clone(),
            timestamp_ms: sparrow_core::time::now_ms(),
        };
        self.client
            .publish(
                &Topics::heartbeat(&self.host_id),
                message.to_payload(),
                MqttQos::AtLeastOnce,
                true,
            )
            .await
            .expect("fake agent heartbeat publish");
    }

    async fn publish_data(&self, key: &str, value: &str) {
        let batch = DataBatch {
            host_id: self.host_id.clone(),
            collector: "fake".to_string(),
            items: vec![MetricItem {
                key: key.to_string(),
                value_type: ValueType::Float,
                value: value.to_string(),
                tags: BTreeMap::new(),
                timestamp_ms: sparrow_core::time::now_ms(),
            }],
        };
        self.client
            .publish(
                &Topics::data(&self.host_id),
                batch.to_payload(),
                MqttQos::AtLeastOnce,
                false,
            )
            .await
            .expect("fake agent data publish");
    }
}

/// Polls an async `predicate` until it returns `true` or `timeout` elapses.
/// Same idea as `crates/agent/tests/support/mod.rs`'s own `wait_until`, but
/// async since every predicate here is an HTTP call through the real
/// `reqwest::Client` (already running inside this test's tokio runtime —
/// `reqwest::blocking` cannot be called from within it).
async fn wait_until<F, Fut>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Polling helper - returns an empty list on any transient failure (the
/// server not accepting connections yet, a non-2xx status) rather than
/// panicking, since this is only ever used inside `wait_until`'s retry
/// loop.
async fn get_json(http: &reqwest::Client, url: String) -> Vec<serde_json::Value> {
    let Ok(response) = http.get(url).send().await else {
        return Vec::new();
    };
    let Ok(response) = response.error_for_status() else {
        return Vec::new();
    };
    response.json().await.unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn full_stack_end_to_end() {
    let db = start_postgres_with_schema().await;
    let broker = start_broker().await;

    // --- Assemble "crates/server" itself, in-process (same components
    // crates/server/src/main.rs wires together in production, substituting
    // test doubles only where the issue calls for them: fake agents, a
    // mocked AiProvider). ---
    let server_mqtt = MqttClient::connect(&MqttConfig::new(
        &broker.host,
        broker.port,
        "sparrow-server",
    ))
    .await
    .expect("server mqtt client construction does not require a live connection");

    let registry = HostRegistry::new(db.pool.clone());
    let history = MetricHistory::new(db.pool.clone());

    tokio::spawn(ingest::run_register_ingest(
        server_mqtt.clone(),
        registry.clone(),
    ));
    tokio::spawn(ingest::run_heartbeat_ingest(
        server_mqtt.clone(),
        registry.clone(),
    ));
    tokio::spawn(ingest::run_data_ingest(
        server_mqtt.clone(),
        history.clone(),
    ));

    let manager = test_task_manager();
    // Small, test-only cadences (per the issue's own instruction for step
    // 5) - not the real 30s/45s production defaults.
    manager
        .spawn(OfflineWatch::new(
            registry.clone(),
            Duration::from_millis(300),
            1, // stale_after_secs - smallest unit OfflineWatch's SQL supports
        ))
        .await
        .expect("spawn offline_watch");
    manager
        .spawn(AlertingTask::new(
            db.pool.clone(),
            Duration::from_millis(300),
            vec![Arc::new(LogSink)],
            Arc::new(UnusedEmbedder),
        ))
        .await
        .expect("spawn alerting");

    let ai_service = AiService::new(Arc::new(ScriptedAiProvider::new()));
    let embedder: Arc<dyn Embedder> = Arc::new(UnusedEmbedder);

    let test_server = HttpServer::builder()
        .bind("127.0.0.1:0")
        .routes(api::hosts::routes(registry.clone(), history.clone()))
        .routes(api::history::routes(history.clone()))
        .routes(api::problems::routes(db.pool.clone()))
        .routes(api::agent_config::routes(
            db.pool.clone(),
            server_mqtt.clone(),
        ))
        .routes(api::analyst::routes(ai_service, db.pool.clone(), embedder))
        .spawn()
        .await
        .expect("http server should spawn");

    let http = reqwest::Client::new();
    let base_url = test_server.base_url();

    // --- Step 1: N fake agents register -> GET /api/hosts shows all N. ---
    let host_ids: Vec<String> = (0..N_FAKE_AGENTS)
        .map(|i| format!("fake-host-{i}"))
        .collect();
    let mut agents = Vec::new();
    for host_id in &host_ids {
        let agent = FakeAgent::connect(&broker, host_id).await;
        agent.register().await;
        agent.heartbeat().await;
        agents.push(agent);
    }

    let registered = wait_until(Duration::from_secs(10), || async {
        let hosts = get_json(&http, format!("{base_url}/api/hosts")).await;
        hosts.len() >= N_FAKE_AGENTS
    })
    .await;
    assert!(
        registered,
        "all {N_FAKE_AGENTS} fake agents should have registered"
    );
    let hosts_response = http
        .get(format!("{base_url}/api/hosts"))
        .send()
        .await
        .expect("GET /api/hosts");
    let hosts: Vec<serde_json::Value> = hosts_response.json().await.expect("valid JSON");
    for host_id in &host_ids {
        assert!(
            hosts.iter().any(|h| h["host_id"] == *host_id),
            "GET /api/hosts should include {host_id}"
        );
    }

    // --- Step 2: fake agents publish data -> GET /api/hosts/{id}/items
    // reflects it. The first agent gets a normal value; the second gets a
    // high one that a rule (seeded next) will trip on. ---
    agents[0].publish_data("cpu.usage_percent", "12.5").await;
    let tripping_host_id = host_ids[1].clone();
    agents[1].publish_data("cpu.usage_percent", "95.0").await;

    let data_arrived = wait_until(Duration::from_secs(10), || async {
        let items = get_json(&http, format!("{base_url}/api/hosts/{}/items", host_ids[0])).await;
        items
            .iter()
            .any(|i| i["key"] == "cpu.usage_percent" && i["value"] == "12.5")
    })
    .await;
    assert!(
        data_arrived,
        "published data should reach GET /api/hosts/{{id}}/items"
    );

    // --- Step 3: a seeded rule trips on one fake agent's data -> a Problem
    // opens (Phase 8). Sustained_for_secs = 0 so it opens on the very next
    // evaluation pass, not after a sustain window. ---
    sqlx::query(
        "INSERT INTO rules (host_id, item_key, operator, threshold, severity, sustained_for_secs)
         VALUES ($1, 'cpu.usage_percent', 'greater_than', 90.0, 'critical', 0)",
    )
    .bind(&tripping_host_id)
    .execute(&db.pool)
    .await
    .expect("seed rule");

    let problem_opened = wait_until(Duration::from_secs(10), || async {
        let problems = get_json(
            &http,
            format!("{base_url}/api/problems?host_id={tripping_host_id}"),
        )
        .await;
        !problems.is_empty()
    })
    .await;
    assert!(
        problem_opened,
        "a rule tripping on {tripping_host_id}'s data should open a Problem"
    );

    // --- Step 4: the mocked AI provider, asked to explain that Problem,
    // returns the scripted response through POST /api/analyst/run. ---
    let analyst_response = http
        .post(format!("{base_url}/api/analyst/run"))
        .json(&serde_json::json!({ "host_id": tripping_host_id, "question": null }))
        .send()
        .await
        .expect("POST /api/analyst/run");
    assert!(analyst_response.status().is_success());
    let analyst_body: serde_json::Value = analyst_response.json().await.expect("valid JSON");
    assert_eq!(
        analyst_body["response"], SCRIPTED_RESPONSE,
        "the analyst endpoint should surface the mocked AiProvider's scripted response verbatim"
    );

    // --- Step 5: one fake agent stops heartbeating -> offline_watch
    // eventually marks it offline. OfflineWatch above was configured with
    // a 300ms sweep / 1s stale threshold, not the real 30s/45s production
    // defaults, so this doesn't need to wait out a real interval. ---
    let stale_host_id = host_ids[2].clone();
    drop(agents.pop().expect("third fake agent")); // stops its client/heartbeats

    let marked_offline = wait_until(Duration::from_secs(10), || async {
        let hosts = get_json(&http, format!("{base_url}/api/hosts")).await;
        hosts
            .iter()
            .any(|h| h["host_id"] == stale_host_id && h["online"] == false)
    })
    .await;
    assert!(
        marked_offline,
        "offline_watch should mark {stale_host_id} offline once its heartbeat goes stale"
    );
}
