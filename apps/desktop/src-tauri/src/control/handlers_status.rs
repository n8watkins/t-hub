//! Session-status / supervision / host-monitoring READ handlers, split out of
//! `control.rs` to shrink that module. `get_status` / `wait_for_status` /
//! `supervision_tree`, the request-status and supervisor-key helpers, the
//! target-status parsing shared with `handlers_fleet`, plus `wsl_health` /
//! `recent_sessions` / `scribe_status` / `invalidate_recent_cache`. The parent
//! dispatch match routes here.

use super::*;

/// `get_status`: FR-012 status for one session id (from the supervision reducer)
/// plus the latest statusline snapshot (context %, rate-limit windows) if one
/// has been ingested. `args.sessionId` selects the session.
pub(super) fn get_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("get_status requires a 'sessionId' argument")?;
    let key = resolve_supervisor_key(ctx, &session_id);
    let status = ctx.with_supervisor(|s| s.status(&key));
    let snapshot = ctx.status.get(&key);
    Ok(json!({
        "sessionId": session_id,
        "resolvedSessionId": key,
        "status": status,
        "snapshot": snapshot,
    }))
}

/// `get_request_status` (Read tier; ask #1): resolve "what happened to request X?"
/// for a spawn-class `requestId`, so a caller whose response leg failed can learn
/// the true outcome instead of guessing (and risking a duplicate). Returns:
///   - `{status:"completed", ok:true,  result}`  the command applied; here is its result
///   - `{status:"completed", ok:false, error}`   the command ran and failed
///   - `{status:"inFlight"}`                      still running; do not retry yet
///   - `{status:"unknown"}`                       never seen / evicted: the command
///                                                did NOT land under this id, so a
///                                                retry with the same id is safe.
/// Args: `requestId` (required).
pub(super) fn get_request_status(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let request_id = arg_str(args, "requestId")
        .or_else(|| arg_str(args, "request_id"))
        .ok_or("get_request_status requires a 'requestId' argument")?;
    let body = match ctx.requests.status(&request_id) {
        RequestStatus::Unknown => match ctx.history.resume_operation(&request_id) {
            Ok(Some(operation)) => {
                authorize_history_request_status(
                    ctx,
                    caller,
                    trusted_internal,
                    operation.authorized_ship_slug.as_deref(),
                    operation.authorized_project_id.as_deref(),
                    operation.authorized_assignment_id.as_deref(),
                )?;
                match exact_history_runtime_liveness(
                    ctx,
                    operation.harness,
                    &operation.conversation_id,
                    &operation.terminal_id,
                ) {
                    crate::history::AssociationLiveness::Active => json!({
                        "requestId": request_id,
                        "status": "completed",
                        "ok": true,
                        "durable": true,
                        "result": {
                            "accepted": "history_resume",
                            "requestId": operation.request_id,
                            "historyId": operation.history_id,
                            "harness": operation.harness,
                            "conversationId": operation.conversation_id,
                            "terminalId": operation.terminal_id,
                            "tabId": operation.actual_tab_id,
                            "status": "active",
                            "replayed": true,
                        },
                    }),
                    crate::history::AssociationLiveness::Inactive => json!({
                        "requestId": request_id,
                        "status": "completed",
                        "ok": false,
                        "durable": true,
                        "error": "history_previous_resume_closed: the resumed terminal is closed or now hosts a different conversation",
                    }),
                    crate::history::AssociationLiveness::Unknown => json!({
                        "requestId": request_id,
                        "status": "inFlight",
                        "durable": true,
                        "retryable": true,
                    }),
                }
            }
            Ok(None) => match ctx.history.pending_resume(&request_id) {
                Ok(Some(pending)) => {
                    authorize_history_request_status(
                        ctx,
                        caller,
                        trusted_internal,
                        pending.authorized_ship_slug.as_deref(),
                        pending.authorized_project_id.as_deref(),
                        pending.authorized_assignment_id.as_deref(),
                    )?;
                    json!({
                        "requestId": request_id,
                        "status": "inFlight",
                        "durable": true,
                        "retryable": true,
                    })
                }
                Ok(None) => json!({ "requestId": request_id, "status": "unknown" }),
                Err(error) => json!({
                    "requestId": request_id,
                    "status": "completed",
                    "ok": false,
                    "durable": true,
                    "error": error,
                }),
            },
            Err(error) => json!({
                "requestId": request_id,
                "status": "completed",
                "ok": false,
                "durable": true,
                "error": error,
            }),
        },
        RequestStatus::InFlight => json!({ "requestId": request_id, "status": "inFlight" }),
        RequestStatus::Completed(Ok(result)) => {
            if result.get("accepted").and_then(Value::as_str) == Some("history_resume") {
                let operation = ctx.history.resume_operation(&request_id)?.ok_or(
                    "history_recovery_required: completed History request has no durable operation",
                )?;
                authorize_history_request_status(
                    ctx,
                    caller,
                    trusted_internal,
                    operation.authorized_ship_slug.as_deref(),
                    operation.authorized_project_id.as_deref(),
                    operation.authorized_assignment_id.as_deref(),
                )?;
            }
            json!({
                "requestId": request_id,
                "status": "completed",
                "ok": true,
                "result": result,
            })
        }
        RequestStatus::Completed(Err(error)) => json!({
            "requestId": request_id,
            "status": "completed",
            "ok": false,
            "error": error,
        }),
    };
    Ok(body)
}

