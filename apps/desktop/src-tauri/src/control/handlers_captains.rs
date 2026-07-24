//! Captain claim / checkpoint lifecycle control handlers, split out of
//! `control.rs` to shrink that module. `claim_captain` / `claim_captain_locked`
//! / `release_captain` / `rename_captain`, `captain_checkpoint`, and the
//! `captains_sync_apply` registry->UI forwarder shared across the captain and
//! spawn handlers. The parent dispatch match routes here.

use super::*;

/// Forward the authoritative captains snapshot to the UI as a `sync_captains`
/// apply (captain-chat phase 2) - the captains twin of [`with_sync`]'s tab
/// snapshot, emitted AFTER a registry mutation so the UI renders FROM the
/// registry. Rides the same [`forward_apply`] path (webview sink + T12 socket
/// broadcast). Returns whether the forward was delivered.
pub(super) fn captains_sync_apply(ctx: &ControlContext) -> bool {
    let snap = ctx.captains.snapshot();
    let args = json!({ "sync": serde_json::to_value(&snap).unwrap_or(Value::Null) });
    forward_apply(ctx, "sync_captains", &args)
}

pub(super) fn captain_checkpoint(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "captain_checkpoint")?;
    let captain_session_id =
        arg_str(args, "captainSessionId").or_else(|| arg_str(args, "captain_session_id"));
    let ship_slug = arg_str(args, "shipSlug").or_else(|| arg_str(args, "ship_slug"));
    let crew_session_id =
        arg_str(args, "crewSessionId").or_else(|| arg_str(args, "crew_session_id"));
    let conversation_id =
        arg_str(args, "conversationId").or_else(|| arg_str(args, "conversation_id"));
    let resume_point = arg_str(args, "resumePoint").or_else(|| arg_str(args, "resume_point"));
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .iter()
        .find(|captain| {
            captain_session_id
                .as_deref()
                .is_some_and(|id| captain.terminal_id.as_deref() == Some(id))
                || ship_slug
                    .as_deref()
                    .is_some_and(|slug| captain.ship_slug == slug)
        })
        .ok_or("captain_checkpoint: no matching Captain is registered")?;
    let target = crew_session_id
        .as_deref()
        .or(captain.terminal_id.as_deref())
        .ok_or("captain_checkpoint: Captain has no active terminal")?;
    let _ = enforce_target_lifecycle_authority(ctx, caller, trusted_internal, target)?;

    let captain = ctx.captains.checkpoint(
        captain_session_id.as_deref(),
        ship_slug.as_deref(),
        crew_session_id.as_deref(),
        conversation_id.as_deref(),
        resume_point.as_deref(),
    )?;
    let _ = captains_sync_apply(ctx);
    Ok(json!({
        "accepted": "captain_checkpoint",
        "audited": true,
        "captain": captain,
        "target": if crew_session_id.is_some() { "crew" } else { "captain" },
    }))
}

pub(super) fn rename_captain(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "rename_captain")?;
    let captain_session_id =
        arg_str(args, "captainSessionId").or_else(|| arg_str(args, "captain_session_id"));
    let ship_slug = arg_str(args, "shipSlug").or_else(|| arg_str(args, "ship_slug"));
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .iter()
        .find(|captain| {
            captain_session_id
                .as_deref()
                .is_some_and(|id| captain.terminal_id.as_deref() == Some(id))
                || ship_slug
                    .as_deref()
                    .is_some_and(|slug| captain.ship_slug == slugify_ship(slug))
        })
        .ok_or("rename_captain: no matching Captain is registered")?;
    enforce_ship_authority(caller, trusted_internal, &captain.ship_slug)?;
    let display_name = arg_str(args, "displayName")
        .or_else(|| arg_str(args, "display_name"))
        .ok_or("rename_captain requires a 'displayName' argument")?;
    let captain = ctx.captains.rename_captain(
        captain_session_id.as_deref(),
        ship_slug.as_deref(),
        &display_name,
    )?;
    let _ = captains_sync_apply(ctx);
    Ok(json!({
        "accepted": "rename_captain",
        "audited": true,
        "captain": captain,
    }))
}

/// `claim_captain` (Organization, audited; captain-chat phase 2): claim captaincy
/// of a ship. This is a durable authority mutation, separate from the UI's visual
/// overlay pin. It is registry-first (strict: a ship already captained by another
/// session is refused), then the authoritative captains snapshot is forwarded via
/// `sync_captains` so every client renders from it. Args: `captainSessionId` (or
/// `sessionId`) required; `shipSlug` optional (slugified; defaults to
/// `ship-<sessionId>`); `workspaceTabIds` optional and always explicit (an omitted
/// list owns no Work Workspace).
///
/// LIVENESS: the session must be a LIVE terminal (`th_<id>` exists in tmux) - a
/// claim for a dead/unknown session would persist and linger forever (nothing
/// ever kills a session that never existed). A re-claim that changes nothing is
/// idempotent: `seq` is unchanged and no redundant `sync_captains` is forwarded.
pub(super) fn claim_captain(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let _identity_transaction = ctx.tabs.identity_transaction();
    claim_captain_locked(ctx, args, caller, trusted_internal, None, true)
}

