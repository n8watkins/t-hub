use super::*;

#[test]
fn remove_worktree_requires_args() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "remove_worktree", &json!({"worktreePath": "/x"})).unwrap_err();
    assert!(err.contains("repoRoot"), "got: {err}");
    let err = dispatch(&ctx, "remove_worktree", &json!({"repoRoot": "/r"})).unwrap_err();
    assert!(err.contains("worktreePath"), "got: {err}");
}

#[test]
fn remove_worktree_without_sink_fails_closed_before_mutation() {
    let ctx = test_ctx("t");
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": "/r", "worktreePath": "/r/wt"}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
}

#[test]
fn remove_worktree_with_sink_fails_before_forwarding() {
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": "/r", "worktreePath": "/r/wt", "force": true}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
    let calls = sink.calls.lock().unwrap();
    assert!(
        calls.is_empty(),
        "no UI mutation may be forwarded: {calls:?}"
    );
}

#[test]
fn owned_empty_tab_rollback_preserves_shared_tabs() {
    let tabs = TabRegistry::new();
    tabs.insert_tab("owned", "Owned");
    tabs.rollback_owned_empty_tab("owned").unwrap();
    assert!(!tabs.has_tab("owned"));

    tabs.insert_tab("shared", "Shared");
    tabs.move_tile("live", "shared").unwrap();
    let err = tabs.rollback_owned_empty_tab("shared").unwrap_err();
    assert!(err.contains("gained a tile"), "got: {err}");
    assert!(tabs.has_tab("shared"));
}

#[test]
fn owned_create_state_rollback_removes_worktree_and_new_tab() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let ctx = test_ctx("t");
    ctx.tabs.insert_tab("owned", "Owned");

    rollback_created_worktree_state(
        &ctx,
        repo.to_str().unwrap(),
        worktree.to_str().unwrap(),
        "owned",
        true,
    )
    .unwrap();

    assert!(!worktree.exists());
    assert!(!ctx.tabs.has_tab("owned"));
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn ambiguous_spawn_failure_reports_preserved_worktree() {
    let error = ambiguous_spawn_rollback_error(
        "spawn outcome unknown",
        "identity store unavailable",
        Ok(()),
    );
    assert!(error.contains("terminal cleanup was not confirmed"));
    assert!(error.contains("worktree was preserved"));
    assert!(!error.contains("worktree was rolled back"));
}

#[test]
fn create_worktree_requires_args() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "create_worktree", &json!({"worktreePath": "/x"})).unwrap_err();
    assert!(err.contains("repoRoot"), "got: {err}");
    let err = dispatch(&ctx, "create_worktree", &json!({"repoRoot": "/r"})).unwrap_err();
    assert!(err.contains("worktreePath"), "got: {err}");
}

#[cfg(not(windows))]
fn checkout_test_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

