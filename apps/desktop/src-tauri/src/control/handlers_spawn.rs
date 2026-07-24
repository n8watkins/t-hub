//! Terminal + agent spawn control handlers, split out of `control.rs` to shrink
//! that module. The spawn-capacity / provider-capacity evaluation cluster
//! (`admit_spawn`, `evaluate_spawn_capacity`, `provider_capacity_evidence`, the
//! dispatch-lane and provider-harness helpers) and the spawn handlers themselves
//! (`start_agent`, `spawn_terminal`, `spawn_terminal_with_private_pane_command*`,
//! `spawn_tmux_terminal*`, `spawn_managed_tmux_terminal_with_id`). The parent
//! dispatch match routes here.

use super::*;

/// `spawn_terminal` (Process-changing, PRD §11.2: confirmation required).
/// Headless-org: the SERVER spawns the tmux session (same id minting + pane wrap
/// as the Tauri `commands::spawn_terminal`), resolves the target tab against the
/// authoritative registry - `tabName` reuses-or-creates a tab WITHOUT switching
/// the user's active tab - places the tile there, and forwards the registry
/// snapshot for the UI (webview sink and/or socket subscribers) to render. The
/// real terminal id is therefore returned synchronously, and a hidden target tab
/// or a minimized window cannot lose the spawn or its placement. Refused only
/// when NO UI is connected at all (nothing would render the tile). Its MCP
/// description still carries the CONFIRMATION REQUIRED contract (the user-facing
/// gate). Args: `cwd`, `name`, `shell`, `startupCommand` (T-B), `tabName`,
/// `tabId` (all optional; `tabId` must exist, default placement is the user's
/// active tab).
///
/// `startupCommand` is the socket twin of the webview "+" presets' field: the
/// command runs inside an interactive login shell the pane execs back into
/// (`commands::pane_command`, the same wrap the Tauri spawn uses), which is what
/// the native client's resume flow rides (`claude --resume <id>`). SECURITY: it
/// is process-changing surface and deliberately stays INSIDE this command's
/// existing confirmation-gate tier — same audit, same remote-peer cwd allowlist,
/// no new ungated path (a caller with this tier could already run commands via
/// the equally-gated `send_text`).
pub(super) fn string_set_arg(
    args: &Value,
    key: &str,
    command: &str,
) -> Result<BTreeSet<String>, String> {
    args.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{command} requires a '{key}' array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{command} '{key}' entries must be strings"))
        })
        .collect()
}

pub(super) fn active_dispatch_lanes(
    snapshot: &CaptainsSnapshot,
    project_id: &str,
) -> Vec<crate::governor::LaneClaim> {
    snapshot
        .agent_sessions
        .iter()
        .filter(|agent| agent.project_id == project_id)
        .filter(|agent| agent_retains_lane_ownership(agent))
        .map(|agent| {
            let mut lane = agent
                .lane_claim
                .clone()
                .unwrap_or_else(|| crate::governor::LaneClaim {
                    lane_id: format!("legacy:{}", agent.agent_session_id),
                    owner_id: agent.agent_session_id.clone(),
                    dependencies: Some(BTreeSet::new()),
                    mutable_files: [agent.directory.clone()].into_iter().collect(),
                    mutable_schemas: BTreeSet::new(),
                    mutable_interfaces: BTreeSet::new(),
                });
            // The lane was admitted only after its dispatch dependencies were
            // satisfied. They are provenance now, not unresolved dependencies
            // for a later preflight.
            lane.dependencies = Some(BTreeSet::new());
            lane
        })
        .collect()
}

pub(super) fn agent_retains_lane_ownership(agent: &AgentSessionRecord) -> bool {
    agent.work_stage != crate::agent_session::WorkStage::Stopped
        && !agent
            .delivery_states()
            .is_some_and(|states| states.integrated)
}

pub(super) fn dispatch_machine_evidence(
    ctx: &ControlContext,
    live_sessions: usize,
) -> (bool, usize) {
    let metrics = ctx.metrics.as_ref().and_then(|fetch| fetch().ok());
    let Some(metrics) = metrics else {
        #[cfg(test)]
        return (true, ctx.governor.max_sessions());
        #[cfg(not(test))]
        return (false, live_sessions);
    };
    let cpu_count = usize::try_from(metrics.cpu_count).unwrap_or(1).max(1);
    let load_healthy = metrics.load_avg[0].is_finite()
        && metrics.load_avg[0] <= (cpu_count.saturating_mul(2)) as f32;
    let memory_known = metrics.mem_total_kib > 0;
    let memory_healthy = memory_known && metrics.mem_available_kib >= 512 * 1024;
    let memory_slots = if memory_known {
        usize::try_from(metrics.mem_available_kib / (512 * 1024)).unwrap_or(0)
    } else {
        ctx.governor.max_sessions()
    };
    let cpu_slots = cpu_count.saturating_mul(8);
    let additional_slots = memory_slots.min(cpu_slots).max(1);
    (
        load_healthy && memory_healthy,
        live_sessions
            .saturating_add(additional_slots)
            .min(crate::governor::HARD_SESSION_CEILING),
    )
}

pub(super) fn recorded_admin_harness(
    snapshot: &CaptainsSnapshot,
    terminal_id: &str,
) -> Option<String> {
    snapshot
        .agent_sessions
        .iter()
        .find(|agent| agent.agent_session_id == terminal_id)
        .map(|agent| agent.provider.clone())
        .or_else(|| {
            snapshot.captains.iter().find_map(|captain| {
                captain
                    .crew
                    .iter()
                    .find(|crew| crew.terminal_id == terminal_id)
                    .and_then(|crew| crew.harness.clone().or_else(|| crew.provider.clone()))
            })
        })
}

pub(super) fn live_admin_counts(
    ctx: &ControlContext,
    snapshot: &CaptainsSnapshot,
) -> (usize, BTreeMap<String, usize>) {
    let active_captain_ships = snapshot
        .captains
        .iter()
        .filter(|captain| captain.role == FleetRole::Captain && captain.state == ClaimState::Active)
        .map(|captain| captain.ship_slug.clone())
        .collect::<BTreeSet<_>>();
    let mut fleet_admin_actors = BTreeSet::new();
    let mut ship_admin_actors_by_scope = BTreeMap::<String, BTreeSet<String>>::new();
    for grant in ctx.delegated_admin.active_grants() {
        let actor = current_admin_actor(ctx, &grant);
        let supervisor = current_delegating_supervisor(ctx, &grant);
        if ctx
            .delegated_admin
            .validate_effective_grant(&grant, &actor, &supervisor)
            .is_err()
        {
            continue;
        }
        let Some(terminal_id) = actor.session_tile.as_deref() else {
            continue;
        };
        let Some(harness) = recorded_admin_harness(snapshot, terminal_id) else {
            continue;
        };
        if tmux::harness_liveness(&tmux_target(terminal_id), &harness)
            != tmux::SessionLiveness::Alive
        {
            continue;
        }
        match grant.role {
            crate::delegated_admin::DelegatedAdminRole::FleetAdmin => {
                fleet_admin_actors.insert(grant.actor_identity_id);
            }
            crate::delegated_admin::DelegatedAdminRole::ShipAdmin => {
                if let crate::delegated_admin::AdminScope::Ship { ship_slug } = grant.scope {
                    if active_captain_ships.contains(&ship_slug) {
                        ship_admin_actors_by_scope
                            .entry(ship_slug)
                            .or_default()
                            .insert(grant.actor_identity_id);
                    }
                }
            }
        }
    }
    (
        fleet_admin_actors.len(),
        ship_admin_actors_by_scope
            .into_iter()
            .map(|(scope, actors)| (scope, actors.len()))
            .collect(),
    )
}

