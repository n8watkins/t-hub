use super::*;

#[test]
fn git_init_recovery_errors_are_structured_on_the_control_wire() {
    let response = ControlResponse::err(
            "git_init_recovery code=git_init_recovery operation=git-init-123 phase=recovery_blocked message=ownership marker changed",
        );
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["errorKind"], "git_init_recovery");
    assert_eq!(wire["errorDetails"]["operation"], "git-init-123");
    assert_eq!(wire["errorDetails"]["phase"], "recovery_blocked");
    assert_eq!(wire["error"], "ownership marker changed");
    assert!(!wire.to_string().contains("git_init_recovery:"));
}

#[test]
fn project_commands_register_idempotently_and_powder_commands_are_tombstoned() {
    let ctx = test_ctx("secret");
    let repo = env!("CARGO_MANIFEST_DIR");
    let first = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": repo, "name": "T-Hub"}),
    )
    .unwrap();
    let second = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": repo, "name": "T-Hub"}),
    )
    .unwrap();
    assert_eq!(first["projectId"], second["projectId"]);

    let catalog = dispatch(&ctx, "list_projects", &json!({})).unwrap();
    assert_eq!(catalog["count"], 1);
    assert_eq!(catalog["projects"][0]["projectId"], first["projectId"]);

    for command in [
        "dispatch_crew",
        "list_powder_boards",
        "bind_project_powder",
        "project_board_snapshot",
        "powder_status",
        "heartbeat_crew_powder",
        "append_crew_powder_work_log",
        "read_crew_powder_evidence",
        "review_crew_powder_criterion",
        "complete_crew_powder",
    ] {
        assert!(is_retired_powder_command(command));
        let response = ControlResponse::powder_retired(command);
        assert!(!response.ok);
        assert_eq!(response.error_kind.as_deref(), Some("powder_retired"));
        assert_eq!(
            response.error.as_deref(),
            Some(
                format!("{command} is retired; use the agent session operations instead").as_str()
            )
        );
        assert!(!response.retryable);
    }
}

#[test]
fn register_project_authorizes_before_creating_or_initializing_files() {
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let captain = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "foreign-ship",
        "foreign-captain",
    );
    let ctx = test_ctx("register-project-scope").with_identity_store(identities);
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-scope-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&parent).unwrap();
    let requested = parent.join("must-not-exist");

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "register-project-scope",
            &captain,
            "register_project",
            json!({
                "repoRoot": requested.to_string_lossy(),
                "name": "Scoped Project",
                "createDirectory": true,
            }),
        ),
    );

    assert!(!response.ok);
    assert!(response
        .error
        .as_deref()
        .is_some_and(|error| error.contains("only General/Cortana")));
    assert!(
        !requested.exists(),
        "authorization ran after filesystem mutation"
    );
    let _ = std::fs::remove_dir(parent);
}

#[test]
fn unauthorized_project_root_requests_have_zero_probe_or_persistence_counts() {
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let caller = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "foreign-project-ship",
        "foreign-project-captain",
    );
    let ctx = test_ctx("project-probe-order").with_identity_store(identities);
    let existing = std::env::temp_dir().join(format!("t-hub-unauthorized-existing-{}", now_ms()));
    std::fs::create_dir_all(&existing).unwrap();
    let missing = existing.join("missing");

    for (command, root) in [
        ("register_project", existing.clone()),
        ("initialize_git", missing),
    ] {
        reset_project_probe_counts();
        let args = if command == "register_project" {
            json!({ "rootPath": root.to_string_lossy(), "name": "Denied Project", "createDirectory": true })
        } else {
            json!({ "rootPath": root.to_string_lossy(), "name": "Denied Project" })
        };
        let response = dispatch_authenticated(
            &ctx,
            req_session("project-probe-order", &caller, command, args),
        );
        assert!(!response.ok, "{command} unexpectedly succeeded");
        assert!(response
            .error
            .unwrap_or_default()
            .contains("only General/Cortana"));
        assert_eq!(
            project_probe_counts(),
            [0; 6],
            "{command} probed before authority"
        );
        assert!(ctx.captains.projects().is_empty());
    }
    let _ = std::fs::remove_dir_all(existing);
}

