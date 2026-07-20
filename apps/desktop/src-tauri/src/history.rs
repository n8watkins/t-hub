#![allow(dead_code)]

//! Provider-neutral History identity and transcript adapter foundation.
//!
//! This module deliberately does not expose a command yet.
//! It locks the native Claude and Codex identity boundary before the catalog is
//! connected to control, MCP, CLI, or UI surfaces.
//! The temporary dead-code allowance is removed when the reviewed catalog API
//! connects this foundation to its first production caller.
//! Legacy `recent_sessions` remains unchanged until exact actions and durable
//! organizational joins are available.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

#[cfg(test)]
use serde_json::json;

pub const HISTORY_LABEL_MAX_CHARS: usize = 120;
pub const HISTORY_LAST_TEXT_MAX_CHARS: usize = 240;
pub const HISTORY_REASON_MAX_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn canonical(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionCompatibility {
    pub status: ActionStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionStatus {
    Supported,
    Unavailable,
    Incompatible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryActions {
    pub focus: ActionCompatibility,
    pub resume: ActionCompatibility,
    pub recover: ActionCompatibility,
    pub archive: ActionCompatibility,
    pub unarchive: ActionCompatibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContinuityState {
    Active,
    Resumable,
    Archived,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub history_id: String,
    pub harness: Harness,
    pub provider: Option<String>,
    pub provider_session_id: Option<String>,
    pub conversation_id: String,
    pub cwd: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub captain_id: Option<String>,
    pub role: Option<String>,
    pub workspace_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
    pub label: String,
    pub last_text: Option<String>,
    pub started_at: Option<String>,
    pub last_seen_at: String,
    pub continuity_state: ContinuityState,
    pub actions: HistoryActions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTranscript {
    pub entry: HistoryEntry,
    pub degraded_reason: Option<String>,
}

fn unavailable(reason: &str) -> ActionCompatibility {
    ActionCompatibility {
        status: ActionStatus::Unavailable,
        reason: Some(bounded_text(reason, HISTORY_REASON_MAX_CHARS)),
    }
}

fn foundational_actions() -> HistoryActions {
    let reason = "History actions are not connected in this source slice.";
    HistoryActions {
        focus: unavailable(reason),
        resume: unavailable(reason),
        recover: unavailable(reason),
        archive: unavailable(reason),
        unarchive: unavailable(reason),
    }
}

pub fn history_id(harness: Harness, conversation_id: &str) -> String {
    history_id_parts(harness.canonical(), conversation_id)
}

fn history_id_parts(harness: &str, conversation_id: &str) -> String {
    let harness = harness.as_bytes();
    let conversation = conversation_id.as_bytes();
    let mut digest = Sha256::new();
    digest.update((harness.len() as u32).to_be_bytes());
    digest.update(harness);
    digest.update((conversation.len() as u32).to_be_bytes());
    digest.update(conversation);
    format!("history:v1:{:x}", digest.finalize())
}

fn bounded_text(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut bounded = one_line
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

fn message_text(value: &Value) -> Option<String> {
    let content = value.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    (!text.trim().is_empty()).then_some(text)
}

fn real_user_text(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && !text.starts_with('<')
}

fn iso_from_epoch(last_seen_epoch: i64) -> Result<String, String> {
    DateTime::<Utc>::from_timestamp(last_seen_epoch, 0)
        .map(|stamp| stamp.to_rfc3339_opts(SecondsFormat::Secs, true))
        .ok_or_else(|| "transcript timestamp is outside the supported range".to_string())
}

fn normalized_timestamp(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp).ok().map(|stamp| {
        stamp
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    })
}

fn parse_degraded_reason(
    harness: &str,
    malformed: usize,
    invalid_timestamps: usize,
) -> Option<String> {
    let mut facts = Vec::new();
    if malformed > 0 {
        facts.push(format!("skipped {malformed} malformed record(s)"));
    }
    if invalid_timestamps > 0 {
        facts.push(format!("ignored {invalid_timestamps} invalid timestamp(s)"));
    }
    (!facts.is_empty()).then(|| {
        bounded_text(
            &format!("{harness} transcript {}.", facts.join(" and ")),
            HISTORY_REASON_MAX_CHARS,
        )
    })
}

fn base_entry(
    harness: Harness,
    conversation_id: String,
    cwd: String,
    label: String,
    last_text: Option<String>,
    started_at: Option<String>,
    last_seen_epoch: i64,
    provider: Option<String>,
) -> Result<HistoryEntry, String> {
    let last_seen_at = iso_from_epoch(last_seen_epoch)?;
    Ok(HistoryEntry {
        history_id: history_id(harness, &conversation_id),
        harness,
        provider,
        provider_session_id: Some(conversation_id.clone()),
        conversation_id,
        cwd,
        project_id: None,
        project_name: None,
        captain_id: None,
        role: None,
        workspace_id: None,
        worktree_id: None,
        branch: None,
        label: bounded_text(&label, HISTORY_LABEL_MAX_CHARS),
        last_text: last_text
            .filter(|text| !text.trim().is_empty())
            .map(|text| bounded_text(&text, HISTORY_LAST_TEXT_MAX_CHARS)),
        started_at,
        last_seen_at,
        continuity_state: ContinuityState::Resumable,
        actions: foundational_actions(),
    })
}

pub fn parse_claude_transcript(
    path: &Path,
    transcript: &str,
    last_seen_epoch: i64,
    archived: bool,
) -> Result<ParsedTranscript, String> {
    let conversation_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| "Claude transcript has no valid filename identity".to_string())?
        .to_string();
    let mut cwd = None;
    let mut summary = None;
    let mut first_user = None;
    let mut last_text = None;
    let mut started_at = None;
    let mut saw_start_timestamp = false;
    let mut malformed = 0usize;
    let mut invalid_timestamps = 0usize;
    for line in transcript.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            malformed += 1;
            continue;
        };
        if cwd.is_none() {
            cwd = value
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.is_empty())
                .map(str::to_string);
        }
        if !saw_start_timestamp {
            if let Some(timestamp_value) = value.get("timestamp") {
                saw_start_timestamp = true;
                if let Some(normalized) = timestamp_value.as_str().and_then(normalized_timestamp) {
                    started_at = Some(normalized);
                } else {
                    invalid_timestamps += 1;
                }
            }
        }
        let kind = value.get("type").and_then(Value::as_str);
        if kind == Some("summary") {
            if let Some(text) = value.get("summary").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    summary = Some(text.to_string());
                }
            }
        }
        if matches!(kind, Some("user" | "assistant")) {
            if let Some(text) = message_text(&value) {
                if kind == Some("user") && first_user.is_none() && real_user_text(&text) {
                    first_user = Some(text.clone());
                }
                if real_user_text(&text) {
                    last_text = Some(text);
                }
            }
        }
    }
    let cwd = cwd.ok_or_else(|| "Claude transcript has no recorded cwd".to_string())?;
    let label = summary
        .or(first_user)
        .unwrap_or_else(|| cwd.rsplit('/').next().unwrap_or(&cwd).to_string());
    let mut entry = base_entry(
        Harness::Claude,
        conversation_id,
        cwd,
        label,
        last_text,
        started_at,
        last_seen_epoch,
        None,
    )?;
    if archived {
        entry.continuity_state = ContinuityState::Archived;
    }
    Ok(ParsedTranscript {
        entry,
        degraded_reason: parse_degraded_reason("Claude", malformed, invalid_timestamps),
    })
}

