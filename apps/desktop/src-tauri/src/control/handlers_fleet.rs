//! Fleet-watch (orchestrator-wake) control handlers, split out of `control.rs`
//! to shrink that module. `watch_fleet` / `unwatch_fleet` arm and disarm the
//! [`crate::fleet`] watch registry the [`crate::fleet::FleetNotifier`] reads;
//! `list_fleet_watches` reads it back. The parent dispatch match routes here.

use super::*;

/// Parse the `scope` argument of `watch_fleet` into a [`crate::fleet::WatchScope`].
/// Accepts the string `"captains"` (default) or `"all"`, or an array of tile ids
/// (an explicit session list). An empty/absent scope defaults to captains.
pub(super) fn parse_watch_scope(args: &Value) -> Result<crate::fleet::WatchScope, String> {
    use crate::fleet::WatchScope;
    match args.get("scope") {
        None | Some(Value::Null) => Ok(WatchScope::Captains),
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "captains" | "" => Ok(WatchScope::Captains),
            "all" => Ok(WatchScope::All),
            other => Err(format!(
                "watch_fleet: unknown scope '{other}' (use \"captains\", \"all\", or an array of session ids)"
            )),
        },
        Some(Value::Array(arr)) => {
            let ids: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if ids.is_empty() {
                return Err("watch_fleet: scope array must contain at least one session id".into());
            }
            Ok(WatchScope::Sessions(ids))
        }
        Some(_) => Err(
            "watch_fleet: 'scope' must be \"captains\", \"all\", or an array of session ids".into(),
        ),
    }
}

/// `watch_fleet` (Organization, audited): arm an orchestrator wake. The CALLING
/// orchestrator (identified by its own tile id in `orchestratorSessionId`) asks to
/// be re-invoked - a wake prompt injected into its terminal - whenever a session in
/// `scope` (default: every claimed captain) transitions into one of `states`
/// (default: the actionable set - idle/turn-complete, needs-input, completed/exited).
/// Requires a live terminal (like `claim_captain`), so a bogus id can't arm a dead
/// watch. Idempotent: re-arming replaces the prior watch for that orchestrator.
pub(super) fn enforce_watch_owner(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    orchestrator: &str,
    command: &str,
) -> Result<(), String> {
    require_socket_identity(caller, trusted_internal, command)?;
    if caller_is_apex(caller, trusted_internal) {
        return Ok(());
    }
    let caller = caller.expect("non-apex watch caller is identified");
    if caller.tile.as_deref() == Some(orchestrator) {
        return Ok(());
    }
    let owns_same_ship = caller.fleet_role == Some(FleetRole::Captain)
        && caller.ship_slug.is_some()
        && target_ship_slug(ctx, orchestrator).as_deref() == caller.ship_slug.as_deref();
    if owns_same_ship {
        return Ok(());
    }
    Err(format!(
        "acl: '{command}' may mutate only the caller's own or same-ship watch"
    ))
}

pub(super) fn ship_scoped_watch_scope(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    scope: crate::fleet::WatchScope,
) -> Result<crate::fleet::WatchScope, String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(scope);
    }
    let caller = caller.ok_or("watch_fleet requires a durable caller identity")?;
    let ship = caller
        .ship_slug
        .as_deref()
        .ok_or("watch_fleet requires an authoritative ship scope")?;
    let snapshot = ctx.captains.snapshot();
    let mut captain_ids = Vec::new();
    let mut all_ids = Vec::new();
    for captain in snapshot
        .captains
        .iter()
        .filter(|captain| captain.ship_slug == ship && captain.state == ClaimState::Active)
    {
        if let Some(terminal_id) = &captain.terminal_id {
            captain_ids.push(terminal_id.clone());
            all_ids.push(terminal_id.clone());
        }
        all_ids.extend(
            captain
                .crew
                .iter()
                .filter(|crew| matches!(crew.state, CrewState::Active))
                .map(|crew| crew.terminal_id.clone()),
        );
    }
    let sessions = match scope {
        crate::fleet::WatchScope::Captains => captain_ids,
        crate::fleet::WatchScope::All => all_ids,
        crate::fleet::WatchScope::Sessions(sessions) => {
            for target in &sessions {
                if caller.tile.as_deref() != Some(target.as_str())
                    && target_ship_slug(ctx, target).as_deref() != Some(ship)
                {
                    return Err(format!(
                        "acl: watch_fleet target '{target}' is outside caller ship '{ship}'"
                    ));
                }
            }
            sessions
        }
    };
    if sessions.is_empty() {
        return Err(format!(
            "watch_fleet has no active sessions in caller ship '{ship}'"
        ));
    }
    Ok(crate::fleet::WatchScope::Sessions(sessions))
}

