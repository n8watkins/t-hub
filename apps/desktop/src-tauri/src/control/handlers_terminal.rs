//! Direct terminal I/O + close-terminal lifecycle control handlers, split out of
//! `control.rs` to shrink that module. The demoted break-glass writers
//! (`mark_break_glass`, `writer_liveness_gate`, `send_text`, `send_keys`) and the
//! full `close_terminal` path - close planning (`ClosePlan`), delegated-cleanup
//! authorization / revalidation, crew-binding reconciliation, and
//! `close_terminal` / `close_terminal_with_policy`. The parent dispatch routes here.

use super::*;

/// comms-plane Phase 1: mark a BREAK-GLASS use of the demoted direct writers
/// (`send_text`/`send_keys`) LOUDLY. These are no longer the primary path - the
/// fleet wake and the in-app automation writers funnel through `plane` (path a/b) -
/// but they are DEMOTED, not DENIED (design H2): they still execute, so a human or
/// external script keeps its escape hatch. Every use emits a `t-hub-plane:`
/// break-glass log line AND a live `control://break-glass` fanout event so the
/// deviation is visible and cannot quietly become the primary path again (D11a).
///
/// HONEST LIMIT (Phase 1): break-glass rides the SHARED control token, so it is
/// attributed only as "some Full caller" (the command that deviated), not the
/// per-session identity - and it stays callable by every crew session until item 3
/// tiers the token away. This marker makes the deviation observable; it does not
/// yet make it impossible.
pub(super) fn mark_break_glass(ctx: &ControlContext, command: &str, args: &Value) {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .unwrap_or_default();
    let target = tmux_target(&session_id);
    // Payload size (length only, never content). `send_text` carries `text`;
    // `send_keys` carries its payload in the `keys` array, so fall back to the
    // joined key names - otherwise every `send_keys` marker would report bytes=0.
    let bytes = if let Some(text) = arg_str(args, "text") {
        text.len()
    } else {
        args.get("keys")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .len()
            })
            .unwrap_or(0)
    };
    plane::note_break_glass(command, &target, bytes);
    ctx.fanout.emit_event(
        "control://break-glass",
        &json!({
            "command": command,
            "sessionId": session_id,
            "target": target,
            "bytes": bytes,
            "breakGlass": true,
            "note": "demoted direct writer used; NOT the plane primary path (Phase 1)",
        }),
    );
}

/// `send_text`: type literal `text` into an existing session, optionally pressing
/// Enter to submit it. Process-changing (PRD §11.2): the MCP tool description
/// marks it CONFIRMATION REQUIRED. Backend-only — drives tmux directly
/// (`send-keys -l`), no UI round trip. Args: `sessionId` + `text` (required),
/// `enter` (optional, default true). Requires the session to exist.
///
/// comms-plane Phase 1: DEMOTED to audited break-glass (see `mark_break_glass`).
/// It is no longer the fleet path; the wake injects via `plane::deliver_tmux`.
/// Liveness gate for the direct-writer break-glass commands (`send_text` /
/// `send_keys`): map a three-state probe to proceed / a DEFINITIVE "no such
/// session" / a RETRYABLE probe-timeout.
///
/// The `Unknown` arm is the spawn-wedge fix (de-conflation): a timed-out probe
/// must NEVER be reported as "no such session". That false negative is exactly
/// what made the app say sessions e05764f5/3647011c/68501753 were gone while tmux
/// held them alive, sending the fleet to raw-tmux break-glass on 0.3.62. The
/// caller is told the CONTROL PLANE is degraded (retry), not that its session died.
pub(super) fn writer_liveness_gate(
    command: &str,
    session_id: &str,
    target: &str,
    liveness: tmux::SessionLiveness,
) -> Result<(), String> {
    match liveness {
        tmux::SessionLiveness::Alive => Ok(()),
        tmux::SessionLiveness::Gone => Err(format!(
            "{command}: no such session '{session_id}' (target {target})"
        )),
        tmux::SessionLiveness::Unknown => Err(retryable_error(format!(
            "{command}: liveness probe for '{session_id}' (target {target}) timed out; \
             session NOT confirmed gone — retry (the control plane is degraded, not the session)"
        ))),
    }
}

