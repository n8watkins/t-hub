// Typed wrapper for the Claude plan-usage query (`claude -p /usage`, parsed in
// src-tauri/src/usage.rs). The sidebar Usage strip polls this to show how much of
// the weekly / session limit is left. Mirrors the Rust `ClaudeUsage` struct
// (serde camelCase).
import { controlRequest } from "./controlClient";

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
 *  backend returns `{ ok: false }` rather than erroring when it can't read it.
 *
 *  Server-split M3 (overlay source over the wire): routed over the control socket
 *  (`claude_usage` in control.rs) instead of the in-process Tauri command —
 *  shape-identical, so it's a transport swap. A thin client now gets the REMOTE
 *  daemon's Claude usage. */
export function claudeUsage(): Promise<ClaudeUsage> {
  return controlRequest("claude_usage") as Promise<ClaudeUsage>;
}
