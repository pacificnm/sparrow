//! Live-broker test for `ConfigReload`: publishes real config changes over a
//! disposable Mosquitto container and observes the running collector set
//! change via the real `Topics::data(host_id)` wire traffic — not by
//! inspecting `ConfigReload`'s private state (this file is a separate crate
//! from `sparrow-agent`'s lib, same as every `tests/*.rs` file, so it only
//! has the public API to work with, which is the point: this is a black-box
//! proof the way an actual server-side consumer would observe it).
//!
//! Requires Docker. Run with `cargo test -p sparrow-agent --test config_reload_live`.

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use nest_mqtt::{MqttClient, MqttConfig, MqttQos};
use nest_task::TaskManager;
use sparrow_agent::config::AgentConfig;
use sparrow_agent::config_reload::ConfigReload;
use sparrow_agent::publisher::Publisher;
use sparrow_agent::scheduler::BatchSink;
use sparrow_core::config::AgentConfigOverride;
use sparrow_core::transport::{DataBatch, Topics};

use support::{start_broker, test_agent_config, test_task_manager, wait_until};

#[tokio::test(flavor = "multi_thread")]
async fn config_reload_stops_and_restarts_collectors_from_a_live_broker() {
    let broker = start_broker().await;
    let agent_config = test_agent_config("config-reload-live-host", &broker, 1);

    let agent_client = MqttClient::connect(&MqttConfig::new(
        &agent_config.broker_host,
        agent_config.broker_port,
        &agent_config.host_id,
    ))
    .await
    .expect("agent client should connect");

    // Publish the baseline (nothing disabled) *before* ConfigReload starts —
    // retained, so its subscribe picks it up immediately regardless of
    // exactly when its background task starts polling.
    publish_config(
        &agent_client,
        &agent_config,
        &AgentConfigOverride::default(),
    )
    .await;

    let sink: Arc<dyn BatchSink> = Arc::new(Publisher::new(agent_client.clone(), &agent_config));
    let manager = test_task_manager();
    manager
        .spawn(ConfigReload::new(
            agent_client.clone(),
            &agent_config,
            Arc::clone(&sink),
            manager.clone(),
        ))
        .await
        .expect("config_reload should spawn");

    let observer = MqttClient::connect(&MqttConfig::new(
        &agent_config.broker_host,
        agent_config.broker_port,
        "config-reload-observer",
    ))
    .await
    .expect("observer client should connect");
    let stream = observer
        .subscribe(&Topics::data(&agent_config.host_id), MqttQos::AtLeastOnce)
        .await
        .expect("observer should subscribe");
    // subscribe() only enqueues the SUBSCRIBE packet and returns; give the
    // broker a moment to actually process it (same grace period nest-mqtt's
    // own tests use) before anything starts publishing.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cpu_seen = Arc::new(AtomicBool::new(false));
    let disk_seen = Arc::new(AtomicBool::new(false));
    {
        let cpu_seen = Arc::clone(&cpu_seen);
        let disk_seen = Arc::clone(&disk_seen);
        tokio::spawn(async move {
            // Pinned here, inside the spawned task's own frame — see
            // agent_integration.rs's identical note for why.
            tokio::pin!(stream);
            while let Some(message) = stream.next().await {
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
        "expected cpu and disk data within 10s of the baseline config"
    );

    // Disable disk. cpu should keep flowing; disk should stop.
    cpu_seen.store(false, Ordering::SeqCst);
    disk_seen.store(false, Ordering::SeqCst);
    publish_config(
        &agent_client,
        &agent_config,
        &AgentConfigOverride {
            disabled_collectors: vec!["disk".to_string()],
            collector_intervals: agent_config.collector_intervals.clone(),
        },
    )
    .await;

    assert!(
        wait_until(Duration::from_secs(5), Duration::from_millis(200), || {
            cpu_seen.load(Ordering::SeqCst)
        })
        .await,
        "cpu should keep publishing after an unrelated collector is disabled"
    );
    // Give any already-in-flight disk publish a chance to land before
    // asserting silence, so this isn't just measuring a race with disk's
    // own last tick before cancellation took effect.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !disk_seen.load(Ordering::SeqCst),
        "disk should have stopped publishing once disabled"
    );

    // Re-enable disk. It should resume.
    disk_seen.store(false, Ordering::SeqCst);
    publish_config(
        &agent_client,
        &agent_config,
        &AgentConfigOverride::default(),
    )
    .await;

    assert!(
        wait_until(Duration::from_secs(10), Duration::from_millis(200), || {
            disk_seen.load(Ordering::SeqCst)
        })
        .await,
        "disk should resume publishing once re-enabled"
    );
}

async fn publish_config(
    client: &MqttClient,
    config: &AgentConfig,
    override_: &AgentConfigOverride,
) {
    client
        .publish(
            &Topics::config(&config.host_id),
            override_.to_payload(),
            MqttQos::AtLeastOnce,
            true,
        )
        .await
        .expect("should publish config override");
}
