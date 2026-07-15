# Phase 14 Task Spec — Testing, Docs, Polish

**Repo:** `pacificnm/sparrow`
**Prerequisite:** everything — this is the closing phase.

## Scope

Three things: one real end-to-end test that exercises the whole system
together (every prior phase tested its own slice in isolation), and two docs
deliverables aimed at people other than the model that built this.

### End-to-end test (`tests/e2e.rs` at the workspace root, or `crates/server/tests/e2e.rs`)

Assemble, via `testcontainers`, the full stack in one test: Mosquitto +
Postgres + a running `crates/server` instance + **N fake agents** (not real
`crates/agent` binaries — lightweight test doubles that publish synthetic
`DataBatch`/`RegisterMessage`/`HeartbeatMessage` payloads directly over MQTT,
faster and more deterministic than spawning real agent processes) + a mocked
AI provider (a fake `AiProvider` test double, same pattern as Phase 10's
`loop.rs` tests — not a real Ollama/Claude call, this test should not depend
on network access or a running model).

Sequence the test asserts, in order:
1. N fake agents register → `GET /api/hosts` shows all N.
2. Fake agents publish data → `GET /api/hosts/{id}/items` reflects it.
3. A seeded rule trips on one fake agent's data → a Problem opens (Phase 8).
4. The mocked AI provider, asked to explain that Problem (Phase 10's
   `run_analysis`), returns a scripted response — assert the loop completes
   and the response surfaces through whatever API endpoint Phase 10/11 wired
   up.
5. One fake agent stops heartbeating → `offline_watch` (Phase 7) eventually
   marks it offline — this step needs the test to either wait out the real
   offline-threshold interval or (better) make that interval configurable
   and set it small for the test, rather than making the test slow.

**This is the test that would have caught any interface mismatch between
phases** (e.g. the `AgentConfigOverride` shape drift flagged as a risk in
Phase 9, or the `GET /api/problems` endpoint Phase 11 assumed but Phase 7/8
never explicitly specified) — treat failures here as signal that two
phases' specs disagreed, not just as bugs to patch locally.

### Collector-authoring guide (`docs/authoring-collectors.md`)

Aimed at someone adding a fourth collector later without reading this
project's phase specs. Cover: the `Collector` trait (Phase 4), the `&mut
self`/persistent-state design note (Phase 4/5), the shared `metric()` helper
convention (Phase 5), where to register a new collector (`collectors/mod.rs`'s
`default_collectors()`), and — importantly — the topic/storage implications
of adding a new metric `key` (does it need a new dashboard panel? a new
default trigger rule? neither is required, but a reader should know they're
optional, not silently expected).

### Architecture docs (`docs/architecture.md`, Sparrow's own, distinct from the Nest framework's)

One diagram (ASCII is fine, doesn't need to be fancy) plus prose covering:
the three components (Collector/Agent/Server) and how they map to Nest
surfaces (per the original plan's §2 mapping table), the pub/sub topic
taxonomy (Phase 4), the deliberate deviations from Zabbix's poll-based model
(retained-message config push instead of active-check polling, LWT instead
of availability polling) — this is the document that explains *why*
Sparrow's architecture looks the way it does, for a reader who knows Zabbix
and is wondering why this doesn't look like it.

### API reference

Can likely be generated rather than hand-written if `crates/server`'s route
handlers (Phase 7, 8, 9, 10, 11) have adequate doc comments — check whether
the Nest ecosystem has a convention for this (an existing product's `docs/`
folder likely shows the expected format) before hand-writing a full REST API
reference from scratch.

## Acceptance for the whole plan

- `cargo test` (workspace-wide) passes, including the new end-to-end test, with Docker running and zero manual setup steps.
- `./build check` passes (whatever CI-equivalent checks that profile runs — lint, format, doc-build).
- The three docs deliverables exist and a person unfamiliar with this specific plan document (but familiar with the Nest platform generally) could plausibly add a fifth collector, or diagnose why a host shows offline, using only `docs/`, not this phase-spec series.

## Explicit "do not" list

- Do not treat an end-to-end test failure as "just fix the code" without first checking whether it's actually exposing a spec-to-spec disagreement between two earlier phases — those are worth fixing at the spec level too, not just patching the symptom.
- Do not hand-write a full API reference if the framework has a doc-generation convention already — check first.
- Do not consider this phase done with only the end-to-end test passing and no docs — both were named explicitly in the original plan's Phase 14 scope for a reason (a fully working but undocumented system is a real gap, not a nice-to-have left for later).
