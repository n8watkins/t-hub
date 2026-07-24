//! History-tier control handlers (provider-neutral conversation catalog plus
//! focus/resume), split out of `control.rs`. The parent dispatch match and a
//! few callers route here; `use super::*;` pulls in the parent's items.

use super::*;

/// `history_list` (organization tier): provider-neutral, exact-identity conversation
/// catalog. Provider transcripts remain read-only evidence; durable registry
/// metadata joins only on Harness plus an exact native conversation identity.
pub(super) fn history_list(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "history_list")?;
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "history_list")?;
    let filter = if args.is_null() {
        crate::history::HistoryFilter::default()
    } else {
        serde_json::from_value::<crate::history::HistoryFilter>(args.clone())
            .map_err(|error| format!("history_invalid_filter: {error}"))?
    };
    let evidence = history_associations(ctx);
    let mut list = match &authority {
        WorkspaceMutationAuthority::Assignment(owner) => ctx.history.list_for_assignment(
            &filter,
            &evidence.associations,
            &owner.ship_slug,
            &owner.project_id,
            &owner.assignment_id,
        )?,
        WorkspaceMutationAuthority::Apex => ctx.history.list(&filter, &evidence.associations)?,
    };
    if evidence.claude_runtime_uncertain {
        crate::history::degrade_runtime_evidence(
            &mut list,
            crate::history::Harness::Claude,
            "Claude active-runtime discovery was unavailable; resume is fail-closed.",
        )?;
    }
    if evidence.codex_runtime_uncertain {
        crate::history::degrade_runtime_evidence(
            &mut list,
            crate::history::Harness::Codex,
            "Codex active-runtime discovery was unavailable; resume is fail-closed.",
        )?;
    }
    if ctx.history.durable_state_error().is_some() {
        for harness in [
            crate::history::Harness::Claude,
            crate::history::Harness::Codex,
        ] {
            crate::history::degrade_runtime_evidence(
                &mut list,
                harness,
                "Durable History resume state is unavailable; resume is fail-closed.",
            )?;
        }
    }
    serde_json::to_value(list).map_err(|error| error.to_string())
}

/// Drop only T-Hub's in-memory History scan cache. Provider transcripts and
/// registry records are never changed.
pub(super) fn invalidate_history_cache(ctx: &ControlContext) -> Result<Value, String> {
    notify_history_changed(ctx, "cache-invalidated");
    Ok(Value::Bool(true))
}

pub(super) fn notify_history_changed(ctx: &ControlContext, reason: &str) {
    ctx.history.invalidate();
    ctx.fanout.emit_event(
        "history://changed",
        &json!({ "reason": reason, "at": now_ms() }),
    );
}

pub(super) fn history_harness(
    provider: Option<&str>,
    harness: Option<&str>,
    claude_uuid: Option<&str>,
) -> Option<crate::history::Harness> {
    match provider.or(harness).map(str::trim) {
        Some("claude") => Some(crate::history::Harness::Claude),
        Some("codex") => Some(crate::history::Harness::Codex),
        Some(_) => None,
        None if claude_uuid.is_some() => Some(crate::history::Harness::Claude),
        None => None,
    }
}

pub(super) fn history_identity_values(
    harness: crate::history::Harness,
    provider_session_id: Option<&str>,
    conversation_id: Option<&str>,
    claude_uuid: Option<&str>,
) -> Vec<String> {
    let mut identities = std::collections::BTreeSet::new();
    for value in [provider_session_id, conversation_id] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            identities.insert(value.to_string());
        }
    }
    if harness == crate::history::Harness::Claude {
        if let Some(value) = claude_uuid.map(str::trim).filter(|value| !value.is_empty()) {
            identities.insert(value.to_string());
        }
    }
    identities.into_iter().collect()
}

pub(super) fn history_registry_liveness(
    expected_active: bool,
    terminal_id: Option<&str>,
    live_sessions: &Result<std::collections::HashSet<String>, String>,
) -> crate::history::AssociationLiveness {
    if !expected_active || terminal_id.is_none() {
        return crate::history::AssociationLiveness::Inactive;
    }
    match live_sessions {
        Ok(live) => {
            if live.contains(&tmux_target(terminal_id.expect("checked above"))) {
                // A surviving tmux shell does not prove that the recorded Harness
                // still owns the pane. Exact Claude/Codex runtime evidence below
                // must promote this association to Active.
                crate::history::AssociationLiveness::Unknown
            } else {
                crate::history::AssociationLiveness::Inactive
            }
        }
        Err(_) => crate::history::AssociationLiveness::Unknown,
    }
}

pub(super) fn merge_history_association(
    associations: &mut Vec<crate::history::HistoryAssociation>,
    candidate: crate::history::HistoryAssociation,
) {
    let Some(existing_index) = associations.iter().position(|association| {
        association.harness == candidate.harness
            && association.conversation_id == candidate.conversation_id
            && association.terminal_id == candidate.terminal_id
    }) else {
        associations.push(candidate);
        return;
    };
    let mut merged = associations[existing_index].clone();
    for (current, next) in [
        (&mut merged.project_id, candidate.project_id.clone()),
        (&mut merged.project_name, candidate.project_name.clone()),
        (&mut merged.captain_id, candidate.captain_id.clone()),
        (&mut merged.assignment_id, candidate.assignment_id.clone()),
        (&mut merged.role, candidate.role.clone()),
        (&mut merged.workspace_id, candidate.workspace_id.clone()),
        (&mut merged.worktree_id, candidate.worktree_id.clone()),
        (&mut merged.branch, candidate.branch.clone()),
    ] {
        match (current.as_ref(), next) {
            (Some(left), Some(right)) if left != &right => {
                associations.push(candidate);
                return;
            }
            (None, Some(value)) => *current = Some(value),
            _ => {}
        }
    }
    merged.liveness = match (merged.liveness, candidate.liveness) {
        (crate::history::AssociationLiveness::Active, _)
        | (_, crate::history::AssociationLiveness::Active) => {
            crate::history::AssociationLiveness::Active
        }
        (crate::history::AssociationLiveness::Unknown, _)
        | (_, crate::history::AssociationLiveness::Unknown) => {
            crate::history::AssociationLiveness::Unknown
        }
        _ => crate::history::AssociationLiveness::Inactive,
    };
    associations[existing_index] = merged;
}