pub(super) fn packaged_provider_capacity_evidence() -> Result<ProviderCapacityEvidence, String> {
    let policy: PackagedProviderCapacity =
        serde_json::from_str(include_str!("../../provider-capacity.json"))
            .map_err(|error| format!("packaged provider capacity policy is invalid: {error}"))?;
    if policy.schema_version != 1 {
        return Err(format!(
            "packaged provider capacity policy schema {} is unsupported",
            policy.schema_version
        ));
    }
    if policy.source.trim().is_empty() || policy.session_capacity == 0 {
        return Err("packaged provider capacity policy is incomplete".into());
    }
    Ok(ProviderCapacityEvidence {
        session_capacity: policy
            .session_capacity
            .min(crate::governor::HARD_SESSION_CEILING),
        status: crate::governor::ProviderCapacityStatus {
            source: policy.source,
            degraded: true,
            detail: Some(
                "live provider quota telemetry is unavailable; enforcing the packaged conservative safety ceiling"
                    .into(),
            ),
        },
    })
}

pub(super) fn provider_capacity_from_environment(
    configured: Result<String, std::env::VarError>,
) -> Result<ProviderCapacityEvidence, String> {
    match configured {
        Ok(raw) => {
            let session_capacity = raw
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0)
                .map(|value| value.min(crate::governor::HARD_SESSION_CEILING))
                .ok_or_else(|| {
                    "configured provider capacity telemetry is invalid: T_HUB_PROVIDER_SESSION_CAPACITY must be a positive integer"
                        .to_string()
                })?;
            Ok(ProviderCapacityEvidence {
                session_capacity,
                status: crate::governor::ProviderCapacityStatus {
                    source: "environment-override:T_HUB_PROVIDER_SESSION_CAPACITY".into(),
                    degraded: false,
                    detail: None,
                },
            })
        }
        Err(std::env::VarError::NotPresent) => packaged_provider_capacity_evidence(),
        Err(std::env::VarError::NotUnicode(_)) => Err(
            "configured provider capacity telemetry is unavailable: T_HUB_PROVIDER_SESSION_CAPACITY is not valid Unicode"
                .into(),
        ),
    }
}

pub(super) fn recorded_provider_harnesses(snapshot: &CaptainsSnapshot) -> BTreeMap<String, String> {
    let mut harnesses = BTreeMap::new();
    for agent in &snapshot.agent_sessions {
        if agent_has_durable_provider_intent(agent) {
            harnesses.insert(tmux_target(&agent.agent_session_id), agent.provider.clone());
        }
    }
    for captain in &snapshot.captains {
        if let (Some(terminal_id), Some(harness)) = (
            captain.terminal_id.as_deref(),
            captain.harness.as_deref().or(captain.provider.as_deref()),
        ) {
            harnesses.insert(tmux_target(terminal_id), harness.to_string());
        }
        for crew in &captain.crew {
            if let Some(harness) = crew.harness.as_deref().or(crew.provider.as_deref()) {
                harnesses.insert(tmux_target(&crew.terminal_id), harness.to_string());
            }
        }
    }
    harnesses
}

pub(super) fn durable_agent_provider_harnesses(
    snapshot: &CaptainsSnapshot,
) -> BTreeMap<String, String> {
    snapshot
        .agent_sessions
        .iter()
        .filter(|agent| agent_has_durable_provider_intent(agent))
        .map(|agent| (tmux_target(&agent.agent_session_id), agent.provider.clone()))
        .collect()
}

pub(super) fn pending_provider_marker(harness: &str) -> String {
    format!("pending:{harness}")
}

pub(super) fn inspect_provider_live_sessions(
    snapshot: &CaptainsSnapshot,
    sessions: &[String],
) -> Result<usize, String> {
    let recorded = recorded_provider_harnesses(snapshot);
    let durable_agents = durable_agent_provider_harnesses(snapshot);
    let mut live = 0usize;
    for tmux_session in sessions.iter().filter(|session| session.starts_with("th_")) {
        let marker = tmux::session_environment(tmux_session, PROVIDER_SESSION_ENV)
            .map_err(|error| {
                format!(
                    "provider session marker is unavailable for tmux session '{tmux_session}': {error}"
                )
            })?;
        if durable_agents.contains_key(tmux_session) {
            live = live.saturating_add(1);
            continue;
        }
        let (harness, pending, established) = match marker.as_deref() {
            Some("none") => (None, false, false),
            Some("pending:codex") => (Some("codex".to_string()), true, false),
            Some("pending:claude") => (Some("claude".to_string()), true, false),
            Some("alive:codex") => (Some("codex".to_string()), false, true),
            Some("alive:claude") => (Some("claude".to_string()), false, true),
            Some(harness @ ("codex" | "claude")) => (Some(harness.to_string()), false, false),
            Some(other) => {
                return Err(format!(
                    "provider session marker for tmux session '{tmux_session}' is invalid: '{other}'"
                ));
            }
            None => match recorded.get(tmux_session).cloned() {
                Some(harness) => (Some(harness), false, false),
                None => {
                    let legacy_provider = ["codex", "claude"].into_iter().any(|harness| {
                        tmux::harness_liveness(tmux_session, harness)
                            == tmux::SessionLiveness::Alive
                    });
                    if legacy_provider {
                        live = live.saturating_add(1);
                    }
                    continue;
                }
            },
        };
        let Some(harness) = harness else {
            continue;
        };
        match tmux::harness_liveness(tmux_session, &harness) {
            tmux::SessionLiveness::Alive => {
                live = live.saturating_add(1);
                if pending {
                    tmux::set_session_environment(
                        tmux_session,
                        PROVIDER_SESSION_ENV,
                        &format!("alive:{harness}"),
                    )
                    .map_err(|error| {
                        format!(
                            "provider readiness marker could not be persisted for tmux session '{tmux_session}': {error}"
                        )
                    })?;
                }
            }
            tmux::SessionLiveness::Gone if pending => {
                // A dead terminal cannot become the provider runtime it was
                // reserving.  Clear the marker and release the quota instead
                // of leaking one provider slot forever after Harness exit.
                // Count this observation once so the in-flight transition is
                // fail-closed, then the cleared marker prevents future leaks.
                live = live.saturating_add(1);
                tmux::set_session_environment(tmux_session, PROVIDER_SESSION_ENV, "none")
                    .map_err(|error| {
                        format!(
                            "provider pending marker could not be cleared for tmux session '{tmux_session}': {error}"
                        )
                    })?;
            }
            tmux::SessionLiveness::Gone => {}
            tmux::SessionLiveness::Unknown => {
                if pending || established {
                    live = live.saturating_add(1);
                } else {
                    return Err(format!(
                        "provider Harness evidence is unavailable for tmux session '{tmux_session}'"
                    ));
                }
            }
        }
    }
    Ok(live)
}

