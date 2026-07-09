// Typed wrappers over the 0.5 IPC surface (agent bridge, supervision, status).
// Kept separate from ./client (the 0.1 nucleus) so the terminal contract stays
// untouched. Mirrors `Commands05` / `Events05` in ./types and the payload types
// in ./model and ./protocol.

import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Commands05, Events05 } from "./types";
import { controlRequest, onControlEvent } from "./controlClient";
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

/**
 * WSL host metrics snapshot.
 *
 * Server-split M3 (overlay source #5): routed over the control socket
 * (`host_metrics` in control.rs) instead of the in-process Tauri command —
 * shape-identical (snake_case `HostMetrics`), so it's a transport swap. The
 * daemon prefers the agent bridge's `/proc` (the WSL agent), so locally this is a
 * no-op; a thin client now gets the REMOTE host's metrics. Still rejects until the
 * bridge is connected (the daemon's local `/proc` is the Windows host = zeros, so
 * we surface the "not connected" error rather than zeros — Linux daemons fall back
 * to their own real `/proc`).
 */
export function hostMetrics(): Promise<HostMetrics> {
  return controlRequest("host_metrics") as Promise<HostMetrics>;
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

/** Read-only orchestrator→subagent tree for one session (null if unseen).
 *
 *  Server-split M1: routed over the loopback control socket (control.rs's
 *  `supervision_tree`) instead of the in-process Tauri command. Shape-identical
 *  (`Option<SupervisionTree>`), so this is a transport swap with zero user-visible
 *  change — the first command migrated onto the wire M2 stretches to remote. */
export function supervisionTree(
  sessionId: string,
): Promise<SupervisionTree | null> {
  return controlRequest("supervision_tree", { sessionId }) as Promise<
    SupervisionTree | null
  >;
}

/** All supervised session ids. Server-split M1: over the control socket. */
export function supervisionSessionIds(): Promise<string[]> {
  return controlRequest("supervision_session_ids") as Promise<string[]>;
}

/** FR-012 status for one session. Server-split M1: over the control socket. The
 *  channel's `get_status` returns `{ status, snapshot }`; we take `status`. */
export function sessionStatus(sessionId: string): Promise<SessionStatus> {
  return controlRequest("get_status", { sessionId }).then(
    (r) => (r as { status: SessionStatus }).status,
  );
}

/** Latest statusline snapshot for a session (null if none ingested yet).
 *  Server-split M1: over the control socket, via `get_status`'s `snapshot`. */
export function statusSnapshot(
  sessionId: string,
): Promise<StatusSnapshot | null> {
  return controlRequest("get_status", { sessionId }).then(
    (r) => (r as { snapshot: StatusSnapshot | null }).snapshot ?? null,
  );
}

/** Push a raw statusline payload into the status bridge; returns the snapshot. */
export function ingestStatus(
  sessionId: string,
  payload: unknown,
): Promise<StatusSnapshot> {
  return invoke(Commands05.ingestStatus, { sessionId, payload });
}

/** Install T-Hub hooks into ~/.claude/settings.json. `consent` must be true.
 *  `events` is the chosen subset; the managed set is reconciled to exactly it
 *  (an empty array means "all"). */
export function installClaudeHooks(
  agentBin: string,
  consent: boolean,
  events: string[],
): Promise<InstallReport> {
  return invoke(Commands05.installClaudeHooks, { agentBin, consent, events });
}

/** Remove T-Hub hooks (clean uninstall). */
export function uninstallClaudeHooks(): Promise<InstallReport> {
  return invoke(Commands05.uninstallClaudeHooks);
}

/** Whether T-Hub hooks are currently installed. */
export function claudeHooksInstalled(): Promise<boolean> {
  return invoke(Commands05.claudeHooksInstalled);
}

/** Which hook events T-Hub currently manages (to pre-check the checklist). */
export function claudeHooksManaged(): Promise<string[]> {
  return invoke(Commands05.claudeHooksManaged);
}

// --- Events ----------------------------------------------------------------
//
// Server-split M1: the whole 0.5 event surface is delivered over the loopback
// control socket. The backend Tee-emits every channel to the socket fanout; the
// Rust forwarder reads the frames off the socket and re-emits them into the
// webview as `control://event`, which `onControlEvent` demuxes by channel. The
// in-process Tauri `emit` for these channels still fires but has no listener, so
// there is no double-delivery. This is the wire M2 stretches to a remote server —
// only the Rust endpoint's address changes.

/** Subscribe to durable journal entries the core consumes from the spine. */
export function onJournal(cb: (e: JournalEvent) => void): Promise<UnlistenFn> {
  return Promise.resolve(
    onControlEvent(Events05.journal, (p) => cb(p as JournalEvent)),
  );
}

/** Subscribe to supervision-tree snapshot changes. */
export function onSupervision(
  cb: (e: SupervisionTree) => void,
): Promise<UnlistenFn> {
  return Promise.resolve(
    onControlEvent(Events05.supervision, (p) => cb(p as SupervisionTree)),
  );
}

/** Subscribe to per-session FR-012 status changes. */
export function onSessionStatus(
  cb: (e: SessionStatusEvent) => void,
): Promise<UnlistenFn> {
  return Promise.resolve(
    onControlEvent(Events05.sessionStatus, (p) => cb(p as SessionStatusEvent)),
  );
}

/** Subscribe to core↔agent connection state changes. */
export function onAgentState(
  cb: (e: AgentStateInfo) => void,
): Promise<UnlistenFn> {
  return Promise.resolve(
    onControlEvent(Events05.agentState, (p) => cb(p as AgentStateInfo)),
  );
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
 *     the ONLY tile↔session key - a tile computes its own `th_<id>` and looks
 *     itself up by it (see store/sessionContext.ts), so two tiles in the same
 *     directory can never collide. A reading with no `tmuxSession` is dropped.
 *   - `tmuxPane`: the raw `$TMUX_PANE` id (e.g. `%37`); diagnostic / underlying
 *     signal the session name was resolved from.
 *   - `cwd`: the session's working directory. NOT used to bind the meter to a
 *     tile (a shared cwd once leaked across same-folder tiles); carried for the
 *     backend restore map only.
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
  return Promise.resolve(
    onControlEvent(Events05.status, (p) => cb(p as StatusSnapshotWire)),
  );
}