/// Resolve a caller-supplied `sessionId` to the supervision reducer's key (a Claude
/// session UUID). The reducer keys sessions by the Claude UUID, but callers routinely
/// pass a T-Hub **tile id** — that is what `list_terminals` / `list_captains` expose
/// (a captain's `captainSessionId` is a tile id). If the id is already a known
/// supervisor key we keep it; otherwise we map `tile -> live UUID` via the status
/// bridge; otherwise we return it unchanged (an unknown id still resolves to
/// `Unknown` / `null`, exactly as before this bridge existed). This closes the split
/// where `get_status` / `supervision_tree` / `wait_for_status` returned `unknown` for
/// a captain addressed by its `captainSessionId`.
pub(super) fn resolve_supervisor_key(ctx: &ControlContext, id: &str) -> String {
    if ctx.with_supervisor(|s| s.knows(id)) {
        return id.to_string();
    }
    if let Some(uuid) = ctx.status.session_for_terminal(id) {
        return uuid;
    }
    id.to_string()
}

/// `wait_for_status`: long-poll the supervision reducer until a session reaches a
/// target FR-012 status (or a timeout). The reducer is snapshot-only, but it keeps
/// a bounded **transition log** (see [`Supervisor`]) so this is *edge-capturing*:
/// a status the session merely passes *through* between two 500ms polls (e.g.
/// working→completed→working, or a transient `needsQuestion`) is still observed,
/// instead of being missed and reported as a spurious `timedOut`.
///
/// How it works: we capture the supervisor's `current_seq()` up front, check the
/// current status for an immediate match, then loop — each iteration checks both
/// (a) the *current* status and (b) any logged transition for this session since
/// the last-consumed seq whose status matches a target (advancing the consumed
/// seq as we go). Either hit returns immediately. Each `with_supervisor` call
/// acquires + drops the supervisor mutex, and the 500ms sleep is *outside* the
/// lock, so the reducer keeps advancing (and logging edges) while we wait.
/// Blocking this control connection's thread for up to `timeoutMs` is expected:
/// connections are handled per-connection.
///
/// Args: `sessionId` (required), `targetStatus` (required; a camelCase status
/// string or an array of them — matches any), `timeoutMs` (optional, default
/// 30000). Returns `{ finalStatus, elapsedMs, timedOut }`. Statuses are compared
/// by serializing [`SessionStatus`] to its camelCase string, so the target
/// strings match the `get_status` / IPC representation exactly.
pub(super) fn wait_for_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("wait_for_status requires a 'sessionId' argument")?;
    let targets = parse_target_statuses(args)?;
    // The same targets, resolved once to enum space for the transition-log edge
    // query (`matched_since`). Hoisted out of the loop since it never changes.
    let target_enums = target_statuses(&targets);
    let timeout = std::time::Duration::from_millis(
        args.get("timeoutMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30000),
    );

    // Watermark: every transition with seq > `consumed` is one we have not yet
    // inspected. Captured before we start waiting, so any edge that lands while we
    // sleep (including a transient status the session passes *through*) is caught
    // on a later iteration. We return on the first match, so this stays fixed.
    let consumed = ctx.with_supervisor(|s| s.current_seq());

    let started = std::time::Instant::now();
    loop {
        // Resolve the caller id to a supervisor key each iteration: a captain passed
        // by its tile id may not have a `tile -> uuid` binding yet on the first poll
        // (no statusline ingested), but it appears once the session emits — so a wait
        // armed a hair early still latches on. Resolved OUTSIDE `with_supervisor`
        // because the resolver itself takes the supervisor lock.
        let key = resolve_supervisor_key(ctx, &session_id);
        // (a) current status, and (b) any transition edge for this session since
        // `consumed` that matches a target — both read under one lock acquisition.
        // We advance `consumed` past every inspected edge so we never re-scan.
        let (status, edge_match) = ctx.with_supervisor(|s| {
            let status = s.status(&key);
            let edge = s.matched_since(&key, &target_enums, consumed);
            (status, edge)
        });
        let status_str = status_camel(status);
        let elapsed = started.elapsed();

        // An edge we slept through matched a target — report that status as final,
        // even though the *current* status may have already moved on past it. (We
        // return on the first match, so there's no need to advance `consumed`
        // past this edge; the watermark only matters across the no-match sleeps.)
        if let Some((_seq, matched_status)) = edge_match {
            return Ok(json!({
                "finalStatus": status_camel(matched_status),
                "elapsedMs": elapsed.as_millis() as u64,
                "timedOut": false,
            }));
        }
        // The current status matches a target.
        if targets.iter().any(|t| t == &status_str) {
            return Ok(json!({
                "finalStatus": status_str,
                "elapsedMs": elapsed.as_millis() as u64,
                "timedOut": false,
            }));
        }
        if elapsed >= timeout {
            return Ok(json!({
                "finalStatus": status_str,
                "elapsedMs": elapsed.as_millis() as u64,
                "timedOut": true,
            }));
        }
        // Mutex is already released (with_supervisor drops it per call); sleep
        // outside the lock so the reducer keeps advancing while we wait. The log
        // captures any edges the session crosses during this sleep window.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Resolve the parsed camelCase target strings back to [`SessionStatus`] values
/// for the transition-log edge query (`matched_since` works in enum space, while
/// the wire targets arrive as strings). Unrecognized strings are dropped — they
/// can never match a real logged status anyway, and the current-status string
/// comparison still covers any exotic value.
pub(super) fn target_statuses(targets: &[String]) -> Vec<crate::model::SessionStatus> {
    targets
        .iter()
        .filter_map(|t| {
            serde_json::from_value::<crate::model::SessionStatus>(Value::String(t.clone())).ok()
        })
        .collect()
}

/// Serialize a [`SessionStatus`] to its camelCase wire string (e.g. "completed",
/// "needsQuestion"), matching the `get_status` / IPC representation. The enum is
/// `#[serde(rename_all = "camelCase")]`, so it serializes to a bare JSON string.
pub(super) fn status_camel(status: crate::model::SessionStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Parse `targetStatus` into a non-empty set of camelCase status strings. Accepts
/// a single string or an array of strings (matches any).
pub(super) fn parse_target_statuses(args: &Value) -> Result<Vec<String>, String> {
    let raw = args
        .get("targetStatus")
        .ok_or("wait_for_status requires a 'targetStatus' argument (string or array of strings)")?;
    let targets: Vec<String> = match raw {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            return Err(
                "wait_for_status 'targetStatus' must be a string or an array of strings".into(),
            )
        }
    };
    if targets.is_empty() {
        return Err("wait_for_status 'targetStatus' must not be empty".into());
    }
    Ok(targets)
}

/// `supervision_session_ids`: every session id the supervision reducer knows.
/// Mirrors the `supervision_session_ids` Tauri command; returns a JSON array of ids
/// (server-split M1 - the supervision/status read surface moves onto the socket).
pub(super) fn supervision_session_ids(ctx: &ControlContext) -> Result<Value, String> {
    let ids = ctx.with_supervisor(|s| s.session_ids());
    serde_json::to_value(ids).map_err(|e| e.to_string())
}

/// `supervision_tree`: the read-only orchestrator→subagent tree for one session.
/// Returns `null` when the session is unknown (matching the Tauri command).
pub(super) fn supervision_tree(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("supervision_tree requires a 'sessionId' argument")?;
    let key = resolve_supervisor_key(ctx, &session_id);
    let tree = ctx.with_supervisor(|s| s.tree(&key));
    serde_json::to_value(tree).map_err(|e| e.to_string())
}

/// `wsl_health`: a compact WSL host snapshot. We synthesize it from the locally
/// observable system (so the read tool works on this dev box without the WSL
/// agent connected) and additionally surface the supervised-session count. The
/// schema mirrors `t_hub_protocol::HostMetrics`.
pub(super) fn wsl_health(ctx: &ControlContext) -> Result<Value, String> {
    let metrics = collect_host_metrics();
    let supervised = ctx.with_supervisor(|s| s.session_ids().len());
    Ok(json!({
        "metrics": metrics,
        "supervisedSessions": supervised,
    }))
}

/// `recent_sessions` (server-split M3 - first overlay source over the wire): the
/// daemon's recent recallable Claude sessions, so a thin client gets the Recent
/// list remotely. Mirrors the `recent_sessions` Tauri command (same
/// `RecentSession[]` shape), reusing its shared scan cache. When the daemon runs
/// natively in WSL (the M3 endgame) this read is a plain local filesystem walk
/// rather than the `wsl.exe`/UNC hop.
pub(super) fn recent_sessions() -> Result<Value, String> {
    serde_json::to_value(crate::recent::recent_sessions_cached()).map_err(|e| e.to_string())
}

/// `scribe_status` (read tier): the Scribe voice-gate - asks Scribe's v1
/// status endpoint (loopback HTTP, discovered via `~/.scribe/control.json`)
/// whether the general is inside a dictation cycle, falling back to Scribe's
/// status.json file (pid + 15s updatedAt TTL) only when the endpoint is
/// unavailable. Returns `{listening, status, since, source}` - `listening` is
/// sourced from the snapshot's level-triggered `busy` flag - and fails open to
/// `listening: false` whenever it cannot positively confirm an active
/// dictation (see crate::scribe). Lets an agent ask "is the general dictating
/// right now?".
pub(super) fn scribe_status() -> Result<Value, String> {
    serde_json::to_value(crate::scribe::read_scribe_status()).map_err(|e| e.to_string())
}

/// `invalidate_recent_cache` (Tier 3 reap): drop the recent-sessions cache so a
/// just-closed workspace's sessions show in Recent immediately, not after the 15s TTL.
pub(super) fn invalidate_recent_cache() -> Result<Value, String> {
    crate::recent::invalidate_recent_cache();
    Ok(Value::Bool(true))
}
