//! Application-specific Tauri state (Issue 11.1).
//!
//! Distinct from `nest_tauri::NestHostState`, which holds nest-tauri's own
//! `Arc<AppContext>` service registry — Sparrow's commands don't need that,
//! per this phase's design decision: Tauri commands call `crates/server`'s
//! REST API over HTTP, not `crates/core` in-process (see `main.rs`'s
//! comment on why). `AppState` instead holds an `HttpClientService` pointed
//! at the Sparrow server's configured base URL. Registered via
//! `TauriApp::with_builder`'s `.manage(...)` (confirmed against
//! `nest-tauri`'s real source in Issue 11.0 — there is no built-in
//! `AppState` to reuse; this type has to exist).

use nest_http_client::HttpClientService;

pub struct AppState {
    http: HttpClientService,
    base_url: String,
}

impl AppState {
    pub fn new(http: HttpClientService, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    pub fn http(&self) -> &HttpClientService {
        &self.http
    }

    /// Joins `path` (e.g. `"/api/hosts"`) onto the configured server base URL.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}
