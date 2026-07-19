/** True when the UI runs inside the Tauri webview. */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function appWindow() {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow();
}

/** Closes the main window (File → Quit). No-op in Vite-only dev. */
export async function quitApp(): Promise<void> {
  await closeWindow();
}

/** Closes the main window. No-op in Vite-only dev. */
export async function closeWindow(): Promise<void> {
  if (!isTauri()) {
    return;
  }
  await (await appWindow()).close();
}

/** Minimizes the main window. No-op in Vite-only dev. */
export async function minimizeWindow(): Promise<void> {
  if (!isTauri()) {
    return;
  }
  await (await appWindow()).minimize();
}

/** Toggles maximized state. No-op in Vite-only dev. */
export async function toggleMaximizeWindow(): Promise<void> {
  if (!isTauri()) {
    return;
  }
  await (await appWindow()).toggleMaximize();
}