#[cfg(not(windows))]
fn extended_wsl_unc(path: &std::path::Path, distro: &str) -> String {
    format!(
        "\\\\?\\UNC\\wsl.localhost\\{distro}{}",
        path.to_string_lossy().replace('/', "\\")
    )
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_accepts_a_wsl_worktree_for_an_extended_unc_project() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let distro = checkout_test_distro();
    let durable_root = extended_wsl_unc(&repo, &distro);
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-wsl-worktree".into(),
        name: "WSL Worktree".into(),
        repo_root: durable_root.clone(),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let checkout = validate_crew_checkout(&project, Some(worktree.to_string_lossy().into_owned()))
        .expect("the WSL checkout must match the extended-UNC Project root");

    assert_eq!(
        checkout,
        std::fs::canonicalize(&worktree).unwrap().to_string_lossy()
    );
    assert_eq!(project.repo_root, durable_root);
    std::fs::remove_dir_all(base).ok();
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_accepts_a_same_distro_unc_worktree() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let distro = checkout_test_distro();
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-same-distro".into(),
        name: "Same Distro".into(),
        repo_root: extended_wsl_unc(&repo, &distro),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let checkout = validate_crew_checkout(&project, Some(extended_wsl_unc(&worktree, &distro)))
        .expect("an explicit checkout in the configured distro must remain valid");

    assert_eq!(
        checkout,
        std::fs::canonicalize(&worktree).unwrap().to_string_lossy()
    );
    std::fs::remove_dir_all(base).ok();
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_rejects_foreign_distro_unc_paths_with_the_same_tail() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let configured = checkout_test_distro();
    let foreign = if configured.eq_ignore_ascii_case("Debian") {
        "Ubuntu-24.04"
    } else {
        "Debian"
    };
    let mut project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-foreign-distro".into(),
        name: "Foreign Distro".into(),
        repo_root: extended_wsl_unc(&repo, &configured),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let requested_error =
        validate_crew_checkout(&project, Some(extended_wsl_unc(&worktree, foreign)))
            .expect_err("the same path tail in a foreign distro must not be remapped");
    assert!(requested_error.contains("requested checkout"));
    assert!(requested_error.contains(foreign));
    assert!(requested_error.contains(&configured));

    project.repo_root = extended_wsl_unc(&repo, foreign);
    let project_error =
        validate_crew_checkout(&project, Some(worktree.to_string_lossy().into_owned()))
            .expect_err("a durable Project root in a foreign distro must fail closed");
    assert!(project_error.contains("Project root"));
    assert!(project_error.contains(foreign));
    assert!(project_error.contains(&configured));

    std::fs::remove_dir_all(base).ok();
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_rejects_unregistered_directories_and_foreign_worktrees() {
    let (base, repo, _worktree) = scratch_repo_with_worktree();
    let (foreign_base, _foreign_repo, foreign_worktree) = scratch_repo_with_worktree();
    let ordinary = base.join("ordinary-checkout");
    std::fs::create_dir(&ordinary).expect("ordinary checkout fixture");
    let distro = checkout_test_distro();
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-wsl-rejections".into(),
        name: "WSL Rejections".into(),
        repo_root: extended_wsl_unc(&repo, &distro),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    for rejected in [&ordinary, &foreign_worktree] {
        let error = validate_crew_checkout(&project, Some(rejected.to_string_lossy().into_owned()))
            .expect_err("only worktrees belonging to the Project may be dispatched");
        assert!(
            error.contains("is not a worktree of project"),
            "got: {error}"
        );
    }

    std::fs::remove_dir_all(base).ok();
    std::fs::remove_dir_all(foreign_base).ok();
}

fn scratch_product_repo_with_worktree() -> (std::path::PathBuf, String, String) {
    let base = std::env::temp_dir().join(format!(
        "t-hub-product-tb-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let repo_host = base.join("repo");
    let worktree_host = base.join("wt");
    std::fs::create_dir_all(&repo_host).expect("mkdir repo");

    let repo = test_product_path(&repo_host);
    let worktree = test_product_path(&worktree_host);
    let run = |args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(&repo, args).expect("git spawns");
        assert!(
            ok,
            "git {args:?} failed: {}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        );
    };

    run(&["init", "-q"]);
    std::fs::write(repo_host.join("a.txt"), "hi").expect("seed file");
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    git::worktree_add(&repo, &worktree, None).expect("worktree add succeeds");
    assert!(worktree_host.exists(), "worktree dir created");
    (base, repo, worktree)
}

#[cfg(not(windows))]
fn test_product_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(windows)]
fn test_product_path(path: &std::path::Path) -> String {
    let native = path.to_string_lossy().replace('\\', "/");
    let bytes = native.as_bytes();
    assert!(
        bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/',
        "expected an absolute drive path, got {native:?}"
    );
    format!(
        "/mnt/{}/{}",
        (bytes[0] as char).to_ascii_lowercase(),
        &native[3..]
    )
}

#[test]
fn remove_worktree_with_subscribers_fails_before_broadcast_or_git() {
    let (base, repo, wt) = scratch_product_repo_with_worktree();

    for force in [false, true] {
        let err = git::worktree_remove(&repo, &wt, force).unwrap_err();
        assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
        assert!(
            base.join("wt").exists(),
            "force={force} must preserve the worktree"
        );
    }

    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    let mut reader = subscribe_test_reader(&fanout);
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": repo, "worktreePath": wt}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
    assert_no_event(&mut reader);
    assert!(
        base.join("wt").exists(),
        "the worktree directory must remain intact"
    );
    let listed_paths = git::worktree_list(&repo)
        .expect("git worktree list succeeds")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert!(
        listed_paths.contains(&wt),
        "the worktree registration must remain intact: expected {wt:?} in {listed_paths:?}"
    );

    git::rollback_created_worktree(&repo, &wt)
        .expect("transaction-owned rollback remains available");
    assert!(
        !base.join("wt").exists(),
        "private rollback must remove its owned worktree"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn create_worktree_runs_the_startup_command_in_the_worktree_terminal() {
    // audit MED (provisioning gap): create_worktree now carries a
    // `startupCommand` plumbed through the SAME pane_command / -ilc exec path
    // spawn_terminal uses, so a worktree crew boots into its command instead of
    // a bare shell. This proves it EXECUTES end-to-end: the startupCommand
    // touches a sentinel file, and we poll for it. BYPASS-WOULD-FAIL: pass
    // `None` for the pane again (the old bare-shell spawn) and the sentinel is
    // never created -> the poll times out RED.
    let (base, repo, _wt) = scratch_repo_with_worktree();
    let new_wt = base.join("wt-startup");
    let sentinel = base.join("startup-ran.marker");
    let startup = format!("touch {}", sentinel.to_str().unwrap());

    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let v = dispatch(
        &ctx,
        "create_worktree",
        &json!({
            "repoRoot": repo.to_str().unwrap(),
            "worktreePath": new_wt.to_str().unwrap(),
            "startupCommand": startup,
        }),
    )
    .unwrap();
    assert_eq!(v["accepted"], "create_worktree");
    // The response + the UI forward both carry the command verbatim.
    assert_eq!(v["startupCommand"], json!(startup));
    let terminal_id = v["terminalId"].as_str().expect("a terminal was spawned");
    {
        let calls = sink.calls.lock().unwrap();
        let fwd = calls
            .iter()
            .find(|(cmd, _)| cmd == "add_worktree_workspace")
            .expect("the worktree forward was delivered");
        assert_eq!(fwd.1["startupCommand"], json!(startup));
    }

    // Poll for the sentinel: proof the -ilc pane wrap actually ran the command
    // (the interactive login shell can take a moment to source rc + exec).
    let mut ran = false;
    for _ in 0..100 {
        if sentinel.exists() {
            ran = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Reap the real session before asserting, so a failure never leaks a tmux
    // session or the scratch dir.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": terminal_id})).ok();
    std::fs::remove_dir_all(&base).ok();
    assert!(
        ran,
        "the worktree terminal must have run the startupCommand"
    );
}

#[test]
fn list_worktrees_lists_main_then_linked() {
    let (base, repo, wt) = scratch_repo_with_worktree();
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "list_worktrees",
        &json!({"cwd": repo.to_str().unwrap()}),
    )
    .unwrap();
    let list = v["worktrees"].as_array().expect("worktrees array");
    assert_eq!(list.len(), 2, "main + linked: {list:?}");
    assert_eq!(list[0]["isLinked"], false);
    assert_eq!(list[1]["isLinked"], true);
    assert_eq!(list[1]["path"], json!(wt.to_str().unwrap()));
    // The IPC-twin alias resolves to the same handler.
    let v2 = dispatch(
        &ctx,
        "git_worktree_list",
        &json!({"cwd": repo.to_str().unwrap()}),
    )
    .unwrap();
    assert_eq!(v2["worktrees"], v["worktrees"]);
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn reprobe_reaped_create_worktree_resolves_against_reality() {
    // M1 full fix. A create_worktree whose InFlight reservation was reaped is
    // retried with the same requestId; before re-applying we RE-PROBE reality.
    let (base, repo, wt) = scratch_repo_with_worktree();
    let ctx = test_ctx("t");

    // The worktree EXISTS on disk (the original DID land): the re-probe must
    // resolve to a success outcome tagged reprobedAfterReap, NOT None (which
    // would let dispatch re-run git worktree add and duplicate/error).
    let args = json!({
        "repoRoot": repo.to_str().unwrap(),
        "worktreePath": wt.to_str().unwrap(),
    });
    let outcome = reprobe_reaped_request(&ctx, "create_worktree", &args)
        .expect("existing worktree must resolve against reality");
    let v = outcome.expect("resolved outcome is Ok");
    assert_eq!(v["accepted"], "create_worktree");
    assert_eq!(v["alreadyCreated"], true);
    assert_eq!(v["reprobedAfterReap"], true);

    // A worktree path that does NOT exist ⇒ None: the original truly died, so
    // dispatch proceeds to a fresh (re-checked) apply.
    let missing = json!({
        "repoRoot": repo.to_str().unwrap(),
        "worktreePath": base.join("never-created").to_str().unwrap(),
    });
    assert!(
        reprobe_reaped_request(&ctx, "create_worktree", &missing).is_none(),
        "an absent worktree must NOT resolve - it should re-apply fresh"
    );

    // spawn_terminal has a SERVER-minted id: nothing in args to probe by ⇒ None.
    assert!(
        reprobe_reaped_request(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).is_none(),
        "spawn_terminal has no probe-able artifact in its args"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn list_worktrees_requires_cwd_and_is_empty_outside_a_repo() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "list_worktrees", &json!({})).unwrap_err();
    assert!(err.contains("cwd"), "got: {err}");
    // Best-effort like the IPC twin: a non-repo dir yields an empty list.
    let v = dispatch(&ctx, "list_worktrees", &json!({"cwd": "/"})).unwrap();
    assert_eq!(v["worktrees"], json!([]));
}

#[test]
fn remote_worktree_ops_are_gated_to_the_allowlist() {
    // A REMOTE peer (peer_is_loopback=false) with no T_HUB_REMOTE_FILE_ROOTS is
    // refused BEFORE any git runs (#27) — the scope gate fires ahead of
    // git::worktree_add and the UI forward. (test_ctx defaults to loopback, so
    // the existing create/remove tests above keep exercising the unrestricted
    // local path.)
    let mut ctx = test_ctx("t");
    ctx.peer_is_loopback = false;
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/home/x/proj".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "remote-none".into(),
            name: "Remote none".into(),
            repo_root: "/home/x/proj".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let before = ctx.captains.snapshot();
    for cmd in ["create_worktree", "remove_worktree", "list_worktrees"] {
        let err = dispatch(
            &ctx,
            cmd,
            &json!({"repoRoot": "/home/x/proj", "worktreePath": "/home/x/proj-wt/feature"}),
        )
        .unwrap_err();
        assert!(
            err.contains("disabled"),
            "{cmd} should be gated for a remote peer; got: {err}"
        );
        assert!(
            !err.contains("git_required"),
            "{cmd} disclosed registered capability before remote path authorization: {err}"
        );
    }
    assert_eq!(ctx.captains.snapshot().seq, before.seq);
    // git_info probes git at a peer-controlled cwd — same allowlist gate.
    let err = dispatch(&ctx, "git_info", &json!({"path": "/home/x/whatever"})).unwrap_err();
    assert!(
        err.contains("disabled"),
        "git_info should be gated; got: {err}"
    );
}

#[test]
fn every_registered_git_only_gate_rejects_non_git_before_operation() {
    let ctx = test_ctx("git-gate-matrix");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/non-git-gate".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "non-git-project".into(),
            name: "Non-Git Project".into(),
            repo_root: "/tmp/non-git-gate".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    for operation in [
        "dispatch_preflight",
        "baseline",
        "integration",
        "delivery",
        "capacity",
        "create_worktree",
        "remove_worktree",
        "list_worktrees",
        "admin_worktree",
    ] {
        let error =
            require_registered_git_capability(&ctx, operation, "/tmp/non-git-gate").unwrap_err();
        assert_eq!(
                error,
                format!(
                    "git_required code=git_required operation={operation} capability=git action=initialize_git"
                )
            );
    }
}

fn assert_native_git_required(response: ControlResponse, operation: &str) {
    assert!(!response.ok, "unexpected success: {response:?}");
    assert_eq!(response.error_kind.as_deref(), Some("git_required"));
    assert!(!response.retryable);
    assert_eq!(
        response.error_details,
        Some(json!({
            "code": "git_required",
            "operation": operation,
            "capability": "git",
            "action": "initialize_git",
        }))
    );
    assert!(response
        .error
        .as_deref()
        .is_some_and(|message| message.contains("initialize_git")));
}

#[test]
fn explicit_none_dispatcher_response_matches_cli_mcp_parity_fixture() {
    let ctx = test_ctx("dispatcher-parity-fixture");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/dispatcher-parity-fixture".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "dispatcher-parity-fixture".into(),
            name: "Dispatcher parity fixture".into(),
            repo_root: "/tmp/dispatcher-parity-fixture".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let response = dispatch_authenticated(
        &ctx,
        req(
            "dispatcher-parity-fixture",
            "dispatch_preflight",
            json!({
                "projectId": "dispatcher-parity-fixture",
                "sourceCommit": "1111111111111111111111111111111111111111",
                "requestedLanes": [],
                "integrationContracts": []
            }),
        ),
    );
    let actual = serde_json::to_value(response).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/explicit-none-dispatch-preflight-response.json"
    ))
    .unwrap();
    assert_eq!(actual, fixture);
}

#[test]
fn real_dispatch_preflight_and_delivery_gates_return_native_git_required_without_mutation() {
    let ctx = test_ctx("native-git-gates");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/native-git-gates".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "native-git-gates-project".into(),
            name: "Native Git Gates".into(),
            repo_root: "/tmp/native-git-gates".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    git::reset_worktree_list_calls();
    let preflight = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "dispatch_preflight",
            json!({
                "projectId": "native-git-gates-project",
                "sourceCommit": "1111111111111111111111111111111111111111",
                "requestedLanes": [],
                "integrationContracts": []
            }),
        ),
    );
    assert_native_git_required(preflight, "dispatch_preflight");

    ctx.captains
        .claim_test("native-git-captain", Some("native-git-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "native-git-ship",
            "native-git-gates-project",
            "Native gate assignment",
            "codex",
        )
        .unwrap();
    let start = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "start_agent",
            json!({
                "requestId": "native-start",
                "captainSessionId": "native-git-captain",
                "assignment": "Native gate assignment",
                "directory": "/tmp/native-git-gates/worktree",
                "harness": "codex",
                "name": "Native gate agent",
                "workspaceTabId": "work",
                "sourceCommit": "1111111111111111111111111111111111111111",
                "visibleProductBug": false,
                "laneId": "native-lane",
                "dependencies": [],
                "mutableFiles": [],
                "mutableSchemas": [],
                "mutableInterfaces": [],
                "integrationContracts": [],
                "admissionPurpose": "ordinary"
            }),
        ),
    );
    assert_native_git_required(start, "start_agent");

    let (lane_claim, dispatch_capacity) =
        test_dispatch_evidence("native-delivery-lane", "native-delivery-agent");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "native-delivery-agent".into(),
            captain_session_id: "native-git-captain".into(),
            project_id: "native-git-gates-project".into(),
            assignment: "Native delivery gate".into(),
            directory: "/tmp/native-git-gates/delivery".into(),
            worktree_path: None,
            branch: None,
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Starting,
            work_stage: crate::agent_session::WorkStage::Assigned,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                "1111111111111111111111111111111111111111",
                false,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let delivery_before = ctx.captains.snapshot();
    let delivery = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "record_agent_delivery",
            json!({
                "agentSessionId": "native-delivery-agent",
                "state": "implemented",
                "evidence": {}
            }),
        ),
    );
    assert_native_git_required(delivery, "delivery");
    let integration = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "record_agent_delivery",
            json!({
                "agentSessionId": "native-delivery-agent",
                "state": "integrated",
                "evidence": {}
            }),
        ),
    );
    assert_native_git_required(integration, "integration");
    let after = ctx.captains.snapshot();
    assert_eq!(after.seq, delivery_before.seq);
    let agent = after
        .agent_sessions
        .iter()
        .find(|agent| agent.agent_session_id == "native-delivery-agent")
        .unwrap();
    assert!(agent
        .delivery
        .as_ref()
        .is_some_and(|delivery| delivery.resulting_commit.is_none()));
    assert_eq!(git::worktree_list_calls(), 0);
}

