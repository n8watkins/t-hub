use super::*;

fn history_service_at(root: &std::path::Path) -> Arc<crate::history::HistoryService> {
    let claude_root = root.join("claude");
    let codex_root = root.join("codex");
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::create_dir_all(&codex_root).unwrap();
    Arc::new(crate::history::HistoryService::new(
        claude_root,
        codex_root,
        Duration::from_secs(60),
    ))
}

fn seed_history_resume(
    history: &crate::history::HistoryService,
    request_id: &str,
    terminal_id: &str,
    complete: bool,
) -> (String, String) {
    let history_id = format!("history:v1:{request_id}");
    let conversation_id = format!("conversation-{request_id}");
    let pending = crate::history::HistoryPendingResume {
        request_id: request_id.to_string(),
        history_id: history_id.clone(),
        harness: crate::history::Harness::Codex,
        conversation_id: conversation_id.clone(),
        terminal_id: terminal_id.to_string(),
        target_tab_id: None,
        authorized_ship_slug: None,
        authorized_project_id: None,
        authorized_assignment_id: None,
        reserved_at_ms: now_ms(),
    };
    history.reserve_resume(pending).unwrap();
    if complete {
        history
            .record_resume(
                crate::history::HistoryBinding {
                    history_id: history_id.clone(),
                    harness: crate::history::Harness::Codex,
                    conversation_id: conversation_id.clone(),
                    terminal_id: terminal_id.to_string(),
                },
                crate::history::HistoryResumeOperation {
                    request_id: request_id.to_string(),
                    history_id: history_id.clone(),
                    harness: crate::history::Harness::Codex,
                    conversation_id: conversation_id.clone(),
                    terminal_id: terminal_id.to_string(),
                    target_tab_id: None,
                    actual_tab_id: None,
                    authorized_ship_slug: None,
                    authorized_project_id: None,
                    authorized_assignment_id: None,
                    recorded_at_ms: now_ms(),
                },
            )
            .unwrap();
    }
    (history_id, conversation_id)
}

#[test]
fn spawn_admission_fails_closed_without_tmux_evidence_and_preserves_rate_token() {
    let available = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let evidence = available.clone();
    let ctx = test_ctx("tmux-evidence")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(move || {
            if evidence.load(Ordering::SeqCst) {
                Ok(Vec::new())
            } else {
                Err("injected enumeration outage".into())
            }
        });

    let refused = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refused.code, "refused-evidence");
    assert!(refused.message.contains("injected enumeration outage"));

    available.store(true, Ordering::SeqCst);
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "an evidence refusal must not consume the sole rate token"
    );
}

#[test]
fn fresh_install_uses_reported_packaged_provider_policy() {
    let evidence = provider_capacity_from_environment(Err(std::env::VarError::NotPresent)).unwrap();
    assert_eq!(evidence.session_capacity, 32);
    assert_eq!(evidence.status.source, "packaged-conservative-policy-v1");
    assert!(evidence.status.degraded);
    assert!(evidence
        .status
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("live provider quota telemetry is unavailable")));

    let ctx = test_ctx("packaged-provider-policy")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_capacity_evidence(|| {
            provider_capacity_from_environment(Err(std::env::VarError::NotPresent))
        })
        .with_provider_live_sessions(|_| Ok(0));
    let admission = admit_spawn(&ctx, SpawnPurpose::Cortana, 1, None).unwrap();
    assert_eq!(admission._capacity.provider_session_limit, 32);
    assert_eq!(admission._capacity.provider_live_sessions, 0);
    assert_eq!(
        admission._capacity.provider_capacity_status.source,
        "packaged-conservative-policy-v1"
    );
    assert!(admission._capacity.provider_capacity_status.degraded);
}