pub(super) fn claim_captain_locked(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    binding: Option<(&str, &str, &str)>,
    forward_projection: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "claim_captain")?;
    let captain_session_id = arg_str(args, "captainSessionId")
        .or_else(|| arg_str(args, "captain_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("claim_captain requires a 'captainSessionId' argument")?;
    // Resolve authority before probing runtime state so a foreign caller cannot
    // use this command as a terminal-liveness oracle.
    let ship_slug = arg_str(args, "shipSlug").or_else(|| arg_str(args, "ship_slug"));
    let role = match arg_str(args, "role").map(|r| r.to_ascii_lowercase()) {
        Some(r) if r == "cortana" => FleetRole::Cortana,
        _ if ship_slug.as_deref().map(slugify_ship).as_deref() == Some(CORTANA_SLUG) => {
            FleetRole::Cortana
        }
        _ => FleetRole::Captain,
    };
    enforce_attach_authority(ctx, caller, trusted_internal, &captain_session_id, role)?;
    // Liveness: refuse a claim for a session with no live terminal, so a bogus
    // or raced id can never be persisted into captains.json to linger forever.
    // De-conflation (spawn-wedge): only a DEFINITIVE `Gone` rejects; an `Unknown`
    // probe is retryable (refuse to persist a claim on an unverifiable session, but
    // never assert it is dead) so a degraded control plane can't block a legitimate
    // claim by mislabelling a live session as gone.
    match tmux::session_liveness(&tmux_target(&captain_session_id)) {
        tmux::SessionLiveness::Alive => {}
        tmux::SessionLiveness::Gone => {
            return Err(format!(
                "claim_captain: no live terminal for session '{captain_session_id}' \
                 (spawn or attach it first - a claim for a dead session would linger)"
            ));
        }
        tmux::SessionLiveness::Unknown => {
            return Err(retryable_error(format!(
                "claim_captain: liveness probe for session '{captain_session_id}' timed out; \
                 not confirmed live — retry (refusing to persist a claim on an unverified session)"
            )));
        }
    }
    // Provider continuity is a checkpoint hint only. It never bypasses the live
    // incumbent check, and runtime-derived identity wins when available.
    let requested_provider = arg_str(args, "provider")
        .or_else(|| arg_str(args, "harness"))
        .map(|value| value.trim().to_ascii_lowercase());
    let existing_provider = ctx
        .captains
        .captain_for_session(&captain_session_id)
        .and_then(|captain| captain.provider.or(captain.harness));
    let runtime_provider = detected_harness(&captain_session_id);
    if let Some(requested) = requested_provider.as_deref() {
        if runtime_provider.as_deref() != Some(requested) {
            return Err(format!(
                "claim_captain: declared provider '{requested}' does not match a live harness in terminal '{captain_session_id}'"
            ));
        }
    }
    let provider = requested_provider
        .or(runtime_provider)
        .or(existing_provider)
        .ok_or_else(|| {
            format!(
                "claim_captain: no supported harness is live in terminal '{captain_session_id}'"
            )
        })?;
    if provider != "claude" && provider != "codex" {
        return Err(format!(
            "claim_captain: unsupported provider '{provider}' (expected 'codex' or 'claude')"
        ));
    }
    if tmux::harness_liveness(&tmux_target(&captain_session_id), &provider)
        != tmux::SessionLiveness::Alive
    {
        return Err(format!(
            "claim_captain: provider '{provider}' is not verifiably live in terminal '{captain_session_id}'"
        ));
    }
    let presented_provider_session_id = arg_str(args, "providerSessionId")
        .or_else(|| arg_str(args, "provider_session_id"))
        .or_else(|| arg_str(args, "conversationId"))
        .or_else(|| arg_str(args, "conversation_id"));
    let provider_session_id = trusted_provider_session_id(
        ctx,
        &captain_session_id,
        &provider,
        presented_provider_session_id,
    )?;
    let workspace_tab_ids: Vec<String> = args
        .get("workspaceTabIds")
        .or_else(|| args.get("workspace_tab_ids"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if tmux::harness_liveness(&tmux_target(&captain_session_id), &provider)
        != tmux::SessionLiveness::Alive
    {
        return Err(format!(
            "claim_captain: provider '{provider}' stopped before the identity transaction committed"
        ));
    }
    let previous_workspace = ctx.tabs.workspace_for_tile(&captain_session_id);
    let captain_workspace_existed = ctx.tabs.has_tab(CAPTAIN_WORKSPACE_ID);
    let requested_slug = match role {
        FleetRole::Cortana => CORTANA_SLUG.to_string(),
        FleetRole::Captain => ship_slug
            .as_deref()
            .map(slugify_ship)
            .filter(|slug| !slug.is_empty())
            .unwrap_or_else(|| slugify_ship(&format!("ship-{captain_session_id}"))),
    };
    let previous_claim = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| {
            captain.terminal_id.as_deref() == Some(captain_session_id.as_str())
                || captain.ship_slug == requested_slug
        });
    let previous_authority = previous_claim.as_ref().map(|captain| {
        let identity_id = captain
            .terminal_id
            .as_deref()
            .and_then(|terminal_id| ctx.identity.for_tile(terminal_id))
            .map(|identity| identity.id);
        (
            captain.ship_slug.clone(),
            captain.terminal_id.clone(),
            identity_id,
        )
    });
    let mut unique_workspace_ids = std::collections::HashSet::new();
    for workspace_id in &workspace_tab_ids {
        if !unique_workspace_ids.insert(workspace_id.as_str()) {
            return Err(format!(
                "claim_captain: duplicate workspaceTabId '{workspace_id}'"
            ));
        }
        if ctx.tabs.work_workspace(workspace_id).is_none() {
            return Err(format!(
                "claim_captain: workspaceTabId '{workspace_id}' is not an existing Work Workspace"
            ));
        }
    }
    let before_seq = ctx.captains.snapshot().seq;
    // The transfer-grade liveness predicate (R1): the SOLE signal that may auto-release
    // an incumbent's slug to this claimant is its tmux session being DEFINITIVELY gone.
    // De-conflation (spawn-wedge): an `Unknown` probe (timed out / failed to spawn) is
    // ambiguous and is NEVER transfer-grade - `is_definitively_gone` returns true only
    // for a completed absent probe, so a degraded control plane can never seize a live
    // ship (item-2 two-tier liveness: ambiguous is never seized). Evaluated lock-free
    // inside `claim` (the CAS discipline, MED-3).
    let is_terminal_dead =
        |tile: &str| tmux::is_definitively_gone(tmux::session_liveness(&tmux_target(tile)));
    // BUG-1: readopt of a resuming captain's Orphaned crew is now GATED on a per-
    // crew liveness probe (Alive->active, Gone->retired, Unknown->stays orphaned)
    // instead of blind-activating every orphan. The probes are tmux subprocesses,
    // so they run HERE, lock-free, into a precomputed map (MED-3: tmux is never
    // called while the registry lock is held). Probe every orphaned crew tile in
    // the current snapshot; `claim` reads the map purely under the lock and any
    // tile not in it (a race adding an orphan after this snapshot) defaults to
    // Unknown -> left orphaned, re-adoptable on the next resume.
    let orphan_liveness: std::collections::HashMap<String, tmux::SessionLiveness> = ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .flat_map(|c| c.crew.iter())
        .filter(|cr| matches!(cr.state, CrewState::Orphaned { .. }))
        .map(|cr| {
            let tile = cr.terminal_id.clone();
            let liveness = tmux::session_liveness(&tmux_target(&tile));
            (tile, liveness)
        })
        .collect();
    let crew_liveness = |tile: &str| {
        orphan_liveness
            .get(tile)
            .copied()
            .unwrap_or(tmux::SessionLiveness::Unknown)
    };
    let outcome = ctx.captains.claim_provider_with_binding(
        &captain_session_id,
        ship_slug.as_deref(),
        role,
        Some(&provider),
        provider_session_id.as_deref(),
        workspace_tab_ids,
        binding,
        &is_terminal_dead,
        &crew_liveness,
    )?;
    ctx.tabs
        .insert_tab(CAPTAIN_WORKSPACE_ID, CAPTAIN_WORKSPACE_NAME);
    if let Err(error) = ctx
        .tabs
        .move_tile(&captain_session_id, CAPTAIN_WORKSPACE_ID)
    {
        let rollback = ctx.captains.rollback_provisioned_claim(
            &captain_session_id,
            &outcome.record,
            previous_claim.clone(),
        );
        let workspace_rollback = (!captain_workspace_existed)
            .then(|| ctx.tabs.rollback_owned_empty_tab(CAPTAIN_WORKSPACE_ID))
            .transpose();
        return Err(format!(
            "claim_captain: Captain Workspace relocation failed: {error}{}{}",
            rollback
                .err()
                .map(|rollback| format!("; registry rollback failed: {rollback}"))
                .unwrap_or_default(),
            workspace_rollback
                .err()
                .map(|rollback| format!("; Workspace rollback failed: {rollback}"))
                .unwrap_or_default()
        ));
    }
    let snap = ctx.captains.snapshot();
    let projection = if forward_projection {
        organization_sync_apply(
            ctx,
            "move_tile",
            json!({"terminalId": captain_session_id, "tabId": CAPTAIN_WORKSPACE_ID}),
        )
    } else {
        Ok(Value::Null)
    };
    if let Err(error) = projection {
        let placement_rollback = ctx
            .tabs
            .restore_tile_placement_locked(&captain_session_id, previous_workspace.as_deref());
        let registry_rollback = ctx.captains.rollback_provisioned_claim(
            &captain_session_id,
            &outcome.record,
            previous_claim,
        );
        let workspace_rollback = (!captain_workspace_existed)
            .then(|| ctx.tabs.rollback_owned_empty_tab(CAPTAIN_WORKSPACE_ID))
            .transpose();
        return Err(format!(
            "claim_captain: Captain Workspace synchronization failed: {error}{}{}{}",
            placement_rollback
                .err()
                .map(|rollback| format!("; placement rollback failed: {rollback}"))
                .unwrap_or_default(),
            registry_rollback
                .err()
                .map(|rollback| format!("; registry rollback failed: {rollback}"))
                .unwrap_or_default(),
            workspace_rollback
                .err()
                .map(|rollback| format!("; Workspace rollback failed: {rollback}"))
                .unwrap_or_default()
        ));
    }
    if let Some((previous_ship_slug, previous_terminal_id, previous_identity_id)) =
        previous_authority
    {
        let ownership_changed = previous_ship_slug != outcome.record.ship_slug
            || previous_terminal_id != outcome.record.terminal_id;
        if ownership_changed {
            ctx.delegated_admin
                .invalidate_ship_delegator(
                    &previous_ship_slug,
                    format!(
                        "Captain ownership for ship '{previous_ship_slug}' changed during claim"
                    ),
                )
                .map_err(|error| format!("{}: {error}", error.code()))?;
            if let Some(identity_id) = previous_identity_id {
                ctx.delegated_admin
                    .invalidate_delegator(
                        &identity_id,
                        "delegating supervisor ownership changed during claim",
                    )
                    .map_err(|error| format!("{}: {error}", error.code()))?;
            }
        }
    }
    if let Some(identity_id) = ctx
        .identity
        .for_tile(&captain_session_id)
        .map(|identity| identity.id)
    {
        ctx.delegated_admin
            .invalidate_actor(
                &identity_id,
                "administrative actor acquired supervisor authority",
            )
            .map_err(|error| format!("{}: {error}", error.code()))?;
    }
    // Idempotent re-claim (unchanged): the registry left `seq` alone, so skip the
    // redundant Captain forward. Placement sync above still heals a stale tile.
    let applied = forward_projection && snap.seq != before_seq && captains_sync_apply(ctx);
    Ok(json!({
        "accepted": "claim_captain",
        "audited": true,
        "applied": applied,
        "captain": outcome.record,
        "disposition": outcome.disposition.label(),
        "seq": snap.seq,
        "captains": snap.captains,
        "note": "captaincy claimed in the server captains registry (authoritative, \
                 persistent) and the snapshot forwarded to the UI (sync_captains).",
    }))
}

/// `release_captain` (Organization, audited; captain-chat phase 2): release a
/// claimed captaincy, addressed by `captainSessionId` (or `sessionId`) or
/// `shipSlug`. Strict (an unknown claim is an error), then the snapshot is
/// forwarded via `sync_captains` exactly like `claim_captain`.
pub(super) fn release_captain(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "release_captain")?;
    let target = arg_str(args, "captainSessionId")
        .or_else(|| arg_str(args, "captain_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .or_else(|| arg_str(args, "shipSlug"))
        .or_else(|| arg_str(args, "ship_slug"))
        .ok_or("release_captain requires a 'captainSessionId' (or 'shipSlug') argument")?;
    let record = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| {
            captain.terminal_id.as_deref() == Some(target.as_str())
                || captain.ship_slug == slugify_ship(&target)
        })
        .ok_or_else(|| format!("release_captain: no claim matches '{target}'"))?;
    enforce_ship_authority(caller, trusted_internal, &record.ship_slug)?;
    ctx.delegated_admin
        .invalidate_ship_delegator(
            &record.ship_slug,
            format!(
                "Captain authority for ship '{}' was released",
                record.ship_slug
            ),
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    if let Some(identity_id) = record
        .terminal_id
        .as_deref()
        .and_then(|terminal_id| ctx.identity.for_tile(terminal_id))
        .map(|identity| identity.id)
    {
        ctx.delegated_admin
            .invalidate_actor(
                &identity_id,
                "actor acquired or released supervisor authority",
            )
            .map_err(|error| format!("{}: {error}", error.code()))?;
    }
    let released = ctx.captains.release(&target)?;
    let applied = captains_sync_apply(ctx);
    let snap = ctx.captains.snapshot();
    Ok(json!({
        "accepted": "release_captain",
        "audited": true,
        "applied": applied,
        "released": released,
        "seq": snap.seq,
        "captains": snap.captains,
    }))
}

// --- Captain provisioning + crew launch (commission_captain / attach_captain) ---
/// Start and durably bind one project Captain as a single process-changing
/// operation. Any failure after spawning reaps the new terminal best-effort.
pub(super) fn commission_captain(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "commission_captain")?;
    if !caller_is_apex(caller, trusted_internal) {
        return Err("acl: only General/Cortana may commission a project Captain".into());
    }
    let project_id = arg_str(args, "projectId")
        .or_else(|| arg_str(args, "project_id"))
        .ok_or("commission_captain requires a 'projectId' argument")?;
    let assignment = arg_str(args, "assignment")
        .filter(|value| !value.trim().is_empty())
        .ok_or("commission_captain requires a non-empty 'assignment' argument")?;
    let harness_name = arg_str(args, "harness").unwrap_or_else(|| "codex".to_string());
    let harness = match harness_name.trim().to_ascii_lowercase().as_str() {
        "codex" => Harness::Codex,
        "claude" => Harness::Claude,
        other => {
            return Err(format!(
                "commission_captain: unsupported harness '{other}' (expected 'codex' or 'claude')"
            ));
        }
    };
    let project = ctx
        .captains
        .projects()
        .into_iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("commission_captain: unknown projectId '{project_id}'"))?;
    let ship_slug = arg_str(args, "shipSlug")
        .or_else(|| arg_str(args, "ship_slug"))
        .map(|value| slugify_ship(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| slugify_ship(&project.name));
    if ship_slug.is_empty() {
        return Err("commission_captain: could not derive a non-empty shipSlug".into());
    }
    #[cfg(test)]
    ctx.captains.pause_dispatch("commission_initial_inspection");
    {
        let _provision = ctx.captains.provision_guard();
        if let Some(response) =
            inspect_commission_contract(ctx, &project, &ship_slug, &assignment, harness)?
        {
            return Ok(response);
        }
    }

    // The inspection-only pass found no exact live Captain. Retry under the
    // global spawn lock order used by Cortana: dispatch admission, then fleet
    // provisioning. Recheck before charging capacity because another caller may
    // have completed the identical commission while this caller waited.
    let _admission_lock = ctx
        .dispatch_admission
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _identity_transaction = ctx.tabs.identity_transaction();
    let _provision = ctx.captains.provision_guard();
    if let Some(response) =
        inspect_commission_contract(ctx, &project, &ship_slug, &assignment, harness)?
    {
        return Ok(response);
    }
    let capacity =
        evaluate_spawn_capacity_for_new_ship(ctx, &ship_slug).map_err(|refusal| refusal.message)?;
    // Commissioning consumes the Captain's provider slot, but the new ship
    // must also have room for its standing Ship Admin.  Keep that slot
    // prospective so a provider cap of one cannot admit a ship that can never
    // satisfy its required administration invariant.
    let standing_admin_needed = capacity.reservations.ship_admins.deficit;
    if capacity.provider_headroom < 1usize.saturating_add(standing_admin_needed)
        || capacity.session_headroom_before_reservations
            < 1usize.saturating_add(standing_admin_needed)
    {
        return Err(format!(
            "commission_captain: insufficient capacity for Captain and standing Ship Admin (provider headroom {}, session headroom {})",
            capacity.provider_headroom, capacity.session_headroom_before_reservations
        ));
    }

    let provisional = CaptainRecord {
        ship_slug: ship_slug.clone(),
        assignment_id: assignment_id_for(Some(&project.project_id), &ship_slug),
        display_name: ship_slug.clone(),
        role: FleetRole::Captain,
        claude_uuid: None,
        provider: Some(harness.as_provider().to_string()),
        provider_session_id: None,
        terminal_id: None,
        project_id: Some(project.project_id.clone()),
        assignment: Some(assignment.clone()),
        harness: Some(harness.as_provider().to_string()),
        conversation_id: None,
        resume_point: None,
        workspace_tab_ids: Vec::new(),
        crew: Vec::new(),
        state: ClaimState::Active,
    };
    let prompt = bootstrap_instructions(&provisional, &project);
    let startup_command = harness
        .adapter()
        .fresh_argv_with_permissions(&prompt, PermMode::BypassPermissions);
    #[cfg(test)]
    let startup_command =
        arg_str(args, "testStartupCommand").unwrap_or_else(|| startup_command.clone());
    let workspace_tab_ids: Vec<String> = args
        .get("workspaceTabIds")
        .or_else(|| args.get("workspace_tab_ids"))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let spawn_args = json!({
        "cwd": project.repo_root,
        "name": format!("Captain - {}", project.name),
        "startupCommand": startup_command,
        "tabId": CAPTAIN_WORKSPACE_ID,
    });
    if ctx.apply_sink.is_none() && ctx.fanout.subscriber_count() == 0 {
        return Err(
            "commission_captain: no UI is connected to adopt the commissioned Captain".into(),
        );
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    #[cfg(test)]
    let terminal_id = arg_str(args, "testTerminalId").unwrap_or_else(|| suffix[..8].to_string());
    #[cfg(not(test))]
    let terminal_id = suffix[..8].to_string();
    let operation = ctx.captains.prepare_commission_operation(
        &terminal_id,
        &project.project_id,
        &assignment,
        &ship_slug,
        harness.as_provider(),
    )?;
    let mut elevation = elevation_env(ctx, &spawn_args);
    audit_control_spawn(ctx, "commission_captain", &spawn_args);
    let identity = match ctx.identity.mint_and_bind(
        crate::identity::Role::Captain,
        Some(ship_slug.clone()),
        &terminal_id,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = ctx
                .captains
                .abort_close_terminal_operation(&operation.operation_id);
            return Err(error);
        }
    };
    elevation.push((
        crate::identity::SESSION_TOKEN_ENV.to_string(),
        identity.secret.clone(),
    ));
    elevation.push((
        PROVIDER_SESSION_ENV.to_string(),
        pending_provider_marker(harness.as_provider()),
    ));
    if let Err(error) = ctx
        .captains
        .bind_commission_operation_identity(&operation.operation_id, &identity.id)
    {
        let _ = ctx.identity.retire(&identity.id);
        let _ = ctx
            .captains
            .abort_close_terminal_operation(&operation.operation_id);
        return Err(error);
    }
    let tmux_cwd = files::posix_form(&project.repo_root);
    let pane = crate::commands::pane_command(None, Some(&startup_command));
    let (_, tmux_session) = match spawn_tmux_terminal_with_id(
        &terminal_id,
        &tmux_cwd,
        pane.as_deref(),
        &elevation,
    ) {
        Ok(spawned) => spawned,
        Err(error) => {
            let _ = ctx.identity.retire(&identity.id);
            let _ = ctx
                .captains
                .abort_close_terminal_operation(&operation.operation_id);
            return Err(format!(
                "commission_captain: terminal startup failed and reserved artifacts were rolled back: {error}"
            ));
        }
    };
    if let Err(error) = wait_for_harness_started(&terminal_id, harness.as_provider()) {
        let _ = tmux::kill_session_tree(&tmux_session);
        let _ = ctx.identity.retire(&identity.id);
        let _ = ctx
            .captains
            .abort_close_terminal_operation(&operation.operation_id);
        return Err(format!(
            "commission_captain: harness startup failed and the terminal was rolled back: {error}"
        ));
    }
    #[cfg(test)]
    ctx.captains.pause_dispatch("commission_effect_applied");
    #[cfg(test)]
    if args
        .get("testCrashAfterTmux")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("injected commission crash after tmux and identity effects".into());
    }
    let claim_args = json!({
        "captainSessionId": terminal_id,
        "shipSlug": ship_slug,
        "provider": harness.as_provider(),
        "workspaceTabIds": workspace_tab_ids,
    });
    #[cfg(test)]
    if args
        .get("testFailCommitPersist")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        ctx.captains
            .fail_next_persist("commission binding persistence failure");
    }
    let claim = claim_captain_locked(
        ctx,
        &claim_args,
        None,
        true,
        Some((&project.project_id, &assignment, harness.as_provider())),
        false,
    );
    if let Err(error) = claim {
        let _ = tmux::kill_session_tree(&tmux_session);
        let _ = ctx.identity.retire(&identity.id);
        let _ = ctx
            .captains
            .abort_close_terminal_operation(&operation.operation_id);
        return Err(format!(
            "commission_captain: terminal was spawned but its Captain claim failed and was rolled back: {error}"
        ));
    }
    let captain = ctx
        .captains
        .captain_for_session(&terminal_id)
        .ok_or("commission_captain: bound Captain disappeared before projection")?;
    let spawned = json!({
        "accepted": "spawn_terminal",
        "id": terminal_id,
        "tmuxSession": tmux_session,
        "cwd": project.repo_root,
        "name": format!("Captain - {}", project.name),
        "startupCommand": startup_command,
        "tabId": CAPTAIN_WORKSPACE_ID,
        "placed": true,
        "audited": true,
    });
    let _ = forward_apply(ctx, "spawn_terminal", &with_sync(ctx, spawned));
    let _ = captains_sync_apply(ctx);
    Ok(commissioned_response(captain, project, false))
}