pub(super) fn provider_capacity_evidence(
    ctx: &ControlContext,
) -> Result<ProviderCapacityEvidence, String> {
    (ctx.provider_capacity)()
        .and_then(|mut evidence| {
            if evidence.session_capacity == 0 {
                return Err("provider capacity evidence reported a zero ceiling".into());
            }
            if evidence.status.source.trim().is_empty() {
                return Err("provider capacity evidence omitted its source".into());
            }
            evidence.session_capacity = evidence
                .session_capacity
                .min(crate::governor::HARD_SESSION_CEILING);
            Ok(evidence)
        })
        .map_err(|error| format!("provider capacity evidence unavailable: {error}"))
}

pub(super) fn runtime_capacity_from_evidence(
    ctx: &ControlContext,
    snapshot: &CaptainsSnapshot,
    live: &LiveSessionEvidence,
    available_worktrees: usize,
) -> Result<crate::governor::RuntimeCapacity, String> {
    let live_sessions = live.total_live_sessions;
    let (machine_healthy, machine_session_capacity) = dispatch_machine_evidence(ctx, live_sessions);
    let provider = provider_capacity_evidence(ctx)?;
    let provider_live_sessions = (ctx.provider_live_sessions)(snapshot, &live.tmux_sessions)
        .map_err(|error| format!("provider usage evidence unavailable: {error}"))?
        .saturating_add(live.pending_provider_sessions);
    let active_captains = snapshot
        .captains
        .iter()
        .filter(|captain| captain.role == FleetRole::Captain && captain.state == ClaimState::Active)
        .map(|captain| captain.ship_slug.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let active_captain_ships = snapshot
        .captains
        .iter()
        .filter(|captain| captain.role == FleetRole::Captain && captain.state == ClaimState::Active)
        .map(|captain| captain.ship_slug.clone())
        .collect::<BTreeSet<_>>();
    let live_cortana = snapshot
        .cortana
        .terminal_id
        .as_deref()
        .is_some_and(|terminal_id| {
            matches!(
                &snapshot.cortana.recovery,
                crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
            ) && snapshot.cortana.harness.as_deref().is_some_and(|harness| {
                tmux::session_liveness(&tmux_target(terminal_id)) == tmux::SessionLiveness::Alive
                    && tmux::harness_liveness(&tmux_target(terminal_id), harness)
                        == tmux::SessionLiveness::Alive
            })
        }) as usize;
    let live_recovery_sessions = snapshot
        .agent_sessions
        .iter()
        .filter(|agent| {
            agent.admission_purpose == crate::governor::AdmissionPurpose::Recovery
                && agent_has_durable_provider_intent(agent)
        })
        .count();
    let (live_fleet_admins, live_ship_admin_scopes) = live_admin_counts(ctx, snapshot);
    let live_ship_admins = live_ship_admin_scopes.values().copied().sum();
    Ok(crate::governor::RuntimeCapacity {
        live_sessions,
        machine_healthy,
        machine_session_capacity,
        provider_session_capacity: provider.session_capacity,
        provider_live_sessions,
        provider_capacity_status: provider.status,
        available_worktrees,
        active_captains,
        active_captain_ships,
        live_cortana,
        live_fleet_admins,
        live_ship_admins,
        live_ship_admin_scopes,
        live_recovery_sessions,
    })
}

pub(super) fn admit_spawn<'a>(
    ctx: &'a ControlContext,
    purpose: SpawnPurpose,
    requested_provider_lanes: usize,
    excluded_history_terminal_id: Option<&str>,
) -> Result<SpawnAdmissionGuard<'a>, crate::governor::Refusal> {
    let lock = ctx
        .dispatch_admission
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let capacity = evaluate_spawn_capacity(
        ctx,
        &purpose,
        requested_provider_lanes,
        excluded_history_terminal_id,
    )?;
    Ok(SpawnAdmissionGuard {
        _lock: lock,
        _capacity: capacity,
    })
}

/// Evaluate and consume one spawn admission while the caller holds
/// `dispatch_admission`. Keeping this separate lets start_agent validate identity,
/// exact baseline, and lane arguments under the atomic lock before it consumes a
/// rate token.
pub(super) fn evaluate_spawn_capacity(
    ctx: &ControlContext,
    purpose: &SpawnPurpose,
    requested_provider_lanes: usize,
    excluded_history_terminal_id: Option<&str>,
) -> Result<crate::governor::CapacityReport, crate::governor::Refusal> {
    let snapshot = ctx.captains.snapshot();
    let live =
        live_session_evidence(ctx, &snapshot, excluded_history_terminal_id).map_err(|message| {
            crate::governor::Refusal {
                code: "refused-evidence",
                message: format!("spawn refused: {message}"),
            }
        })?;
    let mut capacity = runtime_capacity_from_evidence(
        ctx,
        &snapshot,
        &live,
        crate::governor::HARD_SESSION_CEILING,
    )
    .map_err(|message| crate::governor::Refusal {
        code: "refused-provider",
        message: format!("spawn refused: {message}"),
    })?;
    let actual_live_sessions = capacity.live_sessions;
    let configured_recovery_exclusions = if matches!(purpose, SpawnPurpose::Cortana)
        && actual_live_sessions >= ctx.governor.max_sessions()
        && matches!(
            snapshot.cortana.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined { .. }
        ) {
        let mut exclusions = 0usize;
        for quarantine in &snapshot.cortana.quarantine_ledger {
            let target = tmux_target(&quarantine.terminal_id);
            if live.tmux_sessions.iter().any(|session| session == &target) {
                let observed = tmux::observe_session_effect_identity(&target)
                    .map(durable_cortana_effect_identity)
                    .map_err(|error| crate::governor::Refusal {
                        code: "refused-evidence",
                        message: format!(
                            "spawn refused: quarantined Cortana '{}' process evidence is unavailable: {error}",
                            quarantine.terminal_id
                        ),
                    })?;
                if observed != quarantine.tmux
                    || !ctx.identity.is_revoked(&quarantine.identity_id)
                    || tmux::harness_liveness(&target, &quarantine.harness)
                        != tmux::SessionLiveness::Alive
                {
                    return Err(crate::governor::Refusal {
                        code: "refused-evidence",
                        message: format!(
                            "spawn refused: quarantined Cortana '{}' no longer matches its exact revoked process evidence",
                            quarantine.terminal_id
                        ),
                    });
                }
                exclusions = exclusions.saturating_add(1);
            } else if tmux::session_liveness(&target) != tmux::SessionLiveness::Gone {
                return Err(crate::governor::Refusal {
                    code: "refused-evidence",
                    message: format!(
                        "spawn refused: quarantined Cortana '{}' has uncertain liveness",
                        quarantine.terminal_id
                    ),
                });
            }
        }
        exclusions
    } else {
        0
    };
    if configured_recovery_exclusions > 0 {
        if actual_live_sessions.saturating_add(1) > crate::governor::HARD_SESSION_CEILING {
            return Err(crate::governor::Refusal {
                code: "refused-ceiling",
                message: "spawn refused: Cortana recovery would exceed the hard session ceiling"
                    .into(),
            });
        }
        if !capacity.machine_healthy
            || actual_live_sessions.saturating_add(1) > capacity.machine_session_capacity
        {
            return Err(crate::governor::Refusal {
                code: "refused-machine",
                message: "spawn refused: Cortana recovery exceeds healthy machine capacity".into(),
            });
        }
        if requested_provider_lanes
            > capacity
                .provider_session_capacity
                .saturating_sub(capacity.provider_live_sessions)
        {
            return Err(crate::governor::Refusal {
                code: "refused-provider",
                message: "spawn refused: Cortana recovery exceeds provider capacity".into(),
            });
        }
        capacity.live_sessions = capacity
            .live_sessions
            .saturating_sub(configured_recovery_exclusions);
    }
    let configured_live_sessions = capacity.live_sessions;
    let request = crate::governor::DispatchPreflight {
        requested_lanes: vec![crate::governor::LaneClaim {
            lane_id: "spawn-admission".into(),
            owner_id: "spawn-admission".into(),
            dependencies: Some(BTreeSet::new()),
            mutable_files: BTreeSet::new(),
            mutable_schemas: BTreeSet::new(),
            mutable_interfaces: BTreeSet::new(),
        }],
        requested_provider_lanes,
        admission_purpose: durable_admission_purpose(purpose),
        ship_admin_scope: ship_admin_scope(purpose),
        active_lanes: Vec::new(),
        satisfied_dependencies: BTreeSet::new(),
        integration_contracts: Vec::new(),
        capacity,
    };
    let capacity = ctx
        .governor
        .preflight_dispatch(&request)
        .map_err(|refusal| crate::governor::Refusal {
            code: refusal.code.as_str(),
            message: refusal.message,
        })?;
    ctx.governor
        .check_spawn(configured_live_sessions, Instant::now())?;
    Ok(capacity)
}