fn codex_filename_identity(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Codex rollout filename is not valid UTF-8".to_string())?;
    let body = name
        .strip_prefix("rollout-")
        .and_then(|name| name.strip_suffix(".jsonl"))
        .ok_or_else(|| "Codex rollout filename format is not supported".to_string())?;
    if !body.is_ascii() {
        return Err("Codex rollout filename version is not supported".to_string());
    }
    // Version 1 supports only the observed `YYYY-MM-DDTHH-MM-SS-<uuid>` form.
    // A future prefix must become an explicit adapter version instead of being
    // guessed merely because its final 36 bytes resemble a UUID.
    if body.len() != 56 {
        return Err("Codex rollout filename version is not supported".to_string());
    }
    let timestamp = &body[..19];
    let timestamp_valid = timestamp.chars().enumerate().all(|(index, ch)| {
        if matches!(index, 4 | 7) {
            ch == '-'
        } else if index == 10 {
            ch == 'T'
        } else if matches!(index, 13 | 16) {
            ch == '-'
        } else {
            ch.is_ascii_digit()
        }
    }) && body.as_bytes().get(19) == Some(&b'-');
    if !timestamp_valid {
        return Err("Codex rollout filename version is not supported".to_string());
    }
    let identity = &body[body.len() - 36..];
    let valid = identity.chars().enumerate().all(|(index, ch)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            ch == '-'
        } else {
            ch.is_ascii_digit() || ('a'..='f').contains(&ch)
        }
    });
    if !valid {
        return Err("Codex rollout filename identity is not a UUID".to_string());
    }
    Ok(identity.to_string())
}

