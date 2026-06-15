// Typed wrapper for the Claude plan-usage query (`claude -p /usage`, parsed in
// src-tauri/src/usage.rs). The sidebar Usage strip polls this to show how much of
// the weekly / session limit is left. Mirrors the Rust `ClaudeUsage` struct
// (serde camelCase).
import { invoke } from "@tauri-apps/api/core";

export interface ClaudeUsage {
  sessionUsedPct: number | null;
  sessionResets: string | null;
  weekUsedPct: number | null;
  weekResets: string | null;
  weekSonnetUsedPct: number | null;
  /** True when `/usage` produced a recognizable readout. */
  ok: boolean;
}

/** Run `claude -p /usage` and return the parsed plan usage. Best-effort: the
 *  backend returns `{ ok: false }` rather than erroring when it can't read it. */
export function claudeUsage(): Promise<ClaudeUsage> {
  return invoke<ClaudeUsage>("claude_usage");
}
