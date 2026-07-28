//! Delegated-admin control handlers, split out of `control.rs` to shrink that
//! module. Supervisor-authority resolution, admin grant lifecycle (appoint /
//! revoke / approve / list), admin-execution target resolution + revalidation,
//! captain retirement, `execute_admin_operation`, and `plane_admin`. The parent
//! dispatch match routes here.

use super::*;

pub(super) fn stable_supervisor_generation(parts: &[&str]) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
    .max(1)
}

pub(super) fn supervisor_authority_for_caller(
    ctx: &ControlContext,
    caller: &ResolvedIdentity,
) -> Result<crate::delegated_admin::SupervisorAuthority, String> {
    match caller.fleet_role {
        Some(FleetRole::Cortana) => {
            let durable = ctx.captains.cortana_identity();
            let authoritative = ctx
                .identity
                .get(&caller.session_id)
                .is_some_and(|identity| authoritative_cortana_identity(ctx, &identity));
            if !authoritative || durable.identity_id.as_deref() != Some(caller.session_id.as_str())
            {
                return Err("delegated admin: caller is not the durable Cortana identity".into());
            }
            Ok(crate::delegated_admin::SupervisorAuthority {
                identity_id: caller.session_id.clone(),
                role: crate::delegated_admin::DelegatingSupervisorRole::Cortana,
                ship_slug: None,
                authority_generation: durable.generation,
                active: true,
            })
        }
        Some(FleetRole::Captain) => {
            let tile = caller
                .tile
                .as_deref()
                .ok_or("delegated admin: Captain identity has no terminal")?;
            let captain = ctx
                .captains
                .captain_for_session(tile)
                .filter(|captain| captain.role == FleetRole::Captain)
                .ok_or("delegated admin: caller is not an active Captain")?;
            Ok(crate::delegated_admin::SupervisorAuthority {
                identity_id: caller.session_id.clone(),
                role: crate::delegated_admin::DelegatingSupervisorRole::Captain,
                ship_slug: Some(captain.ship_slug.clone()),
                authority_generation: stable_supervisor_generation(&[
                    &captain.assignment_id,
                    &captain.ship_slug,
                ]),
                active: captain.state == ClaimState::Active,
            })
        }
        None => Err("delegated admin: only Cortana or a Captain may grant roles".into()),
    }
}

pub(super) fn current_delegating_supervisor(
    ctx: &ControlContext,
    grant: &crate::delegated_admin::DelegatedAdminGrant,
) -> crate::delegated_admin::SupervisorAuthority {
    match grant.delegator.role {
        crate::delegated_admin::DelegatingSupervisorRole::Cortana => {
            let durable = ctx.captains.cortana_identity();
            let active = durable
                .identity_id
                .as_deref()
                .and_then(|identity_id| ctx.identity.get(identity_id))
                .is_some_and(|identity| authoritative_cortana_identity(ctx, &identity));
            crate::delegated_admin::SupervisorAuthority {
                identity_id: durable
                    .identity_id
                    .unwrap_or_else(|| grant.delegator.identity_id.clone()),
                role: crate::delegated_admin::DelegatingSupervisorRole::Cortana,
                ship_slug: None,
                authority_generation: durable.generation,
                active,
            }
        }
        crate::delegated_admin::DelegatingSupervisorRole::Captain => {
            let captain = grant.delegator.ship_slug.as_deref().and_then(|ship_slug| {
                let matches = ctx
                    .captains
                    .snapshot()
                    .captains
                    .into_iter()
                    .filter(|captain| {
                        captain.role == FleetRole::Captain && captain.ship_slug == ship_slug
                    })
                    .collect::<Vec<_>>();
                (matches.len() == 1).then(|| matches.into_iter().next().unwrap())
            });
            let current_identity = captain
                .as_ref()
                .and_then(|captain| captain.terminal_id.as_deref())
                .and_then(|terminal_id| ctx.identity.for_tile(terminal_id));
            let authority_generation = captain
                .as_ref()
                .map(|captain| {
                    stable_supervisor_generation(&[&captain.assignment_id, &captain.ship_slug])
                })
                .unwrap_or_default();
            crate::delegated_admin::SupervisorAuthority {
                identity_id: current_identity
                    .map(|identity| identity.id)
                    .unwrap_or_else(|| grant.delegator.identity_id.clone()),
                role: crate::delegated_admin::DelegatingSupervisorRole::Captain,
                ship_slug: grant.delegator.ship_slug.clone(),
                authority_generation,
                active: captain
                    .as_ref()
                    .is_some_and(|captain| captain.state == ClaimState::Active),
            }
        }
    }
}

pub(super) fn current_admin_actor_for_identity(
    ctx: &ControlContext,
    actor_identity_id: &str,
) -> crate::delegated_admin::AdminActor {
    let identity = ctx.identity.get(actor_identity_id);
    let session_tile = identity
        .as_ref()
        .and_then(|identity| identity.session_tile.clone());
    let memberships = session_tile
        .as_deref()
        .map(|terminal_id| {
            ctx.captains
                .snapshot()
                .captains
                .into_iter()
                .filter(|captain| {
                    captain.state == ClaimState::Active
                        && captain.crew.iter().any(|crew| {
                            crew.terminal_id == terminal_id
                                && matches!(crew.state, CrewState::Active)
                        })
                })
                .map(|captain| captain.ship_slug)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_ship_slug = match memberships.as_slice() {
        [ship_slug] => Some(ship_slug.clone()),
        _ => None,
    };
    let is_current_crew = identity.as_ref().is_some_and(|identity| {
        identity.role == crate::identity::Role::Crew
            && !ctx.identity.is_revoked(&identity.id)
            && memberships.len() == 1
    });
    let runtime_active = session_tile.as_deref().is_some_and(|terminal_id| {
        tmux::session_liveness(&tmux_target(terminal_id)) == tmux::SessionLiveness::Alive
    });
    crate::delegated_admin::AdminActor {
        identity_id: actor_identity_id.to_string(),
        session_tile,
        current_ship_slug,
        is_current_crew,
        runtime_active,
    }
}

pub(super) fn current_admin_actor(
    ctx: &ControlContext,
    grant: &crate::delegated_admin::DelegatedAdminGrant,
) -> crate::delegated_admin::AdminActor {
    current_admin_actor_for_identity(ctx, &grant.actor_identity_id)
}

pub(super) fn parse_admin_role(
    value: &str,
) -> Result<crate::delegated_admin::DelegatedAdminRole, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shipadmin" | "ship-admin" | "ship_admin" => {
            Ok(crate::delegated_admin::DelegatedAdminRole::ShipAdmin)
        }
        "fleetadmin" | "fleet-admin" | "fleet_admin" => {
            Ok(crate::delegated_admin::DelegatedAdminRole::FleetAdmin)
        }
        _ => Err("appoint_admin role must be 'shipAdmin' or 'fleetAdmin'".into()),
    }
}