#[test]
fn project_root_identity_accepts_posix_and_all_supported_wsl_unc_spellings() {
    let expected = "/home/natkins/projects/demo";
    for spelling in [
        expected,
        "/home/natkins/projects/./demo/",
        r#"\\wsl.localhost\Ubuntu-24.04\home\natkins\projects\demo"#,
        r#"\\wsl$\Ubuntu-24.04\home\natkins\projects\demo"#,
        r#"\\?\UNC\wsl.localhost\Ubuntu-24.04\home\natkins\projects\demo\."#,
    ] {
        assert_eq!(canonical_project_identity(spelling).unwrap(), expected);
    }
}

#[test]
fn project_root_identity_rejects_relative_traversal_foreign_and_unsafe_unc() {
    for spelling in [
        "relative/project",
        "/tmp/../secret",
        r#"\\wsl.localhost\Debian\home\natkins\project"#,
        r#"\\server\share\project"#,
    ] {
        assert!(
            canonical_project_identity(spelling).is_err(),
            "accepted {spelling}"
        );
    }
}

#[test]
fn conflicting_root_aliases_fail_before_project_probes_or_mutation() {
    let ctx = test_ctx("root-alias-conflict");
    reset_project_probe_counts();
    let response = dispatch(
        &ctx,
        "register_project",
        &json!({
            "rootPath": "/tmp/root-primary",
            "repoRoot": "/tmp/root-conflict",
            "name": "Conflicting Roots",
            "createDirectory": true,
        }),
    )
    .unwrap_err();
    assert!(response.contains("conflicting rootPath and repoRoot"));
    assert!(ctx.captains.projects().is_empty());
    assert_eq!(project_probe_counts(), [0; 6]);
    assert!(!std::path::Path::new("/tmp/root-primary").exists());
    assert!(!std::path::Path::new("/tmp/root-conflict").exists());
}

