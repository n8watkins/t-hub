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
//!     `list_captains`, `list_projects`,
//!     `list_fleet_watches`, `read_terminal`,
//!     `my_capability`.
//!   - **Organization** (allowed, audited): `focus_session`, `history_list`,
//!     `history_focus`, `move_tile`,
//!     `rename_tab`, `new_tab`, `focus_tab`, `close_tab`, `claim_captain`,
//!     `release_captain`, `watch_fleet`, `unwatch_fleet`, `open_file`,
//!     `create_worktree`, `remove_worktree`, `register_project`,
//!     `captain_bootstrap` and the agent-session operations.
//!   - **Process-changing** (confirmation required): `spawn_terminal`,
//!     `history_resume`, `start_agent`,
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

fn schema_history_list() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Optional case-insensitive label, path, or preview filter." },
            "harness": { "type": "string", "enum": ["claude", "codex"], "description": "Optional exact Harness filter." },
            "includeArchived": { "type": "boolean", "description": "Include archived conversations (default true)." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum rows to return (default 100)." }
        },
        "additionalProperties": false
    })
}

fn schema_history_focus() -> Value {
    json!({
        "type": "object",
        "properties": {
            "historyId": { "type": "string", "minLength": 1, "description": "Opaque exact History identity returned by history_list." }
        },
        "required": ["historyId"],
        "additionalProperties": false
    })
}

fn schema_history_resume() -> Value {
    json!({
        "type": "object",
        "properties": {
            "historyId": { "type": "string", "minLength": 1, "description": "Opaque exact History identity returned by history_list." },
            "requestId": { "type": "string", "minLength": 1, "maxLength": 128, "pattern": "^[A-Za-z0-9_:-]+$", "description": "Stable idempotency key retained through ambiguous response recovery." },
            "targetTabId": { "type": "string", "minLength": 1, "description": "Optional existing destination workspace tab." }
        },
        "required": ["historyId", "requestId"],
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
            "sessionId": { "type": "string", "description": "Session/terminal id to close (kills the tmux session th_<id> and its process tree)." },
            "force": { "type": "boolean", "description": "Operator escape (default false). When the liveness probe times out under a degraded control plane, close normally REFUSES (retryable). Set force:true to reap a session you KNOW is dead: it re-probes once and reaps unless the re-probe CONFIRMS the session Alive. WARNING: under a sustained wedge a live-but-slow session's re-probe also times out (indistinguishable from dead) and force WILL reap it - use force ONLY when you believe the session is dead. Try a normal close first." },
            "approvalId": { "type": "string", "minLength": 1, "description": "One-time exact approval issued by the delegating supervisor. Required only when a Ship Admin or Fleet Admin performs this cleanup." }
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
            "spawnedBy": { "type": "string", "description": "Optional captain session id: records the spawned session as that captain's CREW in the captains registry (requires the captain to have claim_captain'd; an unclaimed id records nothing - crewRecorded: false)." },
            "capability": { "type": "string", "enum": ["read", "control"], "description": "Capability the new session is granted (item-3 least-privilege, default \"read\"): \"read\" spawns a pure-work crew that can observe but not spawn/type/kill; \"control\" is a deliberate, audited elevation for a session that must orchestrate (e.g. a captain/orchestrator). Omitted defaults to \"read\"." }
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
            "startupCommand": { "type": "string", "description": "Optional command the worktree terminal runs inside an interactive login shell it execs back into (e.g. claude --resume <id>) - same contract and exec path as spawn_terminal's startupCommand. Omitted boots a bare shell in the worktree dir." },
            "spawnedBy":    { "type": "string", "description": "Optional captain session id: records the worktree terminal as that captain's CREW in the captains registry (same contract as spawn_terminal's spawnedBy)." },
            "capability":   { "type": "string", "enum": ["read", "control"], "description": "Capability the worktree terminal is granted (item-3 least-privilege, default \"read\"): same contract as spawn_terminal's capability - \"control\" is a deliberate, audited elevation." }
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
            "provider":         { "type": "string", "enum": ["codex", "claude"], "description": "Harness that owns providerSessionId. Legacy callers default to Claude." },
            "providerSessionId": { "type": "string", "description": "Optional provider-native conversation id, such as CODEX_THREAD_ID or a Claude session UUID." },
            "workspaceTabIds":  { "type": "array", "items": { "type": "string" }, "description": "Optional existing Work Workspace ids this Captain owns. No placement, cwd, or active-tab inference occurs when omitted." }
        },
        "required": ["captainSessionId"],
        "additionalProperties": false
    })
}

fn schema_rename_captain() -> Value {
    json!({
        "type": "object",
        "properties": {
            "captainSessionId": { "type": "string", "description": "Current Captain terminal id." },
            "shipSlug": { "type": "string", "description": "Alternative durable Captain ship slug." },
            "displayName": { "type": "string", "minLength": 1, "maxLength": 120, "description": "Durable trimmed Captain display name." }
        },
        "required": ["displayName"],
        "anyOf": [
            { "required": ["captainSessionId"] },
            { "required": ["shipSlug"] }
        ],
        "additionalProperties": false
    })
}

