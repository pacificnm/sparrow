# Project Sparrow — Phased Implementation Plan (v5)

All three open decisions from v4 are now locked in:

1. **`nest-data-postgres` tests get retrofitted to `testcontainers-rs`**, not just left alone.
2. **Desktop dashboard stays a later phase** (after core data path + alerting are proven).
3. **`nest-ai-claude` gets built now** — a proper `AiProvider` adapter wrapping `nest-claude`, so the AI Health Analyst is swappable between local Ollama and Claude via `nest-ai` from day one, not a hardcoded choice.

That third one adds a genuine new piece of framework work, so it gets its own
phase alongside `nest-mqtt`. Phase plan below is renumbered accordingly.

---

## 1. Repo & workspace (unchanged)

- Product repo: `github.com/pacificnm/sparrow`, checked out locally at `nest/apps/sparrow/`.
- `crates/core` (shared domain logic), `crates/agent` (`nest-cli`), `crates/server` (`nest-http-serve`), `desktop/` (`nest-tauri`, Phase 11).

---

## 2. Framework pre-work, finalized

### `nest-data-postgres` — hardening + retrofit
- Startup retry/backoff on `PostgresConnection::connect` (bounded, configurable) — unchanged from v4.
- **Retrofit the existing integration tests** off the `DATABASE_URL` + `cargo test -- --ignored` pattern onto `testcontainers-rs` (automatic Postgres container spin-up/teardown per test run, no manual env var, no `--ignored` flag).
- **Scope note:** this only touches test files, not the module's public API — `PostgresConfig`/`PostgresConnection`/`PostgresDataModule`/`PostgresMigrationRunner` all stay the same. Should be a non-breaking change for Swift (the module's existing consumer), but since it's a shared framework module, worth a heads-up / review pass rather than merging silently — flagging so the task spec includes "confirm Swift's own test suite still passes after this change" as an explicit acceptance step, not an afterthought.
- Batch-write pattern for high-frequency metric writes still lives in Sparrow's own `crates/core/storage.rs` (not the framework module) — unchanged from v4.

### `nest-mqtt` — build from scratch (unchanged from v4)
- `rumqttc`-backed `MqttClientService` + `MqttModule`, same shape as `nest-http-client`.
- `testcontainers-rs` integration tests against Mosquitto (consistent with the retrofitted `nest-data-postgres` convention above — one testing pattern across all new/touched modules).

### `nest-ai-claude` — new, build from scratch
An `AiProvider` implementation (the trait defined in `nest-ai`) that wraps
`nest-claude`'s `ClaudeClient`, mirroring how `nest-ai-ollama` wraps Ollama's
`/api/chat`. Design sketch (to be confirmed against `nest-ai`'s exact trait
signatures — `tools.rs`/`provider.rs` — when we write the detailed task spec,
since I've only read `nest-ai`'s README so far, not its full source the way I
did for `nest-data-postgres`):

| `nest-ai` concept | Maps to `nest-claude` concept |
|---|---|
| `CompletionRequest` | `CreateMessageRequest` (messages, tools, system, `max_tokens`) |
| `ChatMessage` (user/assistant/tool_result) | `Message::user(...)` / tool-result content blocks |
| `ToolDefinition` | `ToolDefinition::new(name, description, schema)` — shapes look compatible already based on the tool-use examples in both READMEs |
| Response `ToolCall`s | `response.tool_uses()` |
| Streaming chunks | `StreamEvent::ContentBlockDelta` |

- Module registration follows `nest-ai-ollama`'s pattern (`config.rs`, `client.rs`, `provider.rs`, `module.rs`, `error.rs`, `stream.rs`).
- `nest-claude`'s extended thinking / effort config (`ThinkingConfig::adaptive()`, `Effort::High`) doesn't have an obvious `nest-ai`-generic equivalent yet — likely needs either an extension to `nest-ai`'s `CompletionRequest` (a generic "reasoning effort" concept other future providers could also use) or a Claude-specific escape hatch on the adapter. Worth a decision when we're actually writing this task spec, not blocking the phase plan now.
- Crate path: `modules/crates/nest-ai-claude`.

---

## 3. Phase Plan