#[test]
fn register_project_accepts_each_root_identity_contract_form() {
    let forms = ["rootPath", "repoRoot", "repo_root"];
    for (index, field) in forms.into_iter().enumerate() {
        let ctx = test_ctx(&format!("root-alias-form-{field}"));
        let dir = std::env::temp_dir().join(format!(
            "t-hub-root-alias-form-{}-{}-{index}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut args = serde_json::Map::new();
        args.insert(field.to_string(), json!(dir.to_string_lossy()));
        args.insert("name".to_string(), json!(format!("Root Form {field}")));
        let project = dispatch(&ctx, "register_project", &Value::Object(args)).unwrap();
        assert_eq!(project["rootPath"], dir.to_string_lossy().to_string());
        assert_eq!(project["repoRoot"], project["rootPath"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn initialize_git_conflicting_root_aliases_fail_before_probes() {
    let ctx = test_ctx("initialize-root-alias-conflict");
    reset_project_probe_counts();
    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({
            "rootPath": "/tmp/initialize-root-primary",
            "repo_root": "/tmp/initialize-root-conflict",
            "name": "Conflicting Initialize Roots",
        }),
    )
    .unwrap_err();
    assert!(error.contains("conflicting rootPath and repoRoot"));
    assert_eq!(project_probe_counts(), [0; 6]);
    assert!(ctx.captains.projects().is_empty());
}

#[test]
fn every_root_alias_conflict_is_rejected_before_dispatch_probes() {
    let conflicts = [
        json!({
            "rootPath": "/tmp/root-primary",
            "repoRoot": "/tmp/root-conflict",
            "name": "Conflicting Roots",
        }),
        json!({
            "repoRoot": "/tmp/repo-root-primary",
            "repo_root": "/tmp/repo-root-conflict",
            "name": "Conflicting Roots",
        }),
        json!({
            "rootPath": "/tmp/three-field-primary",
            "repoRoot": "/tmp/three-field-primary",
            "repo_root": "/tmp/three-field-conflict",
            "name": "Conflicting Roots",
        }),
    ];
    for command in ["register_project", "initialize_git"] {
        for mut args in conflicts.clone() {
            if command == "register_project" {
                args["createDirectory"] = json!(true);
            }
            let ctx = test_ctx(&format!("root-alias-conflict-{command}"));
            reset_project_probe_counts();
            let error = dispatch(&ctx, command, &args).unwrap_err();
            assert!(
                error.contains("conflicting rootPath and repoRoot"),
                "{command}: {error}"
            );
            assert_eq!(
                project_probe_counts(),
                [0; 6],
                "{command} performed a probe before alias validation"
            );
            assert!(ctx.captains.projects().is_empty());
        }
    }
}

#[test]
fn all_equal_root_aliases_dispatch_without_duplicate_identity() {
    let register_ctx = test_ctx("root-alias-all-equal-register");
    let register_dir = std::env::temp_dir().join(format!(
        "t-hub-root-alias-all-equal-register-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&register_dir).unwrap();
    let register_root = register_dir.to_string_lossy().to_string();
    let registered = dispatch(
        &register_ctx,
        "register_project",
        &json!({
            "rootPath": format!("{register_root}/./"),
            "repoRoot": register_root,
            "repo_root": format!("{}/", register_dir.to_string_lossy()),
            "name": "All Equal Roots",
        }),
    )
    .unwrap();
    assert_eq!(registered["rootPath"], registered["repoRoot"]);
    assert_eq!(register_ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(register_dir);

    let initialize_ctx = test_ctx("root-alias-all-equal-initialize");
    let initialize_dir = std::env::temp_dir().join(format!(
        "t-hub-root-alias-all-equal-initialize-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&initialize_dir).unwrap();
    let initialize_root = initialize_dir.to_string_lossy().to_string();
    let initialized = dispatch(
        &initialize_ctx,
        "initialize_git",
        &json!({
            "rootPath": format!("{initialize_root}/./"),
            "repoRoot": initialize_root,
            "repo_root": format!("{}/", initialize_dir.to_string_lossy()),
            "name": "All Equal Initialized Roots",
        }),
    )
    .unwrap();
    assert_eq!(initialized["rootPath"], initialized["repoRoot"]);
    assert_eq!(initialize_ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(initialize_dir);
}

#[test]
fn equal_root_aliases_register_using_authoritative_root_path() {
    let ctx = test_ctx("root-alias-equal");
    let dir = std::env::temp_dir().join(format!("t-hub-root-alias-{}", now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.to_string_lossy().to_string();
    let response = dispatch(
        &ctx,
        "register_project",
        &json!({
            "rootPath": format!("{root}/./"),
            "repoRoot": root,
            "name": "Equal Roots",
        }),
    )
    .unwrap();
    assert_eq!(response["rootPath"], response["repoRoot"]);
    assert_eq!(response["rootPath"], dir.to_string_lossy().to_string());
    assert_eq!(ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn register_project_retains_linked_selection_and_separate_git_main_root() {
    let (base, repo, linked) = scratch_repo_with_worktree();
    let ctx = test_ctx("linked-project-registration");
    let selected = linked.to_string_lossy().to_string();
    let project = dispatch(
        &ctx,
        "register_project",
        &json!({ "rootPath": selected, "name": "Linked Selection" }),
    )
    .unwrap();
    assert_eq!(project["rootPath"], linked.to_string_lossy().to_string());
    assert_eq!(project["repoRoot"], project["rootPath"]);
    assert_eq!(project["gitMainRoot"], repo.to_string_lossy().to_string());
    assert_ne!(project["rootPath"], project["gitMainRoot"]);
    assert_eq!(ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn concurrent_symlink_equivalent_registrations_converge_to_one_project() {
    let parent = std::env::temp_dir().join(format!("t-hub-project-race-{}", now_ms()));
    let root = parent.join("root");
    let alias = parent.join("alias");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let expected_root = root.to_string_lossy().to_string();
    let ctx = Arc::new(test_ctx("project-registration-race"));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let joins = [root.clone(), alias]
        .into_iter()
        .map(|path| {
            let ctx = Arc::clone(&ctx);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "register_project",
                    &json!({ "rootPath": path.to_string_lossy(), "name": "Raced Project" }),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
    assert!(results.iter().all(Result::is_ok), "results: {results:?}");
    let projects = ctx.captains.projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects[0].root_path.as_deref(),
        Some(expected_root.as_str())
    );
    assert_eq!(
        projects[0].repo_root,
        projects[0].root_path.clone().unwrap()
    );
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn register_project_accepts_a_non_repository_without_initializing_git() {
    let ctx = test_ctx("secret");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-register-nonrepo-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let project = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": dir.to_string_lossy(), "name": "Non Git Project"}),
    )
    .unwrap();
    assert_eq!(project["repoRoot"], dir.to_string_lossy().to_string());
    assert!(!dir.join(".git").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_is_separate_from_register_project() {
    let ctx = test_ctx("secret");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-register-init-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), "preserve me").unwrap();

    let project = dispatch(
        &ctx,
        "initialize_git",
        &json!({"repoRoot": dir.to_string_lossy(), "name": "Initialized Project"}),
    )
    .unwrap();

    assert_eq!(project["repoRoot"], dir.to_string_lossy().as_ref());
    assert_eq!(project["defaultBranch"], "main");
    assert!(dir.join(".git").is_dir());
    assert_eq!(
        std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
        "preserve me"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_recovers_durable_transaction_after_restart() {
    let ctx = test_ctx("initialize-git-recovery");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-recovery-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));
    set_git_init_fault("after_marker_before_project");

    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "repoRoot": dir.to_string_lossy(), "name": "Recovery Project" }),
    )
    .unwrap_err();
    clear_git_init_fault();

    assert!(
        error.contains("injected Git initialization fault"),
        "got: {error}"
    );
    assert!(dir.join(".git").is_dir());
    assert_eq!(registry.pending_git_initializations().len(), 1);
    assert!(dir.join(".git/t-hub-git-init-marker.json").is_file());

    let recovered = CaptainsRegistry::load(registry_path.clone());
    let project = recovered
        .projects()
        .into_iter()
        .find(|project| project.name == "Recovery Project")
        .expect("restart should finalize the owned Git initialization");
    assert_eq!(project.vcs_capability.as_deref(), Some("git"));
    assert_eq!(
        project.root_path.as_deref(),
        project.repo_root.as_str().into()
    );
    assert!(recovered.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());

    let recovered_again = CaptainsRegistry::load(registry_path.clone());
    assert_eq!(recovered_again.projects(), recovered.projects());
    assert!(recovered_again.pending_git_initializations().is_empty());

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn initialize_git_fault_boundaries_recover_deterministically() {
    for (index, fault, expects_git, expects_project) in [
        (0, "after_intent_before_git_init", false, false),
        (1, "after_git_init_before_marker", true, false),
        (2, "after_marker_before_project", true, true),
        (3, "after_project_before_clear", true, true),
        (4, "during_cleanup", true, true),
    ] {
        let ctx = test_ctx("initialize-git-fault");
        let dir = std::env::temp_dir().join(format!(
            "t-hub-initialize-git-fault-{index}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let registry_path = dir.with_extension("json");
        std::fs::create_dir_all(&dir).unwrap();
        let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        let ctx = ctx.with_captains_registry(Arc::clone(&registry));
        set_git_init_fault(fault);

        let response = dispatch(
            &ctx,
            "initialize_git",
            &json!({ "rootPath": dir.to_string_lossy(), "name": "Fault Project" }),
        );
        clear_git_init_fault();
        assert!(response.is_err(), "fault {fault} did not fire");
        assert!(
            response
                .as_ref()
                .unwrap_err()
                .starts_with("git_init_recovery code=git_init_recovery operation="),
            "fault {fault} returned a non-structured error: {response:?}"
        );

        let restarted = CaptainsRegistry::load(registry_path.clone());
        assert_eq!(dir.join(".git").is_dir(), expects_git, "fault {fault}");
        assert_eq!(
            restarted
                .projects()
                .iter()
                .any(|project| project.name == "Fault Project"),
            expects_project,
            "fault {fault}"
        );
        if fault == "after_git_init_before_marker" {
            assert_eq!(restarted.pending_git_initializations().len(), 1);
            assert!(
                restarted.pending_git_initializations()[0].phase.as_str() == "recovery_blocked"
            );
        } else {
            assert!(
                restarted.pending_git_initializations().is_empty(),
                "fault {fault}"
            );
        }
        let restarted_again = CaptainsRegistry::load(registry_path.clone());
        assert_eq!(
            restarted_again.projects(),
            restarted.projects(),
            "fault {fault} was not idempotent"
        );
        if expects_project {
            assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(registry_path);
    }
}

#[test]
fn initialize_git_before_intent_fault_has_no_durable_or_filesystem_residue() {
    let ctx = test_ctx("initialize-git-before-intent");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-before-intent-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    set_git_init_fault("before_intent_write");
    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Before Intent" }),
    )
    .unwrap_err();
    clear_git_init_fault();
    assert!(error.contains("before_intent_write"));
    assert!(!dir.join(".git").exists());
    assert!(ctx.captains.pending_git_initializations().is_empty());
    assert!(ctx.captains.projects().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_concurrent_equivalent_requests_converge_to_one_transaction() {
    let ctx = test_ctx("initialize-git-concurrent-equivalent");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-concurrent-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let joins = (0..2)
        .map(|_| {
            let ctx = ctx.clone();
            let barrier = Arc::clone(&barrier);
            let root = dir.to_string_lossy().to_string();
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "initialize_git",
                    &json!({ "rootPath": root, "name": "Concurrent Git Project" }),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(Result::is_ok), "results: {results:?}");
    assert_eq!(ctx.captains.projects().len(), 1);
    assert!(ctx.captains.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_conflicting_names_refuse_before_a_second_mutation() {
    let ctx = test_ctx("initialize-git-conflicting-names");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-conflicting-names-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let joins = ["First Git Project", "Conflicting Git Project"]
        .into_iter()
        .map(|name| {
            let ctx = ctx.clone();
            let barrier = Arc::clone(&barrier);
            let root = dir.to_string_lossy().to_string();
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "initialize_git",
                    &json!({ "rootPath": root, "name": name }),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let errors = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("conflicting durable Project name"));
    assert_eq!(ctx.captains.projects().len(), 1);
    assert!(ctx.captains.pending_git_initializations().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_project_persistence_failure_leaves_recoverable_evidence() {
    let ctx = test_ctx("initialize-git-project-persist-failure");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-project-failure-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_registry = Arc::clone(&registry);
    let hook_calls = Arc::clone(&calls);
    registry.set_persist_hook(Box::new(move || {
        if hook_calls.fetch_add(1, Ordering::SeqCst) == 2 {
            hook_registry.fail_next_persist("injected Project persistence failure");
        }
    }));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));

    let response = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Persisted Recovery Project" }),
    );
    assert!(response
        .unwrap_err()
        .contains("injected Project persistence failure"));
    assert!(dir.join(".git").is_dir());
    assert!(dir.join(".git/t-hub-git-init-marker.json").is_file());

    let restarted = CaptainsRegistry::load(registry_path.clone());
    assert_eq!(restarted.projects().len(), 1);
    assert_eq!(
        restarted.projects()[0].vcs_capability.as_deref(),
        Some("git")
    );
    assert!(restarted.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn initialize_git_refuses_foreign_or_tampered_git_state_without_deletion() {
    let ctx = test_ctx("initialize-git-ownership");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-ownership-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/foreign"), "keep").unwrap();
    let response = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Foreign Project" }),
    );
    assert!(response.unwrap_err().contains("pre-existing .git"));
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign")).unwrap(),
        "keep"
    );
    assert!(!ctx
        .captains
        .projects()
        .iter()
        .any(|project| project.name == "Foreign Project"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_foreign_git_clear_failure_survives_restart_without_ownership() {
    let ctx = test_ctx("initialize-git-foreign-clear-failure");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-foreign-clear-failure-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/foreign-state"), "preserve").unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_registry = Arc::clone(&registry);
    let hook_calls = Arc::clone(&calls);
    registry.set_persist_hook(Box::new(move || {
        if hook_calls.fetch_add(1, Ordering::SeqCst) == 2 {
            hook_registry.fail_next_persist("injected foreign intent clear failure");
        }
    }));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));

    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Foreign Clear Failure" }),
    )
    .unwrap_err();
    assert!(error.starts_with("git_init_recovery code=git_init_recovery operation="));
    assert_eq!(registry.pending_git_initializations().len(), 1);
    assert_eq!(
        registry.pending_git_initializations()[0].phase,
        "foreign_git"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign-state")).unwrap(),
        "preserve"
    );

    set_git_init_fault("foreign_cleanup");
    let restarted = CaptainsRegistry::load(registry_path.clone());
    clear_git_init_fault();
    assert!(restarted.projects().is_empty());
    assert_eq!(restarted.pending_git_initializations().len(), 1);
    assert_eq!(
        restarted.pending_git_initializations()[0].phase,
        "foreign_git"
    );
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign-state")).unwrap(),
        "preserve"
    );
    let restarted_again = CaptainsRegistry::load(registry_path.clone());
    assert!(restarted_again.projects().is_empty());
    assert!(restarted_again.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign-state")).unwrap(),
        "preserve"
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn initialize_git_tampered_marker_fails_closed_across_restart() {
    let ctx = test_ctx("initialize-git-tampered-marker");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-tampered-marker-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));
    set_git_init_fault("after_marker_before_project");
    let _ = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Tampered Project" }),
    );
    clear_git_init_fault();

    let marker_path = dir.join(".git/t-hub-git-init-marker.json");
    let mut marker: GitInitMarker =
        serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
    marker.marker_nonce = "foreign-nonce".into();
    std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();

    let restarted = CaptainsRegistry::load(registry_path.clone());
    assert!(restarted.projects().is_empty());
    assert_eq!(restarted.pending_git_initializations().len(), 1);
    assert_eq!(
        restarted.pending_git_initializations()[0].phase,
        "recovery_blocked"
    );
    assert!(marker_path.is_file());
    let restarted_again = CaptainsRegistry::load(registry_path.clone());
    assert!(restarted_again.projects().is_empty());
    assert_eq!(restarted_again.pending_git_initializations().len(), 1);
    assert!(marker_path.is_file());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn register_project_never_rewrites_an_existing_git_entry() {
    let ctx = test_ctx("secret");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-register-existing-git-entry-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/owner"), "pre-existing").unwrap();

    let project = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": dir.to_string_lossy(), "name": "Existing Git Project"}),
    )
    .unwrap();
    assert_eq!(project["vcsCapability"], "none");
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/owner")).unwrap(),
        "pre-existing"
    );
    assert_eq!(ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn register_project_creates_an_absent_empty_codebase_leaf() {
    let ctx = test_ctx("secret");
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-new-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir(&parent).unwrap();
    let destination = parent.join("fresh-codebase");

    let project = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": destination.to_string_lossy(),
            "name": "Fresh Codebase",
            "createDirectory": true,
        }),
    )
    .unwrap();

    assert_eq!(project["name"], "Fresh Codebase");
    assert_eq!(project["repoRoot"], destination.to_string_lossy().as_ref());
    assert!(project["defaultBranch"].is_null());
    assert!(!destination.join(".git").exists());
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn register_project_new_codebase_refuses_any_existing_destination() {
    let ctx = test_ctx("secret");
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-new-existing-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let destination = parent.join("already-here");
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("keep.txt"), "preserve me").unwrap();

    let error = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": destination.to_string_lossy(),
            "createDirectory": true,
            "name": "Existing Destination"
        }),
    )
    .unwrap_err();

    assert!(error.contains("already exists"), "got: {error}");
    assert_eq!(
        std::fs::read_to_string(destination.join("keep.txt")).unwrap(),
        "preserve me"
    );
    assert!(ctx.captains.projects().is_empty());
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn register_project_new_codebase_can_remain_non_git() {
    let ctx = test_ctx("secret");
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-new-invalid-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir(&parent).unwrap();
    let destination = parent.join("missing-init");
    let project = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": destination.to_string_lossy(),
            "createDirectory": true,
            "name": "Missing Init"
        }),
    )
    .unwrap();
    assert_eq!(
        project["repoRoot"],
        destination.to_string_lossy().to_string()
    );
    assert!(destination.is_dir());
    assert!(!destination.join(".git").exists());

    let trailing_slash = format!("{}/", parent.join("ambiguous").to_string_lossy());
    let initialized = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": trailing_slash,
            "createDirectory": true,
            "name": "Trailing Path"
        }),
    )
    .unwrap();
    assert_eq!(
        initialized["repoRoot"],
        parent.join("ambiguous").to_string_lossy().to_string()
    );
    assert!(!parent.join("ambiguous/.git").exists());

    let missing_parent = parent.join("missing").join("child");
    let error = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": missing_parent.to_string_lossy(),
            "createDirectory": true,
            "name": "Missing Parent"
        }),
    )
    .unwrap_err();
    assert!(error.contains("parent directory"), "got: {error}");
    assert!(!parent.join("missing").exists());
    let _ = std::fs::remove_dir(parent);
}

#[test]
fn register_project_rejects_retired_powder_arguments_before_git_work() {
    let ctx = test_ctx("secret");
    let response = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": "/tmp/not-touched",
            "powderRepository": "legacy-board"
        }),
    )
    .unwrap_err();
    assert!(response.contains("unexpected argument 'powderRepository'"));
}
