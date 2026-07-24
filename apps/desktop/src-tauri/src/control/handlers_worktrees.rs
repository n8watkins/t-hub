//! Git-worktree control handlers, split out of `control.rs` to shrink that
//! module. `create_worktree` / `remove_worktree` / `list_worktrees` plus their
//! authorization, git-capability, and rollback helpers - thin delegators to
//! [`crate::git`] / [`crate::files`]. The parent dispatch match routes here.

use super::*;

/// `create_worktree` (WS-4): create a git worktree, then open it as a new
/// workspace tab with a terminal spawned in the worktree dir. We run the git
/// command HERE (mirroring the Tauri `git_worktree_add` exec) so a git failure
/// (e.g. a branch already checked out elsewhere) is reported up front and nothing
/// is forwarded to the UI on failure. On success we forward an
/// `add_worktree_workspace` command to the frontend via the [`ApplySink`]; the
/// `controlBridge` maps it to the workspace store's atomic create→tab→spawn helper
/// (`addWorktreeWorkspace`), which is the same path the FilePanel UI uses. The git
/// worktree already exists by then, so the store SKIPS its own `gitWorktreeAdd` —
/// the forward carries `alreadyCreated: true`. Args: `repoRoot`, `worktreePath`
/// (required); `branch`, `tabName`, `startupCommand` (optional).
///
/// `startupCommand` mirrors `spawn_terminal`'s: the command the worktree
/// terminal execs back into inside an interactive login shell (e.g.
/// `claude --resume <id>`), plumbed through the SAME `pane_command` / `-ilc` exec
/// path `spawn_terminal` uses. Without it a worktree crew booted to a bare shell
/// (the provisioning gap powder/Cortana hit).
pub(super) fn create_worktree(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    enforce_public_spawn_contract("create_worktree", args, caller, trusted_internal)?;
    let repo_root = arg_str(args, "repoRoot")
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("create_worktree requires a 'repoRoot' argument")?;
    let worktree_path = arg_str(args, "worktreePath")
        .or_else(|| arg_str(args, "worktree_path"))
        .ok_or("create_worktree requires a 'worktreePath' argument")?;
    let branch = arg_str(args, "branch");
    let tab_name = arg_str(args, "tabName").or_else(|| arg_str(args, "tab_name"));
    // The command the worktree terminal execs into, same contract + exec path as
    // spawn_terminal's startupCommand (snake alias for parity with the other args).
    let startup_command =
        arg_str(args, "startupCommand").or_else(|| arg_str(args, "startup_command"));
    // Captain-chat phase 2: a captain staging a crew worktree identifies itself
    // so the worktree terminal is recorded as crew (same contract as
    // spawn_terminal's spawnedBy).
    let spawned_by = arg_str(args, "spawnedBy").or_else(|| arg_str(args, "spawned_by"));

    let delegated_audit = authorize_worktree_maintenance(
        ctx,
        caller,
        trusted_internal,
        args,
        &repo_root,
        &worktree_path,
        startup_command.as_deref(),
        spawned_by.as_deref(),
    )?;
    let (repo_root, worktree_path) = if ctx.peer_is_loopback {
        (repo_root, worktree_path)
    } else {
        let roots = files::remote_file_roots();
        (
            files::scoped_create_path(&repo_root, true, roots)?
                .to_string_lossy()
                .into_owned(),
            files::scoped_create_path(&worktree_path, true, roots)?
                .to_string_lossy()
                .into_owned(),
        )
    };
    require_registered_git_capability(ctx, "create_worktree", &repo_root)?;
    let result = create_worktree_authorized(
        ctx,
        args,
        repo_root,
        worktree_path,
        branch,
        tab_name,
        startup_command,
        spawned_by,
        &delegated_audit,
    );
    record_delegated_admin_outcome(ctx, delegated_audit.as_ref(), &result);
    result
}

/// Stable capability failure shared by Git-only control operations.
/// The fields are deliberately stable so adapters do not parse locale-dependent
/// Git output or receive JSON encoded inside an error string.
pub(super) fn require_git_capability(operation: &str, root: &str) -> Result<(), String> {
    let posix_root = files::posix_form(root);
    if git::git_info_cached(&posix_root).is_repo {
        return Ok(());
    }
    Err(format!(
        "git_required code=git_required operation={operation} capability=git action=initialize_git"
    ))
}

