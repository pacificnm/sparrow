use sysinfo::System;

use crate::collector::{Collector, CollectorError, MetricItem, ValueType};

pub struct MemoryCollector {
    sys: System,
}

impl MemoryCollector {
    pub fn new() -> Self {
        Self {
            sys: System::new_all(),
        }
    }
}

impl Default for MemoryCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for MemoryCollector {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn default_interval_secs(&self) -> u64 {
        30
    }

    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
        self.sys.refresh_memory();
        let now = crate::time::now_ms();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let percent = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        Ok(vec![
            super::metric(
                "memory.total_bytes",
                ValueType::Integer,
                total.to_string(),
                None,
                now,
            ),
            super::metric(
                "memory.used_bytes",
                ValueType::Integer,
                used.to_string(),
                None,
                now,
            ),
            super::metric(
                "memory.used_percent",
                ValueType::Float,
                format!("{:.2}", percent),
                None,
                now,
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_returns_well_formed_metrics() {
        let mut collector = MemoryCollector::new();
        let items = collector.collect().expect("collect should succeed");

        assert!(!items.is_empty());
        for expected_key in [
            "memory.total_bytes",
            "memory.used_bytes",
            "memory.used_percent",
        ] {
            let item = items
                .iter()
                .find(|item| item.key == expected_key)
                .unwrap_or_else(|| panic!("missing `{expected_key}` metric"));
            assert!(
                item.value.parse::<f64>().is_ok(),
                "value `{}` for `{}` should parse as a number",
                item.value,
                item.key
            );
        }
    }
}
