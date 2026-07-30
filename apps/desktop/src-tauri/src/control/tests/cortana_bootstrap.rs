//! The Cortana singleton: reattach-or-create, and the bootstrap an agent running
//! inside the shell uses to introduce itself.
//!
//! These cases replace ~5,400 lines that covered managed launches, generation
//! ladders, runtime discovery and quarantine planning. None of that exists any
//! more (see `control/cortana.rs`); what is left to prove is that exactly one
//! shell exists, that it is reattached rather than duplicated, and that the
//! identity self-heal still fails closed on a revoked credential.

use super::*;

/// The whole test fixture: a control context with the Captain Workspace present
/// and an apply sink connected, plus a scratch orchestrator home.
struct CortanaFixture {
    ctx: ControlContext,
    home: std::path::PathBuf,
    sink: Arc<RecordingSink>,
}

impl CortanaFixture {
    fn new(name: &str) -> Self {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let mut ctx = test_ctx(name)
            .with_live_sessions(|| Ok(Vec::new()))
            .with_apply_sink(Arc::clone(&sink) as Arc<dyn ApplySink>);
        ctx.addr = "127.0.0.1:4242".into();
        ctx.tab_registry().replace(vec![TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        }]);
        let home = std::env::temp_dir().join(format!(
            "t-hub-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        Self { ctx, home, sink }
    }

    /// Every `(command, args)` the UI was asked to apply.
    fn applied(&self) -> Vec<(String, Value)> {
        self.sink.calls.lock().unwrap().clone()
    }

    fn reconcile(&self, operation_id: &str) -> Result<Value, String> {
        dispatch(
            &self.ctx,
            "reconcile_cortana",
            &json!({
                "operationId": operation_id,
                "testOrchestratorHome": self.home,
            }),
        )
    }

    fn live_cortana_sessions(&self) -> Vec<String> {
        let recorded = self.ctx.captains.cortana_identity().terminal_id;
        tmux::list_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|session| {
                recorded
                    .as_deref()
                    .is_some_and(|terminal_id| session == &tmux_target(terminal_id))
            })
            .collect()
    }
}

