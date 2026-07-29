use super::*;

#[test]
fn generic_spawn_refuses_control_capability_without_a_durable_authority() {
    assert!(require_read_only_spawn(&json!({}), "spawn_terminal").is_ok());
    assert!(require_read_only_spawn(&json!({"capability": "read"}), "spawn_terminal").is_ok());
    assert!(
        require_read_only_spawn(&json!({"capability": "control"}), "spawn_terminal")
            .unwrap_err()
            .contains("unsupported for generic Crew spawns")
    );
    assert!(require_read_only_spawn(&json!({"capability": "unknown"}), "spawn_terminal").is_err());
}

#[test]
fn spawn_env_mints_and_injects_a_per_session_identity_token() {
    let mut ctx = test_ctx("t");
    ctx.addr = "127.0.0.1:4242".to_string();
    let (env, minted) = spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", None).unwrap();
    // Rotating tier and endpoint values are scrubbed.
    assert!(env
        .iter()
        .any(|(k, v)| k == "T_HUB_CONTROL_TOKEN" && v.is_empty()));
    assert!(env
        .iter()
        .any(|(k, v)| k == "T_HUB_CONTROL_ADDR" && v.is_empty()));
    assert!(env
        .iter()
        .any(|(k, v)| k == "T_HUB_CONTROL_FILE" && !v.is_empty()));
    // The durable per-session token is injected alongside stable discovery.
    let session_token = env
        .iter()
        .find(|(k, _)| k == crate::identity::SESSION_TOKEN_ENV)
        .map(|(_, v)| v.clone())
        .expect("spawn env injects the per-session token");
    let identity = minted.expect("an identity is minted when addr is set");
    // The injected token resolves back to exactly this session's identity - the
    // per-session attribution the plane stamps enqueues with.
    let resolved = ctx
        .identity
        .resolve(&session_token)
        .expect("the injected per-session token resolves");
    assert_eq!(resolved.id, identity.id);
    assert_eq!(resolved.role, crate::identity::Role::Crew);
    // The per-session token is NOT the shared control token (that is the whole
    // point - it is per-session, unforgeable across sessions).
    assert_ne!(session_token, ctx.token);

    // Headless (no addr): no identity minted, env empty, spawns behave as before.
    ctx.addr = String::new();
    let (env2, minted2) =
        spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", None).unwrap();
    assert!(env2.is_empty());
    assert!(minted2.is_none());
}

#[test]
fn requested_control_is_refused_before_a_crew_identity_is_minted() {
    let mut ctx = test_ctx("identity-prebind");
    ctx.addr = "127.0.0.1:4242".to_string();
    let error = spawn_env_with_identity(
        &ctx,
        &json!({"capability": "control"}),
        "spawn_terminal",
        Some("fa123456"),
    )
    .unwrap_err();
    assert!(error.contains("unsupported for generic Crew spawns"));
    assert!(ctx.identity.is_empty());
}

#[test]
fn requested_session_identity_is_bound_before_launch_and_prebind_failure_rolls_back() {
    let mut ctx = test_ctx("identity-prebind");
    ctx.addr = "127.0.0.1:4242".to_string();
    let (_, minted) =
        spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", Some("fa123456")).unwrap();
    let minted = minted.unwrap();
    assert_eq!(minted.session_tile.as_deref(), Some("fa123456"));
    assert_eq!(
        ctx.identity
            .resolve(&minted.secret)
            .and_then(|identity| identity.session_tile),
        Some("fa123456".into())
    );

    let path = captains_tmp("identity-prebind-rollback");
    let store = Arc::new(crate::identity::IdentityStore::load(path.clone()));
    // mint_and_bind persists the pre-bound identity atomically in one write.
    store.fail_persist_after(0);
    let mut failing = test_ctx("identity-prebind-rollback").with_identity_store(store.clone());
    failing.addr = "127.0.0.1:4242".to_string();
    let error = spawn_env_with_identity(&failing, &json!({}), "spawn_terminal", Some("fa654321"))
        .unwrap_err();
    assert!(error.contains("identity pre-binding persistence failed"));
    assert!(store.is_empty());
    assert!(crate::identity::IdentityStore::load(path.clone()).is_empty());
    std::fs::remove_file(path).ok();
}

