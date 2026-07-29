use super::*;

#[test]
fn cortana_observation_retries_only_transient_unreadable_evidence() {
    let mut attempts = 0;
    let observed =
        retry_unreadable_cortana_observation(Instant::now() + Duration::from_secs(1), |_| {
            attempts += 1;
            if attempts < 3 {
                Err(crate::harness::LaunchAttestationError::UnreadableEvidence)
            } else {
                Ok("stable")
            }
        })
        .unwrap();
    assert_eq!(observed, "stable");
    assert_eq!(attempts, 3);

    let mut mismatch_attempts = 0;
    let error =
        retry_unreadable_cortana_observation::<()>(Instant::now() + Duration::from_secs(1), |_| {
            mismatch_attempts += 1;
            Err(crate::harness::LaunchAttestationError::ProcessChanged)
        })
        .unwrap_err();
    assert_eq!(
        error,
        crate::harness::LaunchAttestationError::ProcessChanged
    );
    assert_eq!(mismatch_attempts, 1);
}

#[test]
fn cleanup_review_cortana_prepublication_retires_only_new_identity() {
    let ctx = test_ctx("cortana-prepublication-identity");
    let newly_minted = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    retire_new_unreserved_cortana_identity(&ctx, &newly_minted.id, true).unwrap();
    assert!(ctx.identity.get(&newly_minted.id).is_none());

    let reused = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    retire_new_unreserved_cortana_identity(&ctx, &reused.id, false).unwrap();
    assert!(ctx.identity.get(&reused.id).is_some());
}

fn modeled_codex_tool_approval(command: &str, tool: &str) -> &'static str {
    let argv = shell_words::split(command).unwrap();
    let expected = format!("mcp_servers.t-hub.tools.{tool}.approval_mode=\"approve\"");
    let approved = argv
        .windows(2)
        .filter(|pair| matches!(pair[0].as_str(), "-c" | "--config"))
        .any(|pair| pair[1] == expected);
    if approved {
        "approve"
    } else {
        "prompt"
    }
}

#[test]
fn cortana_retryable_managed_evidence_never_enters_quarantine_branch() {
    let timeout = crate::tmux::TmuxError {
        op: "trusted-python",
        code: None,
        io_kind: Some(std::io::ErrorKind::TimedOut),
        message: "bounded WSL observation timed out".into(),
    };
    let timeout_error =
        cortana_tmux_observation_error("active Cortana managed owner changed", timeout);
    assert!(is_retryable_error(&timeout_error));
    assert_eq!(
        separate_retryable_cortana_observation::<()>(Err(timeout_error.clone())),
        Err(timeout_error.clone()),
        "a WSL/tmux timeout must return before authority revocation"
    );
    let timeout_response = ControlResponse::err(timeout_error);
    assert!(timeout_response.retryable);

    let indeterminate = crate::tmux::TmuxError {
        op: "retire-managed-runtime",
        code: None,
        io_kind: Some(std::io::ErrorKind::WouldBlock),
        message: "tmux generation liveness is indeterminate before retirement".into(),
    };
    let indeterminate_error = cortana_tmux_observation_error(
        "managed owner for gone terminal remains populated or unverifiable",
        indeterminate,
    );
    assert!(is_retryable_error(&indeterminate_error));

    for code in [41, 43, 77, 80, 82, 83, 84, 90, 92, 94, 100, 101, 118] {
        let inconclusive_evidence = crate::tmux::TmuxError {
            op: "observe-managed-runtime-owner",
            code: Some(code),
            io_kind: Some(std::io::ErrorKind::WouldBlock),
            message: "managed runtime evidence was unreadable".into(),
        };
        let observation_error = cortana_tmux_observation_error(
            "prepared launch effect ownership is unverifiable",
            inconclusive_evidence,
        );
        assert!(ControlResponse::err(observation_error).retryable);
    }

    let unreadable_error = cortana_harness_observation_error(
        "active Cortana Harness attestation failed",
        crate::harness::LaunchAttestationError::UnreadableEvidence,
    );
    assert!(is_retryable_error(&unreadable_error));
    assert_eq!(
        separate_retryable_cortana_observation::<()>(Err(unreadable_error.clone())),
        Err(unreadable_error),
        "temporarily unreadable process evidence must return before quarantine"
    );

    let definitive = cortana_harness_observation_error(
        "active Cortana Harness attestation failed",
        crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch,
    );
    assert!(!is_retryable_error(&definitive));
    assert_eq!(
        separate_retryable_cortana_observation::<()>(Err(definitive.clone())),
        Ok(Err(definitive)),
        "positive mismatch evidence must remain available to quarantine"
    );
}

