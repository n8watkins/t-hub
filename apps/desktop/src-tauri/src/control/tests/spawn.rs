use super::*;

#[test]
fn spawn_terminal_without_sink_refuses_untracked_session() {
    // No apply sink (headless): there is no UI to adopt the tile, so spawn is
    // refused rather than creating an untracked tmux session (#17).
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap_err();
    assert!(err.contains("no UI"), "got: {err}");
}

#[test]
fn spawn_terminal_with_sink_spawns_places_and_returns_id() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // Headless-org: with a UI sink wired, the SERVER spawns the real session,
    // resolves `tabName` against the authoritative registry (minting a hidden
    // tab without switching the active one), places the tile there, returns
    // the real id synchronously, and forwards id + registry snapshot.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "name": "logs", "tabName": "hidden-ops"}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    assert_eq!(v["audited"], true);
    let id = v["id"]
        .as_str()
        .expect("real id returned synchronously")
        .to_string();
    assert_eq!(v["placed"], true);
    let tab_id = v["tabId"].as_str().unwrap().to_string();
    assert_ne!(
        tab_id, "tab-1",
        "a NEW hidden tab is minted for the new name"
    );

    // The registry (authoritative) holds the placement, and the active tab
    // was NOT touched (no focus steal).
    let snap = ctx.tab_registry().snapshot_full();
    let tab = snap
        .tabs
        .iter()
        .find(|t| t.id == tab_id)
        .expect("tab minted");
    assert_eq!(tab.name, "hidden-ops");
    assert_eq!(tab.tile_ids, vec![id.clone()]);
    assert_eq!(snap.active_tab_id.as_deref(), Some("tab-1"));

    // The forward carries the id + snapshot for the UI to render from.
    {
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "spawn_terminal");
        assert_eq!(calls[0].1["id"], json!(id));
        assert_eq!(calls[0].1["cwd"], "/tmp");
        assert_eq!(calls[0].1["name"], "logs");
        assert_eq!(calls[0].1["tabId"], json!(tab_id));
        assert!(calls[0].1["sync"]["seq"].as_u64().is_some());
    }
    // Reap the real session this spawned.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[cfg(unix)]
#[test]
fn spawn_terminal_converts_wsl_unc_for_tmux_but_preserves_the_public_cwd() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let runtime_dir = tempfile::tempdir().unwrap();
    let runtime_cwd = runtime_dir.path().canonicalize().unwrap();
    let runtime_cwd = runtime_cwd.to_str().unwrap();
    assert!(runtime_cwd.starts_with('/'));
    let canonical_cwd = format!(
        "\\\\?\\UNC\\wsl.localhost\\Ubuntu-24.04{}",
        runtime_cwd.replace('/', "\\")
    );
    let result = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": &canonical_cwd, "startupCommand": "sleep 60"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap().to_string();
    let target = tmux_target(&id);
    let pane_cwd = std::process::Command::new("tmux")
        .args([
            "-L",
            tmux::socket(),
            "display-message",
            "-p",
            "-t",
            &target,
            "#{pane_current_path}",
        ])
        .output()
        .unwrap();

    assert!(pane_cwd.status.success());
    assert_eq!(
        String::from_utf8_lossy(&pane_cwd.stdout).trim(),
        runtime_cwd
    );
    assert_eq!(result["cwd"], canonical_cwd);
    assert_eq!(sink.calls.lock().unwrap()[0].1["cwd"], canonical_cwd);

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn spawn_terminal_forwards_the_startup_command() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // T-B: the socket spawn carries the webview presets' `startupCommand`
    // (the resume flow's `claude --resume <id>` in production; a harmless
    // echo here - headless-org spawns the REAL session server-side now, so
    // the command actually runs). The forward must carry it verbatim.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "startupCommand": "echo resume-proof"}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    assert_eq!(v["startupCommand"], "echo resume-proof");
    let first_id = v["id"].as_str().unwrap().to_string();

    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls[0].0, "spawn_terminal");
    assert_eq!(calls[0].1["startupCommand"], "echo resume-proof");
    // The snake_case alias parses too (loose-args convention).
    drop(calls);
    let v2 = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "startup_command": "echo alias-proof"}),
    )
    .unwrap();
    assert_eq!(
        sink.calls.lock().unwrap()[1].1["startupCommand"],
        "echo alias-proof"
    );
    // Reap the real sessions these spawned.
    for id in [first_id.as_str(), v2["id"].as_str().unwrap()] {
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }
}

