// Shared "is the main window maximized?" state — ONE subscription for the whole
// app.
//
// Two places need this flag: the titlebar (the max/restore glyph + the
// maximize-button rect report) and App (the auto-hide-on-maximize behavior).
// Each used to run its OWN `onResized` listener + `isMaximized()` IPC poll, so
// every resize event triggered two redundant round-trips. A resize-drag fires
// onResized tens of times/sec, doubling that churn for no reason.
//
// This module installs a SINGLE app-lifetime subscription (lazily, on first use)
// and fans the value out to every consumer via `useSyncExternalStore`. Tauri
// fires `tauri://resize` (onResized) on every size change INCLUDING the
// maximize/restore transition — however it was triggered (a control button, the
// native Snap flyout, a double-click, a keyboard shortcut) — so polling
// `isMaximized()` there keeps the flag in lockstep. The value is deduped, so an
// unchanged resize never notifies a subscriber (no re-render storm).
import { useSyncExternalStore } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

let maximized = false;
let started = false;
const listeners = new Set<() => void>();

function notify(): void {
  for (const l of listeners) l();
}

/** Lazily install the single onResized → isMaximized() subscription. Runs once
 *  for the app lifetime (the window's maximized state is global + cheap, so we
 *  never tear it down). Outside a Tauri window (plain `pnpm dev` / tests) the flag
 *  just stays false. */
function ensureStarted(): void {
  if (started) return;
  started = true;
  try {
    const win = getCurrentWindow();
    const refresh = () => {
      void win
        .isMaximized()
        .then((m) => {
          if (m !== maximized) {
            maximized = m;
            notify();
          }
        })
        .catch(() => {});
    };
    refresh(); // seed from the current state
    void win.onResized(() => refresh()).catch(() => {});
  } catch {
    // Not in a Tauri window: leave `maximized` false.
  }
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  ensureStarted();
  return () => {
    listeners.delete(cb);
  };
}

/**
 * Live "is the main window maximized?" flag, backed by one shared subscription.
 * Drop-in replacement for the former per-component `useMaximizedState` /
 * `useWindowMaximized` hooks — every caller now shares a single onResized listener
 * and a single `isMaximized()` poll per resize.
 */
export function useWindowMaximized(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => maximized,
    () => false,
  );
}
