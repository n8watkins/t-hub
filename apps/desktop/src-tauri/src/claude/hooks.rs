//! Claude Code hook installation (PLAN.md Workstream B; REVIEW risk: "hook
//! install edits `~/.claude/settings.json`").
//!
//! The 0.5 hook set is the **verified** list from REVIEW.md §9.6 — every name
//! below is a real, current Claude Code hook event. Each fires a T-Hub handler
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
use t_hub_protocol::JournalEventType;

/// The exact hook event names T-Hub installs handlers for (PLAN.md §B,
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

/// A marker embedded in every T-Hub-authored settings.json hook entry so the
/// uninstaller can remove exactly our entries and leave the user's intact.
pub const T_HUB_HOOK_MARKER: &str = "__t_hub_managed__";

/// The marker pre-rename `termhub` builds wrote. We NEVER write it, but we still
/// RECOGNIZE it so a (re)install strips + migrates stale `termhub-agent` hook /
/// statusLine entries to the current marker + agent path. Those legacy entries
/// point at a `termhub-agent` binary the t-hub build no longer ships, so they
/// silently break the Claude statusline + hooks until migrated.
const LEGACY_HOOK_MARKER: &str = "__termhub_managed__";

/// True when a hook/statusLine `command` string is T-Hub-managed — under the
/// current marker OR the legacy `termhub` one (so reinstall self-heals old entries).
fn command_is_t_hub(command: &str) -> bool {
    command.contains(T_HUB_HOOK_MARKER) || command.contains(LEGACY_HOOK_MARKER)
}

// ---------------------------------------------------------------------------
// statusLine install (the Claude USAGE data source)
// ---------------------------------------------------------------------------
//
// Claude Code's `statusLine` is a SEPARATE setting from `hooks`: a single
// command Claude runs on every status refresh, piping a JSON payload (cost,
// context_window, rate_limits, ...) to its stdin. T-Hub installs a statusline
// that execs `t-hub-agent --statusline`, which journals a `StatusSnapshot`;
// the core then emits `status://snapshot` and the sidebar USAGE strip lights up.
//
// Without this, the 15 lifecycle hooks fire but NO statusline ever feeds the
// status bridge, so USAGE shows only dashes. We manage exactly one statusLine
// entry, tagged with the same marker (in `command`) so uninstall removes only
// ours and leaves a user-authored statusLine intact (we never clobber one).

/// Build the `statusLine` object value T-Hub installs. The marker is embedded
/// in the command string so [`statusline_is_t_hub`] / uninstall can identify
/// (and only remove) our entry. `refreshInterval` keeps cost/rate-limit numbers
/// fresh even between assistant messages (e.g. while a turn is running).
pub fn t_hub_statusline(agent_bin: &str) -> serde_json::Value {
    let command = format!(
        "{bin} --statusline # {marker}",
        bin = agent_bin,
        marker = T_HUB_HOOK_MARKER,
    );
    serde_json::json!({
        "type": "command",
        "command": command,
        "padding": 0,
        "refreshInterval": 5
    })
}

/// True when a `statusLine` value is T-Hub-managed (its `command` carries the
/// marker). Used to avoid clobbering a user's own statusLine and to drive a
/// clean uninstall.
pub fn statusline_is_t_hub(statusline: &serde_json::Value) -> bool {
    statusline
        .get("command")
        .and_then(|c| c.as_str())
        .map(command_is_t_hub)
        .unwrap_or(false)
}

/// Whether `settings` currently has a T-Hub-managed `statusLine` installed.
pub fn statusline_managed(settings: &serde_json::Value) -> bool {
    settings
        .get("statusLine")
        .map(statusline_is_t_hub)
        .unwrap_or(false)
}

/// Merge T-Hub's `statusLine` into `settings`, returning the new value.
///
/// Respects a user-authored statusLine: if `settings.statusLine` exists and is
/// NOT marker-tagged, we leave it untouched (the user chose their own). We only
/// set/overwrite our own (or an absent) statusLine — so re-install is idempotent
/// and never steals the slot from the user.
pub fn merge_statusline_into_settings(
    existing: &serde_json::Value,
    agent_bin: &str,
) -> serde_json::Value {
    let mut root: serde_json::Map<String, serde_json::Value> =
        existing.as_object().cloned().unwrap_or_default();

    let user_owns = root
        .get("statusLine")
        .map(|sl| !statusline_is_t_hub(sl))
        .unwrap_or(false);
    if !user_owns {
        root.insert("statusLine".to_string(), t_hub_statusline(agent_bin));
    }
    serde_json::Value::Object(root)
}

