// Typed wrappers over the 0.5 IPC surface (agent bridge, supervision, status).
// Kept separate from ./client (the 0.1 nucleus) so the terminal contract stays
// untouched. Mirrors `Commands05` / `Events05` in ./types and the payload types
// in ./model and ./protocol.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Commands05, Events05 } from "./types";
import type {
  InstallReport,
  SessionStatus,
  StatusSnapshot,
  SupervisionTree,
} from "./model";
import type {
  AgentStateInfo,
  HostMetrics,
  JournalEvent,
  SessionStatusEvent,
} from "./protocol";

// --- Commands --------------------------------------------------------------

/** Core↔agent connection state + journal cursor (for the health area). */
export function agentState(): Promise<AgentStateInfo> {
  return invoke(Commands05.agentState);
}

/** WSL host metrics snapshot. Rejects until the agent bridge is connected. */
export function hostMetrics(): Promise<HostMetrics> {
  return invoke(Commands05.hostMetrics);
}

/** Derive the current git branch for `cwd` (statusline lacks it). */
export function gitBranch(cwd: string): Promise<string | null> {
  return invoke(Commands05.gitBranch, { cwd });
}

/** Page a tile's tmux scrollback (copy-mode). `session` is the tmux session name
 *  (`th_<terminalId>`); `down` pages toward the live prompt. */
export function tmuxScroll(session: string, down: boolean): Promise<void> {
  return invoke(Commands05.tmuxScroll, { session, down });
}

/** Exit a tile's tmux copy-mode (back to the live prompt). */
export function tmuxExitScroll(session: string): Promise<void> {
  return invoke(Commands05.tmuxExitScroll, { session });
}

/** If the clipboard holds an image, save it to a temp PNG and resolve to that
 *  file's NATIVE path (a Windows path in the packaged app); resolves to null
 *  when there's no image, so the caller can fall back to a text paste. */
export function clipboardImageToTemp(): Promise<string | null> {
  return invoke(Commands05.clipboardImageToTemp);
}

/** Read-only orchestrator→subagent tree for one session (null if unseen). */
export function supervisionTree(
  sessionId: string,
): Promise<SupervisionTree | null> {
  return invoke(Commands05.supervisionTree, { sessionId });
}

/** All supervised session ids. */
export function supervisionSessionIds(): Promise<string[]> {
  return invoke(Commands05.supervisionSessionIds);
}

/** FR-012 status for one session. */
export function sessionStatus(sessionId: string): Promise<SessionStatus> {
  return invoke(Commands05.sessionStatus, { sessionId });
}

/** Latest statusline snapshot for a session (null if none ingested yet). */
export function statusSnapshot(
  sessionId: string,
): Promise<StatusSnapshot | null> {
  return invoke(Commands05.statusSnapshot, { sessionId });
}

/** Push a raw statusline payload into the status bridge; returns the snapshot. */
export function ingestStatus(
  sessionId: string,
  payload: unknown,
): Promise<StatusSnapshot> {
  return invoke(Commands05.ingestStatus, { sessionId, payload });
}

/** Install TermHub hooks into ~/.claude/settings.json. `consent` must be true.
 *  `events` is the chosen subset; the managed set is reconciled to exactly it
 *  (an empty array means "all"). */
export function installClaudeHooks(
  agentBin: string,
  consent: boolean,
  events: string[],
): Promise<InstallReport> {
  return invoke(Commands05.installClaudeHooks, { agentBin, consent, events });
}

/** Remove TermHub hooks (clean uninstall). */
export function uninstallClaudeHooks(): Promise<InstallReport> {
  return invoke(Commands05.uninstallClaudeHooks);
}

/** Whether TermHub hooks are currently installed. */
export function claudeHooksInstalled(): Promise<boolean> {
  return invoke(Commands05.claudeHooksInstalled);
}

/** Which hook events TermHub currently manages (to pre-check the checklist). */
export function claudeHooksManaged(): Promise<string[]> {
  return invoke(Commands05.claudeHooksManaged);
}

// --- Events ----------------------------------------------------------------

/** Subscribe to durable journal entries the core consumes from the spine. */
export function onJournal(cb: (e: JournalEvent) => void): Promise<UnlistenFn> {
  return listen<JournalEvent>(Events05.journal, (ev) => cb(ev.payload));
}

/** Subscribe to supervision-tree snapshot changes. */
export function onSupervision(
  cb: (e: SupervisionTree) => void,
): Promise<UnlistenFn> {
  return listen<SupervisionTree>(Events05.supervision, (ev) => cb(ev.payload));
}

/** Subscribe to per-session FR-012 status changes. */
export function onSessionStatus(
  cb: (e: SessionStatusEvent) => void,
): Promise<UnlistenFn> {
  return listen<SessionStatusEvent>(Events05.sessionStatus, (ev) =>
    cb(ev.payload),
  );
}

/** Subscribe to core↔agent connection state changes. */
export function onAgentState(
  cb: (e: AgentStateInfo) => void,
): Promise<UnlistenFn> {
  return listen<AgentStateInfo>(Events05.agentState, (ev) => cb(ev.payload));
}

/**
 * A status snapshot as it arrives on the wire, WITH the extra correlation fields
 * the backend now includes on it (src/claude/status.rs). Declared here as an
 * augmentation rather than widening the shared `StatusSnapshot` in ipc/model.ts,
 * so the per-tile context meter that needs them stays self-contained + revertible.
 *
 * Binding fields (all optional — absent ones degrade the meter, never break it):
 *   - `tmuxSession`: the tmux session NAME (`th_<terminalId>`) that owns the pane
 *     the statusline ran inside, resolved by the agent from `$TMUX_PANE`. This is
 *     the ROBUST tile↔session key — a tile computes its own `th_<id>` and looks
 *     itself up by it (see store/sessionContext.ts), so two tiles in the same
 *     directory no longer collide.
 *   - `tmuxPane`: the raw `$TMUX_PANE` id (e.g. `%37`); diagnostic / underlying
 *     signal the session name was resolved from.
 *   - `cwd`: the session's working directory — the FALLBACK match used only when
 *     `tmuxSession` is absent (un-upgraded agent / not under tmux).
 */
export type StatusSnapshotWire = StatusSnapshot & {
  cwd?: string;
  tmuxPane?: string;
  tmuxSession?: string;
};

/** Subscribe to new statusline snapshots (carrying the session cwd). */
export function onStatus(
  cb: (e: StatusSnapshotWire) => void,
): Promise<UnlistenFn> {
  return listen<StatusSnapshotWire>(Events05.status, (ev) => cb(ev.payload));
}
