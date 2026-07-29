use super::*;

#[test]
fn managed_cortana_with_lost_session_authority_is_replaced_after_restart_without_signal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let registry_path = captains_tmp("cortana-lost-session-authority");
    let identity_path = captains_tmp("cortana-lost-session-authority-identities");
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut ctx = test_ctx("cortana-lost-session-authority-control")
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_captains_registry(Arc::clone(&captains))
        .with_identity_store(Arc::clone(&identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4260".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-lost-session-authority-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let first = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-initial",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(first["healthy"], true);
    let incumbent_terminal = first["terminalId"].as_str().unwrap().to_string();
    let incumbent_identity = first["identityId"].as_str().unwrap().to_string();
    let incumbent_target = exact_cortana_tmux_target(&incumbent_terminal).unwrap();
    let incumbent_effect = tmux::observe_session_effect_identity(&incumbent_target).unwrap();
    let incumbent_bearer =
        tmux::session_environment(&incumbent_target, crate::identity::SESSION_TOKEN_ENV)
            .unwrap()
            .unwrap();
    assert_eq!(
        tmux::session_environment(&incumbent_target, "T_HUB_CONTROL_ADDR").unwrap(),
        Some(String::new())
    );
    assert_eq!(
        tmux::session_environment(&incumbent_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );
    let healthy_before_negative = captains.snapshot();
    let healthy_candidates = discover_cortana_runtimes(
        &ctx,
        &files::posix_form(&home.to_string_lossy()),
        &healthy_before_negative.cortana,
    )
    .unwrap();
    assert!(retirable_unattested_managed_cortana_incumbent(
        &ctx,
        &healthy_before_negative.cortana,
        &healthy_candidates,
    )
    .is_none());
    assert_eq!(captains.snapshot().seq, healthy_before_negative.seq);
    assert!(!identities.is_revoked(&incumbent_identity));
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );

    identities.revoke(&incumbent_identity).unwrap();
    assert!(identities.resolve(&incumbent_bearer).is_none());
    drop(ctx);
    drop(captains);
    drop(identities);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut restarted = test_ctx("cortana-lost-session-authority-restart")
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)))
        .with_live_sessions({
            let incumbent_target = incumbent_target.clone();
            move || Ok(vec![incumbent_target.clone()])
        })
        .with_captains_registry(Arc::clone(&restarted_captains))
        .with_identity_store(Arc::clone(&restarted_identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    restarted.addr = "127.0.0.1:4261".into();
    restarted.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![incumbent_terminal.clone()],
    }]);
    let durable_before_recovery = restarted_captains.cortana_identity();
    let candidates_before_recovery = discover_cortana_runtimes(
        &restarted,
        &files::posix_form(&home.to_string_lossy()),
        &durable_before_recovery,
    )
    .unwrap();
    let prepared_incumbent = retirable_unattested_managed_cortana_incumbent(
                &restarted,
                &durable_before_recovery,
                &candidates_before_recovery,
            )
            .unwrap_or_else(|| panic!(
                "durable={durable_before_recovery:#?} candidates={candidates_before_recovery:#?} claims={:#?}",
                restarted_captains.snapshot().captains,
            ));
    let seq_before_mismatch = restarted_captains.snapshot().seq;
    let mut mismatched_candidates = candidates_before_recovery.clone();
    mismatched_candidates[0]
        .effect_identity
        .as_mut()
        .unwrap()
        .pane_start_ticks = mismatched_candidates[0]
        .effect_identity
        .as_ref()
        .unwrap()
        .pane_start_ticks
        .saturating_add(1);
    assert!(retirable_unattested_managed_cortana_incumbent(
        &restarted,
        &durable_before_recovery,
        &mismatched_candidates,
    )
    .is_none());
    let mut mismatched_attestation = durable_before_recovery.clone();
    mismatched_attestation
        .active_harness_attestation
        .as_mut()
        .unwrap()
        .process
        .start_ticks = mismatched_attestation
        .active_harness_attestation
        .as_ref()
        .unwrap()
        .process
        .start_ticks
        .saturating_add(1);
    assert!(revalidate_unresolved_cortana_attestation(&mismatched_attestation).is_err());
    assert_eq!(restarted_captains.snapshot().seq, seq_before_mismatch);
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    revalidate_unresolved_cortana_attestation(&durable_before_recovery).unwrap();
    restarted_captains
        .begin_cortana_recovery("lost-session-authority-replacement")
        .unwrap();
    restarted_captains
        .prepare_cortana_orphan_replacement(
            "lost-session-authority-replacement",
            &prepared_incumbent.terminal_id,
            durable_before_recovery.identity_id.as_deref().unwrap(),
            durable_before_recovery.generation,
            durable_before_recovery.harness.as_deref().unwrap(),
            prepared_incumbent.effect_identity.unwrap(),
        )
        .unwrap();
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            managed_basis: Some(_),
            ..
        }
    ));
    drop(restarted);
    drop(restarted_captains);
    drop(restarted_identities);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut restarted = test_ctx("cortana-lost-session-authority-wal-restart")
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)))
        .with_live_sessions({
            let incumbent_target = incumbent_target.clone();
            move || Ok(vec![incumbent_target.clone()])
        })
        .with_captains_registry(Arc::clone(&restarted_captains))
        .with_identity_store(Arc::clone(&restarted_identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    restarted.addr = "127.0.0.1:4261".into();
    restarted.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![incumbent_terminal.clone()],
    }]);
    let gained_identity = restarted_identities
        .mint(crate::identity::Role::Cortana)
        .unwrap();
    restarted_identities
        .bind_tile(&gained_identity.id, &incumbent_terminal)
        .unwrap();
    tmux::set_session_environment(
        &incumbent_target,
        crate::identity::SESSION_TOKEN_ENV,
        &gained_identity.secret,
    )
    .unwrap();
    let seq_before_capability_gain = restarted_captains.snapshot().seq;
    let capability_gain_error = dispatch(
        &restarted,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(
        capability_gain_error.contains("runtime changed after WAL")
            || capability_gain_error.contains("attestation failed"),
        "{capability_gain_error}"
    );
    assert!(!restarted_identities.is_revoked(&gained_identity.id));
    assert_eq!(
        restarted_captains.snapshot().seq,
        seq_before_capability_gain
    );
    assert!(restarted_captains
        .cortana_identity()
        .quarantine_ledger
        .is_empty());
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    tmux::set_session_environment(
        &incumbent_target,
        crate::identity::SESSION_TOKEN_ENV,
        &incumbent_bearer,
    )
    .unwrap();
    restarted_identities.retire(&gained_identity.id).unwrap();
    let wal_durable = restarted_captains.cortana_identity();
    let (
        wal_effect,
        wal_basis,
        wal_identity,
        wal_generation,
        wal_harness,
        original_assignment,
        original_attestation,
    ) = match &wal_durable.recovery {
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            orphan_identity_id,
            orphan_generation,
            harness,
            effect_identity,
            managed_basis: Some(basis),
            ..
        } => (
            *effect_identity,
            basis.clone(),
            orphan_identity_id.clone(),
            *orphan_generation,
            harness.clone(),
            basis.claim_assignment_id.clone(),
            basis.active_harness_attestation.clone(),
        ),
        other => panic!("expected managed quarantine WAL, got {other:#?}"),
    };
    revalidate_unresolved_cortana_attestation(&wal_durable).unwrap();
    let post_wal_candidates = discover_cortana_runtimes(
        &restarted,
        &files::posix_form(&home.to_string_lossy()),
        &wal_durable,
    )
    .unwrap();
    assert_eq!(post_wal_candidates.len(), 1);
    assert!(exact_unresolved_managed_cortana_candidate(
        &post_wal_candidates[0],
        &incumbent_terminal,
        wal_generation,
        &wal_harness,
        &wal_effect,
    ));

    restarted_captains
        .set_cortana_quarantine_claim_assignment_for_test("changed-after-revalidation")
        .unwrap();
    assert!(restarted_captains
        .validate_cortana_managed_quarantine_basis(
            "lost-session-authority-replacement",
            &incumbent_terminal,
            &wal_identity,
            wal_generation,
            &wal_harness,
            &wal_effect,
            &wal_basis,
        )
        .is_err());
    assert!(restarted_captains
        .cortana_identity()
        .quarantine_ledger
        .is_empty());
    restarted_captains
        .set_cortana_quarantine_claim_assignment_for_test(&original_assignment)
        .unwrap();

    restarted_captains
        .set_cortana_quarantine_attestation_for_test(None)
        .unwrap();
    assert!(restarted_captains
        .validate_cortana_managed_quarantine_basis(
            "lost-session-authority-replacement",
            &incumbent_terminal,
            &wal_identity,
            wal_generation,
            &wal_harness,
            &wal_effect,
            &wal_basis,
        )
        .is_err());
    restarted_captains
        .set_cortana_quarantine_attestation_for_test(original_attestation)
        .unwrap();
    assert!(restarted_captains
        .validate_cortana_managed_quarantine_basis(
            "lost-session-authority-replacement",
            &incumbent_terminal,
            &wal_identity,
            wal_generation,
            &wal_harness,
            &wal_effect,
            &wal_basis,
        )
        .is_ok());
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    let recovered = dispatch(
        &restarted,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["action"], "recover", "{recovered:#}");
    assert_eq!(recovered["healthy"], true);
    assert_eq!(recovered["generation"], 2);
    assert_ne!(recovered["terminalId"], incumbent_terminal);
    assert_eq!(
        tmux::session_liveness(&incumbent_target),
        tmux::SessionLiveness::Alive,
        "the exact invalid incumbent must be quarantined without a signal"
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect,
        "the quarantined process generation must remain unchanged"
    );
    let denied = dispatch_authenticated(
        &restarted,
        req_session(
            &restarted.token,
            &incumbent_bearer,
            "register_project",
            json!({"rootPath": "/tmp/lost-session-authority-must-not-register"}),
        ),
    );
    assert!(!denied.ok);
    let replacement_terminal = recovered["terminalId"].as_str().unwrap().to_string();
    let replacement_identity = recovered["identityId"].as_str().unwrap().to_string();
    let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
    let replacement_effect = tmux::observe_session_effect_identity(&replacement_target).unwrap();
    let replacement_bearer =
        tmux::session_environment(&replacement_target, crate::identity::SESSION_TOKEN_ENV)
            .unwrap()
            .unwrap();
    assert_eq!(
        restarted_captains
            .snapshot()
            .captains
            .iter()
            .filter(
                |captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            )
            .count(),
        1
    );
    assert_eq!(
        restarted_captains
            .cortana_identity()
            .quarantine_ledger
            .len(),
        1
    );

    restarted_identities.revoke(&replacement_identity).unwrap();
    assert!(restarted_identities.resolve(&replacement_bearer).is_none());
    drop(restarted);
    drop(restarted_captains);
    drop(restarted_identities);

    let mut native_document: Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    let native_cortana = native_document
        .get_mut("cortana")
        .and_then(Value::as_object_mut)
        .unwrap();
    native_cortana.remove("activeHarnessAttestation");
    native_cortana.insert("providerSessionId".into(), Value::Null);
    native_cortana.insert("conversationId".into(), Value::Null);
    native_cortana.insert(
            "recovery".into(),
            json!({
                "kind": "degraded",
                "operation_id": "native-lost-session-authority",
                "reason": "live managed runtime lost authoritative session identity and control evidence",
                "detected_at": now_ms().max(1),
            }),
        );
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&native_document).unwrap(),
    )
    .unwrap();

    let native_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let native_identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut native = test_ctx("cortana-native-lost-session-authority")
        .with_governor(Arc::new(SpawnGovernor::new(2, 600.0, 8.0)))
        .with_live_sessions({
            let incumbent_target = incumbent_target.clone();
            let replacement_target = replacement_target.clone();
            move || Ok(vec![incumbent_target.clone(), replacement_target.clone()])
        })
        .with_metrics(Arc::new(|| {
            Ok(t_hub_protocol::HostMetrics {
                mem_total_kib: 16_000_000,
                mem_available_kib: 8_000_000,
                swap_total_kib: 2_000_000,
                swap_free_kib: 1_500_000,
                cpu_count: 12,
                load_avg: [1.0, 0.5, 0.25],
                process_count: 432,
                distro: Some("test".into()),
                captured_at_ms: now_ms(),
            })
        }))
        .with_captains_registry(Arc::clone(&native_captains))
        .with_identity_store(Arc::clone(&native_identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    native.addr = "127.0.0.1:4262".into();
    native.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![replacement_terminal.clone()],
    }]);
    let native_durable_before_wal = native_captains.cortana_identity();
    let native_candidates_before_wal = discover_cortana_runtimes(
        &native,
        &files::posix_form(&home.to_string_lossy()),
        &native_durable_before_wal,
    )
    .unwrap();
    let native_incumbent = retirable_unattested_managed_cortana_incumbent(
        &native,
        &native_durable_before_wal,
        &native_candidates_before_wal,
    )
    .expect("native invalid incumbent must have exact managed evidence");
    native_captains
        .begin_cortana_recovery("native-lost-session-authority-replacement")
        .unwrap();
    native_captains
        .prepare_cortana_orphan_replacement(
            "native-lost-session-authority-replacement",
            &native_incumbent.terminal_id,
            native_durable_before_wal.identity_id.as_deref().unwrap(),
            native_durable_before_wal.generation,
            native_durable_before_wal.harness.as_deref().unwrap(),
            native_incumbent.effect_identity.unwrap(),
        )
        .unwrap();
    let gained_native_identity = native_identities
        .mint(crate::identity::Role::Cortana)
        .unwrap();
    native_identities
        .bind_tile(&gained_native_identity.id, &replacement_terminal)
        .unwrap();
    tmux::set_session_environment(
        &replacement_target,
        crate::identity::SESSION_TOKEN_ENV,
        &gained_native_identity.secret,
    )
    .unwrap();
    let native_seq_before_gain = native_captains.snapshot().seq;
    let native_gain_error = dispatch(
        &native,
        "reconcile_cortana",
        &json!({
            "operationId": "native-lost-session-authority-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(
        native_gain_error.contains("runtime changed after WAL"),
        "{native_gain_error}"
    );
    assert_eq!(native_captains.snapshot().seq, native_seq_before_gain);
    assert!(!native_identities.is_revoked(&gained_native_identity.id));
    assert_eq!(
        native_captains.cortana_identity().quarantine_ledger.len(),
        1
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&replacement_target).unwrap(),
        replacement_effect
    );
    tmux::set_session_environment(
        &replacement_target,
        crate::identity::SESSION_TOKEN_ENV,
        &replacement_bearer,
    )
    .unwrap();
    native_identities
        .retire(&gained_native_identity.id)
        .unwrap();
    let native = Arc::new(native);
    let concurrent_start = Arc::new(std::sync::Barrier::new(5));
    let mut concurrent_workers = Vec::new();
    for _ in 0..4 {
        let native = Arc::clone(&native);
        let concurrent_start = Arc::clone(&concurrent_start);
        let home = home.clone();
        let harness_command = harness_command.clone();
        concurrent_workers.push(std::thread::spawn(move || {
            concurrent_start.wait();
            dispatch(
                &native,
                "reconcile_cortana",
                &json!({
                    "operationId": "native-lost-session-authority-replacement",
                    "testOrchestratorHome": home,
                    "testStartupCommand": harness_command,
                }),
            )
            .unwrap()
        }));
    }
    concurrent_start.wait();
    let concurrent_results = concurrent_workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let native_recovered = concurrent_results
        .iter()
        .find(|result| result["action"] == "recover")
        .cloned()
        .expect("one concurrent caller must perform the recovery");
    assert_eq!(
        concurrent_results
            .iter()
            .filter(|result| result["action"] == "recover")
            .count(),
        1
    );
    assert!(concurrent_results.iter().all(|result| {
        result["generation"] == 3
            && result["terminalId"] == native_recovered["terminalId"]
            && matches!(result["action"].as_str(), Some("recover" | "keep"))
    }));
    assert_eq!(
        native_recovered["action"], "recover",
        "{native_recovered:#}"
    );
    assert_eq!(native_recovered["healthy"], true);
    assert_eq!(native_recovered["generation"], 3);
    let generation_three_terminal = native_recovered["terminalId"].as_str().unwrap().to_string();
    assert_ne!(generation_three_terminal, replacement_terminal);
    assert_eq!(
        tmux::session_liveness(&incumbent_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    assert_eq!(
        tmux::session_liveness(&replacement_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&replacement_target).unwrap(),
        replacement_effect
    );
    let native_durable = native_captains.cortana_identity();
    assert_eq!(native_durable.quarantine_ledger.len(), 2);
    assert_eq!(
        native_durable.quarantine_ledger[0].terminal_id,
        incumbent_terminal
    );
    assert_eq!(
        native_durable.quarantine_ledger[1].terminal_id,
        replacement_terminal
    );
    assert!(native_identities.is_revoked(&incumbent_identity));
    assert!(native_identities.is_revoked(&replacement_identity));
    for bearer in [&incumbent_bearer, &replacement_bearer] {
        let denied = dispatch_authenticated(
            &native,
            req_session(
                &native.token,
                bearer,
                "register_project",
                json!({"rootPath": "/tmp/quarantined-cortana-must-not-register"}),
            ),
        );
        assert!(!denied.ok);
    }
    let stale_workspace_report = dispatch(
        &native,
        "report_workspace_tabs",
        &json!({
            "tabs": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "tileIds": [replacement_terminal.clone()],
            }]
        }),
    );
    assert!(stale_workspace_report.is_err());
    assert_eq!(
        native_captains.cortana_identity().terminal_id.as_deref(),
        Some(generation_three_terminal.as_str())
    );
    assert_eq!(
        native_captains
            .snapshot()
            .captains
            .iter()
            .filter(
                |captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            )
            .filter_map(|captain| captain.terminal_id.as_deref())
            .collect::<Vec<_>>(),
        vec![generation_three_terminal.as_str()]
    );

    let after_restart = dispatch(
        &native,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-after-restart",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(after_restart["action"], "keep");
    assert_eq!(after_restart["terminalId"], generation_three_terminal);
    assert_eq!(after_restart["generation"], 3);

    dispatch(
        &native,
        "close_terminal",
        &json!({ "sessionId": generation_three_terminal }),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&incumbent_target);
    reap_test_tmux_session_and_assert_absent(&replacement_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn installed_stale_legacy_cortana_is_quarantined_without_signal_and_replaced() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "installed_stale_legacy_cortana_is_exactly_replaced_from_v22_provenance: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-schema18-orphan");
    let migration_backup = registry_path.parent().unwrap().join(format!(
        "{}.migration-v20.1.bak",
        registry_path.file_name().unwrap().to_string_lossy()
    ));
    let identity_path = captains_tmp("cortana-schema18-orphan-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let orphan_identity = "missing-schema18-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 6,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": orphan_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "providerSessionId": null,
                "conversationId": null,
                "checkpoint": null,
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "installed-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::rename(&registry_path, &migration_backup).unwrap();
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 21,
            "seq": 1531,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": []
            }],
            "cortana": {
                "identityId": orphan_identity,
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "providerSessionId": null,
                "conversationId": null,
                "checkpoint": null,
                "recovery": {
                    "kind": "degraded",
                    "operation_id": "installed-degraded",
                    "reason": "legacy runtime lost its identity",
                    "detected_at": 2
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    assert!(captains.cortana_identity().terminal_id.is_none());
    assert_eq!(
        captains
            .cortana_identity()
            .legacy_orphan_provenance
            .as_ref()
            .map(|provenance| provenance.terminal_id.as_str()),
        Some(orphan_terminal.as_str())
    );
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("schema18-orphan-control")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_captains_registry(captains.clone())
        .with_identity_store(identities.clone())
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4250".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![orphan_terminal.clone()],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-schema18-orphan-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let orphan_target = exact_cortana_tmux_target(&orphan_terminal).unwrap();
    create_test_tmux_session_with_env(
        &orphan_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "untrusted-orphan-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let persist_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_calls = Arc::clone(&persist_calls);
    captains.set_persist_hook(Box::new(move || {
        hook_calls.fetch_add(1, Ordering::SeqCst);
    }));
    let ctx = Arc::new(ctx);
    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "schema18-orphan-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert!(
        persist_calls.load(Ordering::SeqCst) >= 3,
        "begin, quarantine, and managed owner publication must be durable"
    );

    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["healthy"], true);
    assert_eq!(recovered["generation"], 2);
    assert_ne!(recovered["identityId"], orphan_identity);
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive,
        "legacy quarantine must not signal or close the pre-owner runtime"
    );
    let replacement_terminal = recovered["terminalId"].as_str().unwrap().to_string();
    let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
    assert_eq!(
        tmux::session_environment(&replacement_target, CORTANA_GENERATION_ENV).unwrap(),
        Some("2".into())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_FILE").unwrap(),
        Some(discovery_file_for_spawn())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_ADDR").unwrap(),
        Some(String::new())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );
    let durable = captains.snapshot();
    assert_eq!(durable.schema_version, CAPTAINS_SCHEMA_VERSION);
    assert!(matches!(
        durable.cortana.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert_eq!(
        durable
            .cortana
            .quarantine_ledger
            .last()
            .map(|quarantine| quarantine.terminal_id.as_str()),
        Some(orphan_terminal.as_str())
    );
    assert_eq!(
        durable
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );
    let replacement_identity = durable.cortana.identity_id.unwrap();
    assert_eq!(
        identities.get(&replacement_identity).unwrap().role,
        crate::identity::Role::Cortana
    );

    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": replacement_terminal }),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&orphan_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(migration_backup).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn captured_packaged_schema25_orphan_rotates_then_quarantines_without_signal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "captured_packaged_schema25_orphan_rotates_then_quarantines_without_signal: tmux or node not on PATH - skipping"
            );
        return;
    }
    let fixture: Value = serde_json::from_str(PACKAGED_SCHEMA_25_LEGACY_ORPHAN_FIXTURE).unwrap();
    let registry_path = captains_tmp("captured-packaged-schema25-orphan");
    let identity_path = captains_tmp("captured-packaged-schema25-identities");
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&fixture["captainsSnapshot"]).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&fixture["identitiesSnapshot"]).unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let terminal_id = fixture["capture"]["runtime"]["terminalId"]
        .as_str()
        .unwrap();
    let legacy_addr = fixture["capture"]["control"]["legacyAddress"]
        .as_str()
        .unwrap();
    let current_addr = fixture["capture"]["control"]["currentAddress"]
        .as_str()
        .unwrap();
    let shared_token = fixture["capture"]["control"]["sharedPersistentToken"]
        .as_str()
        .unwrap();
    let session_token = fixture["capture"]["runtime"]["sessionToken"]
        .as_str()
        .unwrap();
    let legacy_identity = captains.cortana_identity().identity_id.clone().unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-captured-packaged-orphan-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let target = exact_cortana_tmux_target(terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                session_token.into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), legacy_addr.into()),
            ("T_HUB_CONTROL_TOKEN".into(), shared_token.into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(terminal_id, "codex").unwrap();

    let build_ctx = |token: &str| {
        let mut ctx = test_ctx(token)
            .with_live_sessions(|| Ok(Vec::new()))
            .with_captains_registry(captains.clone())
            .with_identity_store(identities.clone())
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            }));
        ctx.addr = current_addr.into();
        ctx.tab_registry().replace(vec![TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![terminal_id.into()],
        }]);
        ctx
    };
    let same_bearer = build_ctx(shared_token);
    let reproduced = dispatch(
        &same_bearer,
        "reconcile_cortana",
        &json!({
            "operationId": "captured-packaged-before-rotation",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(reproduced["action"], "degraded");
    assert_eq!(
            reproduced["degradedReason"],
            format!(
                "live runtime '{terminal_id}' in Cortana's reserved scope lacks authoritative identity, generation, or control evidence"
            )
        );
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(!identities.is_revoked(&legacy_identity));

    let key_dir = std::env::temp_dir().join(format!(
        "t-hub-captured-packaged-key-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&key_dir).unwrap();
    let key_path = key_dir.join("server-key");
    write_key_file(&key_path, shared_token);
    let rotated_token = persistent_key_for_start_with(&key_path, false, 3600, true).unwrap();
    assert_ne!(rotated_token, shared_token);

    let restarted = build_ctx(&rotated_token);
    assert_eq!(resolve_capability(&restarted, shared_token), None);
    assert_eq!(
        resolve_capability(&restarted, &rotated_token),
        Some(Capability::Full)
    );
    let denied = dispatch_authenticated(
        &restarted,
        ControlRequest {
            token: shared_token.into(),
            command: "close_terminal".into(),
            args: json!({ "sessionId": terminal_id }),
            session: session_token.into(),
            host: String::new(),
            v: Some(PROTOCOL_VERSION),
        },
    );
    assert!(!denied.ok);
    assert_eq!(
        denied.error.as_deref(),
        Some("unauthorized: bad control token")
    );
    let recovered = dispatch(
        &restarted,
        "reconcile_cortana",
        &json!({
            "operationId": "captured-packaged-after-rotation",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["healthy"], true);
    assert_eq!(recovered["generation"], 2);
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(identities.is_revoked(&legacy_identity));
    assert_eq!(
        captains
            .cortana_identity()
            .quarantine_ledger
            .last()
            .map(|quarantine| quarantine.terminal_id.as_str()),
        Some(terminal_id)
    );

    let replacement = recovered["terminalId"].as_str().unwrap().to_string();
    let replacement_target = exact_cortana_tmux_target(&replacement).unwrap();
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_FILE").unwrap(),
        Some(discovery_file_for_spawn())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );
    let replacement_session_token =
        tmux::session_environment(&replacement_target, crate::identity::SESSION_TOKEN_ENV)
            .unwrap()
            .expect("replacement has a per-session bearer");
    let replacement_identity = identities
        .resolve(&replacement_session_token)
        .expect("replacement bearer resolves after control-key rotation");
    assert_eq!(replacement_identity.role, crate::identity::Role::Cortana);
    assert_eq!(
        replacement_identity.session_tile.as_deref(),
        Some(replacement.as_str())
    );
    assert_eq!(
        captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| {
                captain.role == FleetRole::Cortana
                    && captain.state == ClaimState::Active
                    && captain.terminal_id.as_deref() == Some(replacement.as_str())
            })
            .count(),
        1
    );
    dispatch(
        &restarted,
        "close_terminal",
        &json!({ "sessionId": replacement }),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&target);
    std::fs::remove_dir_all(key_dir).ok();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

fn legacy_orphan_durable(
    identity_id: &str,
    terminal_id: &str,
) -> crate::cortana_reconcile::CortanaDurableIdentity {
    crate::cortana_reconcile::CortanaDurableIdentity {
        identity_id: Some(identity_id.into()),
        generation: 1,
        terminal_id: None,
        harness: Some("codex".into()),
        legacy_orphan_provenance: Some(crate::cortana_reconcile::CortanaLegacyOrphanProvenance {
            version: crate::cortana_reconcile::LEGACY_ORPHAN_PROVENANCE_VERSION,
            source_schema_version: 18,
            identity_id: identity_id.into(),
            terminal_id: terminal_id.into(),
            generation: 1,
            harness: "codex".into(),
            healthy_operation_id: "legacy-healthy".into(),
        }),
        recovery: crate::cortana_reconcile::CortanaRecoveryState::Degraded {
            operation_id: "legacy-degraded".into(),
            reason: "identity disappeared".into(),
            detected_at: 1,
        },
        ..Default::default()
    }
}

fn stale_legacy_orphan_candidate(
    terminal_id: &str,
) -> crate::cortana_reconcile::CortanaRuntimeCandidate {
    crate::cortana_reconcile::CortanaRuntimeCandidate {
        terminal_id: terminal_id.into(),
        identity_id: None,
        generation: 1,
        harness: "codex".into(),
        provider_session_id: None,
        terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
        harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
        identity_bound_to_terminal: false,
        canonical_control_file: false,
        rotating_control_env_scrubbed: false,
        stale_legacy_control_env: true,
        unresolved_session_bearer: true,
        effect_identity: Some(test_cortana_effect_identity(100)),
        current_control_capability: false,
        trusted_cortana_identity: false,
    }
}

fn test_cortana_effect_identity(
    seed: u32,
) -> crate::cortana_reconcile::CortanaOrphanEffectIdentity {
    crate::cortana_reconcile::CortanaOrphanEffectIdentity {
        tmux_session_id: u64::from(seed),
        tmux_session_created: u64::from(seed) + 1,
        tmux_window_id: u64::from(seed) + 2,
        tmux_pane_id: u64::from(seed) + 3,
        pane_pid: seed + 4,
        pane_start_ticks: u64::from(seed) + 5,
        pane_process_group_id: seed + 4,
        pane_process_session_id: seed + 4,
        foreground_pid: seed + 6,
        foreground_start_ticks: u64::from(seed) + 7,
        foreground_process_group_id: seed + 6,
        foreground_process_session_id: seed + 4,
    }
}

#[test]
fn schema30_singular_cortana_quarantine_migrates_to_canonical_ledger() {
    let path = captains_tmp("schema30-singular-cortana-quarantine");
    let effect = test_cortana_effect_identity(31);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 30,
            "seq": 9,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [],
            }],
            "cortana": {
                "identityId": "schema30-burned-identity",
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "legacyQuarantine": {
                    "terminalId": "deadbeef",
                    "identityId": "schema30-burned-identity",
                    "generation": 1,
                    "harness": "codex",
                    "tmux": effect,
                    "authorityRevoked": true,
                    "quarantinedAt": 8,
                },
                "recovery": {
                    "kind": "legacyUnownedQuarantined",
                    "operation_id": "schema30-quarantine",
                    "quarantined_at": 8,
                    "legacy_terminal_id": "deadbeef",
                    "legacy_generation": 1,
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let registry = CaptainsRegistry::load(path.clone());
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.cortana.quarantine_ledger.len(), 1);
    assert_eq!(
        snapshot.cortana.quarantine_ledger[0].terminal_id,
        "deadbeef"
    );
    let canonical = serde_json::to_value(snapshot).unwrap();
    assert_eq!(canonical["schemaVersion"], CAPTAINS_SCHEMA_VERSION);
    assert!(canonical.pointer("/cortana/quarantineLedger").is_some());
    assert!(canonical.pointer("/cortana/legacyQuarantine").is_none());
    let conflict_path = captains_tmp("schema31-conflicting-cortana-quarantine");
    let mut conflicting = canonical;
    let duplicate = conflicting["cortana"]["quarantineLedger"][0].clone();
    conflicting["cortana"]["quarantineLedger"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    std::fs::write(
        &conflict_path,
        serde_json::to_vec_pretty(&conflicting).unwrap(),
    )
    .unwrap();
    assert!(CaptainsRegistry::read_snapshot(&conflict_path).is_err());
    std::fs::remove_file(path).ok();
    std::fs::remove_file(conflict_path).ok();
}

#[test]
fn managed_quarantine_generation_allows_only_foreground_transition() {
    let owner = test_cortana_effect_identity(41);
    let mut harness = owner;
    harness.foreground_pid = harness.foreground_pid.saturating_add(100);
    harness.foreground_start_ticks = harness.foreground_start_ticks.saturating_add(100);
    harness.foreground_process_group_id = harness.foreground_pid;
    assert!(same_cortana_tmux_generation(&owner, &harness));
    let mutations: [fn(&mut crate::cortana_reconcile::CortanaOrphanEffectIdentity); 6] = [
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_session_id = value.tmux_session_id.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_session_created = value.tmux_session_created.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_window_id = value.tmux_window_id.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_pane_id = value.tmux_pane_id.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.pane_pid = value.pane_pid.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.pane_start_ticks = value.pane_start_ticks.saturating_add(1)
        },
    ];
    for mutate in mutations {
        let mut changed = harness;
        mutate(&mut changed);
        assert!(!same_cortana_tmux_generation(&owner, &changed));
    }
}

#[test]
fn legacy_orphan_retirement_requires_exact_provenance_and_untrusted_stale_runtime() {
    let terminal_id = "a1b2c3d4";
    let missing_identity = "missing-legacy-cortana";
    let ctx = test_ctx("legacy-retirement-current-token");
    let durable = legacy_orphan_durable(missing_identity, terminal_id);
    let candidate = stale_legacy_orphan_candidate(terminal_id);
    assert!(
        retirable_legacy_cortana_orphan(&ctx, &durable, std::slice::from_ref(&candidate)).is_some()
    );

    let mut no_provenance = durable.clone();
    no_provenance.legacy_orphan_provenance = None;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &no_provenance,
        std::slice::from_ref(&candidate)
    )
    .is_none());

    let mut mismatched_terminal = durable.clone();
    mismatched_terminal
        .legacy_orphan_provenance
        .as_mut()
        .unwrap()
        .terminal_id = "other001".into();
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &mismatched_terminal,
        std::slice::from_ref(&candidate)
    )
    .is_none());

    let mut current_endpoint = candidate.clone();
    current_endpoint.stale_legacy_control_env = false;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        std::slice::from_ref(&current_endpoint)
    )
    .is_none());

    let mut copied_bearer = candidate.clone();
    copied_bearer.identity_id = Some("copied-known-identity".into());
    assert!(
        retirable_legacy_cortana_orphan(&ctx, &durable, std::slice::from_ref(&copied_bearer))
            .is_none()
    );

    let mut unknown_liveness = candidate.clone();
    unknown_liveness.terminal = crate::cortana_reconcile::RuntimeEvidence::Unknown;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        std::slice::from_ref(&unknown_liveness)
    )
    .is_none());

    let mut missing_effect_identity = candidate.clone();
    missing_effect_identity.effect_identity = None;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        std::slice::from_ref(&missing_effect_identity)
    )
    .is_none());

    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        &[candidate.clone(), stale_legacy_orphan_candidate("e5f6g7h8")]
    )
    .is_none());

    let existing = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let existing_durable = legacy_orphan_durable(&existing.id, terminal_id);
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &existing_durable,
        std::slice::from_ref(&candidate)
    )
    .is_none());

    let claimed = ctx
        .captains
        .claim_test("active-cortana", Some("legacy-active-claim"), vec![])
        .unwrap();
    {
        let mut inner = ctx.captains.lock();
        let record = inner
            .captains
            .iter_mut()
            .find(|record| record.ship_slug == claimed.record.ship_slug)
            .unwrap();
        record.role = FleetRole::Cortana;
        record.state = ClaimState::Active;
    }
    assert!(
        retirable_legacy_cortana_orphan(&ctx, &durable, std::slice::from_ref(&candidate)).is_none()
    );
}