fn schema_register_project() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repoRoot": { "type": "string", "description": "Path inside an existing Git repository, or an existing folder when initializeGit is explicitly true; T-Hub resolves its canonical main worktree." },
            "createDirectory": { "type": "boolean", "description": "Explicitly create repoRoot as one absent leaf for a new empty codebase. Requires initializeGit: true and never replaces an existing path." },
            "initializeGit": { "type": "boolean", "description": "Explicitly initialize Git with main as the default branch when repoRoot is not already a repository. Defaults to false and never replaces an existing .git entry." },
            "name": { "type": "string", "description": "Optional display name; defaults to the repository directory name." },
            "remoteUrl": { "type": "string", "description": "Optional canonical Git remote URL." },
        },
        "required": ["repoRoot"],
        "additionalProperties": false
    })
}

fn schema_captain_bootstrap() -> Value {
    json!({
        "type": "object",
        "properties": {
            "shipSlug": { "type": "string", "description": "Durable ship slug to recover." },
            "captainSessionId": { "type": "string", "description": "Alternative current Captain terminal id." }
        },
        "anyOf": [
            { "required": ["captainSessionId"] },
            { "required": ["shipSlug"] }
        ],
        "additionalProperties": false
    })
}

fn schema_list_agents() -> Value {
    json!({
        "type": "object",
        "properties": {
            "captainSessionId": { "type": "string" },
            "projectId": { "type": "string" },
            "cursor": { "type": "string", "pattern": "^[0-9]+$", "default": "0" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
            "state": { "type": "string", "enum": ["active", "removed"] }
        },
        "anyOf": [
            { "required": ["captainSessionId"] },
            { "required": ["projectId"] }
        ],
        "additionalProperties": false
    })
}

fn schema_lane_claim() -> Value {
    json!({
        "type": "object",
        "properties": {
            "laneId": { "type": "string", "minLength": 1 },
            "ownerId": { "type": "string", "minLength": 1 },
            "dependencies": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "uniqueItems": true
            },
            "mutableFiles": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "uniqueItems": true
            },
            "mutableSchemas": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "uniqueItems": true
            },
            "mutableInterfaces": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "uniqueItems": true
            }
        },
        "required": ["laneId", "ownerId", "dependencies"],
        "additionalProperties": false
    })
}

fn schema_integration_contract() -> Value {
    json!({
        "type": "object",
        "properties": {
            "contractId": { "type": "string", "minLength": 1 },
            "integrationOwner": { "type": "string", "minLength": 1 },
            "orderedLaneIds": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "minItems": 2,
                "uniqueItems": true
            }
        },
        "required": ["contractId", "integrationOwner", "orderedLaneIds"],
        "additionalProperties": false
    })
}

fn schema_dispatch_preflight() -> Value {
    json!({
        "type": "object",
        "properties": {
            "projectId": { "type": "string", "minLength": 1 },
            "requestedLanes": {
                "type": "array",
                "items": schema_lane_claim(),
                "minItems": 1
            },
            "integrationContracts": {
                "type": "array",
                "items": schema_integration_contract()
            }
        },
        "required": ["projectId", "requestedLanes", "integrationContracts"],
        "additionalProperties": false
    })
}

fn schema_admin_operation() -> Value {
    json!({
        "type": "string",
        "enum": [
            "inspectStatus",
            "maintainSession",
            "cleanupSession",
            "recoverResource",
            "maintainWorktree",
            "cleanupWorktree",
            "prepareRetirement",
            "buildCrossCaptainReport",
            "maintainFleetResource"
        ]
    })
}

fn schema_list_admin_grants() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn schema_appoint_admin() -> Value {
    json!({
        "type": "object",
        "properties": {
            "actorSessionId": { "type": "string", "minLength": 1 },
            "role": { "type": "string", "enum": ["shipAdmin", "fleetAdmin"] },
            "permittedOperations": {
                "type": "array",
                "items": schema_admin_operation(),
                "minItems": 1,
                "uniqueItems": true
            }
        },
        "required": ["actorSessionId", "role", "permittedOperations"],
        "additionalProperties": false
    })
}

fn schema_revoke_admin() -> Value {
    json!({
        "type": "object",
        "properties": {
            "grantId": { "type": "string", "minLength": 1 },
            "reason": { "type": "string", "minLength": 1, "maxLength": 4096 }
        },
        "required": ["grantId"],
        "additionalProperties": false
    })
}

fn schema_admin_worktree_target() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": { "const": "worktree" },
            "shipSlug": { "type": "string", "minLength": 1 },
            "worktreeId": { "type": "string", "minLength": 1 }
        },
        "required": ["kind", "shipSlug", "worktreeId"],
        "additionalProperties": false
    })
}

fn schema_approve_admin_action() -> Value {
    json!({
        "type": "object",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "grantId": { "type": "string", "minLength": 1 },
                    "operation": { "const": "cleanupSession" },
                    "sessionId": { "type": "string", "minLength": 1 }
                },
                "required": ["grantId", "operation", "sessionId"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "grantId": { "type": "string", "minLength": 1 },
                    "operation": { "const": "cleanupWorktree" },
                    "target": schema_admin_worktree_target()
                },
                "required": ["grantId", "operation", "target"],
                "additionalProperties": false
            }
        ]
    })
}

fn schema_get_agent() -> Value {
    json!({
        "type": "object",
        "properties": { "agentSessionId": { "type": "string" } },
        "required": ["agentSessionId"],
        "additionalProperties": false
    })
}