impl Drop for CortanaFixture {
    fn drop(&mut self) {
        if let Some(terminal_id) = self.ctx.captains.cortana_identity().terminal_id {
            let _ = tmux::kill_session_tree(&tmux_target(&terminal_id));
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Create when nothing exists, adopt when the recorded session is alive, and
/// create exactly one replacement when it is definitively gone. Two reconciles in
/// a row must never produce two shells.
#[test]
fn reconcile_cortana_creates_one_shell_then_reattaches_to_it() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-singleton");

    let created = fixture.reconcile("cortana-startup-1").unwrap();
    assert_eq!(created["action"], "create");
    assert_eq!(created["healthy"], true);
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();
    let identity_id = created["identityId"].as_str().unwrap().to_string();
    assert!(tmux::has_session(&tmux_target(&terminal_id)));
    // The tile is durably recorded AND placed in the reserved workspace.
    assert_eq!(
        fixture
            .ctx
            .captains
            .cortana_identity()
            .terminal_id
            .as_deref(),
        Some(terminal_id.as_str())
    );
    assert_eq!(
        fixture.ctx.tabs.workspace_for_tile(&terminal_id).as_deref(),
        Some(CAPTAIN_WORKSPACE_ID)
    );
    // No agent is started: the pane runs a plain login shell, so nothing claims
    // the Fleet role until a user starts something in it.
    assert_eq!(
        fixture
            .ctx
            .captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        0
    );

    let adopted = fixture.reconcile("cortana-startup-1").unwrap();
    assert_eq!(adopted["action"], "adopt");
    assert_eq!(adopted["terminalId"], terminal_id);
    assert_eq!(adopted["identityId"], identity_id);
    assert_eq!(fixture.live_cortana_sessions().len(), 1);

    // A DIFFERENT operation id must still adopt rather than create a rival.
    let adopted_again = fixture.reconcile("cortana-startup-2").unwrap();
    assert_eq!(adopted_again["action"], "adopt");
    assert_eq!(adopted_again["terminalId"], terminal_id);

    // Definitively gone: exactly one replacement, not one per attempt.
    reap_test_tmux_session_and_assert_absent(&tmux_target(&terminal_id));
    let replaced = fixture.reconcile("cortana-startup-3").unwrap();
    assert_eq!(replaced["action"], "create");
    let replacement = replaced["terminalId"].as_str().unwrap().to_string();
    assert_ne!(replacement, terminal_id);
    let steady = fixture.reconcile("cortana-startup-4").unwrap();
    assert_eq!(steady["action"], "adopt");
    assert_eq!(steady["terminalId"], replacement);
    assert_eq!(fixture.live_cortana_sessions().len(), 1);
}

/// Creating the shell must MATERIALIZE its tile in the UI, not just move it.
///
/// The webview seeds its terminal map from `list_terminals` once at Canvas mount
/// and never adds terminals afterwards - the 15s poll only refreshes metadata for
/// ones it already has. The singleton is created just after that mount, so a
/// `move_tile` for a terminal the UI has no record of is a no-op: Cortana existed
/// in tmux and in the authoritative registry but was invisible until a reload,
/// which is exactly what happened on the 0.3.154 install.
#[test]
fn creating_the_shell_forwards_a_spawn_the_ui_can_render() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-tile-forward");
    let created = fixture.reconcile("cortana-tile-1").unwrap();
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();

    let spawns = fixture
        .applied()
        .into_iter()
        .filter(|(command, _)| command == "spawn_terminal")
        .collect::<Vec<_>>();
    assert_eq!(spawns.len(), 1, "exactly one spawn forward: {spawns:?}");
    let (_, args) = &spawns[0];
    assert_eq!(args["id"], terminal_id);
    assert_eq!(args["tabId"], CAPTAIN_WORKSPACE_ID);
    assert_eq!(args["name"], "Cortana");
    assert_eq!(args["tmuxSession"], tmux_target(&terminal_id));
    assert_eq!(
        args["cwd"].as_str().unwrap(),
        fixture.home.to_string_lossy(),
        "the tile must report the orchestrator home"
    );
    // The forward carries the tab registry snapshot, so the UI places the tile in
    // the same transaction rather than needing a follow-up sync.
    assert_eq!(args["sync"]["tabs"][0]["tileIds"][0], terminal_id, "{args}");

    // Reattaching does NOT forward another spawn: the surviving session is
    // already in the mount-time inventory, and a second spawn for a tile the UI
    // holds would be a duplicate rather than a repair.
    let adopted = fixture.reconcile("cortana-tile-1").unwrap();
    assert_eq!(adopted["action"], "adopt");
    assert_eq!(
        fixture
            .applied()
            .into_iter()
            .filter(|(command, _)| command == "spawn_terminal")
            .count(),
        1
    );
}

/// The reserved workspace must ACCEPT the recorded singleton before any agent
/// has claimed it.
///
/// This is what a 0.3.154 restart actually did: the reconcile adopted the shell
/// and placed its tile in Captain Workspace, and then every layout report the UI
/// sent back was refused with "terminal '...' is not a durable Cortana or Captain
/// identity" - four times in five seconds - because the occupant check only
/// recognized an ACTIVE Fleet claim, and nothing auto-starts an agent to make one.
#[test]
fn the_reserved_workspace_accepts_the_recorded_singleton_without_a_claim() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-placement");
    let created = fixture.reconcile("cortana-placement-1").unwrap();
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();
    assert_eq!(
        fixture
            .ctx
            .captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        0,
        "the shell must have NO Fleet claim - that is the whole point"
    );