#[test]
fn worktree_list_counter_observes_calls_across_threads() {
    git::reset_worktree_list_calls();
    let calls = std::thread::spawn(|| {
        git::reset_worktree_list_calls();
        let _ = git::worktree_list("/tmp/worktree-counter-positive-control");
        git::worktree_list_calls()
    })
    .join()
    .unwrap();
    assert_eq!(calls, 1);
}

#[test]
fn native_worktree_dispatchers_gate_registered_none_before_probe_or_mutation() {
    let ctx = test_ctx("native-worktree-gates");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/native-worktree-gates".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "native-worktree-gates".into(),
            name: "Native worktree gates".into(),
            repo_root: "/tmp/native-worktree-gates".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    for (command, args, operation) in [
        (
            "create_worktree",
            json!({
                "repoRoot": "/tmp/native-worktree-gates",
                "worktreePath": "/tmp/native-worktree-gates-wt"
            }),
            "create_worktree",
        ),
        (
            "remove_worktree",
            json!({
                "repoRoot": "/tmp/native-worktree-gates",
                "worktreePath": "/tmp/native-worktree-gates-wt"
            }),
            "remove_worktree",
        ),
        (
            "list_worktrees",
            json!({ "cwd": "/tmp/native-worktree-gates" }),
            "list_worktrees",
        ),
        (
            "git_worktree_list",
            json!({ "cwd": "/tmp/native-worktree-gates" }),
            "list_worktrees",
        ),
    ] {
        let before = ctx.captains.snapshot();
        git::reset_worktree_list_calls();
        let response = dispatch_authenticated(&ctx, req("native-worktree-gates", command, args));
        assert_native_git_required(response, operation);
        assert_eq!(ctx.captains.snapshot().seq, before.seq);
        assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
        assert_eq!(git::worktree_list_calls(), 0);
    }
}

