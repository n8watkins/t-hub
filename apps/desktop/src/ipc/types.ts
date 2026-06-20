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
  /**
   * Optional command run in the new pane after the login shell starts (the "+"
   * spawn presets — e.g. `claude`, `claude --resume`, or a Custom… line). Run
   * INSIDE a login shell the pane execs back into, so exiting the command drops
   * to a live shell instead of closing the tile. Empty/omitted => plain login
   * shell (the "Shell" preset = today's behavior, no regression).
   */
  startupCommand?: string;
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
  /** Scroll a tile's tmux scrollback by a page (copy-mode). */
  tmuxScroll: "tmux_scroll",
  /** Exit a tile's tmux copy-mode (back to the live prompt). */
  tmuxExitScroll: "tmux_exit_scroll",
  /** Save a pasted clipboard image to a temp PNG; returns its native path (or
   *  null when the clipboard holds no image). */
  clipboardImageToTemp: "clipboard_image_to_temp",
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
  /** Install TermHub hooks into ~/.claude/settings.json (consent-gated). */
  installClaudeHooks: "install_claude_hooks",
  /** Remove TermHub hooks (clean uninstall). */
  uninstallClaudeHooks: "uninstall_claude_hooks",
  /** Whether TermHub hooks are currently installed. */
  claudeHooksInstalled: "claude_hooks_installed",
  /** Which hook events TermHub currently manages (for the install checklist). */
  claudeHooksManaged: "claude_hooks_managed",
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

// ---------------------------------------------------------------------------
// Files — index + fuzzy search + shallow tree + capped reader (PRD §6.8/§9.7;
// FR-014/015/016/017). Mirrors `src-tauri/src/files.rs` (all structs there use
// `rename_all = "camelCase"`). Typed wrappers live in ./files. Keep in lockstep.
// ---------------------------------------------------------------------------

/** File-index Tauri command names (used with `invoke`). */
export const CommandsFiles = {
  /** Walk a project root and build/refresh the in-memory index. → IndexSummary */
  indexProject: "index_project",
  /** Fuzzy basename/path/extension search over the index. → FileHit[] */
  searchFiles: "search_files",
  /** Shallow directory listing for the tree (no recursion). → DirEntry[] */
  listDir: "list_dir",
  /** Read a text file for the reader (capped, rejects binary). → FileContents */
  readTextFile: "read_text_file",
  /** Overwrite a file with new text (the editor's save). → void */
  writeTextFile: "write_text_file",
} as const;

/** Summary returned by `index_project` (the index itself stays in the backend). */
export interface IndexSummary {
  /** The normalized root that was indexed. */
  root: string;
  /** Number of files in the index. */
  count: number;
}

/** A ranked search hit from `search_files`. */
export interface FileHit {
  /** Path relative to the indexed root, `/`-separated. */
  relPath: string;
  /** Final path component (e.g. `lib.rs`). */
  basename: string;
  /** Lowercased extension without the dot (e.g. `rs`), or `""`. */
  ext: string;
  /** True for high-signal project files (README, package.json, ...). */
  isKeyFile: boolean;
  /** Opaque ranking score; higher is a better match. */
  score: number;
}

/** One shallow directory entry from `list_dir`. */
export interface DirEntry {
  /** Final path component. */
  name: string;
  /** Absolute path to this entry. */
  path: string;
  isDir: boolean;
  /** File size in bytes (0 for directories). */
  size: number;
}

/** The capped result of `read_text_file`. */
export interface FileContents {
  path: string;
  /** Lowercased extension without the dot (drives Markdown-vs-plain rendering). */
  ext: string;
  /** Decoded UTF-8 text (lossy for stray non-UTF-8 bytes). */
  text: string;
  /** True if the file exceeded the read cap and `text` is a prefix. */
  truncated: boolean;
  /** Total size of the file on disk, in bytes. */
  size: number;
}