pub(super) fn parse_admin_operations(
    args: &Value,
) -> Result<std::collections::BTreeSet<crate::delegated_admin::AdminOperation>, String> {
    let values = args
        .get("permittedOperations")
        .or_else(|| args.get("permitted_operations"))
        .and_then(Value::as_array)
        .ok_or("appoint_admin requires a permittedOperations array")?;
    values
        .iter()
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                format!("appoint_admin contains an unknown permitted operation: {error}")
            })
        })
        .collect()
}

pub(super) fn appoint_admin(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    // Hold the registry's common mutation transaction across authority
    // validation and grant commit.  Captain/Cortana claim or promotion cannot
    // interleave between these two steps and invalidate the decision.
    let _authorization_transaction = ctx.tabs.identity_transaction();
    require_exact_args(
        args,
        "appoint_admin",
        &["actorSessionId", "role", "permittedOperations"],
    )?;
    require_socket_identity(caller, trusted_internal, "appoint_admin")?;
    let caller = caller.ok_or("appoint_admin requires a supervisor session identity")?;
    if ctx
        .delegated_admin
        .grants_for_actor(&caller.session_id)
        .iter()
        .any(|grant| grant.state.is_active())
    {
        return Err("appoint_admin: delegated administrators cannot re-delegate authority".into());
    }
    let supervisor = supervisor_authority_for_caller(ctx, caller)?;
    if !supervisor.active {
        return Err("appoint_admin requires an active supervisor".into());
    }
    if supervisor.role == crate::delegated_admin::DelegatingSupervisorRole::Cortana
        && !matches!(
            ctx.captains.snapshot().cortana.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        )
    {
        return Err("appoint_admin requires Cortana recovery to be healthy".into());
    }
    let actor_session_id = arg_str(args, "actorSessionId")
        .or_else(|| arg_str(args, "actor_session_id"))
        .ok_or("appoint_admin requires an actorSessionId")?;
    let actor = ctx
        .identity
        .for_tile(&actor_session_id)
        .ok_or_else(|| format!("appoint_admin cannot resolve Crew '{actor_session_id}'"))?;
    if actor.role != crate::identity::Role::Crew {
        return Err("appoint_admin may appoint only a Crew identity".into());
    }
    let actor_authority = current_admin_actor_for_identity(ctx, &actor.id);
    if !actor_authority.is_current_crew || !actor_authority.runtime_active {
        return Err("appoint_admin requires one live, authoritative Crew membership".into());
    }
    let role = parse_admin_role(&arg_str(args, "role").ok_or("appoint_admin requires a role")?)?;
    let scope = match role {
        crate::delegated_admin::DelegatedAdminRole::ShipAdmin => {
            if supervisor.role != crate::delegated_admin::DelegatingSupervisorRole::Captain {
                return Err("appoint_admin Ship Admin requires the owning Captain".into());
            }
            let ship_slug = supervisor
                .ship_slug
                .clone()
                .ok_or("appoint_admin Captain has no ship scope")?;
            let actor_is_crew = ctx.captains.snapshot().captains.iter().any(|captain| {
                captain.ship_slug == ship_slug
                    && captain
                        .crew
                        .iter()
                        .any(|crew| crew.terminal_id == actor_session_id)
            });
            if !actor_is_crew {
                return Err(format!(
                    "appoint_admin Crew '{actor_session_id}' is not owned by ship '{ship_slug}'"
                ));
            }
            if actor_authority.current_ship_slug.as_deref() != Some(ship_slug.as_str()) {
                return Err(format!(
                    "appoint_admin Crew '{actor_session_id}' does not have one active membership in ship '{ship_slug}'"
                ));
            }
            crate::delegated_admin::AdminScope::Ship { ship_slug }
        }
        crate::delegated_admin::DelegatedAdminRole::FleetAdmin => {
            if supervisor.role != crate::delegated_admin::DelegatingSupervisorRole::Cortana {
                return Err("appoint_admin Fleet Admin requires Cortana".into());
            }
            crate::delegated_admin::AdminScope::Fleet
        }
    };
    let grant = ctx
        .delegated_admin
        .appoint(crate::delegated_admin::AppointmentRequest {
            actor_identity_id: actor.id,
            role,
            delegator: supervisor,
            scope,
            permitted_operations: parse_admin_operations(args)?,
        })
        .map_err(|error| format!("{}: {error}", error.code()))?;
    Ok(json!({
        "accepted": "appoint_admin",
        "grant": grant,
        "audited": true,
    }))
}

pub(super) fn revoke_admin(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(args, "revoke_admin", &["grantId", "reason"])?;
    require_socket_identity(caller, trusted_internal, "revoke_admin")?;
    let caller = caller.ok_or("revoke_admin requires a supervisor session identity")?;
    let supervisor = supervisor_authority_for_caller(ctx, caller)?;
    let grant_id = arg_str(args, "grantId")
        .or_else(|| arg_str(args, "grant_id"))
        .ok_or("revoke_admin requires a grantId")?;
    let grant = ctx
        .delegated_admin
        .revoke(&grant_id, &supervisor, arg_str(args, "reason"))
        .map_err(|error| format!("{}: {error}", error.code()))?;
    Ok(json!({
        "accepted": "revoke_admin",
        "grant": grant,
        "audited": true,
    }))
}

