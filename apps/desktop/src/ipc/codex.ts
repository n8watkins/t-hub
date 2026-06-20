// Typed wrapper for Codex plan usage (src-tauri/src/codex.rs), read from Codex's
// newest session rollout file (~/.codex/sessions). Mirrors ipc/usage.ts; the
// shape parallels ClaudeUsage's rate-limit windows (primary ≈ 5h, secondary ≈
// weekly), but Codex reports a Unix-epoch `resetsAt` instead of human text.
import { invoke } from "@tauri-apps/api/core";

export interface CodexRateWindow {
  /** Used amount 0..=100; the UI shows "left" = 100 - used. */
  usedPercent: number | null;
  /** Window length in minutes (≈300 for the 5h, ≈10080 for the weekly). */
  windowMinutes: number | null;
  /** Unix-epoch seconds the window resets (null until known). */
  resetsAt: number | null;
}

export interface CodexUsage {
  /** The ~5h ("session") window. */
  primary: CodexRateWindow | null;
  /** The ~weekly window. */
  secondary: CodexRateWindow | null;
  /** Plan tier reported by Codex (e.g. "plus"). */
  planType: string | null;
  /** Current conversation tokens + the model's context window (a context hint). */
  contextTokens: number | null;
  contextWindow: number | null;
  /** True when a recognizable usage reading was found. */
  ok: boolean;
}

/** Read Codex plan usage from its newest session rollout. Best-effort: resolves
 *  to `{ ok: false }` when no Codex session / usage data is present. */
export function codexUsage(): Promise<CodexUsage> {
  return invoke("codex_usage");
}
