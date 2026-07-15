mod cpu;
mod disk;
mod memory;

pub use cpu::CpuCollector;
pub use disk::DiskCollector;
pub use memory::MemoryCollector;

use std::collections::BTreeMap;

use crate::collector::{MetricItem, ValueType};

use crate::collector::Collector;

/// Return the default set of collectors (CPU, memory, disk).
///
/// No dynamic plugin discovery — explicit construction matching Phase 5 scope.
pub fn default_collectors() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(CpuCollector::new()),
        Box::new(MemoryCollector::new()),
        Box::new(DiskCollector::new()),
    ]
}

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