pub(super) fn evaluate_spawn_capacity_for_new_ship(
    ctx: &ControlContext,
    ship_slug: &str,
) -> Result<crate::governor::CapacityReport, crate::governor::Refusal> {
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(ctx, &snapshot, None).map_err(|message| {
        crate::governor::Refusal {
            code: "refused-evidence",
            message: format!("spawn refused: {message}"),
        }
    })?;
    let mut capacity = runtime_capacity_from_evidence(
        ctx,
        &snapshot,
        &live,
        crate::governor::HARD_SESSION_CEILING,
    )
    .map_err(|message| crate::governor::Refusal {
        code: "refused-provider",
        message: format!("spawn refused: {message}"),
    })?;
    capacity.active_captain_ships.insert(ship_slug.to_string());
    capacity.active_captains = capacity.active_captain_ships.len();
    let request = crate::governor::DispatchPreflight {
        requested_lanes: vec![crate::governor::LaneClaim {
            lane_id: "commission-captain".into(),
            owner_id: "commission-captain".into(),
            dependencies: Some(BTreeSet::new()),
            mutable_files: BTreeSet::new(),
            mutable_schemas: BTreeSet::new(),
            mutable_interfaces: BTreeSet::new(),
        }],
        requested_provider_lanes: 1,
        admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
        ship_admin_scope: None,
        active_lanes: Vec::new(),
        satisfied_dependencies: BTreeSet::new(),
        integration_contracts: Vec::new(),
        capacity,
    };
    ctx.governor
        .preflight_dispatch(&request)
        .map_err(|refusal| crate::governor::Refusal {
            code: "refused-cap",
            message: refusal.message,
        })
}

pub(super) fn dispatch_runtime_capacity(
    ctx: &ControlContext,
    snapshot: &CaptainsSnapshot,
    project_id: &str,
) -> Result<crate::governor::RuntimeCapacity, String> {
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("dispatch preflight: unknown projectId '{project_id}'"))?;
    require_registered_git_capability(ctx, "capacity", &project.repo_root)?;
    let live = live_session_evidence(ctx, snapshot, None)?;
    let active_directories = snapshot
        .agent_sessions
        .iter()
        .filter(|agent| agent_retains_lane_ownership(agent))
        .map(|agent| files::posix_form(&agent.directory))
        .collect::<BTreeSet<_>>();
    let available_worktrees = git::worktree_list(&files::posix_form(&project.repo_root))
        .map_err(|error| format!("dispatch preflight: could not inspect worktrees: {error}"))?
        .into_iter()
        .map(|worktree| files::posix_form(&worktree.path))
        .filter(|path| !active_directories.contains(path))
        .collect::<BTreeSet<_>>()
        .len();
    runtime_capacity_from_evidence(ctx, snapshot, &live, available_worktrees)
}

pub(super) fn parse_integration_contracts(
    args: &Value,
    command: &str,
) -> Result<Vec<crate::governor::IntegrationContract>, String> {
    serde_json::from_value(
        args.get("integrationContracts")
            .cloned()
            .ok_or_else(|| format!("{command} requires an 'integrationContracts' array"))?,
    )
    .map_err(|error| format!("{command} integrationContracts are invalid: {error}"))
}

pub(super) fn validate_dependency_result_ancestry(
    command: &str,
    snapshot: &CaptainsSnapshot,
    project_id: &str,
    dependencies: &BTreeSet<String>,
    checkout: &str,
    source_commit: &str,
) -> Result<BTreeSet<String>, String> {
    let mut satisfied = BTreeSet::new();
    for dependency in dependencies {
        let completed = snapshot
            .agent_sessions
            .iter()
            .filter(|agent| agent.project_id == project_id)
            .filter(|agent| {
                agent
                    .lane_claim
                    .as_ref()
                    .is_some_and(|lane| lane.lane_id == *dependency)
            })
            .filter(|agent| {
                agent
                    .delivery_states()
                    .is_some_and(|states| states.complete)
            })
            .collect::<Vec<_>>();
        if completed.is_empty() {
            return Err(format!(
                "{command}: dependency '{dependency}' has no complete lane in project '{project_id}'"
            ));
        }

        let mut result_commits = BTreeSet::new();
        for agent in completed {
            let resulting_commit = agent
                .delivery
                .as_ref()
                .and_then(|delivery| delivery.resulting_commit.as_deref())
                .ok_or_else(|| {
                    format!(
                        "{command}: dependency '{dependency}' has complete delivery without an exact resulting commit"
                    )
                })?;
            result_commits.insert(resulting_commit.to_string());
        }
        for resulting_commit in result_commits {
            git::require_commit_ancestor(checkout, &resulting_commit, source_commit).map_err(
                |error| {
                    format!(
                        "{command}: dependency '{dependency}' result '{resulting_commit}' is not present in sourceCommit '{source_commit}': {error}"
                    )
                },
            )?;
        }
        satisfied.insert(dependency.clone());
    }
    Ok(satisfied)
}