    let reported = vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![terminal_id.clone()],
    }];
    validate_workspace_report(&reported, &fixture.ctx.captains)
        .expect("the recorded singleton belongs in the reserved workspace");

    // ...and the singleton is refused a WORK workspace, exactly as a claimed
    // supervisor is. Without this the tile drifts out of the reserved workspace:
    // the UI seeded the adopted shell into the user's own work tab and, once the
    // Captain arm above started accepting, the server kept that placement.
    let drifted = vec![TabRecord {
        id: "work-tab".into(),
        name: "thub".into(),
        tile_ids: vec![terminal_id.clone()],
    }];
    let error = validate_workspace_report(&drifted, &fixture.ctx.captains)
        .expect_err("the singleton belongs in the reserved workspace");
    assert!(error.contains("belongs to Captain Workspace"), "{error}");

    // A terminal the durable record does NOT name is still refused there.
    let foreign = vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec!["deadbeef".into()],
    }];
    let error = validate_workspace_report(&foreign, &fixture.ctx.captains)
        .expect_err("a foreign tile must stay out of the reserved workspace");
    assert!(
        error.contains("not a durable Cortana or Captain identity"),
        "{error}"
    );
}

/// An `Unknown` liveness probe is a degraded control plane, not an absent
/// session. Treating it as absent is precisely how a second shell gets created
/// for a session that is in fact alive, so it must fail RETRYABLE instead.
#[test]
fn uncertain_tmux_evidence_never_creates_a_second_shell() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-uncertain");
    let created = fixture.reconcile("cortana-uncertain-1").unwrap();
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();

    tmux::force_next_session_liveness_unknown_for(&tmux_target(&terminal_id));
    let error = fixture
        .reconcile("cortana-uncertain-2")
        .expect_err("an ambiguous probe must not be treated as a dead session");
    assert!(
        is_retryable_error(&error),
        "an ambiguous probe must be retryable, got: {error}"
    );
    assert_eq!(
        fixture
            .ctx
            .captains
            .cortana_identity()
            .terminal_id
            .as_deref(),
        Some(terminal_id.as_str()),
        "a refused reconcile must not rewrite the durable terminal"
    );
    assert_eq!(fixture.live_cortana_sessions().len(), 1);
}

/// Reattaching refreshes the session's control environment. The endpoint and the
/// session token rotate on every app start while a surviving tmux session keeps
/// the environment it was created with, which is why a restarted app used to come
/// back to a Cortana holding a stale (or retired) credential.
#[test]
fn reattaching_refreshes_the_control_environment() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-env-refresh");
    let created = fixture.reconcile("cortana-env-1").unwrap();
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();
    let target = tmux_target(&terminal_id);
    let original = tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV)
        .unwrap()
        .unwrap();

    // The restart shape: the identity behind the live session was pruned, so the
    // reconcile re-mints and must push the NEW secret into the live session.
    let identity_id = created["identityId"].as_str().unwrap().to_string();
    assert!(fixture.ctx.identity.retire(&identity_id).unwrap());
    let adopted = fixture.reconcile("cortana-env-2").unwrap();
    assert_eq!(adopted["action"], "adopt");
    assert_eq!(adopted["terminalId"], terminal_id);

    let refreshed = tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV)
        .unwrap()
        .unwrap();
    assert_ne!(refreshed, original, "the session token must be re-injected");
    assert_eq!(
        fixture.ctx.identity.resolve(&refreshed).map(|id| id.id),
        adopted["identityId"].as_str().map(str::to_string),
        "the refreshed token must resolve to the adopted identity"
    );
    assert_eq!(
        tmux::session_environment(&target, "T_HUB_CONTROL_FILE")
            .unwrap()
            .as_deref(),
        Some(discovery_file_for_spawn().as_str())
    );
}

