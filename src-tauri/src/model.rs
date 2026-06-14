//! TermHub 0.5 data model (PRD §8, PLAN.md "Data-model additions").
//!
//! These are the persisted/serialized records the 0.5 features hang off. They
//! mirror the TypeScript interfaces in `src/ipc/model.ts` field-for-field
//! (`rename_all = "camelCase"`). For 0.5 these are the in-memory + IPC shapes;
//! the SQLite schema that backs them (WAL + explicit `synchronous`, per the
//! REVIEW durability item) lands with the persistence workstream (G) — the
//! struct definitions here are the stable contract that work builds against.
//!
//! Boundaries for parallel work:
//!   - This file defines **types only** (plus trivial constructors/derives). It
//!     does not own behavior; supervision/claude/agent modules consume it.
//!   - Do not add I/O or business logic here — keep it a pure data contract.

// This module is a pure data contract: many of these records are constructed by
// the persistence/recovery/supervision-UI workstreams (subagents), not yet from
// within the crate. Allow unused until those land — the *shape* is the point.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Status model (FR-012, PLAN.md §D)
// ---------------------------------------------------------------------------

/// The session/agent status surfaced in the UI (PLAN.md §D status table). The
/// headline 0.5 addition is [`SessionStatus::WaitingOnSubagents`] — a main agent
/// whose `Stop` fired while `agent_id` children / tasks remain outstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    /// Active turn; `UserPromptSubmit` seen, no terminal `Stop` yet.
    Working,
    /// Main agent `Stop` fired **while** `agent_id` children / tasks remain.
    WaitingOnSubagents,
    /// `Elicitation` (preferred) / `Notification` — agent needs an answer.
    NeedsQuestion,
    /// `PermissionRequest` — agent needs an allow/deny.
    NeedsPermission,
    /// `Stop` with no outstanding subagents/tasks.
    Completed,
    /// `StopFailure` / abnormal `SessionEnd` / non-zero terminal exit.
    Failed,
    /// Statusline `rate_limits.*` near limit / blocked turn.
    RateLimited,
    /// Tile closed, tmux alive (`TerminalState=detached`).
    Detached,
    /// Recovery in progress.
    Restoring,
    /// Transcript missing on a resumability check.
    Expired,
    /// Initial/unknown — no signal yet.
    Unknown,
}

impl Default for SessionStatus {
    fn default() -> Self {
        SessionStatus::Unknown
    }
}

// ---------------------------------------------------------------------------
// AgentSessionRecord (PRD §8)
// ---------------------------------------------------------------------------

/// Resumability of a Claude session against `~/.claude/projects/.../<id>.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resumability {
    /// Transcript present; `-r <id>` should work.
    Resumable,
    /// Transcript cleaned up / moved; resume will fail.
    Expired,
    /// Not yet checked.
    Unknown,
}

impl Default for Resumability {
    fn default() -> Self {
        Resumability::Unknown
    }
}

/// The live-attachment lease state (PLAN.md §E "lease, not latch"). A crash with
/// no `SessionEnd` must reconcile from [`LiveAttachmentState::LiveExternally`] /
/// `LiveInTermhub` back to `Free`, not strand the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LiveAttachmentState {
    /// No live attachment known.
    Free,
    /// A TermHub-owned terminal currently holds this session.
    LiveInTermhub,
    /// A non-TermHub `claude` fired `SessionStart` for this id (detected via the
    /// global hook → journal path). Resume should offer Focus/Fork.
    LiveExternally,
    /// Lease expired / under reconciliation (heartbeat TTL elapsed).
    Stale,
}

impl Default for LiveAttachmentState {
    fn default() -> Self {
        LiveAttachmentState::Free
    }
}

/// A discovered/owned Claude Code session (PRD §8). Populated incrementally:
/// `SessionStart` creates it with the exact `provider_session_id`; the status
/// bridge fills `context_used_pct`; supervision tracks status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRecord {
    /// Adapter/provider id (only `"claude"` in 0.5).
    pub provider: String,
    /// The exact session id captured at `SessionStart` (Claude's `session_id`).
    pub provider_session_id: String,
    /// The TermHub terminal currently hosting it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// The owning project (anchor), if resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Resolved display name (see label priority, PLAN.md §F).
    pub display_name: String,
    /// Claude's own summary, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `~/.claude/projects/<project>/<id>.jsonl`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Epoch-ms created (first `SessionStart`).
    pub created_at: u64,
    /// Epoch-ms of last observed activity.
    pub last_activity_at: u64,
    /// Context window used %, from the statusline status bridge (0..=100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_used_pct: Option<f32>,
    /// Resumability against the transcript path.
    pub resumability: Resumability,
    /// The live-attachment lease state.
    pub live_attachment_state: LiveAttachmentState,
    /// Current status in the FR-012 model.
    pub status: SessionStatus,
    /// Free-form provider metadata (raw status fields, rate-limit block, etc.).
    #[serde(default)]
    pub provider_metadata: serde_json::Value,
}

impl AgentSessionRecord {
    /// Construct a fresh record for a session first seen at `SessionStart`.
    pub fn new(provider_session_id: impl Into<String>, created_at: u64) -> Self {
        let id = provider_session_id.into();
        Self {
            provider: "claude".to_string(),
            display_name: id.clone(),
            provider_session_id: id,
            terminal_id: None,
            project_id: None,
            summary: None,
            transcript_path: None,
            created_at,
            last_activity_at: created_at,
            context_used_pct: None,
            resumability: Resumability::Unknown,
            live_attachment_state: LiveAttachmentState::Free,
            status: SessionStatus::Working,
            provider_metadata: serde_json::Value::Null,
        }
    }
}

