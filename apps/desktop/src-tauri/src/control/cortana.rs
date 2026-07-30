//! The Cortana lifecycle: reattach the recorded orchestrator shell, or create it.
//!
//! Cortana is a SINGLETON SHELL. On every reconcile there are exactly two
//! outcomes: the durably recorded terminal id still has a live tmux session, so
//! adopt it and refresh its control environment; or it does not, so create one
//! plain login shell in the orchestrator home and record its id. No agent is
//! started and no conversation is resumed - T-Hub guarantees that a shell exists
//! in the orchestrator home, and the user decides what runs in it.
//!
//! # Why there is no discovery or vetting here
//!
//! This replaces ~4,300 lines that discovered every runtime in Cortana's home and
//! cryptographically vetted whether each one was legitimately ours (a systemd
//! scope, a cgroup, a process-identity nonce, a harness launch attestation, and a
//! generation ladder with a quarantine plan for the losers). That machinery
//! existed to safely ADOPT runtimes T-Hub did not launch. It had been failing
//! every reconcile with `observe-managed-runtime-owner exit 91` ("systemd,
//! cgroup, process, nonce, and tmux ownership did not agree"), 3,194 consecutive
//! times on the reporting machine, and the durable record had reached generation
//! 16 with 15 revoked identities.
//!
//! It was removed rather than debugged, because T-Hub does not need to adopt
//! anything it did not launch: it can trust the terminal id it wrote down itself.
//!
//! # The trust posture this deliberately accepts
//!
//! Capability is NOT weakened. Cortana's control capability comes from its
//! identity secret and that identity's role, gated in `acl.rs` on
//! `AclRole::Cortana`. The attestation machinery never granted capability; it only
//! decided which discovered runtime was the authoritative Cortana.
//!
//! The trust posture actually NARROWS. Before, T-Hub would adopt a runtime it did
//! not launch if that runtime passed vetting. Now it adopts only the exact
//! terminal id it previously recorded, and only when tmux confirms that session is
//! alive. The residual exposure is that another process running as the same user
//! could occupy that specific tmux session and be adopted without a cryptographic
//! check. The tmux socket is already user-owned, so this is a same-user boundary,
//! and it is a smaller surface than discovery-plus-vetting was. That is a
//! deliberate decision, recorded at the adoption site below.

use super::*;

/// `reconcile_cortana` (trusted in-process app host only).
///
/// Idempotent by construction: the durable terminal id plus the single-flight
/// guard are the whole anti-duplication mechanism. Two reconciles in a row create
/// exactly one session, because the second one finds the id the first recorded.
pub(super) fn reconcile_cortana(
    ctx: &ControlContext,
    args: &Value,
    trusted_internal: bool,
) -> Result<Value, String> {
    ctx.tabs
        .require_authoritative_startup()
        .map_err(retryable_error)?;
    if !trusted_internal {
        return Err("acl: reconcile_cortana requires the trusted in-process app host".into());
    }
    let requested_operation_id = arg_str(args, "operationId")
        .or_else(|| arg_str(args, "requestId"))
        .filter(|value| !value.trim().is_empty())
        .ok_or("reconcile_cortana requires a stable non-empty operationId")?;
    let home = orchestrator_home(args)?;
    if home.is_empty() || !home.starts_with('/') {
        return Err("reconcile_cortana: orchestrator home must be an absolute POSIX path".into());
    }
    std::fs::create_dir_all(files::to_host_path(&home))
        .map_err(|error| format!("reconcile_cortana: could not create '{home}': {error}"))?;

    // ONE lock order for the whole operation: dispatch admission, then the
    // identity transaction, then provisioning. The previous implementation ran an
    // inspection pass under provisioning and re-entered while holding admission
    // only when it reached the spawn boundary; that dance existed to keep a 30s
    // poll off the admission lock, and it is not worth its complexity here. This
    // path holds admission for one tmux liveness probe in the common case.
    let _admission_lock = ctx
        .dispatch_admission
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _identity_transaction = ctx.tabs.identity_transaction();
    let _provision = ctx.captains.provision_guard();

    // Resume an in-flight operation rather than fighting it. A reconcile that died
    // mid-flight leaves `Recovering` behind, and `begin_cortana_recovery` refuses a
    // DIFFERENT operation id - so adopting the recorded id is what stops one
    // crashed attempt from wedging every later one.
    let operation_id = match &ctx.captains.cortana_identity().recovery {
        crate::cortana_reconcile::CortanaRecoveryState::Recovering { operation_id, .. } => {
            operation_id.clone()
        }
        _ => requested_operation_id,
    };
    let durable = ctx.captains.begin_cortana_recovery(&operation_id)?;
    let result = reconcile_shell(ctx, &operation_id, &durable, &home);
    if let Err(error) = &result {
        // A retryable error is a degraded control plane (an ambiguous tmux probe),
        // not a broken Cortana. Recording it as durably degraded would turn a
        // transient into a state the next reconcile has to climb back out of.
        if is_retryable_error(error) {
            return result;
        }
        let _ = ctx.captains.mark_cortana_degraded(&operation_id, error);
    }
    result
}