pub fn parse_codex_rollout(
    path: &Path,
    transcript: &str,
    last_seen_epoch: i64,
) -> Result<ParsedTranscript, String> {
    let conversation_id = codex_filename_identity(path)?;
    let mut matching_meta = Vec::<(String, Option<String>, Option<String>, bool)>::new();
    let mut first_user = None;
    let mut last_text = None;
    let mut malformed = 0usize;
    for line in transcript.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            malformed += 1;
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str);
        let payload = value.get("payload").unwrap_or(&Value::Null);
        if kind == Some("session_meta")
            && payload.get("id").and_then(Value::as_str) == Some(&conversation_id)
        {
            if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    let timestamp_value = payload.get("timestamp");
                    let timestamp = timestamp_value
                        .and_then(Value::as_str)
                        .and_then(normalized_timestamp);
                    let invalid_timestamp = timestamp_value.is_some() && timestamp.is_none();
                    matching_meta.push((
                        cwd.to_string(),
                        timestamp,
                        payload
                            .get("model_provider")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        invalid_timestamp,
                    ));
                }
            }
        }
        if kind == Some("event_msg")
            && payload.get("type").and_then(Value::as_str) == Some("user_message")
        {
            if let Some(text) = payload.get("message").and_then(Value::as_str) {
                if first_user.is_none() && real_user_text(text) {
                    first_user = Some(text.to_string());
                }
                if real_user_text(text) {
                    last_text = Some(text.to_string());
                }
            }
        }
    }
    matching_meta.sort();
    matching_meta.dedup();
    let [(cwd, timestamp, provider, invalid_timestamp)] = matching_meta.as_slice() else {
        return Err(if matching_meta.is_empty() {
            "Codex rollout has no filename-matching session_meta".to_string()
        } else {
            "Codex rollout has conflicting filename-matching session_meta records".to_string()
        });
    };
    let label = first_user.unwrap_or_else(|| cwd.rsplit('/').next().unwrap_or(cwd).to_string());
    let entry = base_entry(
        Harness::Codex,
        conversation_id,
        cwd.clone(),
        label,
        last_text,
        timestamp.clone(),
        last_seen_epoch,
        provider.clone(),
    )?;
    Ok(ParsedTranscript {
        entry,
        degraded_reason: parse_degraded_reason("Codex", malformed, usize::from(*invalid_timestamp)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn codex_path(id: &str) -> PathBuf {
        PathBuf::from(format!("rollout-2026-07-15T10-00-00-{id}.jsonl"))
    }

    #[test]
    fn history_id_uses_length_prefixed_utf8_known_vectors() {
        assert_eq!(
            history_id(Harness::Codex, "abc"),
            "history:v1:2a25ea1f9dfd8982d7a5f332749a1804e48ac4818cf92008660805b3dd342723"
        );
        assert_eq!(
            history_id(Harness::Claude, "abc"),
            "history:v1:4ce9f8b128cb5265641ee81a6d2e2c98f57fe3b4120024fe26f0344fd19261aa"
        );
        assert_eq!(
            history_id_parts("cødex", "会話"),
            "history:v1:d18dc6f6298c2760abc53bc8e64f0e3f55b9935debb4e267db86fec390a29268"
        );
    }

    #[test]
    fn same_native_identity_is_distinct_across_harnesses() {
        assert_ne!(
            history_id(Harness::Claude, "same-id"),
            history_id(Harness::Codex, "same-id")
        );
    }

    #[test]
    fn claude_parser_preserves_two_conversations_in_one_cwd() {
        let first = parse_claude_transcript(
            Path::new("first.jsonl"),
            r#"{"type":"user","cwd":"/repo","timestamp":"2026-07-15T10:00:00Z","message":{"content":"first task"}}
{"type":"assistant","cwd":"/repo","message":{"content":"first answer"}}"#,
            100,
            false,
        )
        .unwrap();
        let second = parse_claude_transcript(
            Path::new("second.jsonl"),
            r#"{"type":"summary","cwd":"/repo","summary":"Second task"}"#,
            200,
            false,
        )
        .unwrap();
        assert_ne!(first.entry.history_id, second.entry.history_id);
        assert_eq!(first.entry.cwd, second.entry.cwd);
        assert_eq!(first.entry.last_text.as_deref(), Some("first answer"));
        assert_eq!(second.entry.label, "Second task");
    }

    #[test]
    fn claude_parser_normalizes_timestamps_and_filters_wrappers() {
        let parsed = parse_claude_transcript(
            Path::new("claude-id.jsonl"),
            r#"not json
{"type":"user","cwd":"/repo","timestamp":"not-a-time","message":{"content":"<local-command-caveat>noise</local-command-caveat>"}}
{"type":"user","cwd":"/repo","timestamp":"2026-07-15T03:00:00-07:00","message":{"content":[{"type":"tool_result","text":"ignored"},{"type":"text","text":"real task"}]}}
{"type":"assistant","cwd":"/repo","message":{"content":"<system-reminder>noise</system-reminder>"}}"#,
            100,
            false,
        )
        .unwrap();
        assert_eq!(parsed.entry.started_at, None);
        assert_eq!(parsed.entry.label, "real task");
        assert_eq!(parsed.entry.last_text.as_deref(), Some("real task"));
        let reason = parsed.degraded_reason.unwrap();
        assert!(reason.contains("malformed"));
        assert!(reason.contains("invalid timestamp"));
    }

    #[test]
    fn claude_parser_normalizes_an_authoritative_offset_start_timestamp() {
        let parsed = parse_claude_transcript(
            Path::new("claude-id.jsonl"),
            r#"{"type":"user","cwd":"/repo","timestamp":"2026-07-15T03:00:00-07:00","message":{"content":"real task"}}"#,
            100,
            false,
        )
        .unwrap();
        assert_eq!(
            parsed.entry.started_at.as_deref(),
            Some("2026-07-15T10:00:00Z")
        );
        assert_eq!(parsed.degraded_reason, None);
    }

    #[test]
    fn labels_and_previews_are_bounded_by_unicode_characters() {
        let long = "🙂".repeat(300);
        let transcript = json!({
            "type":"user",
            "cwd":"/repo",
            "message":{"content":long}
        })
        .to_string();
        let parsed =
            parse_claude_transcript(Path::new("unicode-id.jsonl"), &transcript, 100, false)
                .unwrap();
        assert_eq!(parsed.entry.label.chars().count(), HISTORY_LABEL_MAX_CHARS);
        assert_eq!(
            parsed.entry.last_text.unwrap().chars().count(),
            HISTORY_LAST_TEXT_MAX_CHARS
        );
    }

    #[test]
    fn archived_claude_entry_is_exact_and_non_destructive() {
        let parsed = parse_claude_transcript(
            Path::new("archived-id.jsonl"),
            r#"{"type":"user","cwd":"/repo","message":{"content":"archive me"}}"#,
            100,
            true,
        )
        .unwrap();
        assert_eq!(parsed.entry.conversation_id, "archived-id");
        assert_eq!(parsed.entry.continuity_state, ContinuityState::Archived);
        assert_eq!(
            parsed.entry.actions.resume.status,
            ActionStatus::Unavailable
        );
    }

    #[test]
    fn codex_parser_selects_filename_matching_child_meta() {
        let child = "22222222-2222-4222-8222-222222222222";
        let text = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            json!({"type":"session_meta","payload":{"id":"11111111-1111-4111-8111-111111111111","cwd":"/parent","timestamp":"2026-07-15T09:00:00Z"}}),
            json!({"type":"session_meta","payload":{"id":child,"cwd":"/child","timestamp":"2026-07-15T03:00:00-07:00","model_provider":"openai"}}),
            json!({"type":"response_item","payload":{"role":"user","content":"duplicate noise"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"<system-reminder>before</system-reminder>"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"child task"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"<system-reminder>after</system-reminder>"}}),
        );
        let parsed = parse_codex_rollout(&codex_path(child), &text, 300).unwrap();
        assert_eq!(parsed.entry.conversation_id, child);
        assert_eq!(parsed.entry.cwd, "/child");
        assert_eq!(parsed.entry.label, "child task");
        assert_eq!(parsed.entry.last_text.as_deref(), Some("child task"));
        assert_eq!(parsed.entry.provider.as_deref(), Some("openai"));
        assert_eq!(
            parsed.entry.started_at.as_deref(),
            Some("2026-07-15T10:00:00Z")
        );
    }

    #[test]
    fn codex_parser_fails_closed_without_matching_meta() {
        let child = "22222222-2222-4222-8222-222222222222";
        let text = json!({"type":"session_meta","payload":{"id":"11111111-1111-4111-8111-111111111111","cwd":"/parent"}}).to_string();
        let error = parse_codex_rollout(&codex_path(child), &text, 300).unwrap_err();
        assert!(error.contains("no filename-matching"));
    }

    #[test]
    fn codex_parser_rejects_unknown_filename_versions() {
        let id = "22222222-2222-4222-8222-222222222222";
        let text = json!({"type":"session_meta","payload":{"id":id,"cwd":"/repo"}}).to_string();
        let future = PathBuf::from(format!("rollout-v2-2026-07-15T10-00-00-{id}.jsonl"));
        let error = parse_codex_rollout(&future, &text, 300).unwrap_err();
        assert!(error.contains("version is not supported"));
    }

    #[test]
    fn codex_parser_rejects_non_ascii_filename_without_panicking() {
        let id = "22222222-2222-4222-8222-222222222222";
        let text = json!({"type":"session_meta","payload":{"id":id,"cwd":"/repo"}}).to_string();
        let path = PathBuf::from(format!("rollout-2026-07-15T10-0🙂-{id}.jsonl"));
        let error = parse_codex_rollout(&path, &text, 300).unwrap_err();
        assert!(error.contains("version is not supported"));
    }

    #[test]
    fn codex_parser_rejects_uppercase_identity_without_normalizing_it() {
        let id = "22222222-2222-4222-8222-22222222222A";
        let text = json!({"type":"session_meta","payload":{"id":id,"cwd":"/repo"}}).to_string();
        let error = parse_codex_rollout(&codex_path(id), &text, 300).unwrap_err();
        assert!(error.contains("not a UUID"));
    }

    #[test]
    fn codex_parser_accepts_equivalent_duplicate_meta_and_degrades_invalid_time() {
        let id = "22222222-2222-4222-8222-222222222222";
        let meta = json!({"type":"session_meta","payload":{
            "id":id,
            "cwd":"/repo",
            "timestamp":"invalid",
            "model_provider":"openai"
        }});
        let parsed = parse_codex_rollout(&codex_path(id), &format!("{meta}\n{meta}"), 300).unwrap();
        assert_eq!(parsed.entry.started_at, None);
        assert!(parsed
            .degraded_reason
            .unwrap()
            .contains("invalid timestamp"));
    }

    #[test]
    fn wrong_type_timestamps_are_degraded_and_never_substituted() {
        let claude = parse_claude_transcript(
            Path::new("claude-id.jsonl"),
            r#"{"type":"user","cwd":"/repo","timestamp":123,"message":{"content":"first"}}
{"type":"assistant","cwd":"/repo","timestamp":"2026-07-15T10:00:00Z","message":{"content":"later"}}"#,
            100,
            false,
        )
        .unwrap();
        assert_eq!(claude.entry.started_at, None);
        assert!(claude
            .degraded_reason
            .unwrap()
            .contains("invalid timestamp"));

        let id = "22222222-2222-4222-8222-222222222222";
        let codex_text = json!({"type":"session_meta","payload":{
            "id":id,
            "cwd":"/repo",
            "timestamp":{"future":"shape"}
        }})
        .to_string();
        let codex = parse_codex_rollout(&codex_path(id), &codex_text, 300).unwrap();
        assert_eq!(codex.entry.started_at, None);
        assert!(codex.degraded_reason.unwrap().contains("invalid timestamp"));
    }

    #[test]
    fn codex_parser_rejects_conflicting_matching_meta() {
        let id = "22222222-2222-4222-8222-222222222222";
        let text = format!(
            "{}\n{}",
            json!({"type":"session_meta","payload":{"id":id,"cwd":"/one"}}),
            json!({"type":"session_meta","payload":{"id":id,"cwd":"/two"}}),
        );
        let error = parse_codex_rollout(&codex_path(id), &text, 300).unwrap_err();
        assert!(error.contains("conflicting"));
    }

    #[test]
    fn serialized_entry_keeps_nullable_fields_and_unavailable_actions() {
        let parsed = parse_claude_transcript(
            Path::new("id.jsonl"),
            r#"{"type":"user","cwd":"/repo","message":{"content":"task"}}"#,
            100,
            false,
        )
        .unwrap();
        let value = serde_json::to_value(parsed.entry).unwrap();
        for field in [
            "provider",
            "projectId",
            "projectName",
            "captainId",
            "role",
            "workspaceId",
            "worktreeId",
            "branch",
            "startedAt",
        ] {
            assert_eq!(value[field], Value::Null, "{field}");
        }
        assert_eq!(value["actions"]["resume"]["status"], "unavailable");
        assert_eq!(value["continuityState"], "resumable");
    }
}
