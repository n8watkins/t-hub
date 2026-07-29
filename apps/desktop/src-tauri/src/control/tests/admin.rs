use super::*;

fn fleet_admin_grant_fixture(
    tag: &str,
) -> (
    ControlContext,
    crate::identity::SessionIdentity,
    crate::identity::SessionIdentity,
    String,
    String,
) {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let cortana_tile = format!("co{}", &nonce[..6]);
    let captain_tile = format!("ca{}", &nonce[..6]);
    let admin_tile = format!("fa{}", &nonce[..6]);
    let ctx = test_ctx(&format!("fleet-grant-{tag}-{}", &nonce[..6]));
    ctx.captains
        .claim_test(&captain_tile, Some(&format!("ship-{tag}")), vec![])
        .unwrap();
    ctx.captains
        .record_crew(&captain_tile, &admin_tile)
        .unwrap();
    create_test_tmux_session(&tmux_target(&admin_tile)).unwrap();

    let cortana_secret = mint_current_cortana_session(&ctx.identity, &ctx.captains, &cortana_tile);
    let cortana_identity = ctx.identity.resolve(&cortana_secret).unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_tile)
        .unwrap();
    let cortana = resolve_identity(&ctx, &cortana_secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_tile,
            "role": "fleetAdmin",
            "permittedOperations": ["maintainFleetResource"]
        }),
        Some(&cortana),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap().to_string();
    (ctx, cortana_identity, admin_identity, grant_id, admin_tile)
}

#[test]
fn captain_appoints_and_revokes_a_ship_admin_for_exact_ship_inspection() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("delegated-admin");
    ctx.captains
        .claim_test("captain-admin", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-admin", "crew-admin")
        .unwrap();
    let admin_target = tmux_target("crew-admin");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-admin")
        .unwrap();
    let crew_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&crew_identity.id, "crew-admin")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "crew-admin",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus", "maintainSession"],
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap().to_string();
    let crew = resolve_identity(&ctx, &crew_identity.secret).unwrap();
    let audit = authorize_delegated_admin(
        &ctx,
        &crew,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::CrewSession {
            ship_slug: "alpha".into(),
            session_id: "crew-peer".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap();
    assert_eq!(audit.actor_identity_id, crew_identity.id);
    assert_eq!(audit.delegating_supervisor_identity_id, captain_identity.id);
    let foreign = authorize_delegated_admin(
        &ctx,
        &crew,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::CrewSession {
            ship_slug: "beta".into(),
            session_id: "foreign".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(foreign.contains("targetOutOfScope"));

    revoke_admin(
        &ctx,
        &json!({ "grantId": grant_id, "reason": "rotation" }),
        Some(&captain),
        false,
    )
    .unwrap();
    let revoked = authorize_delegated_admin(
        &ctx,
        &crew,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(revoked.contains("no active administrative grant"));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_grant_fails_closed_for_ambiguous_delegator_ship() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("delegated-admin-ambiguous");
    ctx.captains
        .claim_test("captain-admin", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-admin", "crew-admin")
        .unwrap();
    let admin_target = tmux_target("crew-admin");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-admin")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "crew-admin")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "crew-admin",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"],
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap();

    let mut duplicate = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|record| record.terminal_id.as_deref() == Some("captain-admin"))
        .unwrap();
    duplicate.assignment_id = "ambiguous-assignment".into();
    duplicate.terminal_id = Some("captain-duplicate".into());
    ctx.captains.lock().captains.push(duplicate);

    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let denied = authorize_delegated_admin(
        &ctx,
        &admin,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(denied.contains("supervisorInactive"), "{denied}");
    assert!(matches!(
        ctx.delegated_admin.get(grant_id).unwrap().state,
        crate::delegated_admin::GrantState::Invalidated { .. }
    ));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_executes_own_ship_operations_and_denies_foreign_or_reserved_targets() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captain_alpha = format!("ca{}", &nonce[..6]);
    let captain_beta = format!("cb{}", &nonce[..6]);
    let admin_session = format!("aa{}", &nonce[..6]);
    let peer_alpha = format!("pa{}", &nonce[..6]);
    let peer_beta = format!("pb{}", &nonce[..6]);
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-admin-execute-audit-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("admin-execute-ship").with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.captains
        .claim_test(&captain_alpha, Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .claim_test(&captain_beta, Some("beta"), vec![])
        .unwrap();
    for crew in [&admin_session, &peer_alpha] {
        ctx.captains.record_crew(&captain_alpha, crew).unwrap();
    }
    ctx.captains.record_crew(&captain_beta, &peer_beta).unwrap();
    let session_ids = [
        admin_session.as_str(),
        peer_alpha.as_str(),
        peer_beta.as_str(),
    ];
    for session_id in session_ids {
        create_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, &captain_alpha)
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_session)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_session,
            "role": "shipAdmin",
            "permittedOperations": [
                "maintainSession",
                "recoverResource",
                "prepareRetirement"
            ]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();

    let own = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainSession",
            "target": { "kind": "session", "sessionId": admin_session }
        }),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(own["outcome"]["outcome"], "maintained");
    assert_eq!(
        own["outcome"]["maintainedSessions"][0]["sessionId"],
        admin_session
    );

    let sibling = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": { "kind": "session", "sessionId": peer_alpha }
        }),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(sibling["outcome"]["outcome"], "maintained");

    let retirement = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "prepareRetirement",
            "target": { "kind": "session", "sessionId": peer_alpha }
        }),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(retirement["outcome"]["outcome"], "retirementPrepared");
    assert_eq!(retirement["outcome"]["ready"], false);
    assert!(retirement["outcome"]["planId"]
        .as_str()
        .is_some_and(|plan| plan.starts_with("sha256:")));

    let foreign = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainSession",
            "target": { "kind": "session", "sessionId": peer_beta }
        }),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(foreign.contains("targetOutOfScope"));

    let reserved = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": { "kind": "generalReserved", "action": "installRelease" }
        }),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(reserved.contains("targetOutOfScope"));

    let implementation = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": {
                "kind": "implementation",
                "shipSlug": "alpha",
                "assignmentId": "assignment-1"
            }
        }),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(implementation.contains("targetOutOfScope"));

    let records = read_audit(&audit_dir);
    let operation_records = records
        .iter()
        .filter(|record| record["command"] == "delegated_admin_operation")
        .collect::<Vec<_>>();
    assert_eq!(operation_records.len(), 3);
    assert_eq!(
        operation_records[0]["args"]["authorization"]["actorIdentityId"],
        admin_identity.id
    );
    assert_eq!(
        operation_records[0]["args"]["authorization"]["delegatingSupervisorIdentityId"],
        captain_identity.id
    );
    assert_eq!(
        operation_records[0]["args"]["result"]["outcome"]["outcome"],
        "maintained"
    );

    for session_id in session_ids {
        reap_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
    std::fs::remove_dir_all(audit_dir).ok();
}

