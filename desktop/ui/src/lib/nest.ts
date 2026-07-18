/**
 * Wrappers around nest-tauri's own built-in IPC commands — invoked bare
 * (`invoke("nest_app_metadata")`), not through the `plugin:sparrow|...`
 * prefix Sparrow's own commands need (see api.ts), since these are
 * registered by `attach_invoke_handler`, not `TauriApp::with_builder`
 * (verified against real nest-tauri source in Issue 11.0).
 */
import { invoke } from "@tauri-apps/api/core";

export type AppMetadata = {
  name: string;
  title: string;
};

export type ThemeCss = {
  id: string;
  mode: string;
  variables: Record<string, string>;
  root_block: string;
};

export async function fetchAppMetadata(): Promise<AppMetadata> {
  return invoke<AppMetadata>("nest_app_metadata");
}

export async function fetchThemeCss(): Promise<ThemeCss> {
  return invoke<ThemeCss>("nest_theme_css");
}

/** Injects `nest_theme_css`'s `root_block` into a `<style>` tag. */
export function applyThemeRootBlock(rootBlock: string): void {
  let style = document.getElementById("nest-theme-vars");
  if (!style) {
    style = document.createElement("style");
    style.id = "nest-theme-vars";
    document.head.appendChild(style);
  }
  style.textContent = rootBlock;
}