pub(super) fn start_agent(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "start_agent")?;
    require_exact_args(
        args,
        "start_agent",
        &[
            "requestId",
            "captainSessionId",
            "assignment",
            "directory",
            "harness",
            "name",
            "workspaceTabId",
            "sourceCommit",
            "visibleProductBug",
            "laneId",
            "dependencies",
            "mutableFiles",
            "mutableSchemas",
            "mutableInterfaces",
            "integrationContracts",
            "admissionPurpose",
        ],
    )?;
    arg_str(args, "requestId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("start_agent requires a non-empty 'requestId'")?;
    let captain_session_id = arg_str(args, "captainSessionId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("start_agent requires a non-empty 'captainSessionId'")?;
    let assignment = arg_str(args, "assignment")
        .filter(|value| !value.trim().is_empty())
        .ok_or("start_agent requires a non-empty 'assignment'")?;
    let directory = arg_str(args, "directory")
        .filter(|value| !value.trim().is_empty())
        .ok_or("start_agent requires a non-empty 'directory'")?;
    let source_commit = arg_str(args, "sourceCommit")
        .filter(|value| !value.trim().is_empty())
        .ok_or("start_agent requires a non-empty 'sourceCommit'")?;
    let visible_product_bug = args
        .get("visibleProductBug")
        .and_then(Value::as_bool)
        .ok_or("start_agent requires a boolean 'visibleProductBug'")?;
    let lane_id = arg_str(args, "laneId")
        .filter(|value| !value.trim().is_empty())
        .ok_or("start_agent requires a non-empty 'laneId'")?;
    let dependencies = string_set_arg(args, "dependencies", "start_agent")?;
    let mutable_files = string_set_arg(args, "mutableFiles", "start_agent")?;
    let mutable_schemas = string_set_arg(args, "mutableSchemas", "start_agent")?;
    let mutable_interfaces = string_set_arg(args, "mutableInterfaces", "start_agent")?;
    let integration_contracts = parse_integration_contracts(args, "start_agent")?;
    let spawn_purpose = requested_spawn_purpose("start_agent", args, caller, trusted_internal)
        .map_err(|refusal| format!("start_agent: {}", refusal.message))?;
    let admission_purpose = durable_admission_purpose(&spawn_purpose);
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some(captain_session_id.as_str()))
        .cloned()
        .ok_or_else(|| format!("start_agent: Captain '{captain_session_id}' was not found"))?;
    authorize_agent_filter(
        ctx,
        Some(captain_session_id.as_str()),
        captain.project_id.as_deref(),
        caller,
        trusted_internal,
        "start_agent",
        false,
    )?;
    let project_id = captain
        .project_id
        .clone()
        .ok_or("start_agent: Captain is not bound to a registered Project")?;
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.project_id == project_id)
        .cloned()
        .ok_or_else(|| format!("start_agent: unknown projectId '{project_id}'"))?;
    require_registered_git_capability(ctx, "start_agent", &project.repo_root)?;
    let admission_lock = ctx
        .dispatch_admission
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let checkout = validate_crew_checkout(&project, Some(directory))
        .map_err(|error| error.replacen("dispatch_crew", "start_agent", 1))?;
    let worktree = git::worktree_list(&files::posix_form(&project.repo_root))
        .map_err(|error| format!("start_agent: could not detect worktree: {error}"))?
        .into_iter()
        .find(|worktree| files::posix_form(&worktree.path) == checkout);
    git::require_clean_exact_baseline(&checkout, &source_commit)
        .map_err(|error| format!("start_agent: baseline rejected: {error}"))?;
    if snapshot.agent_sessions.iter().any(|agent| {
        agent_retains_lane_ownership(agent) && files::posix_form(&agent.directory) == checkout
    }) {
        return Err(format!(
            "start_agent: checkout '{checkout}' is already owned by an active implementation lane"
        ));
    }
    let harness_name = arg_str(args, "harness")
        .unwrap_or_else(|| captain.harness.clone().unwrap_or_else(|| "codex".into()));
    let harness_name = harness_name.trim().to_ascii_lowercase();
    if !matches!(harness_name.as_str(), "codex" | "claude") {
        return Err("start_agent harness must be 'codex' or 'claude'".into());
    }
    let harness = Harness::from_provider(&harness_name);
    let agent_session_id = loop {
        let candidate = uuid::Uuid::new_v4().simple().to_string();
        let candidate = candidate[..8].to_string();
        if !snapshot
            .agent_sessions
            .iter()
            .any(|agent| agent.agent_session_id == candidate)
        {
            break candidate;
        }
    };
    let satisfied_dependencies = validate_dependency_result_ancestry(
        "start_agent",
        &snapshot,
        &project.project_id,
        &dependencies,
        &checkout,
        &source_commit,
    )?;
    let lane_claim = crate::governor::LaneClaim {
        lane_id,
        owner_id: agent_session_id.clone(),
        dependencies: Some(dependencies),
        mutable_files,
        mutable_schemas,
        mutable_interfaces,
    };
    let preflight = crate::governor::DispatchPreflight {
        requested_lanes: vec![lane_claim.clone()],
        requested_provider_lanes: 1,
        admission_purpose,
        ship_admin_scope: ship_admin_scope(&spawn_purpose),
        active_lanes: active_dispatch_lanes(&snapshot, &project.project_id),
        satisfied_dependencies,
        integration_contracts: integration_contracts.clone(),
        capacity: dispatch_runtime_capacity(ctx, &snapshot, &project.project_id)?,
    };
    let dispatch_capacity = ctx
        .governor
        .preflight_dispatch(&preflight)
        .map_err(|refusal| {
            format!(
                "start_agent: {}: {}",
                refusal.code.as_str(),
                refusal.message
            )
        })?;
    git::require_clean_exact_baseline(&checkout, &source_commit)
        .map_err(|error| format!("start_agent: baseline changed during admission: {error}"))?;
    let admission_capacity = evaluate_spawn_capacity(ctx, &spawn_purpose, 1, None)
        .map_err(|refusal| format!("start_agent: {}", refusal.message))?;
    let _admission = SpawnAdmissionGuard {
        _lock: admission_lock,
        _capacity: admission_capacity,
    };
    let now = now_ms();
    let record = AgentSessionRecord {
        agent_session_id: agent_session_id.clone(),
        captain_session_id: captain_session_id.clone(),
        project_id: project.project_id.clone(),
        assignment: assignment.clone(),
        directory: checkout.clone(),
        worktree_path: worktree
            .as_ref()
            .map(|worktree| files::posix_form(&worktree.path)),
        branch: worktree.and_then(|worktree| worktree.branch),
        workspace_tab_id: arg_str(args, "workspaceTabId"),
        harness: harness_name.clone(),
        provider: harness_name,
        provider_conversation_id: None,
        resume_point: None,
        runtime_state: RuntimeState::Starting,
        work_stage: crate::agent_session::WorkStage::Assigned,
        delivery: Some(crate::agent_session::DeliveryProvenance::new(
            source_commit,
            visible_product_bug,
        )),
        lane_claim: Some(lane_claim),
        integration_contracts,
        dispatch_capacity: Some(dispatch_capacity.clone()),
        admission_purpose,
        created_at: now,
        updated_at: now,
    };
    ctx.captains.insert_agent_session(record)?;
    #[cfg(test)]
    ctx.captains.pause_dispatch("start_agent_admitted");
    let launch = crew_launch_argv(harness, &assignment);
    let mut spawn_args = json!({
        "cwd": checkout,
        "name": arg_str(args, "name").unwrap_or_else(|| format!("Agent - {agent_session_id}")),
        "startupCommand": launch,
        "spawnedBy": captain_session_id,
    });
    if let Some(tab_id) = arg_str(args, "workspaceTabId") {
        spawn_args["tabId"] = json!(tab_id);
    }
    let spawned = match spawn_terminal_with_private_pane_command_and_id(
        ctx,
        &spawn_args,
        None,
        false,
        false,
        true,
        Some(&agent_session_id),
    ) {
        Ok(spawned) => spawned,
        Err(error) => {
            let cleanup = ctx.captains.mark_agent_unavailable(&agent_session_id);
            let identity_cleanup = ctx.identity.retire_tile(&agent_session_id);
            return Err(match cleanup {
                Ok(()) => match identity_cleanup {
                    Ok(_) => error,
                    Err(cleanup_error) => {
                        format!("{error}; identity cleanup failed: {cleanup_error}")
                    }
                },
                Err(cleanup_error) => {
                    format!(
                        "{error}; agent failure state could not be persisted: {cleanup_error}{}",
                        identity_cleanup
                            .err()
                            .map(|e| format!("; identity cleanup failed: {e}"))
                            .unwrap_or_default()
                    )
                }
            });
        }
    };
    let workspace_tab_id = spawned
        .get("tabId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let started = match ctx
        .captains
        .mark_agent_started(&agent_session_id, workspace_tab_id)
    {
        Ok(started) => started,
        Err(error) => {
            let _ = tmux::kill_session_tree(&tmux_target(&agent_session_id));
            ctx.tabs.retire_tile_locked(&agent_session_id);
            let unavailable = ctx.captains.mark_agent_unavailable(&agent_session_id);
            let identity_cleanup = ctx.identity.retire_tile(&agent_session_id);
            let mut detail = format!(
                "start_agent: launch succeeded but durable start state could not be persisted: {error}"
            );
            if let Err(cleanup) = unavailable {
                detail.push_str(&format!(
                    "; recovery-required: unavailable state could not be persisted: {cleanup}"
                ));
            }
            if let Err(cleanup) = identity_cleanup {
                detail.push_str(&format!("; identity cleanup failed: {cleanup}"));
            }
            return Err(detail);
        }
    };
    let started_source_baseline = started
        .delivery
        .as_ref()
        .map(|delivery| delivery.source_baseline.clone())
        .ok_or("start_agent: durable start record lost its dispatch source baseline")?;
    Ok(json!({
        "agentSessionId": started.agent_session_id,
        "captainSessionId": started.captain_session_id,
        "projectId": started.project_id,
        "directory": started.directory,
        "worktreePath": started.worktree_path,
        "branch": started.branch,
        "workspaceTabId": started.workspace_tab_id,
        "harness": started.harness,
        "provider": started.provider,
        "runtimeState": started.runtime_state,
        "workStage": started.work_stage,
        "sourceCommit": started_source_baseline.clone(),
        "sourceBaseline": started_source_baseline,
        "admissionPurpose": started.admission_purpose,
        "deliveryStates": started.delivery_states(),
        "laneClaim": started.lane_claim,
        "dispatchCapacity": dispatch_capacity,
        "assignmentDelivered": true,
    }))
}