/// Build exact active and durable joins without using cwd as identity.
///
/// Registry associations contribute organizational metadata. Runtime evidence
/// covers ordinary non-Crew tiles: Claude's status bridge provides the exact UUID,
/// while Codex exposes its exact open rollout through one bounded process scan.
pub(super) struct HistoryAssociationEvidence {
    associations: Vec<crate::history::HistoryAssociation>,
    claude_runtime_uncertain: bool,
    codex_runtime_uncertain: bool,
}

pub(super) fn history_associations(ctx: &ControlContext) -> HistoryAssociationEvidence {
    let live_sessions = tmux::list_sessions()
        .map(|sessions| {
            sessions
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        })
        .map_err(|error| error.to_string());
    let snapshot = ctx.captains.snapshot();
    let project_names = snapshot
        .projects
        .iter()
        .map(|project| (project.project_id.as_str(), project.name.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let live_sessions_uncertain = live_sessions.is_err();
    let mut associations = Vec::new();
    for captain in &snapshot.captains {
        let harness = history_harness(
            captain.provider.as_deref(),
            captain.harness.as_deref(),
            captain.claude_uuid.as_deref(),
        );
        if let Some(harness) = harness {
            let liveness = history_registry_liveness(
                matches!(captain.state, ClaimState::Active),
                captain.terminal_id.as_deref(),
                &live_sessions,
            );
            for conversation_id in history_identity_values(
                harness,
                captain.provider_session_id.as_deref(),
                captain.conversation_id.as_deref(),
                captain.claude_uuid.as_deref(),
            ) {
                merge_history_association(
                    &mut associations,
                    crate::history::HistoryAssociation {
                        harness,
                        conversation_id,
                        terminal_id: captain.terminal_id.clone(),
                        liveness,
                        project_id: captain.project_id.clone(),
                        project_name: captain
                            .project_id
                            .as_deref()
                            .and_then(|id| project_names.get(id).copied())
                            .map(str::to_string),
                        captain_id: Some(captain.ship_slug.clone()),
                        assignment_id: Some(captain.assignment_id.clone()),
                        role: Some(captain.role.label().to_string()),
                        workspace_id: (captain.workspace_tab_ids.len() == 1)
                            .then(|| captain.workspace_tab_ids[0].clone()),
                        worktree_id: None,
                        branch: None,
                    },
                );
            }
        }
        for crew in &captain.crew {
            let Some(harness) = history_harness(
                crew.provider.as_deref(),
                crew.harness.as_deref(),
                crew.claude_uuid.as_deref(),
            ) else {
                continue;
            };
            let liveness = history_registry_liveness(
                matches!(crew.state, CrewState::Active),
                Some(&crew.terminal_id),
                &live_sessions,
            );
            for conversation_id in history_identity_values(
                harness,
                crew.provider_session_id.as_deref(),
                crew.conversation_id.as_deref(),
                crew.claude_uuid.as_deref(),
            ) {
                merge_history_association(
                    &mut associations,
                    crate::history::HistoryAssociation {
                        harness,
                        conversation_id,
                        terminal_id: Some(crew.terminal_id.clone()),
                        liveness,
                        project_id: captain.project_id.clone(),
                        project_name: captain
                            .project_id
                            .as_deref()
                            .and_then(|id| project_names.get(id).copied())
                            .map(str::to_string),
                        captain_id: Some(captain.ship_slug.clone()),
                        assignment_id: Some(captain.assignment_id.clone()),
                        role: Some("crew".to_string()),
                        workspace_id: None,
                        worktree_id: None,
                        branch: crew.branch.clone(),
                    },
                );
            }
        }
    }

    // Durable agent-session records outlive legacy Crew membership removal.
    // Joining them keeps a closed conversation associated with its Project,
    // Captain, Workspace, and worktree so an authorized History resume remains
    // possible after the terminal has been reaped.
    for agent in &snapshot.agent_sessions {
        let Some(conversation_id) = agent
            .provider_conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|identity| !identity.is_empty())
        else {
            continue;
        };
        let Some(harness) = history_harness(Some(&agent.provider), Some(&agent.harness), None)
        else {
            continue;
        };
        let exact_captain = snapshot.captains.iter().find(|captain| {
            captain.terminal_id.as_deref() == Some(agent.captain_session_id.as_str())
                && captain.project_id.as_deref() == Some(agent.project_id.as_str())
        });
        let project_captains = snapshot
            .captains
            .iter()
            .filter(|captain| captain.project_id.as_deref() == Some(agent.project_id.as_str()))
            .collect::<Vec<_>>();
        let captain =
            exact_captain.or_else(|| (project_captains.len() == 1).then_some(project_captains[0]));
        let expected_active = !matches!(
            agent.runtime_state,
            crate::agent_session::RuntimeState::Exited
                | crate::agent_session::RuntimeState::Unavailable
        );
        associations.retain(|association| {
            !(association.harness == harness
                && association.conversation_id == conversation_id
                && association.terminal_id.as_deref() == Some(agent.agent_session_id.as_str()))
        });
        merge_history_association(
            &mut associations,
            crate::history::HistoryAssociation {
                harness,
                conversation_id: conversation_id.to_string(),
                terminal_id: Some(agent.agent_session_id.clone()),
                liveness: history_registry_liveness(
                    expected_active,
                    Some(&agent.agent_session_id),
                    &live_sessions,
                ),
                project_id: Some(agent.project_id.clone()),
                project_name: project_names
                    .get(agent.project_id.as_str())
                    .copied()
                    .map(str::to_string),
                captain_id: captain.map(|captain| captain.ship_slug.clone()),
                assignment_id: captain.map(|captain| captain.assignment_id.clone()),
                role: Some("crew".to_string()),
                workspace_id: agent.workspace_tab_id.clone(),
                // A filesystem path is useful context but is not an authoritative
                // durable worktree identity. Preserve branch/workspace metadata and
                // leave this field unset until the registry carries a real ID.
                worktree_id: None,
                branch: agent.branch.clone(),
            },
        );
    }

    let resume_operations = ctx.history.resume_operations();
    for binding in ctx.history.bindings() {
        let operation = resume_operations
            .iter()
            .filter(|operation| {
                operation.history_id == binding.history_id
                    && operation.harness == binding.harness
                    && operation.conversation_id == binding.conversation_id
                    && operation.terminal_id == binding.terminal_id
            })
            .max_by_key(|operation| operation.recorded_at_ms);
        // The latest durable resume binding is authoritative for this conversation.
        // It supersedes inactive registry records from the pre-resume terminal.
        associations.retain(|association| {
            !(association.harness == binding.harness
                && association.conversation_id == binding.conversation_id)
        });
        associations.push(crate::history::HistoryAssociation {
            harness: binding.harness,
            conversation_id: binding.conversation_id,
            terminal_id: Some(binding.terminal_id.clone()),
            liveness: match tmux::session_liveness(&tmux_target(&binding.terminal_id)) {
                // A live pane or matching process name is not exact conversation
                // proof. The runtime scan below performs the only Active promotion.
                tmux::SessionLiveness::Alive => crate::history::AssociationLiveness::Unknown,
                tmux::SessionLiveness::Gone => crate::history::AssociationLiveness::Inactive,
                tmux::SessionLiveness::Unknown => crate::history::AssociationLiveness::Unknown,
            },
            project_id: operation.and_then(|operation| operation.authorized_project_id.clone()),
            project_name: operation
                .and_then(|operation| operation.authorized_project_id.as_deref())
                .and_then(|project_id| project_names.get(project_id).copied())
                .map(str::to_string),
            captain_id: operation.and_then(|operation| operation.authorized_ship_slug.clone()),
            assignment_id: operation
                .and_then(|operation| operation.authorized_assignment_id.clone()),
            role: operation
                .and_then(|operation| operation.authorized_ship_slug.as_ref())
                .map(|_| "resumed".to_string()),
            workspace_id: operation.and_then(|operation| operation.actual_tab_id.clone()),
            worktree_id: None,
            branch: None,
        });
    }

    let claude_panes = tmux::pane_info();
    let mut claude_runtime_uncertain = live_sessions_uncertain || claude_panes.is_err();
    let mut runtime = Vec::<(crate::history::Harness, String, String)>::new();
    if let (Ok(live), Ok(panes)) = (&live_sessions, &claude_panes) {
        for status in ctx.status.all() {
            let Some(tmux_session) = status.tmux_session.as_deref() else {
                continue;
            };
            if live.contains(tmux_session) {
                match panes.iter().find(|pane| pane.session == tmux_session) {
                    Some(pane) if pane.command.eq_ignore_ascii_case("claude") => runtime.push((
                        crate::history::Harness::Claude,
                        status.session_id,
                        tmux_session
                            .strip_prefix("th_")
                            .unwrap_or(tmux_session)
                            .to_string(),
                    )),
                    Some(pane)
                        if matches!(
                            pane.command.trim().to_ascii_lowercase().as_str(),
                            "bash" | "cmd" | "fish" | "nu" | "powershell" | "pwsh" | "sh" | "zsh"
                        ) => {}
                    Some(_) | None => claude_runtime_uncertain = true,
                }
            }
        }
    }
    let codex_rollouts = tmux::active_codex_rollouts();
    let mut codex_runtime_uncertain = codex_rollouts.is_err();
    if let Ok(rollouts) = &codex_rollouts {
        codex_runtime_uncertain |= rollouts.len() > crate::history::HISTORY_ENTRY_LIMIT;
        for rollout in rollouts.iter().take(crate::history::HISTORY_ENTRY_LIMIT) {
            match crate::history::codex_conversation_id_from_path(std::path::Path::new(
                &rollout.path,
            )) {
                Ok(conversation_id) => runtime.push((
                    crate::history::Harness::Codex,
                    conversation_id,
                    rollout.terminal_id.clone(),
                )),
                Err(_) => codex_runtime_uncertain = true,
            }
        }
    }
    runtime.sort();
    runtime.dedup();
    for (harness, conversation_id, terminal_id) in runtime {
        if let Some(existing) = associations.iter_mut().find(|association| {
            association.harness == harness
                && association.conversation_id == conversation_id
                && association.terminal_id.as_deref() == Some(terminal_id.as_str())
        }) {
            existing.liveness = crate::history::AssociationLiveness::Active;
            continue;
        }
        associations.push(crate::history::HistoryAssociation {
            harness,
            conversation_id,
            terminal_id: Some(terminal_id),
            liveness: crate::history::AssociationLiveness::Active,
            project_id: None,
            project_name: None,
            captain_id: None,
            assignment_id: None,
            role: None,
            workspace_id: None,
            worktree_id: None,
            branch: None,
        });
    }
    associations.sort_by(|left, right| {
        left.harness
            .cmp(&right.harness)
            .then_with(|| left.conversation_id.cmp(&right.conversation_id))
            .then_with(|| left.terminal_id.cmp(&right.terminal_id))
    });
    associations.dedup();
    HistoryAssociationEvidence {
        associations,
        claude_runtime_uncertain,
        codex_runtime_uncertain: live_sessions_uncertain || codex_runtime_uncertain,
    }
}

pub(super) fn exact_history_runtime_liveness(
    ctx: &ControlContext,
    harness: crate::history::Harness,
    conversation_id: &str,
    terminal_id: &str,
) -> crate::history::AssociationLiveness {
    let evidence = history_associations(ctx);
    if evidence.associations.iter().any(|association| {
        association.harness == harness
            && association.conversation_id == conversation_id
            && association.terminal_id.as_deref() == Some(terminal_id)
            && association.liveness == crate::history::AssociationLiveness::Active
    }) {
        return crate::history::AssociationLiveness::Active;
    }
    if evidence.associations.iter().any(|association| {
        association.terminal_id.as_deref() == Some(terminal_id)
            && association.liveness == crate::history::AssociationLiveness::Active
            && (association.harness != harness || association.conversation_id != conversation_id)
    }) {
        return crate::history::AssociationLiveness::Inactive;
    }
    match tmux::session_liveness(&tmux_target(terminal_id)) {
        tmux::SessionLiveness::Gone => return crate::history::AssociationLiveness::Inactive,
        tmux::SessionLiveness::Unknown => return crate::history::AssociationLiveness::Unknown,
        tmux::SessionLiveness::Alive => {}
    }
    if tmux::harness_liveness(&tmux_target(terminal_id), harness.canonical())
        == tmux::SessionLiveness::Gone
    {
        return crate::history::AssociationLiveness::Inactive;
    }
    // Even with a healthy scan, a just-started Harness may not have published its
    // exact conversation identity yet. Only exact evidence above may say Active.
    crate::history::AssociationLiveness::Unknown
}

pub(super) fn history_entry(
    ctx: &ControlContext,
    args: &Value,
) -> Result<
    (
        crate::history::HistoryEntry,
        Vec<crate::history::HistoryAssociation>,
    ),
    String,
> {
    let history_id = arg_str(args, "historyId")
        .or_else(|| arg_str(args, "history_id"))
        .ok_or("history_invalid_request: historyId is required")?;
    let evidence = history_associations(ctx);
    let mut entry = ctx
        .history
        .find(&history_id, &evidence.associations)?
        .ok_or_else(|| "history_missing: History conversation was not found".to_string())?;
    let runtime_uncertain = match entry.harness {
        crate::history::Harness::Claude => evidence.claude_runtime_uncertain,
        crate::history::Harness::Codex => evidence.codex_runtime_uncertain,
    };
    if entry.continuity_state == crate::history::ContinuityState::Resumable
        && (runtime_uncertain || ctx.history.durable_state_error().is_some())
    {
        crate::history::mark_entry_recovery_required(
            &mut entry,
            "Active-runtime or durable resume evidence is unavailable.",
        );
    }
    Ok((entry, evidence.associations))
}

pub(super) fn exact_active_history_terminal(
    entry: &crate::history::HistoryEntry,
    associations: &[crate::history::HistoryAssociation],
) -> Result<String, String> {
    let terminals = associations
        .iter()
        .filter(|association| {
            association.harness == entry.harness
                && association.conversation_id == entry.conversation_id
                && association.liveness == crate::history::AssociationLiveness::Active
        })
        .filter_map(|association| association.terminal_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if terminals.len() != 1 {
        return Err(
            "history_unavailable: conversation has no unique authoritative live terminal"
                .to_string(),
        );
    }
    let terminal_id = terminals.into_iter().next().expect("checked one terminal");
    if tmux::harness_liveness(&tmux_target(&terminal_id), entry.harness.canonical())
        != tmux::SessionLiveness::Alive
    {
        return Err(
            "history_unavailable: the expected Harness is no longer verifiably active in the conversation terminal"
                .to_string(),
        );
    }
    Ok(terminal_id)
}

/// Focus an exact active History identity. The frontend cannot nominate a tile.
pub(super) fn history_focus(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "history_focus")?;
    let (entry, associations) = history_entry(ctx, args)?;
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "history_focus")?;
    enforce_history_entry_owner(&authority, &entry, &associations)?;
    if entry.continuity_state != crate::history::ContinuityState::Active
        || entry.actions.focus.status != crate::history::ActionStatus::Supported
    {
        return Err("history_unavailable: conversation is not active".to_string());
    }
    let terminal_id = exact_active_history_terminal(&entry, &associations)?;
    enforce_session_access(ctx, caller, trusted_internal, &terminal_id)?;
    let applied = organization_apply(ctx, "focus_session", &json!({ "sessionId": terminal_id }))?;
    let applied = applied
        .get("applied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !applied {
        return Err(
            "history_unavailable: the active conversation has no connected UI tile to focus".into(),
        );
    }
    Ok(json!({
        "accepted": "history_focus",
        "historyId": entry.history_id,
        "terminalId": terminal_id,
        "status": "focused",
        "applied": true,
    }))
}

