//! Live Mosquitto + Postgres integration tests for `ingest.rs`'s three
//! subscriber loops. Publishes messages directly (bypassing a real agent,
//! for speed/focus) and asserts the expected row appears in Postgres.
//!
//! Requires Docker. Run with `cargo test -p sparrow-server --test ingest_live`.

mod support;

use std::time::Duration;

use nest_mqtt::{MqttClient, MqttConfig, MqttQos};
use sparrow_core::storage::{HostRegistry, MetricHistory};
use sparrow_core::transport::{DataBatch, HeartbeatMessage, RegisterMessage, Topics};
use sparrow_server::ingest::{run_data_ingest, run_heartbeat_ingest, run_register_ingest};

use support::{start_broker, start_postgres_with_schema, TestBroker};

/// How long to wait for a published message to be ingested and land in
/// Postgres before giving up.
const INGEST_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

async fn connect(broker: &TestBroker, client_id: &str) -> MqttClient {
    MqttClient::connect(&MqttConfig::new(&broker.host, broker.port, client_id))
        .await
        .expect("mqtt client should connect")
}

/// subscribe() only enqueues the SUBSCRIBE packet and returns; give the
/// broker a moment to actually process it before publishing, matching
/// `nest-mqtt`'s own tests' established grace period.
async fn subscribe_grace_period() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn register_ingest_upserts_a_host_row() {
    let broker = start_broker().await;
    let db = start_postgres_with_schema().await;
    let registry = HostRegistry::new(db.pool.clone());

    let ingest_client = connect(&broker, "register-ingest").await;
    tokio::spawn(run_register_ingest(ingest_client, registry.clone()));
    subscribe_grace_period().await;

    let publisher = connect(&broker, "register-publisher").await;
    let message = RegisterMessage {
        host_id: "ingest-register-host".to_string(),
        hostname: "sparrow-test-host".to_string(),
        agent_version: "0.1.0".to_string(),
    };
    publisher
        .publish(
            &Topics::register(&message.host_id),
            message.to_payload(),
            MqttQos::AtLeastOnce,
            true,
        )
        .await
        .expect("should publish register message");

    let host = wait_for_host(&registry, &message.host_id)
        .await
        .expect("registered host should appear in hosts");
    assert_eq!(host.hostname, "sparrow-test-host");
    assert!(host.online);
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_ingest_touches_last_seen() {
    let broker = start_broker().await;
    let db = start_postgres_with_schema().await;
    let registry = HostRegistry::new(db.pool.clone());
    let host_id = "ingest-heartbeat-host";
    registry
        .upsert_on_register(host_id, "sparrow-test-host")
        .await
        .expect("seed host");
    registry
        .mark_offline(host_id)
        .await
        .expect("start the host offline so touch_heartbeat's effect is observable");

    let ingest_client = connect(&broker, "heartbeat-ingest").await;
    tokio::spawn(run_heartbeat_ingest(ingest_client, registry.clone()));
    subscribe_grace_period().await;

    let publisher = connect(&broker, "heartbeat-publisher").await;
    let message = HeartbeatMessage {
        host_id: host_id.to_string(),
        timestamp_ms: sparrow_core::time::now_ms(),
    };
    publisher
        .publish(
            &Topics::heartbeat(host_id),
            message.to_payload(),
            MqttQos::AtLeastOnce,
            true,
        )
        .await
        .expect("should publish heartbeat message");

    let online = wait_for_online(&registry, host_id, true).await;
    assert!(online, "heartbeat should have flipped the host back online");
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_ingest_treats_an_empty_payload_as_the_agents_lwt() {
    let broker = start_broker().await;
    let db = start_postgres_with_schema().await;
    let registry = HostRegistry::new(db.pool.clone());
    let host_id = "ingest-heartbeat-lwt-host";
    registry
        .upsert_on_register(host_id, "sparrow-test-host")
        .await
        .expect("seed host (starts online)");

    let ingest_client = connect(&broker, "heartbeat-lwt-ingest").await;
    tokio::spawn(run_heartbeat_ingest(ingest_client, registry.clone()));
    subscribe_grace_period().await;

    // The agent's actual LWT payload is empty — see
    // crates/agent/src/main.rs's build_mqtt_config and ingest.rs's own doc
    // comment on run_heartbeat_ingest for why this needs special handling
    // instead of just failing to parse as JSON.
    let publisher = connect(&broker, "heartbeat-lwt-publisher").await;
    publisher
        .publish(
            &Topics::heartbeat(host_id),
            Vec::new(),
            MqttQos::AtLeastOnce,
            true,
        )
        .await
        .expect("should publish an empty LWT-shaped payload");

    let online = wait_for_online(&registry, host_id, false).await;
    assert!(
        !online,
        "an empty heartbeat payload should mark the host offline, not log a parse failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn data_ingest_inserts_a_metric_batch() {
    let broker = start_broker().await;
    let db = start_postgres_with_schema().await;
    let host_id = "ingest-data-host";
    HostRegistry::new(db.pool.clone())
        .upsert_on_register(host_id, "sparrow-test-host")
        .await
        .expect("host must exist before inserting metrics (FK constraint)");
    let history = MetricHistory::new(db.pool.clone());

    let ingest_client = connect(&broker, "data-ingest").await;
    tokio::spawn(run_data_ingest(ingest_client, history.clone()));
    subscribe_grace_period().await;

    let publisher = connect(&broker, "data-publisher").await;
    let batch = DataBatch {
        host_id: host_id.to_string(),
        collector: "cpu".to_string(),
        items: vec![sparrow_core::collector::MetricItem {
            key: "cpu.usage_percent".to_string(),
            value_type: sparrow_core::collector::ValueType::Float,
            value: "42.0".to_string(),
            tags: Default::default(),
            timestamp_ms: sparrow_core::time::now_ms(),
        }],
    };
    publisher
        .publish(
            &Topics::data(host_id),
            batch.to_payload(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .expect("should publish data batch");

    let items = wait_for_items(&history, host_id).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].key, "cpu.usage_percent");
    assert_eq!(items[0].value, "42.0");
}

#[tokio::test(flavor = "multi_thread")]
async fn data_ingest_survives_a_malformed_payload() {
    let broker = start_broker().await;
    let db = start_postgres_with_schema().await;
    let host_id = "ingest-malformed-host";
    HostRegistry::new(db.pool.clone())
        .upsert_on_register(host_id, "sparrow-test-host")
        .await
        .expect("host must exist before inserting metrics (FK constraint)");
    let history = MetricHistory::new(db.pool.clone());

    let ingest_client = connect(&broker, "malformed-ingest").await;
    tokio::spawn(run_data_ingest(ingest_client, history.clone()));
    subscribe_grace_period().await;

    let publisher = connect(&broker, "malformed-publisher").await;

    // A deliberately malformed payload — not valid JSON at all. The loop
    // must log and continue, never panic or stop consuming the topic.
    publisher
        .publish(
            &Topics::data(host_id),
            b"this is not json {{{".to_vec(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .expect("should publish malformed payload");

    // A well-formed batch published right after must still be processed —
    // this is the actual proof the loop survived, not just that publishing
    // garbage didn't immediately crash the test process.
    let batch = DataBatch {
        host_id: host_id.to_string(),
        collector: "memory".to_string(),
        items: vec![sparrow_core::collector::MetricItem {
            key: "memory.used_bytes".to_string(),
            value_type: sparrow_core::collector::ValueType::Integer,
            value: "123456".to_string(),
            tags: Default::default(),
            timestamp_ms: sparrow_core::time::now_ms(),
        }],
    };
    publisher
        .publish(
            &Topics::data(host_id),
            batch.to_payload(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .expect("should publish the well-formed batch after the malformed one");

    let items = wait_for_items(&history, host_id).await;
    assert_eq!(
        items.len(),
        1,
        "the well-formed batch after the malformed one should still be inserted"
    );
    assert_eq!(items[0].key, "memory.used_bytes");
}

/// Polls `registry.list()` until a row for `host_id` appears or
/// [`INGEST_TIMEOUT`] passes.
async fn wait_for_host(
    registry: &HostRegistry,
    host_id: &str,
) -> Option<sparrow_core::storage::HostRow> {
    let deadline = tokio::time::Instant::now() + INGEST_TIMEOUT;
    loop {
        let hosts = registry.list().await.expect("list should succeed");
        if let Some(host) = hosts.into_iter().find(|h| h.host_id == host_id) {
            return Some(host);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Polls `registry.list()` until `host_id`'s `online` flag matches
/// `expected` or [`INGEST_TIMEOUT`] passes, returning the last observed
/// value (or `false` if the host never even appeared).
async fn wait_for_online(registry: &HostRegistry, host_id: &str, expected: bool) -> bool {
    let deadline = tokio::time::Instant::now() + INGEST_TIMEOUT;
    loop {
        let hosts = registry.list().await.expect("list should succeed");
        if let Some(host) = hosts.iter().find(|h| h.host_id == host_id) {
            if host.online == expected || tokio::time::Instant::now() >= deadline {
                return host.online;
            }
        } else if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Polls `history.latest_items(host_id)` until at least one row appears or
/// [`INGEST_TIMEOUT`] passes.
async fn wait_for_items(
    history: &MetricHistory,
    host_id: &str,
) -> Vec<sparrow_core::storage::MetricHistoryRow> {
    let deadline = tokio::time::Instant::now() + INGEST_TIMEOUT;
    loop {
        let items = history.latest_items(host_id).await.expect("latest_items");
        if !items.is_empty() || tokio::time::Instant::now() >= deadline {
            return items;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