#[test]
fn fleet_admin_maintains_captains_without_crossing_into_crew_or_general_authority() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let cortana_session = format!("co{}", &nonce[..6]);
    let captain_alpha = format!("ca{}", &nonce[..6]);
    let captain_beta = format!("cb{}", &nonce[..6]);
    let fleet_admin_session = format!("fa{}", &nonce[..6]);
    let peer_beta = format!("pb{}", &nonce[..6]);
    let ctx = test_ctx(&format!("admin-execute-fleet-{}", &nonce[..8]));
    ctx.captains
        .claim_provider(
            &cortana_session,
            None,
            FleetRole::Cortana,
            Some("codex"),
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    ctx.captains
        .claim_test(&captain_alpha, Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .claim_test(&captain_beta, Some("beta"), vec![])
        .unwrap();
    ctx.captains
        .record_crew(&captain_alpha, &fleet_admin_session)
        .unwrap();
    ctx.captains.record_crew(&captain_beta, &peer_beta).unwrap();
    let session_ids = [
        fleet_admin_session.as_str(),
        captain_alpha.as_str(),
        captain_beta.as_str(),
        peer_beta.as_str(),
    ];
    for session_id in session_ids {
        create_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
    let cortana_identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    ctx.identity
        .bind_tile(&cortana_identity.id, &cortana_session)
        .unwrap();
    ctx.captains
        .begin_cortana_recovery("fleet-admin-test")
        .unwrap();
    ctx.captains
        .commit_cortana_runtime(
            "fleet-admin-test",
            &cortana_identity.id,
            1,
            &cortana_session,
            "codex",
            None,
        )
        .unwrap();
    let fleet_admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&fleet_admin_identity.id, &fleet_admin_session)
        .unwrap();
    let cortana = resolve_identity(&ctx, &cortana_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": fleet_admin_session,
            "role": "fleetAdmin",
            "permittedOperations": [
                "maintainFleetResource",
                "recoverResource",
                "prepareRetirement"
            ]
        }),
        Some(&cortana),
        false,
    )
    .unwrap();
    let fleet_admin = resolve_identity(&ctx, &fleet_admin_identity.secret).unwrap();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &fleet_admin_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let renewed = renewed.result.unwrap();
    assert_eq!(renewed["scope"]["kind"], "delegatedAdmin");
    assert_eq!(renewed["scope"]["role"], "fleetAdmin");
    let fleet_admin_lease = renewed["lease"].as_str().unwrap();
    let leased_mutation = dispatch_authenticated(
        &ctx,
        req_session(
            fleet_admin_lease,
            &fleet_admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "maintainFleetResource",
                "target": { "kind": "fleet" }
            }),
        ),
    );
    assert!(leased_mutation.ok, "{:?}", leased_mutation.error);

    let maintained = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainFleetResource",
            "target": { "kind": "fleet" }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap();
    assert_eq!(maintained["outcome"]["outcome"], "maintained");
    assert_eq!(
        maintained["outcome"]["maintainedSessions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let retirement = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "prepareRetirement",
            "target": { "kind": "session", "sessionId": captain_beta }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap();
    assert_eq!(retirement["outcome"]["ready"], false);

    let crew_denied = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": { "kind": "session", "sessionId": peer_beta }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap_err();
    assert!(crew_denied.contains("targetOutOfScope"));

    let general_denied = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainFleetResource",
            "target": { "kind": "generalReserved", "action": "approveRelease" }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap_err();
    assert!(general_denied.contains("targetOutOfScope"));

    for session_id in session_ids {
        reap_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
}

#[test]
fn fleet_admin_grants_invalidate_with_every_non_authoritative_cortana_state() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    for state in ["recovering", "degraded", "duplicate"] {
        let (ctx, _cortana_identity, admin_identity, grant_id, admin_tile) =
            fleet_admin_grant_fixture(state);
        match state {
            "recovering" => {
                ctx.captains
                    .begin_cortana_recovery("test-recovering")
                    .unwrap();
            }
            "degraded" => {
                ctx.captains
                    .mark_cortana_degraded("test-degraded", "injected uncertainty")
                    .unwrap();
            }
            "duplicate" => {
                let mut duplicate = ctx
                    .captains
                    .snapshot()
                    .captains
                    .into_iter()
                    .find(|captain| captain.role == FleetRole::Cortana)
                    .unwrap();
                duplicate.ship_slug = "cortana-duplicate".into();
                duplicate.assignment_id = "cortana-duplicate-assignment".into();
                duplicate.terminal_id = Some("duplicate".into());
                ctx.captains.lock().captains.push(duplicate);
            }
            _ => unreachable!(),
        }

        let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
        let denied = authorize_delegated_admin(
            &ctx,
            &admin,
            crate::delegated_admin::AdminOperation::MaintainFleetResource,
            crate::delegated_admin::AdminTarget::Fleet,
            crate::delegated_admin::AdminSafeguards::default(),
        )
        .unwrap_err();
        assert!(
            denied.contains("supervisorInactive")
                || denied.contains("no active administrative grant"),
            "unexpected {state} denial: {denied}"
        );
        assert!(matches!(
            ctx.delegated_admin.get(&grant_id).unwrap().state,
            crate::delegated_admin::GrantState::Invalidated { .. }
        ));
        reap_test_tmux_session(&tmux_target(&admin_tile)).unwrap();
    }
}

#[test]
fn dispatch_capacity_counts_one_live_harness_backed_ship_admin_per_ship() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-capacity");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let mut admin_targets = Vec::new();

    for admin_id in ["adminalfa", "adminbeta"] {
        ctx.captains.record_crew("captain-alpha", admin_id).unwrap();
        ctx.captains
            .bind_crew_context(
                "captain-alpha",
                admin_id,
                "standing administration",
                "codex",
                None,
                None,
                PowderWorkBinding {
                    card_id: format!("card-{admin_id}"),
                    run_id: format!("run-{admin_id}"),
                    agent: None,
                    claim_expires_at: None,
                    mutation_intent: None,
                    dispatch_release_recovery: false,
                    state: PowderWorkState::Active,
                },
            )
            .unwrap();
        let target = tmux_target(admin_id);
        tmux::new_session_with_env(&target, "/tmp", Some(&harness_command), &[]).unwrap();
        wait_for_harness_started(admin_id, "codex").unwrap();
        admin_targets.push(target);

        let identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
        ctx.identity.bind_tile(&identity.id, admin_id).unwrap();
        appoint_admin(
            &ctx,
            &json!({
                "actorSessionId": admin_id,
                "role": "shipAdmin",
                "permittedOperations": ["inspectStatus"]
            }),
            Some(&captain),
            false,
        )
        .unwrap();
    }

    assert_eq!(
        live_admin_counts(&ctx, &ctx.captains.snapshot()),
        (0, [("alpha".to_string(), 2usize)].into_iter().collect())
    );
    reap_test_tmux_session(&admin_targets[0]).unwrap();
    assert_eq!(
        live_admin_counts(&ctx, &ctx.captains.snapshot()),
        (0, [("alpha".to_string(), 1usize)].into_iter().collect())
    );
    reap_test_tmux_session(&admin_targets[1]).unwrap();
    assert_eq!(
        live_admin_counts(&ctx, &ctx.captains.snapshot()),
        (0, BTreeMap::new())
    );
    std::fs::remove_dir_all(harness_bin_dir).ok();
}

