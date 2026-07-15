//! Request routing: turn an [`AgentRequest`] into an [`AgentResponse`] by
//! calling the [`crate::registry`] / [`crate::host`] handlers, and record an
//! audit [`EventJournalEntry`] for actions that mutate or query host state.
//!
//! This is pure glue over the module APIs and is fully implemented; the modules
//! it calls (`host`) are where the remaining `SUBAGENT(host)` work lives.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use t_hub_protocol::{
    AgentRequest, AgentResponse, EventJournalEntry, JournalEventType, JournalSource,
    ResponseErrorKind,
};

use crate::host;
use crate::journal::Journal;
use crate::registry;

/// Handle one request, returning the response body. Side effect: appends an
/// `AgentCommand` journal entry for registry mutations (new/kill session) so the
/// spine records actions the agent took (PRD §8 `result` field).
pub fn handle(journal: &Journal, req: AgentRequest) -> AgentResponse {
    match req {
        AgentRequest::ListSessions => match registry::list_sessions() {
            Ok(names) => AgentResponse::Sessions { names },
            Err(e) => err(ResponseErrorKind::CommandFailed, e.to_string()),
        },

        AgentRequest::NewSession { name, cwd, command } => {
            let res = registry::new_session(&name, &cwd, command.as_deref());
            record_command(journal, &name, "new_session", res.as_ref().err());
            match res {
                Ok(()) => AgentResponse::SessionCreated,
                Err(e) => err(ResponseErrorKind::CommandFailed, e.to_string()),
            }
        }

        AgentRequest::HasSession { name } => AgentResponse::SessionExists {
            exists: registry::has_session(&name),
        },

        AgentRequest::KillSession { name } => {
            let res = registry::kill_session(&name);
            record_command(journal, &name, "kill_session", res.as_ref().err());
            match res {
                Ok(()) => AgentResponse::SessionKilled,
                Err(e) => err(ResponseErrorKind::CommandFailed, e.to_string()),
            }
        }

        AgentRequest::Metrics => AgentResponse::Metrics(host::metrics()),

        AgentRequest::GitBranch { cwd } => match host::git_branch(&cwd) {
            Ok(branch) => AgentResponse::GitBranch { branch },
            Err(e) => err(ResponseErrorKind::CommandFailed, e.to_string()),
        },

        AgentRequest::GitWorktrees { cwd } => match host::git_worktrees(&cwd) {
            Ok(worktrees) => AgentResponse::GitWorktrees { worktrees },
            Err(e) => err(ResponseErrorKind::CommandFailed, e.to_string()),
        },

        AgentRequest::GitInfo { cwd } => match host::git_info(&cwd) {
            Ok(info) => AgentResponse::GitInfo(info),
            Err(e) => err(ResponseErrorKind::CommandFailed, e.to_string()),
        },

        AgentRequest::CapturePane { name } => match registry::capture_pane(&name) {
            Ok(bytes) => AgentResponse::Pane {
                base64: STANDARD.encode(bytes),
            },
            Err(e) => err(ResponseErrorKind::NotFound, e.to_string()),
        },

        AgentRequest::Unknown => err(
            ResponseErrorKind::Unsupported,
            "unsupported request op".to_string(),
        ),
    }
}

fn err(kind: ResponseErrorKind, message: String) -> AgentResponse {
    AgentResponse::Error { kind, message }
}

/// Record an agent-initiated command in the journal (best-effort; a journal
/// write failure must not fail the RPC, so we swallow its error after logging).
fn record_command(journal: &Journal, entity: &str, op: &str, err: Option<&anyhow::Error>) {
    let result = match err {
        Some(e) => format!("error: {e}"),
        None => "ok".to_string(),
    };
    let entry = EventJournalEntry {
        seq: 0,
        timestamp_ms: host::now_ms(),
        source: JournalSource::Agent,
        entity_id: Some(entity.to_string()),
        event_type: JournalEventType::AgentCommand,
        payload: serde_json::json!({ "op": op }),
        result: Some(result),
    };
    if let Err(e) = journal.append(entry) {
        eprintln!("t-hub-agent: journal append failed for {op} on {entity}: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_journal(tag: &str) -> (Journal, PathBuf) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("t-hub-dispatch-{tag}-{ts}"));
        (Journal::open(&dir).unwrap(), dir)
    }

    #[test]
    fn metrics_request_returns_metrics() {
        let (j, dir) = temp_journal("metrics");
        let resp = handle(&j, AgentRequest::Metrics);
        assert!(matches!(resp, AgentResponse::Metrics(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_request_is_unsupported() {
        let (j, dir) = temp_journal("unknown");
        let resp = handle(&j, AgentRequest::Unknown);
        match resp {
            AgentResponse::Error { kind, .. } => {
                assert_eq!(kind, ResponseErrorKind::Unsupported)
            }
            other => panic!("expected Error, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_info_request_returns_non_repo_snapshot() {
        let (j, dir) = temp_journal("git-info");
        let cwd = dir.join("not-a-repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let resp = handle(
            &j,
            AgentRequest::GitInfo {
                cwd: cwd.to_string_lossy().into_owned(),
            },
        );
        match resp {
            AgentResponse::GitInfo(info) => assert!(!info.is_repo),
            other => panic!("expected GitInfo, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_session_records_journal_entry() {
        let (j, dir) = temp_journal("newsess");
        // Use a name that tmux will reject? No — just exercise the journal path
        // by killing a non-existent session (idempotent Ok → records "ok").
        let resp = handle(
            &j,
            AgentRequest::KillSession {
                name: "th_does_not_exist_xyz".into(),
            },
        );
        assert!(matches!(resp, AgentResponse::SessionKilled));
        // A journal entry for the kill action must have been recorded.
        assert_eq!(j.head_seq(), 1);
        let entries = j.replay(0).unwrap();
        assert_eq!(entries[0].event_type, JournalEventType::AgentCommand);
        std::fs::remove_dir_all(&dir).ok();
    }
}