#[test]
fn stale_create_worktree_reprobe_authorizes_then_gates_without_worktree_probe() {
    let mut ctx = test_ctx("stale-native-worktree-gate");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/stale-native-worktree-gate".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "stale-native-worktree-gate".into(),
            name: "Stale native worktree gate".into(),
            repo_root: "/tmp/stale-native-worktree-gate".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let args = json!({
        "requestId": "stale-native-worktree-request",
        "repoRoot": "/tmp/stale-native-worktree-gate",
        "worktreePath": "/tmp/stale-native-worktree-gate-stale",
    });
    ctx.requests = Arc::new(RequestCache::with_bounds(
        8,
        Duration::from_secs(600),
        Duration::from_millis(1),
    ));
    let signature = request_signature("create_worktree", &args);
    assert!(matches!(
        ctx.requests
            .begin_bound_with_reservation("stale-native-worktree-request", &signature)
            .0,
        BeginOutcome::Fresh
    ));
    std::thread::sleep(Duration::from_millis(5));
    let before = ctx.captains.snapshot();
    git::reset_worktree_list_calls();
    let response = dispatch_authenticated(
        &ctx,
        req("stale-native-worktree-gate", "create_worktree", args),
    );
    assert_native_git_required(response, "create_worktree");
    assert_eq!(ctx.captains.snapshot().seq, before.seq);
    assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
    assert_eq!(git::worktree_list_calls(), 0);
    assert!(!std::path::Path::new("/tmp/stale-native-worktree-gate-stale").exists());
}