pub(super) fn approve_admin_action(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "approve_admin_action",
        &["grantId", "operation", "sessionId", "target", "operationId"],
    )?;
    require_socket_identity(caller, trusted_internal, "approve_admin_action")?;
    let caller = caller.ok_or("approve_admin_action requires a supervisor session identity")?;
    let supervisor = supervisor_authority_for_caller(ctx, caller)?;
    if !supervisor.active {
        return Err("approve_admin_action requires an active supervisor".into());
    }
    let grant_id = arg_str(args, "grantId")
        .filter(|grant_id| !grant_id.trim().is_empty())
        .ok_or("approve_admin_action requires a non-empty grantId")?;
    let operation = serde_json::from_value::<crate::delegated_admin::AdminOperation>(
        args.get("operation")
            .cloned()
            .ok_or("approve_admin_action requires an operation")?,
    )
    .map_err(|error| format!("approve_admin_action operation is invalid: {error}"))?;
    let target = match operation {
        crate::delegated_admin::AdminOperation::CleanupSession => {
            if args.get("target").is_some() {
                return Err(
                    "approve_admin_action cleanupSession accepts sessionId only; target kind and ownership are resolved authoritatively"
                        .into(),
                );
            }
            let session_id = arg_str(args, "sessionId")
                .filter(|session_id| !session_id.trim().is_empty())
                .ok_or("approve_admin_action cleanupSession requires a non-empty sessionId")?;
            delegated_admin_target_for_terminal(ctx, &session_id)?
        }
        crate::delegated_admin::AdminOperation::CleanupWorktree => {
            if args.get("sessionId").is_some() {
                return Err(
                    "approve_admin_action cleanupWorktree accepts an exact worktree target only"
                        .into(),
                );
            }
            let target = serde_json::from_value::<crate::delegated_admin::AdminTarget>(
                args.get("target")
                    .cloned()
                    .ok_or("approve_admin_action cleanupWorktree requires a target")?,
            )
            .map_err(|error| format!("approve_admin_action target is invalid: {error}"))?;
            if !matches!(target, crate::delegated_admin::AdminTarget::Worktree { .. }) {
                return Err(
                    "approve_admin_action cleanupWorktree requires a worktree target".into(),
                );
            }
            if let Some(operation_id) = arg_str(args, "operationId") {
                let operation_id = operation_id.trim();
                if operation_id.is_empty() {
                    return Err(
                        "approve_admin_action cleanup recovery requires a non-empty operationId"
                            .into(),
                    );
                }
                let crate::delegated_admin::AdminTarget::Worktree {
                    ship_slug,
                    worktree_id,
                } = target
                else {
                    unreachable!()
                };
                ctx.worktrees
                    .recovery_record(operation_id, &worktree_id)
                    .map_err(|error| error.to_string())?;
                crate::delegated_admin::AdminTarget::WorktreeRetirement {
                    ship_slug,
                    worktree_id,
                    operation_id: operation_id.to_string(),
                }
            } else {
                target
            }
        }
        _ => {
            return Err(
                "approve_admin_action supports only cleanupSession or cleanupWorktree".into(),
            );
        }
    };
    let approval = ctx
        .delegated_admin
        .get(&grant_id)
        .ok_or_else(|| format!("grantNotFound: grant '{grant_id}' was not found"))?;
    let actor = current_admin_actor(ctx, &approval);
    let approval = ctx
        .delegated_admin
        .issue_exact_approval(&grant_id, &actor, &supervisor, operation, &target)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    Ok(json!({
        "accepted": "approve_admin_action",
        "approval": approval,
        "audited": true,
    }))
}

