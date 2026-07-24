//! Agent-session lifecycle control handlers, split out of `control.rs` to shrink
//! that module. Agent listing / inspection (`list_agents`, `get_agent`,
//! `agent_events`), `dispatch_preflight`, the followup apply path
//! (`agent_followup` / `apply_agent_followup`), `agent_checkpoint`, and the
//! recorded-delivery integration contract (`record_agent_delivery` + evidence
//! helpers). The parent dispatch match routes here.

use super::*;

pub(super) fn list_agents(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "list_agents",
        &["captainSessionId", "projectId", "cursor", "limit", "state"],
    )?;
    let captain_session_id = arg_str(args, "captainSessionId");
    let project_id = arg_str(args, "projectId");
    if captain_session_id.is_none() && project_id.is_none() {
        return Err("list_agents requires 'captainSessionId' or 'projectId'".into());
    }
    let state = args
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("active");
    if !matches!(state, "active" | "removed") {
        return Err("list_agents state must be 'active' or 'removed'".into());
    }
    let authorization = authorize_agent_filter(
        ctx,
        captain_session_id.as_deref(),
        project_id.as_deref(),
        caller,
        trusted_internal,
        "list_agents",
        true,
    )?;
    let result = (|| {
        let candidate_ids: Vec<String> = ctx
            .captains
            .snapshot()
            .agent_sessions
            .into_iter()
            .filter(|agent| {
                captain_session_id
                    .as_deref()
                    .is_none_or(|captain| agent.captain_session_id == captain)
                    && project_id
                        .as_deref()
                        .is_none_or(|project| agent.project_id == project)
            })
            .map(|agent| agent.agent_session_id)
            .collect();
        for agent_session_id in candidate_ids {
            reconcile_agent_runtime(ctx, &agent_session_id);
        }
        let cursor = agent_page_cursor(args, "list_agents")?;
        let limit = agent_page_limit(args, "list_agents")?;
        let snapshot = ctx.captains.snapshot();
        let mut records: Vec<_> = snapshot
            .agent_sessions
            .into_iter()
            .filter(|agent| {
                captain_session_id
                    .as_deref()
                    .is_none_or(|captain| agent.captain_session_id == captain)
                    && project_id
                        .as_deref()
                        .is_none_or(|project| agent.project_id == project)
                    && (state == "removed"
                        && agent.work_stage == crate::agent_session::WorkStage::Stopped
                        || state == "active"
                            && agent.work_stage != crate::agent_session::WorkStage::Stopped)
                    && authorization.caller_ship.as_deref().is_none_or(|ship| {
                        ctx.captains
                            .captain_for_session(&agent.captain_session_id)
                            .is_some_and(|captain| captain.ship_slug == ship)
                    })
            })
            .collect();
        let agent_ids: std::collections::HashSet<_> = records
            .iter()
            .map(|agent| agent.agent_session_id.as_str())
            .collect();
        let event_cursor = snapshot
            .agent_events
            .iter()
            .filter(|event| agent_ids.contains(event.agent_session_id.as_str()))
            .map(|event| event.cursor)
            .max()
            .unwrap_or(0);
        records.sort_by(|left, right| left.agent_session_id.cmp(&right.agent_session_id));
        let total = records.len();
        let digest = crate::agent_session::snapshot_digest(&records)?;
        let page: Vec<Value> = records
            .into_iter()
            .skip(cursor)
            .take(limit)
            .map(|agent| agent_status_value(agent, false))
            .collect();
        let next_cursor = (cursor + page.len()).min(total);
        Ok(json!({
            "agents": page,
            "count": page.len(),
            "total": total,
            "cursor": cursor.to_string(),
            "nextCursor": (next_cursor < total).then(|| next_cursor.to_string()),
            "hasMore": next_cursor < total,
            "digest": digest,
            "eventCursor": event_cursor,
        }))
    })();
    record_delegated_admin_outcome(ctx, authorization.delegated_audit.as_ref(), &result);
    result
}

