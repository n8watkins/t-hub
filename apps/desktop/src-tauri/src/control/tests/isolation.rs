use super::*;

#[test]
fn cross_ship_isolation_refuses_a_foreign_read_through_the_gate() {
    // MANDATED cross-ship-isolation guard: a crew on ship-a may NOT read another
    // ship's pane. BYPASS-WOULD-FAIL: remove `enforce_session_access` from
    // `read_terminal` and the foreign read proceeds to tmux (a different, non-acl
    // error) - this assert (the isolation reason) goes RED.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg);

    let foreign = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "read_terminal",
            json!({"sessionId": "crew-b"}),
        ),
    );
    assert!(!foreign.ok, "a foreign read must be refused");
    let foreign_error = foreign.error.unwrap();
    assert!(
        foreign_error.contains("cross-ship isolation"),
        "the refusal must be the isolation ACL, not a downstream tmux error: {foreign_error}"
    );

    // A trusted in-process host fails open - it is not refused by
    // the ACL (it errors later at the tmux capture, which is a different message).
    let host = dispatch_authenticated(
        &ctx,
        req("ctrl", "read_terminal", json!({"sessionId": "crew-b"})),
    );
    assert!(
        !host
            .error
            .unwrap_or_default()
            .contains("cross-ship isolation"),
        "the trusted host must fail open (NORM-now), not be ACL-refused"
    );
}

#[test]
fn full_token_without_host_provenance_cannot_reach_identity_sensitive_handlers() {
    let ctx = test_ctx("ctrl");
    let cases = [
        ("read_terminal", json!({"sessionId": "target"})),
        ("send_text", json!({"sessionId": "target", "text": "x"})),
        (
            "send_keys",
            json!({"sessionId": "target", "keys": ["Escape"]}),
        ),
        ("abort_session", json!({"sessionId": "target"})),
        ("plane_admin", json!({"op": "purge", "recipient": "target"})),
        ("plane_send", json!({"recipient": "target", "text": "x"})),
        ("inbox_ack", json!({"sessionId": "target", "seq": 0})),
        ("history_list", json!({"limit": 10})),
        ("history_focus", json!({"historyId": "history:v1:target"})),
        (
            "history_resume",
            json!({
                "historyId": "history:v1:target",
                "requestId": "history-provenance",
                "targetTabId": "target"
            }),
        ),
    ];

    for (command, args) in cases {
        for session in ["", "invalid-session-token"] {
            let response =
                dispatch_authenticated(&ctx, req_untrusted("ctrl", session, command, args.clone()));
            assert!(!response.ok, "{command} accepted omitted identity");
            assert!(
                response
                    .error
                    .unwrap_or_default()
                    .contains("requires a valid T_HUB_SESSION_TOKEN"),
                "{command} did not fail at the provenance boundary"
            );
        }
    }
}

#[test]
fn untrusted_full_mutations_require_identity_and_audit_omitted_or_invalid_tokens() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let audit_dir = std::env::temp_dir().join(format!("t-hub-identity-gate-{nonce}"));
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let general_secret = mint_session(
        &store,
        crate::identity::Role::General,
        "fleet",
        "general-gate",
    );
    let ctx = test_ctx("identity-gate")
        .with_identity_store(store)
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));

    for (command, args) in [
        ("new_tab", json!({"name": "must-not-exist"})),
        (
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "must-not-spawn"}),
        ),
    ] {
        for session in ["", "invalid-nonempty-session-token"] {
            let response = dispatch_authenticated(
                &ctx,
                req_untrusted("identity-gate", session, command, args.clone()),
            );
            assert!(
                !response.ok,
                "{command} accepted an unidentified Full bearer"
            );
            assert!(response
                .error
                .unwrap_or_default()
                .contains("requires a valid T_HUB_SESSION_TOKEN"));
        }
    }
    assert!(ctx.tabs.id_for_name("must-not-exist").is_none());
    let records = read_audit(&audit_dir);
    assert_eq!(records.len(), 4);
    assert!(records
        .iter()
        .all(|record| record["decision"] == "refused-identity"));
    assert!(records
        .iter()
        .all(|record| record["tokenTier"] == "control"));

    let identified = dispatch_authenticated(
        &ctx,
        req_session(
            "identity-gate",
            &general_secret,
            "new_tab",
            json!({"name": "identified"}),
        ),
    );
    assert!(
        identified.ok,
        "identified Full mutation failed: {:?}",
        identified.error
    );

    let trusted = dispatch_authenticated(
        &ctx,
        req("identity-gate", "new_tab", json!({"name": "trusted-host"})),
    );
    assert!(
        trusted.ok,
        "trusted host mutation failed: {:?}",
        trusted.error
    );
    std::fs::remove_dir_all(audit_dir).ok();
}