pub(super) fn send_text(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("send_text requires a 'sessionId' argument")?;
    let text = arg_str(args, "text").ok_or("send_text requires a 'text' argument")?;
    let enter = args.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);
    let target = tmux_target(&session_id);
    writer_liveness_gate(
        "send_text",
        &session_id,
        &target,
        tmux::session_liveness(&target),
    )?;
    tmux::send_text(&target, &text, enter)
        .map_err(|e| format!("failed to send text to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "send_text",
        "sessionId": session_id,
        "target": target,
        "enter": enter,
        "audited": true,
    }))
}

/// `send_keys`: send one or more named control keys (e.g. `C-c`, `Up`, `Escape`)
/// to an existing session. Process-changing (confirmation-required). Backend-only
/// (`send-keys` with key-name interpretation). Args: `sessionId` (required) +
/// `keys` (required, a non-empty array of tmux key names).
pub(super) fn send_keys(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("send_keys requires a 'sessionId' argument")?;
    let keys: Vec<String> = args
        .get("keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|k| k.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if keys.is_empty() {
        return Err("send_keys requires a non-empty 'keys' array of tmux key names".into());
    }
    let target = tmux_target(&session_id);
    writer_liveness_gate(
        "send_keys",
        &session_id,
        &target,
        tmux::session_liveness(&target),
    )?;
    let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    tmux::send_keys(&target, &key_refs)
        .map_err(|e| format!("failed to send keys to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "send_keys",
        "sessionId": session_id,
        "target": target,
        "keys": keys,
        "audited": true,
    }))
}

/// The action [`close_terminal`] takes for a given liveness probe + `force` flag.
pub(super) enum ClosePlan {
    /// Reap the session (bounded, idempotent tree-kill). `existed` labels the
    /// outcome (`killed` vs `already_gone`); `forced` marks a force-escape reap of a
    /// session whose liveness stayed indeterminate.
    Kill { existed: bool, forced: bool },
    /// The probe timed out and `force` was not set: refuse with a RETRYABLE error
    /// (the de-conflation default - a wedge is not a death).
    RetryableTimeout,
    /// `force` was set but a fresh RE-PROBE CONFIRMED the session ALIVE: refuse.
    /// `force` never kills a session a re-probe reports `Alive` - that first
    /// `Unknown` was merely slowness. (This is the ONLY liveness state that refuses
    /// force; a re-probe that ALSO times out is `Unknown`, not `Alive`, and is
    /// force-reaped - see `plan_close`.)
    RefuseForceOnLive,
}

/// Decide [`close_terminal`]'s action with NO side effects (unit-testable). MED-1
/// (PR-58 review): `close_terminal` refusing every `Unknown` left a genuinely-dead-
/// but-unprobeable session unreapable under a wedge. `force:true` adds an escape:
///
/// - `Alive`            → `Kill{existed:true}`   (normal close of a live tile)
/// - `Gone`             → `Kill{existed:false}`  (idempotent already-gone)
/// - `Unknown`, `!force`→ `RetryableTimeout`     (retry once the plane recovers)
/// - `Unknown`, `force` → decided by a fresh `reprobe`:
///     * `reprobe==Alive`   → `RefuseForceOnLive` (re-probe CONFIRMS live: refuse)
///     * `reprobe==Gone`    → `Kill{existed:false}` (confirmed dead: clean reap)
///     * `reprobe==Unknown` → `Kill{forced:true}`   (still unreachable: forced reap)
///
/// The honest guarantee (NOT "never kills a live session"): force never kills a
/// session a fresh re-probe CONFIRMS `Alive`. Under a SUSTAINED wedge a live-but-
/// slow session's re-probe also returns `Unknown` - indistinguishable from dead -
/// and force WILL reap it. That is what `force:true` means: reap-during-wedge, at
/// the caller's risk. `reprobe` is `Some` ONLY when `force && initial==Unknown`;
/// otherwise `None`.
pub(super) fn plan_close(
    force: bool,
    initial: tmux::SessionLiveness,
    reprobe: Option<tmux::SessionLiveness>,
) -> ClosePlan {
    use tmux::SessionLiveness::*;
    match initial {
        Alive => ClosePlan::Kill {
            existed: true,
            forced: false,
        },
        Gone => ClosePlan::Kill {
            existed: false,
            forced: false,
        },
        Unknown if !force => ClosePlan::RetryableTimeout,
        Unknown => match reprobe {
            Some(Alive) => ClosePlan::RefuseForceOnLive,
            Some(Gone) => ClosePlan::Kill {
                existed: false,
                forced: false,
            },
            // Still indeterminate (or, defensively, no reprobe supplied): the
            // operator asserted death and a fresh probe could not contradict it -
            // perform the bounded forced reap. `kill_session_tree` is idempotent, so
            // if the session was in fact already gone this is a clean no-op.
            _ => ClosePlan::Kill {
                existed: false,
                forced: true,
            },
        },
    }
}

