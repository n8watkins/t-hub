//! The static MCP tool catalog T-Hub exposes (PRD §11.2 permission tiers).
//!
//! Each tool maps 1:1 to a control-channel **command name**: `tools/call` takes
//! the tool name + arguments and forwards `{command: <name>, args: <arguments>}`
//! to the app over the control channel. The MCP server therefore has no
//! compile-time coupling to the app's command implementations — this catalog is
//! the only place tools are declared, and the app dispatches them dynamically.
//!
//! Tiers (PRD §11.2):
//!   - **Read** (allowed): `list_terminals`, `get_status`, `wait_for_status`,
//!     `supervision_tree`, `wsl_health`, `search_files`, `list_tabs`,
//!     `list_captains`, `list_fleet_watches`, `read_terminal`.
//!   - **Organization** (allowed, audited): `focus_session`, `move_tile`,
//!     `rename_tab`, `new_tab`, `focus_tab`, `close_tab`, `claim_captain`,
//!     `release_captain`, `watch_fleet`, `unwatch_fleet`, `open_file`,
//!     `create_worktree`, `remove_worktree`.
//!   - **Process-changing** (confirmation required): `spawn_terminal`,
//!     `send_text`, `send_keys`, `close_terminal`.
//!   - **Theme**: `get_theme`, `set_theme` — forwarded by name verbatim.
//!
//! Process-changing / destructive tools carry an explicit confirmation note in
//! their `description` and are additionally gated on the app side; the
//! description is the user-facing contract, the app-side gate is the enforcement.

use serde_json::{json, Value};

/// The permission tier of a tool (PRD §11.2). Surfaced in the description and
/// used to annotate the tool so a client can reason about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Read-only. Allowed by default.
    Read,
    /// Reorganizes the workspace. Allowed, but emits a visible audit event.
    Organization,
    /// Changes a process (spawn/stop/resume/input). Confirmation required.
    ProcessChanging,
    /// Theme get/set — forwarded by name to the parallel theme track.
    Theme,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::Organization => "organization",
            Tier::ProcessChanging => "process-changing",
            Tier::Theme => "theme",
        }
    }
}

/// One tool definition: the MCP-facing name/description/schema plus its tier.
pub struct ToolDef {
    pub name: &'static str,
    pub tier: Tier,
    pub summary: &'static str,
    /// The JSON Schema for the tool's arguments (MCP `inputSchema`).
    pub input_schema: fn() -> Value,
}

impl ToolDef {
    /// Render this tool as an MCP `tools/list` entry. The description embeds the
    /// tier and, for process-changing tools, an explicit confirmation notice so
    /// the client surfaces it before calling.
    pub fn to_mcp(&self) -> Value {
        let mut description = format!("[{}] {}", self.tier.label(), self.summary);
        if self.tier == Tier::ProcessChanging {
            description.push_str(
                " — CONFIRMATION REQUIRED: this changes a running process. \
                 It is gated on the T-Hub side and will not execute without \
                 explicit permission (PRD §11.2).",
            );
        }
        if self.tier == Tier::Organization {
            description.push_str(" (audited: emits a visible audit event).");
        }
        json!({
            "name": self.name,
            "description": description,
            "inputSchema": (self.input_schema)(),
            // A non-standard annotation block clients may ignore; it carries the
            // tier so a permission-aware host can map it to its own policy.
            "annotations": {
                "t-hubTier": self.tier.label(),
                "confirmationRequired": self.tier == Tier::ProcessChanging,
            },
        })
    }
}

/// An empty-object schema (tools that take no arguments).
fn schema_empty() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

/// `{ sessionId: string }`.
fn schema_session_id() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Exact Claude/T-Hub session id." }
        },
        "required": ["sessionId"],
        "additionalProperties": false
    })
}

/// `wait_for_status` schema: long-poll until a session reaches a target FR-012
/// status (or a timeout). `targetStatus` accepts one camelCase status string or
/// an array of them; the poll returns as soon as the session matches any of them.
fn schema_wait_for_status() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId":    { "type": "string", "description": "Exact Claude/T-Hub session id to watch." },
            "targetStatus": {
                "description": "FR-012 status to wait for (camelCase, e.g. \"completed\", \"needsQuestion\", \"waitingOnSubagents\"). One string, or an array of strings to match any.",
                "oneOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                ]
            },
            "timeoutMs":    { "type": "integer", "minimum": 0, "description": "Max time to wait before returning with timedOut:true (default 30000)." }
        },
        "required": ["sessionId", "targetStatus"],
        "additionalProperties": false
    })
}

