//! Organization-mutation application (T12): the native twin of the webview's
//! `controlBridge.ts` `applyControl` switch.
//!
//! The server accepts + audits MCP Organization commands and forwards them to
//! the webview via the `control://apply` Tauri event; since T12 it ALSO
//! broadcasts every accepted forward to control-socket event subscribers on the
//! [`APPLY_CHANNEL`] event channel. The T9 `OverlayFeed` (the process's single
//! event drainer) decodes those frames through [`parse_event`] onto its
//! `apply_requests()` channel, and the cockpit worker applies each command to
//! the [`ChromeModel`] through [`apply_model`] - so a Claude session driving
//! the t-hub MCP manipulates the native cockpit exactly as it does the webview.
//!
//! Everything here is gpui-free plain data + pure functions (unit-tested under
//! `--no-default-features`); side effects the model cannot express (persist,
//! pool detach, focus-change reports, PTY attach) are returned in [`Outcome`]
//! for the embedding shell to run.
//!
//! ## Webview parity notes
//! - `open_file` has NO arm here, exactly like `controlBridge.ts`: the server
//!   answers the MCP caller with the file contents directly; no UI mutation.
//! - Parsing is tolerant the way the bridge's `str()` helper is: alias keys are
//!   accepted (`id` for `tabId`, snake_case worktree fields), missing required
//!   fields make the frame a no-op rather than an error.

use std::collections::HashMap;

use serde_json::Value;

use crate::chrome::model::ChromeModel;

/// The event channel the server broadcasts accepted Organization forwards on
/// (`control.rs` `APPLY_EVENT_CHANNEL`).
pub const APPLY_CHANNEL: &str = "control://apply";

/// One accepted Organization forward, decoded from an [`APPLY_CHANNEL`] frame.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyCommand {
    /// `move_tile`: within-tab reorder when `target_id` is present (webview
    /// `moveTile`), cross-tab move when `tab_id` is (webview `moveTileToTab`).
    MoveTile { terminal_id: String, target_id: Option<String>, tab_id: Option<String> },
    /// `rename_tab` by tab id.
    RenameTab { tab_id: String, name: String },
    /// `new_tab`: adopt the core-minted `id` verbatim (webview `adoptTab`), or
    /// fall back to a locally-created tab if an older core sent no id.
    NewTab { id: Option<String>, name: Option<String> },
    /// `focus_tab` by tab id.
    FocusTab { tab_id: String },
    /// `focus_session`: the id may name a tile, a tab, or (native extra) a
    /// Claude session UUID the shell resolves through the T9 session index.
    FocusSession { id: String },
    /// `spawn_terminal`: with `id` the server already spawned the session
    /// (native path) and the model places it; without, the webview owns the
    /// spawn and the session arrives via reconcile.
    SpawnTerminal { id: Option<String> },
    /// `add_worktree_workspace` (from `create_worktree`): open/reuse the named
    /// tab and route the worktree terminal into it.
    AddWorktreeWorkspace {
        worktree_path: String,
        branch: Option<String>,
        tab_id: Option<String>,
        tab_name: Option<String>,
        /// Present on the native path (the server spawned the terminal).
        terminal_id: Option<String>,
    },
    /// `remove_worktree_workspace`: detach every tile rooted in the worktree
    /// dir before the (webview-owned) `git worktree remove` tears it down.
    RemoveWorktreeWorkspace { worktree_path: String },
}

/// Read a non-empty string field from a loose args object (the bridge's `str()`).
fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The first present of several alias keys.
fn str_any(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| str_arg(args, k))
}

