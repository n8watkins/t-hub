//! Comms-plane messaging + authorization control handlers, split out of
//! `control.rs` to shrink that module. Inbox intake / status (`inbox_ack`,
//! `inbox_status`), plane messaging (`plane_send`, `abort_session`) with their
//! sender-label / priority helpers, and the delegated-authorization checks
//! (`authorize`, `check_authorization`). The parent dispatch match routes here.

use super::*;

/// `read_terminal` / `capture_pane`: return a session's recent visible output as
/// plain text so an external Claude can SEE what the session shows. Talks to tmux
/// directly (`tmux -L t-hub capture-pane -p [-S -N] -t th_<id>`), no UI round
/// trip. Args: `sessionId` (required), `historyLines` (optional, default 0 =
/// visible screen only; clamped to keep responses bounded).
/// Comms-plane Phase 2: `inbox_ack` - the recipient confirms intake of a delivered
/// inbox message (`delivered -> processed`, §2.4 M2). `sessionId` is the recipient's
/// own tile id (the inbox key the wake enqueued under); `seq` the message. The ACK is
/// idempotent + safe: a lost or duplicate ack never triggers a re-write, and acking
/// before delivery / an unknown seq is reported honestly rather than silently
/// advancing state. Phase 2 does NOT authorize the caller (no ACLs) - Phase 3's
/// ownership ACL gates a cross-session ack.
pub(super) fn inbox_ack(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "inbox_ack")?;
    let recipient = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("inbox_ack requires a 'sessionId' argument")?;
    let seq = args
        .get("seq")
        .and_then(|v| v.as_u64())
        .ok_or("inbox_ack requires a numeric 'seq' argument")?;
    // Phase 3 (§2.4.1): the ownership ACL. A session self-acks ONLY its OWN inbox (its
    // tile == the recipient key); a control-capable HOST/RELAY (Full) may ack on behalf
    // (the interim relay path, still supported). A read-token session acking a DIFFERENT
    // recipient is refused - the cross-session spoof the interim Organization gate feared
    // is closed by the per-session identity, not re-opened.
    // A session identity that is NOT the recipient itself may only ack with the control
    // capability does not override self-scope; only a proven in-process host may ack
    // without a session identity.
    if let Some(id) = caller {
        if let Err(d) = crate::acl::can_ack(&acl_actor(id), &recipient) {
            ctx.fanout.emit_event(
                "control://acl",
                &json!({
                    "cell": "inbox-ack-self-scope",
                    "decision": "refused",
                    "session": id.session_id.as_str(),
                    "recipient": recipient.as_str(),
                    "reason": d.reason.as_str(),
                }),
            );
            return Err(format!("acl: {}", d.reason));
        }
    }
    let outcome = ctx.inbox.ack(&recipient, seq);
    let state = match outcome {
        crate::inbox::AckOutcome::Processed { .. } => "processed",
        crate::inbox::AckOutcome::AlreadyProcessed { .. } => "alreadyProcessed",
        crate::inbox::AckOutcome::NotDelivered { .. } => "notDelivered",
        crate::inbox::AckOutcome::Unknown { .. } => "unknown",
    };
    Ok(json!({
        "accepted": "inbox_ack",
        "sessionId": recipient,
        "seq": seq,
        "state": state,
    }))
}

/// Comms-plane Phase 2: `inbox_status` - per-recipient observability (§2.8). With a
/// `sessionId` it returns that recipient's depth snapshot; without one, every
/// recipient's. Counts + cursors + oldest-un-drained age only, never message content.
pub(super) fn inbox_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    if let Some(recipient) = arg_str(args, "sessionId").or_else(|| arg_str(args, "session_id")) {
        Ok(json!({ "recipient": ctx.inbox.depth(&recipient) }))
    } else {
        Ok(json!({ "recipients": ctx.inbox.depth_all() }))
    }
}

