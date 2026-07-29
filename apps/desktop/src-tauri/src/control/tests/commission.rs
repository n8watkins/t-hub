use super::*;

#[test]
fn concurrent_captain_commission_and_cortana_recovery_follow_one_lock_order() {
    if !tmux_process_tests_available() {
        eprintln!(
                "concurrent_captain_commission_and_cortana_recovery_follow_one_lock_order: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut context = test_ctx("ordered-provisioning")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink)
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)));
    context.addr = "127.0.0.1:4251".into();
    let ctx = Arc::new(context);
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-ordered-provisioning".into(),
            name: "Ordered Provisioning".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "ordered-provisioning".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-ordered-provisioning-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let (reconcile_reached_tx, reconcile_reached_rx) = mpsc::sync_channel(1);
    let (reconcile_resume_tx, reconcile_resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "cortana_spawn_admission_required",
        reached: reconcile_reached_tx,
        resume: reconcile_resume_rx,
    }));

    let reconcile_ctx = Arc::clone(&ctx);
    let reconcile_command = harness_command.clone();
    let reconcile_home = home.clone();
    let (reconcile_done_tx, reconcile_done_rx) = mpsc::sync_channel(1);
    let reconcile_thread = std::thread::spawn(move || {
        let result = dispatch(
            &reconcile_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": "ordered-cortana-recovery",
                "testOrchestratorHome": reconcile_home,
                "testStartupCommand": reconcile_command,
            }),
        );
        reconcile_done_tx.send(result).unwrap();
    });
    assert_eq!(
        reconcile_reached_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("Cortana did not reach the ordered admission boundary"),
        "cortana_spawn_admission_required"
    );

    let (commission_reached_tx, commission_reached_rx) = mpsc::sync_channel(1);
    let (commission_resume_tx, commission_resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "commission_initial_inspection",
        reached: commission_reached_tx,
        resume: commission_resume_rx,
    }));

    let commission_ctx = Arc::clone(&ctx);
    let commission_command = harness_command.clone();
    let (commission_done_tx, commission_done_rx) = mpsc::sync_channel(1);
    let commission_thread = std::thread::spawn(move || {
        let response = dispatch_authenticated(
            &commission_ctx,
            ControlRequest {
                token: commission_ctx.token.clone(),
                command: "commission_captain".into(),
                args: json!({
                    "projectId": "project-ordered-provisioning",
                    "assignment": "Own the ordered project",
                    "harness": "codex",
                    "shipSlug": "ordered-provisioning",
                    "testStartupCommand": commission_command,
                    "testSkipPowderHealth": true,
                }),
                session: String::new(),
                host: commission_ctx.host_token.clone(),
                v: None,
            },
        );
        commission_done_tx.send(response).unwrap();
    });

    assert_eq!(
        commission_reached_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("Captain commission did not reach its inspection pass"),
        "commission_initial_inspection"
    );
    commission_resume_tx.send(()).unwrap();

    // Cortana still owns only provisioning during its inspection pass.
    // Captain inspection may wait for that lock, but must not acquire spawn
    // admission first and recreate the inverse ordering that deadlocked the
    // old one-pass implementation.
    assert!(
        ctx.dispatch_admission.try_lock().is_ok(),
        "Captain inspection held dispatch admission while waiting on provisioning"
    );
    reconcile_resume_tx.send(()).unwrap();

    let commission = commission_done_rx
        .recv_timeout(Duration::from_secs(8))
        .expect("Captain commission deadlocked with Cortana reconciliation");
    assert!(commission.ok, "commission failed: {:?}", commission.error);
    let reconciled = reconcile_done_rx
        .recv_timeout(Duration::from_secs(8))
        .expect("Cortana reconciliation deadlocked with Captain commission")
        .unwrap();
    commission_thread.join().unwrap();
    reconcile_thread.join().unwrap();

    assert_eq!(reconciled["healthy"], true);
    assert_eq!(reconciled["action"], "create");
    let snapshot = ctx.captains.snapshot();
    assert_eq!(
        snapshot
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Captain)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );
    assert_eq!(
        snapshot.cortana.terminal_id.as_deref(),
        reconciled["terminalId"].as_str()
    );

    for terminal_id in snapshot
        .captains
        .iter()
        .filter_map(|captain| captain.terminal_id.as_deref())
    {
        reap_test_tmux_session(&tmux_target(terminal_id)).unwrap();
    }
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn commission_captain_spawns_binds_bootstraps_and_deduplicates() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("secret")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(sink);
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "project-tab".into(),
            name: "Commission Crew".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-e2e".into(),
            name: "Commission E2E".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "commission-e2e".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();

    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let args = json!({
        "requestId": "commission-first-operation",
        "projectId": "project-e2e",
        "assignment": "Keep this project stable",
        "harness": "codex",
        "shipSlug": "commission-e2e",
        "workspaceTabIds": ["project-tab"],
        "testStartupCommand": harness_command,
        "testSkipPowderHealth": true,
    });
    let first = dispatch(&ctx, "commission_captain", &args).unwrap();
    assert_eq!(first["alreadyCommissioned"], false);
    assert_eq!(first["captain"]["projectId"], "project-e2e");
    assert_eq!(first["captain"]["assignment"], "Keep this project stable");
    assert_eq!(first["captain"]["harness"], "codex");
    assert_eq!(first["captain"]["workspaceTabIds"][0], "project-tab");
    assert_eq!(first["project"]["powder"]["repository"], "commission-e2e");
    assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
    let terminal_id = first["captain"]["terminalId"].as_str().unwrap().to_string();
    assert!(tmux::has_session(&tmux_target(&terminal_id)));

    let bootstrap = dispatch(
        &ctx,
        "captain_bootstrap",
        &json!({ "captainSessionId": terminal_id }),
    )
    .unwrap();
    assert_eq!(bootstrap["recoverySource"], "captains-registry");
    assert!(bootstrap["instructions"]
        .as_str()
        .unwrap()
        .contains("Use $captain"));
    assert!(bootstrap["instructions"]
        .as_str()
        .unwrap()
        .contains("commission-e2e"));

    let mut claude_captain = ctx.captains.snapshot().captains[0].clone();
    claude_captain.harness = Some("claude".into());
    let claude_instructions = bootstrap_instructions(&claude_captain, &ctx.captains.projects()[0]);
    assert!(claude_instructions.contains("Use /captain"));
    assert!(!claude_instructions.contains("Use $captain"));

    let mut retry_args = args.clone();
    retry_args["requestId"] = json!("commission-fresh-noop-operation");
    let retry = dispatch(&ctx, "commission_captain", &retry_args).unwrap();
    assert_eq!(retry["alreadyCommissioned"], true);
    assert_eq!(retry["captain"]["terminalId"], terminal_id);
    assert_eq!(ctx.captains.snapshot().captains.len(), 1);
    assert!(
            admit_spawn(&ctx, SpawnPurpose::Ordinary, 0, None).is_ok(),
            "an exact no-op commission with a fresh operation ID must not consume the remaining spawn-rate token"
        );

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
}