pub(super) fn spawn_terminal(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    enforce_public_spawn_contract("spawn_terminal", args, caller, trusted_internal)?;
    if !caller_is_apex(caller, trusted_internal) {
        let caller = caller
            .ok_or("acl: spawn_terminal requires General/Cortana or an active owning Captain")?;
        if has_delegated_admin_history(ctx, &caller.session_id) {
            return Err(
                "acl: delegated administrators cannot spawn implementation sessions".into(),
            );
        }
        let caller_tile = caller
            .tile
            .as_deref()
            .ok_or("acl: spawn_terminal Captain has no terminal binding")?;
        let owns_active_captain = ctx
            .captains
            .captain_for_session(caller_tile)
            .filter(|captain| {
                captain.role == FleetRole::Captain
                    && captain.state == ClaimState::Active
                    && caller.fleet_role == Some(FleetRole::Captain)
                    && caller.ship_slug.as_deref() == Some(captain.ship_slug.as_str())
            })
            .is_some();
        if !owns_active_captain {
            return Err("acl: spawn_terminal requires an active owning Captain".into());
        }
    }
    spawn_terminal_with_private_pane_command(ctx, args, None, false, false, true)
}

/// Internal spawn variant for an already-formed private pane command.
///
/// The raw command is accepted only by in-process dispatch and is deliberately
/// excluded from the public response, apply forward, and command arguments.
pub(super) fn spawn_terminal_with_private_pane_command(
    ctx: &ControlContext,
    args: &Value,
    private_pane_command: Option<&str>,
    allow_captain_workspace: bool,
    require_exact_tab: bool,
    forward_projection: bool,
) -> Result<Value, String> {
    spawn_terminal_with_private_pane_command_and_id(
        ctx,
        args,
        private_pane_command,
        allow_captain_workspace,
        require_exact_tab,
        forward_projection,
        None,
    )
}

