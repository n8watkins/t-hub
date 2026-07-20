//! Powder-independent durable records for supervised agent sessions.
//!
//! This module intentionally contains no terminal, provider, registry, or
//! network code.  It is the stable data boundary used by the de-Powder control
//! contract while the legacy Crew representation remains readable elsewhere.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ASSIGNMENT_BYTES: usize = 16 * 1024;
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1024;
pub const MAX_EVENT_BATCH: usize = 128;
pub const MAX_CHECKPOINT_HISTORY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeState {
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "needsPermission")]
    NeedsPermission,
    #[serde(rename = "exited")]
    Exited,
    #[serde(rename = "unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkStage {
    #[serde(rename = "assigned")]
    Assigned,
    #[serde(rename = "working")]
    Working,
    #[serde(rename = "needsInput")]
    NeedsInput,
    #[serde(rename = "readyForReview")]
    ReadyForReview,
    #[serde(rename = "awaitingIntegration")]
    AwaitingIntegration,
    #[serde(rename = "complete")]
    Complete,
    #[serde(rename = "stopped")]
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSessionRecord {
    pub agent_session_id: String,
    pub captain_session_id: String,
    pub project_id: String,
    pub assignment: String,
    pub directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_tab_id: Option<String>,
    pub harness: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_point: Option<String>,
    pub runtime_state: RuntimeState,
    pub work_stage: WorkStage,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AgentSessionRecord {
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("agentSessionId", self.agent_session_id.as_str()),
            ("captainSessionId", self.captain_session_id.as_str()),
            ("projectId", self.project_id.as_str()),
            ("directory", self.directory.as_str()),
            ("harness", self.harness.as_str()),
            ("provider", self.provider.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("agent session {field} must not be empty"));
            }
        }
        if self.assignment.trim().is_empty() {
            return Err("agent session assignment must not be empty".into());
        }
        if self.assignment.len() > MAX_ASSIGNMENT_BYTES {
            return Err(format!(
                "agent session assignment must be at most {MAX_ASSIGNMENT_BYTES} bytes"
            ));
        }
        if !matches!(self.harness.as_str(), "codex" | "claude") || self.harness != self.provider {
            return Err("agent session harness and provider must both be codex or claude".into());
        }
        if self.updated_at < self.created_at {
            return Err("agent session updatedAt must not precede createdAt".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCheckpoint {
    pub cursor: u64,
    pub agent_session_id: String,
    pub author_session_id: String,
    pub summary: String,
    pub created_at: u64,
}

impl AgentCheckpoint {
    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err("agent checkpoint summary must not be empty".into());
        }
        if self.summary.len() > MAX_CHECKPOINT_BYTES {
            return Err(format!(
                "agent checkpoint summary must be at most {MAX_CHECKPOINT_BYTES} bytes"
            ));
        }
        if self.agent_session_id.trim().is_empty() || self.author_session_id.trim().is_empty() {
            return Err("agent checkpoint identities must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentEvent {
    pub cursor: u64,
    pub agent_session_id: String,
    pub kind: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_state: Option<RuntimeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_stage: Option<WorkStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<AgentCheckpoint>,
}

pub fn snapshot_digest<T: Serialize>(snapshot: &T) -> Result<String, String> {
    let bytes = serde_json::to_vec(snapshot)
        .map_err(|error| format!("failed to serialize agent snapshot for digest: {error}"))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> AgentSessionRecord {
        AgentSessionRecord {
            agent_session_id: "agent-1".into(),
            captain_session_id: "captain-1".into(),
            project_id: "project-1".into(),
            assignment: "Implement the migration".into(),
            directory: "/repo".into(),
            worktree_path: Some("/worktree".into()),
            branch: Some("feature/agent".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: Some("conversation-1".into()),
            resume_point: None,
            runtime_state: RuntimeState::Starting,
            work_stage: WorkStage::Assigned,
            created_at: 10,
            updated_at: 10,
        }
    }

    #[test]
    fn record_serializes_the_frozen_wire_names() {
        let value = serde_json::to_value(record()).unwrap();
        assert_eq!(value["agentSessionId"], "agent-1");
        assert_eq!(value["runtimeState"], "starting");
        assert_eq!(value["workStage"], "assigned");
        assert_eq!(value["providerConversationId"], "conversation-1");
    }

    #[test]
    fn explicit_work_stage_survives_runtime_updates() {
        let mut value = record();
        value.runtime_state = RuntimeState::Idle;
        assert_eq!(value.work_stage, WorkStage::Assigned);
        value.runtime_state = RuntimeState::Exited;
        assert_eq!(value.work_stage, WorkStage::Assigned);
    }

    #[test]
    fn validation_bounds_assignment_and_checkpoint_text() {
        let mut value = record();
        value.assignment = "x".repeat(MAX_ASSIGNMENT_BYTES + 1);
        assert!(value.validate().is_err());

        let checkpoint = AgentCheckpoint {
            cursor: 1,
            agent_session_id: "agent-1".into(),
            author_session_id: "captain-1".into(),
            summary: "x".repeat(MAX_CHECKPOINT_BYTES + 1),
            created_at: 11,
        };
        assert!(checkpoint.validate().is_err());
    }

    #[test]
    fn snapshot_digest_is_stable() {
        let value = record();
        assert_eq!(
            snapshot_digest(&value).unwrap(),
            snapshot_digest(&value).unwrap()
        );
    }
}