/// `close_terminal`: kill an existing session and its process tree. Process-
/// changing/destructive (confirmation-required). Backend-only via tmux
/// Managed Cortana runtimes close through their durable cgroup owner token.
/// Legacy terminals retain the older best-effort enumerated-pidfd path for
/// explicit manual cleanup; it does not claim complete descendant coverage.
/// Idempotent (already-gone ⇒ success).
///
/// Headless-org: the dead tile is also dropped from the server tab registry and
/// a `sync_tabs` snapshot is forwarded, so the tile leaves its tab cleanly even
/// when that tab is hidden or the window is minimized (previously removal relied
/// on the UI's ~5s live-terminal reconcile). Args: `sessionId` (required),
/// `force` (optional bool).
///
/// **`force` (MED-1 operator escape).** By default an `Unknown` liveness probe (a
/// timed-out/failed probe under a degraded spawn path) is REFUSED as retryable -
/// we never run a destructive tree-kill we cannot verify. But that leaves a
/// genuinely-dead-but-unprobeable session unreapable during a wedge. `force:true`
/// re-probes once and, if the session is STILL not confirmably alive, performs the
/// bounded reap anyway (outcome `force_reaped`).
///
/// The guarantee is NARROW and honest: force never kills a session a fresh re-probe
/// reports `Alive`. It is NOT "never kills a live session" - under a SUSTAINED wedge
/// a live-but-slow session's re-probe also times out (`Unknown`), which is
/// probe-indistinguishable from dead, and force WILL reap it. `force:true` therefore
/// means "reap this tile even if it turns out to be live-but-unreachable - I accept
/// that risk," so use it ONLY when you have independent reason to believe the session
/// is DEAD (its work finished, its process is gone) and a wedge is merely blocking
/// the reap. **⚠ Do NOT reach for `force` to work around slowness on a session you
/// think may still be live** - retry a normal `close_terminal` (no force) first; a
/// normal close reaps a confirmed-`Alive` session cleanly and refuses on `Unknown`.
/// If a forced close is refused, the re-probe confirmed the tile LIVE - investigate,
/// do not re-force.
pub(super) fn close_terminal_authorized(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "close_terminal")?;
    let target = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("close_terminal requires a 'sessionId' argument")?;
    // Authorize the target from ownership-only registry state before checking
    // historical Removed Crew state, which can lead to Project and Powder scope
    // resolution. Foreign callers must not use close as a binding-state oracle.
    let delegated_caller = caller.filter(|caller| {
        ctx.delegated_admin
            .grants_for_actor(&caller.session_id)
            .iter()
            .any(|grant| grant.state.is_active())
    });
    let (captain_authority, delegated_authority) = if let Some(caller) = delegated_caller {
        (
            None,
            Some(authorize_delegated_cleanup(ctx, args, caller, &target)?),
        )
    } else {
        (
            enforce_target_lifecycle_authority(ctx, caller, trusted_internal, &target)?,
            None,
        )
    };
    let delegated_audit = delegated_authority
        .as_ref()
        .map(|authority| authority.audit.clone());
    let target_identity_id = ctx.identity.for_tile(&target).map(|identity| identity.id);
    let removed_crew = match ctx.captains.removed_crew_powder_ship(&target) {
        Ok(removed_crew) => removed_crew,
        Err(error) => {
            let result = Err(error);
            record_delegated_admin_outcome(ctx, delegated_audit.as_ref(), &result);
            return result;
        }
    };
    if removed_crew.is_some() {
        let result =
            reconcile_removed_crew_powder_binding(ctx, caller, &target).and_then(|value| {
                invalidate_retired_admin_identity(ctx, target_identity_id.as_deref(), &target)?;
                Ok(value)
            });
        record_delegated_admin_outcome(ctx, delegated_audit.as_ref(), &result);
        return result;
    }
    let authority = captain_authority
        .map(CloseLifecycleAuthority::Captain)
        .or_else(|| {
            delegated_authority
                .map(|authority| CloseLifecycleAuthority::Delegated(Box::new(authority)))
        });
    let result =
        close_terminal_with_policy(ctx, args, false, authority.as_ref()).and_then(|value| {
            invalidate_retired_admin_identity(ctx, target_identity_id.as_deref(), &target)?;
            Ok(value)
        });
    record_delegated_admin_outcome(ctx, delegated_audit.as_ref(), &result);
    result
}

