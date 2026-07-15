mod cpu;
mod disk;
mod memory;

pub use cpu::CpuCollector;
pub use disk::DiskCollector;
pub use memory::MemoryCollector;

use std::collections::BTreeMap;

use crate::collector::{MetricItem, ValueType};

pub(crate) fn metric(
    key: &str,
    value_type: ValueType,
    value: String,
    tags: Option<BTreeMap<String, String>>,
    now: i64,
) -> MetricItem {
    MetricItem {
        key: key.to_string(),
        value_type,
        value,
        tags: tags.unwrap_or_default(),
        timestamp_ms: now,
    }
}
