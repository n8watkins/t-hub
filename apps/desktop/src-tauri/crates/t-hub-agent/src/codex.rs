//! Structured OpenAI Codex lifecycle normalization.
//!
//! Two production inputs converge here:
//!
//! - `codex exec --json | t-hub-agent --codex-tap` emits the stable headless
//!   ThreadEvent stream.
//! - Interactive Codex emits native lifecycle hooks and app-server JSON-RPC
//!   messages. Hook invocations enter through [`entry_from_hook`]; a trusted
//!   app-server mirror can enter through `--codex-tap`.
//!
//! The journal payload is deliberately provider-neutral and credential-safe.
//! Raw commands, reasons, prompts, tool arguments, environment values, and
//! permission paths are never copied into the journal. Stable provider ids and
//! presence flags are sufficient for replay, deduplication, attention routing,
//! and a later typed approval surface.

use std::collections::HashSet;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use serde_json::{json, Value};
use t_hub_protocol::{EventJournalEntry, JournalEventType, JournalSource};

pub const LIFECYCLE_SCHEMA: &str = "t-hub.codex.lifecycle.v1";
pub const PERMISSION_SCHEMA: &str = "t-hub.permission-request.v1";
pub const VERIFIED_CODEX_VERSION: &str = "0.144.4";

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_ID_BYTES: usize = 512;
const MAX_PATH_BYTES: usize = 2048;
static OPAQUE_HOOK_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TapOutcome {
    pub recognized_events: usize,
    pub turn_failed: bool,
}

#[derive(Debug, Clone, Default)]
struct TapState {
    session_id: Option<String>,
    turn_id: Option<String>,
    active_turn: bool,
    terminal_turn: bool,
    recognized_events: usize,
    turn_failed: bool,
    seen: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
struct TmuxBinding {
    pane: Option<String>,
    session: Option<String>,
}

/// Run the streaming `--codex-tap` producer.
pub fn run(journal_dir: Option<&str>) -> anyhow::Result<TapOutcome> {
    let dir: PathBuf = crate::journal::resolve_journal_dir(journal_dir);
    let journal = crate::journal::Journal::open(&dir)
        .with_context(|| format!("opening journal at {dir:?}"))?;
    let (pane, session) = crate::hook::resolve_tmux_pane();
    let binding = TmuxBinding { pane, session };
    let stdin = std::io::stdin();
    ingest_reader(stdin.lock(), &journal, &binding)
}

fn ingest_reader(
    reader: impl BufRead,
    journal: &crate::journal::Journal,
    binding: &TmuxBinding,
) -> anyhow::Result<TapOutcome> {
    let mut state = TapState::default();
    for line in reader.lines() {
        let line = line.context("reading Codex JSONL")?;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            append_health(
                journal,
                &mut state,
                binding,
                "degraded",
                "oversized_structured_frame",
            )?;
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                append_health(
                    journal,
                    &mut state,
                    binding,
                    "degraded",
                    "malformed_structured_frame",
                )?;
                continue;
            }
        };
        if let Some(entry) = map_tap_message(&value, &mut state, binding) {
            append_deduplicated(journal, &mut state, entry)?;
        }
    }

    if state.active_turn && !state.terminal_turn {
        append_health(
            journal,
            &mut state,
            binding,
            "disconnected",
            "structured_stream_ended_mid_turn",
        )?;
    } else if state.recognized_events == 0 {
        append_health(
            journal,
            &mut state,
            binding,
            "degraded",
            "unsupported_structured_stream",
        )?;
    }

    Ok(TapOutcome {
        recognized_events: state.recognized_events,
        turn_failed: state.turn_failed,
    })
}

fn map_tap_message(
    value: &Value,
    state: &mut TapState,
    binding: &TmuxBinding,
) -> Option<EventJournalEntry> {
    if let Some(event_type) = value.get("type").and_then(Value::as_str) {
        return map_exec_event(event_type, value, state, binding);
    }
    let method = value.get("method").and_then(Value::as_str)?;
    map_app_server_message(method, value, state, binding)
}