/// Bind an already-running terminal as a project Captain without changing the
/// terminal's bearer token. The terminal must have been explicitly spawned with
/// control capability. A read-only terminal is refused and must be replaced via
/// `commission_captain`; mutating tmux environment after launch would not revoke
/// the read token already inherited by its process tree and would be a silent,
/// partial elevation.
pub(super) fn attach_captain(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "attach_captain")?;
    let terminal_id = arg_str(args, "captainSessionId")
        .or_else(|| arg_str(args, "captain_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("attach_captain requires a 'captainSessionId' argument")?;
    // Target eligibility is an authority predicate, so evaluate it before runtime,
    // provider, or Project probes can reveal information about a forbidden target.
    enforce_attach_authority(
        ctx,
        caller,
        trusted_internal,
        &terminal_id,
        FleetRole::Captain,
    )?;
    if !caller_is_apex(caller, trusted_internal)
        && caller.and_then(|identity| identity.tile.as_deref()) != Some(terminal_id.as_str())
    {
        return Err("acl: only General/Cortana may attach a different terminal as Captain".into());
    }
    let project_id = arg_str(args, "projectId")
        .or_else(|| arg_str(args, "project_id"))
        .ok_or("attach_captain requires a 'projectId' argument")?;
    let assignment = arg_str(args, "assignment")
        .filter(|value| !value.trim().is_empty())
        .ok_or("attach_captain requires a non-empty 'assignment' argument")?;
    let provider = arg_str(args, "provider")
        .or_else(|| arg_str(args, "harness"))
        .unwrap_or_else(|| "codex".to_string())
        .trim()
        .to_ascii_lowercase();
    if provider != "codex" && provider != "claude" {
        return Err(format!(
            "attach_captain: unsupported provider '{provider}' (expected 'codex' or 'claude')"
        ));
    }
    match tmux::session_liveness(&tmux_target(&terminal_id)) {
        tmux::SessionLiveness::Alive => {}
        tmux::SessionLiveness::Gone => {
            return Err(format!(
                "attach_captain: terminal '{terminal_id}' is not live"
            ));
        }
        tmux::SessionLiveness::Unknown => {
            return Err(retryable_error(format!(
                "attach_captain: terminal '{terminal_id}' liveness is unavailable"
            )));
        }
    }
    let inherited = tmux::session_environment(&tmux_target(&terminal_id), "T_HUB_CONTROL_TOKEN")
        .map_err(|error| {
            format!("attach_captain: could not verify terminal capability: {error}")
        })?;
    if !inherited
        .as_deref()
        .is_some_and(|token| ct_token_eq(token, &ctx.token))
    {
        return Err(format!(
            "attach_captain: terminal '{terminal_id}' is read-only; refusing silent elevation. Use commission_captain to start a control-capability Captain and resume this conversation there"
        ));
    }
    match tmux::harness_liveness(&tmux_target(&terminal_id), &provider) {
        tmux::SessionLiveness::Alive => {}
        tmux::SessionLiveness::Gone => {
            return Err(format!(
                "attach_captain: terminal '{terminal_id}' is not running the declared {provider} harness"
            ));
        }
        tmux::SessionLiveness::Unknown => {
            return Err(retryable_error(format!(
                "attach_captain: could not verify the declared {provider} harness in terminal '{terminal_id}'"
            )));
        }
    }
    let project = ctx
        .captains
        .projects()
        .into_iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("attach_captain: unknown projectId '{project_id}'"))?;
    let ship_slug = arg_str(args, "shipSlug")
        .or_else(|| arg_str(args, "ship_slug"))
        .map(|value| slugify_ship(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| slugify_ship(&project.name));
    // Global lock order: identity transaction precedes fleet provisioning.
    let _identity_transaction = ctx.tabs.identity_transaction();
    let _provision = ctx.captains.provision_guard();
    if let Some(existing) = existing_project_captain(ctx, &project_id, &ship_slug)? {
        if existing.terminal_id.as_deref() != Some(terminal_id.as_str()) {
            return Err(format!(
                "attach_captain: project '{}' already has live Captain '{}'",
                project.name, existing.ship_slug
            ));
        }
    }
    let mut claim_args = json!({
        "captainSessionId": terminal_id,
        "shipSlug": ship_slug,
        "provider": provider,
    });
    for key in ["providerSessionId", "conversationId", "workspaceTabIds"] {
        if let Some(value) = args.get(key) {
            claim_args[key] = value.clone();
        }
    }
    let presented_provider_session_id = arg_str(&claim_args, "providerSessionId")
        .or_else(|| arg_str(&claim_args, "conversationId"));
    if let Some(provider_session_id) =
        trusted_provider_session_id(ctx, &terminal_id, &provider, presented_provider_session_id)?
    {
        claim_args["providerSessionId"] = json!(provider_session_id);
    }
    let previous_claim = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| {
            captain.terminal_id.as_deref() == Some(terminal_id.as_str())
                || captain.ship_slug == ship_slug
        });
    let previous_workspace = ctx.tabs.workspace_for_tile(&terminal_id);
    claim_captain_locked(
        ctx,
        &claim_args,
        None,
        true,
        Some((&project_id, &assignment, &provider)),
        false,
    )
    .map_err(|error| format!("attach_captain: durable claim and binding failed: {error}"))?;
    let captain = ctx
        .captains
        .captain_for_session(&terminal_id)
        .ok_or("attach_captain: bound Captain disappeared before projection")?;
    let instructions = bootstrap_instructions(&captain, &project);
    let attaching_other_terminal =
        caller.and_then(|identity| identity.tile.as_deref()) != Some(terminal_id.as_str());
    if attaching_other_terminal {
        #[cfg(test)]
        let bootstrap_delivery = if args
            .get("testFailBootstrapDelivery")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            Err("injected bootstrap delivery failure".to_string())
        } else {
            send_text(&json!({
                "sessionId": terminal_id,
                "text": instructions,
                "enter": true,
            }))
        };
        #[cfg(not(test))]
        let bootstrap_delivery = send_text(&json!({
            "sessionId": terminal_id,
            "text": instructions,
            "enter": true,
        }));
        if let Err(error) = bootstrap_delivery {
            let rollback =
                ctx.captains
                    .rollback_provisioned_claim(&terminal_id, &captain, previous_claim);
            let placement_rollback = ctx
                .tabs
                .restore_tile_placement_locked(&terminal_id, previous_workspace.as_deref());
            return Err(format!(
                "attach_captain: target bootstrap delivery failed: {error}{}{}",
                rollback
                    .err()
                    .map(|rollback| format!("; registry rollback failed: {rollback}"))
                    .unwrap_or_default(),
                placement_rollback
                    .err()
                    .map(|rollback| format!("; placement rollback failed: {rollback}"))
                    .unwrap_or_default()
            ));
        }
    }
    let _ = organization_sync_apply(
        ctx,
        "move_tile",
        json!({"terminalId": terminal_id, "tabId": CAPTAIN_WORKSPACE_ID}),
    );
    let _ = captains_sync_apply(ctx);
    Ok(json!({
        "accepted": "attach_captain",
        "audited": true,
        "captain": captain,
        "project": project,
        "instructions": instructions,
        "capabilityPreserved": "control",
    }))
}