pub(super) fn dispatch_preflight(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "dispatch_preflight",
        &[
            "projectId",
            "sourceCommit",
            "requestedLanes",
            "integrationContracts",
        ],
    )?;
    let project_id = arg_str(args, "projectId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("dispatch_preflight requires a non-empty 'projectId'")?;
    authorize_agent_filter(
        ctx,
        None,
        Some(&project_id),
        caller,
        trusted_internal,
        "dispatch_preflight",
        false,
    )?;
    let source_commit = arg_str(args, "sourceCommit")
        .filter(|value| !value.trim().is_empty())
        .ok_or("dispatch_preflight requires a non-empty 'sourceCommit'")?;
    let requested_lanes = serde_json::from_value::<Vec<crate::governor::LaneClaim>>(
        args.get("requestedLanes")
            .cloned()
            .ok_or("dispatch_preflight requires a 'requestedLanes' array")?,
    )
    .map_err(|error| format!("dispatch_preflight requestedLanes are invalid: {error}"))?;
    let integration_contracts = parse_integration_contracts(args, "dispatch_preflight")?;
    let snapshot = ctx.captains.snapshot();
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("dispatch_preflight: unknown projectId '{project_id}'"))?;
    let repo_root = files::posix_form(&project.repo_root);
    require_registered_git_capability(ctx, "dispatch_preflight", &project.repo_root)?;
    git::require_commit_ancestor(&repo_root, &source_commit, &source_commit)
        .map_err(|error| format!("dispatch_preflight: sourceCommit rejected: {error}"))?;
    let dependencies = requested_lanes
        .iter()
        .filter_map(|lane| lane.dependencies.as_ref())
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    let satisfied_dependencies = validate_dependency_result_ancestry(
        "dispatch_preflight",
        &snapshot,
        &project_id,
        &dependencies,
        &repo_root,
        &source_commit,
    )?;
    let request = crate::governor::DispatchPreflight {
        requested_provider_lanes: requested_lanes.len(),
        requested_lanes,
        admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
        ship_admin_scope: None,
        active_lanes: active_dispatch_lanes(&snapshot, &project_id),
        satisfied_dependencies,
        integration_contracts,
        capacity: dispatch_runtime_capacity(ctx, &snapshot, &project_id)?,
    };
    Ok(match ctx.governor.preflight_dispatch(&request) {
        Ok(capacity) => json!({
            "admitted": true,
            "capacity": capacity,
        }),
        Err(refusal) => {
            let capacity = refusal.capacity.clone();
            json!({
                "admitted": false,
                "capacity": capacity,
                "refusal": refusal,
            })
        }
    })
}