/// Remove a T-Hub-managed `statusLine` from `settings` (clean uninstall),
/// leaving a user-authored statusLine intact. Idempotent.
pub fn remove_statusline_from_settings(existing: &serde_json::Value) -> serde_json::Value {
    let mut root: serde_json::Map<String, serde_json::Value> =
        existing.as_object().cloned().unwrap_or_default();
    if root
        .get("statusLine")
        .map(statusline_is_t_hub)
        .unwrap_or(false)
    {
        root.remove("statusLine");
    }
    serde_json::Value::Object(root)
}

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
///
/// DEAD CODE (no callers; module is `#![allow(dead_code)]`). It must NOT be wired
/// as-is: `Notification` here maps unconditionally to `NeedsQuestion`, but Claude
/// Code fires that hook for informational pings too (the 60s `idle_prompt`), which
/// is exactly the spurious-cue bug the supervision reducer now guards against by
/// discriminating on `notification_type`. Any real wiring must thread the
/// `notification_type` discriminator through here the same way `Supervisor::ingest`
/// does, or defer the Notification classification entirely to the reducer.
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
/// entrypoint (`t-hub-agent` will gain a `--hook <EVENT>` mode, or a small
/// `t-hub-hook` helper), and returns immediately so it never blocks Claude.
///
/// `agent_bin` is the resolved path to the handler entrypoint inside WSL.
///
/// SUBAGENT(claude-adapter): finalize the exact forwarding mechanism (a
/// `t-hub-agent --hook <EVENT>` subcommand that appends to the journal is the
/// intended design) and harden the script (set -eu, no unbounded reads). The
/// shape below is the contract: stdin JSON in, fast append, exit 0.
pub fn handler_script(agent_bin: &str, hook_event: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # {marker} T-Hub hook handler for {event}. Reads hook JSON on stdin,\n\
         # appends a journal entry via the agent, never blocks Claude.\n\
         set -eu\n\
         exec \"{bin}\" --hook {event}\n",
        marker = T_HUB_HOOK_MARKER,
        event = hook_event,
        bin = agent_bin,
    )
}

/// Build the JSON fragment to merge into `~/.claude/settings.json` `hooks` for
/// all [`HOOK_EVENTS`], each pointing at the rendered handler and tagged with
/// [`T_HUB_HOOK_MARKER`].
///
/// ## Command-string convention
/// The `command` value placed into settings is a one-liner:
/// `<agent_bin> --hook <EVENT> # __t_hub_managed__`
///
/// This embeds the marker directly in the command string so `remove_from_settings`
/// can identify our entries by scanning command strings, without needing to parse
/// the script body. The `handler_script` function renders a fuller bash script
/// that a caller can write to disk; that script also carries the marker, but for
/// settings.json entries we use the compact one-liner.
///
/// ## Matcher-group shape emitted
/// ```json
/// {
///   "hooks": {
///     "SessionStart": [
///       { "matcher": "*", "hooks": [ { "type": "command", "command": "<cmd>" } ] }
///     ],
///     ...
///   }
/// }
/// ```
/// `"matcher": "*"` is included on every event for consistency; Claude Code
/// ignores it on events that don't support matchers, so it is harmless.
pub fn t_hub_hooks_fragment(agent_bin: &str) -> serde_json::Value {
    t_hub_hooks_fragment_for(agent_bin, HOOK_EVENTS)
}

/// Like [`t_hub_hooks_fragment`] but for an explicit subset of events (the
/// user's hook selection). Building from a subset lets the installer manage only
/// the chosen events.
pub fn t_hub_hooks_fragment_for(agent_bin: &str, events: &[&str]) -> serde_json::Value {
    let mut events_map = serde_json::Map::new();
    for event in events {
        let command = format!(
            "{bin} --hook {event} # {marker}",
            bin = agent_bin,
            event = event,
            marker = T_HUB_HOOK_MARKER,
        );
        let group = serde_json::json!([{
            "matcher": "*",
            "hooks": [{ "type": "command", "command": command }]
        }]);
        events_map.insert(event.to_string(), group);
    }
    serde_json::json!({ "hooks": events_map })
}

