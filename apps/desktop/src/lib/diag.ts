// Runtime diagnostics (feat/diag) — frontend half.
//
// The app ships as a RELEASE build on Windows; the orchestrator (in WSL) can't
// see the WebView2 console. `tlog` mirrors a structured log line into BOTH the
// console (as today) AND a fixed file via the `diag_log` Tauri command, so a
// repro on the user's machine leaves a trail the orchestrator can `tail` from
// WSL (see src-tauri/src/diag.rs for the path).
//
// Hard rules for use in hot paths (e.g. the pool sync, fired per frame while
// dragging): NEVER await the invoke, and swallow every error. A logging call
// must never block layout or throw into a render/effect.
import { invoke } from "@tauri-apps/api/core";

/**
 * Runtime DEBUG gate for `tlog`. `tlog` fires per-terminal on every pool sync()
 * (many times/frame while dragging), on every resize settle, on every attach,
 * etc. — a console.log + fire-and-forget invoke on each call adds GC pressure
 * and feeds WebView2 RAM growth. So `tlog` is a near-total no-op unless this is
 * on, while staying switchable WITHOUT a rebuild.
 *
 * Sourced ONCE (cheap per-call boolean check) from either a `localStorage`
 * flag (`t-hub.debug === "1"`) or a `window.__T_HUB_DEBUG__` global, both guarded
 * for non-DOM contexts. Flip at runtime from devtools via `setDiagEnabled(true)`.
 * Default OFF.
 */
let diagEnabled = ((): boolean => {
  try {
    if (typeof window !== "undefined" && (window as { __T_HUB_DEBUG__?: unknown }).__T_HUB_DEBUG__) {
      return true;
    }
    if (typeof localStorage !== "undefined" && localStorage.getItem("t-hub.debug") === "1") {
      return true;
    }
  } catch {
    // localStorage can throw (disabled/partitioned storage); stay OFF.
  }
  return false;
})();

/**
 * Toggle the `tlog` DEBUG gate at runtime (e.g. from devtools) WITHOUT a rebuild.
 * Persists to `localStorage` (best-effort) so the choice survives a reload.
 */
export function setDiagEnabled(on: boolean): void {
  diagEnabled = on;
  try {
    if (typeof localStorage !== "undefined") {
      if (on) localStorage.setItem("t-hub.debug", "1");
      else localStorage.removeItem("t-hub.debug");
    }
  } catch {
    // best-effort persistence only — the in-memory flag is what gates `tlog`.
  }
}

/**
 * Fire-and-forget: ship one already-formatted line to the backend diag file.
 * Best-effort — the promise is intentionally not awaited and its rejection is
 * swallowed (no devtools, no Tauri, or the command missing must all be silent).
 */
function shipToFile(line: string): void {
  try {
    void invoke("diag_log", { line }).catch(() => {});
  } catch {
    // `invoke` can throw synchronously if the Tauri IPC isn't present (e.g. a
    // plain browser dev server). Swallow — diagnostics must never break the app.
  }
}

/**
 * Compact a single console arg into the diag payload. Strings/numbers/bools pass
 * through; everything else is JSON-stringified (with a fallback to String() for
 * cyclic/unserializable values) so the whole line stays single-line JSON.
 */
function compact(arg: unknown): unknown {
  if (
    arg === null ||
    typeof arg === "string" ||
    typeof arg === "number" ||
    typeof arg === "boolean"
  ) {
    return arg;
  }
  if (arg instanceof Error) {
    return { error: arg.message, stack: arg.stack };
  }
  try {
    // Round-trip through JSON so the value is a plain, single-line-safe shape.
    return JSON.parse(JSON.stringify(arg));
  } catch {
    return String(arg);
  }
}

/**
 * Log `tag` + args to the console (exactly like a `console.log` today) AND
 * fire-and-forget the same payload to the diag file as compact single-line JSON
 * `{t:<tag>, m:[...args]}`. Safe to call in hot paths — never awaits, never
 * throws.
 */
export function tlog(tag: string, ...args: unknown[]): void {
  // Gate FIRST: when DEBUG is off this is a near-total no-op — no message build,
  // no console.log, no invoke. Cheap module-level boolean; flip via setDiagEnabled.
  if (!diagEnabled) return;
  // Console first so a devtools session still sees everything live.
  console.log(`[${tag}]`, ...args);
  try {
    const line = JSON.stringify({ t: tag, m: args.map(compact) });
    shipToFile(line);
  } catch {
    // Stringify itself failed (shouldn't, compact() guards) — never let it throw.
  }
}

/**
 * Mirror an already-emitted console.warn/error (or a window error event) into the
 * diag file under a level tag, WITHOUT re-logging to the console (the original
 * call already did). Used by the console/window hooks below.
 */
function mirror(level: "warn" | "error" | "winerror" | "unhandled", args: unknown[]): void {
  try {
    const line = JSON.stringify({ t: level, m: args.map(compact) });
    shipToFile(line);
  } catch {
    // never throw out of a console hook
  }
}

let installed = false;

/**
 * Mount the diagnostics hooks once at app startup: mirror `console.warn`,
 * `console.error`, and the window `'error'`/`'unhandledrejection'` events into
 * the diag file too (on top of their normal behavior). Idempotent — a second
 * call is a no-op so multiple entry modules importing this stay safe.
 */
export function installDiagHooks(): void {
  if (installed) return;
  installed = true;

  // Wrap console.warn / console.error so EVERY warning/error in the app (incl.
  // the pool's degenerate-rect HOLD/PARK warnings) lands in the file. We call
  // through to the original first so the console behaves exactly as before.
  const origWarn = console.warn.bind(console);
  console.warn = (...args: unknown[]) => {
    origWarn(...args);
    mirror("warn", args);
  };
  const origError = console.error.bind(console);
  console.error = (...args: unknown[]) => {
    origError(...args);
    mirror("error", args);
  };

  if (typeof window !== "undefined") {
    window.addEventListener("error", (e: ErrorEvent) => {
      mirror("winerror", [
        e.message,
        `${e.filename}:${e.lineno}:${e.colno}`,
        e.error instanceof Error ? e.error.stack : undefined,
      ]);
    });
    window.addEventListener("unhandledrejection", (e: PromiseRejectionEvent) => {
      const r = e.reason;
      mirror("unhandled", [r instanceof Error ? r.message : r, r instanceof Error ? r.stack : undefined]);
    });
  }

  // A startup marker so the orchestrator can confirm hooks mounted and see when
  // a fresh session began in the log.
  tlog("diag", "installDiagHooks: hooks mounted");
}

// Self-init at import time. The cleanest mount point is the app entry
// (src/main.tsx), but that file is outside this worktree's ownership, so we
// instead install on first import — and a file we DO own (TerminalPool.tsx,
// mounted at app startup) imports this module, guaranteeing the hooks mount once
// at launch. `installed` keeps this idempotent if main.tsx is ever wired too.
installDiagHooks();