#[test]
fn stale_legacy_control_detection_rejects_current_endpoint_or_token() {
    let current_addr = "127.0.0.1:63930";
    let current_token = "current-control-token";
    assert!(stale_legacy_cortana_control_env(
        None,
        Some("127.0.0.1:51330"),
        Some("stale-control-token"),
        current_addr,
        current_token,
    ));
    for (control_file, address, token) in [
        (
            Some("/home/user/.t-hub-dev/control.json"),
            Some("127.0.0.1:51330"),
            Some("stale-control-token"),
        ),
        (None, Some(current_addr), Some("stale-control-token")),
        (None, Some("127.0.0.1:51330"), Some(current_token)),
        (None, None, Some("stale-control-token")),
        (None, Some("127.0.0.1:51330"), None),
    ] {
        assert!(!stale_legacy_cortana_control_env(
            control_file,
            address,
            token,
            current_addr,
            current_token,
        ));
    }
}

#[test]
fn stale_legacy_runtime_without_exact_provenance_stays_alive_and_degraded() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "stale_legacy_runtime_without_exact_provenance_stays_alive_and_degraded: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-stale-no-provenance");
    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 21,
            "seq": 20,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": []
            }],
            "cortana": {
                "identityId": "missing-no-provenance-identity",
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "recovery": {
                    "kind": "degraded",
                    "operation_id": "no-provenance-original",
                    "reason": "identity disappeared",
                    "detected_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    assert!(captains
        .cortana_identity()
        .legacy_orphan_provenance
        .is_none());
    let mut ctx = test_ctx("no-provenance-current-token")
        .with_captains_registry(captains.clone())
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:63930".into();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-no-provenance-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let target = exact_cortana_tmux_target(&terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "unresolved-no-provenance-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let result = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "no-provenance-reconcile",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(result["action"], "degraded");
    assert_eq!(result["healthy"], false);
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(matches!(
        captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Degraded { .. }
    ));

    reap_test_tmux_session(&target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
}

#[test]
fn ownerless_replacement_after_process_restart_is_not_adopted() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "orphan_replacement_adopts_generation_two_after_process_restart: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-orphan-restart");
    let identity_path = captains_tmp("cortana-orphan-restart-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let orphan_identity = "missing-restart-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 11,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": orphan_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "providerSessionId": null,
                "conversationId": null,
                "checkpoint": "restart-checkpoint",
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "restart-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    captains
        .begin_cortana_recovery("orphan-restart-operation")
        .unwrap();
    captains
        .prepare_cortana_orphan_replacement(
            "orphan-restart-operation",
            &orphan_terminal,
            orphan_identity,
            1,
            "codex",
            test_cortana_effect_identity(200),
        )
        .unwrap();
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let replacement = identities.mint(crate::identity::Role::Cortana).unwrap();
    captains
        .bind_cortana_orphan_replacement_identity("orphan-restart-operation", &replacement.id)
        .unwrap();
    let replacement_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    identities
        .bind_tile(&replacement.id, &replacement_terminal)
        .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-orphan-restart-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
    create_test_tmux_session_with_env(
        &replacement_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                replacement.secret.clone(),
            ),
            ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
            ("T_HUB_CONTROL_ADDR".into(), String::new()),
            ("T_HUB_CONTROL_TOKEN".into(), String::new()),
            (CORTANA_GENERATION_ENV.into(), "2".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&replacement_terminal, "codex").unwrap();

    drop(captains);
    drop(identities);
    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("orphan-restart-control")
        .with_captains_registry(restarted_captains.clone())
        .with_identity_store(restarted_identities)
        .with_apply_sink(sink);
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    restarted_captains
        .claim_provider(
            &replacement_terminal,
            None,
            FleetRole::Cortana,
            Some("codex"),
            None,
            Vec::new(),
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let cross_store_error = restarted_captains
        .commit_cortana_runtime(
            "orphan-restart-operation",
            "unreserved-cross-store-identity",
            2,
            &replacement_terminal,
            "codex",
            None,
        )
        .unwrap_err();
    assert!(cross_store_error.contains("durable orphan replacement intent"));
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "new-request-after-restart",
            "testOrchestratorHome": home,
        }),
    )
    .unwrap_err();
    assert!(error.contains("authority is ambiguous"), "{error}");
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));
    reap_test_tmux_session_and_assert_absent(&replacement_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn prepared_legacy_orphan_restart_retires_only_exact_target_before_replacement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "prepared_legacy_orphan_restart_retires_only_exact_target_before_replacement: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-prepared-orphan-restart");
    let identity_path = captains_tmp("cortana-prepared-orphan-restart-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let sentinel_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let missing_identity = "missing-prepared-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 30,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": missing_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "prepared-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-prepared-restart-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let orphan_target = exact_cortana_tmux_target(&orphan_terminal).unwrap();
    create_test_tmux_session_with_env(
        &orphan_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "unresolved-prepared-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let orphan_effect_identity = durable_cortana_effect_identity(
        tmux::observe_session_effect_identity(&orphan_target).unwrap(),
    );
    let sentinel_target = exact_cortana_tmux_target(&sentinel_terminal).unwrap();
    create_test_tmux_session(&sentinel_target).unwrap();

    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    captains
        .begin_cortana_recovery("prepared-restart-operation")
        .unwrap();
    captains
        .prepare_cortana_orphan_replacement(
            "prepared-restart-operation",
            &orphan_terminal,
            missing_identity,
            1,
            "codex",
            orphan_effect_identity,
        )
        .unwrap();
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive
    );
    drop(captains);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut ctx = test_ctx("prepared-restart-current-token")
        .with_captains_registry(restarted_captains.clone())
        .with_identity_store(identities)
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:63930".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![orphan_terminal.clone()],
    }]);

    let competing_claim = ctx
        .captains
        .claim(
            "prepared-restart-competing-cortana",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let claim_error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "ignored-while-competing-claim-exists",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(claim_error.contains("authority is ambiguous"));
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive
    );
    {
        let mut inner = ctx.captains.lock();
        let record = inner
            .captains
            .iter_mut()
            .find(|record| record.ship_slug == competing_claim.record.ship_slug)
            .unwrap();
        record.state = ClaimState::Vacant;
        record.terminal_id = None;
    }

    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "ignored-after-prepared-restart",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["operationId"], "prepared-restart-operation");
    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["generation"], 2);
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::session_liveness(&sentinel_target),
        tmux::SessionLiveness::Alive
    );
    let replacement_terminal = recovered["terminalId"].as_str().unwrap();
    let replacement_target = exact_cortana_tmux_target(replacement_terminal).unwrap();
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_FILE").unwrap(),
        Some(discovery_file_for_spawn())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_ADDR").unwrap(),
        Some(String::new())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );

    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": replacement_terminal }),
    )
    .unwrap();
    reap_test_tmux_session(&orphan_target).unwrap();
    reap_test_tmux_session(&sentinel_target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn prepared_legacy_orphan_restart_preserves_same_session_replacement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "prepared_legacy_orphan_restart_preserves_same_session_replacement: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-prepared-same-session-reuse");
    let identity_path = captains_tmp("cortana-prepared-same-session-reuse-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let missing_identity = "missing-reused-session-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 40,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": missing_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "same-session-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-same-session-reuse-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let orphan_target = exact_cortana_tmux_target(&orphan_terminal).unwrap();
    create_test_tmux_session_with_env(
        &orphan_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "unresolved-reused-session-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let original_effect = tmux::observe_session_effect_identity(&orphan_target).unwrap();

    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    captains
        .begin_cortana_recovery("same-session-reuse-operation")
        .unwrap();
    captains
        .prepare_cortana_orphan_replacement(
            "same-session-reuse-operation",
            &orphan_terminal,
            missing_identity,
            1,
            "codex",
            durable_cortana_effect_identity(original_effect),
        )
        .unwrap();

    let transition =
        tmux::respawn_pane_exact(&orphan_target, home.to_str().unwrap(), &harness_command).unwrap();
    assert_eq!(transition.before.session_id, transition.after.session_id);
    assert_eq!(
        transition.before.session_created,
        transition.after.session_created
    );
    assert_eq!(transition.before.window_id, transition.after.window_id);
    assert_eq!(transition.before.pane_id, transition.after.pane_id);
    assert_ne!(transition.before.pane_pid, transition.after.pane_pid);
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let replacement_effect = tmux::observe_session_effect_identity(&orphan_target).unwrap();
    assert_ne!(replacement_effect, original_effect);
    drop(captains);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut ctx = test_ctx("same-session-reuse-current-token")
        .with_captains_registry(restarted_captains.clone())
        .with_identity_store(identities)
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:63930".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![orphan_terminal.clone()],
    }]);

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "ignored-after-same-session-reuse",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(error.contains("evidence is ambiguous"), "{error}");
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive,
        "same-session replacement must survive a stale prepared retirement"
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&orphan_target).unwrap(),
        replacement_effect
    );
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));

    reap_test_tmux_session(&orphan_target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn orphan_replacement_restart_rejects_copied_bearers_and_control_env_drift() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "orphan_replacement_restart_rejects_copied_bearers_and_control_env_drift: tmux or node not on PATH - skipping"
            );
        return;
    }
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    for case in [
        "copied-bearer-wrong-tile",
        "missing-control-file",
        "wrong-control-file",
        "nonblank-control-addr",
        "nonblank-control-token",
    ] {
        let registry_path = captains_tmp(&format!("cortana-negative-restart-{case}"));
        let identity_path = captains_tmp(&format!("cortana-negative-identity-{case}"));
        let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 18,
                "seq": 1,
                "captains": [],
                "workspaces": [{
                    "id": CAPTAIN_WORKSPACE_ID,
                    "name": CAPTAIN_WORKSPACE_NAME,
                    "kind": "captain",
                    "tileIds": [orphan_terminal.clone()]
                }],
                "cortana": {
                    "identityId": "missing-negative-cortana-identity",
                    "generation": 1,
                    "terminalId": orphan_terminal,
                    "harness": "codex",
                    "providerSessionId": null,
                    "conversationId": null,
                    "checkpoint": null,
                    "recovery": {
                        "kind": "healthy",
                        "operation_id": "negative-original",
                        "verified_at": 1
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        captains
            .begin_cortana_recovery("negative-restart-operation")
            .unwrap();
        captains
            .prepare_cortana_orphan_replacement(
                "negative-restart-operation",
                &orphan_terminal,
                "missing-negative-cortana-identity",
                1,
                "codex",
                test_cortana_effect_identity(300),
            )
            .unwrap();
        let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
        let replacement = identities.mint(crate::identity::Role::Cortana).unwrap();
        captains
            .bind_cortana_orphan_replacement_identity("negative-restart-operation", &replacement.id)
            .unwrap();
        let replacement_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let bound_terminal = if case == "copied-bearer-wrong-tile" {
            "source-tile"
        } else {
            replacement_terminal.as_str()
        };
        identities
            .bind_tile(&replacement.id, bound_terminal)
            .unwrap();
        let home = std::env::temp_dir().join(format!(
            "t-hub-cortana-negative-restart-{case}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut environment = vec![
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                replacement.secret.clone(),
            ),
            (CORTANA_GENERATION_ENV.into(), "2".into()),
        ];
        if case != "missing-control-file" {
            environment.push((
                "T_HUB_CONTROL_FILE".into(),
                if case == "wrong-control-file" {
                    "/tmp/foreign-t-hub-control.json".into()
                } else {
                    discovery_file_for_spawn()
                },
            ));
        }
        environment.push((
            "T_HUB_CONTROL_ADDR".into(),
            if case == "nonblank-control-addr" {
                "127.0.0.1:9".into()
            } else {
                String::new()
            },
        ));
        environment.push((
            "T_HUB_CONTROL_TOKEN".into(),
            if case == "nonblank-control-token" {
                "copied-global-token".into()
            } else {
                String::new()
            },
        ));
        let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
        create_test_tmux_session_with_env(
            &replacement_target,
            home.to_str().unwrap(),
            Some(&harness_command),
            &environment,
        )
        .unwrap();
        wait_for_harness_started(&replacement_terminal, "codex").unwrap();

        drop(captains);
        drop(identities);
        let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        let restarted_identities =
            Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
        let ctx = test_ctx(&format!("negative-restart-{case}"))
            .with_captains_registry(restarted_captains.clone())
            .with_identity_store(restarted_identities)
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            }));

        let error = dispatch(
            &ctx,
            "reconcile_cortana",
            &json!({
                "operationId": "new-request-must-not-replace-durable-operation",
                "testOrchestratorHome": home,
            }),
        )
        .unwrap_err();
        assert!(error.contains("reserved scope changed"), "{case}: {error}");
        assert!(matches!(
            restarted_captains.cortana_identity().recovery,
            crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined { .. }
        ));
        assert!(!restarted_captains
            .snapshot()
            .captains
            .iter()
            .any(|captain| captain.role == FleetRole::Cortana));
        assert_eq!(
            tmux::session_liveness(&replacement_target),
            tmux::SessionLiveness::Alive,
            "{case} must fail closed without killing an untrusted candidate"
        );

        reap_test_tmux_session(&replacement_target).unwrap();
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_file(registry_path).ok();
        std::fs::remove_file(identity_path).ok();
    }
    std::fs::remove_dir_all(harness_bin_dir).ok();
}

