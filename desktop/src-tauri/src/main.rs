#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::result_large_err)]

mod commands;
mod state;

use nest_config::ConfigDocument;
use nest_error::NestResult;
use nest_http_client::{HttpClientConfig, HttpClientService};
use nest_tauri::TauriApp;
use nest_theme::ThemeModule;

use state::AppState;

/// Default server base URL when `desktop/config.toml` is absent or doesn't
/// have a `[server]` section — a locally running Phase 7 server's default.
const DEFAULT_SERVER_BASE_URL: &str = "http://127.0.0.1:8080";

#[derive(serde::Deserialize)]
struct ServerSection {
    base_url: String,
}

fn main() {
    let app_state = build_app_state().expect("failed to build application state");

    // Design decision (Issue 11.1, per the phase-11 spec): Tauri commands
    // call crates/server's REST API over HTTP, not crates/core in-process.
    // Sparrow's server is a genuinely separate long-running process, and
    // this desktop app is an admin view onto a server running somewhere
    // else (not necessarily this machine) — not a copy of the server's
    // own logic linked into a desktop binary. This is a deliberate
    // deviation from the app standard's general in-process-core
    // preference.
    TauriApp::new("sparrow-desktop")
        .module(ThemeModule::default())
        .with_builder(move |builder| builder.manage(app_state).plugin(commands::plugin()))
        .run(tauri::generate_context!());
}

/// Reads the server base URL from `desktop/config.toml`'s `[server]`
/// section (`base_url` field, path overridable via `SPARROW_DESKTOP_CONFIG`
/// for packaged builds), falling back to [`DEFAULT_SERVER_BASE_URL`] when
/// the file or section is absent — the server address is user-configurable
/// (the whole point of a desktop admin app is that it doesn't have to run
/// on the same machine as the server), but a missing config file shouldn't
/// stop the app from launching against a sane local default.
fn build_app_state() -> NestResult<AppState> {
    let config_path = std::env::var("SPARROW_DESKTOP_CONFIG")
        .unwrap_or_else(|_| "desktop/config.toml".to_string());
    let base_url = resolve_base_url(&config_path);

    let http = HttpClientService::new(HttpClientConfig::default())?;
    Ok(AppState::new(http, base_url))
}

/// The pure, file-path-in/`String`-out part of [`build_app_state`], split
/// out so it's directly unit-testable against a temp config file without
/// mutating process-global env vars (which would race under Rust's default
/// parallel test execution).
fn resolve_base_url(config_path: &str) -> String {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|content| ConfigDocument::parse_toml(&content).ok())
        .and_then(|document| document.section::<ServerSection>("server").ok())
        .map(|section| section.base_url)
        .unwrap_or_else(|| DEFAULT_SERVER_BASE_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_base_url_reads_the_server_section_when_present() {
        let dir = std::env::temp_dir().join(format!(
            "sparrow-desktop-test-config-present-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let config_path = dir.join("config.toml");
        std::fs::write(
            &config_path,
            "[server]\nbase_url = \"http://example.invalid:9090\"\n",
        )
        .expect("write temp config");

        let base_url = resolve_base_url(config_path.to_str().expect("utf8 path"));

        assert_eq!(base_url, "http://example.invalid:9090");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_base_url_falls_back_to_the_default_when_the_file_is_absent() {
        let base_url = resolve_base_url("/nonexistent/sparrow-desktop-config.toml");

        assert_eq!(base_url, DEFAULT_SERVER_BASE_URL);
    }

    #[test]
    fn resolve_base_url_falls_back_to_the_default_when_the_server_section_is_absent() {
        let dir = std::env::temp_dir().join(format!(
            "sparrow-desktop-test-config-no-section-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "[other]\nsomething = \"else\"\n").expect("write temp config");

        let base_url = resolve_base_url(config_path.to_str().expect("utf8 path"));

        assert_eq!(base_url, DEFAULT_SERVER_BASE_URL);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