pub(super) fn validate_crew_checkout(
    project: &ProjectRecord,
    requested: Option<String>,
) -> Result<String, String> {
    require_git_capability("dispatch_crew", &project.repo_root)?;
    let requested = requested.unwrap_or_else(|| project.repo_root.clone());
    require_checkout_wsl_distro(&project.repo_root, "Project root")?;
    require_checkout_wsl_distro(&requested, "requested checkout")?;
    let requested_runtime = files::posix_form(&requested);
    let requested_host = files::to_host_path(&requested_runtime);
    let canonical_host = std::fs::canonicalize(&requested_host).map_err(|error| {
        format!(
            "dispatch_crew: checkout '{}' is unavailable: {error}",
            requested
        )
    })?;
    let project_runtime = files::posix_form(&project.repo_root);
    let worktrees = git::worktree_list(&project_runtime)
        .map_err(|error| format!("dispatch_crew: could not validate project worktrees: {error}"))?;
    let valid = worktrees.iter().any(|worktree| {
        let runtime = files::posix_form(&worktree.path);
        std::fs::canonicalize(files::to_host_path(&runtime))
            .map(|path| path == canonical_host)
            .unwrap_or(false)
    });
    if !valid {
        return Err(format!(
            "dispatch_crew: checkout '{}' is not a worktree of project '{}'",
            canonical_host.display(),
            project.name
        ));
    }
    Ok(files::posix_form(&canonical_host.to_string_lossy()))
}