#[test]
fn failed_requested_session_spawn_retires_the_prebound_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "failed_requested_session_spawn_retires_the_prebound_identity: tmux or node not on PATH - skipping"
            );
        return;
    }
    let mut ctx =
        test_ctx("identity-prebound-spawn-rollback").with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4242".to_string();
    let session_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let target = tmux_target(&session_id);
    create_test_tmux_session(&target).unwrap();
    let result = spawn_terminal_with_private_pane_command_and_id(
        &ctx,
        &json!({"cwd": "/tmp", "capability": "control"}),
        None,
        false,
        false,
        false,
        Some(&session_id),
    );
    assert!(result.is_err());
    assert!(ctx.identity.is_empty());
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    reap_test_tmux_session(&target).unwrap();
}

#[test]
fn socket_spawn_fails_before_tmux_when_identity_mint_is_not_durable() {
    let blocker = captains_tmp("identity-mint-blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let store = Arc::new(crate::identity::IdentityStore::load(
        blocker.join("identities.json"),
    ));
    let mut ctx = test_ctx("ctrl")
        .with_identity_store(store.clone())
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4242".to_string();

    let response = dispatch_authenticated(
        &ctx,
        req(
            "ctrl",
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "identity-persist-failure"}),
        ),
    );

    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("identity store persist"),
        "spawn must surface the durability failure"
    );
    assert!(store.is_empty());
    std::fs::remove_file(blocker).unwrap();
}