#[test]
fn discovered_preowner_cortana_is_quarantined_and_replacement_consumes_spawn_rate() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-no-spawn-rate")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink)
        .with_governor(Arc::new(SpawnGovernor::new(64, 0.0, 1.0)));
    ctx.addr = "127.0.0.1:4249".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-no-spawn-rate-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.identity.bind_tile(&identity.id, &terminal_id).unwrap();
    let target = exact_cortana_tmux_target(&terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
            ("T_HUB_CONTROL_ADDR".into(), String::new()),
            ("T_HUB_CONTROL_TOKEN".into(), String::new()),
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                identity.secret.clone(),
            ),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let adopted = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-no-spawn-rate-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(adopted["action"], "recover");
    assert_eq!(adopted["generation"], 2);
    assert_ne!(adopted["terminalId"], terminal_id);
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive,
        "pre-owner quarantine must not signal the old runtime"
    );

    assert!(admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_err());
    let replacement_terminal = adopted["terminalId"].as_str().unwrap();
    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": replacement_terminal }),
    )
    .unwrap();
    reap_test_tmux_session(&target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn copied_cortana_bearer_on_a_second_terminal_fails_closed_without_quarantine() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-cortana-quarantine-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut ctx = test_ctx("cortana-quarantine")
        .with_apply_sink(sink)
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.addr = "127.0.0.1:4244".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-quarantine-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let mut terminal_ids = (0..2)
        .map(|_| uuid::Uuid::new_v4().simple().to_string()[..8].to_string())
        .collect::<Vec<_>>();
    ctx.identity
        .bind_tile(&identity.id, &terminal_ids[0])
        .unwrap();
    let environment = vec![
        ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
        ("T_HUB_CONTROL_ADDR".into(), String::new()),
        ("T_HUB_CONTROL_TOKEN".into(), String::new()),
        (
            crate::identity::SESSION_TOKEN_ENV.into(),
            identity.secret.clone(),
        ),
        (CORTANA_GENERATION_ENV.into(), "7".into()),
    ];
    for terminal_id in &terminal_ids {
        let target = exact_cortana_tmux_target(terminal_id).unwrap();
        create_test_tmux_session_with_env(
            &target,
            home.to_str().unwrap(),
            Some(&harness_command),
            &environment,
        )
        .unwrap();
        wait_for_harness_started(terminal_id, "codex").unwrap();
    }
    terminal_ids.sort();

    let degraded = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-quarantine-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();

    assert_eq!(degraded["action"], "degraded");
    assert_eq!(degraded["healthy"], false);
    assert_eq!(degraded["quarantinedTerminalIds"], json!([]));
    assert!(degraded["degradedReason"]
        .as_str()
        .is_some_and(|reason| reason.contains("lacks authoritative identity")));
    assert!(terminal_ids
        .iter()
        .all(|terminal_id| tmux::session_liveness(
            &exact_cortana_tmux_target(terminal_id).unwrap()
        ) == tmux::SessionLiveness::Alive));
    assert!(ctx.captains.cortana_identity().identity_id.is_none());
    assert!(ctx.identity.resolve(&identity.secret).is_some());
    assert!(!read_audit(&audit_dir)
        .iter()
        .any(|record| record["decision"] == "quarantined"));
    for terminal_id in &terminal_ids {
        reap_test_tmux_session(&exact_cortana_tmux_target(terminal_id).unwrap()).unwrap();
    }
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(audit_dir);
}