// ---------------------------------------------------------------------------
// Subagent supervision (PLAN.md §C — the pulled-forward tree)
// ---------------------------------------------------------------------------

/// Per-subagent state keyed by `agent_id` (from `SubagentStart`/`SubagentStop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentState {
    Running,
    Completed,
}

/// A node in the orchestrator→subagent tree (PLAN.md §C data-model addition).
/// Created on `SubagentStart` under the owning `session_id`; closed on
/// `SubagentStop`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentNode {
    /// The orchestrator session that owns this subagent (parent `session_id`).
    pub parent_session_id: String,
    /// The subagent's `agent_id` (stable per subagent for its lifetime).
    pub agent_id: String,
    /// The subagent's `agent_type`, when provided by the hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    pub state: SubagentState,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
}

/// The read-only tree view payload for one orchestrator (PLAN.md §C "read-only
/// tree view in the sidebar/tile detail"). Sent to the UI as the supervision
/// snapshot for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisionTree {
    /// The orchestrator session id.
    pub session_id: String,
    /// Its derived status (notably `WaitingOnSubagents`).
    pub status: SessionStatus,
    /// Child subagents (running + finished).
    pub children: Vec<SubagentNode>,
    /// Count of outstanding background tasks (`TaskCreated` − `TaskCompleted`).
    pub outstanding_tasks: u32,
}

// ---------------------------------------------------------------------------
// Snapshot-track schema: tabs, terminals, projects (PRD §8)
// ---------------------------------------------------------------------------

/// Grid vs. (future) freeform layout for a workspace tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LayoutMode {
    Grid,
}

impl Default for LayoutMode {
    fn default() -> Self {
        LayoutMode::Grid
    }
}

/// A workspace tab (PLAN.md §F, PRD §8 snapshot track).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTab {
    pub id: String,
    pub name: String,
    /// Sort order among tabs.
    pub order: u32,
    pub layout_mode: LayoutMode,
    /// Opaque layout payload (grid order / geometry) owned by the frontend.
    #[serde(default)]
    pub layout_json: serde_json::Value,
    /// Default zoom for tiles on this tab.
    #[serde(default = "default_zoom")]
    pub zoom_default: f32,
}

fn default_zoom() -> f32 {
    1.0
}

/// What happens to the backing process when a tile is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseBehavior {
    /// Detach the tile, keep the tmux process (the 0.1 default).
    Detach,
    /// Kill the tmux session.
    Kill,
}

impl Default for CloseBehavior {
    fn default() -> Self {
        CloseBehavior::Detach
    }
}

/// Recovery policy after a WSL/Windows restart (PLAN.md §G reviewed recovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryPolicy {
    /// Offer in the recovery review; do nothing automatically.
    Review,
    /// Eligible for auto-recovery (resume conversation + restart shell).
    Auto,
    /// Never auto-recover.
    Never,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        RecoveryPolicy::Review
    }
}

/// A persisted terminal record (PRD §8 snapshot track). Distinct from the
/// runtime `commands::TerminalInfo`: this is the durable description used to
/// reattach/recover, not the live PTY handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecord {
    pub id: String,
    pub tab_id: String,
    /// Always the isolated `termhub` socket in 0.5.
    pub tmux_server: String,
    pub tmux_session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Last observed lifecycle state (mirrors the 0.1 `TerminalState` names).
    pub state: String,
    pub last_seen_at: u64,
    pub close_behavior: CloseBehavior,
    pub recovery_policy: RecoveryPolicy,
    /// A custom launch command, if this terminal runs something other than the
    /// login shell (e.g. a `claude --resume <id>` line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_command: Option<String>,
}

/// A minimal project anchor (PRD §8; full file index is 1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: String,
    pub root_path: String,
    pub repo_root: String,
    pub display_name: String,
    /// The WSL distro this project lives in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_record_camel_case_roundtrip() {
        let rec = AgentSessionRecord::new("sess-123", 1000);
        let json = serde_json::to_string(&rec).unwrap();
        // camelCase keys must be present (mirrors src/ipc/model.ts).
        assert!(json.contains("\"providerSessionId\":\"sess-123\""));
        assert!(json.contains("\"liveAttachmentState\":\"free\""));
        assert!(json.contains("\"status\":\"working\""));
        let back: AgentSessionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider_session_id, "sess-123");
        assert_eq!(back.status, SessionStatus::Working);
    }

    #[test]
    fn supervision_tree_serializes_children() {
        let tree = SupervisionTree {
            session_id: "s1".into(),
            status: SessionStatus::WaitingOnSubagents,
            children: vec![SubagentNode {
                parent_session_id: "s1".into(),
                agent_id: "a1".into(),
                agent_type: Some("general-purpose".into()),
                state: SubagentState::Running,
                started_at: 1,
                ended_at: None,
            }],
            outstanding_tasks: 2,
        };
        let json = serde_json::to_string(&tree).unwrap();
        assert!(json.contains("\"status\":\"waitingOnSubagents\""));
        assert!(json.contains("\"outstandingTasks\":2"));
        assert!(json.contains("\"agentId\":\"a1\""));
    }

    #[test]
    fn defaults_are_sane() {
        assert_eq!(SessionStatus::default(), SessionStatus::Unknown);
        assert_eq!(Resumability::default(), Resumability::Unknown);
        assert_eq!(LiveAttachmentState::default(), LiveAttachmentState::Free);
        assert_eq!(CloseBehavior::default(), CloseBehavior::Detach);
        assert_eq!(RecoveryPolicy::default(), RecoveryPolicy::Review);
    }
}