/// A durable `cortana.identity_id` pointing at an identity the store no longer
/// HOLDS must self-heal, not wedge. The load-time GC (`prune_dead_generation`,
/// wired in lib.rs setup) retires every identity whose session tile is gone -
/// exactly what a restart after Cortana's tmux session died leaves behind -
/// while captains.json keeps referencing the pruned id. Erroring on that made the
/// state PERMANENT: nothing else rewrites `cortana.identity_id`, so every 30s
/// reconcile failed identically and the UI banner never cleared. A REVOKED id is
/// different: revocation is a deliberate burn with a durable tombstone, so it
/// must keep failing closed rather than silently re-minting past it.
#[test]
fn reconcile_cortana_remints_a_pruned_durable_identity_but_not_a_revoked_one() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-durable-identity-gc");

    let created = fixture.reconcile("cortana-gc-1").unwrap();
    assert_eq!(created["action"], "create");
    let created_terminal = created["terminalId"].as_str().unwrap().to_string();
    let pruned_identity = created["identityId"].as_str().unwrap().to_string();

    // The restart shape: the runtime is gone, and the load-time GC has already
    // retired its identity while the durable record still names it.
    reap_test_tmux_session_and_assert_absent(&tmux_target(&created_terminal));
    assert!(fixture.ctx.identity.retire(&pruned_identity).unwrap());
    assert!(fixture.ctx.identity.get(&pruned_identity).is_none());
    assert!(!fixture.ctx.identity.is_revoked(&pruned_identity));
    assert_eq!(
        fixture
            .ctx
            .captains
            .cortana_identity()
            .identity_id
            .as_deref(),
        Some(pruned_identity.as_str()),
        "the durable record must still reference the pruned identity"
    );

    let healed = fixture.reconcile("cortana-gc-2").unwrap();
    assert_eq!(healed["action"], "create");
    assert_eq!(healed["healthy"], true);
    let healed_identity = healed["identityId"].as_str().unwrap().to_string();
    assert_ne!(
        healed_identity, pruned_identity,
        "a pruned durable identity must be replaced by a freshly minted one"
    );
    assert_eq!(
        fixture
            .ctx
            .captains
            .cortana_identity()
            .identity_id
            .as_deref(),
        Some(healed_identity.as_str())
    );
    let healed_terminal = healed["terminalId"].as_str().unwrap().to_string();

    // A REVOKED durable identity still fails closed.
    reap_test_tmux_session_and_assert_absent(&tmux_target(&healed_terminal));
    assert!(fixture.ctx.identity.revoke(&healed_identity).unwrap());
    let error = fixture
        .reconcile("cortana-gc-3")
        .expect_err("a revoked durable identity must fail closed");
    assert!(error.contains("is revoked"), "{error}");
    assert_eq!(
        fixture
            .ctx
            .captains
            .cortana_identity()
            .identity_id
            .as_deref(),
        Some(healed_identity.as_str()),
        "a refused reconcile must not rebind the durable record"
    );
}