#[test]
fn delegated_none_worktree_admin_authorizes_before_git_gate_and_rejects_expired_grants() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("delegated-none-worktree-gate");
    let admin_tile = "delegated-none-admin";
    let worktree = "/tmp/delegated-none-worktree-gate/worktree";
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/delegated-none-worktree-gate".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "delegated-none-worktree-project".into(),
            name: "Delegated none worktree".into(),
            repo_root: "/tmp/delegated-none-worktree-gate".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test(
            "delegated-none-captain",
            Some("delegated-none-ship"),
            vec![],
        )
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "delegated-none-ship",
            "delegated-none-worktree-project",
            "Delegated none assignment",
            "codex",
        )
        .unwrap();
    ctx.captains
        .record_crew("delegated-none-captain", admin_tile)
        .unwrap();
    create_test_tmux_session(&tmux_target(admin_tile)).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "delegated-none-captain")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, admin_tile)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_tile,
            "role": "shipAdmin",
            "permittedOperations": ["recoverResource"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap().to_string();
    let before = ctx.captains.snapshot();
    git::reset_worktree_list_calls();
    let response = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.token,
            &admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": worktree }
            }),
        ),
    );
    assert_native_git_required(response, "admin_worktree");
    assert_eq!(ctx.captains.snapshot().seq, before.seq);
    assert_eq!(git::worktree_list_calls(), 0);

    let foreign_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, "delegated-none-foreign")
        .unwrap();
    let unauthorized = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.token,
            &foreign_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": worktree }
            }),
        ),
    );
    assert!(!unauthorized.ok);
    assert!(unauthorized.error.unwrap().contains("administrative grant"));
    assert_eq!(unauthorized.error_kind, None);
    assert_eq!(git::worktree_list_calls(), 0);

    revoke_admin(
        &ctx,
        &json!({ "grantId": grant_id, "reason": "expired-test" }),
        Some(&captain),
        false,
    )
    .unwrap();
    let expired = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.token,
            &admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": worktree }
            }),
        ),
    );
    assert!(!expired.ok);
    assert!(expired.error.unwrap().contains("administrative grant"));
    assert_eq!(expired.error_kind, None);
    assert_eq!(git::worktree_list_calls(), 0);
    reap_test_tmux_session(&tmux_target(admin_tile)).unwrap();
}