pub(super) fn list_admin_grants(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let grants = if let Some(caller) = caller {
        if caller.fleet_role.is_some() {
            ctx.delegated_admin.grants_delegated_by(&caller.session_id)
        } else {
            ctx.delegated_admin.grants_for_actor(&caller.session_id)
        }
    } else if trusted_internal {
        let actor_identity_id = arg_str(args, "actorIdentityId")
            .or_else(|| arg_str(args, "actor_identity_id"))
            .ok_or("list_admin_grants trusted host requires actorIdentityId")?;
        ctx.delegated_admin.grants_for_actor(&actor_identity_id)
    } else {
        return Err("list_admin_grants requires a session identity".into());
    };
    Ok(json!({
        "count": grants.len(),
        "grants": grants,
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum AdminExecutionTargetInput {
    Fleet,
    Ship {
        #[serde(rename = "shipSlug")]
        ship_slug: String,
    },
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Worktree {
        path: String,
    },
    GeneralReserved {
        action: String,
    },
    Implementation {
        #[serde(rename = "shipSlug")]
        ship_slug: String,
        #[serde(rename = "assignmentId")]
        assignment_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AdminExecutionResource {
    Fleet,
    Ship(String),
    Session(String),
    Worktree(String),
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedAdminExecutionTarget {
    authorization_target: crate::delegated_admin::AdminTarget,
    resource: AdminExecutionResource,
}

pub(super) fn exact_active_captain_for_ship(
    ctx: &ControlContext,
    ship_slug: &str,
) -> Result<CaptainRecord, String> {
    if ship_slug.trim().is_empty() {
        return Err("execute_admin_operation shipSlug must not be empty".into());
    }
    let matching = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .filter(|captain| {
            captain.role == FleetRole::Captain
                && captain.state == ClaimState::Active
                && captain.ship_slug == ship_slug
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [captain] => Ok(captain.clone()),
        [] => Err(format!(
            "execute_admin_operation ship '{ship_slug}' has no active authoritative Captain"
        )),
        _ => Err(format!(
            "execute_admin_operation ship '{ship_slug}' has ambiguous Captain ownership"
        )),
    }
}

pub(super) fn resolve_admin_worktree_target(
    ctx: &ControlContext,
    requested_path: &str,
) -> Result<ResolvedAdminExecutionTarget, String> {
    let requested_path = files::posix_form(requested_path.trim());
    if requested_path.is_empty() || !requested_path.starts_with('/') {
        return Err("execute_admin_operation worktree path must be absolute".into());
    }
    let snapshot = ctx.captains.snapshot();
    let mut matches = Vec::new();
    for project in &snapshot.projects {
        if project.vcs_capability.as_deref() == Some("none") {
            // An explicit non-Git Project still has an authoritative worktree
            // namespace for delegated-admin authorization.  Resolve that namespace
            // from durable registry identity only; the capability gate belongs after
            // the delegated Ship Admin grant is authorized in execute_admin_operation.
            let project_root =
                files::posix_form(project.root_path.as_deref().unwrap_or(&project.repo_root))
                    .trim_end_matches('/')
                    .to_string();
            if requested_path != project_root
                && !requested_path.starts_with(&format!("{project_root}/"))
            {
                continue;
            }
            for captain in snapshot.captains.iter().filter(|captain| {
                captain.role == FleetRole::Captain
                    && captain.state == ClaimState::Active
                    && captain.project_id.as_deref() == Some(project.project_id.as_str())
            }) {
                matches.push((captain.ship_slug.clone(), requested_path.clone()));
            }
            continue;
        }
        let worktrees = git::worktree_list(&files::posix_form(&project.repo_root))?;
        if let Some(worktree) = worktrees
            .into_iter()
            .find(|worktree| files::posix_form(&worktree.path) == requested_path)
        {
            for captain in snapshot.captains.iter().filter(|captain| {
                captain.role == FleetRole::Captain
                    && captain.state == ClaimState::Active
                    && captain.project_id.as_deref() == Some(project.project_id.as_str())
            }) {
                matches.push((captain.ship_slug.clone(), worktree.path.clone()));
            }
        }
    }
    matches.sort();
    matches.dedup();
    match matches.as_slice() {
        [(ship_slug, path)] => Ok(ResolvedAdminExecutionTarget {
            authorization_target: crate::delegated_admin::AdminTarget::Worktree {
                ship_slug: ship_slug.clone(),
                worktree_id: files::posix_form(path),
            },
            resource: AdminExecutionResource::Worktree(files::posix_form(path)),
        }),
        [] => Err(format!(
            "execute_admin_operation worktree '{requested_path}' has no active authoritative ship owner"
        )),
        _ => Err(format!(
            "execute_admin_operation worktree '{requested_path}' has ambiguous ship ownership"
        )),
    }
}

pub(super) fn resolve_admin_execution_target(
    ctx: &ControlContext,
    input: &AdminExecutionTargetInput,
) -> Result<ResolvedAdminExecutionTarget, String> {
    match input {
        AdminExecutionTargetInput::Fleet => Ok(ResolvedAdminExecutionTarget {
            authorization_target: crate::delegated_admin::AdminTarget::Fleet,
            resource: AdminExecutionResource::Fleet,
        }),
        AdminExecutionTargetInput::Ship { ship_slug } => {
            let captain = exact_active_captain_for_ship(ctx, ship_slug)?;
            Ok(ResolvedAdminExecutionTarget {
                authorization_target: crate::delegated_admin::AdminTarget::Ship {
                    ship_slug: captain.ship_slug.clone(),
                },
                resource: AdminExecutionResource::Ship(captain.ship_slug),
            })
        }
        AdminExecutionTargetInput::Session { session_id } => {
            let session_id = session_id.strip_prefix("th_").unwrap_or(session_id).trim();
            if session_id.is_empty() {
                return Err("execute_admin_operation sessionId must not be empty".into());
            }
            Ok(ResolvedAdminExecutionTarget {
                authorization_target: delegated_admin_target_for_terminal(ctx, session_id)?,
                resource: AdminExecutionResource::Session(session_id.to_string()),
            })
        }
        AdminExecutionTargetInput::Worktree { path } => resolve_admin_worktree_target(ctx, path),
        AdminExecutionTargetInput::GeneralReserved { action } => Ok(ResolvedAdminExecutionTarget {
            authorization_target: crate::delegated_admin::AdminTarget::GeneralReserved {
                action: action.clone(),
            },
            resource: AdminExecutionResource::Forbidden,
        }),
        AdminExecutionTargetInput::Implementation {
            ship_slug,
            assignment_id,
        } => Ok(ResolvedAdminExecutionTarget {
            authorization_target: crate::delegated_admin::AdminTarget::Implementation {
                ship_slug: ship_slug.clone(),
                assignment_id: assignment_id.clone(),
            },
            resource: AdminExecutionResource::Forbidden,
        }),
    }
}

pub(super) fn revalidate_admin_execution_authority(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
) -> Result<(), String> {
    let grant = ctx
        .delegated_admin
        .get(&audit.grant_id)
        .filter(|grant| {
            grant.state.is_active()
                && grant.grant_generation == audit.grant_generation
                && grant.actor_identity_id == audit.actor_identity_id
        })
        .ok_or("delegated admin: exact grant changed before administrative execution")?;
    let supervisor = current_delegating_supervisor(ctx, &grant);
    let actor = current_admin_actor(ctx, &grant);
    ctx.delegated_admin
        .authorize(
            &crate::delegated_admin::AdminActor {
                identity_id: audit.actor_identity_id.clone(),
                session_tile: audit.actor_session_tile.clone(),
                ..actor
            },
            &supervisor,
            audit.operation,
            &audit.target,
            &crate::delegated_admin::AdminSafeguards::default(),
        )
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", error.code()))
}

pub(super) fn revalidate_admin_execution_target(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    target: &ResolvedAdminExecutionTarget,
) -> Result<(), String> {
    let current = match &target.resource {
        AdminExecutionResource::Fleet => crate::delegated_admin::AdminTarget::Fleet,
        AdminExecutionResource::Ship(ship_slug) => {
            let captain = exact_active_captain_for_ship(ctx, ship_slug)?;
            crate::delegated_admin::AdminTarget::Ship {
                ship_slug: captain.ship_slug,
            }
        }
        AdminExecutionResource::Session(session_id) => {
            delegated_admin_target_for_terminal(ctx, session_id)?
        }
        AdminExecutionResource::Worktree(path) => {
            resolve_admin_worktree_target(ctx, path)?.authorization_target
        }
        AdminExecutionResource::Forbidden => audit.target.clone(),
    };
    if current != audit.target {
        return Err(
            "delegated admin: exact target ownership changed before administrative execution"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn revalidate_admin_effect_session(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    target: &ResolvedAdminExecutionTarget,
    session_id: &str,
) -> Result<(), String> {
    revalidate_admin_execution_target(ctx, audit, target)?;
    let current_session = delegated_admin_target_for_terminal(ctx, session_id)?;
    match (&audit.target, &current_session) {
        (
            crate::delegated_admin::AdminTarget::Fleet,
            crate::delegated_admin::AdminTarget::Captain { .. },
        ) => Ok(()),
        (
            crate::delegated_admin::AdminTarget::Ship { ship_slug },
            crate::delegated_admin::AdminTarget::Captain {
                ship_slug: current, ..
            }
            | crate::delegated_admin::AdminTarget::CrewSession {
                ship_slug: current, ..
            },
        ) if ship_slug == current => Ok(()),
        (expected, current) if expected == current => Ok(()),
        _ => Err(
            "delegated admin: administrative session target changed ownership before mutation"
                .into(),
        ),
    }
}

pub(super) fn session_liveness_label(liveness: tmux::SessionLiveness) -> &'static str {
    match liveness {
        tmux::SessionLiveness::Alive => "alive",
        tmux::SessionLiveness::Gone => "gone",
        tmux::SessionLiveness::Unknown => "unknown",
    }
}

pub(super) fn admin_session_ids_for_target(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    target: &ResolvedAdminExecutionTarget,
) -> Result<Vec<String>, String> {
    let snapshot = ctx.captains.snapshot();
    let mut sessions = match &target.resource {
        AdminExecutionResource::Session(session_id) => vec![session_id.clone()],
        AdminExecutionResource::Ship(ship_slug) => {
            let captain = exact_active_captain_for_ship(ctx, ship_slug)?;
            let mut sessions = captain.terminal_id.into_iter().collect::<Vec<_>>();
            if audit.delegated_role == crate::delegated_admin::DelegatedAdminRole::ShipAdmin {
                sessions.extend(
                    captain
                        .crew
                        .into_iter()
                        .filter(|crew| !matches!(crew.state, CrewState::Removed { .. }))
                        .map(|crew| crew.terminal_id),
                );
            }
            sessions
        }
        AdminExecutionResource::Fleet => snapshot
            .captains
            .into_iter()
            .filter(|captain| {
                captain.role == FleetRole::Captain && captain.state == ClaimState::Active
            })
            .filter_map(|captain| captain.terminal_id)
            .collect(),
        AdminExecutionResource::Worktree(_) | AdminExecutionResource::Forbidden => Vec::new(),
    };
    sessions.sort();
    sessions.dedup();
    Ok(sessions)
}

pub(super) fn maintain_admin_sessions(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    target: &ResolvedAdminExecutionTarget,
) -> Result<Value, String> {
    let session_ids = admin_session_ids_for_target(ctx, audit, target)?;
    if session_ids.is_empty() {
        return Err("execute_admin_operation target has no authoritative session resources".into());
    }
    let mut maintained = Vec::new();
    let mut recovery_plan = Vec::new();
    for session_id in session_ids {
        let tmux_session = tmux_target(&session_id);
        let liveness = tmux::session_liveness(&tmux_session);
        match liveness {
            tmux::SessionLiveness::Alive => {
                revalidate_admin_effect_session(ctx, audit, target, &session_id)?;
                revalidate_admin_execution_authority(ctx, audit)?;
                tmux::maintain_session(&tmux_session).map_err(|error| {
                    format!("session '{session_id}' maintenance failed: {error}")
                })?;
                maintained.push(json!({
                    "sessionId": session_id,
                    "tmuxSession": tmux_session,
                    "outcome": "maintained",
                }));
            }
            tmux::SessionLiveness::Gone | tmux::SessionLiveness::Unknown => {
                recovery_plan.push(json!({
                    "sessionId": session_id,
                    "tmuxSession": tmux_session,
                    "observedLiveness": session_liveness_label(liveness),
                    "requiredSupervisorDecision": "replaceOrRetireRuntime",
                }));
            }
        }
    }
    revalidate_admin_execution_target(ctx, audit, target)?;
    revalidate_admin_execution_authority(ctx, audit)?;
    Ok(json!({
        "outcome": if recovery_plan.is_empty() { "maintained" } else { "recoveryPrepared" },
        "maintainedSessions": maintained,
        "recoveryPlan": recovery_plan,
    }))
}

pub(super) fn recover_admin_worktree(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    path: &str,
) -> Result<Value, String> {
    revalidate_admin_execution_target(
        ctx,
        audit,
        &ResolvedAdminExecutionTarget {
            authorization_target: audit.target.clone(),
            resource: AdminExecutionResource::Worktree(path.to_string()),
        },
    )?;
    revalidate_admin_execution_authority(ctx, audit)?;
    let info = git::git_info_cached(path);
    if !info.is_repo || info.worktree_root.as_deref() != Some(path) {
        return Err(format!(
            "execute_admin_operation worktree '{path}' no longer resolves as the exact Git worktree"
        ));
    }
    let recovery_required = info.head_commit.is_none();
    Ok(json!({
        "outcome": if recovery_required { "recoveryPrepared" } else { "resourceReconciled" },
        "worktree": {
            "path": path,
            "branch": info.branch,
            "headCommit": info.head_commit,
            "dirtyCount": info.dirty_count,
            "isLinkedWorktree": info.is_linked_worktree,
        },
        "recoveryPlan": recovery_required.then(|| json!({
            "requiredSupervisorDecision": "selectAuthoritativeBaseline",
            "preserveDirtyWork": info.dirty_count > 0,
        })),
    }))
}

pub(super) fn retirement_plan_id(
    audit: &crate::delegated_admin::AdminAuditContext,
    blockers: &[Value],
) -> String {
    let canonical = serde_json::to_vec(&json!({
        "grantId": audit.grant_id,
        "grantGeneration": audit.grant_generation,
        "target": audit.target,
        "blockers": blockers,
    }))
    .expect("retirement plan inputs are serializable");
    format!("sha256:{:x}", Sha256::digest(canonical))
}

pub(super) fn prepare_admin_retirement(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    target: &ResolvedAdminExecutionTarget,
) -> Result<Value, String> {
    revalidate_admin_execution_target(ctx, audit, target)?;
    revalidate_admin_execution_authority(ctx, audit)?;
    let mut blockers = Vec::new();
    match &target.resource {
        AdminExecutionResource::Session(session_id) => {
            let liveness = tmux::session_liveness(&tmux_target(session_id));
            if !matches!(liveness, tmux::SessionLiveness::Gone) {
                blockers.push(json!({
                    "kind": "sessionNotStopped",
                    "sessionId": session_id,
                    "liveness": session_liveness_label(liveness),
                }));
            }
            if let crate::delegated_admin::AdminTarget::Captain {
                captain_identity_id,
                ..
            } = &audit.target
            {
                let dependent_grants = ctx
                    .delegated_admin
                    .grants_delegated_by(captain_identity_id)
                    .into_iter()
                    .filter(|grant| grant.state.is_active())
                    .count();
                if dependent_grants > 0 {
                    blockers.push(json!({
                        "kind": "activeDependentGrants",
                        "count": dependent_grants,
                    }));
                }
            }
        }
        AdminExecutionResource::Ship(ship_slug) => {
            let captain = exact_active_captain_for_ship(ctx, ship_slug)?;
            if let Some(session_id) = &captain.terminal_id {
                blockers.push(json!({
                    "kind": "activeCaptain",
                    "sessionId": session_id,
                }));
            }
            let active_crew = captain
                .crew
                .iter()
                .filter(|crew| !matches!(crew.state, CrewState::Removed { .. }))
                .count();
            if active_crew > 0 {
                blockers.push(json!({
                    "kind": "activeCrew",
                    "count": active_crew,
                }));
            }
            let active_grants = ctx
                .delegated_admin
                .active_grants()
                .into_iter()
                .filter(|grant| {
                    matches!(
                        &grant.scope,
                        crate::delegated_admin::AdminScope::Ship { ship_slug: scope }
                            if scope == ship_slug
                    )
                })
                .count();
            if active_grants > 0 {
                blockers.push(json!({
                    "kind": "activeAdministrativeGrants",
                    "count": active_grants,
                }));
            }
        }
        AdminExecutionResource::Worktree(path) => {
            let info = git::git_info_cached(path);
            if info.dirty_count > 0 {
                blockers.push(json!({
                    "kind": "dirtyWorktree",
                    "dirtyCount": info.dirty_count,
                }));
            }
            let leased_sessions = tmux::pane_info()
                .map_err(|error| format!("retirement lease inspection failed: {error}"))?
                .into_iter()
                .filter(|pane| crate::worktree_coordinator::path_within(&pane.cwd, path))
                .map(|pane| pane.session)
                .collect::<Vec<_>>();
            if !leased_sessions.is_empty() {
                blockers.push(json!({
                    "kind": "liveSessionLeases",
                    "sessions": leased_sessions,
                }));
            }
        }
        AdminExecutionResource::Fleet | AdminExecutionResource::Forbidden => {
            return Err(
                "execute_admin_operation target is not valid for retirement preparation".into(),
            );
        }
    }
    revalidate_admin_execution_target(ctx, audit, target)?;
    revalidate_admin_execution_authority(ctx, audit)?;
    let plan_id = retirement_plan_id(audit, &blockers);
    Ok(json!({
        "outcome": "retirementPrepared",
        "planId": plan_id,
        "ready": blockers.is_empty(),
        "blockers": blockers,
        "destructiveActionPerformed": false,
    }))
}

pub(super) fn execute_admin_operation(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(args, "execute_admin_operation", &["operation", "target"])?;
    require_socket_identity(caller, trusted_internal, "execute_admin_operation")?;
    let caller = caller
        .ok_or("execute_admin_operation requires the exact delegated Crew session identity")?;
    let operation = serde_json::from_value::<crate::delegated_admin::AdminOperation>(
        args.get("operation")
            .cloned()
            .ok_or("execute_admin_operation requires an operation")?,
    )
    .map_err(|error| format!("execute_admin_operation operation is invalid: {error}"))?;
    if !matches!(
        operation,
        crate::delegated_admin::AdminOperation::MaintainSession
            | crate::delegated_admin::AdminOperation::RecoverResource
            | crate::delegated_admin::AdminOperation::PrepareRetirement
            | crate::delegated_admin::AdminOperation::MaintainFleetResource
    ) {
        return Err("execute_admin_operation supports maintainSession, recoverResource, prepareRetirement, or maintainFleetResource only".into());
    }
    let target_input = serde_json::from_value::<AdminExecutionTargetInput>(
        args.get("target")
            .cloned()
            .ok_or("execute_admin_operation requires a target")?,
    )
    .map_err(|error| format!("execute_admin_operation target is invalid: {error}"))?;
    let target = resolve_admin_execution_target(ctx, &target_input)?;
    let audit = authorize_delegated_admin(
        ctx,
        caller,
        operation,
        target.authorization_target.clone(),
        crate::delegated_admin::AdminSafeguards::default(),
    )?;
    if let AdminExecutionResource::Worktree(path) = &target.resource {
        require_registered_git_capability(ctx, "admin_worktree", path)?;
    }
    let result = match operation {
        crate::delegated_admin::AdminOperation::MaintainSession => {
            if !matches!(target.resource, AdminExecutionResource::Session(_)) {
                Err("execute_admin_operation maintainSession requires a session target".into())
            } else {
                maintain_admin_sessions(ctx, &audit, &target)
            }
        }
        crate::delegated_admin::AdminOperation::RecoverResource => match &target.resource {
            AdminExecutionResource::Worktree(path) => recover_admin_worktree(ctx, &audit, path),
            AdminExecutionResource::Forbidden => {
                Err("execute_admin_operation cannot recover a forbidden target".into())
            }
            _ => maintain_admin_sessions(ctx, &audit, &target),
        },
        crate::delegated_admin::AdminOperation::PrepareRetirement => {
            prepare_admin_retirement(ctx, &audit, &target)
        }
        crate::delegated_admin::AdminOperation::MaintainFleetResource => {
            if !matches!(
                target.resource,
                AdminExecutionResource::Fleet
                    | AdminExecutionResource::Ship(_)
                    | AdminExecutionResource::Session(_)
            ) {
                Err("execute_admin_operation maintainFleetResource requires a fleet, ship, or Captain session target".into())
            } else {
                maintain_admin_sessions(ctx, &audit, &target)
            }
        }
        _ => unreachable!(),
    }
    .map(|outcome| {
        json!({
            "accepted": "execute_admin_operation",
            "operation": operation,
            "target": target.authorization_target,
            "outcome": outcome,
            "delegatedAdmin": audit,
            "audited": true,
        })
    });
    record_delegated_admin_execution_outcome(ctx, &audit, &result);
    result
}

pub(super) fn cleanup_worktree_artifacts(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "cleanup_worktree_artifacts",
        &["worktreePath", "approvalId", "confirm"],
    )?;
    require_socket_identity(caller, trusted_internal, "cleanup_worktree_artifacts")?;
    let caller = caller
        .ok_or("cleanup_worktree_artifacts requires the exact delegated Crew session identity")?;
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(
            "cleanup_worktree_artifacts requires explicit confirmation before mutation".into(),
        );
    }
    let requested_path = arg_str(args, "worktreePath")
        .filter(|path| !path.trim().is_empty())
        .ok_or("cleanup_worktree_artifacts requires a non-empty worktreePath")?;
    let approval_id = arg_str(args, "approvalId")
        .filter(|approval_id| !approval_id.trim().is_empty())
        .ok_or("cleanup_worktree_artifacts requires an exact approvalId")?;
    ctx.worktrees.require_provider_configured()?;

    let target = resolve_admin_worktree_target(ctx, &requested_path)?;
    let path = match &target.resource {
        AdminExecutionResource::Worktree(path) => path.clone(),
        _ => return Err("cleanup_worktree_artifacts requires an exact worktree target".into()),
    };
    require_registered_git_capability(ctx, "cleanup_worktree_artifacts", &path)?;
    let capture = crate::worktree_coordinator::inspect_cleanup_candidate(&path)?;
    if !capture.is_linked {
        return Err("cleanup_worktree_artifacts refuses the primary worktree".into());
    }
    if capture.dirty {
        return Err("cleanup_worktree_artifacts requires a clean worktree".into());
    }
    if !capture.merged {
        return Err(
            "cleanup_worktree_artifacts requires HEAD to be merged into origin's default branch"
                .into(),
        );
    }
    let leased_sessions = tmux::pane_info()
        .map_err(|error| format!("cleanup_worktree_artifacts lease inspection failed: {error}"))?
        .into_iter()
        .filter(|pane| crate::worktree_coordinator::path_within(&pane.cwd, &path))
        .map(|pane| pane.session)
        .collect::<Vec<_>>();
    if !leased_sessions.is_empty() {
        return Err(format!(
            "cleanup_worktree_artifacts refuses worktree '{path}' because live sessions are present: {}",
            leased_sessions.join(", ")
        ));
    }

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
            crate::delegated_admin::AdminOperation::CleanupWorktree,
            &target.authorization_target,
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    let evidence_id = {
        let body = serde_json::to_vec(&json!({
            "target": target.authorization_target,
            "worktree": capture.worktree,
            "targets": capture.targets,
            "dirty": capture.dirty,
            "merged": capture.merged,
            "isLinked": capture.is_linked,
        }))
        .expect("worktree cleanup evidence is serializable");
        format!("sha256:{:x}", Sha256::digest(body))
    };
    let audit = authorize_delegated_admin(
        ctx,
        caller,
        crate::delegated_admin::AdminOperation::CleanupWorktree,
        target.authorization_target.clone(),
        crate::delegated_admin::AdminSafeguards {
            authoritative_ownership_verified: true,
            consumed_approval: Some(consumed_approval),
            worktree_safety: Some(crate::delegated_admin::WorktreeSafetyEvidence {
                evidence_id,
                target_fingerprint: target.authorization_target.fingerprint(),
                removable: true,
            }),
        },
    )?;
    let result: Result<Value, String> = (|| {
        revalidate_admin_execution_target(ctx, &audit, &target)?;
        let request_path = ctx.worktrees.next_request_path();
        let request_path = request_path.to_string_lossy().into_owned();
        let target_count = capture.targets.len();
        let mut record = ctx
            .worktrees
            .begin_retirement_if_idle(&path, &request_path, |canonical_path| {
                tmux::pane_info()
                    .map_err(|error| {
                        format!("cleanup_worktree_artifacts lease inspection failed: {error}")
                    })
                    .map(|panes| {
                        panes
                            .into_iter()
                            .filter(|pane| {
                                crate::worktree_coordinator::path_within(&pane.cwd, canonical_path)
                            })
                            .map(|pane| pane.session)
                            .collect()
                    })
            })
            .map_err(|error| error.to_string())?;
        let execution: Result<Value, String> = (|| {
            record = ctx
                .worktrees
                .write_provider_request(&record, capture)
                .map_err(|error| error.to_string())?;
            ctx.worktrees.start_provider_worker(record.clone())?;
            Ok(json!({
                "accepted": "cleanup_worktree_artifacts",
                "operation": "cleanupWorktree",
                "target": target.authorization_target,
                "retirementReservation": crate::worktree_coordinator::RetirementReservation::from(&record),
                "targetCount": target_count,
                "delegatedAdmin": audit,
                "audited": true,
                "sourceRemovalPerformed": false,
            }))
        })();
        if let Err(error) = &execution {
            let _ = ctx.worktrees.transition(
                &record.operation_id,
                crate::worktree_coordinator::RetirementState::RecoveryRequired,
                Some(error.clone()),
            );
        }
        execution
    })();
    record_delegated_admin_execution_outcome(ctx, &audit, &result);
    result
}

pub(super) fn recover_worktree_artifacts(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "recover_worktree_artifacts",
        &["worktreePath", "operationId", "approvalId", "confirm"],
    )?;
    require_socket_identity(caller, trusted_internal, "recover_worktree_artifacts")?;
    let caller = caller
        .ok_or("recover_worktree_artifacts requires the exact delegated Crew session identity")?;
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(
            "recover_worktree_artifacts requires explicit confirmation before mutation".into(),
        );
    }
    let requested_path = arg_str(args, "worktreePath")
        .filter(|path| !path.trim().is_empty())
        .ok_or("recover_worktree_artifacts requires a non-empty worktreePath")?;
    let operation_id = arg_str(args, "operationId")
        .filter(|operation_id| !operation_id.trim().is_empty())
        .ok_or("recover_worktree_artifacts requires an exact operationId")?;
    let approval_id = arg_str(args, "approvalId")
        .filter(|approval_id| !approval_id.trim().is_empty())
        .ok_or("recover_worktree_artifacts requires an exact approvalId")?;
    ctx.worktrees.require_provider_configured()?;

    let target = resolve_admin_worktree_target(ctx, &requested_path)?;
    let (ship_slug, path) = match &target.authorization_target {
        crate::delegated_admin::AdminTarget::Worktree {
            ship_slug,
            worktree_id,
        } => (ship_slug.clone(), worktree_id.clone()),
        _ => return Err("recover_worktree_artifacts requires an exact worktree target".into()),
    };
    require_registered_git_capability(ctx, "recover_worktree_artifacts", &path)?;
    let record = ctx
        .worktrees
        .recovery_record(&operation_id, &path)
        .map_err(|error| error.to_string())?;
    let capture = crate::worktree_coordinator::inspect_cleanup_candidate(&path)?;
    ctx.worktrees.validate_recovery_capture(&record, &capture)?;
    let leased_sessions = tmux::pane_info()
        .map_err(|error| format!("recover_worktree_artifacts lease inspection failed: {error}"))?
        .into_iter()
        .filter(|pane| crate::worktree_coordinator::path_within(&pane.cwd, &path))
        .map(|pane| pane.session)
        .collect::<Vec<_>>();
    if !leased_sessions.is_empty() {
        return Err(format!(
            "recover_worktree_artifacts refuses worktree '{path}' because live sessions are present: {}",
            leased_sessions.join(", ")
        ));
    }

    let authorization_target = crate::delegated_admin::AdminTarget::WorktreeRetirement {
        ship_slug,
        worktree_id: path.clone(),
        operation_id: operation_id.to_string(),
    };
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
            crate::delegated_admin::AdminOperation::CleanupWorktree,
            &authorization_target,
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    let evidence_id = {
        let body = serde_json::to_vec(&json!({
            "operationId": operation_id,
            "target": authorization_target,
            "worktree": capture.worktree,
            "targets": capture.targets,
        }))
        .expect("worktree recovery evidence is serializable");
        format!("sha256:{:x}", Sha256::digest(body))
    };
    let audit = authorize_delegated_admin(
        ctx,
        caller,
        crate::delegated_admin::AdminOperation::CleanupWorktree,
        authorization_target.clone(),
        crate::delegated_admin::AdminSafeguards {
            authoritative_ownership_verified: true,
            consumed_approval: Some(consumed_approval),
            worktree_safety: Some(crate::delegated_admin::WorktreeSafetyEvidence {
                evidence_id,
                target_fingerprint: authorization_target.fingerprint(),
                removable: true,
            }),
        },
    )?;

    let result: Result<Value, String> = (|| {
        let current_target = resolve_admin_worktree_target(ctx, &path)?;
        let current_authorization_target = match current_target.authorization_target {
            crate::delegated_admin::AdminTarget::Worktree {
                ship_slug,
                worktree_id,
            } => crate::delegated_admin::AdminTarget::WorktreeRetirement {
                ship_slug,
                worktree_id,
                operation_id: operation_id.to_string(),
            },
            _ => return Err("recover_worktree_artifacts target identity is invalid".into()),
        };
        if current_authorization_target != authorization_target {
            return Err(
                "delegated admin: exact target ownership changed before cleanup recovery".into(),
            );
        }
        let current_record = ctx
            .worktrees
            .recovery_record(&operation_id, &path)
            .map_err(|error| error.to_string())?;
        if current_record != record {
            return Err("cleanup recovery reservation changed before worker ownership".into());
        }
        let resumed = ctx.worktrees.resume_recovery_worker(record.clone())?;
        Ok(json!({
            "accepted": "recover_worktree_artifacts",
            "operation": "cleanupWorktree",
            "target": authorization_target,
            "retirementReservation": crate::worktree_coordinator::RetirementReservation::from(&record),
            "resumed": resumed,
            "delegatedAdmin": audit,
            "audited": true,
            "sourceRemovalPerformed": false,
        }))
    })();
    record_delegated_admin_execution_outcome(ctx, &audit, &result);
    result
}

pub(super) fn authorize_delegated_admin(
    ctx: &ControlContext,
    caller: &ResolvedIdentity,
    operation: crate::delegated_admin::AdminOperation,
    target: crate::delegated_admin::AdminTarget,
    safeguards: crate::delegated_admin::AdminSafeguards,
) -> Result<crate::delegated_admin::AdminAuditContext, String> {
    let grant = ctx
        .delegated_admin
        .grants_for_actor(&caller.session_id)
        .into_iter()
        .find(|grant| grant.state.is_active())
        .ok_or("delegated admin: caller has no active administrative grant")?;
    let supervisor = current_delegating_supervisor(ctx, &grant);
    let actor = current_admin_actor(ctx, &grant);
    let audit = ctx
        .delegated_admin
        .authorize(
            &crate::delegated_admin::AdminActor {
                identity_id: caller.session_id.clone(),
                session_tile: caller.tile.clone(),
                ..actor
            },
            &supervisor,
            operation,
            &target,
            &safeguards,
        )
        .map_err(|error| format!("{}: {error}", error.code()))?;
    Ok(audit)
}

pub(super) fn record_delegated_admin_outcome<T>(
    ctx: &ControlContext,
    audit: Option<&crate::delegated_admin::AdminAuditContext>,
    result: &Result<T, String>,
) {
    let Some(audit) = audit else {
        return;
    };
    let decision = if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    let audit_value = json!({
        "authorization": audit,
        "outcome": decision,
        "error": result.as_ref().err(),
    });
    ctx.audit.record(
        "delegated_admin_operation",
        "organization",
        decision,
        &audit_value,
        AuditMeta {
            peer: if ctx.peer_is_loopback {
                "loopback"
            } else {
                "remote"
            },
            token_tier: "delegated",
            session: audit.actor_session_tile.as_deref(),
            spawned_by: None,
            error: result.as_ref().err().map(String::as_str),
        },
    );
}

/// Record the bounded typed result of an explicit administration command.
///
/// Other delegated reads intentionally use [`record_delegated_admin_outcome`]
/// without serializing their result because terminal scrollback and status
/// payloads may contain sensitive or unbounded user data.
pub(super) fn record_delegated_admin_execution_outcome(
    ctx: &ControlContext,
    audit: &crate::delegated_admin::AdminAuditContext,
    result: &Result<Value, String>,
) {
    let decision = if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    let audit_value = json!({
        "authorization": audit,
        "outcome": decision,
        "result": result.as_ref().ok(),
        "error": result.as_ref().err(),
    });
    ctx.audit.record(
        "delegated_admin_operation",
        "organization",
        decision,
        &audit_value,
        AuditMeta {
            peer: if ctx.peer_is_loopback {
                "loopback"
            } else {
                "remote"
            },
            token_tier: "delegated",
            session: audit.actor_session_tile.as_deref(),
            spawned_by: None,
            error: result.as_ref().err().map(String::as_str),
        },
    );
}

/// Comms-plane Phase 3: operate-fleet-infra (§2.7 R-L2). The plane's own administrative
/// operations (queue purge/flush) gated to the apex fleet-infra owner
/// (`can_operate_fleet_infra`): a captain/crew may NOT administer the shared plane. The
/// only op today is `purge` (reset a wedged recipient's queue) - the "flushing/
/// administering queues" surface the design names; WHO holds it is a matrix policy call.
pub(super) fn plane_admin(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_socket_identity(caller, trusted_internal, "plane_admin")?;
    if let Some(id) = caller {
        let actor = acl_actor(id);
        if let Err(d) = crate::acl::can_operate_fleet_infra(&actor) {
            ctx.fanout.emit_event(
                "control://acl",
                &json!({
                    "cell": "operate-fleet-infra",
                    "decision": "refused",
                    "session": actor.session_id.as_str(),
                    "role": actor.role.label(),
                    "reason": d.reason.as_str(),
                }),
            );
            return Err(format!("acl: {}", d.reason));
        }
    }
    // A caller without a session identity reached this point only with in-process host proof.
    let op = arg_str(args, "op").unwrap_or_default();
    match op.as_str() {
        "purge" => {
            let recipient = arg_str(args, "recipient")
                .or_else(|| arg_str(args, "sessionId"))
                .ok_or("plane_admin op=purge requires a 'recipient' tile id")?;
            let removed = ctx.inbox.purge_recipient(&recipient);
            Ok(json!({
                "accepted": "plane_admin",
                "op": "purge",
                "recipient": recipient,
                "removed": removed,
            }))
        }
        other => Err(format!(
            "plane_admin: unknown op '{other}' (supported: 'purge')"
        )),
    }
}