fn schema_agent_checkpoint() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agentSessionId": { "type": "string" },
            "authorSessionId": { "type": "string" },
            "summary": { "type": "string", "minLength": 1, "maxLength": 4096 },
            "stage": {
                "type": "string",
                "enum": ["working", "needsInput", "readyForReview", "awaitingIntegration", "complete", "stopped"]
            }
        },
        "required": ["agentSessionId", "authorSessionId", "summary"],
        "additionalProperties": false
    })
}

fn schema_record_agent_delivery() -> Value {
    let commit = json!({ "type": "string", "pattern": "^[0-9a-fA-F]{40}([0-9a-fA-F]{24})?$" });
    let reference = json!({ "type": "string", "minLength": 1, "maxLength": 16384 });
    json!({
        "type": "object",
        "properties": {
            "agentSessionId": { "type": "string", "minLength": 1 },
            "state": {
                "type": "string",
                "enum": ["implemented", "reviewed", "tested", "integrated", "packaged", "installed", "liveVerified"]
            },
            "evidence": { "type": "object" }
        },
        "required": ["agentSessionId", "state", "evidence"],
        "oneOf": [
            {
                "properties": {
                    "state": { "const": "implemented" },
                    "evidence": {
                        "type": "object",
                        "properties": { "commit": commit.clone() },
                        "required": ["commit"],
                        "additionalProperties": false
                    }
                }
            },
            {
                "properties": {
                    "state": { "const": "reviewed" },
                    "evidence": {
                        "type": "object",
                        "properties": { "commit": commit.clone(), "reference": reference.clone() },
                        "required": ["commit", "reference"],
                        "additionalProperties": false
                    }
                }
            },
            {
                "properties": {
                    "state": { "const": "tested" },
                    "evidence": {
                        "type": "object",
                        "properties": {
                            "commit": commit.clone(),
                            "reference": reference.clone(),
                            "environment": {
                                "oneOf": [
                                    {
                                        "type": "object",
                                        "properties": { "kind": { "const": "source" } },
                                        "required": ["kind"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {
                                            "kind": { "const": "packagedGuiE2e" },
                                            "artifactId": { "type": "string", "minLength": 1 },
                                            "sourceCommit": commit.clone(),
                                            "installationTarget": { "type": "string", "minLength": 1 }
                                        },
                                        "required": ["kind", "artifactId", "sourceCommit", "installationTarget"],
                                        "additionalProperties": false
                                    }
                                ]
                            }
                        },
                        "required": ["commit", "reference", "environment"],
                        "additionalProperties": false
                    }
                }
            },
            {
                "properties": {
                    "state": { "const": "integrated" },
                    "evidence": {
                        "type": "object",
                        "properties": {
                            "sourceCommit": commit.clone(),
                            "canonicalBaseline": { "type": "string", "minLength": 1 },
                            "canonicalCommit": commit.clone(),
                            "reference": reference.clone()
                        },
                        "required": ["sourceCommit", "canonicalBaseline", "canonicalCommit", "reference"],
                        "additionalProperties": false
                    }
                }
            },
            {
                "properties": {
                    "state": { "const": "packaged" },
                    "evidence": {
                        "type": "object",
                        "properties": {
                            "artifactId": { "type": "string", "minLength": 1 },
                            "sourceBaseline": commit.clone(),
                            "reference": reference.clone()
                        },
                        "required": ["artifactId", "sourceBaseline", "reference"],
                        "additionalProperties": false
                    }
                }
            },
            {
                "properties": {
                    "state": { "const": "installed" },
                    "evidence": {
                        "type": "object",
                        "properties": {
                            "artifactId": { "type": "string", "minLength": 1 },
                            "target": { "type": "string", "minLength": 1 },
                            "reference": reference.clone()
                        },
                        "required": ["artifactId", "target", "reference"],
                        "additionalProperties": false
                    }
                }
            },
            {
                "properties": {
                    "state": { "const": "liveVerified" },
                    "evidence": {
                        "type": "object",
                        "properties": {
                            "artifactId": { "type": "string", "minLength": 1 },
                            "target": { "type": "string", "minLength": 1 },
                            "verifierKind": { "type": "string", "enum": ["human", "aiAgent"] },
                            "reference": reference
                        },
                        "required": ["artifactId", "target", "verifierKind", "reference"],
                        "additionalProperties": false
                    }
                }
            }
        ],
        "additionalProperties": false
    })
}

fn schema_agent_events() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agentSessionId": { "type": "string" },
            "cursor": { "type": "string", "pattern": "^[0-9]+$", "default": "0" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
        },
        "required": ["agentSessionId"],
        "additionalProperties": false
    })
}