pub(super) fn history_request_id(args: &Value) -> Result<String, String> {
    let request_id = arg_str(args, "requestId")
        .or_else(|| arg_str(args, "request_id"))
        .ok_or("history_invalid_request: requestId is required")?;
    let valid = !request_id.is_empty()
        && request_id.len() <= 128
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'));
    if !valid {
        return Err(
            "history_invalid_request: requestId must be 1-128 ASCII identifier characters"
                .to_string(),
        );
    }
    Ok(request_id)
}

pub(super) fn history_resume_command(entry: &crate::history::HistoryEntry) -> String {
    let harness = match entry.harness {
        crate::history::Harness::Claude => Harness::Claude,
        crate::history::Harness::Codex => Harness::Codex,
    };
    harness.adapter().resume_argv(&entry.conversation_id)
}

pub(super) fn history_resume_owner(
    authority: &WorkspaceMutationAuthority,
) -> (Option<String>, Option<String>, Option<String>) {
    match authority {
        WorkspaceMutationAuthority::Apex => (None, None, None),
        WorkspaceMutationAuthority::Assignment(owner) => (
            Some(owner.ship_slug.clone()),
            Some(owner.project_id.clone()),
            Some(owner.assignment_id.clone()),
        ),
    }
}

pub(super) fn enforce_history_resume_owner(
    authority: &WorkspaceMutationAuthority,
    ship_slug: Option<&str>,
    project_id: Option<&str>,
    assignment_id: Option<&str>,
) -> Result<(), String> {
    let WorkspaceMutationAuthority::Assignment(owner) = authority else {
        return Ok(());
    };
    if ship_slug == Some(owner.ship_slug.as_str())
        && project_id == Some(owner.project_id.as_str())
        && assignment_id == Some(owner.assignment_id.as_str())
    {
        Ok(())
    } else {
        Err(
            "acl: history_resume durable operation does not belong to the caller's current Project Assignment"
                .into(),
        )
    }
}