pub(super) fn spawn_terminal_with_private_pane_command_and_id(
    ctx: &ControlContext,
    args: &Value,
    private_pane_command: Option<&str>,
    allow_captain_workspace: bool,
    require_exact_tab: bool,
    forward_projection: bool,
    requested_session_id: Option<&str>,
) -> Result<Value, String> {
    let _identity_transaction = ctx.tabs.identity_transaction();
    let cwd = arg_str(args, "cwd");
    let name = arg_str(args, "name");
    let shell = arg_str(args, "shell");
    let startup_command =
        arg_str(args, "startupCommand").or_else(|| arg_str(args, "startup_command"));
    // Captain-chat phase 2: a captain spawning crew identifies itself so the
    // spawned session is recorded as crew in the captains registry.
    let spawned_by = arg_str(args, "spawnedBy").or_else(|| arg_str(args, "spawned_by"));
    if let Some(requested) = requested_session_id {
        if requested.len() != 8 || !requested.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(
                "spawn_terminal: requested session id must be eight alphanumeric bytes".into(),
            );
        }
    }

    // #27: a REMOTE peer may spawn ONLY with a cwd under the operator allowlist —
    // the spawn execs a shell SERVER-SIDE at a peer-controlled dir. Loopback (the
    // local frontend/MCP) is unrestricted. An absent cwd is fine (the UI spawns in
    // the shell's default dir).
    let cwd = match cwd {
        Some(c) if !ctx.peer_is_loopback => Some(
            files::scoped_create_path(&c, true, files::remote_file_roots())?
                .to_string_lossy()
                .into_owned(),
        ),
        other => other,
    };

    // A UI must exist to render the tile (webview sink or socket subscribers);
    // with neither, keep refusing rather than spawn a session nothing shows.
    if ctx.apply_sink.is_none() && ctx.fanout.subscriber_count() == 0 {
        return Err(
            "spawn_terminal: no UI is connected to adopt the new terminal tile; \
             refusing to spawn an untracked session (the app must be running to \
             spawn a terminal)"
                .to_string(),
        );
    }

    // Headless-org: resolve the TARGET TAB server-side, against the authoritative
    // registry, BEFORE spawning - `tabId` must exist (strict), `tabName` reuses an
    // existing tab or mints one (created hidden; the user's active tab is NOT
    // switched), and neither means the UI's active tab per the registry mirror.
    let tab_name = arg_str(args, "tabName").or_else(|| arg_str(args, "tab_name"));
    let tab_id = match (
        arg_str(args, "tabId").or_else(|| arg_str(args, "tab_id")),
        &tab_name,
    ) {
        (Some(id), _) => {
            if !ctx.tabs.has_tab(&id) {
                return Err(format!("spawn_terminal: unknown tabId '{id}'"));
            }
            if id == CAPTAIN_WORKSPACE_ID && !allow_captain_workspace {
                return Err(
                    "spawn_terminal: only durable Cortana or Captain identities may target Captain Workspace"
                        .into(),
                );
            }
            Some(id)
        }
        (None, Some(name)) if name == CAPTAIN_WORKSPACE_NAME || name == "Captains" => {
            if !allow_captain_workspace {
                return Err(
                    "spawn_terminal: only durable Cortana or Captain identities may target Captain Workspace"
                        .into(),
                );
            }
            ctx.tabs
                .insert_tab(CAPTAIN_WORKSPACE_ID, CAPTAIN_WORKSPACE_NAME);
            Some(CAPTAIN_WORKSPACE_ID.to_string())
        }
        (None, Some(name)) => Some(match ctx.tabs.id_for_name(name) {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                ctx.tabs.insert_tab(&id, name);
                id
            }
        }),
        // No target given: resolved atomically at placement time (active/first
        // tab) inside place_tile_with_fallback below.
        (None, None) => None,
    };

    // Spawn the tmux session SERVER-side (same id minting + pane wrap as the Tauri
    // `commands::spawn_terminal`) so the real id is known synchronously, the tile
    // can be placed in the registry atomically, and a hidden/suspended webview
    // cannot lose the spawn. Mirror `commands::resolve_cwd`'s unix arm ($HOME
    // fallback).
    let cwd_effective = cwd
        .clone()
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_default());
    let tmux_cwd = files::posix_form(&cwd_effective);
    let public_pane = crate::commands::pane_command(shell.as_deref(), startup_command.as_deref());
    let pane = private_pane_command.map(str::to_owned).or(public_pane);
    // Spawned terminals receive stable discovery plus a durable Crew identity.
    // Orchestration authority is acquired later from authoritative server state.
    let provider_harness = arg_str(args, "_providerHarness").or_else(|| {
        requested_session_id.and_then(|session_id| {
            ctx.captains
                .snapshot()
                .agent_sessions
                .into_iter()
                .find(|agent| agent.agent_session_id == session_id)
                .map(|agent| agent.provider)
        })
    });
    if let Some(provider_harness) = provider_harness.as_deref() {
        if !matches!(provider_harness, "codex" | "claude") {
            return Err("spawn_terminal: internal provider Harness marker is invalid".into());
        }
    }
    let (mut elevation, minted_identity) =
        spawn_env_with_identity(ctx, args, "spawn_terminal", requested_session_id)?;
    if let Some(provider_harness) = provider_harness {
        elevation.push((
            PROVIDER_SESSION_ENV.into(),
            pending_provider_marker(&provider_harness),
        ));
    }
    let spawn_result = match requested_session_id {
        Some(requested) => {
            spawn_tmux_terminal_with_id(requested, &tmux_cwd, pane.as_deref(), &elevation)
        }
        None => spawn_tmux_terminal(&tmux_cwd, pane.as_deref(), &elevation),
    };
    let (id, tmux_session) = match spawn_result {
        Ok(v) => v,
        Err(e) => {
            // Review L2: the mint persisted before this point, so a failed spawn would
            // leave an orphaned, secret-bearing identity for a session that never
            // existed. Retire it on the error leg.
            if let Some(identity) = &minted_identity {
                if let Err(rollback) = ctx.identity.retire(&identity.id) {
                    return Err(format!("{e}; identity rollback also failed: {rollback}"));
                }
            }
            return Err(e);
        }
    };
    // A requested id is pre-bound before tmux starts so a privileged child can never
    // observe a usable identity that is not yet tied to its durable AgentSession.
    // Generic terminal ids remain unknown until tmux returns and bind here.
    if let Some(identity) = minted_identity
        .as_ref()
        .filter(|_| requested_session_id.is_none())
    {
        if let Err(error) = ctx.identity.bind_tile(&identity.id, &id) {
            let _ = tmux::kill_session_tree(&tmux_session);
            let rollback = ctx.identity.retire(&identity.id);
            return Err(format!(
                "spawn_terminal: identity binding persistence failed and the terminal was rolled back: {error}{}",
                rollback
                    .err()
                    .map(|rollback| format!("; identity rollback also failed: {rollback}"))
                    .unwrap_or_default()
            ));
        }
    }

    // Atomic placement with fallback: if the resolved tab was closed in the race
    // window between spawn and placement, the tile lands in the active (else
    // first) tab instead - never orphaned outside the registry. The response
    // carries the ACTUAL placement.
    let placed_tab = if require_exact_tab {
        let exact = tab_id.as_deref().ok_or_else(|| {
            "spawn_terminal: exact placement requires an explicit tabId".to_string()
        });
        match exact.and_then(|tab_id| ctx.tabs.place_tile_exact(&id, tab_id)) {
            Ok(tab_id) => Some(tab_id),
            Err(error) => {
                let _ = tmux::kill_session_tree(&tmux_session);
                if let Some(identity) = &minted_identity {
                    if let Err(cleanup) = ctx.identity.retire(&identity.id) {
                        return Err(format!(
                            "spawn_terminal: exact Workspace placement failed; identity cleanup failed: {cleanup}"
                        ));
                    }
                }
                return Err(format!(
                    "spawn_terminal: exact Workspace placement failed and the terminal was rolled back: {error}"
                ));
            }
        }
    } else {
        ctx.tabs.place_tile_with_fallback(&id, tab_id.as_deref())
    };

    // A headless client may have no Work tab yet. Placement can be adopted by
    // the UI later; only exact placement is a hard requirement.
    if placed_tab.is_none() && require_exact_tab {
        let _ = tmux::kill_session_tree(&tmux_session);
        ctx.tabs.retire_tile_locked(&id);
        if let Some(identity) = &minted_identity {
            if let Err(error) = ctx.identity.retire(&identity.id) {
                return Err(format!(
                    "spawn_terminal: Workspace placement failed and identity cleanup failed: {error}"
                ));
            }
        }
        return Err(
            "spawn_terminal: Workspace placement failed and the terminal was rolled back".into(),
        );
    }

    // Captain-chat phase 2: record the crew link under the spawning captain.
    // The spawn NEVER fails on this - an unclaimed spawnedBy simply records
    // nothing (crewRecorded: false tells the caller to claim_captain first).
    let crew_recorded = match spawned_by.as_deref() {
        Some(cap) => match ctx.captains.record_crew(cap, &id) {
            Ok(recorded) => recorded,
            Err(error) => {
                let _ = tmux::kill_session_tree(&tmux_session);
                ctx.tabs.retire_tile_locked(&id);
                if let Some(identity) = &minted_identity {
                    if let Err(rollback) = ctx.identity.retire(&identity.id) {
                        return Err(format!(
                            "spawn_terminal: Crew registry persistence failed ({error}); identity rollback also failed: {rollback}"
                        ));
                    }
                }
                return Err(format!(
                    "spawn_terminal: Crew registry persistence failed and the terminal was rolled back: {error}"
                ));
            }
        },
        None => false,
    };
    if crew_recorded {
        let _ = captains_sync_apply(ctx);
    }

    let forward = with_sync(
        ctx,
        json!({
            "id": id,
            "tmuxSession": tmux_session,
            "cwd": cwd_effective,
            "name": name,
            "shell": shell,
            "startupCommand": startup_command,
            "tabId": placed_tab,
            "tabName": tab_name,
            "spawnedBy": spawned_by,
        }),
    );
    let applied = forward_projection && forward_apply(ctx, "spawn_terminal", &forward);
    Ok(json!({
        "accepted": "spawn_terminal",
        "id": id,
        "tmuxSession": tmux_session,
        "cwd": cwd_effective,
        "name": name,
        "shell": shell,
        "startupCommand": startup_command,
        "tabId": placed_tab,
        "placed": placed_tab.is_some(),
        "spawnedBy": spawned_by,
        "crewRecorded": crew_recorded,
        "audited": true,
        "applied": applied,
        "note": "the server spawned the session, placed the tile in the target tab \
                 in the authoritative registry (without switching the user's active \
                 tab), and forwarded the snapshot for the UI to render. tabId is the \
                 ACTUAL placement (falls back to the active tab if the target was \
                 closed mid-spawn).",
    }))
}

