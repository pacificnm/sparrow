//! Full-integration tests for `crates/agent`: real `CollectorTask`s, a real
//! `Publisher`, and a real (disposable) Mosquitto broker.
//!
//! Requires Docker. Run with `cargo test -p sparrow-agent --test agent_integration`.

mod support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use nest_mqtt::{MqttClient, MqttConfig, MqttQos};
use nest_task::TaskManager;
use sparrow_agent::config::AgentConfig;
use sparrow_agent::publisher::Publisher;
use sparrow_agent::scheduler::{BatchSink, CollectorTask};
use sparrow_core::collectors::default_collectors;
use sparrow_core::transport::{DataBatch, Topics};

use support::{
    start_broker, start_broker_with_fixed_port, test_agent_config, test_task_manager, wait_until,
};

/// Spawns one real `CollectorTask` per `default_collectors()` entry,
/// publishing through a real `Publisher` connected to `agent_config`'s
/// broker.
async fn spawn_real_collectors(agent_config: &AgentConfig, client: MqttClient) {
    let sink: Arc<dyn BatchSink> = Arc::new(Publisher::new(client, agent_config));
    let manager = test_task_manager();

    for collector in default_collectors() {
        let name = collector.name();
        let interval = agent_config
            .collector_intervals
            .get(name)
            .copied()
            .unwrap_or_else(|| collector.default_interval_secs());
        manager
            .spawn(CollectorTask::new(
                collector,
                Duration::from_secs(interval),
                Arc::clone(&sink),
            ))
            .await
            .expect("collector task should spawn");
    }

    // Spawned tasks run independently on the runtime once `spawn()` returns
    // — dropping `manager` here doesn't stop them (there's nothing else to
    // do with it in this test; `#[tokio::test]` tears down the whole runtime
    // at the end regardless).
}

/// Connects a fresh observer client, subscribes to `Topics::data(host_id)`,
/// and asserts data from all three collectors arrives within `timeout`.
async fn assert_collector_data_flows(
    agent_config: &AgentConfig,
    observer_client_id: &str,
    timeout: Duration,
) {
    let observer = MqttClient::connect(&MqttConfig::new(
        &agent_config.broker_host,
        agent_config.broker_port,
        observer_client_id,
    ))
    .await
    .expect("observer client should connect");
    let stream = observer
        .subscribe(&Topics::data(&agent_config.host_id), MqttQos::AtLeastOnce)
        .await
        .expect("observer should subscribe");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let seen = Arc::new(Mutex::new(HashSet::new()));
    {
        let seen = Arc::clone(&seen);
        tokio::spawn(async move {
            // Pinned here, inside the spawned task's own frame — `stream`
            // (moved in whole) isn't `Unpin`, and pinning it before the move
            // would tie the pin to this function's (non-'static) stack frame.
            tokio::pin!(stream);
            while let Some(message) = stream.next().await {
                if let Ok(batch) = DataBatch::from_payload(&message.payload) {
                    seen.lock().unwrap().insert(batch.collector);
                }
            }
        });
    }

    let ok = wait_until(timeout, Duration::from_millis(200), || {
        seen.lock().unwrap().len() >= 3
    })
    .await;

    let names = seen.lock().unwrap().clone();
    assert!(
        ok,
        "expected cpu, memory, and disk data via {observer_client_id} within {timeout:?}, only saw {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn real_collectors_publish_data_within_one_interval() {
    let broker = start_broker().await;
    let agent_config = test_agent_config("integration-basic-host", &broker, 1);

    let client = MqttClient::connect(&MqttConfig::new(
        &agent_config.broker_host,
        agent_config.broker_port,
        &agent_config.host_id,
    ))
    .await
    .expect("agent client should connect");

    spawn_real_collectors(&agent_config, client).await;

    // 1s interval; a 5s window is generous slack for a timing test, matching
    // scheduler.rs's own "roughly once per interval" test's spirit.
    assert_collector_data_flows(&agent_config, "basic-observer", Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_resumes_publishing_after_broker_restart() {
    // Needs a fixed (not Docker-assigned-random) host port: this test
    // restarts the same container mid-run and depends on its address
    // staying reachable afterward — see start_broker_with_fixed_port's doc
    // comment for why start_broker's dynamic port can't be used here.
    let broker = start_broker_with_fixed_port().await;
    let agent_config = test_agent_config("integration-restart-host", &broker, 1);

    let client = MqttClient::connect(&MqttConfig::new(
        &agent_config.broker_host,
        agent_config.broker_port,
        &agent_config.host_id,
    ))
    .await
    .expect("agent client should connect");

    spawn_real_collectors(&agent_config, client).await;

    // Phase 1: confirm data flows before any disruption.
    assert_collector_data_flows(
        &agent_config,
        "pre-restart-observer",
        Duration::from_secs(5),
    )
    .await;

    // Kill the broker mid-run. The already-running CollectorTasks/Publisher
    // must not panic — nest-mqtt's MqttClient reconnects at the transport
    // level (confirmed in its own run_event_loop: it logs and keeps polling
    // on error rather than giving up), and Publisher buffers failed batches
    // rather than dropping them, so this is expected to degrade gracefully,
    // not crash.
    broker
        .container
        .stop()
        .await
        .expect("failed to stop broker container");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Same container, same port mapping — restart, don't recreate.
    broker
        .container
        .start()
        .await
        .expect("failed to restart broker container");
    let restarted_port = broker
        .container
        .get_host_port_ipv4(1883)
        .await
        .expect("container port after restart");
    assert_eq!(
        restarted_port, broker.port,
        "restarting (not recreating) the container should keep the same port mapping"
    );
    // Give Mosquitto a moment to finish starting and nest-mqtt's background
    // reconnect loop a moment to notice the broker is back.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Phase 2: a *fresh* observer, deliberately — whether an
    // already-subscribed client's own subscription survives the broker's
    // session reset is a separate question this test isn't answering. What
    // matters here is whether the still-running agent-side tasks resume
    // publishing, which a new observer proves independently of that.
    assert_collector_data_flows(
        &agent_config,
        "post-restart-observer",
        Duration::from_secs(10),
    )
    .await;
}