#[test]
fn explicit_provider_capacity_configuration_is_validated_fail_closed() {
    for invalid in ["", "0", "unknown", "-1"] {
        let error = provider_capacity_from_environment(Ok(invalid.into())).unwrap_err();
        assert!(error.contains("must be a positive integer"), "got: {error}");
    }
    let configured = provider_capacity_from_environment(Ok("7".into())).unwrap();
    assert_eq!(configured.session_capacity, 7);
    assert_eq!(
        configured.status.source,
        "environment-override:T_HUB_PROVIDER_SESSION_CAPACITY"
    );
    assert!(!configured.status.degraded);
    let unavailable = provider_capacity_from_environment(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("configured-unavailable"),
    )))
    .unwrap_err();
    assert!(unavailable.contains("not valid Unicode"));
}

#[test]
fn legacy_captains_snapshot_derives_nested_provider_reservation_headroom() {
    let ctx = test_ctx("legacy-capacity-snapshot");
    seed_starting_agent(&ctx, "legacya1");
    let mut document = serde_json::to_value(ctx.captains.snapshot()).unwrap();
    let report = document["agentSessions"][0]["dispatchCapacity"]
        .as_object_mut()
        .expect("seeded dispatch report");
    let provider_headroom = report["providerHeadroom"].as_u64().unwrap() as usize;
    let reservation_deficit = report["reservations"]["totalDeficit"].as_u64().unwrap() as usize;
    report.remove("requestedProviderLanes");
    report.remove("providerHeadroomAfterReservations");

    let restored: CaptainsSnapshot = serde_json::from_value(document).unwrap();
    let restored = restored.agent_sessions[0]
        .dispatch_capacity
        .as_ref()
        .unwrap();
    assert_eq!(restored.requested_provider_lanes, restored.requested_lanes);
    assert_eq!(
        restored.provider_headroom_after_reservations,
        provider_headroom.saturating_sub(reservation_deficit)
    );
}

#[test]
fn provider_usage_attestation_excludes_generic_tmux_terminals() {
    if !tmux_process_tests_available() {
        eprintln!(
                "provider_usage_attestation_excludes_generic_tmux_terminals: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let generic_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let provider_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let generic_target = tmux_target(&generic_id);
    let provider_target = tmux_target(&provider_id);
    create_test_tmux_session(&generic_target).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    create_test_tmux_session_with_env(
        &provider_target,
        "/tmp",
        Some(&harness_command),
        &[(PROVIDER_SESSION_ENV.into(), "codex".into())],
    )
    .unwrap();
    wait_for_harness_started(&provider_id, "codex").unwrap();

    let snapshot = test_ctx("provider-usage-attestation").captains.snapshot();
    assert_eq!(
        inspect_provider_live_sessions(
            &snapshot,
            &[generic_target.clone(), provider_target.clone()]
        )
        .unwrap(),
        1
    );

    reap_test_tmux_session(&generic_target).unwrap();
    reap_test_tmux_session(&provider_target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
}

#[test]
fn pending_ui_provider_marker_consumes_quota_before_harness_readiness() {
    if !tmux_process_tests_available() {
        eprintln!(
                "pending_ui_provider_marker_consumes_quota_before_harness_readiness: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let provider_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let provider_target = tmux_target(&provider_id);
    create_test_tmux_session_with_env(
        &provider_target,
        "/tmp",
        None,
        &[(
            PROVIDER_SESSION_ENV.into(),
            pending_provider_marker("codex"),
        )],
    )
    .unwrap();

    let sessions = vec![provider_target.clone()];
    let listed = sessions.clone();
    let governor = SpawnGovernor::new(8, 20.0, 8.0).with_reservation_policy(
        crate::governor::ReservationPolicy {
            cortana: 0,
            fleet_admins: 0,
            ship_admins_per_active_captain: 0,
            recovery: 0,
        },
    );
    let ctx = test_ctx("pending-ui-provider")
        .with_governor(Arc::new(governor))
        .with_provider_capacity(|| Ok(1))
        .with_live_sessions(move || Ok(listed.clone()));
    assert_eq!(
        inspect_provider_live_sessions(&ctx.captains.snapshot(), &sessions).unwrap(),
        1
    );
    assert_eq!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None)
            .unwrap_err()
            .code,
        "provider-capacity"
    );
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 0, None).is_ok(),
        "a generic shell remains admissible at full provider quota"
    );

    reap_test_tmux_session(&provider_target).unwrap();
}

#[test]
fn pending_history_provider_intent_is_counted_but_its_own_admission_is_not_double_counted() {
    let temp = tempfile::tempdir().unwrap();
    let history = history_service_at(temp.path());
    seed_history_resume(&history, "pending-capacity", "histpend", false);
    let governor = SpawnGovernor::new(8, 20.0, 8.0).with_reservation_policy(
        crate::governor::ReservationPolicy {
            cortana: 0,
            fleet_admins: 0,
            ship_admins_per_active_captain: 0,
            recovery: 0,
        },
    );
    let ctx = test_ctx("pending-history-provider")
        .with_governor(Arc::new(governor))
        .with_history_service(history)
        .with_provider_capacity(|| Ok(1))
        .with_provider_live_sessions(|_| Ok(0))
        .with_live_sessions(|| Ok(Vec::new()));

    assert_eq!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None)
            .unwrap_err()
            .code,
        "provider-capacity"
    );
    assert!(admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, Some("histpend")).is_ok());
}