pub(super) fn enforce_history_entry_owner(
    authority: &WorkspaceMutationAuthority,
    entry: &crate::history::HistoryEntry,
    associations: &[crate::history::HistoryAssociation],
) -> Result<(), String> {
    let WorkspaceMutationAuthority::Assignment(owner) = authority else {
        return Ok(());
    };
    if associations.iter().any(|association| {
        association.harness == entry.harness
            && association.conversation_id == entry.conversation_id
            && association.captain_id.as_deref() == Some(owner.ship_slug.as_str())
            && association.project_id.as_deref() == Some(owner.project_id.as_str())
            && association.assignment_id.as_deref() == Some(owner.assignment_id.as_str())
    }) {
        Ok(())
    } else {
        Err(
            "acl: History conversation does not belong to the caller's current Project Assignment"
                .into(),
        )
    }
}

pub(super) fn authorize_history_request_status(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    ship_slug: Option<&str>,
    project_id: Option<&str>,
    assignment_id: Option<&str>,
) -> Result<(), String> {
    let authority =
        workspace_mutation_authority(ctx, caller, trusted_internal, "get_request_status")?;
    enforce_history_resume_owner(&authority, ship_slug, project_id, assignment_id)
}

pub(super) fn validate_history_resume_target(
    ctx: &ControlContext,
    authority: &WorkspaceMutationAuthority,
    target_tab: Option<&str>,
) -> Result<(), String> {
    if let Some(tab_id) = target_tab {
        if !ctx.tabs.has_tab(tab_id) {
            return Err(format!(
                "history_invalid_request: unknown targetTabId '{tab_id}'"
            ));
        }
        enforce_workspace_owner(ctx, authority, tab_id, "history_resume")
    } else if matches!(authority, WorkspaceMutationAuthority::Assignment(_)) {
        Err("history_invalid_request: a Captain must select an owned targetTabId".into())
    } else {
        Ok(())
    }
}