/// Mint a terminal id + create its detached tmux session. The id IS the tmux
/// session's own suffix, exactly like `commands::spawn_terminal` (bug #16 there:
/// id and session name must never disagree). Shared by the T12 native-path arms
/// of `spawn_terminal` / `create_worktree`, where no webview exists to run the
/// spawn client-side.
pub(super) fn spawn_tmux_terminal(
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<(String, String), String> {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let id = suffix[..8].to_string();
    spawn_tmux_terminal_with_id(&id, cwd, command, env)
}

pub(super) fn spawn_tmux_terminal_with_id(
    id: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<(String, String), String> {
    let tmux_session = format!("th_{id}");
    let mut session_env = env.to_vec();
    if !session_env
        .iter()
        .any(|(key, _)| key == PROVIDER_SESSION_ENV)
    {
        session_env.push((PROVIDER_SESSION_ENV.into(), "none".into()));
    }
    // Phase 2b: `env` carries the capability token (+ addr) for the new session, set
    // via tmux `-e` so it is present BEFORE the first pane execs and never lands in
    // argv. Empty ⇒ a plain session (headless tests / addr unknown).
    tmux::new_session_with_env(&tmux_session, cwd, command, &session_env)
        .map_err(|e| format!("failed to create tmux session: {e}"))?;
    // Registry-vs-reality (Incident A/B, ask #3): never hand back an id whose tmux
    // session did not actually materialize. `new-session` returning success is not
    // enough - a session can fail to appear (a raced server teardown, a wsl.exe
    // relaunch dropping the detached session), and the caller would then place +
    // record a GHOST tile keyed to a session that never existed. Verifying
    // has-session here means the id is live BEFORE it is placed/recorded, so a
    // spawn that didn't take fails loudly (and idempotently retryable) instead of
    // registering a phantom.
    // `has_session` here is the ONE safe use of the boolean form under the
    // de-conflation: both `Gone` (spawn genuinely didn't take) and `Unknown`
    // (verify probe timed out) map to `false`, and the action for BOTH is identical
    // and safe - best-effort reap + a loud, idempotently-retryable failure. We must
    // NOT hand back an id we could not verify live (that is the Incident A/B ghost),
    // so an ambiguous verify deliberately fails-retryable rather than registering.
    if !tmux::has_session(&tmux_session) {
        // L1: a FALSE negative is possible (a has-session hiccup / TOCTOU / probe
        // timeout) - the session may in fact have come up. Returning Err WITHOUT
        // tearing it down
        // would orphan it: a live pane with no tile, invisible to close_terminal,
        // and (under a requestId) the failure is cached so the retry won't adopt
        // it. Best-effort reap the maybe-live session before failing, so a spawn
        // that DID take is killed, not leaked. Idempotent: a truly-absent session
        // is a no-op.
        let _ = tmux::kill_session_tree(&tmux_session);
        return Err(format!(
            "tmux session '{tmux_session}' did not materialize after new-session \
             (the spawn did not take; any partial session was reaped and nothing \
             was registered)"
        ));
    }
    Ok((id.to_string(), tmux_session))
}

pub(super) fn spawn_managed_tmux_terminal_with_id(
    id: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
    launch: &tmux::ManagedRuntimeLaunchSpec,
) -> Result<(String, String, tmux::ManagedRuntimeOwnerToken), String> {
    let tmux_session = format!("th_{id}");
    let mut session_env = env.to_vec();
    if !session_env
        .iter()
        .any(|(key, _)| key == PROVIDER_SESSION_ENV)
    {
        session_env.push((PROVIDER_SESSION_ENV.into(), "none".into()));
    }
    let owner = tmux::new_prepared_managed_session_with_env(
        &tmux_session,
        cwd,
        command,
        &session_env,
        launch,
    )
    .map_err(|error| format!("failed to create cgroup-owned tmux session: {error}"))?;
    if !tmux::has_session(&tmux_session) {
        tmux::retire_managed_runtime(&tmux_session, &owner).map_err(|cleanup| {
            format!(
                "managed tmux session '{tmux_session}' was unobservable and exact owner cleanup failed: {cleanup}"
            )
        })?;
        return Err(format!(
            "managed tmux session '{tmux_session}' did not materialize after ownership verification"
        ));
    }
    Ok((id.to_string(), tmux_session, owner))
}