#[test]
fn non_git_captain_checkpoint_reload_and_bootstrap_preserve_real_projects() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let base = std::env::temp_dir().join(format!(
        "t-hub-non-git-captain-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let populated = base.join("populated");
    let empty = base.join("empty");
    std::fs::create_dir_all(&populated).unwrap();
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(populated.join("README.txt"), b"non-Git fixture\n").unwrap();
    let registry_path = base.join("captains.json");
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    for (project_id, name, root) in [
        ("non-git-populated", "Populated non-Git", &populated),
        ("non-git-empty", "Empty non-Git", &empty),
    ] {
        let root = root.canonicalize().unwrap().to_string_lossy().into_owned();
        registry
            .upsert_project(ProjectRecord {
                root_path: Some(root.clone()),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                project_id: project_id.into(),
                name: name.into(),
                repo_root: root,
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "non-git-populated-tab".into(),
            name: "Populated".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "non-git-empty-tab".into(),
            name: "Empty".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("non-git-captain")
        .with_captains_registry(Arc::clone(&registry))
        .with_tab_registry(Arc::clone(&tabs))
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(sink);
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let mut terminal_ids = Vec::new();
    for (project_id, ship_slug, tab_id) in [
        (
            "non-git-populated",
            "non-git-populated-ship",
            "non-git-populated-tab",
        ),
        ("non-git-empty", "non-git-empty-ship", "non-git-empty-tab"),
    ] {
        let result = dispatch(
            &ctx,
            "commission_captain",
            &json!({
                "requestId": format!("commission-{project_id}"),
                "projectId": project_id,
                "assignment": format!("Maintain {project_id}"),
                "harness": "codex",
                "shipSlug": ship_slug,
                "workspaceTabIds": [tab_id],
                "testStartupCommand": harness_command,
                "testSkipPowderHealth": true,
            }),
        )
        .unwrap();
        assert_eq!(result["alreadyCommissioned"], false);
        terminal_ids.push(
            result["captain"]["terminalId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
        let before_dispatch = ctx.captains.snapshot();
        let retired_dispatch = dispatch_authenticated(
            &ctx,
            req(
                "non-git-captain",
                "dispatch_crew",
                json!({
                    "captainSessionId": terminal_ids.last().unwrap(),
                    "cardId": "non-git-card",
                    "task": "must refuse before Git"
                }),
            ),
        );
        assert!(!retired_dispatch.ok);
        assert_eq!(retired_dispatch.error_kind, None);
        assert_eq!(
            retired_dispatch.error.as_deref(),
            Some(
                "control: command 'dispatch_crew' is not exposed over the control channel \
                 (process-changing/destructive commands are gated; see PRD §11.2)"
            )
        );
        assert_eq!(ctx.captains.snapshot().seq, before_dispatch.seq);
        let checkpoint = dispatch(
            &ctx,
            "captain_checkpoint",
            &json!({
                "shipSlug": ship_slug,
                "conversationId": format!("conversation-{project_id}"),
                "resumePoint": format!("resume-{project_id}"),
            }),
        )
        .unwrap();
        assert_eq!(checkpoint["accepted"], "captain_checkpoint");
    }
    assert_eq!(ctx.captains.projects().len(), 2);
    assert_eq!(ctx.captains.snapshot().captains.len(), 2);
    assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
    assert!(populated.join(".git").metadata().is_err());
    assert!(empty.join(".git").metadata().is_err());

    let restarted_registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted = test_ctx("non-git-captain-restart")
        .with_captains_registry(Arc::clone(&restarted_registry))
        .with_tab_registry(Arc::clone(&tabs))
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    for (project_id, ship_slug, terminal_id) in [
        (
            "non-git-populated",
            "non-git-populated-ship",
            &terminal_ids[0],
        ),
        ("non-git-empty", "non-git-empty-ship", &terminal_ids[1]),
    ] {
        let project = restarted
            .captains
            .projects()
            .into_iter()
            .find(|project| project.project_id == project_id)
            .unwrap();
        assert_eq!(project.vcs_capability.as_deref(), Some("none"));
        assert_eq!(
            project.root_path.as_deref(),
            Some(project.repo_root.as_str())
        );
        let bootstrap = dispatch(
            &restarted,
            "captain_bootstrap",
            &json!({ "captainSessionId": terminal_id }),
        )
        .unwrap();
        assert_eq!(bootstrap["project"]["projectId"], project_id);
        assert_eq!(bootstrap["project"]["vcsCapability"], "none");
        assert_eq!(bootstrap["captain"]["shipSlug"], ship_slug);
        assert_eq!(
            bootstrap["captain"]["conversationId"],
            format!("conversation-{project_id}")
        );
        assert_eq!(
            bootstrap["captain"]["resumePoint"],
            format!("resume-{project_id}")
        );
        assert_eq!(
            bootstrap["captain"]["terminalId"].as_str(),
            Some(terminal_id.as_str())
        );
        assert!(bootstrap["instructions"]
            .as_str()
            .unwrap()
            .contains(project_id));
    }
    for (project_id, ship_slug, tab_id) in [
        (
            "non-git-populated",
            "non-git-populated-ship",
            "non-git-populated-tab",
        ),
        ("non-git-empty", "non-git-empty-ship", "non-git-empty-tab"),
    ] {
        let retry = dispatch(
            &restarted,
            "commission_captain",
            &json!({
                "requestId": format!("retry-{project_id}"),
                "projectId": project_id,
                "assignment": format!("Maintain {project_id}"),
                "harness": "codex",
                "shipSlug": ship_slug,
                "workspaceTabIds": [tab_id],
                "testStartupCommand": harness_command,
                "testSkipPowderHealth": true,
            }),
        )
        .unwrap();
        assert_eq!(retry["alreadyCommissioned"], true);
    }
    assert_eq!(restarted.captains.projects().len(), 2);
    assert_eq!(restarted.captains.snapshot().captains.len(), 2);
    assert!(populated.join(".git").metadata().is_err());
    assert!(empty.join(".git").metadata().is_err());
    assert_eq!(git::worktree_list_calls(), 0);
    for terminal_id in terminal_ids {
        let _ = dispatch(
            &restarted,
            "close_terminal",
            &json!({ "sessionId": terminal_id }),
        );
    }
    std::fs::remove_dir_all(&harness_bin_dir).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn non_git_captain_commission_persistence_failure_preserves_project_and_cleans_exactly() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let base = std::env::temp_dir().join(format!(
        "t-hub-non-git-captain-failure-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let root = base.join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), b"non-Git\n").unwrap();
    let registry_path = base.join("captains.json");
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let root = root.canonicalize().unwrap().to_string_lossy().into_owned();
    registry
        .upsert_project(ProjectRecord {
            root_path: Some(root.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "non-git-failure".into(),
            name: "Non-Git failure".into(),
            repo_root: root.clone(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "non-git-failure-tab".into(),
            name: "Non-Git failure".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    let ctx = test_ctx("non-git-captain-failure")
        .with_captains_registry(Arc::clone(&registry))
        .with_tab_registry(tabs)
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let error = dispatch(
        &ctx,
        "commission_captain",
        &json!({
            "projectId": "non-git-failure",
            "assignment": "Recover safely",
            "harness": "codex",
            "shipSlug": "non-git-failure-ship",
            "workspaceTabIds": ["non-git-failure-tab"],
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
            "testFailCommitPersist": true
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("commission binding persistence failure"),
        "got: {error}"
    );
    let snapshot = registry.snapshot();
    assert!(snapshot.captains.is_empty());
    assert!(snapshot.pending_fleet_operations.is_empty());
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].vcs_capability.as_deref(), Some("none"));
    assert!(std::path::Path::new(&root).join(".git").metadata().is_err());
    let restarted = CaptainsRegistry::load(registry_path);
    assert_eq!(restarted.projects().len(), 1);
    assert_eq!(restarted.snapshot().captains.len(), 0);
    assert_eq!(
        restarted.projects()[0].vcs_capability.as_deref(),
        Some("none")
    );
    assert!(std::path::Path::new(&root).join(".git").metadata().is_err());
    std::fs::remove_dir_all(&harness_bin_dir).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn concurrent_non_git_commissions_converge_and_conflicts_fail_closed() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let base = std::env::temp_dir().join(format!(
        "t-hub-non-git-captain-concurrent-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let root = base.join("source");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap().to_string_lossy().into_owned();
    let registry = Arc::new(CaptainsRegistry::load(base.join("captains.json")));
    registry
        .upsert_project(ProjectRecord {
            root_path: Some(root.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "non-git-concurrent".into(),
            name: "Non-Git concurrent".into(),
            repo_root: root,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "non-git-concurrent-tab".into(),
            name: "Concurrent".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    let context = Arc::new(
        test_ctx("non-git-concurrent")
            .with_captains_registry(Arc::clone(&registry))
            .with_tab_registry(tabs)
            .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            })),
    );
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let joins = (0..2)
        .map(|index| {
            let context = Arc::clone(&context);
            let barrier = Arc::clone(&barrier);
            let harness_command = harness_command.clone();
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &context,
                    "commission_captain",
                    &json!({
                        "requestId": format!("concurrent-{index}"),
                        "projectId": "non-git-concurrent",
                        "assignment": "Same explicit-none assignment",
                        "harness": "codex",
                        "shipSlug": "non-git-concurrent-ship",
                        "workspaceTabIds": ["non-git-concurrent-tab"],
                        "testStartupCommand": harness_command,
                        "testSkipPowderHealth": true,
                    }),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(Result::is_ok), "results: {results:?}");
    let result_terminal_ids = results
        .iter()
        .map(|result| {
            result.as_ref().unwrap()["captain"]["terminalId"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(result_terminal_ids.len(), 2);
    assert_eq!(result_terminal_ids[0], result_terminal_ids[1]);
    assert_eq!(registry.snapshot().captains.len(), 1);
    assert_eq!(registry.snapshot().pending_fleet_operations.len(), 0);
    let terminal_id = registry.snapshot().captains[0].terminal_id.clone().unwrap();
    assert_eq!(terminal_id, result_terminal_ids[0]);
    let matching_sessions = tmux::list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session == &tmux_target(&terminal_id))
        .collect::<Vec<_>>();
    assert_eq!(matching_sessions, vec![tmux_target(&terminal_id)]);
    let before_conflict = registry.snapshot();
    let conflict = dispatch(
        &context,
        "commission_captain",
        &json!({
            "requestId": "concurrent-conflict",
            "projectId": "non-git-concurrent",
            "assignment": "Conflicting assignment",
            "harness": "codex",
            "shipSlug": "non-git-concurrent-ship",
            "workspaceTabIds": ["non-git-concurrent-tab"],
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
        }),
    )
    .unwrap_err();
    assert_eq!(
            conflict,
            "commission_captain: project 'Non-Git concurrent' already has live Captain 'non-git-concurrent-ship' with a different assignment, harness, or shipSlug; release or update that Captain explicitly"
        );
    assert_eq!(registry.snapshot().captains.len(), 1);
    assert_eq!(registry.snapshot().seq, before_conflict.seq);
    assert!(base.join("source/.git").metadata().is_err());
    let _ = dispatch(
        &context,
        "close_terminal",
        &json!({ "sessionId": terminal_id }),
    );
    std::fs::remove_dir_all(&harness_bin_dir).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn commission_binding_failure_never_projects_a_ghost_captain_or_placement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("commission-projection-rollback");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-commission-fail".into(),
            name: "Commission Failure".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "commission-failure".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "commission-work".into(),
        name: "Commission Work".into(),
        tile_ids: Vec::new(),
    }]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = test_ctx("commission-projection-rollback")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_apply_sink(sink.clone());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let terminal_id = suffix[..8].to_string();
    let identities_before = context.identity.len();

    let error = dispatch(
        &context,
        "commission_captain",
        &json!({
            "projectId": "project-commission-fail",
            "assignment": "Own failure",
            "harness": "codex",
            "shipSlug": "commission-failure",
            "workspaceTabIds": ["commission-work"],
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
            "testFailCommitPersist": true,
            "testTerminalId": terminal_id
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("commission binding persistence failure"),
        "got: {error}"
    );
    assert!(captains.snapshot().captains.is_empty());
    assert!(captains.snapshot().pending_fleet_operations.is_empty());
    assert_eq!(context.identity.len(), identities_before);
    assert_eq!(
        tmux::session_liveness(&tmux_target(&terminal_id)),
        tmux::SessionLiveness::Gone
    );
    assert!(tabs
        .snapshot()
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .is_empty());
    let projected_commands: Vec<String> = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(command, _)| command.clone())
        .collect();
    assert!(!projected_commands.iter().any(|command| {
        matches!(
            command.as_str(),
            "spawn_terminal" | "move_tile" | "sync_captains"
        )
    }));

    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn commission_crash_recovery_reaps_exact_tmux_identity_and_unprojected_intent() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("commission-crash-recovery");
    let non_git_root = std::env::temp_dir().join(format!(
        "t-hub-commission-crash-non-git-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&non_git_root).unwrap();
    std::fs::write(non_git_root.join("README"), b"non-Git crash fixture\n").unwrap();
    let non_git_root = non_git_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let identity_path = path.with_extension("identities.json");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: Some(non_git_root.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "project-commission-crash".into(),
            name: "Commission Crash".into(),
            repo_root: non_git_root.clone(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "commission-crash".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(captains.workspace_projection());
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = Arc::new(
        test_ctx("commission-crash-recovery")
            .with_captains_registry(Arc::clone(&captains))
            .with_tab_registry(Arc::clone(&tabs))
            .with_identity_store(Arc::clone(&identities))
            .with_apply_sink(sink.clone()),
    );
    git::reset_worktree_list_calls();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "commission_effect_applied",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let commissioning_context = Arc::clone(&context);
    let commissioning = std::thread::spawn(move || {
        git::reset_worktree_list_calls();
        let result = dispatch(
            &commissioning_context,
            "commission_captain",
            &json!({
            "projectId": "project-commission-crash",
            "assignment": "Own crash recovery",
            "harness": "codex",
            "shipSlug": "commission-crash",
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
            "testCrashAfterTmux": true
            }),
        );
        (result, git::worktree_list_calls())
    });
    assert_eq!(
        reached_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "commission_effect_applied"
    );
    let during_effect = dispatch(&context, "list_tabs", &Value::Null).unwrap();
    assert!(during_effect["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|workspace| workspace["tileIds"].as_array().unwrap().is_empty()));
    assert!(dispatch(&context, "list_captains", &Value::Null).is_ok());
    resume_tx.send(()).unwrap();
    let (commission_result, commission_worktree_calls) = commissioning.join().unwrap();
    let error = commission_result.unwrap_err();
    assert!(error.contains("injected commission crash"));
    assert_eq!(commission_worktree_calls, 0);
    let durable = captains.snapshot();
    assert!(durable.captains.is_empty());
    assert_eq!(durable.pending_fleet_operations.len(), 1);
    let PendingFleetOperationPayload::CommissionCaptain {
        terminal_id,
        identity_id,
        ..
    } = &durable.pending_fleet_operations[0].payload
    else {
        panic!("expected pending commission operation")
    };
    assert!(tmux::has_session(&tmux_target(terminal_id)));
    assert!(identity_id.is_some());
    assert_eq!(identities.len(), 1);
    let listed = dispatch(&context, "list_tabs", &Value::Null).unwrap();
    assert!(listed["tabs"].as_array().unwrap().iter().all(|workspace| {
        workspace["tileIds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tile| tile != terminal_id)
    }));
    assert!(sink.calls.lock().unwrap().is_empty());
    let terminal_id = terminal_id.clone();

    drop(context);
    drop(tabs);
    drop(captains);
    drop(identities);
    let restarted_captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let restarted_tabs = Arc::new(TabRegistry::new());
    restarted_tabs.replace(restarted_captains.workspace_projection());
    let restarted = test_ctx("commission-crash-recovery-restart")
        .with_captains_registry(Arc::clone(&restarted_captains))
        .with_tab_registry(Arc::clone(&restarted_tabs))
        .with_identity_store(Arc::clone(&restarted_identities));
    git::reset_worktree_list_calls();
    recover_pending_fleet_operations(&restarted);
    assert!(!tmux::has_session(&tmux_target(&terminal_id)));
    assert_eq!(restarted_identities.len(), 0);
    assert!(restarted_captains
        .snapshot()
        .pending_fleet_operations
        .is_empty());
    assert!(restarted_captains.snapshot().captains.is_empty());
    assert_eq!(restarted_captains.projects().len(), 1);
    assert_eq!(
        restarted_captains.projects()[0].vcs_capability.as_deref(),
        Some("none")
    );
    assert!(std::path::Path::new(&non_git_root)
        .join(".git")
        .metadata()
        .is_err());
    assert_eq!(git::worktree_list_calls(), 0);

    drop(restarted);
    drop(restarted_tabs);
    drop(restarted_identities);
    drop(restarted_captains);
    let second_captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let second_identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let second = test_ctx("commission-crash-recovery-second-restart")
        .with_captains_registry(Arc::clone(&second_captains))
        .with_identity_store(second_identities);
    git::reset_worktree_list_calls();
    recover_pending_fleet_operations(&second);
    assert!(second_captains
        .snapshot()
        .pending_fleet_operations
        .is_empty());
    assert!(second_captains.snapshot().captains.is_empty());
    assert_eq!(second_captains.projects().len(), 1);
    assert!(std::path::Path::new(&non_git_root)
        .join(".git")
        .metadata()
        .is_err());

    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(identity_path.with_extension("json.bak"));
    let _ = std::fs::remove_file(identity_path);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(&non_git_root);
}

#[test]
fn captain_bootstrap_uses_the_wsl_runtime_root_without_mutating_the_project() {
    let canonical_repo_root =
        "\\\\?\\UNC\\wsl.localhost\\Ubuntu-24.04\\home\\natkins\\projects\\tools\\t-hub\\t-hub-app";
    let runtime_repo_root = "/home/natkins/projects/tools/t-hub/t-hub-app";
    let captain = CaptainRecord {
        ship_slug: "t-hub-app".into(),
        assignment_id: "assignment:project-1:t-hub-app".into(),
        display_name: "t-hub-app".into(),
        role: FleetRole::Captain,
        claude_uuid: None,
        provider: Some("codex".into()),
        provider_session_id: None,
        terminal_id: None,
        project_id: Some("project-e2e".into()),
        assignment: Some("Keep this project stable".into()),
        harness: Some("codex".into()),
        conversation_id: None,
        resume_point: None,
        workspace_tab_ids: Vec::new(),
        crew: Vec::new(),
        state: ClaimState::Active,
    };
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-e2e".into(),
        name: "T-Hub".into(),
        repo_root: canonical_repo_root.into(),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let instructions = bootstrap_instructions(&captain, &project);

    assert!(instructions.contains(runtime_repo_root));
    assert!(!instructions.contains(canonical_repo_root));
    assert_eq!(project.repo_root, canonical_repo_root);
}

#[test]
fn attach_captain_refuses_read_only_and_preserves_existing_control_capability() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let mut ctx = test_ctx("control-secret").with_apply_sink(Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    }));
    ctx.addr = "127.0.0.1:4242".into();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-attach".into(),
            name: "Attach Project".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "attach-project".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    ctx.tab_registry().replace(vec![TabRecord {
        id: "attach-work".into(),
        name: "Attach Work".into(),
        tile_ids: Vec::new(),
    }]);
    let read_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({ "cwd": "/tmp", "capability": "read", "tabId": "attach-work" }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let error = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": read_id,
            "projectId": "project-attach",
            "assignment": "Own stability",
            "provider": "codex",
            "testSkipPowderHealth": true,
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("read-only; refusing silent elevation"),
        "got: {error}"
    );

    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let control_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "capability": "read",
            "tabId": "attach-work",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&control_id, "codex").unwrap();
    // Convert this ordinary read-only spawn into a compatibility fixture for
    // a terminal created before Package 0. New terminals never receive this
    // rotating global credential from their spawn request.
    tmux::set_session_environment(&tmux_target(&control_id), "T_HUB_CONTROL_TOKEN", &ctx.token)
        .unwrap();
    let attached = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": control_id,
            "projectId": "project-attach",
            "assignment": "Own stability",
            "provider": "codex",
            "testSkipPowderHealth": true,
        }),
    )
    .unwrap();
    assert_eq!(attached["accepted"], "attach_captain");
    assert_eq!(attached["capabilityPreserved"], "control");
    assert_eq!(attached["captain"]["provider"], "codex");
    assert!(attached["captain"].get("providerSessionId").is_none());
    assert!(attached["captain"].get("claudeUuid").is_none());
    let attached_tabs = ctx.tabs.snapshot_full();
    assert!(!attached_tabs
        .tabs
        .iter()
        .find(|tab| tab.id == "attach-work")
        .unwrap()
        .tile_ids
        .contains(&control_id));
    assert_eq!(
        attached_tabs
            .tabs
            .iter()
            .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
            .unwrap()
            .tile_ids
            .iter()
            .filter(|tile| *tile == &control_id)
            .count(),
        1
    );
    let unchanged_report = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({
            "baseSeq": attached_tabs.seq,
            "tabs": attached_tabs.tabs,
            "activeTabId": attached_tabs.active_tab_id
        }),
    )
    .unwrap();
    assert!(unchanged_report.get("reported").is_some());

    let checkpoint = dispatch(
        &ctx,
        "captain_checkpoint",
        &json!({
            "captainSessionId": control_id,
            "conversationId": "thread-attach",
            "resumePoint": "Continue verification",
        }),
    )
    .unwrap();
    assert_eq!(
        checkpoint["captain"]["resumePoint"],
        "Continue verification"
    );

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": read_id })).unwrap();
    dispatch(&ctx, "close_terminal", &json!({ "sessionId": control_id })).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
}

#[test]
fn attach_captain_binding_failure_restores_placement_and_retry_is_durable() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("attach-relocation-rollback");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-attach-rollback".into(),
            name: "Attach Rollback".into(),
            repo_root: "/tmp/attach-rollback".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "attach-rollback".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "attach-work".into(),
        name: "Attach Work".into(),
        tile_ids: Vec::new(),
    }]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("attach-relocation-rollback")
        .with_apply_sink(sink.clone())
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    ctx.addr = "127.0.0.1:4242".into();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "capability": "read",
            "tabId": "attach-work",
            "startupCommand": harness_command
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&captain_id, "codex").unwrap();
    // Convert the read-only spawn into a legacy attach fixture without
    // restoring global-token persistence for newly spawned terminals.
    tmux::set_session_environment(&tmux_target(&captain_id), "T_HUB_CONTROL_TOKEN", &ctx.token)
        .unwrap();
    sink.calls.lock().unwrap().clear();
    captains.fail_next_persist("attach bind persistence failure");

    let error = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": captain_id,
            "projectId": "project-attach-rollback",
            "assignment": "Own rollback",
            "provider": "codex",
            "testSkipPowderHealth": true
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("attach bind persistence failure"),
        "got: {error}"
    );
    assert!(captains.snapshot().captains.is_empty());
    let rolled_back = tabs.snapshot_full();
    assert_eq!(
        rolled_back
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );
    assert!(rolled_back
        .tabs
        .iter()
        .find(|tab| tab.id == "attach-work")
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    let failed_projection = serde_json::to_string(&*sink.calls.lock().unwrap()).unwrap();
    assert!(
        !failed_projection.contains(&captain_id),
        "failed attachment must never project a ghost Captain or placement: {failed_projection}"
    );

    let retry = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": captain_id,
            "projectId": "project-attach-rollback",
            "assignment": "Own rollback",
            "provider": "codex",
            "testSkipPowderHealth": true
        }),
    )
    .unwrap();
    assert_eq!(retry["accepted"], "attach_captain");
    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.captains.len(), 1);
    assert_eq!(
        restored.captains[0].project_id.as_deref(),
        Some("project-attach-rollback")
    );
    let final_tabs = tabs.snapshot_full();
    assert_eq!(
        final_tabs
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );
    assert!(final_tabs
        .tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .contains(&captain_id));

    dispatch(
        &ctx,
        "release_captain",
        &json!({"captainSessionId": captain_id}),
    )
    .unwrap();
    dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": captain_id, "tabId": "attach-work"}),
    )
    .unwrap();
    let before_bootstrap_failure = captains.snapshot();
    sink.calls.lock().unwrap().clear();
    let bootstrap_error = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": captain_id,
            "projectId": "project-attach-rollback",
            "assignment": "Own rollback",
            "provider": "codex",
            "testSkipPowderHealth": true,
            "testFailBootstrapDelivery": true
        }),
    )
    .unwrap_err();
    assert!(bootstrap_error.contains("injected bootstrap delivery failure"));
    assert_eq!(
        captains.snapshot().captains,
        before_bootstrap_failure.captains
    );
    assert!(tabs
        .snapshot()
        .iter()
        .find(|tab| tab.id == "attach-work")
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    assert!(
        sink.calls.lock().unwrap().is_empty(),
        "bootstrap rollback must occur before any Captain or Workspace projection"
    );

    dispatch(&ctx, "close_terminal", &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}