pub(super) fn mint_history_terminal_id(ctx: &ControlContext) -> Result<String, String> {
    for _ in 0..16 {
        let candidate = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if ctx.tabs.workspace_for_tile(&candidate).is_some() {
            continue;
        }
        match tmux::session_liveness(&tmux_target(&candidate)) {
            tmux::SessionLiveness::Gone => return Ok(candidate),
            tmux::SessionLiveness::Alive => continue,
            tmux::SessionLiveness::Unknown => {
                return Err(retryable_error(
                    "history_recovery_required: terminal identity availability is unknown",
                ));
            }
        }
    }
    Err("history_capacity: could not allocate a unique terminal identity".into())
}

pub(super) fn finish_history_resume(
    ctx: &ControlContext,
    pending: &crate::history::HistoryPendingResume,
    actual_tab_id: Option<String>,
    replayed: bool,
) -> Result<Value, String> {
    let binding = crate::history::HistoryBinding {
        history_id: pending.history_id.clone(),
        harness: pending.harness,
        conversation_id: pending.conversation_id.clone(),
        terminal_id: pending.terminal_id.clone(),
    };
    let operation = crate::history::HistoryResumeOperation {
        request_id: pending.request_id.clone(),
        history_id: pending.history_id.clone(),
        harness: pending.harness,
        conversation_id: pending.conversation_id.clone(),
        terminal_id: pending.terminal_id.clone(),
        target_tab_id: pending.target_tab_id.clone(),
        actual_tab_id: actual_tab_id.clone(),
        authorized_ship_slug: pending.authorized_ship_slug.clone(),
        authorized_project_id: pending.authorized_project_id.clone(),
        authorized_assignment_id: pending.authorized_assignment_id.clone(),
        recorded_at_ms: now_ms(),
    };
    ctx.history.record_resume(binding, operation)?;
    notify_history_changed(ctx, "conversation-resumed");
    Ok(json!({
        "accepted": "history_resume",
        "requestId": pending.request_id,
        "historyId": pending.history_id,
        "harness": pending.harness,
        "conversationId": pending.conversation_id,
        "terminalId": pending.terminal_id,
        "tabId": actual_tab_id,
        "status": "active",
        "replayed": replayed,
    }))
}