#[test]
fn ambiguous_quarantine_revokes_all_bearers_without_signaling_runtimes() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-cortana-identity-quarantine-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("cortana-identity-quarantine")
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    let durable_identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let foreign_identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let durable_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let foreign_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.identity
        .bind_tile(&durable_identity.id, &durable_terminal)
        .unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, &foreign_terminal)
        .unwrap();
    for terminal_id in [&durable_terminal, &foreign_terminal] {
        create_test_tmux_session(&exact_cortana_tmux_target(terminal_id).unwrap()).unwrap();
    }
    let candidates = vec![
        crate::cortana_reconcile::CortanaRuntimeCandidate {
            terminal_id: durable_terminal.clone(),
            identity_id: Some(durable_identity.id.clone()),
            generation: 4,
            harness: "codex".into(),
            provider_session_id: None,
            terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
            harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
            identity_bound_to_terminal: true,
            canonical_control_file: true,
            rotating_control_env_scrubbed: true,
            stale_legacy_control_env: false,
            unresolved_session_bearer: false,
            effect_identity: None,
            current_control_capability: true,
            trusted_cortana_identity: true,
        },
        crate::cortana_reconcile::CortanaRuntimeCandidate {
            terminal_id: foreign_terminal.clone(),
            identity_id: Some(foreign_identity.id.clone()),
            generation: 4,
            harness: "codex".into(),
            provider_session_id: None,
            terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
            harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
            identity_bound_to_terminal: true,
            canonical_control_file: true,
            rotating_control_env_scrubbed: true,
            stale_legacy_control_env: false,
            unresolved_session_bearer: false,
            effect_identity: None,
            current_control_capability: true,
            trusted_cortana_identity: true,
        },
    ];
    let durable = crate::cortana_reconcile::CortanaDurableIdentity {
        identity_id: Some(durable_identity.id.clone()),
        generation: 4,
        terminal_id: Some(durable_terminal.clone()),
        harness: Some("codex".into()),
        ..Default::default()
    };
    let requested = vec![durable_terminal, foreign_terminal];

    let quarantined = quarantine_cortana_runtimes(
        &ctx,
        "cortana-identity-quarantine-1",
        &requested,
        &candidates,
        &durable,
    )
    .unwrap();

    let mut expected = requested;
    expected.sort();
    assert_eq!(quarantined, expected);
    for (identity, terminal_id) in [
        (&durable_identity, &candidates[0].terminal_id),
        (&foreign_identity, &candidates[1].terminal_id),
    ] {
        assert!(ctx.identity.resolve(&identity.secret).is_none());
        assert!(ctx.identity.is_revoked(&identity.id));
        let denied = dispatch_authenticated(
            &ctx,
            req_session(
                &ctx.token,
                &identity.secret,
                "register_project",
                json!({"rootPath": "/tmp/ambiguous-bearer-must-not-register"}),
            ),
        );
        assert!(!denied.ok);
        assert_eq!(
                denied.error.as_deref(),
                Some(
                    "unauthorized: 'register_project' requires a valid T_HUB_SESSION_TOKEN with the control capability"
                )
            );
        assert_eq!(
            tmux::session_liveness(&exact_cortana_tmux_target(terminal_id).unwrap()),
            tmux::SessionLiveness::Alive
        );
    }
    assert!(!ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    for terminal_id in &expected {
        reap_test_tmux_session_and_assert_absent(&exact_cortana_tmux_target(terminal_id).unwrap());
    }
    let _ = std::fs::remove_dir_all(audit_dir);
}