/// `search_files` schema.
fn schema_search_files() -> Value {
    json!({
        "type": "object",
        "properties": {
            "root":  { "type": "string", "description": "Absolute project root to index/search." },
            "query": { "type": "string", "description": "Fuzzy basename/path/extension query." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Max hits (default 20)." }
        },
        "required": ["root", "query"],
        "additionalProperties": false
    })
}

/// `open_file` schema.
fn schema_open_file() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Absolute path of the text file to open (read, capped)." }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

/// `focus_session` schema.
fn schema_focus_session() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Session to focus (switches tab + focuses its tile)." }
        },
        "required": ["sessionId"],
        "additionalProperties": false
    })
}

/// `move_tile` schema.
fn schema_move_tile() -> Value {
    json!({
        "type": "object",
        "properties": {
            "terminalId": { "type": "string", "description": "Terminal/tile id to move." },
            "tabId":      { "type": "string", "description": "Destination tab id." }
        },
        "required": ["terminalId", "tabId"],
        "additionalProperties": false
    })
}

/// `rename_tab` schema.
fn schema_rename_tab() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tabId": { "type": "string", "description": "Tab id to rename." },
            "name":  { "type": "string", "description": "New tab name." }
        },
        "required": ["tabId", "name"],
        "additionalProperties": false
    })
}

/// `read_terminal` schema: read a session's recent visible output (plain text).
fn schema_read_terminal() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId":    { "type": "string", "description": "Session/terminal id whose pane to read (the tmux target is th_<id>)." },
            "historyLines": { "type": "integer", "minimum": 0, "maximum": 10000, "description": "Lines of scrollback to include above the visible screen (default 0 = visible screen only)." }
        },
        "required": ["sessionId"],
        "additionalProperties": false
    })
}

/// `send_text` schema: type literal text (optionally submitting it) into a session.
fn schema_send_text() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Session/terminal id to type into (tmux target th_<id>)." },
            "text":      { "type": "string", "description": "Literal text to type into the session's pane." },
            "enter":     { "type": "boolean", "description": "Send a trailing Enter to submit the text (default true)." }
        },
        "required": ["sessionId", "text"],
        "additionalProperties": false
    })
}

/// `send_keys` schema: send named control keys (e.g. C-c, Up, Escape) to a session.
fn schema_send_keys() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Session/terminal id to send keys to (tmux target th_<id>)." },
            "keys": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "description": "tmux key names to send in order, e.g. [\"C-c\"], [\"Up\",\"Enter\"], [\"Escape\"]."
            }
        },
        "required": ["sessionId", "keys"],
        "additionalProperties": false
    })
}

/// `close_terminal` schema: kill a session/pane.
fn schema_close_terminal() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Session/terminal id to close (kills the tmux session th_<id> and its process tree)." }
        },
        "required": ["sessionId"],
        "additionalProperties": false
    })
}

/// `new_tab` schema.
fn schema_new_tab() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "Optional name for the new workspace tab (auto-named if omitted)." }
        },
        "additionalProperties": false
    })
}

/// `focus_tab` schema.
fn schema_focus_tab() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tabId": { "type": "string", "description": "Workspace tab id to activate." }
        },
        "required": ["tabId"],
        "additionalProperties": false
    })
}

/// `spawn_terminal` schema.
fn schema_spawn_terminal() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cwd":   { "type": "string", "description": "Working directory for the new terminal." },
            "shell": { "type": "string", "description": "Optional shell/command preset." },
            "name":  { "type": "string", "description": "Optional tile title." },
            "startupCommand": { "type": "string", "description": "Optional command run inside an interactive login shell the pane execs back into (e.g. claude --resume <id>)." },
            "tabName": { "type": "string", "description": "Optional target workspace tab, by name: reused if it exists, created (hidden - the user's active tab is NOT switched) if not." },
            "tabId":   { "type": "string", "description": "Optional target workspace tab, by id (must exist; see list_tabs). Defaults to the user's active tab." },
            "spawnedBy": { "type": "string", "description": "Optional captain session id: records the spawned session as that captain's CREW in the captains registry (requires the captain to have claim_captain'd; an unclaimed id records nothing - crewRecorded: false)." }
        },
        "additionalProperties": false
    })
}

