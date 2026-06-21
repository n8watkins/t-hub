// TypeScript mirror of the subset of the `t-hub-protocol` crate that the core
// forwards to the UI: host metrics, worktree info, and the durable journal
// entry. The frontend never speaks the NDJSON wire protocol directly (the core
// does), but it renders these payloads, so they are part of the IPC contract.
//
// Rust side uses default serde naming for the protocol crate (snake_case keys),
// EXCEPT the journal/host types below, which the core passes through verbatim.
// These therefore use snake_case to match `t-hub-protocol`'s on-wire shape.
// Keep in lockstep with `src-tauri/crates/t-hub-protocol/src/lib.rs`.

// --- Host metrics (HostMetrics) --------------------------------------------

/** A snapshot of WSL host health (memory values in KiB). */
export interface HostMetrics {
  mem_total_kib: number;
  mem_available_kib: number;
  swap_total_kib: number;
  swap_free_kib: number;
  cpu_count: number;
  /** 1/5/15-minute load averages. */
  load_avg: [number, number, number];
  process_count: number;
  distro?: string;
  /** Unix-epoch ms the snapshot was taken (agent clock). */
  captured_at_ms: number;
}

/** One entry from `git worktree list --porcelain`. */
export interface WorktreeInfo {
  path: string;
  branch?: string;
  head?: string;
  bare: boolean;
  detached: boolean;
}

// --- Event journal (EventJournalEntry) -------------------------------------

/** Who/what produced a journal entry. */
export type JournalSource = "hook" | "status" | "agent" | "core" | "unknown";

/**
 * The kind of event a journal entry records. The hook-derived values use the
 * EXACT Claude Code hook names (verified, REVIEW §9.6); non-hook values cover
 * the agent/core/status sources.
 */
export type JournalEventType =
  // Claude Code hooks (verified names)
  | "sessionStart"
  | "sessionEnd"
  | "userPromptSubmit"
  | "stop"
  | "stopFailure"
  | "permissionRequest"
  | "notification"
  | "elicitation"
  | "subagentStart"
  | "subagentStop"
  | "taskCreated"
  | "taskCompleted"
  | "cwdChanged"
  | "worktreeCreate"
  | "worktreeRemove"
  // status bridge
  | "statusSnapshot"
  // agent / core lifecycle + actions
  | "agentConnected"
  | "agentCommand"
  | "coreAction"
  | "unknown";

/**
 * A single durable entry in the WSL-side append-only event journal (PRD §8).
 * The journal survives the Windows app closing and is replayed on reconnect; it
 * is the authority for reconstruction intent, not live process state.
 */
export interface EventJournalEntry {
  /** Monotonic sequence assigned by the agent on append (1-based). */
  seq: number;
  /** Unix-epoch ms the event was recorded (agent clock). */
  timestamp_ms: number;
  source: JournalSource;
  /** Primary entity: usually a Claude session_id / subagent agent_id / tmux name. */
  entity_id?: string;
  event_type: JournalEventType;
  /** Arbitrary structured payload (raw hook stdin, status JSON, command args). */
  payload: unknown;
  /** Outcome of a recorded action (command success/failure), if any. */
  result?: string;
}

// --- Core-side connection + event payloads ---------------------------------

/** Core↔agent connection lifecycle (mirrors agent::ConnectionState). */
export type ConnectionState =
  | "disconnected"
  | "handshaking"
  | "replaying"
  | "live"
  | "reconnecting"
  | "failed";

/** Payload of the `agent_state` command / `agent://state` event. */
export interface AgentStateInfo {
  connection: ConnectionState;
  journalCursor: number;
}

/** Payload of the `agent://journal` event: a journal entry the core consumed. */
export interface JournalEvent {
  entry: EventJournalEntry;
}

/** Payload of the `session://status` event. */
export interface SessionStatusEvent {
  sessionId: string;
  status: import("./model").SessionStatus;
}