pub(super) const HISTORY_PENDING_RUNTIME_PROOF_GRACE_MS: u64 = 120_000;

pub(super) fn history_pending_runtime_proof_expired(
    pending: &crate::history::HistoryPendingResume,
) -> bool {
    now_ms().saturating_sub(pending.reserved_at_ms) >= HISTORY_PENDING_RUNTIME_PROOF_GRACE_MS
}

pub(super) fn reap_history_pending_terminal(
    ctx: &ControlContext,
    pending: &crate::history::HistoryPendingResume,
) -> Result<(), String> {
    if tmux::session_liveness(&tmux_target(&pending.terminal_id)) == tmux::SessionLiveness::Gone {
        return Ok(());
    }
    let close_error = close_terminal(ctx, &json!({ "sessionId": pending.terminal_id })).err();
    match tmux::session_liveness(&tmux_target(&pending.terminal_id)) {
        tmux::SessionLiveness::Gone => Ok(()),
        tmux::SessionLiveness::Alive | tmux::SessionLiveness::Unknown => {
            Err(retryable_error(format!(
                "history_recovery_required: could not reap the unproven reserved terminal{}",
                close_error
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default()
            )))
        }
    }
}

/// Resume one exact provider conversation. The backend owns cwd, Harness, native
/// identity, and executable command; callers provide only the opaque History ID,
/// stable request ID, and optional destination tab.
pub(super) fn history_resume(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "history_resume")?;
    let request_id = history_request_id(args)?;
    let history_id = arg_str(args, "historyId")
        .or_else(|| arg_str(args, "history_id"))
        .ok_or("history_invalid_request: historyId is required")?;
    let target_tab = arg_str(args, "targetTabId")
        .or_else(|| arg_str(args, "target_tab_id"))
        .or_else(|| arg_str(args, "tabId"));
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "history_resume")?;

    if let Some(operation) = ctx.history.resume_operation(&request_id)? {
        let target_matches = operation.target_tab_id == target_tab
            || (operation
                .target_tab_id
                .as_deref()
                .is_some_and(|tab_id| !ctx.tabs.has_tab(tab_id))
                && operation
                    .actual_tab_id
                    .as_deref()
                    .is_some_and(|tab_id| ctx.tabs.has_tab(tab_id)));
        if operation.history_id != history_id || !target_matches {
            return Err(
                "request_conflict: requestId is already bound to a different History resume".into(),
            );
        }
        enforce_history_resume_owner(
            &authority,
            operation.authorized_ship_slug.as_deref(),
            operation.authorized_project_id.as_deref(),
            operation.authorized_assignment_id.as_deref(),
        )?;
        return match exact_history_runtime_liveness(
            ctx,
            operation.harness,
            &operation.conversation_id,
            &operation.terminal_id,
        ) {
            crate::history::AssociationLiveness::Active => Ok(json!({
                "accepted": "history_resume",
                "requestId": request_id,
                "historyId": operation.history_id,
                "harness": operation.harness,
                "conversationId": operation.conversation_id,
                "terminalId": operation.terminal_id,
                "tabId": operation.actual_tab_id,
                "status": "active",
                "replayed": true,
            })),
            crate::history::AssociationLiveness::Inactive => Err(
                "history_previous_resume_closed: this request already completed and its terminal is closed or replaced; start a new resume request"
                    .into(),
            ),
            crate::history::AssociationLiveness::Unknown => Err(retryable_error(
                "history_recovery_required: previous resume terminal liveness is unavailable",
            )),
        };
    }

    if let Some(pending) = ctx.history.pending_resume(&request_id)? {
        let reserved_target_missing = pending
            .target_tab_id
            .as_deref()
            .is_some_and(|tab_id| !ctx.tabs.has_tab(tab_id));
        if pending.history_id != history_id
            || (pending.target_tab_id != target_tab && !reserved_target_missing)
        {
            return Err(
                "request_conflict: requestId is already bound to a different History resume".into(),
            );
        }
        enforce_history_resume_owner(
            &authority,
            pending.authorized_ship_slug.as_deref(),
            pending.authorized_project_id.as_deref(),
            pending.authorized_assignment_id.as_deref(),
        )?;
        if reserved_target_missing {
            let actual_tab_id = ctx.tabs.workspace_for_tile(&pending.terminal_id);
            let runtime = exact_history_runtime_liveness(
                ctx,
                pending.harness,
                &pending.conversation_id,
                &pending.terminal_id,
            );
            if runtime == crate::history::AssociationLiveness::Active {
                if let Some(actual_tab_id) = actual_tab_id.as_deref() {
                    if enforce_workspace_owner(ctx, &authority, actual_tab_id, "history_resume")
                        .is_ok()
                    {
                        return finish_history_resume(
                            ctx,
                            &pending,
                            Some(actual_tab_id.to_string()),
                            true,
                        )
                        .map_err(retryable_error);
                    }
                }
            }
            if runtime == crate::history::AssociationLiveness::Unknown
                && !history_pending_runtime_proof_expired(&pending)
            {
                return Err(retryable_error(
                    "history_recovery_required: the reserved target Workspace closed before exact runtime identity was available",
                ));
            }
            reap_history_pending_terminal(ctx, &pending)?;
            return match ctx.history.cancel_resume_reservation(&pending) {
                Ok(()) => Err(
                    "history_invalid_request: the reserved target Workspace was closed; start a new request with an existing targetTabId"
                        .into(),
                ),
                Err(error) => Err(retryable_error(error)),
            };
        }
        validate_history_resume_target(ctx, &authority, target_tab.as_deref())?;
        match exact_history_runtime_liveness(
            ctx,
            pending.harness,
            &pending.conversation_id,
            &pending.terminal_id,
        ) {
            crate::history::AssociationLiveness::Unknown
                if !history_pending_runtime_proof_expired(&pending) =>
            {
                return Err(retryable_error(
                    "history_recovery_required: reserved terminal exact runtime identity is not available yet",
                ));
            }
            crate::history::AssociationLiveness::Active => {
                let actual_tab_id =
                    ctx.tabs
                        .workspace_for_tile(&pending.terminal_id)
                        .or_else(|| match pending.target_tab_id.as_deref() {
                            Some(tab_id) => {
                                ctx.tabs.place_tile_exact(&pending.terminal_id, tab_id).ok()
                            }
                            None => ctx
                                .tabs
                                .place_tile_with_fallback(&pending.terminal_id, None),
                        });
                let Some(actual_tab_id) = actual_tab_id else {
                    if !history_pending_runtime_proof_expired(&pending) {
                        return Err(retryable_error(
                            "history_recovery_required: resumed terminal has no recoverable Workspace placement",
                        ));
                    }
                    reap_history_pending_terminal(ctx, &pending)?;
                    return match ctx.history.cancel_resume_reservation(&pending) {
                        Ok(()) => Err(
                            "history_placement_unavailable: the resumed terminal could not be placed; start a new request"
                                .into(),
                        ),
                        Err(error) => Err(retryable_error(error)),
                    };
                };
                enforce_workspace_owner(ctx, &authority, &actual_tab_id, "history_resume")?;
                return finish_history_resume(ctx, &pending, Some(actual_tab_id), true)
                    .map_err(retryable_error);
            }
            crate::history::AssociationLiveness::Unknown => {
                reap_history_pending_terminal(ctx, &pending)?;
                return match ctx.history.cancel_resume_reservation(&pending) {
                    Ok(()) => Err(
                        "history_runtime_unproven: the reserved terminal never produced exact conversation evidence; start a new request"
                            .into(),
                    ),
                    Err(error) => Err(retryable_error(error)),
                };
            }
            crate::history::AssociationLiveness::Inactive => {
                if !history_pending_runtime_proof_expired(&pending) {
                    return Err(retryable_error(
                        "history_resume_in_flight: the durable terminal reservation is awaiting runtime creation",
                    ));
                }
                // A fallback shell, closed pane, or replaced conversation cannot
                // satisfy an expired reservation. Reap it, then reuse the same
                // durable request and exact terminal identity for one clean
                // recovery launch below.
                reap_history_pending_terminal(ctx, &pending)?;
                if tmux::session_liveness(&tmux_target(&pending.terminal_id))
                    != tmux::SessionLiveness::Gone
                {
                    return Err(retryable_error(
                        "history_recovery_required: stale reserved terminal is not yet gone",
                    ));
                }
            }
        }
    }
    validate_history_resume_target(ctx, &authority, target_tab.as_deref())?;
    let (entry, associations) = history_entry(ctx, args)?;
    enforce_history_entry_owner(&authority, &entry, &associations)?;

    if entry.continuity_state != crate::history::ContinuityState::Resumable
        || entry.actions.resume.status != crate::history::ActionStatus::Supported
    {
        return Err("history_unavailable: conversation is not resumable".to_string());
    }
    let mut fresh_admission_lock = None;
    let mut fresh_capacity = None;
    let (pending, reserved_here) = match ctx.history.pending_resume(&request_id)? {
        Some(pending) => {
            if pending.history_id != entry.history_id
                || pending.harness != entry.harness
                || pending.conversation_id != entry.conversation_id
            {
                return Err(
                    "request_conflict: durable reservation does not match current History identity"
                        .into(),
                );
            }
            (pending, false)
        }
        None => {
            // Serialize creation of fresh durable intents with admission.  If
            // two resumes race, the second cannot become invisible pending
            // demand while the first is being admitted.
            let admission_lock = ctx
                .dispatch_admission
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let (authorized_ship_slug, authorized_project_id, authorized_assignment_id) =
                history_resume_owner(&authority);
            let pending = crate::history::HistoryPendingResume {
                request_id: request_id.clone(),
                history_id: entry.history_id.clone(),
                harness: entry.harness,
                conversation_id: entry.conversation_id.clone(),
                terminal_id: mint_history_terminal_id(ctx)?,
                target_tab_id: target_tab.clone(),
                authorized_ship_slug,
                authorized_project_id,
                authorized_assignment_id,
                reserved_at_ms: now_ms(),
            };
            if let Err(error) = ctx.history.reserve_resume(pending.clone()) {
                return Err(if error.starts_with("history_resume_in_flight:") {
                    retryable_error(error)
                } else {
                    error
                });
            }
            let capacity = match evaluate_spawn_capacity(
                ctx,
                &SpawnPurpose::Ordinary,
                1,
                Some(&pending.terminal_id),
            ) {
                Ok(capacity) => capacity,
                Err(refusal) => {
                    let _ = ctx.history.cancel_resume_reservation(&pending);
                    return Err(refusal.message);
                }
            };
            fresh_admission_lock = Some(admission_lock);
            fresh_capacity = Some(capacity);
            (pending, true)
        }
    };
    let _admission = if let Some(lock) = fresh_admission_lock {
        SpawnAdmissionGuard {
            _lock: lock,
            _capacity: fresh_capacity.expect("fresh capacity accompanies its admission lock"),
        }
    } else {
        match admit_spawn(ctx, SpawnPurpose::Ordinary, 1, Some(&pending.terminal_id)) {
            Ok(admission) => admission,
            Err(refusal) => {
                if reserved_here {
                    if let Err(cleanup) = ctx.history.cancel_resume_reservation(&pending) {
                        return Err(retryable_error(format!(
                            "{}; durable reservation cleanup also failed: {cleanup}",
                            refusal.message
                        )));
                    }
                }
                return Err(refusal.message);
            }
        }
    };
    let mut spawn_args = json!({
        "cwd": entry.cwd.clone(),
        "startupCommand": history_resume_command(&entry),
        "_providerHarness": entry.harness.canonical(),
    });
    if let Some(tab_id) = target_tab.as_deref() {
        spawn_args["tabId"] = json!(tab_id);
    }
    let spawned = match spawn_terminal_with_private_pane_command_and_id(
        ctx,
        &spawn_args,
        None,
        false,
        target_tab.is_some(),
        true,
        Some(&pending.terminal_id),
    ) {
        Ok(spawned) => spawned,
        Err(error) => {
            return match tmux::session_liveness(&tmux_target(&pending.terminal_id)) {
                tmux::SessionLiveness::Gone => {
                    match ctx.history.cancel_resume_reservation(&pending) {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(retryable_error(format!(
                            "{error}; durable reservation cleanup also failed: {cleanup}"
                        ))),
                    }
                }
                tmux::SessionLiveness::Alive | tmux::SessionLiveness::Unknown => Err(
                    retryable_error(format!(
                        "history_recovery_required: spawn outcome is ambiguous and remains durably reserved: {error}"
                    )),
                ),
            };
        }
    };
    let terminal_id = spawned
        .get("id")
        .and_then(Value::as_str)
        .ok_or("history_resume_failed: spawn returned no terminal identity")?
        .to_string();
    let actual_tab_id = spawned
        .get("tabId")
        .and_then(Value::as_str)
        .map(str::to_string);
    if terminal_id != pending.terminal_id {
        let _ = close_terminal(ctx, &json!({ "sessionId": terminal_id }));
        return Err(retryable_error(
            "history_recovery_required: spawn returned a terminal other than its durable reservation",
        ));
    }
    if actual_tab_id.is_none() {
        let rollback = close_terminal(ctx, &json!({ "sessionId": terminal_id.clone() }));
        let message = format!(
            "history_resume_failed: spawned terminal has no Workspace placement{}",
            rollback
                .err()
                .map(|rollback| format!("; spawned terminal rollback also failed: {rollback}"))
                .unwrap_or_default()
        );
        return match tmux::session_liveness(&tmux_target(&terminal_id)) {
            tmux::SessionLiveness::Gone => match ctx.history.cancel_resume_reservation(&pending) {
                Ok(()) => Err(message),
                Err(cleanup) => Err(retryable_error(format!(
                    "{message}; durable reservation cleanup also failed: {cleanup}"
                ))),
            },
            tmux::SessionLiveness::Alive | tmux::SessionLiveness::Unknown => {
                Err(retryable_error(message))
            }
        };
    }
    if let Err(error) = finish_history_resume(ctx, &pending, actual_tab_id.clone(), false) {
        let rollback = close_terminal(ctx, &json!({ "sessionId": terminal_id.clone() }));
        let message = format!(
            "{error}{}",
            rollback
                .err()
                .map(|rollback| format!("; spawned terminal rollback also failed: {rollback}"))
                .unwrap_or_default(),
        );
        return match tmux::session_liveness(&tmux_target(&terminal_id)) {
            tmux::SessionLiveness::Gone => match ctx.history.cancel_resume_reservation(&pending) {
                Ok(()) => Err(message),
                Err(cleanup) => Err(retryable_error(format!(
                    "{message}; durable reservation cleanup also failed: {cleanup}"
                ))),
            },
            tmux::SessionLiveness::Alive | tmux::SessionLiveness::Unknown => {
                Err(retryable_error(message))
            }
        };
    }
    Ok(json!({
        "accepted": "history_resume",
        "requestId": request_id,
        "historyId": entry.history_id,
        "harness": entry.harness,
        "conversationId": entry.conversation_id,
        "terminalId": terminal_id,
        "tabId": actual_tab_id,
        "status": "active",
        "replayed": false,
    }))
}