#[test]
fn captain_lifecycle_authority_is_enforced_through_authenticated_dispatch() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let general = mint_session(&store, crate::identity::Role::General, "fleet", "general");
    let promoted = mint_session(&store, crate::identity::Role::Crew, "pending", "new-cap");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg);

    let promoted = resolve_identity(&ctx, &promoted).unwrap();
    assert!(
        enforce_attach_authority(&ctx, Some(&promoted), false, "new-cap", FleetRole::Captain,)
            .is_ok()
    );
    assert!(
        enforce_attach_authority(&ctx, Some(&promoted), false, "other", FleetRole::Captain,)
            .unwrap_err()
            .contains("attach a different terminal")
    );

    let foreign = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "release_captain",
            json!({"shipSlug": "ship-b"}),
        ),
    );
    assert!(!foreign.ok);
    assert!(foreign.error.unwrap().contains("same ship"));
    assert!(ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|record| record.ship_slug == "ship-b"));

    let own = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "release_captain",
            json!({"shipSlug": "ship-a"}),
        ),
    );
    assert!(own.ok, "same-ship release failed: {:?}", own.error);

    let apex = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &general,
            "release_captain",
            json!({"shipSlug": "ship-b"}),
        ),
    );
    assert!(apex.ok, "General release failed: {:?}", apex.error);
}

#[test]
fn full_socket_token_without_session_identity_has_no_lifecycle_authority() {
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    let ctx = test_ctx("ctrl").with_captains_registry(reg);

    for session in ["", "invalid-session-secret"] {
        let response = dispatch_authenticated(
            &ctx,
            ControlRequest {
                token: "ctrl".into(),
                command: "release_captain".into(),
                args: json!({"shipSlug": "ship-a"}),
                session: session.into(),
                host: String::new(),
                v: None,
            },
        );
        assert!(!response.ok);
        assert!(response
            .error
            .unwrap_or_default()
            .contains("requires a valid T_HUB_SESSION_TOKEN"));
    }
    assert_eq!(ctx.captains.snapshot().captains.len(), 1);
}

#[test]
fn crew_cannot_self_assign_the_reserved_cortana_role_or_slug() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let crew = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl").with_identity_store(store);

    for args in [
        json!({"captainSessionId": "crew-a", "role": "cortana"}),
        json!({"captainSessionId": "crew-a", "shipSlug": "cortana"}),
    ] {
        let response =
            dispatch_authenticated(&ctx, req_session("ctrl", &crew, "claim_captain", args));
        assert!(!response.ok);
        assert!(response
            .error
            .unwrap_or_default()
            .contains("General/Cortana"));
    }
    assert!(ctx.captains.snapshot().captains.is_empty());
}

#[test]
fn captain_cannot_close_foreign_crew() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    reg.bind_crew_context(
        "cap-b",
        "crew-b",
        "foreign task",
        "codex",
        None,
        None,
        PowderWorkBinding {
            card_id: "card-b".into(),
            run_id: "run-b".into(),
            agent: None,
            claim_expires_at: None,
            mutation_intent: None,
            dispatch_release_recovery: false,
            state: PowderWorkState::Active,
        },
    )
    .unwrap();
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg);

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "close_terminal",
            json!({"sessionId": "crew-b"}),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap_or_default();
    assert!(error.starts_with("acl:"), "got: {error}");
}

