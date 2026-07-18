//! Sparrow's own Tauri IPC commands, packaged as a plugin.
//!
//! `nest-tauri`'s `TauriApp::run()` already claims the Tauri builder's one
//! `invoke_handler` slot for its own built-in commands (`nest_app_metadata`,
//! `nest_theme_css`) — a second direct `invoke_handler` call from
//! application code would silently overwrite those, not add to them
//! (verified against real `nest-tauri` source in Issue 11.0). The
//! supported extension point is a Tauri **plugin**, attached via
//! `TauriApp::with_builder` in `main.rs`. UI-side consequence: these
//! commands are invoked as `plugin:sparrow|list_hosts`, not a bare
//! `invoke('list_hosts')`.

pub mod analyst;
pub mod hosts;
pub mod problems;

/// Builds the `"sparrow"` plugin registering every command in this module.
/// `generate_handler!` must live in the same module as the `#[tauri::command]`
/// fns it lists, hence this one function rather than one per submodule.
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