#[test]
fn spawn_admission_fails_closed_without_provider_evidence_and_at_provider_limit() {
    let available = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let evidence = available.clone();
    let ctx = test_ctx("provider-evidence")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_capacity(move || {
            if evidence.load(Ordering::SeqCst) {
                Ok(128)
            } else {
                Err("injected provider outage".into())
            }
        });
    let unavailable = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(unavailable.code, "refused-provider");
    assert!(unavailable.message.contains("injected provider outage"));
    available.store(true, Ordering::SeqCst);
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "a provider-evidence refusal must not consume the sole rate token"
    );

    let at_limit = test_ctx("provider-limit")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_live0001".into()]))
        .with_provider_capacity(|| Ok(1));
    let refusal = admit_spawn(&at_limit, SpawnPurpose::Cortana, 1, None).unwrap_err();
    assert_eq!(refusal.code, "provider-capacity");
}

#[test]
fn durable_starting_agent_consumes_capacity_before_tmux_exists() {
    let ctx = test_ctx("pending-start")
        .with_governor(Arc::new(SpawnGovernor::new(5, 20.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()));
    seed_starting_agent(&ctx, "pending1");

    assert_eq!(
        live_session_count(&ctx, &ctx.captains.snapshot()).unwrap(),
        1
    );
    let refusal = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refusal.code, "reserved-capacity");

    let same_runtime_visible = test_ctx("pending-visible")
        .with_governor(Arc::new(SpawnGovernor::new(8, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_pending2".into()]));
    seed_starting_agent(&same_runtime_visible, "pending2");
    assert_eq!(
        live_session_count(
            &same_runtime_visible,
            &same_runtime_visible.captains.snapshot()
        )
        .unwrap(),
        1,
        "a Starting record whose tmux session is visible must not be double counted"
    );
}

#[test]
fn durable_provider_intent_survives_starting_and_counts_once_when_tmux_appears() {
    let absent = test_ctx("pending-provider-absent")
        .with_governor(Arc::new(SpawnGovernor::new(16, 20.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_capacity(|| Ok(1))
        .with_provider_live_sessions(|_| Ok(0));
    seed_starting_agent(&absent, "pendprv1");
    let refusal = admit_spawn(&absent, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refusal.code, "provider-capacity");

    absent
        .captains
        .mark_agent_started("pendprv1", None)
        .unwrap();
    let refusal = admit_spawn(&absent, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refusal.code, "provider-capacity");

    let visible = test_ctx("pending-provider-visible")
        .with_governor(Arc::new(SpawnGovernor::new(16, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_pendprv2".into()]))
        .with_provider_capacity(|| Ok(2))
        .with_provider_live_sessions(|_| Ok(1));
    seed_starting_agent(&visible, "pendprv2");
    let live = live_session_evidence(&visible, &visible.captains.snapshot(), None).unwrap();
    let runtime =
        runtime_capacity_from_evidence(&visible, &visible.captains.snapshot(), &live, 16).unwrap();
    assert_eq!(runtime.provider_live_sessions, 1);

    let baseline = "1111111111111111111111111111111111111111";
    let resulting = "2222222222222222222222222222222222222222";
    let mut integrated = visible.captains.snapshot().agent_sessions[0].clone();
    integrated.work_stage = crate::agent_session::WorkStage::Complete;
    let mut delivery = completed_delivery(baseline, resulting);
    delivery
        .record_integration(crate::agent_session::IntegrationEvidence {
            source_commit: resulting.into(),
            canonical_baseline: "main".into(),
            canonical_commit: resulting.into(),
            reference: "integration://provider-capacity".into(),
            recorded_at: 3,
            manifest: Some(crate::agent_session::IntegrationManifest {
                integration_owner_identity: "integration-owner".into(),
                inputs: vec![crate::agent_session::IntegrationInput {
                    lane_id: "capacity-lane".into(),
                    agent_session_id: integrated.agent_session_id.clone(),
                    source_baseline: baseline.into(),
                    resulting_commit: resulting.into(),
                }],
            }),
        })
        .unwrap();
    integrated.delivery = Some(delivery);
    assert!(!agent_has_durable_provider_intent(&integrated));
}

#[test]
fn recovery_reservation_counts_only_nonterminal_recovery_agent_records() {
    let ctx = test_ctx("recovery-agent-record")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_live_sessions(|_| Ok(0));
    ctx.captains
        .begin_cortana_recovery("recovering-state")
        .unwrap();
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 16).unwrap();
    assert_eq!(runtime.live_recovery_sessions, 0);

    seed_starting_agent_with_purpose(
        &ctx,
        "recovery1",
        crate::governor::AdmissionPurpose::Recovery,
    );
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 16).unwrap();
    assert_eq!(runtime.live_recovery_sessions, 1);

    ctx.captains
        .update_agent_stage("recovery1", crate::agent_session::WorkStage::Stopped)
        .unwrap();
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 16).unwrap();
    assert_eq!(runtime.live_recovery_sessions, 0);
}

#[test]
fn reserved_purposes_fill_only_their_authorized_slot() {
    let ctx = test_ctx("reserved-purpose")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_existing".into()]));
    let ordinary = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(ordinary.code, "reserved-capacity");
    assert!(admit_spawn(&ctx, SpawnPurpose::FleetAdmin, 1, None).is_ok());
    assert!(admit_spawn(&ctx, SpawnPurpose::Recovery, 1, None).is_ok());
    assert!(admit_spawn(&ctx, SpawnPurpose::Cortana, 1, None).is_ok());
}

#[test]
fn privileged_admission_purposes_require_the_delegating_supervisor() {
    let crew = ResolvedIdentity {
        session_id: "crew-identity".into(),
        mint_role: crate::identity::Role::Crew,
        tile: Some("crew-tile".into()),
        ship_slug: Some("ship-one".into()),
        fleet_role: None,
        claude_uuid: None,
    };
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "captain-one", "admissionPurpose": "fleet-admin"}),
        Some(&crew),
        false,
    )
    .is_err());
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "captain-one", "admissionPurpose": "recovery"}),
        Some(&crew),
        false,
    )
    .is_err());

    let captain = ResolvedIdentity {
        session_id: "captain-identity".into(),
        mint_role: crate::identity::Role::Captain,
        tile: Some("captain-one".into()),
        ship_slug: Some("ship-one".into()),
        fleet_role: Some(FleetRole::Captain),
        claude_uuid: None,
    };
    assert_eq!(
        requested_spawn_purpose(
            "start_agent",
            &json!({"captainSessionId": "captain-one", "admissionPurpose": "ship-admin"}),
            Some(&captain),
            false,
        )
        .unwrap(),
        SpawnPurpose::ShipAdmin {
            ship_slug: "ship-one".into()
        }
    );
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "sibling-captain", "admissionPurpose": "ship-admin"}),
        Some(&captain),
        false,
    )
    .is_err());
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "captain-one", "admissionPurpose": "fleet-admin"}),
        Some(&captain),
        false,
    )
    .is_err());

    let cortana = ResolvedIdentity {
        session_id: "cortana-identity".into(),
        mint_role: crate::identity::Role::Cortana,
        tile: Some("cortana-one".into()),
        ship_slug: Some("fleet".into()),
        fleet_role: Some(FleetRole::Cortana),
        claude_uuid: None,
    };
    assert_eq!(
        requested_spawn_purpose(
            "start_agent",
            &json!({"captainSessionId": "captain-one", "admissionPurpose": "fleet-admin"}),
            Some(&cortana),
            false,
        )
        .unwrap(),
        SpawnPurpose::FleetAdmin
    );
}