#[test]
fn read_terminal_ownership_matrix_through_the_gate() {
    // The full DoD ownership matrix for `read_terminal`, exercised END-TO-END through
    // `dispatch_authenticated` (session-token resolve -> `enforce_session_access` ->
    // `can_access_session`). The sibling `cross_ship_isolation_refuses_a_foreign_read_
    // through_the_gate` test is the bypass-would-fail sentinel (drop the guard and the
    // foreign-crew cell flips to a non-ACL error); THIS test proves the ALLOW cells go
    // through and the orchestrator cells resolve correctly.
    //
    // An ALLOWED read cannot fully succeed in the headless test env (there is no live
    // `th_*` tmux session), so it fails at the tmux capture with a DIFFERENT message.
    // The invariant for an allow cell is therefore: NOT refused with the isolation ACL
    // reason. A DENIED cell must carry "cross-ship isolation".
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    assert!(reg.record_crew("cap-a", "crew-a").unwrap());
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let cortana = mint_current_cortana_session(&store, &reg, "cor");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg);

    // An allow cell: refused ONLY if the isolation ACL fired (else it fell through to
    // the tmux layer, which is the intended "permitted" outcome).
    let is_isolation_denied = |session: &str, target: &str| -> bool {
        let resp = dispatch_authenticated(
            &ctx,
            req_session(
                "read-t",
                session,
                "read_terminal",
                json!({"sessionId": target}),
            ),
        );
        resp.error
            .unwrap_or_default()
            .contains("cross-ship isolation")
    };

    // SELF: a crew reading its OWN pane -> permitted (falls through to tmux).
    assert!(
        !is_isolation_denied(&crew_a, "crew-a"),
        "self-read must be permitted"
    );
    // OWN-CREW: a captain reading its own ship's crew -> permitted.
    assert!(
        !is_isolation_denied(&cap_a, "crew-a"),
        "captain reading own crew must be permitted"
    );
    // OWN-SHIP SUPERVISOR: a crew reading its own captain's pane -> permitted (same ship).
    assert!(
        !is_isolation_denied(&crew_a, "cap-a"),
        "same-ship supervisor read must be permitted"
    );
    // ORCHESTRATOR: cortana reading a SUPERVISOR on any ship (her subordinate) -> permitted.
    assert!(
        !is_isolation_denied(&cortana, "cap-b"),
        "cortana reading a captain must be permitted"
    );
    // FOREIGN-CREW: a crew reading another ship's crew -> DENIED.
    assert!(
        is_isolation_denied(&crew_a, "crew-b"),
        "cross-ship crew read must be refused"
    );
    // ORCHESTRATOR SKIP-LEVEL: cortana reading a FOREIGN ship's crew directly -> DENIED.
    assert!(
        is_isolation_denied(&cortana, "crew-b"),
        "cortana skip-level into foreign crew must be refused"
    );

    // IN-PROCESS HOST: the local host proof admits a request without a session identity.
    let host = dispatch_authenticated(
        &ctx,
        req("ctrl", "read_terminal", json!({"sessionId": "crew-b"})),
    );
    assert!(
        !host
            .error
            .unwrap_or_default()
            .contains("cross-ship isolation"),
        "the full-token host must fail open, not be ACL-refused"
    );
}

#[test]
fn cross_ship_isolation_refuses_a_foreign_break_glass_write() {
    // The write side of H3: even break-glass `send_text` rides the isolation ACL. A
    // captain on ship-a (holding the Full control token) may not write ship-b's crew.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg);
    let resp = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cap_a,
            "send_text",
            json!({"sessionId": "crew-b", "text": "hi"}),
        ),
    );
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("cross-ship isolation"));
}
