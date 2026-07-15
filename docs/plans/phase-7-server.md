# Phase 7 Task Spec — Server (`crates/server`, `nest-http-serve`)

**Repo:** `pacificnm/sparrow`
**Crate:** `crates/server`
**Prerequisite:** Phase 4 (core contracts/storage), Phase 6 (agent — need a real agent to test ingest against).

## Ground truth — confirmed against real framework source

- `nest_http_serve`'s routing (`router.rs`): `RouteGroup::new(prefix)`, `.get(path, handler)`/`.post(path, handler)` etc., handlers take a `RequestContext` and return `HttpResult`. Routes get collected into a `RouteRegistry` via `.add_group(group)`.
- `nest_data_postgres::PostgresConnection::pool() -> &PgPool` — the server needs this directly for `sparrow_core::storage`'s `HostRegistry`/`MetricHistory`.
- `nest_mqtt::MqttClient::subscribe(topic_filter, qos) -> Stream<Item = MqttMessage>` — the server's ingest pipeline is one long-running subscriber loop per topic pattern (or one loop handling all three via `sparrow/agents/+/#`, filtering by topic suffix — decide which during implementation, see design note below).

---

## Design

```
crates/server/
├── Cargo.toml
└── src/
    ├── main.rs        # nest_http_serve host bootstrap
    ├── ingest.rs        # MQTT subscriber loop(s): register/heartbeat/data -> storage
    ├── offline_watch.rs  # marks hosts offline when heartbeats stop arriving
    └── api/
        ├── mod.rs
        ├── hosts.rs      # GET /hosts, GET /hosts/{id}/items
        └── history.rs     # GET /hosts/{id}/items/{key}/history
```

### `ingest.rs`

**Design decision, make it explicitly, don't default into it accidentally:**
subscribe to three separate topic filters (`Topics::all_register()`,
`Topics::all_heartbeat()`, `Topics::all_data()`) as three independent
long-running tasks, rather than one subscription to a broader wildcard with
manual topic-suffix branching. Three simple loops are easier for a low-cost
model to get right and to test independently than one loop with a match
statement — the marginal MQTT overhead of three subscriptions is irrelevant
at Sparrow's scale.

```rust
pub async fn run_register_ingest(mqtt: nest_mqtt::MqttClient, registry: sparrow_core::storage::HostRegistry) -> NestResult<()> {
    let mut stream = mqtt.subscribe(sparrow_core::transport::Topics::all_register(), nest_mqtt::MqttQos::AtLeastOnce).await?;
    while let Some(msg) = futures_util::StreamExt::next(&mut stream).await {
        match sparrow_core::transport::RegisterMessage::from_payload(&msg.payload) {
            Ok(register) => {
                if let Err(err) = registry.upsert_on_register(&register.host_id, &register.hostname).await {
                    tracing::warn!(error = %err, host_id = %register.host_id, "failed to upsert host on register");
                }
            }
            Err(err) => tracing::warn!(error = %err, topic = %msg.topic, "malformed register payload"),
        }
    }
    Ok(())
}
```

Write `run_heartbeat_ingest` and `run_data_ingest` following the exact same
shape (subscribe → loop → parse → store → warn-and-continue on error, never
panic on one bad message). `run_data_ingest` calls
`MetricHistory::insert_batch`, not a per-item insert — this is the actual
high-frequency write path Phase 4's batch design exists for.

**Malformed-payload handling:** log and continue, never let one bad message
from a misbehaving agent kill the whole ingest loop. This is worth a test
(see below) specifically because it's the kind of thing that's easy to get
right by accident (an unhandled `Result` that happens to not panic) and easy
to get wrong later during a refactor.

### `offline_watch.rs`