#[test]
fn public_captain_spawn_assignment_is_refused_before_rate_or_process_side_effects() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .claim_test("captain-one", Some("ship-one"), vec![])
        .unwrap();
    let captain = mint_session(
        &store,
        crate::identity::Role::Captain,
        "ship-one",
        "captain-one",
    );
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("captain-spawn-contract")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_identity_store(store)
        .with_captains_registry(registry)
        .with_apply_sink(sink.clone());

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "captain-spawn-contract",
            &captain,
            "spawn_terminal",
            json!({
                "cwd": "/tmp",
                "spawnedBy": "captain-one",
                "startupCommand": "codex"
            }),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("must use start_agent"), "got: {error}");
    assert!(ctx.captains.snapshot().captains[0].crew.is_empty());
    assert!(sink.calls.lock().unwrap().is_empty());
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "a contract refusal must not consume the sole spawn-rate token"
    );
}

#[test]
fn start_agent_caller_cannot_set_its_own_capability() {
    let ctx = test_ctx("start-agent-capability-contract");
    let error = start_agent(
        &ctx,
        &json!({
            "requestId": "caller-capability",
            "captainSessionId": "captain-one",
            "assignment": "Attempt capability relabel",
            "directory": "/tmp",
            "sourceCommit": "1111111111111111111111111111111111111111",
            "visibleProductBug": false,
            "laneId": "caller-capability",
            "dependencies": [],
            "mutableFiles": [],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "capability": "control"
        }),
        None,
        true,
    )
    .unwrap_err();
    assert!(error.contains("unexpected argument"), "got: {error}");
    assert!(error.contains("capability"), "got: {error}");
    assert!(ctx.captains.snapshot().agent_sessions.is_empty());
}