pub(super) fn authorize_delegated_cleanup(
    ctx: &ControlContext,
    args: &Value,
    caller: &ResolvedIdentity,
    target: &str,
) -> Result<DelegatedCleanupAuthority, String> {
    let approval_id = arg_str(args, "approvalId")
        .filter(|approval_id| !approval_id.trim().is_empty())
        .ok_or("delegated admin: close_terminal requires an exact supervisor approvalId")?;
    let admin_target = delegated_admin_target_for_terminal(ctx, target)?;
    let grant = ctx
        .delegated_admin
        .grants_for_actor(&caller.session_id)
        .into_iter()
        .find(|grant| grant.state.is_active())
        .ok_or("delegated admin: caller has no active administrative grant")?;
    let supervisor = current_delegating_supervisor(ctx, &grant);
    let actor = current_admin_actor(ctx, &grant);
    let consumed_approval = ctx
        .delegated_admin
        .consume_exact_approval(
            &approval_id,
            &crate::delegated_admin::AdminActor {
                identity_id: caller.session_id.clone(),
                session_tile: caller.tile.clone(),
                ..actor
            },
            &supervisor,
            crate::delegated_admin::AdminOperation::CleanupSession,
            &admin_target,
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    let audit = authorize_delegated_admin(
        ctx,
        caller,
        crate::delegated_admin::AdminOperation::CleanupSession,
        admin_target,
        crate::delegated_admin::AdminSafeguards {
            authoritative_ownership_verified: true,
            consumed_approval: Some(consumed_approval.clone()),
            worktree_safety: None,
        },
    )?;
    Ok(DelegatedCleanupAuthority {
        audit,
        consumed_approval,
    })
}

pub(super) fn invalidate_retired_admin_identity(
    ctx: &ControlContext,
    identity_id: Option<&str>,
    terminal_id: &str,
) -> Result<(), String> {
    let Some(identity_id) = identity_id else {
        return Ok(());
    };
    ctx.delegated_admin
        .invalidate_actor(
            identity_id,
            format!("administrative actor terminal '{terminal_id}' was retired"),
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    ctx.delegated_admin
        .invalidate_delegator(
            identity_id,
            format!("delegating supervisor terminal '{terminal_id}' was retired"),
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct DelegatedCleanupAuthority {
    audit: crate::delegated_admin::AdminAuditContext,
    consumed_approval: crate::delegated_admin::ConsumedExactApproval,
}

#[derive(Debug, Clone)]
pub(super) enum CloseLifecycleAuthority {
    Captain(TargetLifecycleAuthority),
    Delegated(Box<DelegatedCleanupAuthority>),
}

pub(super) fn revalidate_close_lifecycle_authority(
    ctx: &ControlContext,
    authority: &CloseLifecycleAuthority,
) -> Result<(), String> {
    match authority {
        CloseLifecycleAuthority::Captain(authority) => {
            revalidate_target_lifecycle_authority(ctx, authority)
        }
        CloseLifecycleAuthority::Delegated(authority) => {
            let current_target = match &authority.audit.target {
                crate::delegated_admin::AdminTarget::CrewSession { session_id, .. } => {
                    delegated_admin_target_for_terminal(ctx, session_id)?
                }
                crate::delegated_admin::AdminTarget::Captain {
                    captain_identity_id,
                    ..
                } => {
                    let terminal_id = ctx
                        .identity
                        .get(captain_identity_id)
                        .and_then(|identity| identity.session_tile)
                        .ok_or("delegated admin: Captain target lost its authoritative terminal")?;
                    delegated_admin_target_for_terminal(ctx, &terminal_id)?
                }
                _ => {
                    return Err(
                        "delegated admin: cleanup authority has an invalid session target".into(),
                    );
                }
            };
            if current_target != authority.audit.target {
                return Err(
                    "delegated admin: exact target ownership changed before terminal teardown"
                        .into(),
                );
            }
            let grant = ctx
                .delegated_admin
                .get(&authority.audit.grant_id)
                .filter(|grant| {
                    grant.state.is_active()
                        && grant.grant_generation == authority.audit.grant_generation
                        && grant.actor_identity_id == authority.audit.actor_identity_id
                })
                .ok_or("delegated admin: cleanup authority changed before terminal teardown")?;
            let supervisor = current_delegating_supervisor(ctx, &grant);
            let actor = current_admin_actor(ctx, &grant);
            ctx.delegated_admin
                .authorize(
                    &crate::delegated_admin::AdminActor {
                        identity_id: authority.audit.actor_identity_id.clone(),
                        session_tile: authority.audit.actor_session_tile.clone(),
                        ..actor
                    },
                    &supervisor,
                    crate::delegated_admin::AdminOperation::CleanupSession,
                    &authority.audit.target,
                    &crate::delegated_admin::AdminSafeguards {
                        authoritative_ownership_verified: true,
                        consumed_approval: Some(authority.consumed_approval.clone()),
                        worktree_safety: None,
                    },
                )
                .map(|_| ())
                .map_err(|error| format!("{}: {error}", error.code()))
        }
    }
}

pub(super) fn reconcile_removed_crew_powder_binding(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    crew_session_id: &str,
) -> Result<Value, String> {
    // Removed Crew records are historical tombstones. Their old Powder binding
    // remains deserializable and addressable for compatibility, but close must
    // never turn a tombstone lookup into a live Powder read or mutation.
    let _ = (ctx, caller);
    Ok(json!({
        "accepted": "close_terminal",
        "sessionId": crew_session_id,
        "target": tmux_target(crew_session_id),
        "outcome": "already_gone",
        "powderRelease": {
            "released": false,
            "outcome": "retired",
        },
        "crewBindingRetained": true,
        "audited": true,
    }))
}

pub(super) fn close_terminal(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    close_terminal_with_policy(ctx, args, false, None)
}

#[allow(dead_code)]
pub(super) fn retain_crew_binding_after_powder_failure(
    preserve_on_failure: bool,
    cleanup_was_pending: bool,
    completion_recovery_required: bool,
    has_powder_binding: bool,
    powder_release: Option<&Value>,
) -> bool {
    (preserve_on_failure
        || cleanup_was_pending
        || completion_recovery_required
        || has_powder_binding)
        && powder_release
            .is_some_and(|release| release.get("released").and_then(Value::as_bool) != Some(true))
}

pub(super) fn freeze_close_terminal_powder_release(
    ctx: &ControlContext,
    crew_session_id: &str,
) -> Result<(u64, Option<PendingDispatchRelease>), String> {
    let snapshot = ctx.captains.snapshot();
    let matching_crew = snapshot
        .captains
        .iter()
        .flat_map(|captain| captain.crew.iter())
        .filter(|crew| crew.terminal_id == crew_session_id)
        .collect::<Vec<_>>();
    let crew = match matching_crew.as_slice() {
        [] => return Ok((snapshot.seq, None)),
        [crew] => *crew,
        _ => {
            return Err(format!(
                "Crew session '{crew_session_id}' is ambiguously assigned to multiple Captains"
            ));
        }
    };
    let _legacy_work = crew.powder_work.as_ref();
    Ok((snapshot.seq, None))
}

pub(super) fn close_terminal_with_policy(
    ctx: &ControlContext,
    args: &Value,
    _preserve_crew_on_powder_failure: bool,
    authority: Option<&CloseLifecycleAuthority>,
) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("close_terminal requires a 'sessionId' argument")?;
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let target = tmux_target(&session_id);
    let tile_id = session_id.strip_prefix("th_").unwrap_or(&session_id);
    // Serialize the entire terminal-close transaction with completion,
    // renewal, and cleanup for this Crew. The guard must precede liveness and
    // tmux teardown so a close queued behind completion cannot kill the worker
    // before the authoritative Powder transition converges.
    let _operation_guard = ctx
        .captains
        .serialize_crew_powder_operation(tile_id, CrewPowderOperationKind::Cleanup);
    if let Some(authority) = authority {
        revalidate_close_lifecycle_authority(ctx, authority)?;
    }
    // Probe and choose the requested effect before preparing durable state.
    // A refused close therefore leaves no cleanup intent for startup recovery
    // to misinterpret as authority to force-reap the terminal.
    let initial = tmux::session_liveness(&target);
    let reprobe = if force && matches!(initial, tmux::SessionLiveness::Unknown) {
        Some(tmux::session_liveness(&target))
    } else {
        None
    };
    let (existed, forced) = match plan_close(force, initial, reprobe) {
        ClosePlan::Kill { existed, forced } => (existed, forced),
        ClosePlan::RetryableTimeout => {
            return Err(retryable_error(format!(
                "close_terminal: liveness probe for '{session_id}' (target {target}) timed out; \
                 session NOT confirmed gone — retry once the control plane recovers, or pass \
                 force:true to reap a session you know is dead (refusing an unverifiable tree-kill)"
            )));
        }
        ClosePlan::RefuseForceOnLive => {
            return Err(format!(
                "close_terminal: force refused — a re-probe shows session '{session_id}' (target \
                 {target}) is LIVE; the earlier probe was merely slow, not dead. Retry a normal \
                 close_terminal (no force) to reap it."
            ));
        }
    };
    let fleet_operation = match ctx.captains.pending_close_terminal_operation(tile_id) {
        Some(operation) => operation,
        None => {
            let (expected_seq, powder_release) =
                freeze_close_terminal_powder_release(ctx, tile_id)?;
            #[cfg(test)]
            ctx.captains.pause_dispatch("close_terminal_scope_frozen");
            ctx.captains
                .prepare_close_terminal_operation(tile_id, expected_seq, powder_release)?
        }
    };
    #[cfg(test)]
    ctx.captains.pause_dispatch("close_terminal_effect");
    // Registry-vs-reality (Incident C, ask #3): `kill_session_tree` is idempotent -
    // it returns Ok for an already-gone session too - so a caller could never tell
    // a real kill from a phantom close (ghost ids f0f3207b / 709c7252). Probe
    // liveness BEFORE the kill so we can report an HONEST outcome. We check first
    // (not the kill's own status) because the tree sweep SIGKILLs the pane pids,
    // which can auto-destroy the session before `kill-session` runs, making a real
    // kill look already-gone. The kill stays idempotent; only the label is refined.
    //
    // De-conflation (spawn-wedge) + MED-1 force escape: an `Unknown` probe means the
    // control plane is degraded, not that the session is gone, so the DEFAULT refuses
    // (retryable). `force:true` re-probes ONCE and reaps unless that re-probe CONFIRMS
    // `Alive` (only a definitive Alive refuses force). Under a sustained wedge a live-
    // but-slow session's re-probe is also `Unknown` - indistinguishable from dead - so
    // force reaps it too; force is a deliberate reap-during-wedge override, not a
    // never-touch-a-live-session guarantee. See `plan_close`.
    let durable_cortana = ctx.captains.cortana_identity();
    let teardown = if durable_cortana.terminal_id.as_deref() == Some(tile_id) {
        durable_cortana
            .owner
            .as_ref()
            .ok_or_else(|| {
                "close_terminal: managed Cortana has no durable owner token and was preserved"
                    .to_string()
            })
            .and_then(|owner| {
                tmux::retire_managed_runtime(&target, &tmux_cortana_owner(owner))
                    .map_err(String::from)
            })
    } else {
        tmux::kill_session_tree(&target).map_err(String::from)
    };
    if let Err(error) = teardown {
        return Err(retryable_error(format!(
            "failed to close terminal '{session_id}': {error}; durable close recovery remains prepared"
        )));
    }
    // The terminal is now definitely stopped. Powder claims and release
    // recoveries are retired compatibility data, so close only commits the
    // local terminal/tombstone transition and never contacts Powder.
    let committed = Some(ctx.captains.commit_close_terminal_operation(
        &fleet_operation,
        false,
        None,
    )?);
    let outcome = if forced {
        "force_reaped"
    } else if existed {
        "killed"
    } else {
        "already_gone"
    };
    // The registry keys tiles by the bare terminal id; strip an already-prefixed
    // caller the same way tmux_target normalizes the other direction.
    if let Some(committed) = committed {
        ctx.tabs.replace(ctx.captains.workspace_projection());
        if committed.workspace_changed || ctx.tabs.retire_tile_locked(tile_id) {
            let _ = forward_apply(ctx, "sync_tabs", &with_sync(ctx, json!({})));
        }
        // Captain-chat phase 2: a dead session leaves the captains registry too -
        // its captaincy is released and it drops out of every crew list.
        if committed.captain_state_changed {
            let _ = captains_sync_apply(ctx);
        }
        // Comms-plane Phase 2 (review M3): a dead session's per-session identity is
        // retired too, so its secret stops resolving and the identity store does not
        // accrete dead sessions (it is bounded to live + not-yet-closed sessions).
        ctx.identity.retire_tile(tile_id)?;
    }
    // The provider transcript remains intact and is now resumable. Force the next
    // History read to observe any final transcript write immediately.
    notify_history_changed(ctx, "terminal-closed");
    Ok(json!({
        "accepted": "close_terminal",
        "sessionId": session_id,
        "target": target,
        // killed = a live session was reaped; already_gone = nothing was there to
        // kill (idempotent no-op); force_reaped = an operator force:true reaped a
        // session whose liveness stayed indeterminate. ok:true in every case, so a
        // retry stays safe.
        "outcome": outcome,
        "powderRelease": {
            "released": false,
            "outcome": "retired",
        },
        "crewBindingRetained": false,
        "audited": true,
    }))
}

// --- Terminal read (read_terminal) ---
pub(super) fn read_terminal(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("read_terminal requires a 'sessionId' argument")?;
    // Phase 3 (§2.6 H3): the cross-ship READ hole - `read_terminal` captures another
    // session's scrollback directly via tmux, bypassing the plane entirely. An
    // identified session may read a pane ONLY on its own ship; the proven host is unrestricted.
    let caller_has_active_admin_grant = caller.is_some_and(|caller| {
        ctx.delegated_admin
            .grants_for_actor(&caller.session_id)
            .iter()
            .any(|grant| grant.state.is_active())
    });
    let delegated_audit = if caller_has_active_admin_grant {
        let caller = caller.expect("an active delegated grant requires an identified caller");
        let target = delegated_admin_target_for_terminal(ctx, &session_id)?;
        Some(authorize_delegated_admin(
            ctx,
            caller,
            crate::delegated_admin::AdminOperation::InspectStatus,
            target,
            crate::delegated_admin::AdminSafeguards::default(),
        )?)
    } else {
        enforce_session_access(ctx, caller, trusted_internal, &session_id)?;
        None
    };
    let result = (|| {
        let target = tmux_target(&session_id);
        let history = args
            .get("historyLines")
            .or_else(|| args.get("history_lines"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .min(10_000) as u32;
        let text = tmux::capture_pane_text(&target, history)
            .map_err(|e| format!("failed to capture pane for '{session_id}': {e}"))?;
        Ok(json!({
            "sessionId": session_id,
            "target": target,
            "historyLines": history,
            "text": text,
        }))
    })();
    record_delegated_admin_outcome(ctx, delegated_audit.as_ref(), &result);
    result
}

pub(super) fn delegated_admin_target_for_terminal(
    ctx: &ControlContext,
    terminal_id: &str,
) -> Result<crate::delegated_admin::AdminTarget, String> {
    let terminal_id = terminal_id.strip_prefix("th_").unwrap_or(terminal_id);
    let snapshot = ctx.captains.snapshot();
    if let Some(captain) = snapshot
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some(terminal_id))
    {
        let captain_identity_id = ctx
            .identity
            .for_tile(terminal_id)
            .map(|identity| identity.id)
            .unwrap_or_else(|| captain.assignment_id.clone());
        return Ok(crate::delegated_admin::AdminTarget::Captain {
            ship_slug: captain.ship_slug.clone(),
            captain_identity_id,
        });
    }
    let owners = snapshot
        .captains
        .iter()
        .filter(|captain| {
            captain
                .crew
                .iter()
                .any(|crew| crew.terminal_id == terminal_id)
        })
        .collect::<Vec<_>>();
    match owners.as_slice() {
        [owner] => Ok(crate::delegated_admin::AdminTarget::CrewSession {
            ship_slug: owner.ship_slug.clone(),
            session_id: terminal_id.to_string(),
        }),
        [] => Err(format!(
            "delegated admin: terminal '{terminal_id}' has no authoritative Fleet owner"
        )),
        _ => Err(format!(
            "delegated admin: terminal '{terminal_id}' has ambiguous Fleet ownership"
        )),
    }
}
