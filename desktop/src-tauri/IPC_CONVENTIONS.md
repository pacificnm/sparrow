# `nest-tauri` IPC conventions — research spike (Issue 11.0)

Phase 11's spec was written without reading `nest-tauri`'s actual source
(its own "Honesty note"). This doc records what a full read-through of
`core/crates/nest-tauri` (plus the `tauri` crate itself and one real
downstream consumer) actually shows, so Issues 11.1/11.2 implement
against verified conventions instead of the spec's plausible-but-unconfirmed
sketch.

## 1. Command registration — the spec's sketch is wrong here

The spec's structure sketch (`src-tauri/src/commands/hosts.rs` etc., each a
plain `#[tauri::command]` fn, wired up in `main.rs`) does not work as
written. `nest-tauri`'s `TauriApp::run()` already calls
`attach_invoke_handler` internally (`bootstrap.rs::run_with_context`),
which claims the builder's *one* `invoke_handler` slot for its own built-in
commands (`nest_app_metadata`, `nest_theme_css`, …). Tauri only keeps the
*last* `invoke_handler` call on a builder — a second direct call from
application code would silently replace, not add to, the built-ins.

**The real mechanism** (confirmed in `nest-tauri/src/app.rs`'s own doc
comment on `TauriApp::with_builder`): application commands must be
packaged as a **Tauri plugin** and attached via `with_builder`, which runs
*after* the built-in handler is already registered:

```rust
// desktop/src-tauri/src/commands/mod.rs
pub fn plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("sparrow")
        .invoke_handler(tauri::generate_handler![
            hosts::list_hosts,
            hosts::get_host_items,
            problems::get_active_problems,
            analyst::run_analysis,
        ])
        .build()
}

// desktop/src-tauri/src/main.rs
TauriApp::new("sparrow-desktop")
    .module(nest_theme::ThemeModule::default())
    .with_builder(|builder| {
        builder
            .manage(SparrowState::new(/* ... */))
            .plugin(commands::plugin())
    })
    .run(tauri::generate_context!());
```

(`tauri::plugin::Builder::new(name).invoke_handler(...).build()` —
verified against the `tauri` crate itself, `src/plugin.rs`.)

**UI-side consequence:** plugin commands are invoked as
`plugin:sparrow|list_hosts`, not a bare `invoke('list_hosts')`. `lib/api.ts`
(Issue 11.1) needs to use that invocation form, not the bare form the
spec's sketch implicitly assumed.

`generate_handler!` must live in the same module as the `#[tauri::command]`
fns it lists (a real macro constraint, not a style choice) — so
`commands/mod.rs` re-exporting all four command fns and listing them
together in one `generate_handler!` call, as sketched above, is required,
not just tidy.

## 2. State access — `tauri::State<'_, T>` is right, `AppState` isn't the type

The spec guessed `tauri::State<'_, AppState>`. The idiom
(`tauri::State<'_, T>`) is correct, but there is no built-in `AppState` —
there are two *separate* managed state types, and Sparrow's commands need
their own:

- **`nest_tauri::NestHostState`** — registered automatically by
  `TauriApp::run()` before the built-in commands attach
  (`bootstrap.rs::run_with_context`: `.manage(host_state)` happens first).
  Holds `app_name`, `runtime_config`, and `context: Arc<AppContext>` (the
  Nest service registry). This is what `nest_app_metadata`/`nest_theme_css`
  use internally. **Sparrow's own commands do not need this** — per the
  phase spec's own design decision (HTTP to the running server, not
  in-process `crates/core` access), there's no `AppContext` service
  Sparrow's IPC commands need to reach.
- **Sparrow's own state** — does not exist until application code creates
  it. Must be a new struct (e.g. `SparrowState { http: HttpClientService,
  server_base_url: String }`), registered via `.manage(SparrowState::new(..))`
  inside the `with_builder` closure (see §1's example), and accessed in
  each command as `tauri::State<'_, SparrowState>`. This is the direct
  replacement for the spec's `tauri::State<'_, AppState>` guess — same
  idiom, different (application-defined, not framework-provided) type.

## 3. Theme pipeline — already fully handled, no Sparrow code needed

`nest-tauri`'s built-in `nest_theme_css` command (`commands.rs`) already
wraps `nest_theme::ThemeService` + `nest_react_theme::ReactThemeAdapter`
and returns ready-to-inject CSS variables. Registering
`.module(nest_theme::ThemeModule::default())` on `TauriApp` (confirmed
against `templates/desktop/src-tauri/src/main.rs`, a real working example)
is the entire integration point. The React UI calls the built-in
`invoke('nest_theme_css')` (bare, not a plugin command — it's part of
`attach_invoke_handler`, not `with_builder`) to get theme tokens. Sparrow's
`ui/src/lib/api.ts` needs no theme-specific code beyond that one call.

## 4. Command return type — `NestResult<T>`, and it needs a feature flag

`nest-tauri`'s own commands return `NestResult<T>` (i.e. `Result<T,
NestError>`), not `Result<T, String>` (the pattern an older, non-nest-tauri
Tauri app in this monorepo uses instead — `ui/src-tauri`, which predates/
bypasses `nest-tauri` entirely and isn't the pattern to follow here).
Tauri command error types must implement `Serialize`; `NestError` does,
but **only behind `nest-error`'s `serde` feature flag**
(`core/crates/nest-error/src/error.rs`, `#[cfg(feature = "serde")]`).
Sparrow's `desktop/src-tauri/Cargo.toml` must enable
`nest-error = { workspace = true, features = ["serde"] }` (or add the
feature at the workspace level) or these commands won't compile.

Re-derived signatures for Issue 11.1, following this:

```rust
#[tauri::command]
pub async fn list_hosts(state: tauri::State<'_, SparrowState>) -> NestResult<Vec<HostRow>> { .. }

#[tauri::command]
pub async fn get_host_items(
    state: tauri::State<'_, SparrowState>,
    host_id: String,
) -> NestResult<Vec<MetricHistoryRow>> { .. }

#[tauri::command]
pub async fn get_active_problems(
    state: tauri::State<'_, SparrowState>,
    host_id: Option<String>,
) -> NestResult<Vec<Problem>> { .. }

#[tauri::command]
pub async fn run_analysis(
    state: tauri::State<'_, SparrowState>,
    request: RunAnalysisRequest,
) -> NestResult<String> { .. }
```

(`async fn` + `tauri::State<'_, T>` first arg: Tauri commands invoking
async work must be `async fn`, confirmed against `tauri`'s own command
macro expectations and consistent with `nest-tauri`'s `nest_image_fetch`,
which is sync only because image fetch is itself sync in that crate — an
HTTP round-trip to `crates/server` is not, so Sparrow's commands need
`async fn`.)

## Summary: what changes from the spec's sketch

| Spec's guess | What's actually true |
|---|---|
| Plain `#[tauri::command]` fns wired via `main.rs` directly | Must be a Tauri **plugin**, attached via `TauriApp::with_builder` |
| Bare `invoke('list_hosts')` | `invoke('plugin:sparrow\|list_hosts')` |
| `tauri::State<'_, AppState>` | `tauri::State<'_, SparrowState>` (app-defined, `.manage()`d in `with_builder`) |
| (unstated) return type | `NestResult<T>`, requires `nest-error`'s `serde` feature |
| (unstated) theming work | None — `ThemeModule::default()` + the built-in `nest_theme_css` command already cover it |
