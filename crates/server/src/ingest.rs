//! MQTT subscriber loops that turn agent wire traffic into storage writes.
//!
//! Three independent long-running loops, one per topic filter
//! (`Topics::all_register()`, `Topics::all_heartbeat()`, `Topics::all_data()`)
//! — deliberately not one loop on a broader wildcard with manual topic-suffix
//! branching. Three simple loops are easier to get right and test
//! independently than one loop with a match statement, and the marginal MQTT
//! overhead of three subscriptions is irrelevant at this scale.
//!
//! Shared shape: subscribe once, then loop → parse → store → warn-and-continue
//! on error. A malformed payload from a misbehaving agent must never kill the
//! loop — every parse/store failure below is logged and the loop moves on to
//! the next message, it never propagates out of the `while let` body.

use futures_util::StreamExt;
use nest_error::NestResult;
use nest_mqtt::{MqttClient, MqttQos};
use sparrow_core::storage::{HostRegistry, MetricHistory};
use sparrow_core::transport::{DataBatch, HeartbeatMessage, RegisterMessage, Topics};

/// Subscribes to `Topics::all_register()` and upserts a host row for every
/// well-formed `RegisterMessage` received.
pub async fn run_register_ingest(mqtt: MqttClient, registry: HostRegistry) -> NestResult<()> {
    let stream = mqtt
        .subscribe(Topics::all_register(), MqttQos::AtLeastOnce)
        .await?;
    let mut stream = std::pin::pin!(stream);

    while let Some(msg) = StreamExt::next(&mut stream).await {
        match RegisterMessage::from_payload(&msg.payload) {
            Ok(register) => {
                if let Err(err) = registry
                    .upsert_on_register(&register.host_id, &register.hostname)
                    .await
                {
                    tracing::warn!(error = %err, host_id = %register.host_id, "failed to upsert host on register");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, topic = %msg.topic, "malformed register payload")
            }
        }
    }

    Ok(())
}

/// Subscribes to `Topics::all_heartbeat()` and records liveness for every
/// well-formed `HeartbeatMessage` received.
///
/// **Empty payload = the agent's LWT, not a malformed message.** The agent
/// (`crates/agent/src/main.rs`'s `build_mqtt_config`) configures its
/// Last-Will-and-Testament as an empty payload on this exact topic, published
/// by the broker on an unclean disconnect — there is no `host_id` to
/// deserialize out of an empty payload, so this must be checked for and
/// handled (via the topic string instead, through
/// `Topics::host_id_from_topic`) *before* attempting to parse it as a
/// `HeartbeatMessage`. Without this check, every unclean agent disconnect
/// would silently log as "malformed heartbeat payload" and the host would
/// never actually get marked offline — defeating the entire point of
/// configuring the LWT in the first place.
pub async fn run_heartbeat_ingest(mqtt: MqttClient, registry: HostRegistry) -> NestResult<()> {
    let stream = mqtt
        .subscribe(Topics::all_heartbeat(), MqttQos::AtLeastOnce)
        .await?;
    let mut stream = std::pin::pin!(stream);

    while let Some(msg) = StreamExt::next(&mut stream).await {
        if msg.payload.is_empty() {
            match Topics::host_id_from_topic(&msg.topic) {
                Some(host_id) => {
                    if let Err(err) = registry.mark_offline(host_id).await {
                        tracing::warn!(error = %err, host_id, "failed to mark host offline on LWT");
                    }
                }
                None => {
                    tracing::warn!(topic = %msg.topic, "heartbeat LWT arrived on an unparseable topic")
                }
            }
            continue;
        }

        match HeartbeatMessage::from_payload(&msg.payload) {
            Ok(heartbeat) => {
                if let Err(err) = registry.touch_heartbeat(&heartbeat.host_id).await {
                    tracing::warn!(error = %err, host_id = %heartbeat.host_id, "failed to record heartbeat");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, topic = %msg.topic, "malformed heartbeat payload")
            }
        }
    }

    Ok(())
}

/// Subscribes to `Topics::all_data()` and stores every well-formed
/// `DataBatch` received via a single multi-row insert
/// (`MetricHistory::insert_batch`) — not a per-item insert, which is the
/// entire reason that batch API exists (this is the high-frequency write
/// path).
pub async fn run_data_ingest(mqtt: MqttClient, history: MetricHistory) -> NestResult<()> {
    let stream = mqtt
        .subscribe(Topics::all_data(), MqttQos::AtLeastOnce)
        .await?;
    let mut stream = std::pin::pin!(stream);

    while let Some(msg) = StreamExt::next(&mut stream).await {
        match DataBatch::from_payload(&msg.payload) {
            Ok(batch) => {
                if let Err(err) = history.insert_batch(&batch.host_id, &batch).await {
                    tracing::warn!(error = %err, host_id = %batch.host_id, collector = %batch.collector, "failed to insert metric batch");
                }
            }
            Err(err) => tracing::warn!(error = %err, topic = %msg.topic, "malformed data payload"),
        }
    }

    Ok(())
}