#[test]
fn registered_git_gate_uses_the_most_specific_project_independent_of_order() {
    for (outer_capability, inner_capability, expected_error) in
        [("none", "git", false), ("git", "none", true)]
    {
        for order in [["outer", "inner"], ["inner", "outer"]] {
            let registry = CaptainsRegistry::new();
            for project_id in order {
                let (root, capability) = if project_id == "outer" {
                    ("/tmp/project-nesting", outer_capability)
                } else {
                    ("/tmp/project-nesting/selected", inner_capability)
                };
                registry
                    .upsert_project(ProjectRecord {
                        root_path: Some(root.into()),
                        vcs_capability: Some(capability.into()),
                        git_main_root: None,
                        project_id: project_id.into(),
                        name: project_id.into(),
                        repo_root: root.into(),
                        remote_url: None,
                        default_branch: None,
                        powder: None,
                        created_at: 1,
                        updated_at: 1,
                    })
                    .unwrap();
            }
            let ctx = test_ctx("specific-git-gate").with_captains_registry(Arc::new(registry));
            let result = require_registered_git_capability(
                &ctx,
                "list_worktrees",
                "/tmp/project-nesting/selected/worktree",
            );
            assert_eq!(result.is_err(), expected_error);
        }
    }
}

#[test]
fn registered_git_gate_fails_closed_for_equal_specificity_ambiguity() {
    let registry = CaptainsRegistry::new();
    {
        let mut inner = registry.lock();
        inner.projects = vec![
            ProjectRecord {
                root_path: Some("/tmp/ambiguous-root".into()),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                project_id: "ambiguous-a".into(),
                name: "Ambiguous A".into(),
                repo_root: "/tmp/ambiguous-root".into(),
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            },
            ProjectRecord {
                root_path: Some("/tmp/ambiguous-root/".into()),
                vcs_capability: Some("git".into()),
                git_main_root: None,
                project_id: "ambiguous-b".into(),
                name: "Ambiguous B".into(),
                repo_root: "/tmp/ambiguous-root/".into(),
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            },
        ];
    }
    let ctx = test_ctx("ambiguous-git-gate").with_captains_registry(Arc::new(registry));
    let error =
        require_registered_git_capability(&ctx, "list_worktrees", "/tmp/ambiguous-root/selected")
            .unwrap_err();
    assert!(error.contains("ambiguous"));
    assert!(!error.contains("git_required"));
}