fn map_exec_event(
    event_type: &str,
    value: &Value,
    state: &mut TapState,
    binding: &TmuxBinding,
) -> Option<EventJournalEntry> {
    match event_type {
        "thread.started" => {
            state.session_id = bounded_string(value.get("thread_id"), MAX_ID_BYTES);
            state.terminal_turn = false;
            lifecycle_entry(
                state,
                binding,
                JournalEventType::SessionStart,
                "thread_started",
            )
        }
        "turn.started" => {
            state.active_turn = true;
            state.terminal_turn = false;
            lifecycle_entry(
                state,
                binding,
                JournalEventType::UserPromptSubmit,
                "turn_started",
            )
        }
        "turn.completed" => {
            state.active_turn = false;
            state.terminal_turn = true;
            lifecycle_entry(state, binding, JournalEventType::Stop, "turn_completed")
        }
        "turn.failed" => {
            state.active_turn = false;
            state.terminal_turn = true;
            state.turn_failed = true;
            lifecycle_entry(state, binding, JournalEventType::StopFailure, "turn_failed")
        }
        _ => None,
    }
}

fn map_app_server_message(
    method: &str,
    value: &Value,
    state: &mut TapState,
    binding: &TmuxBinding,
) -> Option<EventJournalEntry> {
    let params = value.get("params").unwrap_or(&Value::Null);
    if let Some(thread_id) = bounded_string(
        params
            .get("threadId")
            .or_else(|| params.get("thread").and_then(|thread| thread.get("id"))),
        MAX_ID_BYTES,
    ) {
        state.session_id = Some(thread_id);
    }
    if let Some(turn_id) = bounded_string(
        params
            .get("turnId")
            .or_else(|| params.get("turn").and_then(|turn| turn.get("id"))),
        MAX_ID_BYTES,
    ) {
        state.turn_id = Some(turn_id);
    }

    match method {
        "thread/started" => lifecycle_entry(
            state,
            binding,
            JournalEventType::SessionStart,
            "thread_started",
        ),
        "turn/started" => {
            state.active_turn = true;
            state.terminal_turn = false;
            lifecycle_entry(
                state,
                binding,
                JournalEventType::UserPromptSubmit,
                "turn_started",
            )
        }
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "execCommandApproval"
        | "applyPatchApproval" => {
            state.active_turn = true;
            state.terminal_turn = false;
            permission_entry(method, value, state, binding)
        }
        "serverRequest/resolved" => {
            let request_id = value_id(params.get("requestId"))?;
            let mut entry = lifecycle_entry(
                state,
                binding,
                JournalEventType::CoreAction,
                "permission_resolved",
            )?;
            entry.payload["permission_request_id"] = Value::String(request_id);
            Some(entry)
        }
        "turn/completed" => {
            state.active_turn = false;
            state.terminal_turn = true;
            let status = params
                .get("turn")
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed");
            if matches!(status, "failed" | "interrupted") {
                state.turn_failed = true;
                lifecycle_entry(state, binding, JournalEventType::StopFailure, "turn_failed")
            } else {
                lifecycle_entry(state, binding, JournalEventType::Stop, "turn_completed")
            }
        }
        "error"
            if !params
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            state.turn_failed = true;
            state.active_turn = false;
            state.terminal_turn = true;
            lifecycle_entry(state, binding, JournalEventType::StopFailure, "turn_failed")
        }
        "error" => health_entry(state, binding, "degraded", "provider_retrying"),
        "thread/closed" => lifecycle_entry(
            state,
            binding,
            JournalEventType::SessionEnd,
            "thread_closed",
        ),
        _ => None,
    }
}

