use sysinfo::Disks;

use crate::collector::{Collector, CollectorError, MetricItem, ValueType};

pub struct DiskCollector {
    disks: Disks,
}

impl DiskCollector {
    pub fn new() -> Self {
        Self {
            disks: Disks::new_with_refreshed_list(),
        }
    }
}

impl Default for DiskCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for DiskCollector {
    fn name(&self) -> &'static str {
        "disk"
    }

    fn default_interval_secs(&self) -> u64 {
        60
    }

    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
        self.disks.refresh(true);
        let now = crate::time::now_ms();
        let mut items = Vec::new();

        for disk in self.disks.iter() {
            let mount = disk.mount_point().to_string_lossy().to_string();
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            let percent = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            let mut tags = std::collections::BTreeMap::new();
            tags.insert("mount".to_string(), mount);

            items.push(super::metric(
                "disk.total_bytes",
                ValueType::Integer,
                total.to_string(),
                Some(tags.clone()),
                now,
            ));
            items.push(super::metric(
                "disk.used_bytes",
                ValueType::Integer,
                used.to_string(),
                Some(tags.clone()),
                now,
            ));
            items.push(super::metric(
                "disk.used_percent",
                ValueType::Float,
                format!("{:.2}", percent),
                Some(tags),
                now,
            ));
        }

        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_reports_at_least_one_disk() {
        let mut collector = DiskCollector::new();
        let items = collector.collect().expect("collect should succeed");

        assert!(!items.is_empty(), "should report at least one disk");

        let mounts: Vec<_> = items.iter().map(|i| i.tags.get("mount").unwrap()).collect();
        assert!(!mounts.is_empty(), "each disk metric must have a mount tag");
    }

    #[test]
    fn disk_metrics_are_consistent() {
        let mut collector = DiskCollector::new();
        let items = collector.collect().expect("collect should succeed");

        // Group items by mount point.
        let mut last_by_mount: std::collections::BTreeMap<String, Vec<&MetricItem>> =
            std::collections::BTreeMap::new();
        for item in &items {
            let mount = item.tags.get("mount").unwrap().clone();
            last_by_mount.entry(mount).or_default().push(item);
        }

        for (_mount, group) in last_by_mount {
            let total = group.iter().find(|i| i.key == "disk.total_bytes");
            let used = group.iter().find(|i| i.key == "disk.used_bytes");
            let percent = group.iter().find(|i| i.key == "disk.used_percent");

            assert!(total.is_some(), "missing disk.total_bytes for mount");
            assert!(used.is_some(), "missing disk.used_bytes for mount");
            assert!(percent.is_some(), "missing disk.used_percent for mount");

            let total_val = total
                .unwrap()
                .value
                .parse::<u64>()
                .expect("total should be u64");
            let used_val = used
                .unwrap()
                .value
                .parse::<u64>()
                .expect("used should be u64");
            let percent_val = percent
                .unwrap()
                .value
                .parse::<f64>()
                .expect("percent should be f64");

            assert!(
                used_val <= total_val,
                "used ({}) should not exceed total ({})",
                used_val,
                total_val
            );
            assert!(
                (0.0..=100.5).contains(&percent_val),
                "percent ({percent_val:.2}) should be roughly in [0, 100.5]"
            );
        }
    }
}