pub(super) fn watch_fleet(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let orchestrator = arg_str(args, "orchestratorSessionId")
        .or_else(|| arg_str(args, "orchestrator_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("watch_fleet requires an 'orchestratorSessionId' argument (the orchestrator's own session id)")?;
    enforce_watch_owner(ctx, caller, trusted_internal, &orchestrator, "watch_fleet")?;
    // De-conflation (spawn-wedge): only a DEFINITIVE `Gone` rejects; an `Unknown`
    // probe is a retryable control-plane timeout, not proof the orchestrator died.
    match tmux::session_liveness(&tmux_target(&orchestrator)) {
        tmux::SessionLiveness::Alive => {}
        tmux::SessionLiveness::Gone => {
            return Err(format!(
                "watch_fleet: no live terminal for orchestrator '{orchestrator}' \
                 (a wake could never be delivered to a dead session)"
            ));
        }
        tmux::SessionLiveness::Unknown => {
            return Err(retryable_error(format!(
                "watch_fleet: liveness probe for orchestrator '{orchestrator}' timed out; \
                 not confirmed live — retry (the control plane is degraded, not the session)"
            )));
        }
    }
    let scope = ship_scoped_watch_scope(ctx, caller, trusted_internal, parse_watch_scope(args)?)?;
    // `states`: an array of camelCase status strings, or absent for the default
    // actionable set. Unrecognized strings are dropped (they can never match a real
    // status); an all-unrecognized list falls back to the default rather than a
    // watch that can never fire.
    let states = match args.get("states").and_then(|v| v.as_array()) {
        Some(arr) => {
            let strs: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            target_statuses(&strs)
        }
        None => Vec::new(),
    };
    let watch = ctx.fleet_watches.arm(&orchestrator, scope, states);
    Ok(json!({
        "accepted": "watch_fleet",
        "audited": true,
        "watch": watch,
        "note": "armed - this session will be woken (a prompt injected into its \
                 terminal) when a watched session transitions into a target state.",
    }))
}

/// `unwatch_fleet` (Organization, audited): disarm an orchestrator wake previously
/// armed by `watch_fleet`, addressed by `orchestratorSessionId`. Idempotent-ish:
/// reports whether a watch was actually removed.
pub(super) fn unwatch_fleet(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let orchestrator = arg_str(args, "orchestratorSessionId")
        .or_else(|| arg_str(args, "orchestrator_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("unwatch_fleet requires an 'orchestratorSessionId' argument")?;
    enforce_watch_owner(
        ctx,
        caller,
        trusted_internal,
        &orchestrator,
        "unwatch_fleet",
    )?;
    let removed = ctx.fleet_watches.disarm(&orchestrator);
    Ok(json!({
        "accepted": "unwatch_fleet",
        "audited": true,
        "removed": removed,
    }))
}

/// `list_fleet_watches` (Read): the armed orchestrator wakes.
pub(super) fn list_fleet_watches(ctx: &ControlContext) -> Result<Value, String> {
    let watches = ctx.fleet_watches.snapshot();
    Ok(json!({
        "watches": watches,
        "count": watches.len(),
    }))
}