fn permission_entry(
    method: &str,
    value: &Value,
    state: &TapState,
    binding: &TmuxBinding,
) -> Option<EventJournalEntry> {
    let params = value.get("params").unwrap_or(&Value::Null);
    let request_id = value_id(value.get("id"))
        .or_else(|| value_id(params.get("approvalId")))
        .or_else(|| bounded_string(params.get("itemId"), MAX_ID_BYTES))?;
    let kind = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => "command_execution",
        "item/fileChange/requestApproval" | "applyPatchApproval" => "file_change",
        "item/permissions/requestApproval" => "additional_permissions",
        _ => "unknown",
    };
    let tool_name = match kind {
        "command_execution" => "Bash",
        "file_change" => "apply_patch",
        "additional_permissions" => "request_permissions",
        _ => "unknown",
    };
    let requested_at_ms = params
        .get("startedAtMs")
        .and_then(Value::as_u64)
        .unwrap_or_else(now_ms);
    let provider_request_id = request_id.clone();
    let mut entry = base_entry(
        state.session_id.clone(),
        state.turn_id.clone(),
        binding,
        JournalEventType::PermissionRequest,
        "permission_requested",
        requested_at_ms,
    );
    entry.payload["permission_request"] = json!({
        "schema_version": PERMISSION_SCHEMA,
        "id": request_id,
        "kind": kind,
        "provider": "codex",
        "provider_request_id": provider_request_id,
        "session_id": state.session_id,
        "turn_id": state.turn_id,
        "item_id": bounded_string(params.get("itemId"), MAX_ID_BYTES),
        "tool_name": tool_name,
        "requested_at_ms": requested_at_ms,
        "has_command": params.get("command").is_some_and(|value| !value.is_null()),
        "has_reason": params.get("reason").is_some_and(|value| !value.is_null()),
        "has_additional_permissions": params
            .get("additionalPermissions")
            .or_else(|| params.get("permissions"))
            .is_some_and(|value| !value.is_null()),
    });
    entry.payload["permission_request_id"] = entry.payload["permission_request"]["id"].clone();
    Some(entry)
}

fn lifecycle_entry(
    state: &TapState,
    binding: &TmuxBinding,
    event_type: JournalEventType,
    lifecycle: &str,
) -> Option<EventJournalEntry> {
    state.session_id.as_ref()?;
    Some(base_entry(
        state.session_id.clone(),
        state.turn_id.clone(),
        binding,
        event_type,
        lifecycle,
        now_ms(),
    ))
}

fn base_entry(
    session_id: Option<String>,
    turn_id: Option<String>,
    binding: &TmuxBinding,
    event_type: JournalEventType,
    lifecycle: &str,
    timestamp_ms: u64,
) -> EventJournalEntry {
    let payload = json!({
        "schema_version": LIFECYCLE_SCHEMA,
        "provider": "codex",
        "provider_version": VERIFIED_CODEX_VERSION,
        "session_id": session_id,
        "turn_id": turn_id,
        "lifecycle": lifecycle,
        "cwd": bounded_owned(std::env::current_dir().ok().and_then(|path| path.to_str().map(str::to_string)), MAX_PATH_BYTES),
        "tmux_pane": binding.pane,
        "tmux_session": binding.session,
        "telemetry": {
            "transport": "structured",
            "quality": "authoritative",
            "runtime_health": "ready",
        }
    });
    EventJournalEntry {
        seq: 0,
        timestamp_ms,
        source: JournalSource::Agent,
        entity_id: session_id,
        event_type,
        payload,
        result: None,
    }
}

fn health_entry(
    state: &TapState,
    binding: &TmuxBinding,
    health: &str,
    detail: &str,
) -> Option<EventJournalEntry> {
    let mut entry = lifecycle_entry(
        state,
        binding,
        JournalEventType::CoreAction,
        "telemetry_health",
    )?;
    entry.payload["telemetry"] = json!({
        "transport": "structured",
        "quality": if health == "ready" { "authoritative" } else { "stale" },
        "runtime_health": health,
        "detail": detail,
    });
    Some(entry)
}