/// Decode one `{"command":..,"args":..}` apply payload. `None` means "nothing
/// to apply natively" - an unknown command, a command with no UI arm
/// (`open_file`), or missing required args - mirroring the webview bridge's
/// total, never-throwing switch.
pub fn parse_event(payload: &Value) -> Option<ApplyCommand> {
    let command = payload.get("command")?.as_str()?;
    let empty = Value::Null;
    let args = payload.get("args").unwrap_or(&empty);
    match command {
        "move_tile" => Some(ApplyCommand::MoveTile {
            terminal_id: str_any(args, &["terminalId", "id"])?,
            target_id: str_any(args, &["targetId", "targetTerminalId"]),
            tab_id: str_arg(args, "tabId"),
        }),
        "rename_tab" => Some(ApplyCommand::RenameTab {
            tab_id: str_any(args, &["tabId", "id"])?,
            name: str_arg(args, "name")?,
        }),
        "new_tab" => Some(ApplyCommand::NewTab {
            id: str_arg(args, "id"),
            name: str_arg(args, "name"),
        }),
        "focus_tab" => Some(ApplyCommand::FocusTab { tab_id: str_any(args, &["tabId", "id"])? }),
        "focus_session" => Some(ApplyCommand::FocusSession {
            id: str_any(args, &["sessionId", "terminalId", "tabId", "id"])?,
        }),
        "spawn_terminal" => Some(ApplyCommand::SpawnTerminal { id: str_arg(args, "id") }),
        "add_worktree_workspace" => Some(ApplyCommand::AddWorktreeWorkspace {
            worktree_path: str_any(args, &["worktreePath", "worktree_path"])?,
            branch: str_arg(args, "branch"),
            tab_id: str_any(args, &["tabId", "tab_id"]),
            tab_name: str_any(args, &["tabName", "tab_name"]),
            terminal_id: str_arg(args, "terminalId"),
        }),
        "remove_worktree_workspace" => Some(ApplyCommand::RemoveWorktreeWorkspace {
            worktree_path: str_any(args, &["worktreePath", "worktree_path"])?,
        }),
        // `open_file` and anything unknown: no native UI arm (webview parity).
        _ => None,
    }
}

/// What the embedding shell must do after [`apply_model`] mutated the model.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// The command found its target (a `false` on `FocusSession` lets the shell
    /// retry with the session-UUID alias from the T9 index).
    pub matched: bool,
    /// The layout (tabs/tiles/active/focus) changed: persist + re-report tabs.
    pub layout_changed: bool,
    /// A session was (or is being) spawned server-side: reconcile promptly so
    /// the tile attaches.
    pub want_reconcile: bool,
    /// Tiles that left the layout and must leave the attach pool.
    pub detach: Vec<String>,
}