#[test]
fn start_agent_rejects_dependency_result_missing_from_source_baseline() {
    let ctx = test_ctx("dependency-ancestry");
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let initial_commit = exact_head(&worktree);
    std::fs::write(worktree.join("source.txt"), "dependent source\n").unwrap();
    let worktree_path = worktree.to_string_lossy().to_string();
    let run = |cwd: &str, args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(cwd, args).unwrap();
        assert!(ok, "git {args:?} failed: {stderr}");
        stdout
    };
    run(&worktree_path, &["add", "source.txt"]);
    run(
        &worktree_path,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "dependent source",
        ],
    );
    let source_commit = exact_head(&worktree);

    let repo_path = repo_root.to_string_lossy().to_string();
    std::fs::write(repo_root.join("dependency.txt"), "dependency result\n").unwrap();
    run(&repo_path, &["add", "dependency.txt"]);
    run(
        &repo_path,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "divergent dependency result",
        ],
    );
    let dependency_result = exact_head(&repo_root);

    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-dependency".into(),
            name: "Dependency Project".into(),
            repo_root: repo_path,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-dependency", Some("captain-dependency"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-dependency",
            "project-dependency",
            "Assignment",
            "codex",
        )
        .unwrap();
    let (lane_claim, dispatch_capacity) =
        test_dispatch_evidence("dependency-lane", "dependency-agent");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "dependency-agent".into(),
            captain_session_id: "captain-dependency".into(),
            project_id: "project-dependency".into(),
            assignment: "Build dependency".into(),
            directory: repo_root.to_string_lossy().to_string(),
            worktree_path: None,
            branch: Some("main".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Exited,
            work_stage: crate::agent_session::WorkStage::Complete,
            delivery: Some(completed_delivery(&initial_commit, &dependency_result)),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();

    let mut ancestor_snapshot = ctx.captains.snapshot();
    ancestor_snapshot.agent_sessions[0].delivery =
        Some(completed_delivery(&initial_commit, &initial_commit));
    assert_eq!(
        validate_dependency_result_ancestry(
            "test_dependency_ancestry",
            &ancestor_snapshot,
            "project-dependency",
            &BTreeSet::from(["dependency-lane".to_string()]),
            &worktree_path,
            &source_commit,
        )
        .unwrap(),
        BTreeSet::from(["dependency-lane".to_string()])
    );

    let preflight_error = dispatch(
        &ctx,
        "dispatch_preflight",
        &json!({
            "projectId": "project-dependency",
            "sourceCommit": source_commit,
            "requestedLanes": [{
                "laneId": "dependent-lane",
                "ownerId": "dependent-owner",
                "dependencies": ["dependency-lane"],
                "mutableFiles": ["src/dependent.rs"],
                "mutableSchemas": [],
                "mutableInterfaces": []
            }],
            "integrationContracts": []
        }),
    )
    .unwrap_err();
    assert!(
        preflight_error.contains("dispatch_preflight: dependency 'dependency-lane'"),
        "got: {preflight_error}"
    );
    assert!(
        preflight_error.contains("not present in sourceCommit"),
        "got: {preflight_error}"
    );

    let error = start_agent(
        &ctx,
        &json!({
            "requestId": "dependency-ancestry-rejected",
            "captainSessionId": "captain-dependency",
            "assignment": "Build dependent lane",
            "directory": worktree_path,
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "dependent-lane",
            "dependencies": ["dependency-lane"],
            "mutableFiles": ["src/dependent.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": []
        }),
        None,
        true,
    )
    .unwrap_err();
    assert!(error.contains("dependency-lane"), "got: {error}");
    assert!(
        error.contains("not present in sourceCommit"),
        "got: {error}"
    );
    assert_eq!(ctx.captains.snapshot().agent_sessions.len(), 1);
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn concurrent_start_agent_admission_cannot_double_claim_a_checkout() {
    if !tmux_process_tests_available() {
        eprintln!(
                "concurrent_start_agent_admission_cannot_double_claim_a_checkout: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let governor = SpawnGovernor::new(8, 20.0, 8.0).with_reservation_policy(
        crate::governor::ReservationPolicy {
            cortana: 0,
            fleet_admins: 0,
            ship_admins_per_active_captain: 0,
            recovery: 0,
        },
    );
    let ctx = test_ctx("atomic-start-agent")
        .with_governor(Arc::new(governor))
        .with_provider_capacity(|| Ok(1))
        .with_provider_live_sessions(|_| Ok(0))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let checkout = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-atomic-start".into(),
            name: "Atomic Start Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-atomic-start", Some("captain-atomic-start"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-atomic-start",
            "project-atomic-start",
            "Assignment",
            "codex",
        )
        .unwrap();

    let args = json!({
        "requestId": "atomic-start-first",
        "captainSessionId": "captain-atomic-start",
        "assignment": "Own the shared checkout",
        "directory": checkout,
        "harness": "codex",
        "sourceCommit": source_commit,
        "visibleProductBug": false,
        "laneId": "atomic-lane-first",
        "dependencies": [],
        "mutableFiles": ["src/shared.rs"],
        "mutableSchemas": [],
        "mutableInterfaces": [],
        "integrationContracts": []
    });
    let (reached, wait_for_admission) = std::sync::mpsc::sync_channel(1);
    let (resume, continue_start) = std::sync::mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "start_agent_admitted",
        reached,
        resume: continue_start,
    }));
    let first_ctx = ctx.clone();
    let first_args = args.clone();
    let first = std::thread::spawn(move || start_agent(&first_ctx, &first_args, None, true));
    assert_eq!(
        wait_for_admission
            .recv_timeout(Duration::from_secs(2))
            .expect("first start did not reach durable admission"),
        "start_agent_admitted"
    );
    assert!(ctx.dispatch_admission.try_lock().is_err());
    assert_eq!(ctx.captains.snapshot().agent_sessions.len(), 1);
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 8).unwrap();
    assert_eq!(
        runtime.provider_live_sessions, 1,
        "the paused durable start must occupy the sole provider slot before tmux exists"
    );

    let mut second_args = args;
    second_args["requestId"] = json!("atomic-start-second");
    second_args["laneId"] = json!("atomic-lane-second");
    let second_ctx = ctx.clone();
    let (attempted_tx, attempted_rx) = std::sync::mpsc::sync_channel(1);
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let second = std::thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let result = start_agent(&second_ctx, &second_args, None, true);
        finished_tx.send(result).unwrap();
    });
    attempted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(150)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));

    resume.send(()).unwrap();
    let first_result = first.join().unwrap().unwrap();
    let second_error = finished_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap_err();
    second.join().unwrap();
    assert!(
        second_error.contains("already owned"),
        "got: {second_error}"
    );
    assert_eq!(ctx.captains.snapshot().agent_sessions.len(), 1);
    assert_eq!(first_result["sourceCommit"], source_commit);
    assert_eq!(first_result["sourceBaseline"], source_commit);
    assert_eq!(first_result["admissionPurpose"], "ordinary");
    let agent_session_id = first_result["agentSessionId"].as_str().unwrap();
    reap_test_tmux_session(&tmux_target(agent_session_id)).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_persists_before_a_launch_failure_and_records_unavailable() {
    let ctx = test_ctx("secret");
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-start".into(),
            name: "Start Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-start", Some("captain-start"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("captain-start", "project-start", "Assignment", "codex")
        .unwrap();

    let error = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "start-agent-test",
            "captainSessionId": "captain-start",
            "assignment": "Do one bounded change",
            "directory": repo.clone(),
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "lane-start-failure",
            "dependencies": [],
            "mutableFiles": ["src/start-failure.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "admissionPurpose": "fleet-admin"
        }),
    )
    .unwrap_err();
    assert!(error.contains("no UI is connected"), "got: {error}");
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .next()
        .expect("agent record persisted before launch");
    assert_eq!(agent.runtime_state, RuntimeState::Unavailable);
    assert_eq!(agent.work_stage, crate::agent_session::WorkStage::Stopped);
    assert_eq!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
    );
    let events = ctx.captains.snapshot().agent_events;
    assert_eq!(
        events.last().map(|event| event.kind.as_str()),
        Some("unavailable")
    );
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_uses_matching_reserved_capacity_in_project_preflight() {
    let ctx = test_ctx("reserved-start")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()));
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-reserved-start".into(),
            name: "Reserved Start Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test(
            "captain-reserved-start",
            Some("captain-reserved-start"),
            vec![],
        )
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-reserved-start",
            "project-reserved-start",
            "Assignment",
            "codex",
        )
        .unwrap();

    let ordinary_refusal = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(ordinary_refusal.code, "reserved-capacity");

    let error = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "reserved-start-agent-test",
            "captainSessionId": "captain-reserved-start",
            "assignment": "Start the standing Fleet Admin",
            "directory": repo,
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "reserved-fleet-admin-lane",
            "dependencies": [],
            "mutableFiles": ["src/reserved-admin.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "admissionPurpose": "fleet-admin"
        }),
    )
    .unwrap_err();
    assert!(error.contains("no UI is connected"), "got: {error}");
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .next()
        .expect("Fleet Admin persisted after reserved admission");
    assert_eq!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
    );
    assert_eq!(agent.runtime_state, RuntimeState::Unavailable);
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_records_running_after_launch_without_inventing_provider_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("start-agent-success").with_apply_sink(sink);
    ctx.addr = "127.0.0.1:1".into();
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-start-success".into(),
            name: "Start Success Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test(
            "captain-start-success",
            Some("captain-start-success"),
            vec![],
        )
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-start-success",
            "project-start-success",
            "Assignment",
            "codex",
        )
        .unwrap();

    let response = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "start-agent-success",
            "captainSessionId": "captain-start-success",
            "assignment": "Do one bounded change",
            "directory": repo.clone(),
            "harness": "codex",
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "lane-start-success",
            "dependencies": [],
            "mutableFiles": ["src/start-success.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": []
        }),
    )
    .unwrap();
    let agent_session_id = response["agentSessionId"].as_str().unwrap();
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .expect("agent record persisted after launch");
    assert_eq!(agent.runtime_state, RuntimeState::Running);
    assert_eq!(agent.work_stage, crate::agent_session::WorkStage::Assigned);
    assert!(agent.provider_conversation_id.is_none());
    assert_eq!(response["runtimeState"], "running");
    assert!(response.get("providerConversationId").is_none());
    assert_eq!(
        tmux::session_environment(&tmux_target(agent_session_id), "T_HUB_CONTROL_TOKEN")
            .unwrap()
            .as_deref(),
        Some(""),
        "ordinary implementation lanes must not persist rotating credentials"
    );
    let event = ctx
        .captains
        .snapshot()
        .agent_events
        .into_iter()
        .find(|event| event.agent_session_id == agent_session_id)
        .expect("started lifecycle event");
    assert_eq!(event.kind, "started");
    assert_eq!(event.runtime_state, Some(RuntimeState::Running));

    reap_test_tmux_session(&tmux_target(agent_session_id)).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn authorized_admin_start_agent_receives_control_capability() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("start-fleet-admin").with_apply_sink(sink);
    ctx.addr = "127.0.0.1:1".into();
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-fleet-admin".into(),
            name: "Fleet Admin Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-fleet-admin", Some("captain-fleet-admin"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-fleet-admin",
            "project-fleet-admin",
            "Assignment",
            "codex",
        )
        .unwrap();

    let response = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "start-fleet-admin",
            "captainSessionId": "captain-fleet-admin",
            "assignment": "Perform delegated fleet administration",
            "directory": repo,
            "harness": "codex",
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "lane-fleet-admin",
            "dependencies": [],
            "mutableFiles": [],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "admissionPurpose": "fleet-admin"
        }),
    )
    .unwrap();
    let agent_session_id = response["agentSessionId"].as_str().unwrap();
    assert_eq!(
        tmux::session_environment(&tmux_target(agent_session_id), "T_HUB_CONTROL_TOKEN")
            .unwrap()
            .as_deref(),
        Some(""),
        "an administrative lane must reacquire scoped authority from durable identity"
    );
    assert!(
        tmux::session_environment(&tmux_target(agent_session_id), "GH_CONFIG_DIR")
            .unwrap()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        tmux::session_environment(&tmux_target(agent_session_id), "NPM_TOKEN").unwrap(),
        Some(String::new()),
        "administrative control capability must not restore ambient credentials"
    );
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .unwrap();
    assert_eq!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
    );

    reap_test_tmux_session(&tmux_target(agent_session_id)).unwrap();
    std::fs::remove_dir_all(base).ok();
}
