use serde::{Deserialize, Serialize};

/// The type of a metric value — mirrors what the storage schema needs to
/// pick a column type; keep this small and closed, do not make it open-ended.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Float,
    Integer,
    Text,
}

/// One data point produced by a [`Collector`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricItem {
    /// Dotted key, e.g. `cpu.usage_percent`, `disk.used_bytes`.
    pub key: String,
    pub value_type: ValueType,
    /// Always populated regardless of `value_type` — store as text, cast on
    /// read/write at the storage boundary; keeps this struct simple and
    /// avoids a serde-untagged-enum footgun for a low-cost model to debug.
    pub value: String,
    /// Free-form tags (e.g. `{"mount": "/data"}` for a disk item, `{"core": "0"}`
    /// for a per-core CPU item) — optional, empty map if unused.
    #[serde(default)]
    pub tags: std::collections::BTreeMap<String, String>,
    /// Unix millis, set by the collector at read time.
    pub timestamp_ms: i64,
}

/// A modular metric producer. No knowledge of the broker, the agent's
/// scheduling, or storage — a `Collector` is pure: given a call to `collect`,
/// return the current readings.
///
/// Explicit design note: `collect` takes `&mut self`, not `&self` — this is
/// a deliberate deviation from the "pure function" framing above, made
/// necessary by collectors like `cpu` that need a persistent `sysinfo::System`
/// instance to compute usage deltas (Phase 5). Don't "fix" this to `&self`
/// later without re-reading Phase 5's note.
pub trait Collector: Send + Sync {
    /// Stable name, used as the topic segment and the `collector` tag in storage.
    /// Must be a valid MQTT topic segment: no `/`, `+`, `#`, or whitespace —
    /// validate this in `Collector::name`'s implementations, not centrally.
    fn name(&self) -> &'static str;

    /// Default interval when the agent's config doesn't override it.
    fn default_interval_secs(&self) -> u64;

    /// Reads current values. Implementations that need persistent state
    /// between calls (see Phase 5's `sysinfo` CPU-usage note) should hold
    /// that state as `&mut self` — this trait takes `&mut self` deliberately,
    /// not `&self`, for exactly that reason.
    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError>;
}

#[derive(Debug, thiserror::Error)]
#[error("collector `{collector}` failed: {message}")]
pub struct CollectorError {
    pub collector: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCollector {
        calls: u32,
    }

    impl Collector for FakeCollector {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn default_interval_secs(&self) -> u64 {
            30
        }

        fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
            self.calls += 1;
            Ok(vec![MetricItem {
                key: "fake.count".to_string(),
                value_type: ValueType::Integer,
                value: self.calls.to_string(),
                tags: std::collections::BTreeMap::from([("core".to_string(), "0".to_string())]),
                timestamp_ms: 1_700_000_000_000,
            }])
        }
    }

    #[test]
    fn fake_collector_round_trips_through_serde() {
        let mut collector = FakeCollector { calls: 0 };
        let items = collector.collect().expect("collect should succeed");

        let json = serde_json::to_string(&items).expect("serialize");
        let round_tripped: Vec<MetricItem> = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(round_tripped.len(), 1);
        assert_eq!(round_tripped[0].key, "fake.count");
        assert_eq!(round_tripped[0].value_type, ValueType::Integer);
        assert_eq!(round_tripped[0].value, "1");
        assert_eq!(round_tripped[0].timestamp_ms, 1_700_000_000_000);
        assert_eq!(round_tripped[0].tags.get("core"), Some(&"0".to_string()));
    }

    #[test]
    fn value_type_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&ValueType::Integer).unwrap(), "\"integer\"");
        assert_eq!(serde_json::to_string(&ValueType::Float).unwrap(), "\"float\"");
        assert_eq!(serde_json::to_string(&ValueType::Text).unwrap(), "\"text\"");
    }
}
