// Per-tile CLIENT IDENTITY: which agent (if any) is running in a terminal —
// Claude Code, Codex, or just a plain shell. This is the single, shared place
// that question is answered, so the tile header (the Claude/Codex icon) and
// Wave 3 (Codex usage, auto-resume) read the SAME signal. Keep the exported
// signature stable: `clientForTerminal(id) => "claude" | "codex" | "shell"`.
//
// It's pure + synchronous: it snapshots the workspace store (no React hook), so
// render code can call it inline and non-React callers can use it too. Callers
// re-render on their own store subscriptions — e.g. Tile already subscribes to
// the terminal record + its label, which is exactly what this reads.
import type { TerminalId } from "../ipc/types";
import { useWorkspace } from "./workspace";

/** The client running in a tile. "shell" = no agent (a plain login shell). */
export type ClientType = "claude" | "codex" | "shell";

/**
 * Best-effort detection of the client running in a terminal tile.
 *
 * The signal is the backend `TerminalInfo.title`: `list_terminals` sets it to
 * the tmux pane's foreground command, so an active tile reports `claude` /
 * `codex` / `zsh` — i.e. the client itself. (Right after a spawn, before tmux
 * is polled, the title is briefly the generic "terminal"; the effective label
 * and any live Claude-suggested title are folded in as a fallback to bridge
 * that window and to honor an explicit rename.)
 *
 * Heuristic — Claude wins ties (its title is the authoritative `claude`, so a
 * Claude tile whose WORK label happens to mention "codex" still reads claude):
 * any "claude" => claude; else any "codex" => codex; otherwise a plain shell.
 */
export function clientForTerminal(id: TerminalId): ClientType {
  const s = useWorkspace.getState();
  const haystack = [s.terminals[id]?.title, s.labels[id], s.claudeTitles[id]]
    .filter((v): v is string => typeof v === "string" && v.length > 0)
    .join(" ")
    .toLowerCase();
  if (haystack.includes("claude")) return "claude";
  if (haystack.includes("codex")) return "codex";
  return "shell";
}