fn reconcile_shell(
    ctx: &ControlContext,
    operation_id: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    home: &str,
) -> Result<Value, String> {
    if let Some(terminal_id) = durable.terminal_id.clone() {
        match tmux::session_liveness(&tmux_target(&terminal_id)) {
            // ADOPTION SITE. The only evidence required is that the exact terminal
            // id this app recorded is alive. See the module note on the trust
            // posture: the vetting that used to happen here was removed on purpose.
            tmux::SessionLiveness::Alive => {
                return adopt_shell(ctx, operation_id, durable, &terminal_id)
            }
            // De-conflation (spawn-wedge): only a DEFINITIVE `Gone` means absent.
            // An `Unknown` probe is a degraded control plane, and treating it as
            // absent is exactly how a second shell would get created for a session
            // that is in fact alive.
            tmux::SessionLiveness::Unknown => {
                return Err(retryable_error(format!(
                    "reconcile_cortana: terminal '{terminal_id}' has uncertain tmux evidence"
                )))
            }
            tmux::SessionLiveness::Gone => {
                ctx.tabs.retire_tile_locked(&terminal_id);
                ctx.captains.remove_session(&terminal_id)?;
            }
        }
    }
    create_shell(ctx, operation_id, durable, home)
}

/// Reattach: bind the identity to the live tile, refresh the control environment,
/// and record the outcome. Creates nothing.
fn adopt_shell(
    ctx: &ControlContext,
    operation_id: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    terminal_id: &str,
) -> Result<Value, String> {
    let (identity, newly_minted) = resolve_identity(ctx, operation_id, durable)?;
    let adopt = (|| {
        ctx.identity.bind_tile(&identity.id, terminal_id)?;
        refresh_control_environment(ctx, terminal_id, &identity)?;
        place_in_captain_workspace(ctx, terminal_id);
        ctx.captains
            .commit_cortana_shell(operation_id, &identity.id, terminal_id)
    })();
    let committed = match adopt {
        Ok(committed) => committed,
        Err(error) => {
            retire_uncommitted_identity(ctx, &identity.id, newly_minted);
            return Err(error);
        }
    };
    let _ = captains_sync_apply(ctx);
    Ok(response(
        operation_id,
        crate::cortana_reconcile::CortanaReconcileAction::Adopt,
        committed,
        false,
    ))
}