/// Two reconciles racing on startup produce ONE shell. With discovery gone, the
/// single-flight guard plus the durable terminal id are the entire
/// anti-duplication mechanism, so this is the case that proves it.
#[test]
fn concurrent_cortana_startup_calls_produce_one_shell() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-concurrent");
    let start = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let ctx = fixture.ctx.clone();
        let home = fixture.home.clone();
        let start = Arc::clone(&start);
        workers.push(std::thread::spawn(move || {
            start.wait();
            dispatch(
                &ctx,
                "reconcile_cortana",
                &json!({
                    "operationId": "cortana-concurrent-startup",
                    "testOrchestratorHome": home,
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
    let actions = [
        results[0]["action"].as_str().unwrap(),
        results[1]["action"].as_str().unwrap(),
    ];
    assert!(
        actions.contains(&"create") && actions.contains(&"adopt"),
        "one racer creates and the other adopts: {actions:?}"
    );
    assert_eq!(fixture.live_cortana_sessions().len(), 1);
}

/// A record written by the retired discovery machinery - a wedged prepared
/// managed launch, a quarantine ledger, a generation ladder, and a `Recovering`
/// state owned by an operation that will never return - must LOAD and be taken
/// over, not wedge every later reconcile. This is the exact shape found on the
/// reporting machine (generation 16, 15 revoked identities, 3,194 consecutive
/// failures).
#[test]
fn a_wedged_discovery_era_record_is_taken_over_and_cleared() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-legacy-takeover");
    // The literal shape of the record found on the reporting machine, so this
    // also proves the dormant fields still PARSE (the reason they were kept in
    // the struct instead of migrated away).
    let wedged: crate::cortana_reconcile::CortanaDurableIdentity = serde_json::from_value(json!({
        "identityId": "05fe0a0a3e0a484bbf82c9b6b5cc6c2d",
        "generation": 16,
        "terminalId": null,
        "harness": "codex",
        "providerSessionId": null,
        "conversationId": null,
        "checkpoint": null,
        "managedLaunch": {
            "version": 4,
            "operationId": "8a953a74-d4d1-4be9-851d-d7652dca9999",
            "terminalId": "b5d5bac3",
            "tmuxTarget": "th_b5d5bac3",
            "identityId": "05fe0a0a3e0a484bbf82c9b6b5cc6c2d",
            "generation": 17,
            "harness": "codex",
            "unitName": "t-hub-0d2e0782f5b145ee8f87a5ea48b1f2aa.scope",
            "launchNonce": "0d2e0782f5b145ee8f87a5ea48b1f2aa",
            "tools": {
                "python": {"path": "/usr/bin/python3.12", "device": 2096, "inode": 11282},
                "systemctl": {"path": "/usr/bin/systemctl", "device": 2096, "inode": 1776247},
                "systemdRun": {"path": "/usr/bin/systemd-run", "device": 2096, "inode": 1776268}
            },
            "phase": "prepared"
        },
        "recovery": {
            "kind": "recovering",
            "operation_id": "8a953a74-d4d1-4be9-851d-d7652dca9999",
            "started_at": 1_785_363_588_792_u64
        }
    }))
    .expect("a discovery-era captains.json must still parse");
    assert!(wedged.managed_launch.is_some());
    fixture.ctx.captains.set_cortana_for_test(wedged);

    let recovered = fixture.reconcile("cortana-legacy-1").unwrap();
    assert_eq!(recovered["action"], "create");
    assert_eq!(recovered["healthy"], true);
    let durable = fixture.ctx.captains.cortana_identity();
    assert!(durable.terminal_id.is_some());
    assert!(
        durable.managed_launch.is_none()
            && durable.owner.is_none()
            && durable.quarantine_ledger.is_empty(),
        "the discovery-era fields must be cleared, not merely ignored: {durable:?}"
    );
    assert!(matches!(
        durable.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
}

/// The agent the USER starts in the shell is what publishes the Fleet claim, so
/// the recorded singleton must be able to claim the Cortana role for its OWN
/// terminal. Nothing else may: not a foreign Cortana-role bearer, and not the
/// singleton pointing at someone else's terminal.
#[test]
fn the_recorded_singleton_may_claim_the_cortana_role_for_its_own_terminal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture = CortanaFixture::new("cortana-self-claim");
    let created = fixture.reconcile("cortana-self-claim-1").unwrap();
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();
    let bearer = tmux::session_environment(
        &tmux_target(&terminal_id),
        crate::identity::SESSION_TOKEN_ENV,
    )
    .unwrap()
    .unwrap();
    let caller = resolve_identity(&fixture.ctx, &bearer).unwrap();

    assert!(enforce_attach_authority(
        &fixture.ctx,
        Some(&caller),
        false,
        &terminal_id,
        FleetRole::Cortana,
    )
    .is_ok());
    // Not someone else's terminal, even for the singleton.
    assert!(enforce_attach_authority(
        &fixture.ctx,
        Some(&caller),
        false,
        "other-tile",
        FleetRole::Cortana,
    )
    .unwrap_err()
    .contains("only General/Cortana"));

    // A Cortana-role identity the durable record does not name is not the
    // singleton, whatever it is bound to.
    let impostor = fixture
        .ctx
        .identity
        .mint(crate::identity::Role::Cortana)
        .unwrap();
    fixture
        .ctx
        .identity
        .bind_tile(&impostor.id, &terminal_id)
        .unwrap();
    let impostor_caller = resolve_identity(&fixture.ctx, &impostor.secret).unwrap();
    assert!(enforce_attach_authority(
        &fixture.ctx,
        Some(&impostor_caller),
        false,
        &terminal_id,
        FleetRole::Cortana,
    )
    .unwrap_err()
    .contains("only General/Cortana"));
}

/// The skill tells the orchestrator agent to claim on every session start, so a
/// repeat claim of the SAME terminal must be a refresh, not a refusal - otherwise
/// a resumed agent would be told the crown is taken by itself.
#[test]
fn reclaiming_the_cortana_role_on_the_same_terminal_is_idempotent() {
    let registry = CaptainsRegistry::new();
    let first = registry
        .claim(
            "cort0001",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .expect("first cortana claim");
    let second = registry
        .claim(
            "cort0001",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .expect("a repeat claim of the same terminal must be admitted");
    assert_eq!(second.record.terminal_id, first.record.terminal_id);
    assert_eq!(second.record.role, FleetRole::Cortana);
    assert_eq!(
        registry
            .snapshot()
            .captains
            .iter()
            .filter(|c| c.role == FleetRole::Cortana)
            .count(),
        1,
        "a repeat claim must not create a second Cortana"
    );
}

fn modeled_codex_tool_approval(command: &str, tool: &str) -> &'static str {
    let override_flag = format!("mcp_servers.t-hub.tools.{tool}.approval_mode=");
    match command.split(&override_flag).nth(1) {
        Some(rest) if rest.starts_with("\"approve\"") => "approve",
        Some(rest) if rest.starts_with("\"never\"") => "never",
        _ => "prompt",
    }
}

/// `cortana_bootstrap` is how an agent the USER started in the shell introduces
/// itself. The authorization is the durable record plus the live terminal
/// binding, and the response stays bounded and redacted.
#[test]
fn cortana_bootstrap_requires_the_recorded_singleton_and_returns_a_bounded_redacted_snapshot() {
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
    let started = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-bootstrap-start",
            "testOrchestratorHome": home,
        }),
    )
    .unwrap();
    let terminal_id = started["terminalId"].as_str().unwrap().to_string();
    let target = tmux_target(&terminal_id);
    let bearer = tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV)
        .unwrap()
        .unwrap();

    // The bootstrap tool stays pre-approved in the Codex launch policy the user's
    // own `codex` invocation inherits, while spawning stays a prompt.
    let modeled_launch = crate::harness::Harness::Codex
        .adapter()
        .fresh_cortana_argv("bootstrap");
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "cortana_bootstrap"),
        "approve"
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
    assert_eq!(
        result["ships"][0]["resumePoint"].as_str().unwrap().len(),
        CORTANA_BOOTSTRAP_MAX_TEXT_BYTES
    );
    let encoded = serde_json::to_vec(&result).unwrap();
    assert!(encoded.len() <= CORTANA_BOOTSTRAP_MAX_RESPONSE_BYTES);
    let redacted = String::from_utf8(encoded).unwrap().to_ascii_lowercase();
    for forbidden in ["assignment", "launchnonce", "owner", "argv", "sessiontoken"] {
        assert!(!redacted.contains(forbidden), "{forbidden}: {redacted}");
    }

    // The bootstrap bearer is read-tier: it introduces the agent, it does not
    // grant it the control capability.
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

    // A crew bearer is not the singleton.
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

    // A SECOND Cortana-role identity bound to the same tile is ambiguous.
    let ambiguous = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    ctx.identity.bind_tile(&ambiguous.id, &terminal_id).unwrap();
    let denied_ambiguous = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(!denied_ambiguous.ok);
    ctx.identity.retire(&ambiguous.id).unwrap();

    // A dead terminal cannot bootstrap, and neither can a missing bearer.
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

    let _ = tmux::kill_session_tree(&target);
    let _ = std::fs::remove_dir_all(home);
}
