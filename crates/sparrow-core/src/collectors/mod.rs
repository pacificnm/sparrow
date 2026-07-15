mod cpu;
mod memory;

pub use cpu::CpuCollector;
pub use memory::MemoryCollector;

use crate::collector::{MetricItem, ValueType};

pub(crate) fn metric(key: &str, value_type: ValueType, value: String, now: i64) -> MetricItem {
    MetricItem {
        key: key.to_string(),
        value_type,
        value,
        tags: Default::default(),
        timestamp_ms: now,
    }
}
