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
 * Precedence (Claude wins ties throughout):
 * 1. AUTHORITATIVE TITLE — the strongest signal. If the title's basename
 *    (split on `/` and `\`, last segment) is EXACTLY `claude` or `codex`, the
 *    foreground command IS that client; classify decisively. This beats a label
 *    that merely mentions the other client.
 * 2. WORD-BOUNDARY FALLBACK — otherwise scan the full haystack (title + label +
 *    claudeTitle) for the whole word `\bclaude\b` / `\bcodex\b`. This rescues a
 *    freshly-spawned tile (title still the generic "terminal") or a node-hosted
 *    CLI via its label / Claude-suggested title.
 *
 * Word-boundary matching (vs. a naive substring) avoids false positives where a
 * signal merely CONTAINS the word: a binary like `codexgen`/`codexlib`, or a
 * shell tile a user renamed to e.g. "review claude's PR", no longer misfire.
 *
 * Residual limitations (acceptable / out of scope):
 * - An adjacent non-word char still matches: `claude-monitor` reads as claude.
 *   Rare enough to live with.
 * - A CLI whose `pane_current_command` is `node` with no claude/codex word in
 *   ANY signal reads as "shell" — environment-dependent. The real fix is a
 *   backend session→client mapping, which is out of scope here.
 */
export function clientForTerminal(id: TerminalId): ClientType {
  const s = useWorkspace.getState();

  // 1. Authoritative title: the tmux foreground command's basename.
  const title = s.terminals[id]?.title;
  if (typeof title === "string" && title.length > 0) {
    const base = title.split(/[/\\]/).pop()?.trim().toLowerCase() ?? "";
    if (base === "claude") return "claude";
    if (base === "codex") return "codex";
  }

  // 2. Word-boundary fallback over the full haystack (title + label + summary).
  const haystack = [title, s.labels[id], s.claudeTitles[id]]
    .filter((v): v is string => typeof v === "string" && v.length > 0)
    .join(" ")
    .toLowerCase();
  if (/\bclaude\b/.test(haystack)) return "claude";
  if (/\bcodex\b/.test(haystack)) return "codex";
  return "shell";
}

/**
 * True when ANY open tile is running Codex — i.e. "Codex is in use". Used to GATE
 * Codex usage polling: when no Codex session is open we don't spawn the WSL read
 * (the cached, time-advanced last-known value still shows). Subscribes to the
 * workspace store, so it re-evaluates when a tile's title/label changes (e.g. a
 * spawned `codex` tile becomes identifiable, or it closes).
 */
export function useHasCodexSession(): boolean {
  return useWorkspace((s) =>
    Object.keys(s.terminals).some(
      (id) => clientForTerminal(id as TerminalId) === "codex",
    ),
  );
}
