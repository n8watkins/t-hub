// Typed IPC wrappers for the theme contract — the surface Claude/MCP also
// targets. Mirrors `src-tauri/src/theme.rs`:
//   - command `get_theme()  -> String` (JSON of the persisted theme, or "" when
//     none is persisted yet)
//   - command `set_theme(theme: String)` (persists the JSON + emits the event)
//   - event   `theme://changed` (payload: the theme JSON String)
//
// The store imports these lazily so it carries no hard dependency on Tauri
// (importing the store outside a webview must not throw). Keep the command and
// event names here in lockstep with the Rust side and with the MCP forwarder.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Theme } from "../store/theme";

/** Exact Tauri command names for the theme contract. */
export const ThemeCommands = {
  /** Returns the persisted theme as a JSON string ("" if none). */
  getTheme: "get_theme",
  /** Persists a theme (JSON string) and emits `theme://changed`. */
  setTheme: "set_theme",
} as const;

/** Exact event channel the backend emits when the theme changes (incl. via MCP). */
export const ThemeEvents = {
  changed: "theme://changed",
} as const;

/**
 * Fetch the backend's persisted theme. Resolves to the parsed Theme, or `null`
 * when the backend has nothing persisted yet (fresh install) — the caller then
 * seeds it from the local default.
 */
export async function getThemeBackend(): Promise<Theme | null> {
  const json = await invoke<string>(ThemeCommands.getTheme);
  if (!json) return null;
  try {
    return JSON.parse(json) as Theme;
  } catch {
    return null;
  }
}

/** Persist `theme` to the backend (which then emits `theme://changed`). */
export async function setThemeBackend(theme: Theme): Promise<void> {
  await invoke(ThemeCommands.setTheme, { theme: JSON.stringify(theme) });
}

/**
 * Subscribe to `theme://changed`. The payload is the theme JSON string; we parse
 * it and hand the caller a Theme. Returns the Tauri unlisten fn.
 */
export async function onThemeChanged(
  cb: (theme: Theme) => void,
): Promise<UnlistenFn> {
  return listen<string>(ThemeEvents.changed, (ev) => {
    try {
      cb(JSON.parse(ev.payload) as Theme);
    } catch {
      // Malformed payload — ignore rather than crash the listener.
    }
  });
}