#[test]
fn ship_admin_can_read_own_captain_status_but_cannot_run_dispatch_preflight() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("ship-admin-status");
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-alpha".into(),
            name: "Alpha".into(),
            repo_root: "/tmp/project-alpha".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("alpha", "project-alpha", "Assignment", "codex")
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();

    let report = list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(report["count"], 0);

    let denied = authorize_agent_filter(
        &ctx,
        Some("captain-alpha"),
        Some("project-alpha"),
        Some(&admin),
        false,
        "dispatch_preflight",
        false,
    )
    .unwrap_err();
    assert!(denied.contains("owning Captain or a fleet supervisor"));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_worktree_maintenance_is_scoped_and_cannot_dispatch_implementation() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let provider_probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_probe_count = provider_probes.clone();
    let tmux_probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tmux_probe_count = tmux_probes.clone();
    let ctx = test_ctx("ship-admin-worktree")
        .with_apply_sink(sink.clone())
        .with_provider_capacity(move || {
            provider_probe_count.fetch_add(1, Ordering::SeqCst);
            Err("provider admission unavailable".into())
        })
        .with_live_sessions(move || {
            tmux_probe_count.fetch_add(1, Ordering::SeqCst);
            Err("tmux admission unavailable".into())
        });
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let (base, repo_root, _existing_worktree) = scratch_repo_with_worktree();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-alpha".into(),
            name: "Alpha".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("alpha", "project-alpha", "Assignment", "codex")
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["maintainWorktree"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let target = base.join("admin-worktree").to_string_lossy().to_string();

    let audit = authorize_worktree_maintenance(
        &ctx,
        Some(&admin),
        false,
        &json!({}),
        repo_root.to_str().unwrap(),
        &target,
        None,
        None,
    )
    .unwrap()
    .expect("delegated audit context");
    assert_eq!(audit.delegated_role.label(), "shipAdmin");
    assert_eq!(
        audit.target.fingerprint(),
        format!("worktree:alpha:{target}")
    );

    let denied = authorize_worktree_maintenance(
        &ctx,
        Some(&admin),
        false,
        &json!({"startupCommand": "codex exec implement"}),
        repo_root.to_str().unwrap(),
        &target,
        Some("codex exec implement"),
        None,
    )
    .unwrap_err();
    assert!(denied.contains("cannot create or elevate a runtime"));

    let tabs_before = ctx.tabs.snapshot_full();
    let identities_before = ctx.identity.len();
    let maintained = dispatch_authenticated(
        &ctx,
        req_session(
            "ship-admin-worktree",
            &admin_identity.secret,
            "create_worktree",
            json!({
                "repoRoot": repo_root,
                "worktreePath": target,
            }),
        ),
    );
    assert!(
        maintained.ok,
        "maintenance-only create was governed as a spawn: {:?}",
        maintained.error
    );
    let maintained = maintained.result.unwrap();
    assert_eq!(maintained["administrativeMaintenanceOnly"], true);
    assert!(maintained["tabId"].is_null());
    assert!(maintained["terminalId"].is_null());
    assert!(std::path::Path::new(&target).exists());
    assert_eq!(ctx.tabs.snapshot_full().seq, tabs_before.seq);
    assert_eq!(
        serde_json::to_value(ctx.tabs.snapshot_full().tabs).unwrap(),
        serde_json::to_value(tabs_before.tabs).unwrap()
    );
    assert_eq!(ctx.identity.len(), identities_before);
    // The tmux namespace is shared by concurrent tests, so verify this
    // fixture's exact runtime instead of comparing the global session list.
    assert_eq!(
        tmux::session_liveness(&admin_target),
        tmux::SessionLiveness::Alive
    );
    assert!(sink.calls.lock().unwrap().is_empty());
    assert_eq!(provider_probes.load(Ordering::SeqCst), 0);
    assert_eq!(tmux_probes.load(Ordering::SeqCst), 0);

    let elevated_target = base.join("elevated-worktree");
    let elevated = dispatch_authenticated(
        &ctx,
        req_session(
            "ship-admin-worktree",
            &admin_identity.secret,
            "create_worktree",
            json!({
                "repoRoot": repo_root,
                "worktreePath": elevated_target,
                "capability": "control",
            }),
        ),
    );
    assert!(!elevated.ok);
    let elevated = elevated.error.unwrap();
    assert!(elevated.contains("cannot create or elevate a runtime"));
    assert!(!elevated_target.exists());
    assert_eq!(ctx.tabs.snapshot_full().seq, tabs_before.seq);
    assert_eq!(ctx.identity.len(), identities_before);
    assert_eq!(
        tmux::session_liveness(&admin_target),
        tmux::SessionLiveness::Alive
    );
    assert!(sink.calls.lock().unwrap().is_empty());
    assert_eq!(provider_probes.load(Ordering::SeqCst), 0);
    assert_eq!(tmux_probes.load(Ordering::SeqCst), 0);
    reap_test_tmux_session(&admin_target).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn list_captains_exposes_active_admin_role_without_granting_captain_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-role-wire");
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();

    let roster = list_captains(&ctx).unwrap();
    assert_eq!(
        roster["captains"][0]["crew"][0]["delegatedRole"],
        "shipAdmin"
    );
    assert!(roster["captains"][0]["crew"][0]["delegatedGrantGeneration"]
        .as_u64()
        .is_some_and(|generation| generation > 0));
    assert_eq!(roster["captains"][0]["crew"][0]["terminalId"], admin_id);
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_session_cleanup_requires_and_consumes_exact_supervisor_approval() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-cleanup");
    let crew_target_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &crew_target_id)
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let grant = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["cleanupSession"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let target = tmux_target(&crew_target_id);
    create_test_tmux_session(&target).unwrap();

    let denied = close_terminal_authorized(
        &ctx,
        &json!({"sessionId": crew_target_id}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(denied.contains("exact supervisor approvalId"));
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );

    let fabricated = approve_admin_action(
        &ctx,
        &json!({
            "grantId": grant["grant"]["grantId"],
            "operation": "cleanupSession",
            "target": {
                "kind": "crewSession",
                "shipSlug": "alpha",
                "sessionId": crew_target_id
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(fabricated.contains("sessionId only"));

    let approval = approve_admin_action(
        &ctx,
        &json!({
            "grantId": grant["grant"]["grantId"],
            "operation": "cleanupSession",
            "sessionId": crew_target_id,
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(approval["approval"]["target"]["kind"], "crewSession");
    assert_eq!(approval["approval"]["target"]["shipSlug"], "alpha");
    let approval_id = approval["approval"]["approval"]["approvalId"]
        .as_str()
        .unwrap();
    let closed = close_terminal_authorized(
        &ctx,
        &json!({"sessionId": crew_target_id, "approvalId": approval_id}),
        Some(&admin),
        false,
    )
    .unwrap();

    assert_eq!(closed["outcome"], "killed");
    assert_eq!(tmux::session_liveness(&target), tmux::SessionLiveness::Gone);
    assert!(ctx
        .delegated_admin
        .get_approval(approval_id)
        .is_some_and(|approval| matches!(
            approval.state,
            crate::delegated_admin::ApprovalState::Consumed { .. }
        )));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn delegated_admin_control_token_is_limited_to_role_aware_routes() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-boundaries");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["maintainSession"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &admin_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let renewed = renewed.result.unwrap();
    assert_eq!(renewed["scope"]["kind"], "delegatedAdmin");
    assert_eq!(renewed["scope"]["role"], "shipAdmin");
    let admin_lease = renewed["lease"].as_str().unwrap().to_string();

    let read_denied = read_terminal(
        &ctx,
        &json!({"sessionId": "admin-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(read_denied.contains("operationNotGranted"));
    let list_denied = list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(list_denied.contains("operationNotGranted"));
    let close_denied = close_terminal_authorized(
        &ctx,
        &json!({"sessionId": "admin-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(close_denied.contains("exact supervisor approvalId"));
    assert_eq!(
        tmux::session_liveness(&admin_target),
        tmux::SessionLiveness::Alive
    );

    for command in [
        "spawn_terminal",
        "start_agent",
        "dispatch_crew",
        "send_text",
        "move_tile",
        "register_project",
        "appoint_admin",
    ] {
        let response = dispatch_authenticated(
            &ctx,
            req_session(&admin_lease, &admin_identity.secret, command, json!({})),
        );
        assert!(
            !response.ok,
            "delegated admin unexpectedly called {command}"
        );
        assert!(
            response
                .error
                .unwrap_or_default()
                .contains("outside their exact administrative operation grants"),
            "{command} did not fail at the delegated-role boundary"
        );
    }
    assert!(
        enforce_attach_authority(&ctx, Some(&admin), false, "admin-alpha", FleetRole::Captain,)
            .unwrap_err()
            .contains("cannot acquire Captain or Cortana authority")
    );
    assert!(appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["maintainSession"]
        }),
        Some(&admin),
        false,
    )
    .unwrap_err()
    .contains("cannot re-delegate authority"));

    let maintained = dispatch_authenticated(
        &ctx,
        req_session(
            &admin_lease,
            &admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "maintainSession",
                "target": { "kind": "session", "sessionId": "admin-alpha" }
            }),
        ),
    );
    assert!(
        maintained.ok,
        "role-authorized maintenance route failed: {:?}",
        maintained.error
    );

    let grants = dispatch_authenticated(
        &ctx,
        req_session(
            &admin_lease,
            &admin_identity.secret,
            "list_admin_grants",
            json!({}),
        ),
    );
    assert!(grants.ok, "self grant listing failed: {:?}", grants.error);
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn privileged_agent_intent_is_permanent_admin_history_before_appointment_and_reload() {
    for (label, purpose) in [
        ("fleet", crate::governor::AdmissionPurpose::FleetAdmin),
        ("ship", crate::governor::AdmissionPurpose::ShipAdmin),
        ("recovery", crate::governor::AdmissionPurpose::Recovery),
    ] {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let captains_path = captains_tmp(&format!("intent-history-{label}-{nonce}"));
        let identities_path = std::env::temp_dir().join(format!(
            "t-hub-intent-history-identities-{label}-{nonce}.json"
        ));
        let agent_id = format!("{}{}", &label[..2], &nonce[..6]);
        let admin_secret;
        let general_secret;

        {
            let captains = Arc::new(CaptainsRegistry::load(captains_path.clone()));
            let identities = Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            ));
            let ctx = test_ctx(&format!("intent-history-{label}"))
                .with_captains_registry(captains)
                .with_identity_store(identities);
            seed_starting_agent_with_purpose(&ctx, &agent_id, purpose);
            admin_secret = mint_session(
                &ctx.identity,
                crate::identity::Role::Crew,
                "capacity-ship",
                &agent_id,
            );
            general_secret = mint_session(
                &ctx.identity,
                crate::identity::Role::General,
                "fleet",
                "general-intent",
            );
            let admin = resolve_identity(&ctx, &admin_secret).unwrap();
            assert!(has_delegated_admin_history(&ctx, &admin.session_id));

            let mutation = dispatch_authenticated(
                &ctx,
                req_session(
                    &format!("intent-history-{label}"),
                    &admin_secret,
                    "new_tab",
                    json!({"name": "forbidden-before-appointment"}),
                ),
            );
            assert!(!mutation.ok);
            assert!(mutation
                .error
                .unwrap_or_default()
                .contains("outside their exact administrative operation grants"));

            let general = resolve_identity(&ctx, &general_secret).unwrap();
            for role in [FleetRole::Captain, FleetRole::Cortana] {
                assert!(
                    enforce_attach_authority(&ctx, Some(&general), false, &agent_id, role,)
                        .unwrap_err()
                        .contains("administrative Crew identity")
                );
            }
            for (command, args) in [
                (
                    "claim_captain",
                    json!({"captainSessionId": &agent_id, "shipSlug": "forbidden"}),
                ),
                (
                    "attach_captain",
                    json!({
                        "captainSessionId": &agent_id,
                        "projectId": "capacity-project",
                        "assignment": "forbidden"
                    }),
                ),
            ] {
                let response = dispatch_authenticated(
                    &ctx,
                    req_session(
                        &format!("intent-history-{label}"),
                        &general_secret,
                        command,
                        args,
                    ),
                );
                assert!(!response.ok, "{command} promoted {label} intent");
                assert!(response
                    .error
                    .unwrap_or_default()
                    .contains("administrative Crew identity"));
            }
        }

        {
            let ctx = test_ctx(&format!("intent-history-{label}"))
                .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
                .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                    identities_path.clone(),
                )));
            let admin = resolve_identity(&ctx, &admin_secret).unwrap();
            assert!(has_delegated_admin_history(&ctx, &admin.session_id));
            let mutation = dispatch_authenticated(
                &ctx,
                req_session(
                    &format!("intent-history-{label}"),
                    &admin_secret,
                    "new_tab",
                    json!({"name": "forbidden-after-reload"}),
                ),
            );
            assert!(!mutation.ok);
            assert!(mutation
                .error
                .unwrap_or_default()
                .contains("outside their exact administrative operation grants"));
        }

        for path in [
            captains_path.with_extension("json.bak"),
            captains_path,
            identities_path,
        ] {
            std::fs::remove_file(path).ok();
        }
    }
}

#[test]
fn revoked_and_invalidated_admin_tokens_stay_restricted_after_reload() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captains_path = captains_tmp(&format!("admin-history-{nonce}"));
    let identities_path =
        std::env::temp_dir().join(format!("t-hub-admin-history-identities-{nonce}.json"));
    let grants_path = std::env::temp_dir().join(format!("t-hub-admin-history-grants-{nonce}.json"));
    let captain_tile = format!("cp{}", &nonce[..6]);
    let revoked_tile = format!("rv{}", &nonce[..6]);
    let invalidated_tile = format!("iv{}", &nonce[..6]);
    let captain_secret;
    let admin_credentials;

    {
        let captains = Arc::new(CaptainsRegistry::load(captains_path.clone()));
        captains
            .claim_test(&captain_tile, Some("alpha"), vec![])
            .unwrap();
        captains.record_crew(&captain_tile, &revoked_tile).unwrap();
        captains
            .record_crew(&captain_tile, &invalidated_tile)
            .unwrap();
        let identities = Arc::new(crate::identity::IdentityStore::load(
            identities_path.clone(),
        ));
        let grants = Arc::new(
            crate::delegated_admin::DelegatedAdminStore::load(grants_path.clone()).unwrap(),
        );
        let captain_identity = identities.mint(crate::identity::Role::Captain).unwrap();
        identities
            .bind_tile(&captain_identity.id, &captain_tile)
            .unwrap();
        captain_secret = captain_identity.secret.clone();
        let mut credentials = Vec::new();
        for tile in [&revoked_tile, &invalidated_tile] {
            create_test_tmux_session(&tmux_target(tile)).unwrap();
            let identity = identities.mint(crate::identity::Role::Crew).unwrap();
            identities.bind_tile(&identity.id, tile).unwrap();
            credentials.push((tile.clone(), identity.id, identity.secret));
        }
        let ctx = test_ctx("persisted-admin-token")
            .with_captains_registry(captains)
            .with_identity_store(identities)
            .with_delegated_admin(grants);
        let captain = resolve_identity(&ctx, &captain_secret).unwrap();
        for (tile, _, _) in &credentials {
            appoint_admin(
                &ctx,
                &json!({
                    "actorSessionId": tile,
                    "role": "shipAdmin",
                    "permittedOperations": ["maintainSession"]
                }),
                Some(&captain),
                false,
            )
            .unwrap();
        }
        admin_credentials = credentials;
    }

    // An active appointment and its exact operation survive a process reload.
    {
        let ctx = test_ctx("persisted-admin-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )))
            .with_delegated_admin(Arc::new(
                crate::delegated_admin::DelegatedAdminStore::load(grants_path.clone()).unwrap(),
            ));
        for (tile, _, secret) in &admin_credentials {
            let maintained = dispatch_authenticated(
                &ctx,
                req_session(
                    "persisted-admin-token",
                    secret,
                    "execute_admin_operation",
                    json!({
                        "operation": "maintainSession",
                        "target": { "kind": "session", "sessionId": tile }
                    }),
                ),
            );
            assert!(
                maintained.ok,
                "active admin reload failed: {:?}",
                maintained.error
            );
        }
        let captain = resolve_identity(&ctx, &captain_secret).unwrap();
        let revoked_grant = ctx
            .delegated_admin
            .grants_for_actor(&admin_credentials[0].1)
            .into_iter()
            .find(|grant| grant.state.is_active())
            .unwrap();
        revoke_admin(
            &ctx,
            &json!({ "grantId": revoked_grant.grant_id, "reason": "rotation" }),
            Some(&captain),
            false,
        )
        .unwrap();
        ctx.delegated_admin
            .invalidate_actor(&admin_credentials[1].1, "Crew ownership changed")
            .unwrap();
    }

    // Both durable tombstone forms continue to classify the old bearer as an
    // administrator after another reload. Full control admission cannot turn
    // either identity back into an ordinary mutator or a Captain claimant.
    {
        let ctx = test_ctx("persisted-admin-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )))
            .with_delegated_admin(Arc::new(
                crate::delegated_admin::DelegatedAdminStore::load(grants_path.clone()).unwrap(),
            ));
        for (tile, _, secret) in &admin_credentials {
            for (command, args) in [
                ("new_tab", json!({"name": "escaped"})),
                (
                    "claim_captain",
                    json!({"captainSessionId": tile, "shipSlug": "escaped"}),
                ),
                (
                    "attach_captain",
                    json!({
                        "captainSessionId": tile,
                        "projectId": "escaped",
                        "assignment": "escaped"
                    }),
                ),
            ] {
                let response = dispatch_authenticated(
                    &ctx,
                    req_session("persisted-admin-token", secret, command, args),
                );
                assert!(!response.ok, "historical admin called {command}");
                assert!(
                    response
                        .error
                        .unwrap_or_default()
                        .contains("outside their exact administrative operation grants"),
                    "historical admin did not fail at durable boundary for {command}"
                );
            }
            let resolved = resolve_identity(&ctx, secret).unwrap();
            assert!(enforce_attach_authority(
                &ctx,
                Some(&resolved),
                false,
                tile,
                FleetRole::Captain,
            )
            .unwrap_err()
            .contains("cannot acquire Captain or Cortana authority"));
        }
    }

    for (tile, _, _) in &admin_credentials {
        reap_test_tmux_session(&tmux_target(tile)).unwrap();
    }
    for path in [
        captains_path.with_extension("json.bak"),
        captains_path,
        identities_path,
        grants_path,
    ] {
        std::fs::remove_file(path).ok();
    }
}

#[test]
fn captain_release_invalidates_dependent_grants_before_reclaim() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-release");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap();

    release_captain(&ctx, &json!({"shipSlug": "alpha"}), Some(&captain), false).unwrap();
    assert!(matches!(
        ctx.delegated_admin.get(grant_id).unwrap().state,
        crate::delegated_admin::GrantState::Invalidated { .. }
    ));
    ctx.captains
        .claim_test("captain-replacement", Some("alpha"), vec![])
        .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let denied = authorize_delegated_admin(
        &ctx,
        &admin,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(denied.contains("no active administrative grant"));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn delegated_admin_operation_invalidates_a_transferred_actor() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-transfer");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .claim_test("captain-beta", Some("beta"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap();
    ctx.captains.rollback_crew("admin-alpha").unwrap();
    ctx.captains
        .record_crew("captain-beta", "admin-alpha")
        .unwrap();

    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let denied = authorize_delegated_admin(
        &ctx,
        &admin,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(denied.contains("actorMismatch"));
    assert!(matches!(
        ctx.delegated_admin.get(grant_id).unwrap().state,
        crate::delegated_admin::GrantState::Invalidated { .. }
    ));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn delegated_admin_audit_records_attributed_success_and_failure_outcomes() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-admin-outcome-audit-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("admin-audit").with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();

    list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap();
    list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha", "limit": 0}),
        Some(&admin),
        false,
    )
    .unwrap_err();

    let records = read_audit(&audit_dir);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["command"], "delegated_admin_operation");
    assert_eq!(records[0]["decision"], "succeeded");
    assert_eq!(records[0]["args"]["outcome"], "succeeded");
    assert_eq!(
        records[0]["args"]["authorization"]["actorIdentityId"],
        admin_identity.id
    );
    assert_eq!(
        records[0]["args"]["authorization"]["delegatingSupervisorIdentityId"],
        captain_identity.id
    );
    assert_eq!(records[1]["decision"], "failed");
    assert_eq!(records[1]["args"]["outcome"], "failed");
    assert!(records[1]["args"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("limit must be between")));

    reap_test_tmux_session(&admin_target).unwrap();
    std::fs::remove_dir_all(audit_dir).ok();
}