#[test]
fn reconcile_cortana_is_idempotent_and_recovers_the_same_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-control")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4242".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let first = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-startup-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(first["action"], "create");
    assert_eq!(first["healthy"], true);
    assert_eq!(first["generation"], 1);
    let first_terminal = first["terminalId"].as_str().unwrap().to_string();
    let identity_id = first["identityId"].as_str().unwrap().to_string();

    let second = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-startup-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(second["action"], "keep");
    assert_eq!(second["terminalId"], first_terminal);
    assert_eq!(second["identityId"], identity_id);
    assert_eq!(
        ctx.captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );

    reap_test_tmux_session_and_assert_absent(&tmux_target(&first_terminal));
    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-startup-2",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["generation"], 2);
    assert_eq!(recovered["identityId"], identity_id);
    assert_ne!(recovered["terminalId"], first_terminal);
    let recovered_terminal = recovered["terminalId"].as_str().unwrap();
    assert!(tmux::has_session(&tmux_target(recovered_terminal)));

    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": recovered_terminal }),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
}

/// A durable `cortana.identity_id` pointing at an identity the store no longer
/// HOLDS must self-heal, not wedge. The load-time GC (`prune_dead_generation`,
/// wired in lib.rs setup) retires every identity whose session tile is gone -
/// exactly what a restart after Cortana's tmux session died leaves behind -
/// while captains.json keeps referencing the pruned id. Erroring on that made
/// the state PERMANENT: nothing else rewrites `cortana.identity_id`, so every
/// 30s reconcile failed identically and the UI banner never cleared. A REVOKED
/// id is different: revocation is a deliberate burn with a durable tombstone, so
/// it must keep failing closed rather than silently re-minting past it.
#[test]
fn reconcile_cortana_remints_a_pruned_durable_identity_but_not_a_revoked_one() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-durable-identity-gc")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4242".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-durable-identity-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");

    let created = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-gc-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(created["action"], "create");
    let created_terminal = created["terminalId"].as_str().unwrap().to_string();
    let pruned_identity = created["identityId"].as_str().unwrap().to_string();

    // The restart shape: the runtime is gone, and the load-time GC has already
    // retired its identity while the durable record still names it.
    reap_test_tmux_session_and_assert_absent(&tmux_target(&created_terminal));
    assert!(ctx.identity.retire(&pruned_identity).unwrap());
    assert!(ctx.identity.get(&pruned_identity).is_none());
    assert!(!ctx.identity.is_revoked(&pruned_identity));
    assert_eq!(
        ctx.captains.cortana_identity().identity_id.as_deref(),
        Some(pruned_identity.as_str()),
        "the durable record must still reference the pruned identity"
    );

    let healed = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-gc-2",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(healed["action"], "recover");
    assert_eq!(healed["healthy"], true);
    let healed_identity = healed["identityId"].as_str().unwrap().to_string();
    assert_ne!(
        healed_identity, pruned_identity,
        "a pruned durable identity must be replaced by a freshly minted one"
    );
    // The durable record is rebound, so the next reconcile resolves cleanly
    // instead of re-reading the dead pointer.
    assert_eq!(
        ctx.captains.cortana_identity().identity_id.as_deref(),
        Some(healed_identity.as_str())
    );
    let healed_terminal = healed["terminalId"].as_str().unwrap().to_string();

    // A REVOKED durable identity still fails closed.
    reap_test_tmux_session_and_assert_absent(&tmux_target(&healed_terminal));
    assert!(ctx.identity.revoke(&healed_identity).unwrap());
    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-gc-3",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("is revoked"),
        "a revoked durable identity must fail closed, got: {error}"
    );
    assert_eq!(
        ctx.captains.cortana_identity().identity_id.as_deref(),
        Some(healed_identity.as_str()),
        "a refused reconcile must not rebind the durable record"
    );

    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn cortana_bootstrap_requires_exact_live_authority_and_returns_a_bounded_redacted_snapshot() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-bootstrap")
        .with_live_sessions(|| tmux::list_sessions().map_err(|error| error.to_string()))
        .with_apply_sink(sink.clone());
    ctx.addr = "127.0.0.1:4263".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-bootstrap-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let started = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-bootstrap-start",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    let terminal_id = started["terminalId"].as_str().unwrap().to_string();
    let target = tmux_target(&terminal_id);
    let bearer = tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV)
        .unwrap()
        .unwrap();
    let modeled_launch = cortana_startup_command(
        &crate::cortana_reconcile::CortanaDurableIdentity::default(),
        &json!({}),
        Harness::Codex,
    );
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "cortana_bootstrap"),
        "approve"
    );
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "focus_session"),
        "prompt"
    );
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "spawn_terminal"),
        "prompt"
    );

    for index in (0..20).rev() {
        let ship_slug = format!("ship-{index:02}");
        ctx.captains
            .claim_test(&format!("captain-{index:02}"), Some(&ship_slug), vec![])
            .unwrap();
        ctx.captains
            .checkpoint(
                None,
                Some(&ship_slug),
                None,
                Some(&format!("thread-{index:02}")),
                Some(&"x".repeat(CORTANA_BOOTSTRAP_MAX_TEXT_BYTES + 64)),
            )
            .unwrap();
    }

    let bootstrap = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(bootstrap.ok, "{:?}", bootstrap.error);
    let result = bootstrap.result.unwrap();
    assert_eq!(result["activeCount"], 20);
    assert_eq!(result["returnedCount"], CORTANA_BOOTSTRAP_MAX_SHIPS);
    assert_eq!(result["omittedCount"], 4);
    assert_eq!(result["ships"][0]["shipSlug"], "ship-00");
    assert_eq!(result["ships"][15]["shipSlug"], "ship-15");
    assert_eq!(
        result["ships"][0]["resumePoint"].as_str().unwrap().len(),
        CORTANA_BOOTSTRAP_MAX_TEXT_BYTES
    );
    let encoded = serde_json::to_vec(&result).unwrap();
    assert!(encoded.len() <= CORTANA_BOOTSTRAP_MAX_RESPONSE_BYTES);
    let redacted = String::from_utf8(encoded).unwrap().to_ascii_lowercase();
    for forbidden in [
        "assignment",
        "launchnonce",
        "owner",
        "harnessprocess",
        "argv",
        "sessiontoken",
    ] {
        assert!(!redacted.contains(forbidden), "{forbidden}: {redacted}");
    }
    let effects_before_denials = sink.calls.lock().unwrap().len();
    for (command, args) in [
        ("focus_session", json!({"sessionId": terminal_id.clone()})),
        (
            "spawn_terminal",
            json!({"requestId": "cortana-bootstrap-must-not-spawn"}),
        ),
    ] {
        let denied =
            dispatch_authenticated(&ctx, req_session(&ctx.read_token, &bearer, command, args));
        assert!(!denied.ok, "{command}");
        assert!(denied
            .error
            .as_deref()
            .is_some_and(|error| { error.contains("requires the control capability") }));
    }
    assert_eq!(sink.calls.lock().unwrap().len(), effects_before_denials);

    let crew = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    let denied_crew = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &crew.secret,
            "cortana_bootstrap",
            json!({}),
        ),
    );
    assert!(!denied_crew.ok);

    let ambiguous = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    ctx.identity.bind_tile(&ambiguous.id, &terminal_id).unwrap();
    let denied_ambiguous = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(!denied_ambiguous.ok);
    ctx.identity.retire(&ambiguous.id).unwrap();

    let dead = test_ctx("cortana-bootstrap-dead")
        .with_captains_registry(Arc::clone(&ctx.captains))
        .with_identity_store(Arc::clone(&ctx.identity))
        .with_live_sessions(|| Ok(Vec::new()));
    let denied_dead = dispatch_authenticated(
        &dead,
        req_session(&dead.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(!denied_dead.ok);
    let denied_missing = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, "", "cortana_bootstrap", json!({})),
    );
    assert!(!denied_missing.ok);

    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "cortana-bootstrap-response-built",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let raced = std::thread::scope(|scope| {
        let request_ctx = ctx.clone();
        let request_bearer = bearer.clone();
        let request = scope.spawn(move || {
            dispatch_authenticated(
                &request_ctx,
                req_session(
                    &request_ctx.read_token,
                    &request_bearer,
                    "cortana_bootstrap",
                    json!({}),
                ),
            )
        });
        assert_eq!(
            reached_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            "cortana-bootstrap-response-built"
        );
        ctx.captains
            .begin_cortana_recovery("cortana-bootstrap-raced-basis")
            .unwrap();
        resume_tx.send(()).unwrap();
        request.join().unwrap()
    });
    assert!(!raced.ok);
    assert!(raced.error.as_deref().is_some_and(|error| {
        error.contains("not healthy or in an admitted launch phase")
            || error.contains("basis changed")
    }));

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn concurrent_cortana_startup_calls_produce_one_runtime() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-concurrent")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4243".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-concurrent-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let start = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let ctx = ctx.clone();
        let home = home.clone();
        let harness_command = harness_command.clone();
        let start = start.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            dispatch(
                &ctx,
                "reconcile_cortana",
                &json!({
                    "operationId": "cortana-concurrent-startup",
                    "testOrchestratorHome": home,
                    "testStartupCommand": harness_command,
                }),
            )
            .unwrap()
        }));
    }
    start.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results[0]["terminalId"], results[1]["terminalId"]);
    assert_eq!(results[0]["identityId"], results[1]["identityId"]);
    assert_eq!(
        ctx.captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );
    let terminal_id = results[0]["terminalId"].as_str().unwrap();
    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn cortana_attestation_transition_retries_are_bounded() {
    let ctx = test_ctx("cortana-transition-budget");
    let error = reconcile_cortana_with_transition_count(&ctx, &json!({}), true, 7)
        .expect_err("an exhausted attestation transition budget must fail closed");
    assert!(
        error.contains("did not advance after 6 transitions"),
        "{error}"
    );
}