#[test]
fn socket_spawn_kills_terminal_when_identity_bind_is_not_durable() {
    let path = captains_tmp("identity-bind-failure");
    let store = Arc::new(crate::identity::IdentityStore::load(path.clone()));
    store.fail_persist_after(1);
    let mut ctx = test_ctx("ctrl")
        .with_identity_store(store.clone())
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4242".to_string();

    let response = dispatch_authenticated(
        &ctx,
        req(
            "ctrl",
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "identity-bind-failure"}),
        ),
    );

    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("terminal was rolled back"),
        "spawn must report its compensating rollback"
    );
    assert!(
        store.is_empty(),
        "the rolled-back spawn must retire its identity"
    );
    let persisted = crate::identity::IdentityStore::load(path.clone());
    assert!(
        persisted.is_empty(),
        "rollback must remove the durable identity"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn socket_close_reports_identity_retirement_persistence_failure() {
    let path = captains_tmp("identity-retire-close-failure");
    let store = Arc::new(crate::identity::IdentityStore::load(path.clone()));
    let identity = store.mint(crate::identity::Role::Crew).unwrap();
    store.bind_tile(&identity.id, "already-gone").unwrap();
    store.fail_persist_after(0);
    let ctx = test_ctx("ctrl").with_identity_store(store.clone());

    let response = dispatch_authenticated(
        &ctx,
        req(
            "ctrl",
            "close_terminal",
            json!({"sessionId": "already-gone"}),
        ),
    );

    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("identity store persist failure injected"),
        "close must surface failed durable identity retirement"
    );
    assert!(store.resolve(&identity.secret).is_some());
    assert!(
        crate::identity::IdentityStore::load(path.clone())
            .resolve(&identity.secret)
            .is_some(),
        "failed retirement must leave memory and disk aligned"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn every_crew_spawn_is_credential_withheld_regardless_of_capability() {
    // item-3 §2.3.5: every Crew spawn gets gh withholding (GH_CONFIG_DIR at an
    // empty dir) plus blanked ambient tokens.
    // A request for a generic administrative Crew is refused. Every allowed
    // read-only Crew spawn still withholds publishing credentials.
    let mut ctx = test_ctx("t");
    ctx.addr = "127.0.0.1:4242".to_string();

    let (env, _) = spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", None).unwrap();
    let gh_dir = env
        .iter()
        .find(|(k, _)| k == "GH_CONFIG_DIR")
        .map(|(_, v)| v.as_str());
    assert!(
        gh_dir.is_some_and(|v| !v.is_empty()),
        "a crew spawn must withhold gh via GH_CONFIG_DIR"
    );
    // The value rides a `tmux -e` into a WSL shell, so it must be a POSIX path:
    // no backslash, no `C:`-style drive, forward-slash absolute. A Windows path
    // (the old USERPROFILE/PathBuf::join form) silently defeated withholding.
    assert!(
        !env.iter().any(|(_, v)| v.contains('\\')),
        "no emitted env value may contain a backslash (Windows) path: {env:?}"
    );
    assert!(
        gh_dir.is_some_and(|v| v.starts_with('/') && !v.contains(":\\")),
        "GH_CONFIG_DIR must be a POSIX-absolute path, got {gh_dir:?}"
    );
    assert!(
        env.iter().any(|(k, v)| k == "GH_TOKEN" && v.is_empty()),
        "a crew spawn must blank the ambient GH_TOKEN"
    );

    for purpose in ["fleet-admin", "ship-admin", "recovery"] {
        let refusal = spawn_env_with_identity(
            &ctx,
            &json!({
                "capability": "control",
                "admissionPurpose": purpose
            }),
            "spawn_terminal",
            None,
        )
        .unwrap_err();
        assert!(refusal.contains("unsupported for generic Crew spawns"));
        let (admin_env, _) = spawn_env_with_identity(
            &ctx,
            &json!({
                "capability": "read",
                "admissionPurpose": purpose
            }),
            "spawn_terminal",
            None,
        )
        .unwrap();
        assert!(
            admin_env.iter().any(|(key, _)| key == "GH_CONFIG_DIR"),
            "a {purpose} Crew spawn must still withhold gh credentials"
        );
        for token in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "NPM_TOKEN",
            "NODE_AUTH_TOKEN",
            "CARGO_REGISTRY_TOKEN",
        ] {
            assert!(
                admin_env
                    .iter()
                    .any(|(key, value)| key == token && value.is_empty()),
                "a {purpose} Crew spawn must blank ambient {token}"
            );
        }
    }
}

#[test]
fn crew_gh_config_dir_is_always_a_backslash_free_posix_path() {
    // audit HIGH: the value rides a `tmux -e` into WSL, so it must ALWAYS be a
    // POSIX path. BYPASS-WOULD-FAIL: restore the USERPROFILE/PathBuf::join form
    // and the Windows-path cases below emit `C:\...\.t-hub\...` → RED.

    // A POSIX-absolute HOME (WSL-launched app) is used verbatim.
    assert_eq!(
        crew_gh_config_dir_from_home(Some("/home/natkins")),
        format!("/home/natkins/{CREW_GH_CONFIG_SUBDIR}")
    );
    // A trailing slash is normalized (no doubled `//`).
    assert_eq!(
        crew_gh_config_dir_from_home(Some("/home/natkins/")),
        format!("/home/natkins/{CREW_GH_CONFIG_SUBDIR}")
    );
    // A Windows USERPROFILE-style value is REJECTED (the crux of the bug): it
    // falls back to a fixed POSIX path, never a backslash/drive path.
    for windows_home in [r"C:\Users\natha", r"C:\Users\natha\", r"D:\home"] {
        let dir = crew_gh_config_dir_from_home(Some(windows_home));
        assert_eq!(dir, format!("/tmp/{CREW_GH_CONFIG_SUBDIR}"));
        assert!(!dir.contains('\\'), "no backslash: {dir}");
        assert!(!dir.contains(":\\"), "no drive path: {dir}");
    }
    // An absent HOME also falls back to the POSIX path (native-Windows launch).
    assert_eq!(
        crew_gh_config_dir_from_home(None),
        format!("/tmp/{CREW_GH_CONFIG_SUBDIR}")
    );
}

#[test]
fn orchestrator_home_is_scoped_and_rejects_traversal() {
    assert_eq!(
        resolve_orchestrator_home("/home/tester", None).unwrap(),
        format!("/home/tester/{CORTANA_HOME_DEFAULT}")
    );
    assert_eq!(
        resolve_orchestrator_home("/home/tester", Some(".t-hub-dev/custom-orchestrator")).unwrap(),
        "/home/tester/.t-hub-dev/custom-orchestrator"
    );
    assert_eq!(
        resolve_orchestrator_home("/home/tester", Some("/srv/t-hub/cortana")).unwrap(),
        "/srv/t-hub/cortana"
    );
    assert!(resolve_orchestrator_home("/home/tester", Some("../production")).is_err());
    assert!(resolve_orchestrator_home("/home/tester", Some(r"C:\production")).is_err());
}