pub(super) fn require_checkout_wsl_distro(path: &str, field: &str) -> Result<(), String> {
    let Some(path_distro) = explicit_wsl_unc_distro(path) else {
        return Ok(());
    };
    let configured = std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
    if path_distro.eq_ignore_ascii_case(&configured) {
        return Ok(());
    }
    Err(format!(
        "dispatch_crew: {field} uses WSL distribution '{path_distro}', but T-Hub is configured for '{configured}'"
    ))
}

pub(super) fn explicit_wsl_unc_distro(path: &str) -> Option<String> {
    let slashes = path.trim().replace('/', "\\");
    let extended_prefix = "\\\\?\\UNC\\";
    let normalized = if slashes
        .get(..extended_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(extended_prefix))
    {
        format!("\\\\{}", slashes.get(extended_prefix.len()..)?)
    } else {
        slashes
    };
    let without_leading = normalized.strip_prefix("\\\\")?;
    let mut parts = without_leading.split('\\');
    let server = parts.next()?;
    if !server.eq_ignore_ascii_case("wsl.localhost") && !server.eq_ignore_ascii_case("wsl$") {
        return None;
    }
    parts
        .next()
        .filter(|distro| !distro.is_empty())
        .map(str::to_string)
}

pub(super) const CODEX_UNOBSERVED_COMMAND: &str = "t-hub-agent --codex-unobserved";