fn schema_start_agent() -> Value {
    json!({
        "type": "object",
        "properties": {
            "requestId": { "type": "string", "minLength": 1 },
            "captainSessionId": { "type": "string" },
            "assignment": { "type": "string", "minLength": 1, "maxLength": 16384 },
            "directory": { "type": "string" },
            "harness": { "type": "string", "enum": ["codex", "claude"] },
            "name": { "type": "string" },
            "workspaceTabId": { "type": "string" },
            "sourceCommit": { "type": "string", "pattern": "^[0-9a-fA-F]{40}([0-9a-fA-F]{24})?$", "description": "Exact commit checked out in the clean dispatch worktree." },
            "visibleProductBug": { "type": "boolean", "description": "True when acceptance requires packaged GUI E2E evidence." },
            "laneId": { "type": "string", "minLength": 1, "description": "Stable identity for this independent implementation lane." },
            "dependencies": { "type": "array", "items": { "type": "string", "minLength": 1 }, "uniqueItems": true },
            "mutableFiles": { "type": "array", "items": { "type": "string", "minLength": 1 }, "uniqueItems": true },
            "mutableSchemas": { "type": "array", "items": { "type": "string", "minLength": 1 }, "uniqueItems": true },
            "mutableInterfaces": { "type": "array", "items": { "type": "string", "minLength": 1 }, "uniqueItems": true },
            "integrationContracts": { "type": "array", "items": schema_integration_contract() }
        },
        "required": ["requestId", "captainSessionId", "assignment", "directory", "sourceCommit", "visibleProductBug", "laneId", "dependencies", "mutableFiles", "mutableSchemas", "mutableInterfaces", "integrationContracts"],
        "additionalProperties": false
    })
}

fn schema_commission_captain() -> Value {
    json!({
        "type": "object",
        "properties": {
            "projectId": { "type": "string", "description": "Registered Project to supervise." },
            "assignment": { "type": "string", "description": "Durable Captain assignment restored after resets." },
            "harness": { "type": "string", "enum": ["codex", "claude"], "description": "Agent harness. Defaults to codex." },
            "shipSlug": { "type": "string", "description": "Optional durable ship slug. Defaults to the project name." },
            "workspaceTabIds": { "type": "array", "items": { "type": "string" }, "description": "Project workspace tabs this Captain owns." }
        },
        "required": ["projectId", "assignment"],
        "additionalProperties": false
    })
}

fn schema_attach_captain() -> Value {
    json!({
        "type": "object",
        "properties": {
            "captainSessionId": { "type": "string", "description": "Live terminal to attach. It must already have control capability; read-only terminals are refused without elevation." },
            "projectId": { "type": "string", "description": "Registered Project to supervise." },
            "assignment": { "type": "string", "description": "Durable Captain assignment restored after resets." },
            "provider": { "type": "string", "enum": ["codex", "claude"], "description": "Agent harness. Defaults to codex." },
            "providerSessionId": { "type": "string", "description": "Provider-native conversation id to checkpoint immediately." },
            "shipSlug": { "type": "string", "description": "Optional durable ship slug. Defaults to the project name." },
            "workspaceTabIds": { "type": "array", "items": { "type": "string" }, "description": "Existing project Work Workspace ids this Captain owns. No current-tab inference occurs." }
        },
        "required": ["captainSessionId", "projectId", "assignment"],
        "additionalProperties": false
    })
}