#[test]
fn cortana_startup_budget_covers_atomic_windows_observation_contract() {
    const MEASURED_WSL_HELPER_LATENCY: Duration = Duration::from_millis(1_100);
    const LEGACY_WINDOWS_HELPERS_PER_OBSERVATION: usize = 2;
    const LEGACY_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

    let observations = CORTANA_HARNESS_REQUIRED_CONFIRMATIONS + 1;
    let legacy_measured_floor = MEASURED_WSL_HELPER_LATENCY
        * (observations * LEGACY_WINDOWS_HELPERS_PER_OBSERVATION) as u32
        + CORTANA_HARNESS_CONFIRM_INTERVAL * (observations - 1) as u32;
    assert!(
        legacy_measured_floor > LEGACY_STARTUP_TIMEOUT,
        "the measured two-helper contract must reproduce the five-second startup failure"
    );

    let atomic_measured_floor = MEASURED_WSL_HELPER_LATENCY
        * (observations * crate::harness::WINDOWS_SCOPED_HARNESS_HELPERS_PER_OBSERVATION) as u32
        + CORTANA_HARNESS_CONFIRM_INTERVAL * (observations - 1) as u32;
    assert!(atomic_measured_floor < CORTANA_HARNESS_STARTUP_TIMEOUT);

    let bounded_cold_start_contract = crate::harness::SCOPED_HARNESS_SINGLE_HELPER_TIMEOUT
        * observations as u32
        + CORTANA_HARNESS_CONFIRM_INTERVAL * (observations - 1) as u32;
    assert!(
        bounded_cold_start_contract < CORTANA_HARNESS_STARTUP_TIMEOUT,
        "the hard startup budget must contain baseline plus two maximally bounded observations"
    );
    assert_eq!(
        crate::harness::WINDOWS_SCOPED_HARNESS_HELPERS_PER_OBSERVATION,
        1
    );
}

#[test]
fn cortana_startup_prompt_and_resume_use_the_dedicated_bootstrap_policy() {
    let durable = crate::cortana_reconcile::CortanaDurableIdentity::default();
    let fresh = cortana_startup_command(&durable, &json!({}), Harness::Codex);
    assert!(fresh.contains("First call cortana_bootstrap"));
    assert!(!fresh.contains("captain_bootstrap"));
    assert!(fresh.contains("--sandbox read-only"));
    assert!(fresh.contains(crate::harness::CORTANA_CODEX_TOOL_APPROVAL_OVERRIDE));

    let resumed = cortana_startup_command(
        &crate::cortana_reconcile::CortanaDurableIdentity {
            provider_session_id: Some("thread-cortana".into()),
            ..Default::default()
        },
        &json!({}),
        Harness::Codex,
    );
    assert_eq!(
            resumed,
            "codex resume --sandbox read-only -c 'mcp_servers.t-hub.tools.cortana_bootstrap.approval_mode=\"approve\"' 'thread-cortana'"
        );
}