pub(super) fn require_registered_git_capability(
    ctx: &ControlContext,
    operation: &str,
    root: &str,
) -> Result<(), String> {
    let identity = files::posix_form(root).trim_end_matches('/').to_string();
    let mut candidates = ctx
        .captains
        .projects()
        .into_iter()
        .filter_map(|project| {
            let project_root = files::posix_form(&project.repo_root)
                .trim_end_matches('/')
                .to_string();
            (identity == project_root || identity.starts_with(&format!("{project_root}/")))
                .then_some((project_root.len(), project))
        })
        .collect::<Vec<_>>();
    let Some(max_specificity) = candidates.iter().map(|(length, _)| *length).max() else {
        return Ok(());
    };
    candidates.retain(|(length, _)| *length == max_specificity);
    if candidates.len() != 1 {
        return Err(format!(
            "{operation}: registered Project identity for '{}' is ambiguous; refusing Git capability resolution",
            identity
        ));
    }
    if candidates[0].1.vcs_capability.as_deref() == Some("none") {
        return Err(format!(
            "git_required code=git_required operation={operation} capability=git action=initialize_git"
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_worktree_authorized(
    ctx: &ControlContext,
    args: &Value,
    repo_root: String,
    worktree_path: String,
    branch: Option<String>,
    tab_name: Option<String>,
    startup_command: Option<String>,
    spawned_by: Option<String>,
    delegated_audit: &Option<crate::delegated_admin::AdminAuditContext>,
) -> Result<Value, String> {
    // #27: a REMOTE peer may create worktrees ONLY under the operator allowlist —
    // this execs `git worktree add` SERVER-SIDE at peer-controlled paths (a write/
    // exec surface). Loopback (the local frontend/MCP) is unrestricted. For a remote
    // peer we substitute the SCOPED (normalized) paths so the security check and the
    // git call can't diverge; the new worktree dir doesn't exist yet, hence
    // scoped_create_path (checks the deepest existing ancestor).
    let (repo_root, worktree_path) = if ctx.peer_is_loopback {
        (repo_root, worktree_path)
    } else {
        let roots = files::remote_file_roots();
        (
            files::scoped_create_path(&repo_root, true, roots)?
                .to_string_lossy()
                .into_owned(),
            files::scoped_create_path(&worktree_path, true, roots)?
                .to_string_lossy()
                .into_owned(),
        )
    };

    // Create the worktree on disk first (shares git_worktree_add's impl). A git
    // failure short-circuits here — no tab/terminal is spawned for a failed add.
    let git_output = git::worktree_add(&repo_root, &worktree_path, branch.as_deref())?;

    // A delegated administrator owns filesystem maintenance, not runtime creation.
    // Stop at the exact authorized artifact boundary: no tab, terminal, identity,
    // capability, Crew membership, or UI orchestration is created or forwarded.
    if delegated_audit.is_some() {
        return Ok(json!({
            "accepted": "create_worktree",
            "worktreePath": worktree_path,
            "branch": branch,
            "tabId": Value::Null,
            "terminalId": Value::Null,
            "gitOutput": git_output,
            "delegatedAdmin": delegated_audit,
            "administrativeMaintenanceOnly": true,
            "crewRecorded": false,
            "audited": true,
            "applied": false,
            "note": "worktree filesystem maintenance completed within the delegated Ship Admin grant; no runtime, identity, tab, or UI state was created.",
        }));
    }

    // Resolve the TARGET TAB by NAME deterministically (TASK C / #22): the tile
    // must land in a tab identified by name, NOT in whatever tab is focused. Reuse
    // an existing tab with this name; otherwise mint a fresh id CORE-side. We record
    // it in the registry now (so it's addressable immediately) and forward the
    // chosen `tabId` so the frontend places the tile in THAT tab (creating it with
    // this id+name if needed) rather than defaulting to the active workspace.
    let effective_tab_name = tab_name
        .clone()
        .or_else(|| branch.clone())
        .unwrap_or_else(|| final_path_component(&worktree_path));
    let existing_tab_id = ctx.tabs.id_for_name(&effective_tab_name);
    let tab_was_created = existing_tab_id.is_none();
    let tab_id = existing_tab_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ctx.tabs.insert_tab(&tab_id, &effective_tab_name);

    // Headless-org: spawn the worktree terminal SERVER-side (the server owns tmux
    // either way - the webview's spawnTerminal IPC lands in this same process) and
    // place the tile in the named tab in the authoritative registry, so placement
    // holds even when that tab is hidden or the window is minimized, and the
    // terminal id is returned synchronously. With NO UI at all (no sink, no
    // subscribers), keep the headless behavior: worktree created on disk, tab
    // recorded, no terminal spawned (nothing would render it).
    let has_ui = ctx.apply_sink.is_some() || ctx.fanout.subscriber_count() > 0;
    let mut terminal_id: Option<String> = None;
    let mut tab_id = tab_id;
    if has_ui {
        // Worktree terminals are Crew and receive stable discovery plus a durable
        // per-session identity token.
        let (elevation, minted_identity) =
            match spawn_env_with_identity(ctx, args, "create_worktree", None) {
                Ok(value) => value,
                Err(error) => {
                    let rollback = rollback_created_worktree_state(
                        ctx,
                        &repo_root,
                        &worktree_path,
                        &tab_id,
                        tab_was_created,
                    );
                    return Err(create_worktree_rollback_error(
                        format!("create_worktree: identity persistence failed: {error}"),
                        rollback,
                    ));
                }
            };
        // Wrap the startupCommand into the pane exec the SAME way spawn_terminal
        // does (pane_command → an interactive login shell that execs the command);
        // None keeps the prior bare-shell behavior. No `shell` preset for a
        // worktree spawn - the crew boots into the worktree dir running this.
        let pane = crate::commands::pane_command(None, startup_command.as_deref());
        match spawn_tmux_terminal(&worktree_path, pane.as_deref(), &elevation) {
            Ok((id, _)) => {
                if let Some(identity) = &minted_identity {
                    if let Err(error) = ctx.identity.bind_tile(&identity.id, &id) {
                        let terminal_reap = tmux::kill_session_tree(&tmux_target(&id));
                        let identity_rollback = ctx.identity.retire(&identity.id);
                        let primary = format!(
                            "create_worktree: identity binding persistence failed: {error}{}",
                            identity_rollback
                                .err()
                                .map(|rollback| format!(
                                    "; identity rollback also failed: {rollback}"
                                ))
                                .unwrap_or_default()
                        );
                        let rollback = match terminal_reap {
                            Ok(()) => rollback_created_worktree_state(
                                ctx,
                                &repo_root,
                                &worktree_path,
                                &tab_id,
                                tab_was_created,
                            ),
                            Err(reap_error) => {
                                let tab_rollback =
                                    rollback_created_tab(ctx, &tab_id, tab_was_created);
                                Err(format!(
                                    "terminal reap failed ({reap_error}); the worktree was preserved{}",
                                    tab_rollback
                                        .err()
                                        .map(|error| format!("; tab rollback also failed: {error}"))
                                        .unwrap_or_default()
                                ))
                            }
                        };
                        return Err(create_worktree_rollback_error(primary, rollback));
                    }
                }
                // Atomic placement with fallback: if the named tab was closed in
                // the race window between resolution and placement, the tile
                // lands in the active (else first) tab - never orphaned outside
                // the registry. tab_id then reflects the ACTUAL placement.
                if let Some(placed) = ctx.tabs.place_tile_with_fallback(&id, Some(&tab_id)) {
                    tab_id = placed;
                }
                terminal_id = Some(id);
            }
            Err(e) => {
                // Review L2: retire the just-minted identity so a failed worktree
                // spawn does not leave an orphaned, secret-bearing entry.
                if let Some(identity) = &minted_identity {
                    if let Err(rollback) = ctx.identity.retire(&identity.id) {
                        let tab_rollback = rollback_created_tab(ctx, &tab_id, tab_was_created);
                        return Err(ambiguous_spawn_rollback_error(&e, &rollback, tab_rollback));
                    }
                }
                eprintln!("t-hub-control: create_worktree: worktree terminal spawn failed: {e}")
            }
        }
    }

    // Captain-chat phase 2: link the spawned worktree terminal to its captain.
    // No terminal (headless boot / spawn failure) = no crew session to record.
    let crew_recorded = match (&spawned_by, &terminal_id) {
        (Some(cap), Some(id)) => match ctx.captains.record_crew(cap, id) {
            Ok(recorded) => recorded,
            Err(error) => {
                let _ = close_terminal(ctx, &json!({ "sessionId": id }));
                return Err(format!(
                    "create_worktree: Crew registry persistence failed and the terminal was rolled back: {error}"
                ));
            }
        },
        _ => false,
    };
    if crew_recorded {
        let _ = captains_sync_apply(ctx);
    }

    // Forward the UI orchestration (open/reuse the named tab + adopt the spawned
    // terminal, rendered from the attached registry snapshot). The git worktree
    // already exists, so `alreadyCreated: true` tells any legacy consumer not to
    // run `gitWorktreeAdd` again.
    let forward = with_sync(
        ctx,
        json!({
            "worktreePath": worktree_path,
            "repoRoot": repo_root,
            "branch": branch,
            "tabId": tab_id,
            "tabName": effective_tab_name,
            "terminalId": terminal_id,
            "startupCommand": startup_command,
            "alreadyCreated": true,
        }),
    );
    let applied = has_ui && forward_apply(ctx, "add_worktree_workspace", &forward);
    Ok(json!({
        "accepted": "create_worktree",
        "worktreePath": worktree_path,
        "branch": branch,
        "tabId": tab_id,
        "tabName": effective_tab_name,
        "terminalId": terminal_id,
        "startupCommand": startup_command,
        "gitOutput": git_output,
        "spawnedBy": spawned_by,
        "crewRecorded": crew_recorded,
        "delegatedAdmin": delegated_audit,
        "audited": true,
        "applied": applied,
        "note": if applied {
            "worktree created on disk; the terminal was spawned server-side and \
             placed in the tab identified by tabName in the authoritative registry \
             (the user's active tab is not switched)."
        } else {
            "worktree created on disk; the UI tab/terminal forward was not \
             delivered (headless/no sink)."
        },
    }))
}

pub(super) fn authorize_worktree_maintenance(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    args: &Value,
    repo_root: &str,
    worktree_path: &str,
    startup_command: Option<&str>,
    spawned_by: Option<&str>,
) -> Result<Option<crate::delegated_admin::AdminAuditContext>, String> {
    let admin_grants = caller
        .map(|caller| ctx.delegated_admin.grants_for_actor(&caller.session_id))
        .unwrap_or_default();
    let active_admin_grant = admin_grants
        .iter()
        .find(|grant| grant.state.is_active())
        .cloned();
    let has_admin_history = !admin_grants.is_empty();
    if trusted_internal && caller.is_none() {
        return Ok(None);
    }
    let caller = caller.ok_or("acl: create_worktree requires a session identity")?;
    if has_admin_history && active_admin_grant.is_none() {
        return Err(
            "acl: create_worktree administrative identity has no active Ship Admin grant".into(),
        );
    }
    if active_admin_grant.is_none() && caller_is_apex(Some(caller), false) {
        return Ok(None);
    }
    let snapshot = ctx.captains.snapshot();
    let project = snapshot
        .projects
        .iter()
        .find(|project| files::posix_form(&project.repo_root) == files::posix_form(repo_root))
        .ok_or("acl: create_worktree repository is not a registered Project")?;

    if active_admin_grant.is_none() && caller.fleet_role == Some(FleetRole::Captain) {
        let captain = snapshot.captains.iter().find(|captain| {
            captain.role == FleetRole::Captain
                && captain.state == ClaimState::Active
                && captain.terminal_id.as_deref() == caller.tile.as_deref()
                && captain.ship_slug == caller.ship_slug.as_deref().unwrap_or_default()
                && captain.project_id.as_deref() == Some(project.project_id.as_str())
        });
        let captain = captain.ok_or(
            "acl: create_worktree requires the active Captain that owns the registered Project",
        )?;
        if spawned_by.is_some_and(|terminal_id| captain.terminal_id.as_deref() != Some(terminal_id))
        {
            return Err("acl: create_worktree spawnedBy must name the owning Captain".into());
        }
        return Ok(None);
    }

    let runtime_or_capability_requested = startup_command.is_some()
        || spawned_by.is_some()
        || [
            "capability",
            "admissionPurpose",
            "admission_purpose",
            "shell",
            "preset",
            "startupCommand",
            "startup_command",
            "spawnedBy",
            "spawned_by",
        ]
        .iter()
        .any(|field| args.get(field).is_some_and(|value| !value.is_null()));
    if runtime_or_capability_requested {
        return Err(
            "acl: delegated worktree maintenance cannot create or elevate a runtime, dispatch implementation, or assign Crew".into(),
        );
    }
    let grant = active_admin_grant
        .ok_or("acl: create_worktree requires the owning Captain or a Ship Admin grant")?;
    let ship_slug = match &grant.scope {
        crate::delegated_admin::AdminScope::Ship { ship_slug } => ship_slug.clone(),
        crate::delegated_admin::AdminScope::Fleet => {
            return Err("acl: Fleet Admins cannot create implementation worktrees".into());
        }
    };
    let owns_project = snapshot.captains.iter().any(|captain| {
        captain.role == FleetRole::Captain
            && captain.state == ClaimState::Active
            && captain.ship_slug == ship_slug
            && captain.project_id.as_deref() == Some(project.project_id.as_str())
    });
    if !owns_project {
        return Err(format!(
            "acl: Ship Admin scope '{ship_slug}' does not own this registered Project"
        ));
    }
    authorize_delegated_admin(
        ctx,
        caller,
        crate::delegated_admin::AdminOperation::MaintainWorktree,
        crate::delegated_admin::AdminTarget::Worktree {
            ship_slug,
            worktree_id: files::posix_form(worktree_path),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .map(Some)
}

pub(super) fn create_worktree_rollback_error(
    primary: String,
    rollback: Result<(), String>,
) -> String {
    match rollback {
        Ok(()) => format!("{primary}; the new worktree was rolled back"),
        Err(error) => format!("{primary}; worktree rollback also failed: {error}"),
    }
}

pub(super) fn ambiguous_spawn_rollback_error(
    spawn_error: &str,
    identity_error: &str,
    tab_rollback: Result<(), String>,
) -> String {
    format!(
        "create_worktree: terminal spawn failed ({spawn_error}); identity rollback also failed: {identity_error}; the worktree was preserved because terminal cleanup was not confirmed{}",
        tab_rollback
            .err()
            .map(|error| format!("; tab rollback also failed: {error}"))
            .unwrap_or_default()
    )
}

pub(super) fn rollback_created_tab(
    ctx: &ControlContext,
    tab_id: &str,
    tab_was_created: bool,
) -> Result<(), String> {
    if tab_was_created {
        ctx.tabs.rollback_owned_empty_tab(tab_id)
    } else {
        Ok(())
    }
}

pub(super) fn rollback_created_worktree_state(
    ctx: &ControlContext,
    repo_root: &str,
    worktree_path: &str,
    tab_id: &str,
    tab_was_created: bool,
) -> Result<(), String> {
    let worktree = git::rollback_created_worktree(repo_root, worktree_path);
    let tab = rollback_created_tab(ctx, tab_id, tab_was_created);
    match (worktree, tab) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(worktree), Ok(())) => Err(worktree),
        (Ok(()), Err(tab)) => Err(format!("tab rollback failed: {tab}")),
        (Err(worktree), Err(tab)) => Err(format!(
            "worktree rollback failed: {worktree}; tab rollback failed: {tab}"
        )),
    }
}

/// The final non-empty path component of a POSIX path (the worktree dir's name),
/// used as a fallback tab name when neither `tabName` nor `branch` was given.
pub(super) fn final_path_component(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// `remove_worktree` (WS-4): fail closed until one backend service can prove the
/// complete removal decision required by the worktree status contract.
///
/// The same gate is used by direct Tauri removal, so control, MCP, CLI, and UI
/// callers all receive a synchronous refusal before any UI detach or Git
/// mutation. Args: `repoRoot`, `worktreePath` (required); `force` (optional).
pub(super) fn remove_worktree(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let repo_root = arg_str(args, "repoRoot")
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("remove_worktree requires a 'repoRoot' argument")?;
    let worktree_path = arg_str(args, "worktreePath")
        .or_else(|| arg_str(args, "worktree_path"))
        .ok_or("remove_worktree requires a 'worktreePath' argument")?;
    enforce_project_path_authority(ctx, caller, trusted_internal, &repo_root, "remove_worktree")?;
    enforce_project_path_authority(
        ctx,
        caller,
        trusted_internal,
        &worktree_path,
        "remove_worktree",
    )?;
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    // #27: a REMOTE peer may remove worktrees ONLY under the operator allowlist —
    // this forwards a `git worktree remove` of a peer-controlled path to the UI.
    // Loopback is unrestricted. (scoped_create_path also handles the existing path.)
    let (repo_root, worktree_path) = if ctx.peer_is_loopback {
        (repo_root, worktree_path)
    } else {
        let roots = files::remote_file_roots();
        (
            files::scoped_create_path(&repo_root, true, roots)?
                .to_string_lossy()
                .into_owned(),
            files::scoped_create_path(&worktree_path, true, roots)?
                .to_string_lossy()
                .into_owned(),
        )
    };
    require_registered_git_capability(ctx, "remove_worktree", &repo_root)?;

    // Preserve the remote path security boundary, then fail every authorized
    // caller before forwarding UI state or invoking Git.
    git::require_worktree_removal_safety_service()?;

    let forward = json!({
        "worktreePath": worktree_path,
        "repoRoot": repo_root,
        "force": force,
    });
    match &ctx.apply_sink {
        Some(sink) => {
            sink.apply("remove_worktree_workspace", &forward)
                .map_err(|e| {
                    format!("remove_worktree: failed to forward removal to the UI: {e}")
                })?;
            // T12: a native cockpit attached to this same server detaches its
            // own tiles rooted in the worktree in parallel; the detach->git
            // ordering and the git removal itself stay webview-owned. (With no
            // sink there is still no removal path - the refusal below - because
            // a socket client cannot run the git side; documented T12 deviation,
            // revisited at the T14 cutover.)
            let _ = broadcast_apply(ctx, "remove_worktree_workspace", &forward);
            Ok(json!({
                "accepted": "remove_worktree",
                "worktreePath": worktree_path,
                "force": force,
                "audited": true,
                // We only *forwarded* the removal request over this channel — the
                // real `git worktree remove` runs later in the frontend (after it
                // detaches live tiles) and can still fail (dirty tree without
                // force, a tile detach throwing). The control channel cannot
                // confirm that completion synchronously, so we report `requested`,
                // not `applied`, to avoid falsely telling the caller it succeeded.
                "requested": true,
                "note": "the UI was asked to detach any live tiles rooted in the \
                         worktree and then remove it (git worktree remove). \
                         Completion is NOT confirmed synchronously over this \
                         channel — the removal runs in the frontend and may still \
                         fail (e.g. a dirty tree without force).",
            }))
        }
        None => {
            // No UI at all ⇒ refuse rather than orphan a process unwitnessed.
            if ctx.fanout.subscriber_count() == 0 {
                return Err(
                    "remove_worktree: no UI is connected to detach the worktree's live \
                     tiles first; refusing to remove it to avoid orphaning a running \
                     process (the app must be running for worktree removal)"
                        .to_string(),
                );
            }
            // T-B native path: detach broadcast FIRST (queued to every
            // subscriber's socket), then the git removal server-side. A git
            // failure (e.g. dirty tree without force) surfaces verbatim — the
            // detach has still been requested, exactly like the webview path
            // where gitWorktreeRemove rejects after the tiles detached.
            let applied = broadcast_apply(ctx, "remove_worktree_workspace", &forward) > 0;
            git::worktree_remove(&repo_root, &worktree_path, force)?;
            Ok(json!({
                "accepted": "remove_worktree",
                "worktreePath": worktree_path,
                "force": force,
                "audited": true,
                "applied": applied,
                "removed": true,
                "note": "no webview is attached; the detach forward was broadcast to \
                         socket UI subscribers (the native cockpit detaches its tiles \
                         rooted in the worktree) and the server then ran `git worktree \
                         remove` itself. The removal IS confirmed: the worktree is gone.",
            }))
        }
    }
}

/// `list_worktrees` (T-B, read-only): the worktrees of the repo containing `cwd`
/// — the socket twin of the `git_worktree_list` Tauri command, sharing its
/// implementation (`git::worktree_list`), so a socket UI can build the worktree
/// list/re-open/remove flow the webview drives via IPC. Best-effort like the
/// IPC twin: a non-repo yields an empty list. Args: `cwd` (or `path`/`repoRoot`).
/// Remote peers are allowlist-gated exactly like `git_info` (the probe leaks
/// repo topology for an arbitrary host path).
pub(super) fn list_worktrees(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let cwd = arg_str(args, "cwd")
        .or_else(|| arg_str(args, "path"))
        .or_else(|| arg_str(args, "repoRoot"))
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("list_worktrees requires a 'cwd' argument")?;
    let cwd = if ctx.peer_is_loopback {
        cwd.to_string()
    } else {
        files::scoped_create_path(&cwd, true, files::remote_file_roots())?
            .to_string_lossy()
            .into_owned()
    };
    require_registered_git_capability(ctx, "list_worktrees", &cwd)?;
    let list = git::worktree_list(&cwd)?;
    Ok(json!({ "worktrees": list }))
}