#[test]
fn public_captain_worktree_assignment_is_refused_before_filesystem_or_rate_side_effects() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .claim_test("captain-one", Some("ship-one"), vec![])
        .unwrap();
    let captain = mint_session(
        &store,
        crate::identity::Role::Captain,
        "ship-one",
        "captain-one",
    );
    let root = std::env::temp_dir().join(format!(
        "t-hub-contract-no-worktree-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let worktree = root.join("worktree");
    let ctx = test_ctx("captain-worktree-contract")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_identity_store(store)
        .with_captains_registry(registry);

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "captain-worktree-contract",
            &captain,
            "create_worktree",
            json!({
                "repoRoot": root,
                "worktreePath": worktree,
                "spawnedBy": "captain-one",
                "startupCommand": "claude"
            }),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("must use start_agent"), "got: {error}");
    assert!(!worktree.exists());
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "a contract refusal must not consume the sole spawn-rate token"
    );
}

#[test]
fn plain_supervisor_shell_and_worktree_remain_generic_operations() {
    let captain = ResolvedIdentity {
        session_id: "captain-identity".into(),
        mint_role: crate::identity::Role::Captain,
        tile: Some("captain-one".into()),
        ship_slug: Some("ship-one".into()),
        fleet_role: Some(FleetRole::Captain),
        claude_uuid: None,
    };
    assert!(enforce_public_spawn_contract(
        "spawn_terminal",
        &json!({"cwd": "/tmp"}),
        Some(&captain),
        false,
    )
    .is_ok());
    assert!(enforce_public_spawn_contract(
        "create_worktree",
        &json!({"repoRoot": "/repo", "worktreePath": "/worktree"}),
        Some(&captain),
        false,
    )
    .is_ok());
    for command in ["spawn_terminal", "create_worktree"] {
        let error = enforce_public_spawn_contract(
            command,
            &json!({"capability": "control"}),
            Some(&captain),
            false,
        )
        .unwrap_err();
        assert!(error.contains("must use start_agent"), "got: {error}");
    }
}