fn append_health(
    journal: &crate::journal::Journal,
    state: &mut TapState,
    binding: &TmuxBinding,
    health: &str,
    detail: &str,
) -> anyhow::Result<()> {
    if let Some(entry) = health_entry(state, binding, health, detail) {
        append_deduplicated(journal, state, entry)?;
    }
    Ok(())
}

fn append_deduplicated(
    journal: &crate::journal::Journal,
    state: &mut TapState,
    entry: EventJournalEntry,
) -> anyhow::Result<()> {
    let key = format!(
        "{:?}:{}:{}:{}:{}:{}",
        entry.event_type,
        entry.entity_id.as_deref().unwrap_or(""),
        entry
            .payload
            .get("turn_id")
            .and_then(Value::as_str)
            .unwrap_or(""),
        entry
            .payload
            .get("permission_request_id")
            .and_then(Value::as_str)
            .unwrap_or(""),
        entry
            .payload
            .get("lifecycle")
            .and_then(Value::as_str)
            .unwrap_or(""),
        entry
            .payload
            .pointer("/telemetry/detail")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    if !state.seen.insert(key) {
        return Ok(());
    }
    journal.append(entry).context("appending Codex lifecycle")?;
    state.recognized_events += 1;
    Ok(())
}

/// Whether a hook payload contains Codex's documented provider-specific fields.
pub fn is_codex_hook(payload: &Value) -> bool {
    payload.get("provider").and_then(Value::as_str) == Some("codex")
        || payload.get("turn_id").and_then(Value::as_str).is_some()
        || (payload.get("model").and_then(Value::as_str).is_some()
            && payload
                .get("permission_mode")
                .and_then(Value::as_str)
                .is_some())
}

/// Normalize one native interactive Codex hook without retaining raw tool data.
pub fn entry_from_hook(
    hook_name: &str,
    raw: &Value,
    pane: Option<String>,
    tmux_session: Option<String>,
) -> Option<EventJournalEntry> {
    if !is_codex_hook(raw) {
        return None;
    }
    let session_id = bounded_string(raw.get("session_id"), MAX_ID_BYTES);
    let turn_id = bounded_string(raw.get("turn_id"), MAX_ID_BYTES);
    let binding = TmuxBinding {
        pane,
        session: tmux_session,
    };
    let state = TapState {
        session_id,
        turn_id,
        ..TapState::default()
    };
    match hook_name {
        "SessionStart" => lifecycle_entry(
            &state,
            &binding,
            JournalEventType::SessionStart,
            "thread_started",
        ),
        "UserPromptSubmit" => lifecycle_entry(
            &state,
            &binding,
            JournalEventType::UserPromptSubmit,
            "turn_started",
        ),
        "PermissionRequest" => {
            let request_id = bounded_string(
                raw.get("approval_id")
                    .or_else(|| raw.get("tool_use_id"))
                    .or_else(|| raw.get("item_id")),
                MAX_ID_BYTES,
            )
            .unwrap_or_else(opaque_hook_request_id);
            let mut envelope = json!({
                "id": request_id,
                "params": {
                    "threadId": state.session_id,
                    "turnId": state.turn_id,
                    "itemId": bounded_string(raw.get("item_id").or_else(|| raw.get("tool_use_id")), MAX_ID_BYTES),
                    "startedAtMs": now_ms(),
                    "command": raw.get("tool_input").or_else(|| raw.get("command")).map(|_| true),
                    "reason": raw.get("reason").map(|_| true),
                    "additionalPermissions": raw.get("permission_suggestions").map(|_| true),
                }
            });
            let tool_name = raw
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("Bash");
            let method = match tool_name {
                "apply_patch" | "Edit" | "Write" => "item/fileChange/requestApproval",
                "request_permissions" => "item/permissions/requestApproval",
                _ => "item/commandExecution/requestApproval",
            };
            envelope["params"]["toolName"] = Value::String(bounded_str(tool_name, MAX_ID_BYTES));
            permission_entry(method, &envelope, &state, &binding)
        }
        "PostToolUse" => {
            let request_id = bounded_string(
                raw.get("approval_id")
                    .or_else(|| raw.get("tool_use_id"))
                    .or_else(|| raw.get("item_id")),
                MAX_ID_BYTES,
            )?;
            let mut entry = lifecycle_entry(
                &state,
                &binding,
                JournalEventType::CoreAction,
                "permission_resolved",
            )?;
            entry.payload["permission_request_id"] = Value::String(request_id);
            Some(entry)
        }
        "Stop" => lifecycle_entry(&state, &binding, JournalEventType::Stop, "turn_completed"),
        "StopFailure" => lifecycle_entry(
            &state,
            &binding,
            JournalEventType::StopFailure,
            "turn_failed",
        ),
        "SessionEnd" => lifecycle_entry(
            &state,
            &binding,
            JournalEventType::SessionEnd,
            "thread_closed",
        ),
        _ => health_entry(&state, &binding, "degraded", "unsupported_native_hook"),
    }
}

fn value_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(bounded_str(value, MAX_ID_BYTES)),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn bounded_string(value: Option<&Value>, max: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|value| bounded_str(value, max))
}