/// The attributed `sender` label for a plane message from a resolved identity: the same
/// `role:id` shape `identity::SessionIdentity::sender_label` produces, so the inbox's
/// attribution stamp is per-session (not the coarse tier). A host (no session) stamps
/// the coarse source label.
pub(super) fn caller_sender_label(caller: Option<&ResolvedIdentity>) -> String {
    match caller {
        Some(id) => format!("{}:{}", acl_actor(id).role.label(), id.session_id),
        None => "control-host".to_string(),
    }
}

/// Parse the requested plane priority. Absent / `"standard"` => Standard; `"emergency"`
/// => Emergency (the authority to actually SET it is checked separately by
/// [`crate::acl::can_flag_emergency`] - the field is a REQUEST, the role is the grant).
pub(super) fn parse_priority(args: &Value) -> Result<crate::inbox::Priority, String> {
    match arg_str(args, "priority").as_deref() {
        None | Some("standard") | Some("Standard") => Ok(crate::inbox::Priority::Standard),
        Some("emergency") | Some("Emergency") => Ok(crate::inbox::Priority::Emergency),
        Some(other) => Err(format!(
            "plane_send: unknown priority '{other}' (expected 'standard' or 'emergency')"
        )),
    }
}

/// Comms-plane Phase 3: the agent-to-agent plane SEND. Enforces the settled matrix
/// message rows (`can_message`) at enqueue time and the EMERGENCY-flag authority
/// (`can_flag_emergency`), then enqueues a durable, attributed message. A denied send is
/// REFUSED + attributed on `control://acl`, never a silent drop. An identified session is
/// REQUIRED (or a proven in-process host) - a token with no session cannot
/// enqueue as anyone.
pub(super) fn plane_send(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "plane_send")?;
    let recipient = arg_str(args, "recipient")
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("plane_send requires a 'recipient' (recipient tile id) argument")?;
    let body = arg_str(args, "text")
        .or_else(|| arg_str(args, "body"))
        .ok_or("plane_send requires a 'text' argument")?;
    let enter = args.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);
    let priority = parse_priority(args)?;

    let refuse = |ctx: &ControlContext, cell: &str, who: &str, reason: &str| -> String {
        ctx.fanout.emit_event(
            "control://acl",
            &json!({
                "cell": cell,
                "decision": "refused",
                "session": who,
                "recipient": recipient.as_str(),
                "reason": reason,
            }),
        );
        format!("acl: {reason}")
    };

    if let Some(id) = caller {
        let actor = acl_actor(id);
        let target = message_target(ctx, &recipient);
        if let Err(d) = crate::acl::can_message(&actor, &target) {
            return Err(refuse(ctx, "message-send", &actor.session_id, &d.reason));
        }
        if priority == crate::inbox::Priority::Emergency {
            if let Err(d) = crate::acl::can_flag_emergency(&actor) {
                return Err(refuse(ctx, "emergency-flag", &actor.session_id, &d.reason));
            }
        }
    }

    let sender = caller_sender_label(caller);
    match ctx
        .inbox
        .enqueue(&recipient, &sender, priority, &body, enter)
    {
        Ok(outcome) => Ok(json!({
            "accepted": "plane_send",
            "recipient": recipient,
            "seq": outcome.seq,
            "priority": priority,
            "sender": sender,
        })),
        Err(e) => Err(e.to_string()),
    }
}