/// `close_tab` schema: close a workspace tab headlessly.
fn schema_close_tab() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tabId":   { "type": "string", "description": "Workspace tab id to close (see list_tabs)." },
            "tabName": { "type": "string", "description": "Alternative to tabId: resolve the tab by exact name." },
            "force":   { "type": "boolean", "description": "Close even if the tab still holds tiles (their live sessions are re-adopted into the active tab, never orphaned). Default false: a non-empty tab is refused - close its terminals first." }
        },
        "additionalProperties": false
    })
}

/// `create_worktree` schema (WS-4): create a git worktree and open it as a new
/// workspace tab with a terminal spawned in the worktree dir.
fn schema_create_worktree() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repoRoot":     { "type": "string", "description": "Path inside the repo to create the worktree from (any path in the working tree)." },
            "worktreePath": { "type": "string", "description": "Absolute POSIX path for the new worktree's working-tree dir." },
            "branch":       { "type": "string", "description": "Optional branch to check out at the worktree (must not be checked out elsewhere). Omitted => git creates a new branch named after the path's final component." },
            "tabName":      { "type": "string", "description": "Optional name for the new workspace tab (defaults to the branch / final path component)." },
            "spawnedBy":    { "type": "string", "description": "Optional captain session id: records the worktree terminal as that captain's CREW in the captains registry (same contract as spawn_terminal's spawnedBy)." }
        },
        "required": ["repoRoot", "worktreePath"],
        "additionalProperties": false
    })
}

/// `claim_captain` schema (captain-chat phase 2): claim captaincy of a ship.
fn schema_claim_captain() -> Value {
    json!({
        "type": "object",
        "properties": {
            "captainSessionId": { "type": "string", "description": "The captain's own session/terminal id (the tmux target is th_<id>)." },
            "shipSlug":         { "type": "string", "description": "Optional ship name (slugified server-side; defaults to ship-<captainSessionId>). One captain per ship: a slug held by another captain is refused." },
            "workspaceTabIds":  { "type": "array", "items": { "type": "string" }, "description": "Optional workspace tab ids this captain controls (defaults to the tab currently holding the captain's tile)." }
        },
        "required": ["captainSessionId"],
        "additionalProperties": false
    })
}

/// `release_captain` schema (captain-chat phase 2): release a claimed captaincy.
fn schema_release_captain() -> Value {
    json!({
        "type": "object",
        "properties": {
            "captainSessionId": { "type": "string", "description": "The claiming session id to release." },
            "shipSlug":         { "type": "string", "description": "Alternative to captainSessionId: release by ship slug." }
        },
        "additionalProperties": false
    })
}

/// `watch_fleet` schema (orchestrator wake): arm a server-side push that
/// re-invokes THIS orchestrator's loop when a watched session changes state.
fn schema_watch_fleet() -> Value {
    json!({
        "type": "object",
        "properties": {
            "orchestratorSessionId": { "type": "string", "description": "YOUR OWN session/terminal id (where the wake is injected). Get it from list_terminals / list_captains." },
            "scope": {
                "description": "Which sessions to be woken about: \"captains\" (default - every claimed captain), \"all\" (every supervised session), or an array of specific session ids.",
                "oneOf": [
                    { "type": "string", "enum": ["captains", "all"] },
                    { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                ]
            },
            "states": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Which states fire a wake (camelCase, e.g. \"completed\", \"needsQuestion\", \"needsPermission\", \"failed\"). Omit for the default actionable set (idle/turn-complete, needs-input, completed/exited)."
            }
        },
        "required": ["orchestratorSessionId"],
        "additionalProperties": false
    })
}

/// `unwatch_fleet` schema (orchestrator wake): disarm this orchestrator's wake.
fn schema_unwatch_fleet() -> Value {
    json!({
        "type": "object",
        "properties": {
            "orchestratorSessionId": { "type": "string", "description": "The orchestrator session id whose watch to disarm (the one passed to watch_fleet)." }
        },
        "required": ["orchestratorSessionId"],
        "additionalProperties": false
    })
}

/// `remove_worktree` schema (WS-4): remove a git worktree (its live tiles are
/// detached first so no process is orphaned).
fn schema_remove_worktree() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repoRoot":     { "type": "string", "description": "Path inside the repo the worktree belongs to." },
            "worktreePath": { "type": "string", "description": "Absolute POSIX path of the worktree to remove." },
            "force":        { "type": "boolean", "description": "Force removal even with uncommitted changes (git refuses otherwise). Default false." }
        },
        "required": ["repoRoot", "worktreePath"],
        "additionalProperties": false
    })
}

