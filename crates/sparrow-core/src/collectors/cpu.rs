use sysinfo::System;

use crate::collector::{Collector, CollectorError, MetricItem, ValueType};

pub struct CpuCollector {
    sys: System,
}

impl CpuCollector {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        // Baseline reading — CPU usage is a diff between refreshes, so the
        // first collect() after this establishes the starting point.
        sys.refresh_cpu_usage();
        Self { sys }
    }
}

impl Default for CpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for CpuCollector {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn default_interval_secs(&self) -> u64 {
        10
    }

    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
        self.sys.refresh_cpu_usage();
        let now = crate::time::now_ms();

        let mut items = vec![MetricItem {
            key: "cpu.usage_percent".to_string(),
            value_type: ValueType::Float,
            value: format!("{:.2}", self.sys.global_cpu_usage()),
            tags: Default::default(),
            timestamp_ms: now,
        }];

        for (i, cpu) in self.sys.cpus().iter().enumerate() {
            let mut tags = std::collections::BTreeMap::new();
            tags.insert("core".to_string(), i.to_string());
            items.push(MetricItem {
                key: "cpu.core_usage_percent".to_string(),
                value_type: ValueType::Float,
                value: format!("{:.2}", cpu.cpu_usage()),
                tags,
                timestamp_ms: now,
            });
        }

        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_reports_usage_percent_across_calls() {
        let mut collector = CpuCollector::new();

        let first = collector.collect().expect("first collect should succeed");
        assert!(first.iter().any(|item| item.key == "cpu.usage_percent"));

        std::thread::sleep(std::time::Duration::from_millis(100));

        let second = collector.collect().expect("second collect should succeed");
        assert!(second.iter().any(|item| item.key == "cpu.usage_percent"));

        for item in first.iter().chain(second.iter()) {
            if item.key == "cpu.usage_percent" || item.key == "cpu.core_usage_percent" {
                assert!(
                    item.value.parse::<f64>().is_ok(),
                    "value `{}` for `{}` should parse as f64",
                    item.value,
                    item.key
                );
            }
        }
    }
}