### Phase 0 — Foundations & Decisions (no code)
- All decisions now locked (§2 above). `MetricItem`/`AgentConfig` structs, topic taxonomy, `NEST_SPARROW_*`/`NEST_MQTT_*`/`NEST_AI_CLAUDE_*` error code ranges.

### Phase 1 — Harden + retrofit `nest-data-postgres` (framework repo)
- Retry/backoff on connect; retrofit existing tests to `testcontainers-rs`; confirm Swift's test suite unaffected.
- **Acceptance:** `nest-data-postgres` test suite passes under `testcontainers-rs` with no manual setup steps; Swift's own tests still pass.

### Phase 2 — Build `nest-mqtt` (framework repo)
- `MqttClientService` + `MqttModule`, `testcontainers-rs` tests, docs.
- **Acceptance:** standalone example publishes/subscribes through a real (containerized) broker.

### Phase 3 — Build `nest-ai-claude` (framework repo)
- `AiProvider` impl per §2's mapping table, module registration, tests (can mock the Anthropic API at the HTTP layer the way `nest-claude`'s own tests do — no live API key needed for CI).
- **Acceptance:** a standalone example runs the same completion request against both `nest-ai-ollama` and `nest-ai-claude` and gets a valid response from each — proves the swap works at the `AiProvider` trait level.

### Phase 4 — Sparrow Core Contracts
- `crates/core/collector.rs` (`Collector` trait, `MetricItem`), `crates/core/transport.rs` (topic taxonomy on `nest-mqtt`), `crates/core/storage.rs` (host registry + batch-write metric history on `nest-data-postgres`).
- **Acceptance:** round-trip tests for publish/subscribe and write/read pass.

### Phase 5 — Collectors
- `cpu`, `memory`, `disk`, unit tested, explicit registry.

### Phase 6 — Agent (`crates/agent`, `nest-cli`)
- `nest-task-runtime` scheduler, `nest-mqtt` publish, heartbeat + LWT, buffered outages, live config reload.

### Phase 7 — Server (`crates/server`, `nest-http-serve`)
- Subscribes to all agent topics, host registry + batch ingest via `nest-data-postgres`, REST API.

### Phase 8 — Trigger / Alerting Engine
- Rule model, evaluation loop, Problem state persisted.

### Phase 9 — Server → Agent Config Push
- Retained MQTT config messages; agents apply live.

### Phase 10 — AI Health Analyst
- Tool-calling loop (`get_host_status`, `get_metric_history`, `get_active_problems`, `search_similar_incidents` via `nest-data-postgres`'s pgvector `VectorSearch`) against **either** provider through the now-swappable `nest-ai` abstraction — config picks `nest-ai-ollama` or `nest-ai-claude` per deployment, no code change either way.
- Embedding generation for `search_similar_incidents` needs a quick check against Ollama's `/api/embeddings` support before finalizing the task spec (flagged in v4, still open — small research step, not a phase blocker).
- Two analysis modes: fast per-Problem explanation, slower periodic health-trend report (uses `nest-ai-claude`'s access to extended thinking when that provider is selected).

### Phase 11 — Desktop Dashboard (`desktop/`, `nest-tauri`)
- Host list, live values, active Problems, AI Health Analyst panel.

### Phase 12 — Security Hardening
- TLS + auth on Mosquitto, broker ACLs per host topic.

### Phase 13 — Packaging & Deployment
- Release builds, systemd units, Docker/compose for broker + server + Postgres.

### Phase 14 — Testing, Docs, Polish
- End-to-end test: broker + Postgres + server + N fake agents + a mocked AI provider.
- Collector-authoring guide, architecture docs, API reference.

---

## 4. Status

No open decisions remain at the plan level. Two small research steps are
flagged inline for when we get to their phases (not blockers now):

- Exact `nest-ai` trait signatures for the `nest-ai-claude` adapter (Phase 3).
- Ollama embeddings support for `search_similar_incidents` (Phase 10).

Ready to start writing detailed, strict, per-task specs for Qwen — suggest
starting at **Phase 1**, since everything else is blocked on the framework
pre-work landing first. Let me know if you want to review Phase 1's task
breakdown before I write the actual instructions, or if you'd rather I just
go ahead and draft them.
