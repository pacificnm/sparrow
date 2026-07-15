# Phase 5 Task Spec — Collectors (`cpu`, `memory`, `disk`)

**Repo:** `pacificnm/sparrow`
**Crate:** `crates/core/src/collectors/`
**Prerequisite:** Phase 4 (`Collector` trait, `MetricItem`).
**Pinned dependency:** `sysinfo = "0.39"` (confirmed current on crates.io as of this writing — check before pinning if time has passed).

## Ground truth — read before starting

`sysinfo`'s own docs are explicit about a correctness trap: **CPU usage is
computed as a diff between two refreshes on the same `System` instance.** A
fresh `System::new_all()` + single `refresh_cpu_usage()` call will not give a
meaningful percentage — the first reading needs a prior baseline. This is
exactly why `Collector::collect` takes `&mut self` (Phase 4's design note) —
the `cpu` collector must hold its `System` across calls, not recreate it
every tick.

Also from `sysinfo`'s docs: prefer `refresh_specifics(...)` over
`refresh_all()` for anything running on a tight interval — refreshing more
than needed wastes cycles on every tick, which matters when this runs every
few seconds on potentially many monitored hosts.

---

## `collectors/cpu.rs`

```rust
use sysinfo::System;
use crate::collector::{Collector, CollectorError, MetricItem, ValueType};

pub struct CpuCollector {
    sys: System,
}

impl CpuCollector {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_cpu_usage(); // baseline reading — first collect() call after this
                                  // will report a real (non-zero-by-construction) value
                                  // once at least one further refresh has elapsed.
        Self { sys }
    }
}

impl Default for CpuCollector {
    fn default() -> Self { Self::new() }
}

impl Collector for CpuCollector {
    fn name(&self) -> &'static str { "cpu" }
    fn default_interval_secs(&self) -> u64 { 10 }

    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
        self.sys.refresh_cpu_usage();
        let now = crate::time::now_ms(); // CHECK: confirm this helper exists in crates/core
            // (a small `pub fn now_ms() -> i64` wrapping `std::time::SystemTime`) — if Phase 4
            // didn't create one, add it to `crates/core/src/time.rs` as part of this task,
            // don't inline duplicate timestamp logic in every collector.

        let mut items = vec![MetricItem {
            key: "cpu.usage_percent".to_string(),
            value_type: ValueType::Float,
            value: format!("{:.2}", self.sys.global_cpu_usage()), // CHECK exact method
                // name against the pinned sysinfo version — API has changed across
                // 0.3x releases (was `global_cpu_info().cpu_usage()` in older versions).
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
```

**Explicit unresolved item:** `sysinfo` 0.39's exact method name for global
CPU usage (`global_cpu_usage()` above is a best guess based on recent-version
docs, not verified against the pinned patch version's actual API). Check
`cargo doc -p sysinfo` for the pinned version before shipping — if the method
name differs, fix it; don't leave a compile error and call it done.

## `collectors/memory.rs`

```rust
use sysinfo::System;
use crate::collector::{Collector, CollectorError, MetricItem, ValueType};

pub struct MemoryCollector {
    sys: System,
}

impl MemoryCollector {
    pub fn new() -> Self { Self { sys: System::new_all() } }
}

impl Default for MemoryCollector {
    fn default() -> Self { Self::new() }
}

impl Collector for MemoryCollector {
    fn name(&self) -> &'static str { "memory" }
    fn default_interval_secs(&self) -> u64 { 30 }

    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
        self.sys.refresh_memory();
        let now = crate::time::now_ms();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let percent = if total > 0 { (used as f64 / total as f64) * 100.0 } else { 0.0 };

        Ok(vec![
            metric("memory.total_bytes", ValueType::Integer, total.to_string(), now),
            metric("memory.used_bytes", ValueType::Integer, used.to_string(), now),
            metric("memory.used_percent", ValueType::Float, format!("{:.2}", percent), now),
        ])
    }
}

fn metric(key: &str, value_type: ValueType, value: String, now: i64) -> MetricItem {
    MetricItem { key: key.to_string(), value_type, value, tags: Default::default(), timestamp_ms: now }
}
```

(Reuse this `metric()` helper in `disk.rs` too — pull it into `collectors/mod.rs` as a shared `pub(crate) fn` rather than duplicating it three times.)

## `collectors/disk.rs`

```rust
use sysinfo::Disks;
use crate::collector::{Collector, CollectorError, MetricItem, ValueType};

pub struct DiskCollector {
    disks: Disks,
}

impl DiskCollector {
    pub fn new() -> Self { Self { disks: Disks::new_with_refreshed_list() } }
}

impl Default for DiskCollector {
    fn default() -> Self { Self::new() }
}

impl Collector for DiskCollector {
    fn name(&self) -> &'static str { "disk" }
    fn default_interval_secs(&self) -> u64 { 60 }

    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError> {
        self.disks.refresh(true); // CHECK: confirm `refresh`'s exact signature/bool meaning
            // for the pinned sysinfo version (refreshing the disk list vs. just usage stats
            // are different calls in some versions) — don't guess, check docs.rs.
        let now = crate::time::now_ms();
        let mut items = Vec::new();

        for disk in self.disks.iter() {
            let mount = disk.mount_point().to_string_lossy().to_string();
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            let percent = if total > 0 { (used as f64 / total as f64) * 100.0 } else { 0.0 };

            let mut tags = std::collections::BTreeMap::new();
            tags.insert("mount".to_string(), mount);

            for (key, value_type, value) in [
                ("disk.total_bytes", ValueType::Integer, total.to_string()),
                ("disk.used_bytes", ValueType::Integer, used.to_string()),
                ("disk.used_percent", ValueType::Float, format!("{:.2}", percent)),
            ] {
                items.push(MetricItem {
                    key: key.to_string(),
                    value_type,
                    value,
                    tags: tags.clone(),
                    timestamp_ms: now,
                });
            }
        }

        Ok(items)
    }
}
```

## Registry (`collectors/mod.rs`)

```rust
pub fn default_collectors() -> Vec<Box<dyn crate::collector::Collector>> {
    vec![
        Box::new(cpu::CpuCollector::new()),
        Box::new(memory::MemoryCollector::new()),
        Box::new(disk::DiskCollector::new()),
    ]
}
```

Explicit, no dynamic plugin discovery — matches the plan's Phase 5 scope note.

---

## Tests

Unit tests per collector, run on whatever CI host runs the tests (no
`testcontainers` needed here, no external services — `sysinfo` reads the
local machine):

- Each `collect()` call returns a non-empty `Vec<MetricItem>` with well-formed keys and parseable numeric `value` strings.
- `CpuCollector`: call `collect()` twice with a short sleep between (e.g. 100ms), assert both calls succeed and produce a `cpu.usage_percent` item both times — this is the test that would catch the "fresh System every call" bug if `&mut self` state accidentally got reset.
- `DiskCollector`: assert at least one disk is reported (every CI runner has a root filesystem) and `used_bytes + available <= total_bytes` roughly holds (allow slack for concurrent disk activity, don't assert exact equality).

**Acceptance:** `cargo test -p sparrow-core collectors::` passes on a plain CI runner, no Docker needed for this phase.

## Explicit "do not" list

- Do not recreate `System`/`Disks` inside `collect()` — defeats the whole point of the `&mut self` design from Phase 4.
- Do not guess `sysinfo` method names without checking `cargo doc` for the pinned version first — two are explicitly flagged above as unverified.
- Do not duplicate the `metric()` helper three times — share it via `collectors/mod.rs`.