#[test]
fn generic_spawn_admission_is_atomic_through_runtime_creation_window() {
    let live = Arc::new(StdMutex::new(Vec::<String>::new()));
    let evidence = live.clone();
    let ctx = test_ctx("atomic-generic")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(move || Ok(evidence.lock().unwrap().clone()));
    let (held_tx, held_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let first_ctx = ctx.clone();
    let first_live = live.clone();
    let first = std::thread::spawn(move || {
        let guard = admit_spawn(&first_ctx, SpawnPurpose::FleetAdmin, 1, None).unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        first_live.lock().unwrap().push("th_newadmin".into());
        drop(guard);
    });
    held_rx.recv().unwrap();
    let second_ctx = ctx.clone();
    let second = std::thread::spawn(move || {
        admit_spawn(&second_ctx, SpawnPurpose::Ordinary, 1, None)
            .expect_err("ordinary admission must be refused")
    });
    assert!(ctx.dispatch_admission.try_lock().is_err());
    release_tx.send(()).unwrap();
    first.join().unwrap();
    let refusal = second.join().unwrap();
    assert_eq!(refusal.code, "reserved-capacity");
}

#[test]
fn create_worktree_organization_command_cannot_bypass_spawn_capacity() {
    let ctx = test_ctx("worktree-cap")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        });
    let response = dispatch_authenticated(
        &ctx,
        req(
            "worktree-cap",
            "create_worktree",
            json!({"repoRoot": "/tmp/repo", "worktreePath": "/tmp/worktree"}),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("dispatch refused"), "got: {error}");
    assert!(
        !error.contains("repoRoot"),
        "handler ran before admission: {error}"
    );
}

#[test]
fn fresh_history_resume_acquires_capacity_and_cancels_its_new_reservation_on_refusal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let temp = tempfile::tempdir().unwrap();
    let codex_root = temp.path().join("codex/2026/07/20");
    let project_cwd = temp.path().join("project");
    std::fs::create_dir_all(&codex_root).unwrap();
    std::fs::create_dir_all(&project_cwd).unwrap();
    let conversation_id = "22222222-2222-4222-8222-222222222222";
    std::fs::write(
            codex_root.join(format!(
                "rollout-2026-07-20T10-00-00-{conversation_id}.jsonl"
            )),
            format!(
                "{}\n{}",
                json!({"type":"session_meta","payload":{"id":conversation_id,"cwd":project_cwd,"model_provider":"openai"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Resume me"}})
            ),
        )
        .unwrap();
    let history = Arc::new(crate::history::HistoryService::new(
        temp.path().join("claude"),
        temp.path().join("codex"),
        Duration::from_secs(60),
    ));
    let ctx = test_ctx("history-cap")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        })
        .with_history_service(history.clone());
    let listed = history_list(&ctx, &json!({"limit": 10}), None, true).unwrap();
    let history_id = listed["entries"][0]["historyId"]
        .as_str()
        .unwrap()
        .to_string();
    let response = dispatch_authenticated(
        &ctx,
        req(
            "history-cap",
            "history_resume",
            json!({"historyId": history_id, "requestId": "fresh-capacity"}),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("dispatch refused"), "got: {error}");
    assert!(
        history.pending_resume("fresh-capacity").unwrap().is_none(),
        "a pre-spawn capacity refusal must not strand a durable reservation"
    );
}

#[test]
fn completed_history_replay_precedes_full_capacity_and_preserves_one_rate_token() {
    let temp = tempfile::tempdir().unwrap();
    let history = history_service_at(temp.path());
    let (full_history_id, _) = seed_history_resume(&history, "completed-full", "cmpfull1", true);
    let full = test_ctx("history-completed-full")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        })
        .with_history_service(history.clone());
    let replay = dispatch_authenticated(
        &full,
        req(
            "history-completed-full",
            "history_resume",
            json!({"historyId": full_history_id, "requestId": "completed-full"}),
        ),
    );
    assert!(!replay.ok);
    let error = replay.error.unwrap();
    assert!(
        error.contains("history_previous_resume_closed"),
        "got: {error}"
    );
    assert!(!error.contains("spawn refused"), "got: {error}");

    let (token_history_id, _) = seed_history_resume(&history, "completed-token", "cmptoken", true);
    let one_token = test_ctx("history-completed-token")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_history_service(history);
    let replay = dispatch_authenticated(
        &one_token,
        req(
            "history-completed-token",
            "history_resume",
            json!({"historyId": token_history_id, "requestId": "completed-token"}),
        ),
    );
    assert!(!replay.ok);
    assert!(replay
        .error
        .unwrap_or_default()
        .contains("history_previous_resume_closed"));
    assert!(
        admit_spawn(&one_token, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "completed replay must not consume the sole spawn-rate token"
    );
}