pub(super) fn get_agent(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(args, "get_agent", &["agentSessionId"])?;
    let agent_session_id = arg_str(args, "agentSessionId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("get_agent requires a non-empty 'agentSessionId'")?;
    let initial = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .ok_or_else(|| format!("get_agent: agent '{}' was not found", agent_session_id))?;
    authorize_agent(ctx, &initial, caller, trusted_internal, "get_agent")?;
    reconcile_agent_runtime(ctx, &agent_session_id);
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .ok_or_else(|| format!("get_agent: agent '{}' was not found", agent_session_id))?;
    Ok(agent_status_value(agent, true))
}

pub(super) fn parse_agent_followup(
    args: &Value,
) -> Result<crate::agent_session::AgentFollowup, String> {
    require_exact_args(
        args,
        "agent_followup",
        &[
            "requestId",
            "captainSessionId",
            "shipSlug",
            "projectId",
            "agentSessionId",
            "message",
            "replacementAssignment",
        ],
    )?;
    let required = |field: &str| {
        arg_str(args, field).ok_or_else(|| format!("agent_followup requires a non-empty '{field}'"))
    };
    let followup = crate::agent_session::AgentFollowup {
        request_id: required("requestId")?,
        captain_session_id: required("captainSessionId")?,
        ship_slug: required("shipSlug")?,
        project_id: required("projectId")?,
        agent_session_id: required("agentSessionId")?,
        message: required("message")?,
        replacement_assignment: arg_str(args, "replacementAssignment"),
    };
    followup.validate()?;
    Ok(followup)
}

pub(super) fn authorize_agent_followup(
    ctx: &ControlContext,
    followup: &crate::agent_session::AgentFollowup,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<AgentSessionRecord, String> {
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .iter()
        .find(|captain| {
            captain.terminal_id.as_deref() == Some(followup.captain_session_id.as_str())
        })
        .ok_or_else(|| {
            agent_followup_error(
                "captain_not_found",
                format!(
                    "agent_followup: Captain '{}' was not found",
                    followup.captain_session_id
                ),
            )
        })?;
    if captain.role != FleetRole::Captain
        || captain.state != ClaimState::Active
        || captain.ship_slug != followup.ship_slug
        || captain.project_id.as_deref() != Some(followup.project_id.as_str())
    {
        return Err(agent_followup_error(
            "ownership_mismatch",
            "agent_followup: Captain, ship, and Project ownership do not match",
        ));
    }
    let agent = snapshot
        .agent_sessions
        .iter()
        .find(|agent| agent.agent_session_id == followup.agent_session_id)
        .cloned()
        .ok_or_else(|| {
            agent_followup_error(
                "agent_not_found",
                format!(
                    "agent_followup: agent '{}' was not found",
                    followup.agent_session_id
                ),
            )
        })?;
    if agent.captain_session_id != followup.captain_session_id
        || agent.project_id != followup.project_id
    {
        return Err(agent_followup_error(
            "ownership_mismatch",
            "agent_followup: agent is not owned by the specified Captain and Project",
        ));
    }
    if authorize_agent(ctx, &agent, caller, trusted_internal, "agent_followup")
        != Ok(AgentAuthority::Captain)
    {
        return Err(agent_followup_error(
            "unauthorized",
            "acl: 'agent_followup' requires the exact active owning Captain",
        ));
    }
    if agent.runtime_state == RuntimeState::Exited
        || agent.work_stage == crate::agent_session::WorkStage::Stopped
    {
        return Err(agent_followup_error(
            "agent_exited",
            format!(
                "agent_followup: agent '{}' has exited and cannot receive follow-up work",
                followup.agent_session_id
            ),
        ));
    }
    Ok(agent)
}

/// Deliver an owned agent a durable follow-up without terminal injection. The
/// inbox is keyed by durable agentSessionId, and its request receipt provides the
/// restart-safe idempotency boundary. Assignment metadata changes only through
/// the explicit replacementAssignment field.
pub(super) fn agent_followup(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let followup = parse_agent_followup(args)
        .map_err(|error| agent_followup_error("invalid_request", error))?;
    let outcome = apply_agent_followup(ctx, &followup, caller, trusted_internal)?;
    Ok(json!({
        "accepted": "agent_followup",
        "requestId": outcome.request_id,
        "captainSessionId": outcome.captain_session_id,
        "shipSlug": outcome.ship_slug,
        "projectId": outcome.project_id,
        "agentSessionId": outcome.agent_session_id,
        "messageSeq": outcome.message_seq,
        "idempotentReplay": outcome.idempotent_replay,
        "assignmentChanged": outcome.assignment_changed,
    }))
}

pub(super) fn apply_agent_followup(
    ctx: &ControlContext,
    followup: &crate::agent_session::AgentFollowup,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<crate::agent_session::AgentFollowupOutcome, String> {
    let agent = authorize_agent_followup(ctx, followup, caller, trusted_internal)?;

    let sender = format!("captain:{}", followup.captain_session_id);
    let prepared = ctx
        .inbox
        .prepare_idempotent(
            &followup.agent_session_id,
            &sender,
            crate::inbox::Priority::Standard,
            &followup.message,
            true,
            &followup.request_id,
            &followup.semantic_digest(),
        )
        .map_err(|error| match error {
            crate::inbox::EnqueueError::IdempotencyConflict { .. } => {
                agent_followup_error("request_conflict", error)
            }
            crate::inbox::EnqueueError::Persistence { .. } => {
                agent_followup_error("persistence_failed", error)
            }
            crate::inbox::EnqueueError::Overflow { .. } => {
                agent_followup_error("inbox_overflow", error)
            }
        })?;
    let assignment_changed = followup
        .replacement_assignment
        .as_deref()
        .is_some_and(|replacement| replacement != agent.assignment);
    if let Some(replacement) = &followup.replacement_assignment {
        ctx.captains
            .replace_agent_assignment(&followup.agent_session_id, replacement)
            .map_err(|error| agent_followup_error("persistence_failed", error))?;
    }
    let activated = ctx
        .inbox
        .activate_prepared(&followup.agent_session_id, &followup.request_id)
        .map_err(|error| match error {
            crate::inbox::EnqueueError::IdempotencyConflict { .. } => {
                agent_followup_error("request_conflict", error)
            }
            crate::inbox::EnqueueError::Persistence { .. } => {
                agent_followup_error("persistence_failed", error)
            }
            crate::inbox::EnqueueError::Overflow { .. } => {
                agent_followup_error("inbox_overflow", error)
            }
        })?;
    Ok(crate::agent_session::AgentFollowupOutcome {
        request_id: followup.request_id.clone(),
        captain_session_id: followup.captain_session_id.clone(),
        ship_slug: followup.ship_slug.clone(),
        project_id: followup.project_id.clone(),
        agent_session_id: followup.agent_session_id.clone(),
        message_seq: activated.seq,
        idempotent_replay: prepared.duplicate,
        assignment_changed,
    })
}

pub(super) fn agent_status_value(agent: AgentSessionRecord, include_assignment: bool) -> Value {
    let delivery_states = agent.delivery_states();
    let mut value = serde_json::to_value(agent).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        if !include_assignment {
            object.remove("assignment");
        }
        object.insert(
            "deliveryStates".into(),
            serde_json::to_value(delivery_states).unwrap_or(Value::Null),
        );
    }
    value
}

pub(super) fn agent_checkpoint(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "agent_checkpoint",
        &["agentSessionId", "authorSessionId", "summary", "stage"],
    )?;
    let agent_session_id = arg_str(args, "agentSessionId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("agent_checkpoint requires a non-empty 'agentSessionId'")?;
    let author_session_id = arg_str(args, "authorSessionId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("agent_checkpoint requires a non-empty 'authorSessionId'")?;
    let summary = arg_str(args, "summary")
        .filter(|value| !value.trim().is_empty())
        .ok_or("agent_checkpoint requires a non-empty 'summary'")?;
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .ok_or_else(|| format!("agent_checkpoint: agent '{agent_session_id}' was not found"))?;
    let authority = authorize_agent(ctx, &agent, caller, trusted_internal, "agent_checkpoint")?;
    if !trusted_internal && caller.is_none_or(|identity| identity.session_id != author_session_id) {
        return Err(
            "acl: agent_checkpoint authorSessionId must match the authenticated session".into(),
        );
    }
    let stage = args
        .get("stage")
        .map(|value| {
            serde_json::from_value::<crate::agent_session::WorkStage>(value.clone())
                .map_err(|_| "agent_checkpoint stage is invalid".to_string())
        })
        .transpose()?;
    if let Some(stage) = stage {
        let allowed = match authority {
            AgentAuthority::Apex | AgentAuthority::Captain => true,
            AgentAuthority::Agent => matches!(
                stage,
                crate::agent_session::WorkStage::Working
                    | crate::agent_session::WorkStage::NeedsInput
                    | crate::agent_session::WorkStage::ReadyForReview
            ),
        };
        if !allowed {
            return Err(
                "acl: agent_checkpoint stage is not permitted for the authenticated actor".into(),
            );
        }
    }
    let checkpoint = ctx.captains.append_agent_checkpoint(
        &agent_session_id,
        &author_session_id,
        &summary,
        stage,
    )?;
    Ok(json!({
        "checkpoint": checkpoint,
        "eventCursor": checkpoint.cursor,
    }))
}

pub(super) fn required_delivery_evidence<'a>(
    args: &'a Value,
    state: &str,
    fields: &[&str],
) -> Result<&'a Value, String> {
    let evidence = args
        .get("evidence")
        .filter(|value| value.is_object())
        .ok_or("record_agent_delivery requires an evidence object")?;
    require_exact_args(
        evidence,
        &format!("record_agent_delivery {state} evidence"),
        fields,
    )?;
    Ok(evidence)
}

pub(super) fn evidence_string(
    evidence: &Value,
    field: &str,
    state: &str,
) -> Result<String, String> {
    arg_str(evidence, field)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!("record_agent_delivery {state} requires a non-empty evidence.{field}")
        })
}