/// item-3 Pillar C: the BLOCKING `PreToolUse` gate event. Unlike the observe-only
/// [`HOOK_EVENTS`] (which fire `--hook <EVENT>` and never block), this installs a
/// single blocking hook that runs `t-hub-agent --gate` on every `Bash` tool call and
/// can DENY an outward-facing command a crew may not run. It is kept out of
/// `HOOK_EVENTS` so the two classes never mix, but rides the SAME managed-marker,
/// consent, and clean-uninstall discipline (the marker in its command string means
/// [`remove_from_settings`] strips it exactly like every other T-Hub entry).
pub const PRETOOLUSE_GATE_EVENT: &str = "PreToolUse";

/// The `command` string for the blocking gate hook: `<agent_bin> --gate # <marker>`.
pub fn t_hub_gate_command(agent_bin: &str) -> String {
    format!("{agent_bin} --gate # {T_HUB_HOOK_MARKER}")
}

/// Install (idempotently) the blocking `PreToolUse` gate group into `existing`,
/// matcher `Bash`. Any pre-existing T-Hub `PreToolUse` group is dropped first (so a
/// reinstall does not duplicate it) and user-authored `PreToolUse` groups are
/// preserved. Mirrors [`merge_statusline_into_settings`]'s additive, non-destructive
/// shape; [`remove_from_settings`] already removes it by marker.
pub fn merge_gate_into_settings(
    existing: &serde_json::Value,
    agent_bin: &str,
) -> serde_json::Value {
    let mut root: serde_json::Map<String, serde_json::Value> =
        existing.as_object().cloned().unwrap_or_default();
    let hooks_obj: &mut serde_json::Map<String, serde_json::Value> = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("hooks must be an object");
    let arr: &mut Vec<serde_json::Value> = hooks_obj
        .entry(PRETOOLUSE_GATE_EVENT.to_string())
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .expect("PreToolUse value must be an array");
    arr.retain(|g| !group_is_t_hub(g));
    arr.push(serde_json::json!({
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": t_hub_gate_command(agent_bin) }]
    }));
    serde_json::Value::Object(root)
}

/// Remove ONLY the T-Hub-managed blocking gate (the `PreToolUse` group), leaving the
/// observe-only hooks, the statusLine, and any user-authored `PreToolUse` hook intact.
/// The distinct gate opt-OUT (mirrors [`merge_gate_into_settings`]).
pub fn remove_gate_from_settings(existing: &serde_json::Value) -> serde_json::Value {
    let mut root: serde_json::Map<String, serde_json::Value> =
        existing.as_object().cloned().unwrap_or_default();
    if let Some(hooks_obj) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        if let Some(arr) = hooks_obj
            .get_mut(PRETOOLUSE_GATE_EVENT)
            .and_then(|v| v.as_array_mut())
        {
            arr.retain(|g| !group_is_t_hub(g));
            if arr.is_empty() {
                hooks_obj.remove(PRETOOLUSE_GATE_EVENT);
            }
        }
    }
    serde_json::Value::Object(root)
}

/// Whether the blocking `PreToolUse` gate is currently installed (a T-Hub-managed
/// group under `PreToolUse`).
pub fn gate_managed(settings: &serde_json::Value) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get(PRETOOLUSE_GATE_EVENT))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().any(group_is_t_hub))
        .unwrap_or(false)
}

/// The subset of [`HOOK_EVENTS`] currently managed by T-Hub in `settings`
/// (each event whose group array contains a marker-tagged command). Lets the UI
/// pre-check exactly the installed events.
pub fn managed_events(settings: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) {
        for event in HOOK_EVENTS {
            if let Some(arr) = hooks.get(*event).and_then(|v| v.as_array()) {
                if arr.iter().any(group_is_t_hub) {
                    out.push(event.to_string());
                }
            }
        }
    }
    out
}