/// Create the one shell. Reached only when the recorded terminal is definitively
/// gone, or when nothing was ever recorded.
fn create_shell(
    ctx: &ControlContext,
    operation_id: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    home: &str,
) -> Result<Value, String> {
    if ctx.apply_sink.is_none() && ctx.fanout.subscriber_count() == 0 {
        return Err("reconcile_cortana: no UI is connected to render a new shell".into());
    }
    // No provider lane is requested: this launches a login shell, not a harness.
    let _capacity = evaluate_spawn_capacity(ctx, &SpawnPurpose::Cortana, 0, None)
        .map_err(|refusal| format!("reconcile_cortana: {}", refusal.message))?;
    let tmux_cwd = files::posix_form(home);
    let worktree_admission = ctx.admit_worktree_activity(&tmux_cwd, "reconcile_cortana")?;
    let (identity, newly_minted) = resolve_identity(ctx, operation_id, durable)?;

    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let spawn_args = json!({
        "cwd": home,
        "name": "Cortana",
        "tabId": CAPTAIN_WORKSPACE_ID,
    });
    let mut elevation = elevation_env(ctx, &spawn_args);
    audit_control_spawn(ctx, "reconcile_cortana", &spawn_args);
    elevation.push((
        crate::identity::SESSION_TOKEN_ENV.to_string(),
        identity.secret.clone(),
    ));
    // `command: None` is the whole point of the singleton: tmux launches the
    // user's login shell. Whatever the user then runs in it inherits the Cortana
    // identity from the session environment above.
    let (pane, elevation) = match worktree_admission.contain_process(None, elevation) {
        Ok(contained) => contained,
        Err(error) => {
            retire_uncommitted_identity(ctx, &identity.id, newly_minted);
            return Err(error);
        }
    };
    let (_, tmux_session) =
        match spawn_tmux_terminal_with_id(&terminal_id, &tmux_cwd, pane.as_deref(), &elevation) {
            Ok(spawned) => spawned,
            Err(error) => {
                retire_uncommitted_identity(ctx, &identity.id, newly_minted);
                return Err(format!("reconcile_cortana: {error}"));
            }
        };

    let register = (|| {
        ctx.identity.bind_tile(&identity.id, &terminal_id)?;
        ctx.tabs
            .insert_tab(CAPTAIN_WORKSPACE_ID, CAPTAIN_WORKSPACE_NAME);
        ctx.tabs
            .place_tile_exact(&terminal_id, CAPTAIN_WORKSPACE_ID)?;
        ctx.captains
            .commit_cortana_shell(operation_id, &identity.id, &terminal_id)
    })();
    let committed = match register {
        Ok(committed) => committed,
        Err(error) => {
            // Roll the external effect back so a failed create cannot leave a live
            // shell with no tile and no durable record - the leak that used to add
            // one orphaned session (and one systemd scope) per failed attempt.
            let _ = tmux::kill_session_tree(&tmux_session);
            ctx.tabs.retire_tile_locked(&terminal_id);
            retire_uncommitted_identity(ctx, &identity.id, newly_minted);
            return Err(error);
        }
    };
    let _ = captains_sync_apply(ctx);
    // Tell the UI to MATERIALIZE the tile, not merely to move it.
    //
    // The webview seeds its terminal map from `list_terminals` exactly ONCE, at
    // Canvas mount; the 15s poll after that calls `updateTerminalsMeta`, which
    // refreshes cwd/title/state for terminals it already knows and never adds new
    // ones. The singleton is created a moment AFTER that mount, so a `move_tile`
    // for a tile the UI has no terminal record for is a no-op and Cortana stays
    // invisible until the next reload. Forward the same shape the crew spawn path
    // forwards, which is what makes the tile appear immediately.
    let forward = with_sync(
        ctx,
        json!({
            "id": terminal_id,
            "tmuxSession": tmux_session,
            "cwd": home,
            "name": "Cortana",
            "tabId": CAPTAIN_WORKSPACE_ID,
        }),
    );
    let _ = forward_apply(ctx, "spawn_terminal", &forward);
    Ok(response(
        operation_id,
        crate::cortana_reconcile::CortanaReconcileAction::Create,
        committed,
        true,
    ))
}