pub(super) fn crew_interactive_launch(
    harness: Harness,
    provider_launch: &str,
    codex_unobserved_command: &str,
) -> String {
    match harness {
        // The explicit degraded marker is the fail-safe for today's unmirrored
        // interactive TUI. A native lifecycle hook or trusted app-server mirror
        // remains the future structured telemetry path. `exec` preserves the
        // provider-native foreground argv used by launch attestation.
        Harness::Codex => format!("{codex_unobserved_command} && exec {provider_launch}"),
        Harness::Claude => provider_launch.to_string(),
    }
}

pub(super) fn crew_launch_argv(harness: Harness, prompt: &str) -> String {
    let provider_launch = harness
        .adapter()
        .fresh_argv_with_permissions(prompt, CREW_DEFAULT_PERMISSION);
    crew_interactive_launch(harness, &provider_launch, CODEX_UNOBSERVED_COMMAND)
}

pub(super) fn require_exact_args(
    args: &Value,
    command: &str,
    allowed: &[&str],
) -> Result<(), String> {
    let object = args
        .as_object()
        .ok_or_else(|| format!("{command} requires an argument object"))?;
    if let Some(unexpected) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{command}: unexpected argument '{unexpected}'"));
    }
    Ok(())
}

/// Replay durable Fleet intents before the control listener accepts requests.
/// External effects are idempotent, and each handler verifies the exact persisted
/// operation identity again before committing its post-state.
pub fn recover_pending_fleet_operations(ctx: &ControlContext) {
    let pending = ctx.captains.snapshot().pending_fleet_operations;
    for operation in pending {
        match &operation.payload {
            PendingFleetOperationPayload::CloseTerminal { terminal_id, .. } => {
                if let Err(error) = close_terminal_with_policy(
                    ctx,
                    &json!({"sessionId": terminal_id, "force": true}),
                    false,
                    None,
                ) {
                    eprintln!(
                        "t-hub-fleet: pending close operation '{}' remains for retry: {error}",
                        operation.operation_id
                    );
                }
            }
            PendingFleetOperationPayload::CommissionCaptain {
                terminal_id,
                identity_id,
                ..
            } => {
                let _ = tmux::kill_session_tree(&tmux_target(terminal_id));
                if let Some(identity_id) = identity_id {
                    let _ = ctx.identity.retire(identity_id);
                }
                if let Err(error) = ctx
                    .captains
                    .abort_close_terminal_operation(&operation.operation_id)
                {
                    eprintln!(
                        "t-hub-fleet: pending commission operation '{}' remains for retry: {error}",
                        operation.operation_id
                    );
                }
            }
        }
    }
}