pub(super) fn enforce_recorded_integration_contract(
    target_agent: &AgentSessionRecord,
    manifest: &crate::agent_session::IntegrationManifest,
    actor_identity: &str,
) -> Result<(), String> {
    if target_agent.integration_contracts.is_empty() {
        return Ok(());
    }
    let mut contract_ids = BTreeSet::new();
    for contract in &target_agent.integration_contracts {
        let unique_lanes = contract.ordered_lane_ids.iter().collect::<BTreeSet<_>>();
        if contract.contract_id.trim().is_empty()
            || contract.integration_owner.trim().is_empty()
            || contract.ordered_lane_ids.len() < 2
            || unique_lanes.len() != contract.ordered_lane_ids.len()
            || !contract_ids.insert(contract.contract_id.as_str())
        {
            return Err(
                "record_agent_delivery integrated durable integration contracts are invalid or ambiguous"
                    .into(),
            );
        }
    }

    let manifest_lane_ids = manifest
        .inputs
        .iter()
        .map(|input| input.lane_id.as_str())
        .collect::<Vec<_>>();
    let matching = target_agent
        .integration_contracts
        .iter()
        .filter(|contract| {
            contract
                .ordered_lane_ids
                .iter()
                .map(String::as_str)
                .eq(manifest_lane_ids.iter().copied())
        })
        .collect::<Vec<_>>();
    let contract = match matching.as_slice() {
        [contract] => *contract,
        [] => {
            return Err(
                "record_agent_delivery integrated manifest lane order must exactly match one durable integration contract"
                    .into(),
            )
        }
        _ => {
            return Err(
                "record_agent_delivery integrated manifest matches multiple durable integration contracts and is ambiguous"
                    .into(),
            )
        }
    };
    let target_lane_id = target_agent
        .lane_claim
        .as_ref()
        .map(|lane| lane.lane_id.as_str())
        .ok_or(
            "record_agent_delivery integrated target has contracts without a durable lane claim",
        )?;
    if !contract
        .ordered_lane_ids
        .iter()
        .any(|lane_id| lane_id == target_lane_id)
    {
        return Err(
            "record_agent_delivery integrated contract does not include the target agent lane"
                .into(),
        );
    }
    if contract.integration_owner != actor_identity {
        return Err(format!(
            "record_agent_delivery integrated contract '{}' designates integration owner '{}', not authenticated actor '{}'",
            contract.contract_id, contract.integration_owner, actor_identity
        ));
    }
    Ok(())
}

