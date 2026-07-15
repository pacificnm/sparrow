# Phase 11 Task Spec — Desktop Dashboard (`desktop/`, `nest-tauri`)

**Repo:** `pacificnm/sparrow`
**Crate/dir:** `desktop/` (`src-tauri/` + `ui/`)
**Prerequisite:** Phase 7 (server API), Phase 8 (Problems), Phase 10 (AI Health Analyst).

**Honesty note on this spec's depth:** unlike Phases 1–10, this one was
**not** written against `nest-tauri`'s actual source — Phases 1–10 consumed
the research budget for this pass, and `nest-tauri`'s IPC command
registration, `AppContext` access from Tauri command handlers, and the
`nest-theme`/`nest-react-theme` token pipeline all need their own read-through
before this spec is trustworthy at the same level as the earlier ones.
Treat everything below as a **first-pass structure**, not a final spec —
re-derive the IPC command signatures against `nest-tauri`'s real
`#[tauri::command]` conventions (referenced in the app standard's IPC
boundary section: `ui/src/ → invoke() → src-tauri/ #[tauri::command] →
crates/core`) before handing this to Qwen.

---

## Structure (per the app standard's desktop layout)

```
desktop/
├── ui/                      # React + TypeScript + Tailwind (Vite)
│   ├── src/
│   │   ├── App.tsx
│   │   ├── components/
│   │   │   ├── HostList.tsx
│   │   │   ├── HostDetail.tsx        # latest values for one host
│   │   │   ├── ProblemsPanel.tsx
│   │   │   └── AnalystPanel.tsx       # Phase 10's output surfaced here
│   │   └── lib/
│   │       └── api.ts                 # thin wrappers around Tauri `invoke()`
│   └── tailwind.config.ts
└── src-tauri/
    ├── src/
    │   ├── main.rs           # TauriApp::new("sparrow-desktop").module(...).run()
    │   └── commands/
    │       ├── hosts.rs        # #[tauri::command] wrappers -> crates/core or an HTTP call to crates/server
    │       ├── problems.rs
    │       └── analyst.rs
    └── tauri.conf.json
```

## Design decision to make explicitly before starting (not resolved here)

The desktop app can reach Sparrow's data two ways: (a) Tauri commands call
`crates/server`'s REST API over HTTP (treating the desktop app as just
another API client), or (b) Tauri commands call `crates/core` directly
in-process (bypassing HTTP, per the app standard's general preference for
"implement once in core, host adapters delegate to it"). **Recommendation:
(a), HTTP to the running server** — Sparrow's server is a genuinely separate
long-running process (unlike, say, Swift's desktop-only model), and the
desktop dashboard is meant to be an *admin view onto a server that's already
running somewhere*, not a copy of the server's own logic linked into a
desktop binary. This is a deliberate deviation from the app standard's
general in-process-core preference, worth a one-line comment in `main.rs`
explaining why, so it doesn't read as an oversight later.

## IPC commands (sketch — verify signatures against real `nest-tauri` conventions)

```rust
#[tauri::command]
async fn list_hosts(state: tauri::State<'_, AppState>) -> Result<Vec<HostSummary>, String> {
    state.http_client().get_json("/api/hosts").await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_host_items(state: tauri::State<'_, AppState>, host_id: String) -> Result<Vec<ItemValue>, String> {
    state.http_client().get_json(&format!("/api/hosts/{host_id}/items")).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_active_problems(state: tauri::State<'_, AppState>) -> Result<Vec<Problem>, String> {
    state.http_client().get_json("/api/problems").await.map_err(|e| e.to_string())
    // `GET /api/problems` (optionally `?host_id=`) is specified in Phase 8's
    // own spec ("API (`crates/server/src/api/problems.rs`)" section,
    // docs/plans/phase-8-trigger-alerting.md) — implemented as part of this
    // milestone (Issue 11.2) since that's when it's first needed, but the
    // contract lives with the Problem model, not invented here.
}

#[tauri::command]
async fn run_analysis(state: tauri::State<'_, AppState>, host_id: Option<String>) -> Result<String, String> {
    // Calls POST /api/analyst/run, specified in Phase 10's own spec
    // ("API (`crates/server/src/api/analyst.rs`)" section,
    // docs/plans/phase-10-ai-health-analyst.md) — implemented as part of
    // this milestone (Issue 11.2), contract lives with the agent loop it wraps.
    todo!()
}
```

`AppState` holds a configured `nest_http_client::HttpClientService` pointed
at the (locally or remotely) running Sparrow server's base URL, set via
`desktop/config.toml` (server address is user-configurable — the whole point
of a desktop admin app is that it doesn't have to run on the same machine as
the server).

## UI components — kept intentionally light on prescription

- `HostList.tsx` — table of hosts, online/offline badge, click-through to `HostDetail`.
- `HostDetail.tsx` — latest values grouped by collector, using whatever chart primitives the frontend-design skill's conventions call for (check `nest-react-theme`'s token set before hardcoding colors — same instruction as the rest of the platform's UI work).
- `ProblemsPanel.tsx` — open Problems list, severity-colored.
- `AnalystPanel.tsx` — a text box for a free-form question plus a "explain this Problem" quick action per Problem row, rendering `run_analysis`'s returned text (plain text/Markdown — check whether `nest_ai::CompletionResponse.content` is expected to contain Markdown formatting by convention elsewhere in the framework before deciding whether to render it as such here).

## Tests

- Rust side (`src-tauri`): unit test each command's HTTP-call construction against a mocked server (`wiremock`), same pattern used throughout Phases 1–3.
- UI side: standard React component tests (whatever test runner the rest of `ui/` in other Nest products uses — check an existing product's `ui/package.json`, e.g. Swift's, before picking one independently).

**Acceptance:** `./build dev` (tauri profile) launches the dashboard against a locally running Phase 7 server + Phase 6 agent, shows live host/item data, shows Problems opened by Phase 8, and returns a real analysis from Phase 10's endpoint.

## Explicit "do not" list

- Do not treat this spec's IPC signatures as final — re-verify against real `nest-tauri` source before implementation, per the honesty note at the top.
- Do not implement `GET /api/problems` or `POST /api/analyst/run` against a shape that differs from Phase 8's and Phase 10's own "API" sections — those are now the canonical contract, not this file. If the real `RequestContext` verification (Issue 7.3/9.3's "check, don't guess" instruction) forces a shape change, update Phase 8/10's spec too, don't let this file or the implementation quietly diverge from them.
- Do not hardcode UI colors without checking `nest-react-theme`'s token set first.