/// Apply one command to the chrome model. `cwds` maps placed tile ids to their
/// last-known cwd (`list_terminals`), used by the worktree-removal detach
/// match. Pure model mutation - side effects ride the returned [`Outcome`].
pub fn apply_model(
    cmd: &ApplyCommand,
    m: &mut ChromeModel,
    cwds: &HashMap<String, String>,
) -> Outcome {
    let mut out = Outcome::default();
    match cmd {
        ApplyCommand::MoveTile { terminal_id, target_id, tab_id } => {
            // Webview precedence: a targetId is a within-tab reorder; else a
            // tabId is a cross-tab move.
            out.matched = match (target_id, tab_id) {
                (Some(target), _) => m.reorder_tile(terminal_id, target),
                (None, Some(tab)) => m.move_tile_to_tab(terminal_id, tab),
                (None, None) => false,
            };
            out.layout_changed = out.matched;
        }
        ApplyCommand::RenameTab { tab_id, name } => {
            out.matched = m.rename_tab_by_id(tab_id, name);
            out.layout_changed = out.matched;
        }
        ApplyCommand::NewTab { id, name } => {
            let name = name.as_deref().unwrap_or("Workspace");
            match id {
                Some(id) => {
                    m.adopt_tab(id, name);
                }
                None => {
                    // Older core sent no id: create locally, then rename
                    // (webview fallback path).
                    let i = m.add_tab();
                    let local_id = m.tabs[i].id.clone();
                    m.rename_tab_by_id(&local_id, name);
                }
            }
            out.matched = true;
            out.layout_changed = true;
        }
        ApplyCommand::FocusTab { tab_id } => {
            out.matched = m.set_active_by_id(tab_id);
            out.layout_changed = out.matched;
        }
        ApplyCommand::FocusSession { id } => {
            // Webview order: a tile id activates its owning tab then focuses the
            // tile; else a tab id activates that tab; else best-effort no-op.
            if let Some(tab) = m.owning_tab_of(id) {
                m.set_active(tab);
                m.set_focused(id);
                out.matched = true;
            } else {
                out.matched = m.set_active_by_id(id);
            }
            out.layout_changed = out.matched;
        }
        ApplyCommand::SpawnTerminal { id } => {
            // With a server-minted id (native path), place + focus it now; the
            // shell's reconcile attaches it. Without one the webview owns the
            // spawn and reconcile adopts the session when it appears.
            if let Some(id) = id {
                out.layout_changed = m.place_tile(id);
            }
            out.matched = true;
            out.want_reconcile = true;
        }
        ApplyCommand::AddWorktreeWorkspace {
            worktree_path,
            branch,
            tab_id,
            tab_name,
            terminal_id,
        } => {
            // Tab name priority mirrors the webview's addWorktreeWorkspace:
            // tabName > branch > the path's last component > "Worktree".
            let name = tab_name
                .clone()
                .or_else(|| branch.clone())
                .or_else(|| {
                    worktree_path
                        .rsplit('/')
                        .find(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "Worktree".to_string());
            let resolved_tab = match tab_id {
                Some(id) => {
                    m.adopt_tab(id, &name);
                    id.clone()
                }
                None => {
                    let i = m.add_tab();
                    let local_id = m.tabs[i].id.clone();
                    m.rename_tab_by_id(&local_id, &name);
                    local_id
                }
            };
            match terminal_id {
                // Native path: the server already spawned the worktree terminal;
                // adopt_tab activated the tab, so place_tile lands it there.
                Some(tid) => {
                    m.place_tile(tid);
                }
                // Webview path: the spawn happens client-side over there; route
                // the session into the named tab when its cwd shows up.
                None => m.note_pending_placement(worktree_path, &resolved_tab),
            }
            out.matched = true;
            out.layout_changed = true;
            out.want_reconcile = true;
        }
        ApplyCommand::RemoveWorktreeWorkspace { worktree_path } => {
            // Webview cwd match: the worktree dir itself or anything inside it,
            // on a path-segment boundary.
            let path = worktree_path.trim_end_matches('/');
            if path.is_empty() {
                return out;
            }
            let prefix = format!("{path}/");
            let victims: Vec<String> = m
                .all_tiles()
                .into_iter()
                .filter(|id| {
                    cwds.get(id).is_some_and(|cwd| {
                        let cwd = cwd.trim_end_matches('/');
                        cwd == path || cwd.starts_with(&prefix)
                    })
                })
                .collect();
            for id in &victims {
                m.close_tile(id);
            }
            out.matched = true;
            out.layout_changed = !victims.is_empty();
            out.detach = victims;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn ev(command: &str, args: Value) -> Value {
        json!({ "command": command, "args": args })
    }

    // -- parse ------------------------------------------------------------------

    #[test]
    fn parses_every_command_with_alias_keys() {
        assert_eq!(
            parse_event(&ev("move_tile", json!({"terminalId": "aa", "tabId": "t1"}))),
            Some(ApplyCommand::MoveTile {
                terminal_id: "aa".into(),
                target_id: None,
                tab_id: Some("t1".into())
            })
        );
        assert_eq!(
            parse_event(&ev("move_tile", json!({"id": "aa", "targetTerminalId": "bb"}))),
            Some(ApplyCommand::MoveTile {
                terminal_id: "aa".into(),
                target_id: Some("bb".into()),
                tab_id: None
            })
        );
        assert_eq!(
            parse_event(&ev("rename_tab", json!({"id": "t1", "name": "ops"}))),
            Some(ApplyCommand::RenameTab { tab_id: "t1".into(), name: "ops".into() })
        );
        assert_eq!(
            parse_event(&ev("new_tab", json!({"id": "t2", "name": "Logs"}))),
            Some(ApplyCommand::NewTab { id: Some("t2".into()), name: Some("Logs".into()) })
        );
        assert_eq!(
            parse_event(&ev("focus_tab", json!({"tabId": "t1"}))),
            Some(ApplyCommand::FocusTab { tab_id: "t1".into() })
        );
        assert_eq!(
            parse_event(&ev("focus_session", json!({"sessionId": "aa"}))),
            Some(ApplyCommand::FocusSession { id: "aa".into() })
        );
        assert_eq!(
            parse_event(&ev("spawn_terminal", json!({"cwd": "/tmp", "id": "cc"}))),
            Some(ApplyCommand::SpawnTerminal { id: Some("cc".into()) })
        );
        assert_eq!(
            parse_event(&ev(
                "add_worktree_workspace",
                json!({"worktree_path": "/r/wt", "tab_id": "t9", "tab_name": "feat", "terminalId": "dd"})
            )),
            Some(ApplyCommand::AddWorktreeWorkspace {
                worktree_path: "/r/wt".into(),
                branch: None,
                tab_id: Some("t9".into()),
                tab_name: Some("feat".into()),
                terminal_id: Some("dd".into()),
            })
        );
        assert_eq!(
            parse_event(&ev("remove_worktree_workspace", json!({"worktreePath": "/r/wt"}))),
            Some(ApplyCommand::RemoveWorktreeWorkspace { worktree_path: "/r/wt".into() })
        );
    }

    #[test]
    fn open_file_unknown_and_malformed_frames_are_no_ops() {
        // open_file has no UI arm (webview parity: the server answers the MCP
        // caller directly).
        assert_eq!(parse_event(&ev("open_file", json!({"path": "/etc/hosts"}))), None);
        assert_eq!(parse_event(&ev("something_new", json!({}))), None);
        // Missing required fields.
        assert_eq!(parse_event(&ev("move_tile", json!({}))), None);
        assert_eq!(parse_event(&ev("rename_tab", json!({"tabId": "t"}))), None);
        assert_eq!(parse_event(&ev("focus_session", json!({}))), None);
        // Not even the right envelope.
        assert_eq!(parse_event(&json!("nope")), None);
        assert_eq!(parse_event(&json!({"args": {}})), None);
        // Absent args still parse where nothing is required.
        assert_eq!(
            parse_event(&json!({"command": "spawn_terminal"})),
            Some(ApplyCommand::SpawnTerminal { id: None })
        );
    }

    // -- apply ------------------------------------------------------------------

    fn model_with(tiles: &[&str]) -> ChromeModel {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(tiles));
        m
    }

    #[test]
    fn move_tile_applies_both_modes() {
        let mut m = model_with(&["aa", "bb"]);
        m.adopt_tab("t2", "two");
        m.set_active(0);
        let cwds = HashMap::new();

        // Cross-tab: tabId.
        let out = apply_model(
            &ApplyCommand::MoveTile {
                terminal_id: "aa".into(),
                target_id: None,
                tab_id: Some("t2".into()),
            },
            &mut m,
            &cwds,
        );
        assert!(out.matched && out.layout_changed);
        assert_eq!(m.tabs[1].tiles, ids(&["aa"]));

        // Within-tab: targetId splices in the active tab.
        m.reconcile(&ids(&["aa", "bb", "cc"])); // "cc" joins the active tab 0
        let out = apply_model(
            &ApplyCommand::MoveTile {
                terminal_id: "cc".into(),
                target_id: Some("bb".into()),
                tab_id: None,
            },
            &mut m,
            &cwds,
        );
        assert!(out.matched);
        assert_eq!(m.tabs[0].tiles, ids(&["cc", "bb"]));

        // Unknown ids: total no-op.
        let out = apply_model(
            &ApplyCommand::MoveTile {
                terminal_id: "ghost".into(),
                target_id: None,
                tab_id: Some("t2".into()),
            },
            &mut m,
            &cwds,
        );
        assert!(!out.matched && !out.layout_changed);
    }

    #[test]
    fn new_tab_adopts_the_core_id_and_focus_commands_land() {
        let mut m = model_with(&["aa", "bb"]);
        let cwds = HashMap::new();

        let out = apply_model(
            &ApplyCommand::NewTab { id: Some("core-1".into()), name: Some("Logs".into()) },
            &mut m,
            &cwds,
        );
        assert!(out.matched && out.layout_changed);
        assert_eq!(m.tabs[1].id, "core-1");
        assert_eq!(m.tabs[1].name, "Logs");
        assert_eq!(m.active, 1);

        // focus_tab back to the first tab by id.
        let first = m.tabs[0].id.clone();
        let out = apply_model(&ApplyCommand::FocusTab { tab_id: first }, &mut m, &cwds);
        assert!(out.matched);
        assert_eq!(m.active, 0);

        // focus_session on a tile in ANOTHER tab activates it + focuses.
        apply_model(
            &ApplyCommand::MoveTile {
                terminal_id: "bb".into(),
                target_id: None,
                tab_id: Some("core-1".into()),
            },
            &mut m,
            &cwds,
        );
        let out = apply_model(&ApplyCommand::FocusSession { id: "bb".into() }, &mut m, &cwds);
        assert!(out.matched);
        assert_eq!(m.active, 1);
        assert_eq!(m.focused.as_deref(), Some("bb"));

        // focus_session on an unknown id reports unmatched (shell retries with
        // the T9 uuid alias).
        let out = apply_model(&ApplyCommand::FocusSession { id: "nope".into() }, &mut m, &cwds);
        assert!(!out.matched);

        // A no-id new_tab still creates locally (older core fallback).
        let out = apply_model(
            &ApplyCommand::NewTab { id: None, name: Some("Local".into()) },
            &mut m,
            &cwds,
        );
        assert!(out.matched);
        assert_eq!(m.tabs.last().unwrap().name, "Local");
    }

    #[test]
    fn spawn_terminal_places_a_server_minted_id_once() {
        let mut m = model_with(&["aa"]);
        let cwds = HashMap::new();
        let out =
            apply_model(&ApplyCommand::SpawnTerminal { id: Some("cc".into()) }, &mut m, &cwds);
        assert!(out.matched && out.layout_changed && out.want_reconcile);
        assert_eq!(m.active_tiles(), ids(&["aa", "cc"]).as_slice());
        assert_eq!(m.focused.as_deref(), Some("cc"));
        // Idempotent vs the reconcile race: placing again is a no-op.
        let out =
            apply_model(&ApplyCommand::SpawnTerminal { id: Some("cc".into()) }, &mut m, &cwds);
        assert!(out.matched && !out.layout_changed);
        // The webview-path forward (no id) just asks for a prompt reconcile.
        let out = apply_model(&ApplyCommand::SpawnTerminal { id: None }, &mut m, &cwds);
        assert!(out.matched && !out.layout_changed && out.want_reconcile);
    }

    #[test]
    fn add_worktree_workspace_opens_the_named_tab_and_routes_the_terminal() {
        let cwds = HashMap::new();

        // Native path: terminalId present - placed directly in the adopted tab.
        let mut m = model_with(&["aa"]);
        let out = apply_model(
            &ApplyCommand::AddWorktreeWorkspace {
                worktree_path: "/r/wt".into(),
                branch: Some("feat-x".into()),
                tab_id: Some("wt-tab".into()),
                tab_name: None,
                terminal_id: Some("dd".into()),
            },
            &mut m,
            &cwds,
        );
        assert!(out.matched && out.layout_changed && out.want_reconcile);
        assert_eq!(m.tabs[1].id, "wt-tab");
        assert_eq!(m.tabs[1].name, "feat-x"); // branch beats path component
        assert_eq!(m.tabs[1].tiles, ids(&["dd"]));

        // Webview path: no terminalId - a pending placement routes the session
        // in when its cwd appears.
        let mut m = model_with(&["aa"]);
        apply_model(
            &ApplyCommand::AddWorktreeWorkspace {
                worktree_path: "/r/wt".into(),
                branch: None,
                tab_id: Some("wt-tab".into()),
                tab_name: None,
                terminal_id: None,
            },
            &mut m,
            &cwds,
        );
        assert_eq!(m.tabs[1].name, "wt"); // path's last component
        m.set_active(0);
        m.reconcile_with_cwds(&[("aa".into(), "/elsewhere".into()), ("ee".into(), "/r/wt".into())]);
        assert_eq!(m.tabs[1].tiles, ids(&["ee"]));
    }

    #[test]
    fn remove_worktree_workspace_detaches_by_cwd_boundary() {
        let mut m = model_with(&["aa", "bb", "cc"]);
        let cwds: HashMap<String, String> = [
            ("aa".to_string(), "/r/wt".to_string()),
            ("bb".to_string(), "/r/wt/sub/".to_string()),
            ("cc".to_string(), "/r/wt-other".to_string()),
        ]
        .into();
        let out = apply_model(
            &ApplyCommand::RemoveWorktreeWorkspace { worktree_path: "/r/wt/".into() },
            &mut m,
            &cwds,
        );
        assert!(out.matched && out.layout_changed);
        assert_eq!(out.detach, ids(&["aa", "bb"]));
        assert_eq!(m.active_tiles(), ids(&["cc"]).as_slice()); // boundary respected
    }
}
