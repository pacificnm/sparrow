# Authoring a new collector

A guide for adding a fourth collector to Sparrow (today there are three:
`cpu`, `memory`, `disk`) without needing to read the project's phase-spec
series. Everything below is checked directly against the real source in
`crates/sparrow-core/src/collector.rs` and `crates/sparrow-core/src/collectors/`.

## The `Collector` trait

```rust
pub trait Collector: Send + Sync {
    /// Stable name, used as the topic segment and the `collector` tag in
    /// storage. Must be a valid MQTT topic segment: no `/`, `+`, `#`, or
    /// whitespace.
    fn name(&self) -> &'static str;

    /// Default interval when the agent's config doesn't override it.
    fn default_interval_secs(&self) -> u64;

    /// Reads current values.
    fn collect(&mut self) -> Result<Vec<MetricItem>, CollectorError>;
}
```

Three methods, no more. `collect` returns a `Vec<MetricItem>`, not a single
value — one call can (and often does) produce several data points, e.g.
`CpuCollector` returns one `cpu.usage_percent` item plus one
`cpu.core_usage_percent` item per core.

### Why `&mut self`, not `&self`

This is a deliberate deviation from "a collector is a pure function of the
current system state." Some collectors need to remember something between
calls to compute a meaningful value at all — `CpuCollector` holds a
`sysinfo::System` and calls `refresh_cpu_usage()` on it every `collect()`;
CPU usage is inherently a *delta* between two readings, not a value you can
read once. `CpuCollector::new()` even takes a baseline reading up front for
exactly this reason (the very first `collect()` call needs something to
diff against). If your metric is an instantaneous, no-history-needed
snapshot (e.g. "is this file present"), you don't need any persistent
state — just don't fight the signature: it's still `&mut self`, you simply
won't mutate anything.

## `MetricItem` — what one data point looks like

```rust
pub struct MetricItem {
    pub key: String,             // dotted, e.g. "cpu.usage_percent"
    pub value_type: ValueType,   // Float | Integer | Text
    pub value: String,           // always a string, regardless of value_type
    pub tags: BTreeMap<String, String>, // e.g. {"mount": "/data"}, {"core": "0"}
    pub timestamp_ms: i64,
}
```

`value` is always a `String` even for `Float`/`Integer` types — this is
deliberate, not an oversight: it keeps the wire format simple and avoids a
serde-untagged-enum footgun. Format numbers yourself (e.g.
`format!("{:.2}", percent)`) before putting them in `value`.

`tags` is how you distinguish multiple readings under the same `key` in one
`collect()` call — `disk` tags each item with `{"mount": "..."}` since it
reports one `disk.used_bytes` per mounted filesystem; `cpu` tags per-core
items with `{"core": "0"}`, `{"core": "1"}`, etc. If your collector only
ever produces one reading per key per call, you don't need tags at all
(pass `None`/an empty map).

### The `metric()` helper

`collectors/mod.rs` has a small `pub(crate)` helper:

```rust
pub(crate) fn metric(
    key: &str,
    value_type: ValueType,
    value: String,
    tags: Option<BTreeMap<String, String>>,
    now: i64,
) -> MetricItem
```

`memory.rs` and `disk.rs` both use it (`super::metric("memory.total_bytes",
ValueType::Integer, total.to_string(), None, now)`) to avoid repeating
`timestamp_ms: now, tags: tags.unwrap_or_default()` on every item. It's a
convenience, not a requirement — `cpu.rs` builds `MetricItem` struct
literals directly instead (its per-core loop already needs to build a fresh
`tags` map per iteration, so the helper doesn't save much there). Use
whichever reads more clearly for your collector. Being `pub(crate)`, it's
only callable from within `sparrow_core` itself — which is fine, since a
new collector's implementation lives in this crate too (see below).

## Registering a new collector

One place: `crates/sparrow-core/src/collectors/mod.rs`.

```rust
mod cpu;
mod disk;
mod memory;
mod gpu;                          // 1. add your module

pub use cpu::CpuCollector;
pub use disk::DiskCollector;
pub use memory::MemoryCollector;
pub use gpu::GpuCollector;        // 2. re-export the type

pub fn default_collectors() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(CpuCollector::new()),
        Box::new(MemoryCollector::new()),
        Box::new(DiskCollector::new()),
        Box::new(GpuCollector::new()), // 3. add it to the default set
    ]
}
```

That's the entire registration surface — no dynamic plugin discovery, no
separate config-file declaration needed just to make a collector exist.
`default_collectors()`'s own doc comment says as much: "No dynamic plugin
discovery — explicit construction."

### Enabling, disabling, and interval overrides

Once a collector is in `default_collectors()`, an operator can disable it
or override its interval **without a rebuild**, via `AgentConfig`'s
`[agent]` TOML section:

```toml
[agent]
host_id = "web-01"
broker_host = "mosquitto.internal"
broker_port = 8883
disabled_collectors = ["gpu"]        # matched against Collector::name()

[agent.collector_intervals]
gpu = 15                              # seconds; falls back to
                                       # default_interval_secs() if absent
```

`crates/agent/src/config_reload.rs` is where this is actually applied: it
walks `default_collectors()`, skips any whose `name()` appears in
`disabled_collectors`, and looks up `collector_intervals.get(name())`
(falling back to `default_interval_secs()`) for everything else. There's no
separate "is this collector known" registration step beyond
`default_collectors()` itself — a typo in `disabled_collectors` (a name
that doesn't match any real collector) is silently a no-op, not an error,
since the config only ever *removes from* or *overrides intervals for* the
compiled-in set.

## Adding a new metric `key` — what you do and don't need to do

Say your new `GpuCollector` publishes `gpu.utilization_percent`. What
happens automatically, and what's optional:

**Automatic, no action needed:**

- **Storage**: `metric_history`'s `key` column is plain `TEXT`, not an enum
  — no migration, no schema change, for any new key.
- **The `GET /api/hosts/{id}/items` and `GET /api/hosts/{id}/items/{key}/history`
  endpoints**: both read whatever's in storage generically; a new key
  shows up automatically once your collector publishes it.
- **The desktop UI's `HostDetail` panel**: groups items by `collector` and
  renders `key`/`value` pairs generically — it has no per-key allowlist to
  update.

**Optional — you decide, nothing here is silently expected:**

- **A default trigger rule.** Rules (the `rules` table, Phase 8) are
  independent, opt-in rows — there is no dedicated `/api/rules` endpoint
  today, so the only way to create one is a direct `INSERT INTO rules
  (host_id, item_key, operator, threshold, severity, sustained_for_secs)
  VALUES (...)`. Nothing about adding a collector creates a rule for you,
  and nothing requires one to exist. If `gpu.utilization_percent` sitting
  there with no rule is exactly what you want (just visible in the
  dashboard, not alerted on), that's a completely valid, unremarkable end
  state.
- **A dedicated dashboard panel/visualization.** The generic `HostDetail`
  rendering (grouped by collector, plain key/value rows) already shows any
  new metric with zero code changes. A bespoke chart or panel for it is
  something you'd build only if the generic rendering isn't good enough
  for that particular metric — not a prerequisite for the metric to be
  useful or visible.

## Testing

Follow the existing collectors' own test convention (plain `#[test]`, no
external test framework): construct the collector, call `collect()`, assert
on the returned `Vec<MetricItem>`. See `cpu.rs`'s
`collect_reports_usage_percent_across_calls` (asserts a delta-dependent key
is present across two calls) and `memory.rs`'s
`collect_returns_well_formed_metrics` (asserts every expected key is
present and its `value` parses as the right kind of number) for the two
established shapes to copy from.
