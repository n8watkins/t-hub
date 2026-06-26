// Shared localStorage persistence codec for the Zustand stores
// (store/settings.ts, store/keybindings.ts, store/rules.ts).
//
// Each of those stores hand-rolled the same versioned load/save boilerplate:
//   - an SSR / headless guard (`typeof localStorage === "undefined"`), so a
//     `pnpm dev` without a DOM (or a test) never throws;
//   - a load that reads the key, falls back when it's absent, and on a corrupt /
//     unparseable blob returns the fallback rather than throwing into the store;
//   - a save that swallows quota / serialization failures (non-fatal — the value
//     simply stays in memory).
//
// This module captures EXACTLY those semantics so the stores share one
// implementation. Each store keeps its OWN `coerce`/sanitize and its own fallback
// (defaults / builtins) — only the guard + try/catch plumbing is shared.

/**
 * Load and decode a persisted value under `key`.
 *
 * Returns `fallback` when:
 *   - `localStorage` is unavailable (SSR / headless / no DOM);
 *   - the key is absent (`getItem` returns `null`);
 *   - the stored blob fails to JSON-parse, OR `coerce` throws.
 *
 * Otherwise the parsed JSON (an `unknown`) is handed to `coerce`, whose return
 * value is the decoded result. `coerce` owns all shape validation / sanitization
 * (defaulting individual fields, dropping unknown entries, clamping numbers, …);
 * with no `coerce`, the raw parsed JSON is returned cast to `T`.
 *
 * NOTE: an empty-string value (which is not valid JSON) falls through to the
 * parse `catch` and yields `fallback` — matching the prior hand-rolled behavior,
 * where a missing/blank key resolved to the fallback either way.
 */
export function loadPersisted<T>(
  key: string,
  fallback: T,
  coerce?: (parsed: unknown) => T,
): T {
  if (typeof localStorage === "undefined") return fallback;
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const parsed = JSON.parse(raw) as unknown;
    return coerce ? coerce(parsed) : (parsed as T);
  } catch {
    // Absent / corrupt / unavailable — non-fatal; use the fallback.
    return fallback;
  }
}

/**
 * Persist `value` under `key` as JSON, best-effort.
 *
 * A no-op when `localStorage` is unavailable (SSR / headless). A quota or
 * serialization failure is swallowed (non-fatal — the value stays in memory),
 * exactly like the prior per-store `savePersisted`/`save` did.
 */
export function savePersisted(key: string, value: unknown): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Quota / serialization failure — non-fatal; the value stays in memory.
  }
}