A periodic task (same interval-loop-with-cancel-poll pattern established in
Phase 6 — reuse it, don't reinvent) that runs every ~30s, queries hosts whose
`last_seen` is older than some threshold (e.g. 3× the expected heartbeat
interval — 45s, given Phase 6's 15s heartbeat), and marks them offline via
`HostRegistry::mark_offline`. This is a **polling backstop**, not the
primary offline-detection mechanism — MQTT's LWT (configured on the agent's
`MqttConfig` in Phase 6) should mark a host offline near-instantly on an
unclean disconnect. This periodic sweep catches the case where an agent hangs
without disconnecting cleanly *or* uncleanly (e.g. frozen process still
holding the TCP connection). Document this distinction in the module's doc
comment — it's not obvious from the code alone why both mechanisms exist.

### `api/hosts.rs`

```rust
pub fn routes() -> nest_http_serve::RouteGroup {
    nest_http_serve::RouteGroup::new("/api")
        .get("/hosts", list_hosts)
        .get("/hosts/:id/items", get_latest_items)
}

async fn list_hosts(ctx: nest_http_serve::RequestContext) -> nest_http_serve::HttpResult {
    // CHECK: how does RequestContext expose registered services (AppContext access)?
    // Confirm the exact accessor (likely `ctx.app().service::<T>()` mirroring
    // AppContext::service seen in nest-core, but verify against RequestContext's
    // actual methods in context.rs before writing this) — do not guess the method name.
    todo!("fetch HostRegistry via ctx, list hosts, return JSON")
}

async fn get_latest_items(ctx: nest_http_serve::RequestContext) -> nest_http_serve::HttpResult {
    // CHECK: how are path params (`:id`) extracted from RequestContext? Confirm
    // against router.rs/context.rs before writing — likely `ctx.param("id")` or similar,
    // do not guess the exact method name or return type (Option<&str> vs Result).
    todo!("fetch latest metric_history rows per (host_id, key), return JSON")
}
```

Both `todo!()`s above are marked because `RequestContext`'s exact service-
access and path-param-extraction methods weren't verified against source in
this spec's research pass — **read `nest-http-serve/src/context.rs` before
implementing this file**, don't guess plausible-looking method names the way
a confident-but-wrong pattern-match might.

For `list_hosts`, add a small query to `HostRegistry` (Phase 4's
`storage.rs`) if one doesn't exist yet: `pub async fn list(&self) -> sqlx::Result<Vec<HostRow>>`.

For `get_latest_items`, "latest" means: for each distinct `key` under that
`host_id`, the row with the max `ts`. Write this as a single SQL query using
`DISTINCT ON (key) ... ORDER BY key, ts DESC` (Postgres-specific, fine since
we're Postgres-only) rather than fetching all history and filtering in Rust —
don't pull unbounded history into memory just to find the latest row per key.

### `api/history.rs`

```rust
pub fn routes() -> nest_http_serve::RouteGroup {
    nest_http_serve::RouteGroup::new("/api")
        .get("/hosts/:id/items/:key/history", get_history)
}
```

Accept optional query params for a time range (`from_ms`, `to_ms`) and a
result cap (`limit`, default something sane like 1000, hard max e.g. 10000 —
this endpoint reads `metric_history` directly and needs a bound or a chatty
client can pull an unbounded row set). Confirm how `nest-http-serve` exposes
query-string params on `RequestContext` before implementing — same "check,
don't guess" instruction as above.

---

## Tests

- `ingest.rs`: `testcontainers`-backed integration test — start Mosquitto + Postgres, publish a `RegisterMessage`/`HeartbeatMessage`/`DataBatch` directly (bypassing a real agent, to keep this test fast and focused on the server side), assert the expected row appears in Postgres. Include one test that publishes a deliberately malformed payload and asserts the ingest loop keeps running afterward (survives the bad message, doesn't panic or hang).
- `offline_watch.rs`: seed a host with an old `last_seen`, run one sweep, assert it's marked offline; seed a fresh one, assert it's untouched.
- `api/`: standard HTTP handler tests against a running server instance (spin up the real `nest-http-serve` host on a random port, issue requests via a plain HTTP client) — `GET /hosts` returns seeded hosts, `GET /hosts/{id}/items` returns only the latest row per key (seed multiple rows for the same key, assert only the newest comes back), `GET /hosts/{id}/items/{key}/history` respects `limit`.

**Acceptance:** `cargo test -p sparrow-server` passes with Docker running. End-to-end: start server + Phase 6 agent against the same broker/Postgres, confirm the host shows up via `GET /hosts` within one register-interval, and `GET /hosts/{id}/items` returns live cpu/memory/disk values.

## Explicit "do not" list

- Do not implement `api/hosts.rs`/`api/history.rs`'s `RequestContext` access without first reading the real `context.rs` — both handlers above are deliberately left as flagged `todo!()`s rather than guessed code, because guessing plausible-but-wrong Nest-specific API surface is worse than an honest gap here.
- Do not fetch full history and filter for "latest per key" in application code — use the SQL query shape described above.
- Do not let one malformed MQTT payload crash an ingest loop — log and continue, and write the test that proves it.