/// Resolve the Cortana-role identity for this reconcile, self-healing a durable
/// pointer that no longer resolves.
///
/// The self-heal (PR #89) is load-bearing, not cosmetic. The load-time identity GC
/// (`prune_dead_generation`) retires every identity whose session tile is gone -
/// the routine outcome of a restart after Cortana's tmux session died - while
/// `captains.json` keeps referencing the pruned id. Erroring on that made the
/// state PERMANENT, because nothing else rewrites `cortana.identity_id`.
///
/// A REVOKED id still fails closed: revocation is a deliberate "this credential is
/// burned" act with a durable tombstone, and re-minting past it would defeat that.
fn resolve_identity(
    ctx: &ControlContext,
    operation_id: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<(crate::identity::SessionIdentity, bool), String> {
    let Some(identity_id) = durable.identity_id.as_deref() else {
        return Ok((ctx.identity.mint(crate::identity::Role::Cortana)?, true));
    };
    match ctx.identity.get(identity_id) {
        Some(identity) => {
            if identity.role != crate::identity::Role::Cortana {
                return Err(
                    "reconcile_cortana: durable identity no longer has the Cortana role".into(),
                );
            }
            Ok((identity, false))
        }
        None if ctx.identity.is_revoked(identity_id) => Err(format!(
            "reconcile_cortana: durable identity '{identity_id}' is revoked"
        )),
        None => {
            let identity = ctx.identity.mint(crate::identity::Role::Cortana)?;
            if let Err(error) =
                ctx.captains
                    .rebind_pruned_cortana_identity(operation_id, identity_id, &identity.id)
            {
                let _ = ctx.identity.retire(&identity.id);
                return Err(error);
            }
            Ok((identity, true))
        }
    }
}

/// Re-inject the control environment into a session that survived an app restart.
///
/// This is the fix for the long-standing "Cortana came back read-only" problem.
/// The control endpoint and the session token rotate on every app start, while a
/// surviving tmux session keeps whatever environment it was created with - so an
/// agent started in a reattached shell presented a stale (or retired) identity and
/// fell back to the read-only control token. tmux session environment is inherited
/// by processes started AFTER it is set, which is exactly the case that matters:
/// the user has not started the agent yet.
fn refresh_control_environment(
    ctx: &ControlContext,
    terminal_id: &str,
    identity: &crate::identity::SessionIdentity,
) -> Result<(), String> {
    let mut refresh = elevation_env(ctx, &Value::Null);
    if refresh.is_empty() {
        return Ok(());
    }
    refresh.push((
        crate::identity::SESSION_TOKEN_ENV.to_string(),
        identity.secret.clone(),
    ));
    // ONE tmux invocation for the whole set: on Windows each call is a `wsl.exe`
    // spawn, and this runs on every reconcile.
    tmux::set_session_environment_many(&tmux_target(terminal_id), &refresh).map_err(|error| {
        format!("reconcile_cortana: could not refresh the adopted control environment: {error}")
    })
}

/// Put the tile in the Captain Workspace when it is not placed anywhere.
///
/// Deliberately NOT unconditional: this runs every reconcile, and forcing the
/// placement would drag the tile back every 30 seconds if the user moved it.
fn place_in_captain_workspace(ctx: &ControlContext, terminal_id: &str) {
    if ctx.tabs.workspace_for_tile(terminal_id).is_some() {
        return;
    }
    ctx.tabs
        .insert_tab(CAPTAIN_WORKSPACE_ID, CAPTAIN_WORKSPACE_NAME);
    let _ = ctx.tabs.place_tile_exact(terminal_id, CAPTAIN_WORKSPACE_ID);
}

/// Drop an identity minted for an attempt that did not commit, so a failed
/// reconcile cannot leave a secret-bearing identity behind for a session that
/// never became authoritative.
fn retire_uncommitted_identity(ctx: &ControlContext, identity_id: &str, newly_minted: bool) {
    if !newly_minted {
        return;
    }
    // A re-mint from the pruned-identity self-heal is already durable in
    // `cortana.identity_id`; retiring it would recreate the wedge it just fixed.
    if ctx.captains.cortana_identity().identity_id.as_deref() == Some(identity_id) {
        return;
    }
    let _ = ctx.identity.retire(identity_id);
}

fn response(
    operation_id: &str,
    action: crate::cortana_reconcile::CortanaReconcileAction,
    durable: crate::cortana_reconcile::CortanaDurableIdentity,
    audited: bool,
) -> Value {
    json!({
        "accepted": "reconcile_cortana",
        "operationId": operation_id,
        "action": action,
        "healthy": true,
        "terminalId": durable.terminal_id,
        "identityId": durable.identity_id,
        "harness": durable.harness,
        "providerSessionId": durable.provider_session_id,
        "conversationId": durable.conversation_id,
        "recovery": durable.recovery,
        "degradedReason": Value::Null,
        "audited": audited,
    })
}