fn schema_captain_checkpoint() -> Value {
    json!({
        "type": "object",
        "properties": {
            "captainSessionId": { "type": "string", "description": "Current Captain terminal id." },
            "shipSlug": { "type": "string", "description": "Alternative durable Captain ship slug." },
            "crewSessionId": { "type": "string", "description": "Optional Crew terminal to checkpoint instead of the Captain." },
            "conversationId": { "type": "string", "description": "Harness conversation or thread identifier used for provider resume." },
            "resumePoint": { "type": "string", "description": "Concise durable handoff with current state and next ordered action." }
        },
        "allOf": [
            {
                "anyOf": [
                    { "required": ["captainSessionId"] },
                    { "required": ["shipSlug"] }
                ]
            },
            {
                "anyOf": [
                    { "required": ["conversationId"] },
                    { "required": ["resumePoint"] }
                ]
            }
        ],
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
        "anyOf": [
            { "required": ["captainSessionId"] },
            { "required": ["shipSlug"] }
        ],
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

/// `remove_worktree` schema (WS-4). Removal is temporarily unavailable until
/// the unified worktree status service can authorize it safely.
fn schema_remove_worktree() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repoRoot":     { "type": "string", "description": "Path inside the repo the worktree belongs to." },
            "worktreePath": { "type": "string", "description": "Absolute POSIX path of the worktree to remove." },
            "force":        { "type": "boolean", "description": "Reserved removal option. It cannot bypass the temporary safety suspension." }
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
            name: "my_capability",
            tier: Tier::Read,
            summary: "Report whether the presented T-Hub token grants read or control capability.",
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
            name: "list_projects",
            tier: Tier::Read,
            summary: "List durable registered projects and their Git repository metadata.",
            input_schema: schema_empty,
        },
        ToolDef {
            name: "list_agents",
            tier: Tier::Read,
            summary: "List bounded durable agent-session summaries for one Captain or Project.",
            input_schema: schema_list_agents,
        },
        ToolDef {
            name: "dispatch_preflight",
            tier: Tier::Read,
            summary: "Evaluate independent lane ownership, dependencies, mutable-resource collisions, live capacity, and reserved supervisor or recovery headroom without imposing a fixed lane-count cap.",
            input_schema: schema_dispatch_preflight,
        },
        ToolDef {
            name: "list_admin_grants",
            tier: Tier::Read,
            summary: "List the caller's durable Ship Admin or Fleet Admin grants, including scope, generation, permitted operations, and revocation state.",
            input_schema: schema_list_admin_grants,
        },
        ToolDef {
            name: "get_agent",
            tier: Tier::Read,
            summary: "Get the full durable record for one agent session, including its assignment.",
            input_schema: schema_get_agent,
        },
        ToolDef {
            name: "agent_events",
            tier: Tier::Read,
            summary: "Read bounded lifecycle and checkpoint events after a cursor.",
            input_schema: schema_agent_events,
        },
        ToolDef {
            name: "captain_bootstrap",
            tier: Tier::Read,
            summary: "Recover a Captain's durable project, assignment, and agent-session roster after a reset or new conversation.",
            input_schema: schema_captain_bootstrap,
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
            summary: "Is the general dictating right now? Asks Scribe's v1 status endpoint (discovered via ~/.scribe/control.json; status.json file fallback with pid + 15s TTL) and returns {listening, status, since, source} - listening mirrors Scribe's level-triggered `busy` flag; fails open to listening=false when it can't tell (unreachable endpoint, missing/stale/dead-pid file).",
            input_schema: schema_empty,
        },
        // ---- Organization tier -----------------------------------------
        ToolDef {
            name: "history_list",
            tier: Tier::Organization,
            summary: "List the provider-neutral History catalog with exact continuity and action compatibility.",
            input_schema: schema_history_list,
        },
        ToolDef {
            name: "history_focus",
            tier: Tier::Organization,
            summary: "Focus the unique live terminal for one exact active History identity.",
            input_schema: schema_history_focus,
        },
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
            name: "rename_captain",
            tier: Tier::Organization,
            summary: "Rename one durable Captain identity without changing its Assignment, terminal, Harness, or Workspace ownership.",
            input_schema: schema_rename_captain,
        },
        ToolDef {
            name: "captain_checkpoint",
            tier: Tier::Organization,
            summary: "Persist a Captain or Crew conversation identifier and reset-safe resume point in the ship manifest.",
            input_schema: schema_captain_checkpoint,
        },
        ToolDef {
            name: "agent_checkpoint",
            tier: Tier::Organization,
            summary: "Append a bounded human-readable checkpoint to a durable agent session.",
            input_schema: schema_agent_checkpoint,
        },
        ToolDef {
            name: "record_agent_delivery",
            tier: Tier::Organization,
            summary: "Record immutable exact-commit evidence for implemented, reviewed, tested, complete, integrated, packaged, installed, and live-verified states. Complete is derived only from review plus acceptance testing.",
            input_schema: schema_record_agent_delivery,
        },
        ToolDef {
            name: "appoint_admin",
            tier: Tier::Organization,
            summary: "Appoint one Crew identity as a durable Ship Admin or Fleet Admin within the authenticated supervisor's exact authority.",
            input_schema: schema_appoint_admin,
        },
        ToolDef {
            name: "approve_admin_action",
            tier: Tier::Organization,
            summary: "Issue a durable one-time approval for one destructive delegated operation bound to the exact grant, actor, target, supervisor, and authority generation. Session target kind and ownership are derived by the backend from sessionId.",
            input_schema: schema_approve_admin_action,
        },
        ToolDef {
            name: "revoke_admin",
            tier: Tier::Organization,
            summary: "Revoke one durable administrative grant while retaining its audit tombstone.",
            input_schema: schema_revoke_admin,
        },
        ToolDef {
            name: "register_project",
            tier: Tier::Organization,
            summary: "Register an existing Git repository or explicitly create one absent empty-codebase leaf.",
            input_schema: schema_register_project,
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
            name: "history_resume",
            tier: Tier::ProcessChanging,
            summary: "Resume one exact provider conversation using backend-owned Harness, identity, cwd, and command selection.",
            input_schema: schema_history_resume,
        },
        ToolDef {
            name: "start_agent",
            tier: Tier::ProcessChanging,
            summary: "Start one Codex or Claude agent in an existing Project checkout with a durable assignment.",
            input_schema: schema_start_agent,
        },
        ToolDef {
            name: "commission_captain",
            tier: Tier::ProcessChanging,
            summary: "Commission one project-aware Captain in Codex or Claude and bind it transactionally to its durable ship.",
            input_schema: schema_commission_captain,
        },
        ToolDef {
            name: "attach_captain",
            tier: Tier::ProcessChanging,
            summary: "Attach an existing control-capability terminal as a project Captain without rewriting or elevating its bearer token.",
            input_schema: schema_attach_captain,
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
            summary: "Worktree removal is temporarily unavailable pending the unified safety service.",
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
            "my_capability",
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
            "history_list",
            "history_focus",
            "move_tile",
            "rename_tab",
            "new_tab",
            "focus_tab",
            "spawn_terminal",
            "history_resume",
            "send_text",
            "send_keys",
            "close_terminal",
            "open_file",
            "create_worktree",
            "remove_worktree",
            "claim_captain",
            "attach_captain",
            "release_captain",
            "rename_captain",
            "get_theme",
            "set_theme",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn retired_powder_tools_are_not_advertised() {
        for name in [
            "dispatch_crew",
            "list_powder_boards",
            "bind_project_powder",
            "project_board_snapshot",
            "powder_status",
            "heartbeat_crew_powder",
            "append_crew_powder_work_log",
            "read_crew_powder_evidence",
            "review_crew_powder_criterion",
            "complete_crew_powder",
        ] {
            assert!(
                find(name).is_none(),
                "retired tool is still advertised: {name}"
            );
        }
    }

    #[test]
    fn agent_listing_schema_exposes_removed_history_filter() {
        let schema = (find("list_agents").unwrap().input_schema)();
        let state = schema["properties"]["state"]["enum"].as_array().unwrap();
        assert_eq!(state, &vec![json!("active"), json!("removed")]);
    }

    #[test]
    fn delivery_evidence_schema_keeps_every_state_explicit() {
        let tool = find("record_agent_delivery").unwrap();
        assert_eq!(tool.tier, Tier::Organization);
        let schema = (tool.input_schema)();
        let states = schema["properties"]["state"]["enum"].as_array().unwrap();
        assert_eq!(
            states,
            &vec![
                json!("implemented"),
                json!("reviewed"),
                json!("tested"),
                json!("integrated"),
                json!("packaged"),
                json!("installed"),
                json!("liveVerified"),
            ]
        );
        assert_eq!(schema["oneOf"].as_array().unwrap().len(), states.len());
    }

    #[test]
    fn new_process_changing_tools_demand_confirmation() {
        for name in ["send_text", "send_keys", "close_terminal"] {
            let mcp = find(name).unwrap().to_mcp();
            let desc = mcp["description"].as_str().unwrap();
            assert!(
                desc.contains("CONFIRMATION REQUIRED"),
                "{name} desc: {desc}"
            );
            assert_eq!(mcp["annotations"]["confirmationRequired"], true, "{name}");
            assert_eq!(
                mcp["annotations"]["t-hubTier"], "process-changing",
                "{name}"
            );
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
    fn history_tools_expose_exact_identity_schemas_and_tiers() {
        let list = find("history_list").unwrap();
        assert_eq!(list.to_mcp()["annotations"]["t-hubTier"], "organization");
        assert_eq!((list.input_schema)()["properties"]["limit"]["maximum"], 500);

        let focus = find("history_focus").unwrap();
        assert_eq!(focus.to_mcp()["annotations"]["t-hubTier"], "organization");
        assert_eq!((focus.input_schema)()["required"], json!(["historyId"]));
        assert_eq!(
            (focus.input_schema)()["properties"]["historyId"]["minLength"],
            1
        );

        let resume = find("history_resume").unwrap();
        let advertised = resume.to_mcp();
        assert_eq!(advertised["annotations"]["t-hubTier"], "process-changing");
        assert_eq!(advertised["annotations"]["confirmationRequired"], true);
        assert_eq!(
            (resume.input_schema)()["required"],
            json!(["historyId", "requestId"])
        );
        assert_eq!(
            (resume.input_schema)()["properties"]["historyId"]["minLength"],
            1
        );
        assert_eq!(
            (resume.input_schema)()["properties"]["targetTabId"]["minLength"],
            1
        );
    }

    #[test]
    fn read_tools_are_not_confirmation_gated() {
        for name in [
            "list_terminals",
            "my_capability",
            "get_status",
            "wsl_health",
        ] {
            let mcp = find(name).unwrap().to_mcp();
            assert_eq!(
                mcp["annotations"]["confirmationRequired"], false,
                "{name} should not require confirmation"
            );
        }
    }

    #[test]
    fn my_capability_is_read_tier_with_no_arguments() {
        let tool = find("my_capability").unwrap();
        let mcp = tool.to_mcp();
        assert_eq!(mcp["annotations"]["t-hubTier"], "read");
        assert_eq!(mcp["annotations"]["confirmationRequired"], false);
        assert_eq!((tool.input_schema)(), schema_empty());
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
        for name in [
            "claim_captain",
            "release_captain",
            "rename_captain",
            "captain_checkpoint",
        ] {
            let mcp = find(name).unwrap().to_mcp();
            assert_eq!(mcp["annotations"]["t-hubTier"], "organization", "{name}");
            assert_eq!(mcp["annotations"]["confirmationRequired"], false, "{name}");
            assert!(
                mcp["description"].as_str().unwrap().contains("audited"),
                "{name}"
            );
        }
        let claim_schema = (find("claim_captain").unwrap().input_schema)();
        assert_eq!(claim_schema["required"], json!(["captainSessionId"]));
        let rename_schema = (find("rename_captain").unwrap().input_schema)();
        assert_eq!(rename_schema["required"], json!(["displayName"]));
        assert_eq!(rename_schema["properties"]["displayName"]["maxLength"], 120);
    }

    #[cfg(any())]
    #[test]
    fn project_tools_expose_read_and_audited_mutation_tiers() {
        let list = find("list_projects").unwrap();
        assert_eq!(list.tier, Tier::Read);
        assert_eq!((list.input_schema)(), schema_empty());

        let boards = find("list_powder_boards").unwrap();
        assert_eq!(boards.tier, Tier::Read);
        assert_eq!(
            (boards.input_schema)()["properties"]["limit"]["maximum"],
            500
        );
        assert_eq!((boards.input_schema)()["additionalProperties"], false);
        let snapshot = find("project_board_snapshot").unwrap();
        assert_eq!(snapshot.tier, Tier::Read);
        assert_eq!((snapshot.input_schema)()["required"], json!(["terminalId"]));
        assert_eq!(
            (snapshot.input_schema)()["properties"]["limit"]["maximum"],
            1000
        );

        for name in ["register_project", "bind_project_powder"] {
            let tool = find(name).unwrap();
            assert_eq!(tool.tier, Tier::Organization);
            assert_eq!(tool.to_mcp()["annotations"]["confirmationRequired"], false);
        }
        assert_eq!(
            (find("register_project").unwrap().input_schema)()["required"],
            json!(["repoRoot"])
        );
        assert_eq!(
            (find("register_project").unwrap().input_schema)()["properties"]["initializeGit"]
                ["type"],
            "boolean"
        );
        assert_eq!(
            (find("register_project").unwrap().input_schema)()["properties"]["createDirectory"]
                ["type"],
            "boolean"
        );
        assert_eq!(
            (find("bind_project_powder").unwrap().input_schema)()["required"],
            json!(["projectId", "repository"])
        );

        let bootstrap = find("captain_bootstrap").unwrap();
        assert_eq!(bootstrap.tier, Tier::Read);
        assert_eq!(
            bootstrap.to_mcp()["annotations"]["confirmationRequired"],
            false
        );
        assert_eq!(
            (bootstrap.input_schema)()["anyOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let commission = find("commission_captain").unwrap();
        assert_eq!(commission.tier, Tier::ProcessChanging);
        assert_eq!(
            commission.to_mcp()["annotations"]["confirmationRequired"],
            true
        );
        assert_eq!(
            (commission.input_schema)()["required"],
            json!(["projectId", "assignment"])
        );

        let attach = find("attach_captain").unwrap();
        assert_eq!(attach.tier, Tier::ProcessChanging);
        assert_eq!(
            (attach.input_schema)()["required"],
            json!(["captainSessionId", "projectId", "assignment"])
        );

        let status = find("powder_status").unwrap();
        assert_eq!(status.tier, Tier::Read);
        assert_eq!((status.input_schema)()["required"], json!(["projectId"]));

        let dispatch = find("dispatch_crew").unwrap();
        assert_eq!(dispatch.tier, Tier::ProcessChanging);
        assert_eq!(
            (dispatch.input_schema)()["required"],
            json!(["cardId", "task"])
        );
        assert_eq!(
            (dispatch.input_schema)()["anyOf"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            (dispatch.input_schema)()["properties"]["workspaceTabId"]["type"],
            "string"
        );

        let checkpoint = find("captain_checkpoint").unwrap();
        assert_eq!(checkpoint.tier, Tier::Organization);
        assert_eq!(
            (checkpoint.input_schema)()["allOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let heartbeat = find("heartbeat_crew_powder").unwrap();
        assert_eq!(heartbeat.tier, Tier::ProcessChanging);
        assert_eq!(
            (heartbeat.input_schema)()["required"],
            json!(["crewSessionId"])
        );
    }

    #[cfg(any())]
    #[test]
    fn powder_evidence_tools_share_minimal_bound_authority_schemas() {
        let append = find("append_crew_powder_work_log").unwrap();
        assert_eq!(append.tier, Tier::Organization);
        assert_eq!(
            (append.input_schema)()["required"],
            json!(["operationId", "message"])
        );

        let evidence = find("read_crew_powder_evidence").unwrap();
        assert_eq!(evidence.tier, Tier::Read);
        assert_eq!(
            (evidence.input_schema)()["properties"]["limit"]["maximum"],
            20
        );

        let complete = find("complete_crew_powder").unwrap();
        assert_eq!(complete.tier, Tier::ProcessChanging);
        assert_eq!(
            (complete.input_schema)()["required"],
            json!(["crewSessionId", "operationId", "proof", "criterionProofs"])
        );
        assert_eq!(
            (complete.input_schema)()["properties"]["criterionProofs"]["maxItems"],
            128
        );
        assert_eq!(
            (append.input_schema)()["properties"]["message"]["maxLength"],
            16384
        );
        assert_eq!(
            (complete.input_schema)()["properties"]["proof"]["maxLength"],
            4096
        );
        assert_eq!(
            complete.to_mcp()["annotations"]["confirmationRequired"],
            true
        );

        let review = find("review_crew_powder_criterion").unwrap();
        assert_eq!(review.tier, Tier::Organization);
        assert_eq!(
            (review.input_schema)()["required"],
            json!([
                "crewSessionId",
                "operationId",
                "criterion",
                "criterionId",
                "decision",
                "proof",
                "expectedReviewerIdentity"
            ])
        );
        assert_eq!(
            (review.input_schema)()["properties"]["expectedReviewerIdentity"]["description"],
            "Legacy caller-facing reviewer label retained for durable-intent compatibility. It is not authoritative: T-Hub verifies the receipt against the protected Powder profile operationIdentity."
        );

        // Append and review remain Organization mutations. The backend
        // separately admits the narrow Crew-self work-log case through a read
        // token, then rechecks exact Crew and ship ownership.
        for name in [
            "append_crew_powder_work_log",
            "review_crew_powder_criterion",
        ] {
            let tool = find(name).unwrap().to_mcp();
            assert_eq!(tool["annotations"]["t-hubTier"], "organization", "{name}");
            assert_eq!(
                tool["annotations"]["confirmationRequired"], false,
                "{name} must reach role-bound backend authorization"
            );
        }
        let complete_mcp = complete.to_mcp();
        assert_eq!(complete_mcp["annotations"]["t-hubTier"], "process-changing");
        assert_eq!(complete_mcp["annotations"]["confirmationRequired"], true);
        assert!(complete_mcp["description"]
            .as_str()
            .unwrap()
            .contains("CONFIRMATION REQUIRED"));
        let read = evidence.to_mcp();
        assert_eq!(read["annotations"]["t-hubTier"], "read");
        assert_eq!(read["annotations"]["confirmationRequired"], false);

        // JSON Schema maxLength counts Unicode scalar values. The descriptions
        // therefore state the backend's byte contract explicitly; CLI process
        // and combined-control tests enforce the UTF-8 byte limit itself.
        assert!(
            (append.input_schema)()["properties"]["message"]["description"]
                .as_str()
                .unwrap()
                .contains("16 KiB UTF-8")
        );
        assert!(
            (complete.input_schema)()["properties"]["proof"]["description"]
                .as_str()
                .unwrap()
                .contains("4096 UTF-8 bytes")
        );

        for name in [
            "append_crew_powder_work_log",
            "read_crew_powder_evidence",
            "review_crew_powder_criterion",
            "complete_crew_powder",
        ] {
            let schema = (find(name).unwrap().input_schema)();
            assert_eq!(schema["type"], "object", "{name}");
            assert_eq!(schema["additionalProperties"], false, "{name}");
            for escape in [
                "card",
                "cardId",
                "card_id",
                "run",
                "runId",
                "run_id",
                "profile",
                "connectionProfile",
                "connection_profile",
                "endpoint",
                "powderEndpoint",
                "powder_endpoint",
                "repository",
                "powderRepository",
                "powder_repository",
                "repo",
                "credential",
                "apiKey",
                "api_key",
                "key",
                "token",
                "secret",
            ] {
                assert!(
                    schema["properties"].get(escape).is_none(),
                    "{name} must not expose {escape}"
                );
            }
        }
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
    fn adaptive_dispatch_tools_require_explicit_lane_and_collision_evidence() {
        let preflight = find("dispatch_preflight").unwrap();
        let preflight_mcp = preflight.to_mcp();
        assert_eq!(preflight_mcp["annotations"]["t-hubTier"], "read");
        let preflight_schema = (preflight.input_schema)();
        assert_eq!(
            preflight_schema["required"],
            json!(["projectId", "requestedLanes", "integrationContracts"])
        );
        let lane = &preflight_schema["properties"]["requestedLanes"]["items"];
        assert_eq!(
            lane["required"],
            json!(["laneId", "ownerId", "dependencies"])
        );

        let start = (find("start_agent").unwrap().input_schema)();
        for required in [
            "sourceCommit",
            "laneId",
            "dependencies",
            "mutableFiles",
            "mutableSchemas",
            "mutableInterfaces",
            "integrationContracts",
        ] {
            assert!(
                start["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value == required),
                "start_agent must require {required}"
            );
        }
    }

    #[test]
    fn delegated_admin_tools_expose_durable_role_scope_without_general_authority() {
        let list = find("list_admin_grants").unwrap().to_mcp();
        assert_eq!(list["annotations"]["t-hubTier"], "read");
        assert_eq!(list["annotations"]["confirmationRequired"], false);

        let appoint = find("appoint_admin").unwrap();
        assert_eq!(appoint.to_mcp()["annotations"]["t-hubTier"], "organization");
        let schema = (appoint.input_schema)();
        assert_eq!(
            schema["required"],
            json!(["actorSessionId", "role", "permittedOperations"])
        );
        let operations = schema["properties"]["permittedOperations"]["items"]["enum"]
            .as_array()
            .unwrap();
        for forbidden in [
            "directImplementation",
            "grantAdministrativeRole",
            "assumeCaptainAuthority",
            "approveGeneralReservedAction",
        ] {
            assert!(!operations.iter().any(|operation| operation == forbidden));
        }

        let revoke = find("revoke_admin").unwrap().to_mcp();
        assert_eq!(revoke["annotations"]["t-hubTier"], "organization");

        let approve = find("approve_admin_action").unwrap();
        assert_eq!(approve.to_mcp()["annotations"]["t-hubTier"], "organization");
        let approve_schema = (approve.input_schema)();
        assert_eq!(
            approve_schema["oneOf"][0]["required"],
            json!(["grantId", "operation", "sessionId"])
        );
        assert!(approve_schema["oneOf"][0]["properties"]
            .get("target")
            .is_none());
        assert_eq!(
            approve_schema["oneOf"][1]["required"],
            json!(["grantId", "operation", "target"])
        );
        assert_eq!(
            (find("close_terminal").unwrap().input_schema)()["properties"]["approvalId"]["type"],
            "string"
        );
    }

    #[test]
    fn spawn_paths_expose_startup_command() {
        // audit MED: create_worktree must accept startupCommand just like
        // spawn_terminal, so a worktree crew can boot into its command (e.g.
        // `claude --resume <id>`) instead of a bare shell.
        for name in ["spawn_terminal", "create_worktree"] {
            let schema = (find(name).unwrap().input_schema)();
            assert!(
                schema["properties"]["startupCommand"].is_object(),
                "{name} must accept startupCommand"
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