/// `set_theme` schema.
fn schema_set_theme() -> Value {
    json!({
        "type": "object",
        "properties": {
            "theme": { "type": "string", "description": "Theme name/id to apply." }
        },
        "required": ["theme"],
        "additionalProperties": false
    })
}

/// The full catalog, in `tools/list` order.
pub fn catalog() -> Vec<ToolDef> {
    vec![
        // ---- Read tier --------------------------------------------------
        ToolDef {
            name: "list_terminals",
            tier: Tier::Read,
            summary: "List the live T-Hub terminals (tmux-backed sessions on the isolated socket).",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "get_status",
            tier: Tier::Read,
            summary: "Get the FR-012 status (+ latest statusline snapshot) for one session.",
            input_schema: schema_session_id,
        },
        ToolDef {
            name: "wait_for_status",
            tier: Tier::Read,
            summary: "Long-poll until a session reaches a target FR-012 status (or a timeout); returns the final status, elapsed ms, and whether it timed out.",
            input_schema: schema_wait_for_status,
        },
        ToolDef {
            name: "supervision_tree",
            tier: Tier::Read,
            summary: "Get the orchestrator→subagent supervision tree for one session.",
            input_schema: schema_session_id,
        },
        ToolDef {
            name: "wsl_health",
            tier: Tier::Read,
            summary: "Get a compact WSL host snapshot (RAM/swap/CPU/load/process count) + supervised-session count.",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "search_files",
            tier: Tier::Read,
            summary: "Fuzzy-search a project's indexed file paths (names + metadata only, never contents).",
            input_schema: schema_search_files,
        },
        ToolDef {
            name: "list_tabs",
            tier: Tier::Read,
            summary: "List the workspace tabs.",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "list_captains",
            tier: Tier::Read,
            summary: "List the claimed captains from the server captains registry ({shipSlug, captainSessionId, workspaceTabIds, crew} + revision).",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "list_fleet_watches",
            tier: Tier::Read,
            summary: "List the armed orchestrator wakes (who gets woken, for which sessions + states).",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "read_terminal",
            tier: Tier::Read,
            summary: "Read a session's recent visible output (plain text; optional scrollback) so you can see what it currently shows.",
            input_schema: schema_read_terminal,
        },
        ToolDef {
            name: "scribe_status",
            tier: Tier::Read,
            summary: "Is the general dictating right now? Reads the Scribe voice-gate status file and returns {listening, status, since}; fails open to listening=false when it can't tell (missing/stale/dead-pid file).",
            input_schema: schema_empty,
        },
        // ---- Organization tier -----------------------------------------
        ToolDef {
            name: "focus_session",
            tier: Tier::Organization,
            summary: "Focus a session: switch to its tab and focus its tile.",
            input_schema: schema_focus_session,
        },
        ToolDef {
            name: "move_tile",
            tier: Tier::Organization,
            summary: "Move a terminal tile to another tab (the process stays attached + alive).",
            input_schema: schema_move_tile,
        },
        ToolDef {
            name: "rename_tab",
            tier: Tier::Organization,
            summary: "Rename a workspace tab.",
            input_schema: schema_rename_tab,
        },
        ToolDef {
            name: "new_tab",
            tier: Tier::Organization,
            summary: "Create a new (empty) workspace tab in the background (use focus_tab to switch to it).",
            input_schema: schema_new_tab,
        },
        ToolDef {
            name: "focus_tab",
            tier: Tier::Organization,
            summary: "Activate a workspace tab by id.",
            input_schema: schema_focus_tab,
        },
        ToolDef {
            name: "close_tab",
            tier: Tier::Organization,
            summary: "Close a workspace tab (refused while it still holds tiles unless force; the last tab is never closed).",
            input_schema: schema_close_tab,
        },
        ToolDef {
            name: "claim_captain",
            tier: Tier::Organization,
            summary: "Claim captaincy of a ship in the server captains registry (one captain per ship; a captain self-registers with its own session id instead of hand-editing ship files).",
            input_schema: schema_claim_captain,
        },
        ToolDef {
            name: "release_captain",
            tier: Tier::Organization,
            summary: "Release a claimed captaincy (by captainSessionId or shipSlug; unknown claims are refused).",
            input_schema: schema_release_captain,
        },
        ToolDef {
            name: "watch_fleet",
            tier: Tier::Organization,
            summary: "Arm an orchestrator wake: T-Hub re-invokes YOUR loop (injects a prompt into your terminal) when a watched session (default: any captain) goes idle / needs-input / completes. Ends the need to poll. Idempotent; re-arming replaces the prior watch.",
            input_schema: schema_watch_fleet,
        },
        ToolDef {
            name: "unwatch_fleet",
            tier: Tier::Organization,
            summary: "Disarm the orchestrator wake previously armed with watch_fleet.",
            input_schema: schema_unwatch_fleet,
        },
        ToolDef {
            // Spawning a process is the process-changing subset of the
            // organization actions; it carries the confirmation contract.
            name: "spawn_terminal",
            tier: Tier::ProcessChanging,
            summary: "Spawn a new terminal in a directory (optionally into a named workspace tab, without switching the user's view).",
            input_schema: schema_spawn_terminal,
        },
        ToolDef {
            name: "send_text",
            tier: Tier::ProcessChanging,
            summary: "Type literal text into a session's terminal (optionally pressing Enter to submit it).",
            input_schema: schema_send_text,
        },
        ToolDef {
            name: "send_keys",
            tier: Tier::ProcessChanging,
            summary: "Send named control keys (e.g. C-c, Up, Escape) to a session's terminal.",
            input_schema: schema_send_keys,
        },
        ToolDef {
            name: "close_terminal",
            tier: Tier::ProcessChanging,
            summary: "Close a terminal: kill its tmux session and process tree.",
            input_schema: schema_close_terminal,
        },
        ToolDef {
            name: "open_file",
            tier: Tier::Organization,
            summary: "Open a text file in T-Hub's reader (returns capped contents + metadata).",
            input_schema: schema_open_file,
        },
        ToolDef {
            name: "create_worktree",
            tier: Tier::Organization,
            summary: "Create a git worktree at a path (optionally a branch), open it as a new workspace tab, and spawn a terminal in the worktree dir.",
            input_schema: schema_create_worktree,
        },
        ToolDef {
            name: "remove_worktree",
            tier: Tier::Organization,
            summary: "Remove a git worktree (detaching any live tiles first so no process is orphaned).",
            input_schema: schema_remove_worktree,
        },
        // ---- Theme ------------------------------------------------------
        ToolDef {
            name: "get_theme",
            tier: Tier::Theme,
            summary: "Get the current UI theme.",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "set_theme",
            tier: Tier::Theme,
            summary: "Set the UI theme.",
            input_schema: schema_set_theme,
        },
    ]
}

