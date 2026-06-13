//! Claude Code hook installation (PLAN.md Workstream B; REVIEW risk: "hook
//! install edits `~/.claude/settings.json`").
//!
//! The 0.5 hook set is the **verified** list from REVIEW.md §9.6 — every name
//! below is a real, current Claude Code hook event. Each fires a TermHub handler
//! script that reads the hook's JSON stdin (which always carries the base fields
//! `session_id`, `transcript_path`, `cwd`, and inside subagents `agent_id` /
//! `agent_type`), appends an [`EventJournalEntry`] to the WSL journal, and pings
//! the agent. The handler does **not** block the hook (fast append + return).
//!
//! ## Non-destructive install (hard requirement, REVIEW)
//! Installing must: require explicit consent (enforced at the call site / UI),
//! merge with the user's existing `hooks` block rather than overwrite it,
//! survive hand-edits, and ship a clean uninstall. We tag our entries so an
//! uninstall removes exactly ours.

// This module is the install-side contract surface that SUBAGENT(claude-adapter)
// wires into Tauri commands + the agent's hook-ingest path. Several items are
// not yet called from within the crate; allow that until the subagent lands.
#![allow(dead_code)]

use crate::model::SessionStatus;
use termhub_protocol::JournalEventType;

/// The exact hook event names TermHub installs handlers for (PLAN.md §B,
/// verified REVIEW §9.6). Order is the documented lifecycle order for readability.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "PermissionRequest",
    "Notification",
    "Elicitation",
    "SubagentStart",
    "SubagentStop",
    "TaskCreated",
    "TaskCompleted",
    "CwdChanged",
    "WorktreeCreate",
    "WorktreeRemove",
];

/// A marker embedded in every TermHub-authored settings.json hook entry so the
/// uninstaller can remove exactly our entries and leave the user's intact.
pub const TERMHUB_HOOK_MARKER: &str = "__termhub_managed__";

/// Map a Claude hook event name to the journal event type we record. Returns
/// `None` for an unrecognized name (forward-compat: a future hook we don't model
/// yet is journaled as `Unknown` by the caller).
pub fn event_type_for_hook(hook_name: &str) -> Option<JournalEventType> {
    use JournalEventType::*;
    Some(match hook_name {
        "SessionStart" => SessionStart,
        "SessionEnd" => SessionEnd,
        "UserPromptSubmit" => UserPromptSubmit,
        "Stop" => Stop,
        "StopFailure" => StopFailure,
        "PermissionRequest" => PermissionRequest,
        "Notification" => Notification,
        "Elicitation" => Elicitation,
        "SubagentStart" => SubagentStart,
        "SubagentStop" => SubagentStop,
        "TaskCreated" => TaskCreated,
        "TaskCompleted" => TaskCompleted,
        "CwdChanged" => CwdChanged,
        "WorktreeCreate" => WorktreeCreate,
        "WorktreeRemove" => WorktreeRemove,
        _ => return None,
    })
}

/// The intended UI status hint a given hook most directly implies (used to
/// pre-classify before the supervision reducer runs; the reducer remains
/// authoritative for `WaitingOnSubagents`). Pure mapping, no I/O.
pub fn status_hint_for_hook(hook_name: &str) -> Option<SessionStatus> {
    Some(match hook_name {
        "UserPromptSubmit" => SessionStatus::Working,
        "Elicitation" | "Notification" => SessionStatus::NeedsQuestion,
        "PermissionRequest" => SessionStatus::NeedsPermission,
        "StopFailure" => SessionStatus::Failed,
        // `Stop` deliberately omitted — its classification depends on outstanding
        // children/tasks and belongs to the supervision reducer.
        _ => return None,
    })
}