pub(super) fn validate_registered_integration_inputs(
    ctx: &ControlContext,
    target_agent: &AgentSessionRecord,
    manifest: &crate::agent_session::IntegrationManifest,
    source_commit: &str,
    canonical_baseline: &str,
    canonical_commit: &str,
) -> Result<(), String> {
    let snapshot = ctx.captains.snapshot();
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.project_id == target_agent.project_id)
        .ok_or_else(|| {
            format!(
                "record_agent_delivery integrated Project '{}' is not registered",
                target_agent.project_id
            )
        })?;
    let repo_root = files::posix_form(&project.repo_root);
    for input in &manifest.inputs {
        let registered = snapshot
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == input.agent_session_id)
            .ok_or_else(|| {
                format!(
                    "record_agent_delivery integrated manifest agentSessionId '{}' is not registered",
                    input.agent_session_id
                )
            })?;
        if registered.project_id != target_agent.project_id {
            return Err(format!(
                "record_agent_delivery integrated manifest agentSessionId '{}' belongs to a different project",
                input.agent_session_id
            ));
        }
        if registered
            .lane_claim
            .as_ref()
            .map(|lane| lane.lane_id.as_str())
            != Some(input.lane_id.as_str())
        {
            return Err(format!(
                "record_agent_delivery integrated manifest laneId '{}' does not match agentSessionId '{}'",
                input.lane_id, input.agent_session_id
            ));
        }
        let delivery = registered.delivery.as_ref().ok_or_else(|| {
            format!(
                "record_agent_delivery integrated manifest agentSessionId '{}' has no delivery provenance",
                input.agent_session_id
            )
        })?;
        if delivery.source_baseline != input.source_baseline
            || delivery.resulting_commit.as_deref() != Some(input.resulting_commit.as_str())
        {
            return Err(format!(
                "record_agent_delivery integrated manifest commits do not match agentSessionId '{}'",
                input.agent_session_id
            ));
        }
        if !delivery.states().complete {
            return Err(format!(
                "record_agent_delivery integrated manifest agentSessionId '{}' is not complete",
                input.agent_session_id
            ));
        }
    }
    git::require_exact_local_branch_tip(&repo_root, canonical_baseline, canonical_commit).map_err(
        |error| format!("record_agent_delivery integrated canonical baseline rejected: {error}"),
    )?;
    git::require_commit_ancestor(&repo_root, source_commit, canonical_commit).map_err(|error| {
        format!(
            "record_agent_delivery integrated sourceCommit '{source_commit}' is not incorporated by canonicalCommit '{canonical_commit}': {error}"
        )
    })?;
    for input in &manifest.inputs {
        git::require_commit_ancestor(
            &repo_root,
            &input.source_baseline,
            &input.resulting_commit,
        )
        .map_err(|error| {
            format!(
                "record_agent_delivery integrated manifest laneId '{}' resultingCommit does not descend from sourceBaseline: {error}",
                input.lane_id
            )
        })?;
        git::require_commit_ancestor(&repo_root, &input.resulting_commit, canonical_commit)
            .map_err(|error| {
                format!(
                    "record_agent_delivery integrated manifest laneId '{}' is not incorporated by canonicalCommit '{}': {error}",
                    input.lane_id, canonical_commit
                )
            })?;
    }
    Ok(())
}