/// True when `settings` contains ANY T-Hub-managed entry — a hook group OR a
/// statusLine — under the CURRENT marker OR the legacy `termhub` one (via
/// [`command_is_t_hub`]). This is the "has the user ever consented to T-Hub
/// hooks?" probe the startup reconcile uses to decide whether to migrate stale
/// `termhub` entries WITHOUT silently installing where none existed. Read-only.
pub fn any_managed(settings: &serde_json::Value) -> bool {
    if statusline_managed(settings) {
        return true;
    }
    settings
        .get("hooks")
        .and_then(|h| h.as_object())
        .map(|hooks| {
            hooks.values().any(|groups| {
                groups
                    .as_array()
                    .map(|arr| arr.iter().any(group_is_t_hub))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// True when any T-Hub-managed entry is STALE and the startup reconcile should
/// rewrite it: a command carries the legacy `termhub` marker, OR it doesn't already
/// point at `agent_bin`. False when every managed hook + statusLine already matches
/// the current marker + path — so the reconcile can NO-OP and avoid rewriting
/// settings.json (and bumping its mtime) on every launch. Read-only.
pub fn managed_stale(settings: &serde_json::Value, agent_bin: &str) -> bool {
    let cmd_stale = |c: &str| c.contains(LEGACY_HOOK_MARKER) || !c.contains(agent_bin);
    // statusLine
    if let Some(sl) = settings.get("statusLine") {
        if statusline_is_t_hub(sl) {
            let c = sl.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if cmd_stale(c) {
                return true;
            }
        }
    }
    // Every managed hook command across all groups.
    let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    for groups in hooks.values() {
        let Some(arr) = groups.as_array() else {
            continue;
        };
        for g in arr {
            let Some(inner) = g.get("hooks").and_then(|h| h.as_array()) else {
                continue;
            };
            for h in inner {
                if let Some(c) = h.get("command").and_then(|c| c.as_str()) {
                    if command_is_t_hub(c) && cmd_stale(c) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Return true if a matcher-group object contains any T-Hub-managed command.
///
/// A group is T-Hub-managed when at least one of its inner `hooks[].command`
/// strings contains [`T_HUB_HOOK_MARKER`] or the legacy `termhub` marker (via
/// [`command_is_t_hub`]).
pub fn group_is_t_hub(group: &serde_json::Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|inner_hooks| {
            inner_hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(command_is_t_hub)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Non-destructively merge the T-Hub hooks into an existing settings.json
/// value, returning the merged value.
///
/// ## Algorithm
/// 1. Clone `existing` (or start from `{}` if it is not an object).
/// 2. Ensure a top-level `"hooks"` object exists, preserving all other keys.
/// 3. For each event in [`HOOK_EVENTS`]:
///    - Keep the event's current array (user-authored groups survive).
///    - Remove any pre-existing T-Hub groups (identified by the marker) to
///      avoid duplicates on re-install.
///    - Append our fresh T-Hub matcher-group.
///
/// ## Idempotency
/// Running install twice yields exactly ONE T-Hub group per event (the old one
/// is dropped before the fresh one is appended), while user groups are never
/// touched.
///
/// ## Preservation
/// Every non-hook top-level key in `existing` (e.g. `model`, `permissions`,
/// `cleanupPeriodDays`) is carried through unchanged.
pub fn merge_into_settings(existing: &serde_json::Value, agent_bin: &str) -> serde_json::Value {
    merge_into_settings_for(existing, agent_bin, HOOK_EVENTS)
}

/// Like [`merge_into_settings`] but manages only the given `events` subset (the
/// user's hook selection). Events not listed are left untouched here; the
/// installer first strips ALL T-Hub hooks then merges the selection, so the
/// managed set ends up exactly equal to `events`.
pub fn merge_into_settings_for(
    existing: &serde_json::Value,
    agent_bin: &str,
    events: &[&str],
) -> serde_json::Value {
    // Start from existing (clone) or an empty object if existing is not an object.
    let mut root: serde_json::Map<String, serde_json::Value> =
        existing.as_object().cloned().unwrap_or_default();

    // Ensure the top-level "hooks" key is an object.
    let hooks_obj: &mut serde_json::Map<String, serde_json::Value> = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("hooks must be an object");

    let fragment = t_hub_hooks_fragment_for(agent_bin, events);
    let fragment_hooks = fragment["hooks"].as_object().expect("fragment has hooks");

    for event in events {
        // Get the new T-Hub group for this event from the fragment.
        let new_t_hub_group = &fragment_hooks[*event].as_array().expect("array")[0].clone();

        // Get or create the event's group array.
        let event_array: &mut Vec<serde_json::Value> = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .expect("event value must be an array");

        // Drop any pre-existing T-Hub groups (idempotency).
        event_array.retain(|g| !group_is_t_hub(g));

        // Append the fresh T-Hub group.
        event_array.push(new_t_hub_group.clone());
    }

    serde_json::Value::Object(root)
}

/// Remove exactly the T-Hub-tagged hook entries from a settings.json value
/// (clean uninstall), leaving user-authored hooks and all non-hook keys intact.
///
/// For each event array under `hooks`, matcher-groups whose any inner
/// `hooks[].command` contains [`T_HUB_HOOK_MARKER`] are dropped. If an event's
/// array becomes empty the event key is removed entirely. All user (non-marker)
/// groups are preserved, as are all top-level keys outside `hooks`.
pub fn remove_from_settings(existing: &serde_json::Value) -> serde_json::Value {
    let mut root: serde_json::Map<String, serde_json::Value> =
        existing.as_object().cloned().unwrap_or_default();

    if let Some(hooks_val) = root.get_mut("hooks") {
        if let Some(hooks_obj) = hooks_val.as_object_mut() {
            // For each event, drop T-Hub groups.
            hooks_obj.retain(|_event, groups_val| {
                if let Some(groups) = groups_val.as_array_mut() {
                    groups.retain(|g| !group_is_t_hub(g));
                    // Remove the event key if no groups remain.
                    !groups.is_empty()
                } else {
                    // Non-array value: leave untouched.
                    true
                }
            });
        }
    }

    serde_json::Value::Object(root)
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
    fn gate_installs_a_blocking_pretooluse_bash_hook_and_uninstalls_cleanly() {
        // item-3 Pillar C: the gate installs one blocking PreToolUse/Bash hook running
        // `--gate` with the managed marker; a user's own PreToolUse hook is preserved,
        // and remove_from_settings strips exactly the gate. BYPASS-WOULD-FAIL: drop the
        // merge_gate call and the "gate present" assert goes RED.
        let bin = "/usr/local/bin/t-hub-agent";
        // A settings.json that already has a user-authored PreToolUse hook.
        let existing = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Write", "hooks": [{ "type": "command", "command": "my-linter" }] }
                ]
            }
        });
        let installed = merge_gate_into_settings(&existing, bin);
        assert!(gate_managed(&installed), "the gate must be installed");
        let arr = installed["hooks"]["PreToolUse"].as_array().unwrap();
        // Both the user hook and our gate are present.
        assert_eq!(arr.len(), 2, "the user's PreToolUse hook must be preserved");
        let gate = arr
            .iter()
            .find(|g| group_is_t_hub(g))
            .expect("gate group present");
        assert_eq!(gate["matcher"], "Bash");
        assert!(gate["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("--gate"));

        // Idempotent: re-installing does not duplicate the gate.
        let reinstalled = merge_gate_into_settings(&installed, bin);
        assert_eq!(
            reinstalled["hooks"]["PreToolUse"].as_array().unwrap().len(),
            2
        );

        // Clean uninstall: the gate is stripped, the user hook stays.
        let removed = remove_from_settings(&reinstalled);
        assert!(
            !gate_managed(&removed),
            "the gate must be removed on uninstall"
        );
        let arr = removed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["matcher"], "Write");
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

    // ---------------------------------------------------------------------------
    // statusLine install / merge / remove
    // ---------------------------------------------------------------------------

    #[test]
    fn t_hub_statusline_is_command_with_marker_and_flag() {
        let sl = t_hub_statusline("/usr/local/bin/t-hub-agent");
        assert_eq!(sl["type"].as_str(), Some("command"));
        let cmd = sl["command"].as_str().expect("command string");
        assert!(cmd.contains(T_HUB_HOOK_MARKER), "must carry marker");
        assert!(cmd.contains("--statusline"), "must invoke --statusline");
        assert!(statusline_is_t_hub(&sl));
    }

    #[test]
    fn merge_statusline_into_empty_installs_ours() {
        let bin = "/usr/local/bin/t-hub-agent";
        let out = merge_statusline_into_settings(&serde_json::json!({}), bin);
        assert!(statusline_managed(&out));
        assert!(out["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("--statusline"));
    }

    #[test]
    fn merge_statusline_preserves_user_statusline() {
        let bin = "/usr/local/bin/t-hub-agent";
        let existing = serde_json::json!({
            "statusLine": { "type": "command", "command": "my-own-status.sh" }
        });
        let out = merge_statusline_into_settings(&existing, bin);
        // User's statusLine must be left intact (not stolen).
        assert_eq!(
            out["statusLine"]["command"].as_str(),
            Some("my-own-status.sh")
        );
        assert!(!statusline_managed(&out));
    }

    #[test]
    fn merge_statusline_is_idempotent_over_ours() {
        let bin = "/usr/local/bin/t-hub-agent";
        let once = merge_statusline_into_settings(&serde_json::json!({}), bin);
        let twice = merge_statusline_into_settings(&once, bin);
        assert_eq!(once, twice);
        assert!(statusline_managed(&twice));
    }

    #[test]
    fn remove_statusline_strips_ours_keeps_user() {
        let bin = "/usr/local/bin/t-hub-agent";
        // Ours is removed.
        let ours = merge_statusline_into_settings(&serde_json::json!({}), bin);
        let cleaned = remove_statusline_from_settings(&ours);
        assert!(cleaned.get("statusLine").is_none());
        // User's is kept.
        let user = serde_json::json!({
            "statusLine": { "type": "command", "command": "my-own-status.sh" }
        });
        let kept = remove_statusline_from_settings(&user);
        assert_eq!(
            kept["statusLine"]["command"].as_str(),
            Some("my-own-status.sh")
        );
    }

    #[test]
    fn handler_script_carries_marker_and_event() {
        let s = handler_script("/usr/local/bin/t-hub-agent", "SessionStart");
        assert!(s.contains(T_HUB_HOOK_MARKER));
        assert!(s.contains("--hook SessionStart"));
        assert!(s.starts_with("#!/usr/bin/env bash"));
    }

    // ---------------------------------------------------------------------------
    // t_hub_hooks_fragment
    // ---------------------------------------------------------------------------

    #[test]
    fn fragment_has_all_15_events_with_marker() {
        let bin = "/usr/local/bin/t-hub-agent";
        let frag = t_hub_hooks_fragment(bin);
        let hooks = frag["hooks"].as_object().expect("hooks must be object");
        assert_eq!(hooks.len(), HOOK_EVENTS.len());
        for event in HOOK_EVENTS {
            let groups = hooks[*event].as_array().expect("event must be array");
            assert_eq!(groups.len(), 1, "event {event} should have exactly 1 group");
            let cmd = groups[0]["hooks"][0]["command"]
                .as_str()
                .expect("command must be string");
            assert!(
                cmd.contains(T_HUB_HOOK_MARKER),
                "command for {event} must contain marker"
            );
            assert!(
                cmd.contains(&format!("--hook {event}")),
                "command for {event} must contain --hook <EVENT>"
            );
            assert_eq!(
                groups[0]["matcher"].as_str(),
                Some("*"),
                "matcher for {event} must be *"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // merge_into_settings — empty base
    // ---------------------------------------------------------------------------

    #[test]
    fn merge_into_empty_produces_all_15_events() {
        let bin = "/usr/local/bin/t-hub-agent";
        let result = merge_into_settings(&serde_json::json!({}), bin);
        let hooks = result["hooks"].as_object().expect("hooks must be object");
        assert_eq!(
            hooks.len(),
            HOOK_EVENTS.len(),
            "all 15 events must be present after merging into empty"
        );
        for event in HOOK_EVENTS {
            let groups = hooks[*event].as_array().expect("event must be array");
            assert_eq!(groups.len(), 1);
            let cmd = groups[0]["hooks"][0]["command"]
                .as_str()
                .expect("command string");
            assert!(
                cmd.contains(T_HUB_HOOK_MARKER),
                "command for {event} must carry marker"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // merge_into_settings — preservation
    // ---------------------------------------------------------------------------

    #[test]
    fn merge_preserves_user_hooks_and_non_hook_keys() {
        let bin = "/usr/local/bin/t-hub-agent";

        // Pre-existing settings: a non-hook keys, a user PreToolUse group, and a
        // user Stop group (no marker — this is a user-authored Stop handler).
        let existing = serde_json::json!({
            "model": "opus",
            "cleanupPeriodDays": 30,
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo user_pretooluse" }] }
                ],
                "Stop": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "echo user_stop_handler" }] }
                ]
            }
        });

        let result = merge_into_settings(&existing, bin);

        // Non-hook keys must be preserved.
        assert_eq!(result["model"].as_str(), Some("opus"));
        assert_eq!(result["cleanupPeriodDays"].as_u64(), Some(30));

        let hooks = result["hooks"].as_object().expect("hooks");

        // User PreToolUse group must survive (PreToolUse is not in HOOK_EVENTS,
        // so it should be left completely untouched).
        let pretooluse = hooks["PreToolUse"].as_array().expect("array");
        assert_eq!(
            pretooluse.len(),
            1,
            "user PreToolUse group must be preserved"
        );
        assert_eq!(
            pretooluse[0]["hooks"][0]["command"].as_str(),
            Some("echo user_pretooluse")
        );

        // User Stop group (no marker) must survive alongside the T-Hub Stop group.
        let stop_groups = hooks["Stop"].as_array().expect("array");
        let user_stop_groups: Vec<_> = stop_groups.iter().filter(|g| !group_is_t_hub(g)).collect();
        assert_eq!(
            user_stop_groups.len(),
            1,
            "user Stop group must be preserved"
        );
        assert_eq!(
            user_stop_groups[0]["hooks"][0]["command"].as_str(),
            Some("echo user_stop_handler")
        );

        // T-Hub Stop group must also be present.
        let t_hub_stop_groups: Vec<_> = stop_groups.iter().filter(|g| group_is_t_hub(g)).collect();
        assert_eq!(
            t_hub_stop_groups.len(),
            1,
            "T-Hub Stop group must be present"
        );
    }

    // ---------------------------------------------------------------------------
    // merge_into_settings — idempotency
    // ---------------------------------------------------------------------------

    #[test]
    fn double_merge_produces_exactly_one_t_hub_group_per_event() {
        let bin = "/usr/local/bin/t-hub-agent";
        let base = serde_json::json!({});

        // First merge.
        let after_first = merge_into_settings(&base, bin);
        // Second merge on top of the first result.
        let after_second = merge_into_settings(&after_first, bin);

        let hooks = after_second["hooks"].as_object().expect("hooks");
        for event in HOOK_EVENTS {
            let groups = hooks[*event].as_array().expect("array");
            let t_hub_count = groups.iter().filter(|g| group_is_t_hub(g)).count();
            assert_eq!(
                t_hub_count, 1,
                "event {event} must have exactly 1 T-Hub group after double-merge, got {t_hub_count}"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // remove_from_settings
    // ---------------------------------------------------------------------------

    #[test]
    fn remove_strips_t_hub_groups_and_preserves_user_entries() {
        let bin = "/usr/local/bin/t-hub-agent";

        // Build the same "existing" settings as the preservation test, then merge.
        let existing = serde_json::json!({
            "model": "opus",
            "cleanupPeriodDays": 30,
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo user_pretooluse" }] }
                ],
                "Stop": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "echo user_stop_handler" }] }
                ]
            }
        });
        let merged = merge_into_settings(&existing, bin);

        // Now remove T-Hub entries.
        let cleaned = remove_from_settings(&merged);

        // Non-hook keys preserved.
        assert_eq!(cleaned["model"].as_str(), Some("opus"));
        assert_eq!(cleaned["cleanupPeriodDays"].as_u64(), Some(30));

        let hooks = cleaned["hooks"].as_object().expect("hooks");

        // PreToolUse: user group remains, no T-Hub group was ever there.
        let pretooluse = hooks["PreToolUse"].as_array().expect("array");
        assert_eq!(pretooluse.len(), 1);
        assert_eq!(
            pretooluse[0]["hooks"][0]["command"].as_str(),
            Some("echo user_pretooluse")
        );

        // Stop: T-Hub group stripped, user group intact.
        let stop_groups = hooks["Stop"].as_array().expect("array");
        assert_eq!(stop_groups.len(), 1, "only user Stop group should remain");
        assert_eq!(
            stop_groups[0]["hooks"][0]["command"].as_str(),
            Some("echo user_stop_handler")
        );

        // All 15 T-Hub-managed events should have no T-Hub groups remaining.
        for event in HOOK_EVENTS {
            if let Some(groups_val) = hooks.get(*event) {
                let groups = groups_val.as_array().expect("array");
                let t_hub_count = groups.iter().filter(|g| group_is_t_hub(g)).count();
                assert_eq!(
                    t_hub_count, 0,
                    "event {event} must have 0 T-Hub groups after remove"
                );
            }
            // If the key was removed entirely (empty array → key removed), that's also fine.
        }
    }
}