#[test]
fn pending_history_replay_precedes_full_capacity_and_preserves_one_rate_token() {
    let temp = tempfile::tempdir().unwrap();
    let history = history_service_at(temp.path());
    let terminal_id = "pendrep1";
    let (history_id, _) = seed_history_resume(&history, "pending-replay", terminal_id, false);

    let full = test_ctx("history-pending-full")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        })
        .with_history_service(history.clone());
    let replay = dispatch_authenticated(
        &full,
        req(
            "history-pending-full",
            "history_resume",
            json!({"historyId": history_id, "requestId": "pending-replay"}),
        ),
    );
    assert!(!replay.ok);
    let error = replay.error.unwrap();
    assert!(error.contains("history_resume_in_flight"), "got: {error}");
    assert!(!error.contains("spawn refused"), "got: {error}");

    let one_token = test_ctx("history-pending-token")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_history_service(history);
    let replay = dispatch_authenticated(
        &one_token,
        req(
            "history-pending-token",
            "history_resume",
            json!({"historyId": "history:v1:pending-replay", "requestId": "pending-replay"}),
        ),
    );
    assert!(!replay.ok);
    assert!(replay
        .error
        .unwrap_or_default()
        .contains("history_resume_in_flight"));
    assert!(
        admit_spawn(&one_token, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "pending replay must not consume the sole spawn-rate token"
    );
}

#[test]
fn dispatch_preflight_admits_six_independent_lanes_with_available_capacity() {
    let ctx = test_ctx("dispatch-six").with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 8.0)));
    let (base, repo_root, _worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&repo_root);
    for index in 2..=5 {
        let branch = format!("lane-{index}");
        let path = base.join(format!("wt-{index}"));
        let output = std::process::Command::new("git")
            .current_dir(&repo_root)
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                path.to_str().unwrap(),
            ])
            .output()
            .expect("git worktree add spawns");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-six".into(),
            name: "Six Lane Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let requested_lanes = (1..=6)
        .map(|index| {
            json!({
                "laneId": format!("lane-{index}"),
                "ownerId": format!("owner-{index}"),
                "dependencies": [],
                "mutableFiles": [format!("scope-{index}")],
                "mutableSchemas": [],
                "mutableInterfaces": []
            })
        })
        .collect::<Vec<_>>();

    let response = dispatch(
        &ctx,
        "dispatch_preflight",
        &json!({
            "projectId": "project-six",
            "sourceCommit": source_commit,
            "requestedLanes": requested_lanes,
            "integrationContracts": []
        }),
    )
    .unwrap();

    assert_eq!(response["admitted"], true);
    assert_eq!(response["capacity"]["requestedLanes"], 6);
    assert!(response["capacity"]["effectiveLaneHeadroom"]
        .as_u64()
        .is_some_and(|headroom| headroom >= 6));
    std::fs::remove_dir_all(base).ok();
}