pub(super) fn record_agent_delivery(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "record_agent_delivery",
        &["agentSessionId", "state", "evidence"],
    )?;
    let agent_session_id = arg_str(args, "agentSessionId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("record_agent_delivery requires a non-empty 'agentSessionId'")?;
    let state = arg_str(args, "state")
        .filter(|value| !value.trim().is_empty())
        .ok_or("record_agent_delivery requires a non-empty 'state'")?;
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .ok_or_else(|| {
            format!("record_agent_delivery: agent '{agent_session_id}' was not found")
        })?;
    let project_root = ctx
        .captains
        .projects()
        .into_iter()
        .find(|project| project.project_id == agent.project_id)
        .ok_or_else(|| {
            format!(
                "record_agent_delivery: Project '{}' was not found",
                agent.project_id
            )
        })?
        .repo_root;
    let authority = authorize_agent(
        ctx,
        &agent,
        caller,
        trusted_internal,
        "record_agent_delivery",
    )?;
    let gate_operation = if state == "integrated" {
        "integration"
    } else {
        "delivery"
    };
    require_registered_git_capability(ctx, gate_operation, &project_root)?;
    if authority == AgentAuthority::Agent && !matches!(state.as_str(), "implemented" | "tested") {
        return Err(format!(
            "acl: an implementing agent may record only implemented or tested evidence, not '{state}'"
        ));
    }
    let actor_identity = caller
        .map(|identity| identity.session_id.clone())
        .unwrap_or_else(|| "trusted-host".into());
    let recorded_at = now_ms();
    let update = match state.as_str() {
        "implemented" => {
            let evidence = required_delivery_evidence(args, &state, &["commit"])?;
            AgentDeliveryUpdate::Implemented(evidence_string(evidence, "commit", &state)?)
        }
        "reviewed" => {
            let evidence = required_delivery_evidence(args, &state, &["commit", "reference"])?;
            AgentDeliveryUpdate::Reviewed(crate::agent_session::ReviewEvidence {
                commit: evidence_string(evidence, "commit", &state)?,
                reviewer_identity: actor_identity,
                reference: evidence_string(evidence, "reference", &state)?,
                recorded_at,
            })
        }
        "tested" => {
            let evidence = required_delivery_evidence(
                args,
                &state,
                &["commit", "reference", "environment"],
            )?;
            let environment = serde_json::from_value::<crate::agent_session::AcceptanceEnvironment>(
                evidence
                    .get("environment")
                    .cloned()
                    .ok_or("record_agent_delivery tested requires evidence.environment")?,
            )
            .map_err(|error| {
                format!("record_agent_delivery tested environment is invalid: {error}")
            })?;
            AgentDeliveryUpdate::Tested(crate::agent_session::AcceptanceTestEvidence {
                commit: evidence_string(evidence, "commit", &state)?,
                runner_identity: actor_identity,
                reference: evidence_string(evidence, "reference", &state)?,
                environment,
                recorded_at,
            })
        }
        "integrated" => {
            let evidence = required_delivery_evidence(
                args,
                &state,
                &[
                    "sourceCommit",
                    "canonicalBaseline",
                    "canonicalCommit",
                    "reference",
                    "manifest",
                ],
            )?;
            let manifest = serde_json::from_value::<crate::agent_session::IntegrationManifest>(
                evidence
                    .get("manifest")
                    .cloned()
                    .ok_or("record_agent_delivery integrated requires evidence.manifest")?,
            )
            .map_err(|error| {
                format!("record_agent_delivery integrated manifest is invalid: {error}")
            })?;
            let source_commit = evidence_string(evidence, "sourceCommit", &state)?;
            let canonical_baseline = evidence_string(evidence, "canonicalBaseline", &state)?;
            let canonical_commit = evidence_string(evidence, "canonicalCommit", &state)?;
            manifest.validate_for_source_commit(&source_commit)?;
            if manifest.integration_owner_identity != actor_identity {
                return Err(
                    "record_agent_delivery integrated manifest.integrationOwnerIdentity must equal the authenticated actor identity"
                        .into(),
                );
            }
            enforce_recorded_integration_contract(&agent, &manifest, &actor_identity)?;
            validate_registered_integration_inputs(
                ctx,
                &agent,
                &manifest,
                &source_commit,
                &canonical_baseline,
                &canonical_commit,
            )?;
            AgentDeliveryUpdate::Integrated(crate::agent_session::IntegrationEvidence {
                source_commit,
                canonical_baseline,
                canonical_commit,
                reference: evidence_string(evidence, "reference", &state)?,
                recorded_at,
                manifest: Some(manifest),
            })
        }
        "packaged" => {
            let evidence = required_delivery_evidence(
                args,
                &state,
                &["artifactId", "sourceBaseline", "reference", "manifest"],
            )?;
            let manifest = serde_json::from_value::<crate::agent_session::ArtifactManifest>(
                evidence
                    .get("manifest")
                    .cloned()
                    .ok_or("record_agent_delivery packaged requires evidence.manifest")?,
            )
            .map_err(|error| {
                format!("record_agent_delivery packaged manifest is invalid: {error}")
            })?;
            AgentDeliveryUpdate::Packaged(crate::agent_session::ArtifactEvidence {
                artifact_id: evidence_string(evidence, "artifactId", &state)?,
                source_baseline: evidence_string(evidence, "sourceBaseline", &state)?,
                reference: evidence_string(evidence, "reference", &state)?,
                recorded_at,
                manifest: Some(manifest),
            })
        }
        "installed" => {
            let evidence = required_delivery_evidence(
                args,
                &state,
                &["artifactId", "target", "reference"],
            )?;
            AgentDeliveryUpdate::Installed(crate::agent_session::InstallationEvidence {
                artifact_id: evidence_string(evidence, "artifactId", &state)?,
                target: evidence_string(evidence, "target", &state)?,
                reference: evidence_string(evidence, "reference", &state)?,
                recorded_at,
            })
        }
        "liveVerified" => {
            let evidence = required_delivery_evidence(
                args,
                &state,
                &["artifactId", "target", "verifierKind", "reference"],
            )?;
            let verifier_kind = serde_json::from_value::<crate::agent_session::VerifierKind>(
                evidence
                    .get("verifierKind")
                    .cloned()
                    .ok_or("record_agent_delivery liveVerified requires evidence.verifierKind")?,
            )
            .map_err(|error| {
                format!("record_agent_delivery liveVerified verifierKind is invalid: {error}")
            })?;
            AgentDeliveryUpdate::LiveVerified(crate::agent_session::LiveVerificationEvidence {
                artifact_id: evidence_string(evidence, "artifactId", &state)?,
                target: evidence_string(evidence, "target", &state)?,
                verifier_identity: actor_identity,
                verifier_kind,
                reference: evidence_string(evidence, "reference", &state)?,
                recorded_at,
            })
        }
        _ => {
            return Err(
                "record_agent_delivery state must be implemented, reviewed, tested, integrated, packaged, installed, or liveVerified"
                    .into(),
            )
        }
    };
    let agent = ctx
        .captains
        .record_agent_delivery(&agent_session_id, update)?;
    Ok(json!({
        "accepted": "record_agent_delivery",
        "state": state,
        "agent": agent_status_value(agent.clone(), true),
        "deliveryStates": agent.delivery_states(),
        "audited": true,
    }))
}