/// Render the POSIX-shell handler script body that every hook invokes. The
/// script reads the hook JSON on stdin, forwards it to the agent's hook-ingest
/// entrypoint (`termhub-agent` will gain a `--hook <EVENT>` mode, or a small
/// `termhub-hook` helper), and returns immediately so it never blocks Claude.
///
/// `agent_bin` is the resolved path to the handler entrypoint inside WSL.
///
/// SUBAGENT(claude-adapter): finalize the exact forwarding mechanism (a
/// `termhub-agent --hook <EVENT>` subcommand that appends to the journal is the
/// intended design) and harden the script (set -eu, no unbounded reads). The
/// shape below is the contract: stdin JSON in, fast append, exit 0.
pub fn handler_script(agent_bin: &str, hook_event: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # {marker} TermHub hook handler for {event}. Reads hook JSON on stdin,\n\
         # appends a journal entry via the agent, never blocks Claude.\n\
         set -eu\n\
         exec \"{bin}\" --hook {event}\n",
        marker = TERMHUB_HOOK_MARKER,
        event = hook_event,
        bin = agent_bin,
    )
}

/// Build the JSON fragment to merge into `~/.claude/settings.json` `hooks` for
/// all [`HOOK_EVENTS`], each pointing at the rendered handler and tagged with
/// [`TERMHUB_HOOK_MARKER`].
///
/// SUBAGENT(claude-adapter): produce the real per-event matcher objects in the
/// shape Claude expects (`{ "hooks": { "<Event>": [ { "hooks": [ { "type":
/// "command", "command": "<script>" } ] } ] } }`), each carrying the marker.
pub fn termhub_hooks_fragment(_agent_bin: &str) -> serde_json::Value {
    // Stub: an empty object. The real fragment is built by the subagent.
    serde_json::json!({ "hooks": {} })
}

/// Non-destructively merge the TermHub hooks into an existing settings.json
/// value, returning the merged value. Must preserve every non-TermHub key and
/// every user hook entry; only add/refresh entries tagged with the marker.
///
/// SUBAGENT(claude-adapter): implement the deep merge + idempotency (re-running
/// install must not duplicate entries) and round-trip-test against a settings
/// file that already has user hooks.
pub fn merge_into_settings(
    existing: &serde_json::Value,
    _agent_bin: &str,
) -> serde_json::Value {
    // Stub: return the existing settings unchanged (no-op merge) so callers
    // compile and an accidental call is non-destructive.
    existing.clone()
}

/// Remove exactly the TermHub-tagged hook entries from a settings.json value
/// (clean uninstall), leaving the user's own hooks intact.
///
/// SUBAGENT(claude-adapter): strip entries whose command contains the marker.
pub fn remove_from_settings(existing: &serde_json::Value) -> serde_json::Value {
    existing.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_list_has_the_verified_fifteen() {
        // The 0.5 set (REVIEW §9.6). Guards against accidental edits.
        assert_eq!(HOOK_EVENTS.len(), 15);
        assert!(HOOK_EVENTS.contains(&"SubagentStart"));
        assert!(HOOK_EVENTS.contains(&"SubagentStop"));
        assert!(HOOK_EVENTS.contains(&"TaskCreated"));
        assert!(HOOK_EVENTS.contains(&"TaskCompleted"));
        assert!(HOOK_EVENTS.contains(&"Elicitation"));
    }

    #[test]
    fn every_hook_maps_to_a_journal_event_type() {
        for name in HOOK_EVENTS {
            assert!(
                event_type_for_hook(name).is_some(),
                "hook {name} must map to a journal event type"
            );
        }
    }

    #[test]
    fn status_hints_are_specific() {
        assert_eq!(
            status_hint_for_hook("PermissionRequest"),
            Some(SessionStatus::NeedsPermission)
        );
        assert_eq!(
            status_hint_for_hook("Elicitation"),
            Some(SessionStatus::NeedsQuestion)
        );
        // Stop must NOT have a hint (supervision owns its classification).
        assert_eq!(status_hint_for_hook("Stop"), None);
    }

    #[test]
    fn handler_script_carries_marker_and_event() {
        let s = handler_script("/usr/local/bin/termhub-agent", "SessionStart");
        assert!(s.contains(TERMHUB_HOOK_MARKER));
        assert!(s.contains("--hook SessionStart"));
        assert!(s.starts_with("#!/usr/bin/env bash"));
    }
}
