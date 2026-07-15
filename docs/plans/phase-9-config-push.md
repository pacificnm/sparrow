# Phase 9 Task Spec — Server → Agent Config Push

**Repo:** `pacificnm/sparrow`
**Crate:** `crates/server/src/api/agent_config.rs` (new endpoint) + reuses Phase 6's `crates/agent/src/config_reload.rs` (already built to *receive* this — this phase is mostly the server-side publish path and a persistence layer for "what config is each agent supposed to have")

## Design

Phase 6 already built the agent-side half of this (`config_reload.rs`
subscribes to `Topics::config(host_id)` and applies changes live). This
phase is the other half: a place to store each agent's desired config
server-side, and an API to change it.

### Storage (add to Phase 4's migrations)

```sql
CREATE TABLE agent_configs (
    host_id TEXT PRIMARY KEY REFERENCES hosts(host_id),
    disabled_collectors JSONB NOT NULL DEFAULT '[]',
    collector_intervals JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

One row per host, defaults to "everything enabled, default intervals" until
explicitly overridden — a host with no row in this table should behave
exactly as if it had a row with empty overrides, so the read path needs an
explicit default, not a missing-row error.

### API (`crates/server/src/api/agent_config.rs`)

```rust
pub fn routes() -> nest_http_serve::RouteGroup {
    nest_http_serve::RouteGroup::new("/api")
        .get("/hosts/:id/config", get_agent_config)
        .put("/hosts/:id/config", update_agent_config)
}

async fn update_agent_config(ctx: nest_http_serve::RequestContext) -> nest_http_serve::HttpResult {
    // 1. Parse the request body into sparrow_core::config::AgentConfigOverride
    //    (a new small type — do not reuse crates/agent's full AgentConfig here,
    //    since the server only ever sets disabled_collectors/collector_intervals,
    //    never host_id/broker_host, which are agent-local concerns).
    // 2. Upsert into agent_configs.
    // 3. Publish the resulting merged config as a RETAINED MQTT message on
    //    Topics::config(host_id) via the server's MqttClient — retained, QoS 1.
    // 4. Return the applied config as the response body.
    //
    // CHECK: confirm RequestContext's method for reading a PUT body and for
    // returning a typed JSON response — same "verify against context.rs/response.rs,
    // don't guess" instruction as Phase 7's api/ handlers.
    todo!()
}
```

`sparrow_core::config::AgentConfigOverride` (new type in `crates/core`, not
`crates/agent`, since both the server and the agent need it — the server to
construct/persist it, the agent to deserialize the retained message it
receives):

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentConfigOverride {
    #[serde(default)]
    pub disabled_collectors: Vec<String>,
    #[serde(default)]
    pub collector_intervals: std::collections::BTreeMap<String, u64>,
}
```

Phase 6's `config_reload.rs` should already be deserializing roughly this
shape from the retained message — **go back and confirm Phase 6's actual
implementation matches this type exactly** rather than each phase inventing
its own slightly-different shape independently; this is the one payload
that must agree byte-for-byte between agent and server.

### Retained-message semantics, worth stating explicitly

Because this is published with `retain: true`, an agent that connects (or
reconnects) *after* the server last published a config change still receives
it immediately on subscribe — this is precisely why Phase 2 called out MQTT
retained messages as "config-push almost for free." No polling, no
"agent asks server for its config on startup" round trip needed.

---

## Tests

- `agent_configs` upsert/default-read round trip (plain `testcontainers` Postgres test, no MQTT needed for this part).
- End-to-end (`testcontainers` Mosquitto + Postgres + a real Phase 6 agent): `PUT /api/hosts/{id}/config` disabling the `disk` collector, assert (a) the retained message lands on the topic, (b) a running agent picks it up and stops publishing `disk.*` items within one cancel-poll cycle, (c) a **freshly connecting** agent (started after the PUT) also comes up with `disk` disabled from the start — this last case is the one that actually proves retained-message semantics are working, not just live pub/sub.

**Acceptance:** `cargo test -p sparrow-server agent_config::` passes with Docker running. Manual/integration acceptance per the plan's original Phase 9 note: change an agent's collector interval from the server without touching the agent host, confirm live application.

## Explicit "do not" list

- Do not let `AgentConfigOverride` drift between what Phase 6's agent expects and what this phase's server publishes — reconcile against Phase 6's actual code, don't assume this spec's sketch is what got built.
- Do not skip the "freshly connecting agent" test case — it's the only one that actually exercises the retained-message behavior this whole design depends on.
- Do not treat a missing `agent_configs` row as an error — default to "everything enabled."