#[test]
fn legacy_healthy_cortana_without_active_attestation_fails_closed_on_restart() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captains_path = captains_tmp(&format!("legacy-active-attestation-{nonce}"));
    let identities_path = std::env::temp_dir().join(format!(
        "t-hub-legacy-active-attestation-identities-{nonce}.json"
    ));
    let tile = format!("co{}", &nonce[..6]);
    let identities = Arc::new(crate::identity::IdentityStore::load(
        identities_path.clone(),
    ));
    let secret = {
        let registry = CaptainsRegistry::load(captains_path.clone());
        mint_current_cortana_session(&identities, &registry, &tile)
    };
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&captains_path).unwrap()).unwrap();
    document["schemaVersion"] = json!(28);
    document["cortana"]
        .as_object_mut()
        .unwrap()
        .remove("activeHarnessAttestation");
    std::fs::write(
        &captains_path,
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();

    let restarted_registry = Arc::new(CaptainsRegistry::load(captains_path.clone()));
    let restarted = test_ctx("legacy-active-attestation-restart")
        .with_captains_registry(Arc::clone(&restarted_registry))
        .with_identity_store(identities);
    assert!(matches!(
        restarted_registry.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Degraded { .. }
    ));
    let resolved = resolve_identity(&restarted, &secret).unwrap();
    assert_eq!(resolved.fleet_role, None);
    assert_eq!(resolved.mint_role, crate::identity::Role::Unknown);

    std::fs::remove_file(captains_path).ok();
    std::fs::remove_file(identities_path).ok();
}
