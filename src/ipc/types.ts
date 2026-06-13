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
