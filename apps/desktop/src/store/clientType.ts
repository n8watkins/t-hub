// Per-tile CLIENT IDENTITY: which agent (if any) is running in a terminal —
// Claude Code, Codex, or just a plain shell. This is the single, shared place
// that question is answered, so the tile header (the Claude/Codex icon) and
// Wave 3 (Codex usage, auto-resume) read the SAME signal. Keep the exported
// signature stable: `clientForTerminal(id) => "claude" | "codex" | "shell"`.
//
// The synchronous API snapshots the workspace and Captain stores for non-React
// callers. React surfaces use `useClientForTerminal` so provider changes and
// heuristic terminal changes both trigger a render.
import type { TerminalId } from "../ipc/types";
import {
  useCaptain,
  type CaptainClaimRecord,
  type CrewRef,
} from "./captain";
import { useWorkspace } from "./workspace";

/** The client running in a tile. "shell" = no agent (a plain login shell). */
export type ClientType = "claude" | "codex" | "shell";

type RuntimeIdentity = Pick<
  CaptainClaimRecord | CrewRef,
  "provider" | "harness" | "providerSessionId"
>;

function clientFromIdentity(
  identity: RuntimeIdentity,
): Exclude<ClientType, "shell"> | null {
  const { provider, harness } = identity;
  if (provider && harness && provider !== harness) return null;
  return provider ?? harness ?? null;
}

function identityFromClaims(
  claims: Record<TerminalId, CaptainClaimRecord>,
  id: TerminalId,
): RuntimeIdentity | null {
  const captain = claims[id];
  if (captain) return captain;
  for (const claim of Object.values(claims)) {
    const crew = claim.crew.find((member) => member.terminalId === id);
    if (crew) return crew;
  }
  return null;
}

/** Durable provider identity for a commissioned Captain or Crew tile. */
export function authoritativeIdentityForTerminal(
  id: TerminalId,
): RuntimeIdentity | null {
  return identityFromClaims(useCaptain.getState().claims, id);
}

/** Reactive form used by UI surfaces that render provider-specific identity. */
export function useAuthoritativeIdentityForTerminal(
  id: TerminalId,
): RuntimeIdentity | null {
  return useCaptain((s) => identityFromClaims(s.claims, id));
}

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
 * Precedence:
 * 1. DURABLE REGISTRY IDENTITY - commissioned Captain and Crew records declare
 *    their provider/harness independently of the foreground process name.
 * 2. AUTHORITATIVE TITLE — the strongest process signal. If the title's basename
 *    (split on `/` and `\`, last segment) is EXACTLY `claude` or `codex`, the
 *    foreground command IS that client; classify decisively. This beats a label
 *    that merely mentions the other client.
 * 3. WORD-BOUNDARY FALLBACK — otherwise scan the full haystack (title + label +
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
 * - An unregistered CLI whose `pane_current_command` is `node` with no provider
 *   word in any heuristic signal reads as "shell". Commissioned sessions use
 *   durable identity and are unaffected.
 */
/** The detection core, against an ALREADY-snapshotted workspace state. Split out
 *  so callers iterating every terminal (e.g. {@link useHasCodexSession}) can run a
 *  single pass over one state object instead of a `getState()` per terminal. */
function clientFromState(
  s: ReturnType<typeof useWorkspace.getState>,
  id: TerminalId,
): ClientType {
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

export function clientForTerminal(id: TerminalId): ClientType {
  const identity = authoritativeIdentityForTerminal(id);
  const authoritative = identity ? clientFromIdentity(identity) : null;
  if (authoritative) return authoritative;
  return clientFromState(useWorkspace.getState(), id);
}

/** Reactive client classification for headers and navigation rows. */
export function useClientForTerminal(id: TerminalId): ClientType {
  const identity = useAuthoritativeIdentityForTerminal(id);
  const heuristic = useWorkspace((s) => clientFromState(s, id));
  return (identity && clientFromIdentity(identity)) || heuristic;
}

/**
 * True when ANY open tile is running Codex — i.e. "Codex is in use". Used to GATE
 * Codex usage polling: when no Codex session is open we don't spawn the WSL read
 * (the cached, time-advanced last-known value still shows). Subscribes to the
 * workspace and Captain stores, so it re-evaluates when a tile's title/label or
 * durable provider identity changes.
 */
export function useHasCodexSession(): boolean {
  const claims = useCaptain((s) => s.claims);
  return useWorkspace((s) =>
    // Single pass over the SAME snapshot — no `getState()` per terminal.
    Object.keys(s.terminals).some((id) => {
      const identity = identityFromClaims(claims, id);
      const authoritative = identity ? clientFromIdentity(identity) : null;
      return (authoritative ?? clientFromState(s, id as TerminalId)) === "codex";
    }),
  );
}