/// Look up a tool by name (so `tools/call` can validate the name before
/// forwarding, and reject unknown tools with a clear MCP error).
pub fn find(name: &str) -> Option<ToolDef> {
    catalog().into_iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_all_prd_tools() {
        let names: Vec<&str> = catalog().iter().map(|t| t.name).collect();
        for expected in [
            "list_terminals",
            "get_status",
            "wait_for_status",
            "supervision_tree",
            "wsl_health",
            "search_files",
            "list_tabs",
            "list_captains",
            "read_terminal",
            "scribe_status",
            "focus_session",
            "move_tile",
            "rename_tab",
            "new_tab",
            "focus_tab",
            "spawn_terminal",
            "send_text",
            "send_keys",
            "close_terminal",
            "open_file",
            "create_worktree",
            "remove_worktree",
            "claim_captain",
            "release_captain",
            "get_theme",
            "set_theme",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn new_process_changing_tools_demand_confirmation() {
        for name in ["send_text", "send_keys", "close_terminal"] {
            let mcp = find(name).unwrap().to_mcp();
            let desc = mcp["description"].as_str().unwrap();
            assert!(desc.contains("CONFIRMATION REQUIRED"), "{name} desc: {desc}");
            assert_eq!(mcp["annotations"]["confirmationRequired"], true, "{name}");
            assert_eq!(mcp["annotations"]["t-hubTier"], "process-changing", "{name}");
        }
    }

    #[test]
    fn read_terminal_is_read_tier_and_unconfirmed() {
        let mcp = find("read_terminal").unwrap().to_mcp();
        assert_eq!(mcp["annotations"]["t-hubTier"], "read");
        assert_eq!(mcp["annotations"]["confirmationRequired"], false);
    }

    #[test]
    fn process_changing_tool_description_demands_confirmation() {
        let spawn = find("spawn_terminal").unwrap();
        let mcp = spawn.to_mcp();
        let desc = mcp["description"].as_str().unwrap();
        assert!(desc.contains("CONFIRMATION REQUIRED"), "desc: {desc}");
        assert_eq!(mcp["annotations"]["confirmationRequired"], true);
        assert_eq!(mcp["annotations"]["t-hubTier"], "process-changing");
    }

    #[test]
    fn read_tools_are_not_confirmation_gated() {
        for name in ["list_terminals", "get_status", "wsl_health"] {
            let mcp = find(name).unwrap().to_mcp();
            assert_eq!(
                mcp["annotations"]["confirmationRequired"], false,
                "{name} should not require confirmation"
            );
        }
    }

    #[test]
    fn scribe_status_is_read_tier_and_unconfirmed() {
        let mcp = find("scribe_status").unwrap().to_mcp();
        assert_eq!(mcp["annotations"]["t-hubTier"], "read");
        assert_eq!(mcp["annotations"]["confirmationRequired"], false);
        // Takes no arguments (an empty object schema).
        let schema = (find("scribe_status").unwrap().input_schema)();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn organization_tools_note_audit() {
        let mcp = find("rename_tab").unwrap().to_mcp();
        assert!(mcp["description"].as_str().unwrap().contains("audited"));
    }

    #[test]
    fn captain_tools_carry_the_phase2_tiers() {
        // list_captains reads the registry; claim/release mutate it (audited,
        // organization tier - never confirmation-gated like process changes).
        let list = find("list_captains").unwrap().to_mcp();
        assert_eq!(list["annotations"]["t-hubTier"], "read");
        assert_eq!(list["annotations"]["confirmationRequired"], false);
        for name in ["claim_captain", "release_captain"] {
            let mcp = find(name).unwrap().to_mcp();
            assert_eq!(mcp["annotations"]["t-hubTier"], "organization", "{name}");
            assert_eq!(mcp["annotations"]["confirmationRequired"], false, "{name}");
            assert!(mcp["description"].as_str().unwrap().contains("audited"), "{name}");
        }
        let claim_schema = (find("claim_captain").unwrap().input_schema)();
        assert_eq!(claim_schema["required"], json!(["captainSessionId"]));
    }

    #[test]
    fn fleet_wake_tools_are_exposed_with_the_right_tiers() {
        // list_fleet_watches reads; watch/unwatch mutate (organization, audited,
        // never confirmation-gated).
        let list = find("list_fleet_watches").unwrap().to_mcp();
        assert_eq!(list["annotations"]["t-hubTier"], "read");
        assert_eq!(list["annotations"]["confirmationRequired"], false);
        for name in ["watch_fleet", "unwatch_fleet"] {
            let mcp = find(name).unwrap().to_mcp();
            assert_eq!(mcp["annotations"]["t-hubTier"], "organization", "{name}");
            assert_eq!(mcp["annotations"]["confirmationRequired"], false, "{name}");
            let schema = (find(name).unwrap().input_schema)();
            assert_eq!(
                schema["required"],
                json!(["orchestratorSessionId"]),
                "{name} keys the wake on the orchestrator's own id"
            );
        }
    }

    #[test]
    fn spawn_paths_expose_spawned_by_for_crew_linkage() {
        for name in ["spawn_terminal", "create_worktree"] {
            let schema = (find(name).unwrap().input_schema)();
            assert!(
                schema["properties"]["spawnedBy"].is_object(),
                "{name} must accept spawnedBy"
            );
        }
    }

    #[test]
    fn every_tool_has_an_object_schema() {
        for t in catalog() {
            let schema = (t.input_schema)();
            assert_eq!(schema["type"], "object", "tool {} schema", t.name);
        }
    }

    #[test]
    fn unknown_tool_is_not_found() {
        assert!(find("not_a_tool").is_none());
    }
}