/// `report_workspace_tabs` (T12 / headless-org): a UI client up-syncs its live tab
/// layout - the control-socket twin of the Tauri command of the same name (the
/// native cockpit is a socket client and cannot call Tauri). Consistency model
/// (headless-org): the SERVER registry is authoritative; a report carrying
/// `baseSeq` is accepted only when it matches the current revision, otherwise it
/// is rejected as stale and answered with the authoritative snapshot so the
/// reporter converges instead of clobbering a server-side mutation it has not
/// applied yet. A report WITHOUT `baseSeq` (legacy reporter) is accepted
/// unconditionally. Args: `tabs`: `[{id, name, tileIds}]`; `activeTabId`
/// (optional); `baseSeq` (optional).
pub(super) fn report_workspace_tabs(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    if !matches!(
        workspace_mutation_authority(ctx, caller, trusted_internal, "report_workspace_tabs")?,
        WorkspaceMutationAuthority::Apex
    ) {
        return Err(
            "acl: report_workspace_tabs is a full-layout mutation restricted to General/Cortana or the trusted host"
                .into(),
        );
    }
    let tabs: Vec<TabRecord> = serde_json::from_value(
        args.get("tabs")
            .cloned()
            .ok_or("report_workspace_tabs requires a 'tabs' array")?,
    )
    .map_err(|e| format!("report_workspace_tabs: bad 'tabs' shape: {e}"))?;
    let count = tabs.len();
    let active = arg_str(args, "activeTabId").or_else(|| arg_str(args, "active_tab_id"));
    let base_seq = args
        .get("baseSeq")
        .or_else(|| args.get("base_seq"))
        .and_then(|v| v.as_u64());

    match apply_workspace_report(&ctx.tabs, &ctx.captains, tabs, active, base_seq)? {
        (ReportOutcome::Accepted { seq, .. }, captains_changed, reconciled) => {
            if captains_changed {
                let _ = captains_sync_apply(ctx);
            }
            let snapshot = ctx.tabs.snapshot_full();
            Ok(json!({
                "reported": count,
                "seq": seq,
                "stale": reconciled,
                "activeTabId": reconciled.then_some(snapshot.active_tab_id).flatten(),
                "tabs": reconciled.then_some(snapshot.tabs),
            }))
        }
        (ReportOutcome::Stale(snapshot), _, _) => Ok(json!({
            "stale": true,
            "seq": snapshot.seq,
            "activeTabId": snapshot.active_tab_id,
            "tabs": snapshot.tabs,
            "note": "report based on a stale revision; adopt the returned snapshot \
                     and re-report on the next local change.",
        })),
    }
}