/// Comms-plane Phase 3: the ABORT/interrupt-subordinate primitive. Delivers an Escape
/// interrupt (the claude turn-interrupt; the Escape/C-c equivalent) to the target's
/// runtime - a preempt signal, NOT a queued input message, so it cannot be typed over or
/// corrupt a human draft. Gated by `can_abort`; an identified caller must own the target
/// (Cortana->captain, captain->own crew, general->anyone), cross-ship/sibling is DENIED,
/// and crew have no subordinate to abort. A proven in-process host acts as the apex.
pub(super) fn abort_session(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "abort_session")?;
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .or_else(|| arg_str(args, "target"))
        .ok_or("abort_session requires a 'sessionId' (target tile id) argument")?;

    if let Some(id) = caller {
        let actor = acl_actor(id);
        let target = target_ship_ref(ctx, &session_id);
        if let Err(d) = crate::acl::can_abort(&actor, &target) {
            ctx.fanout.emit_event(
                "control://acl",
                &json!({
                    "cell": "abort",
                    "decision": "refused",
                    "session": actor.session_id.as_str(),
                    "role": actor.role.label(),
                    "target": session_id.as_str(),
                    "reason": d.reason.as_str(),
                }),
            );
            return Err(format!("acl: {}", d.reason));
        }
    }
    // A caller without a session identity reached this point only with in-process host proof.

    let target = tmux_target(&session_id);
    writer_liveness_gate(
        "abort_session",
        &session_id,
        &target,
        tmux::session_liveness(&target),
    )?;
    // Escape interrupts the current turn (claude's in-turn interrupt) WITHOUT killing the
    // session or its in-flight state - the C4-safe abort the design calls for.
    tmux::send_keys(&target, &["Escape"])
        .map_err(|e| format!("failed to deliver abort interrupt to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "abort_session",
        "sessionId": session_id,
        "target": target,
        "signal": "Escape",
        "audited": true,
    }))
}

/// Comms-plane Phase 3: record a durable general-authorization artifact (the delegation-
/// gate carrier, M1). Only the GENERAL may ORIGINATE (`can_originate_authorization`);
/// Cortana may relay by reference but never originate. The origin is APP-STAMPED from the
/// resolved identity's role (never sender-supplied), so a captain/crew with a Full token
/// but a non-general session cannot forge a general authorization.
pub(super) fn authorize(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
) -> Result<Value, String> {
    let Some(id) = caller else {
        return Err(
            "authorize requires a per-session identity resolving to the GENERAL role \
             (only the general originates an authorization)"
                .to_string(),
        );
    };
    let actor = acl_actor(id);
    if let Err(d) = crate::acl::can_originate_authorization(&actor) {
        ctx.fanout.emit_event(
            "control://acl",
            &json!({
                "cell": "authorization-originate",
                "decision": "refused",
                "session": actor.session_id.as_str(),
                "role": actor.role.label(),
                "reason": d.reason.as_str(),
            }),
        );
        return Err(format!("acl: {}", d.reason));
    }
    let action = arg_str(args, "action")
        .ok_or("authorize requires an 'action' (the authorized scope, e.g. 'spend'/'publish')")?;
    let target_ship = arg_str(args, "targetShip").or_else(|| arg_str(args, "target_ship"));
    // Origin role is app-stamped GENERAL (the ACL above proved it); origin_session is the
    // unforgeable attribution root. Direct authorization => no relay reference.
    let auth = ctx.authz.record(
        crate::authz::ORIGIN_GENERAL,
        &actor.session_id,
        &action,
        target_ship,
        None,
    );
    Ok(json!({
        "accepted": "authorize",
        "id": auth.id,
        "action": auth.action,
        "targetShip": auth.target_ship,
    }))
}

/// Comms-plane Phase 3: the resolve-and-verify GATE a captain's money/publish gate
/// consults (`general_authorization_present`). Resolves the referenced artifact by id and
/// verifies its app-stamped origin == general, it is not revoked, and (under STATE 2) it
/// is not a relayed reference. Read-only.
pub(super) fn check_authorization(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let id = arg_str(args, "id")
        .or_else(|| arg_str(args, "authorizationId"))
        .ok_or("check_authorization requires an 'id' (the authorization reference)")?;
    let verdict = ctx
        .authz
        .present(&id, crate::authz::accept_relayed_authorization());
    Ok(json!({
        "id": id,
        "present": verdict.is_present(),
        "verdict": verdict.label(),
    }))
}