pub(super) fn agent_events(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(args, "agent_events", &["agentSessionId", "cursor", "limit"])?;
    let agent_session_id = arg_str(args, "agentSessionId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("agent_events requires a non-empty 'agentSessionId'")?;
    let after = agent_page_cursor(args, "agent_events")? as u64;
    let limit = agent_page_limit(args, "agent_events")?;
    let snapshot = ctx.captains.snapshot();
    let Some(agent) = snapshot
        .agent_sessions
        .iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
    else {
        return Err(format!(
            "agent_events: agent '{agent_session_id}' was not found"
        ));
    };
    authorize_agent(ctx, agent, caller, trusted_internal, "agent_events")?;
    let event_cursor = snapshot
        .agent_events
        .iter()
        .filter(|event| event.agent_session_id == agent_session_id)
        .map(|event| event.cursor)
        .max()
        .unwrap_or(after);
    let available: Vec<_> = snapshot
        .agent_events
        .into_iter()
        .filter(|event| event.agent_session_id == agent_session_id && event.cursor > after)
        .collect();
    let events: Vec<_> = available.iter().take(limit).cloned().collect();
    let count = events.len();
    let next_cursor = events.last().map(|event| event.cursor).unwrap_or(after);
    let oldest_cursor = available.first().map(|event| event.cursor).unwrap_or(after);
    Ok(json!({
        "events": events,
        "count": count,
        "cursor": after.to_string(),
        "nextCursor": next_cursor.to_string(),
        "eventCursor": event_cursor,
        "cursorExpired": after.saturating_add(1) < oldest_cursor,
        "hasMore": available.len() > count,
    }))
}