fn bounded_owned(value: Option<String>, max: usize) -> Option<String> {
    value.map(|value| bounded_str(&value, max))
}

fn bounded_str(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Generate a versioned opaque fallback when Codex omits every request/item id.
///
/// The generator deliberately accepts no hook payload. In particular, commands,
/// tool input, reasons, prompts, paths, and credentials cannot influence the
/// durable identifier, so it is not a dictionary-testable content fingerprint.
fn opaque_hook_request_id() -> String {
    let sequence = OPAQUE_HOOK_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "hook-opaque-v1:{:x}:{:x}:{sequence:x}",
        now_ms(),
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "t-hub-codex-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn app_server_permission_fixture_normalizes_without_raw_command() {
        let dir = temp_dir("fixture");
        let journal = crate::journal::Journal::open(&dir).unwrap();
        let input =
            include_str!("../tests/fixtures/codex-0.144.4-app-server-permission-lifecycle.jsonl");
        let outcome = ingest_reader(
            std::io::Cursor::new(input),
            &journal,
            &TmuxBinding {
                pane: Some("%42".into()),
                session: Some("th_crew0001".into()),
            },
        )
        .unwrap();
        assert_eq!(outcome.recognized_events, 4);
        assert!(!outcome.turn_failed);

        let entries = journal.replay(0).unwrap();
        assert_eq!(entries[1].event_type, JournalEventType::PermissionRequest);
        assert_eq!(
            entries[1].payload["permission_request"]["kind"],
            "command_execution"
        );
        assert_eq!(entries[1].payload["tmux_session"], "th_crew0001");
        let serialized = serde_json::to_string(&entries[1].payload).unwrap();
        assert!(!serialized.contains("credential-bearing-command"));
        assert!(!serialized.contains("sanitized reason"));
        assert!(entries[1].payload["permission_request"]["has_command"] == true);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn recorded_exec_fixtures_preserve_clean_resume_and_failure_boundaries() {
        let fixtures = [
            (
                "clean",
                include_str!("../tests/fixtures/codex-0.142.5-clean-turn.jsonl"),
                false,
            ),
            (
                "resumed",
                include_str!("../tests/fixtures/codex-0.142.5-resumed-turn.jsonl"),
                false,
            ),
            (
                "failed",
                include_str!("../tests/fixtures/codex-0.142.5-turn-failed.jsonl"),
                true,
            ),
        ];
        for (tag, input, failed) in fixtures {
            let dir = temp_dir(tag);
            let journal = crate::journal::Journal::open(&dir).unwrap();
            let outcome = ingest_reader(
                std::io::Cursor::new(input),
                &journal,
                &TmuxBinding::default(),
            )
            .unwrap();
            assert_eq!(outcome.recognized_events, 3, "fixture {tag}");
            assert_eq!(outcome.turn_failed, failed, "fixture {tag}");
            let entries = journal.replay(0).unwrap();
            assert_eq!(entries[0].event_type, JournalEventType::SessionStart);
            assert_eq!(entries[1].event_type, JournalEventType::UserPromptSubmit);
            assert_eq!(
                entries[2].event_type,
                if failed {
                    JournalEventType::StopFailure
                } else {
                    JournalEventType::Stop
                }
            );
            let persisted = serde_json::to_string(&entries).unwrap();
            assert!(!persisted.contains("this-model-does-not-exist-xyz"));
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn duplicate_permission_callbacks_are_journaled_once() {
        let dir = temp_dir("dedup");
        let journal = crate::journal::Journal::open(&dir).unwrap();
        let request = r#"{"method":"item/commandExecution/requestApproval","id":"req-1","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","startedAtMs":1}}"#;
        let input = format!("{request}\n{request}\n");
        let outcome = ingest_reader(
            std::io::Cursor::new(input),
            &journal,
            &TmuxBinding::default(),
        )
        .unwrap();
        assert_eq!(outcome.recognized_events, 2);
        let entries = journal.replay(0).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.event_type == JournalEventType::PermissionRequest)
                .count(),
            1
        );
        assert_eq!(
            entries.last().unwrap().payload["telemetry"]["runtime_health"],
            "disconnected"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mid_turn_eof_is_explicitly_disconnected() {
        let dir = temp_dir("disconnect");
        let journal = crate::journal::Journal::open(&dir).unwrap();
        let input = "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n{\"type\":\"turn.started\"}\n";
        ingest_reader(
            std::io::Cursor::new(input),
            &journal,
            &TmuxBinding::default(),
        )
        .unwrap();
        let entries = journal.replay(0).unwrap();
        assert_eq!(
            entries.last().unwrap().payload["telemetry"]["runtime_health"],
            "disconnected"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn codex_hook_permission_is_credential_safe() {
        let raw = json!({
            "session_id": "thread-1",
            "turn_id": "turn-1",
            "model": "gpt-5.6-sol",
            "permission_mode": "default",
            "tool_name": "Bash",
            "tool_use_id": "item-1",
            "tool_input": {"command": "token=top-secret do-it"},
            "reason": "contains top-secret"
        });
        let entry = entry_from_hook(
            "PermissionRequest",
            &raw,
            Some("%9".into()),
            Some("th_crew0001".into()),
        )
        .unwrap();
        let payload = serde_json::to_string(&entry.payload).unwrap();
        assert_eq!(entry.event_type, JournalEventType::PermissionRequest);
        assert!(!payload.contains("top-secret"));
        assert_eq!(entry.payload["tmux_session"], "th_crew0001");
    }

    #[test]
    fn missing_provider_ids_use_non_content_derived_opaque_ids() {
        let make = |secret: &str| {
            entry_from_hook(
                "PermissionRequest",
                &json!({
                    "session_id": "thread-1",
                    "turn_id": "turn-1",
                    "model": "gpt-5.6-sol",
                    "permission_mode": "default",
                    "tool_name": "Bash",
                    "tool_input": {"command": secret},
                    "reason": secret
                }),
                None,
                None,
            )
            .unwrap()
        };
        let first = make("credential-alpha");
        let second = make("credential-beta");
        let first_id = first.payload["permission_request"]["id"].as_str().unwrap();
        let second_id = second.payload["permission_request"]["id"].as_str().unwrap();

        assert!(first_id.starts_with("hook-opaque-v1:"));
        assert!(second_id.starts_with("hook-opaque-v1:"));
        assert_ne!(first_id, second_id);
        let persisted = serde_json::to_string(&[first, second]).unwrap();
        assert!(!persisted.contains("credential-alpha"));
        assert!(!persisted.contains("credential-beta"));
    }
}
