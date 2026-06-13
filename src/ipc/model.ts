// TermHub 0.5 data-model types — the TypeScript mirror of
// `src-tauri/src/model.rs` (and the status types from `src/claude/status.rs`).
// All Rust structs use `rename_all = "camelCase"`, so these interfaces use
// camelCase keys verbatim. Keep field-for-field in lockstep with the Rust side.

// --- Status model (FR-012, PLAN.md §D) -------------------------------------

/**
 * The session/agent status surfaced in the UI. The headline 0.5 addition is
 * `waitingOnSubagents` — a main agent whose `Stop` fired while `agent_id`
 * children / tasks remain outstanding.
 */
export type SessionStatus =
  | "working"
  | "waitingOnSubagents"
  | "needsQuestion"
  | "needsPermission"
  | "completed"
  | "failed"
  | "rateLimited"
  | "detached"
  | "restoring"
  | "expired"
  | "unknown";

// --- AgentSessionRecord (PRD §8) -------------------------------------------

export type Resumability = "resumable" | "expired" | "unknown";

export type LiveAttachmentState =
  | "free"
  | "liveInTermhub"
  | "liveExternally"
  | "stale";

/** A discovered/owned Claude Code session (PRD §8). */
export interface AgentSessionRecord {
  provider: string;
  /** Exact session id captured at SessionStart (Claude's `session_id`). */
  providerSessionId: string;
  terminalId?: string;
  projectId?: string;
  displayName: string;
  summary?: string;
  transcriptPath?: string;
  createdAt: number;
  lastActivityAt: number;
  /** Context window used %, from the status bridge (0..=100). */
  contextUsedPct?: number;
  resumability: Resumability;
  liveAttachmentState: LiveAttachmentState;
  status: SessionStatus;
  /** Free-form provider metadata (raw status fields, rate-limit block, ...). */
  providerMetadata: unknown;
}

// --- Subagent supervision (PLAN.md §C) -------------------------------------

export type SubagentState = "running" | "completed";

/** A node in the orchestrator→subagent tree (created on SubagentStart). */
export interface SubagentNode {
  parentSessionId: string;
  agentId: string;
  agentType?: string;
  state: SubagentState;
  startedAt: number;
  endedAt?: number;
}

/** The read-only tree view payload for one orchestrator (sidebar/tile detail). */
export interface SupervisionTree {
  sessionId: string;
  status: SessionStatus;
  children: SubagentNode[];
  /** Outstanding background tasks (`TaskCreated` − `TaskCompleted`). */
  outstandingTasks: number;
}

// --- Snapshot-track schema: tabs, terminals, projects (PRD §8) --------------

export type LayoutMode = "grid";

/** A workspace tab (PLAN.md §F). */
export interface WorkspaceTab {
  id: string;
  name: string;
  order: number;
  layoutMode: LayoutMode;
  /** Opaque layout payload (grid order / geometry) owned by the frontend. */
  layoutJson: unknown;
  zoomDefault: number;
}

export type CloseBehavior = "detach" | "kill";
export type RecoveryPolicy = "review" | "auto" | "never";

/** A persisted terminal record (PRD §8 snapshot track). */
export interface TerminalRecord {
  id: string;
  tabId: string;
  tmuxServer: string;
  tmuxSession: string;
  projectId?: string;
  cwd: string;
  shell?: string;
  state: string;
  lastSeenAt: number;
  closeBehavior: CloseBehavior;
  recoveryPolicy: RecoveryPolicy;
  customCommand?: string;
}

/** A minimal project anchor (PRD §8; full file index is 1.0). */
export interface ProjectRecord {
  id: string;
  rootPath: string;
  repoRoot: string;
  displayName: string;
  distro?: string;
}

/** The outcome of a Claude hook install/uninstall (mirrors install.rs). */
export interface InstallReport {
  settingsPath: string;
  backedUp: boolean;
  managedEvents: number;
  message: string;
}

// --- Status bridge snapshot (src/claude/status.rs) -------------------------

/** One rate-limit window from the statusline `rate_limits` block. */
export interface RateLimitWindow {
  /** Unix-epoch seconds the window resets (undefined until known). */
  resetsAt?: number;
  /** Percentage of the window used (0..=100), when reported. */
  usedPercentage?: number;
}

/**
 * A normalized statusline snapshot keyed by exact session id. Every field is
 * optional so absent blocks degrade gracefully. `rateLimitsPresent === false`
 * means free tier or pre-first-API-response — treat reset time as unknown
 * (REVIEW / PRD §6.10 caveat).
 */
export interface StatusSnapshot {
  sessionId: string;
  contextUsedPct?: number;
  costUsd?: number;
  fiveHour?: RateLimitWindow;
  sevenDay?: RateLimitWindow;
  rateLimitsPresent: boolean;
  ingestedAtMs: number;
}
