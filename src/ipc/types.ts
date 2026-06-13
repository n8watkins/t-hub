// Shared IPC contract between the React frontend and the Rust/Tauri backend.
//
// `Commands` values are the exact `#[tauri::command]` identifiers; `Events`
// values are the exact channels emitted from Rust. This file is the single
// source of truth for the 0.1 terminal nucleus and must stay in lockstep with
// src-tauri/src/commands.rs (Rust structs there use `rename_all = "camelCase"`).

export type TerminalId = string;

export type TerminalState =
  | "starting"
  | "live"
  | "detached"
  | "exited"
  | "error";

export interface SpawnOptions {
  /** Working directory to launch in (a WSL path on Windows, native path on Unix). */
  cwd?: string;
  /** Optional shell/command preset. Defaults to the user's login shell. */
  shell?: string;
  /** Optional human-readable label. */
  name?: string;
}

export interface TerminalInfo {
  id: TerminalId;
  /** tmux session name on the isolated `termhub` socket. */
  tmuxSession: string;
  cwd: string;
  title: string;
  state: TerminalState;
}

/** Tauri command names (used with `invoke`). */
export const Commands = {
  spawnTerminal: "spawn_terminal",
  /** (Re)attach a PTY client to a tmux session; returns base64 scrollback to seed xterm. */
  attachTerminal: "attach_terminal",
  writeTerminal: "write_terminal",
  resizeTerminal: "resize_terminal",
  /** Detach the tile but keep the tmux process alive. */
  closeTerminal: "close_terminal",
  /** Stop: terminate the tmux session and its process. */
  killTerminal: "kill_terminal",
  listTerminals: "list_terminals",
} as const;

/** Event channels emitted from the backend (payloads below). */
export const Events = {
  output: "terminal://output",
  state: "terminal://state",
  exit: "terminal://exit",
} as const;

// ---------------------------------------------------------------------------
// 0.5 additions — agent bridge, supervision, status (Workstreams A/B/C).
//
// These mirror `src-tauri/src/commands_05.rs` (command names) and the event
// channels the core fans out from the WSL journal spine. Payload *types* live
// in ./model and ./protocol (mirroring src-tauri/src/model.rs and the
// termhub-protocol crate). Keep this in lockstep with those Rust files.
// ---------------------------------------------------------------------------

/** 0.5 Tauri command names (used with `invoke`). */
export const Commands05 = {
  /** Core↔agent connection state + journal cursor. */
  agentState: "agent_state",
  /** WSL host metrics snapshot (RAM/swap/CPU/load/...). */
  hostMetrics: "host_metrics",
  /** Derive the current git branch for a cwd (statusline lacks it). */
  gitBranch: "git_branch",
  /** Read-only orchestrator→subagent tree for one session. */
  supervisionTree: "supervision_tree",
  /** All supervised session ids. */
  supervisionSessionIds: "supervision_session_ids",
  /** FR-012 status for one session. */
  sessionStatus: "session_status",
  /** Latest statusline snapshot for a session (may be null). */
  statusSnapshot: "status_snapshot",
  /** Push a raw statusline payload into the status bridge. */
  ingestStatus: "ingest_status",
} as const;

/**
 * 0.5 event channels the core emits as it consumes the WSL journal spine and
 * agent stream. Payloads are in ./protocol / ./model.
 *
 * NOTE: these are the *intended* channels for the agent-bridge subagent to emit
 * from the journal reader. The command surface above already works; live event
 * emission lights up with the transport.
 */
export const Events05 = {
  /** A durable journal entry arrived (streamed or replayed). → JournalEvent */
  journal: "agent://journal",
  /** A supervision tree snapshot changed for a session. → SupervisionTree */
  supervision: "supervision://tree",
  /** A session's FR-012 status changed. → SessionStatusEvent */
  sessionStatus: "session://status",
  /** The core↔agent connection state changed. → AgentStateInfo */
  agentState: "agent://state",
  /** A new statusline snapshot was ingested. → StatusSnapshot */
  status: "status://snapshot",
} as const;

export interface OutputEvent {
  id: TerminalId;
  /** base64-encoded raw PTY bytes (binary-safe across UTF-8 boundaries). */
  base64: string;
}

export interface StateEvent {
  id: TerminalId;
  state: TerminalState;
}

export interface ExitEvent {
  id: TerminalId;
  code: number | null;
}
