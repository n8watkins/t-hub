use super::*;

#[test]
fn inbox_status_unscoped_enumeration_requires_organization() {
    // item-3 §2.4 (ledger #15): a SCOPED inbox_status (own recipient) stays Read,
    // but an UNSCOPED fleet-wide enumeration (depth_all) is Organization so a bare
    // read token cannot enumerate every recipient's counts/cursors. inbox_ack STAYS
    // Organization regardless (§2.4.1). BYPASS-WOULD-FAIL: drop the effective_tier
    // refinement and the unscoped case falls back to Read and the assert goes RED.
    assert_eq!(
        effective_tier("inbox_status", &json!({"sessionId": "tileX"})),
        CommandTier::Read,
        "a scoped inbox_status is a Read"
    );
    assert_eq!(
        effective_tier("inbox_status", &json!({})),
        CommandTier::Organization,
        "an unscoped inbox_status enumeration must require Organization"
    );
    // inbox_ack is Organization independent of scope (no self-scope until the
    // session-token-on-request substrate lands, §2.4.1).
    assert_eq!(
        effective_tier("inbox_ack", &json!({"sessionId": "tileX"})),
        CommandTier::Organization
    );
    // Every other command's effective tier is exactly its required_tier.
    assert_eq!(
        effective_tier("list_terminals", &json!({})),
        CommandTier::Read
    );
    assert_eq!(
        effective_tier("spawn_terminal", &json!({})),
        CommandTier::ProcessChanging
    );
}

#[test]
fn read_token_cannot_enumerate_all_inboxes_but_can_scope_its_own() {
    // End-to-end through the gate: a read token doing an UNSCOPED inbox_status is
    // authz-refused (Organization), while a SCOPED inbox_status is admitted (Read).
    let ctx = test_ctx("t").with_inbox(Arc::new(crate::inbox::Inbox::ephemeral()));
    let unscoped = dispatch_authenticated(&ctx, req("read-t", "inbox_status", json!({})));
    assert!(
        unscoped
            .error
            .clone()
            .unwrap_or_default()
            .contains("requires the control capability"),
        "read token must be refused an unscoped enumeration, got: {:?}",
        unscoped.error
    );
    let scoped = dispatch_authenticated(
        &ctx,
        req("read-t", "inbox_status", json!({"sessionId": "me"})),
    );
    assert!(
        !scoped
            .error
            .clone()
            .unwrap_or_default()
            .contains("requires the control capability"),
        "read token must be allowed a scoped inbox_status, got: {:?}",
        scoped.error
    );
}

#[test]
fn inbox_ack_and_status_handlers_round_trip() {
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    inbox
        .enqueue(
            "tileX",
            "crew:a",
            crate::inbox::Priority::Standard,
            "hi",
            true,
        )
        .unwrap();
    // Deliver it so it is ackable (the drain's at-most-once write).
    inbox.drain_one("tileX", |_r| Ok(()));
    let ctx = test_ctx("t").with_inbox(inbox.clone());

    // Status reflects the delivered-not-yet-processed record.
    let status = inbox_status(&ctx, &json!({"sessionId": "tileX"})).unwrap();
    assert_eq!(status["recipient"]["delivered"].as_u64(), Some(1));
    assert_eq!(status["recipient"]["enqueued"].as_u64(), Some(0));

    // Ack retires it (`delivered -> processed`).
    let ack = inbox_ack(&ctx, &json!({"sessionId": "tileX", "seq": 0}), None, true).unwrap();
    assert_eq!(ack["accepted"], "inbox_ack");
    assert_eq!(ack["state"], "processed");
    // A duplicate ack is a benign no-op (a lost-then-retried ack never re-writes).
    let reack = inbox_ack(&ctx, &json!({"sessionId": "tileX", "seq": 0}), None, true).unwrap();
    assert_eq!(reack["state"], "alreadyProcessed");

    // No sessionId => the all-recipients snapshot.
    let all = inbox_status(&ctx, &json!({})).unwrap();
    assert!(all["recipients"].is_array());

    // A malformed ack (missing seq) is rejected, not silently accepted.
    assert!(inbox_ack(&ctx, &json!({"sessionId": "tileX"}), None, true).is_err());
    // Acking an unknown recipient/seq is honest, not a crash.
    assert_eq!(
        inbox_ack(&ctx, &json!({"sessionId": "nope", "seq": 7}), None, true).unwrap()["state"],
        "unknown"
    );
}

#[test]
fn inbox_ack_self_scope_admits_own_ack_at_read_refuses_cross_session() {
    // The retired interim price: a crew self-acks its OWN inbox with only a READ
    // token (no control-capable relay needed). A cross-session ack is refused.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    for tile in ["crew-a", "crew-b"] {
        inbox
            .enqueue(tile, "cap:x", crate::inbox::Priority::Standard, "m", true)
            .unwrap();
        inbox.drain_one(tile, |_r| Ok(())); // deliver so it is ackable
    }
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_inbox(inbox.clone());

    // Self-ack with a bare READ token: ADMITTED (the §2.4.1 upgrade).
    let ok = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "inbox_ack",
            json!({"sessionId": "crew-a", "seq": 0}),
        ),
    );
    assert!(
        ok.ok,
        "self-ack must be admitted at read tier: {:?}",
        ok.error
    );
    assert_eq!(ok.result.unwrap()["state"], "processed");

    // Cross-session ack (crew-a acking crew-b) with the read token: REFUSED, and
    // crew-b's message is untouched.
    let bad = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "inbox_ack",
            json!({"sessionId": "crew-b", "seq": 0}),
        ),
    );
    assert!(
        !bad.ok,
        "a cross-session ack with a read token must be refused"
    );
    assert_eq!(
        inbox.depth("crew-b").delivered,
        1,
        "a refused cross-session ack must not process crew-b's message"
    );

    let full_token_cross = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &crew_a,
            "inbox_ack",
            json!({"sessionId": "crew-b", "seq": 0}),
        ),
    );
    assert!(
        !full_token_cross.ok,
        "Full capability must not substitute for host provenance"
    );
    assert_eq!(inbox.depth("crew-b").delivered, 1);
}

#[test]
fn plane_send_enforces_message_rows_and_never_crew_emergency() {
    // MANDATED never-crew-emergency guard + the message rows through the gate.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    assert!(reg.record_crew("cap-a", "crew-a").unwrap());
    assert!(reg.record_crew("cap-a", "crew-a2").unwrap());
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg)
        .with_inbox(inbox.clone());

    // Crew -> its OWN captain (up): ALLOWED at read tier.
    let up = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "plane_send",
            json!({"recipient": "cap-a", "text": "status"}),
        ),
    );
    assert!(up.ok, "crew->own captain must be allowed: {:?}", up.error);

    // Crew -> a SIBLING crew: REFUSED (no daisy-chain).
    let sib = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "plane_send",
            json!({"recipient": "crew-a2", "text": "psst"}),
        ),
    );
    assert!(!sib.ok);
    assert!(sib.error.unwrap().contains("daisy-chain"));

    // Crew raising EMERGENCY: REFUSED (never-crew-emergency). BYPASS-WOULD-FAIL:
    // admit crew emergency and this goes RED.
    let emg = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "plane_send",
            json!({"recipient": "cap-a", "text": "!!", "priority": "emergency"}),
        ),
    );
    assert!(!emg.ok);
    assert!(emg.error.unwrap().contains("EMERGENCY"));

    // A CAPTAIN may raise EMERGENCY to its own crew.
    let cap_emg = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cap_a,
            "plane_send",
            json!({"recipient": "crew-a", "text": "!!", "priority": "emergency"}),
        ),
    );
    assert!(
        cap_emg.ok,
        "a captain's emergency to own crew must be allowed: {:?}",
        cap_emg.error
    );
    assert_eq!(cap_emg.result.unwrap()["priority"], "emergency");
}

#[test]
fn abort_session_denies_cross_ship_and_crew_through_the_gate() {
    // The never-seized guard through the gate: a captain may not abort another
    // ship's crew, and a crew (read token) cannot reach the ProcessChanging abort at
    // all. No tmux is touched - the ACL refuses first.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    assert!(reg.record_crew("cap-a", "crew-a").unwrap());
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg);

    // Captain of ship-a aborting ship-b's crew: cross-ship, REFUSED.
    let cross = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cap_a,
            "abort_session",
            json!({"sessionId": "crew-b"}),
        ),
    );
    assert!(!cross.ok);
    assert!(cross.error.unwrap().contains("abort denied"));

    // A crew presenting a read token cannot even reach the ProcessChanging abort.
    let crew_try = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "abort_session",
            json!({"sessionId": "cap-a"}),
        ),
    );
    assert!(!crew_try.ok, "a read-token crew must not be able to abort");
}

#[test]
fn only_a_general_session_authorizes_and_the_gate_resolves_it() {
    // The delegation-gate carrier through the gate: only a general-roled session may
    // ORIGINATE; the resolve-and-verify gate then reports Present. A captain cannot.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let general = mint_session(&store, crate::identity::Role::General, "cortana", "gen");
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl").with_identity_store(store);

    // A captain session may NOT originate an authorization.
    let capauth = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "authorize", json!({"action": "spend"})),
    );
    assert!(!capauth.ok);
    assert!(capauth.error.unwrap().contains("only the general"));

    // The general originates one; the captain's gate consult resolves it Present.
    let ga = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &general,
            "authorize",
            json!({"action": "spend", "targetShip": "ship-a"}),
        ),
    );
    assert!(ga.ok, "general authorize failed: {:?}", ga.error);
    let id = ga.result.unwrap()["id"].as_str().unwrap().to_string();
    let chk = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "check_authorization", json!({"id": id})),
    );
    let r = chk.result.unwrap();
    assert_eq!(r["present"], json!(true));
    assert_eq!(r["verdict"], "present");

    // An unknown reference is Absent (the captain's gate FIRES = escalate).
    let miss = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "check_authorization",
            json!({"id": "no-such"}),
        ),
    );
    assert_eq!(miss.result.unwrap()["verdict"], "absent");
}

#[test]
fn plane_admin_purge_is_apex_only() {
    // operate-fleet-infra through the gate: a captain may not administer the shared
    // plane; the apex (Cortana) may.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    inbox
        .enqueue("crew-a", "x", crate::inbox::Priority::Standard, "m", true)
        .unwrap();
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let cortana = mint_current_cortana_session(&store, &reg, "cor");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg)
        .with_inbox(inbox.clone());

    // A captain may NOT purge.
    let capadm = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "plane_admin",
            json!({"op": "purge", "recipient": "crew-a"}),
        ),
    );
    assert!(!capadm.ok);
    assert!(capadm.error.unwrap().contains("apex-owned"));
    assert_eq!(
        inbox.depth("crew-a").enqueued,
        1,
        "a refused purge leaves the queue intact"
    );

    // Cortana (apex) may.
    let coradm = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cortana,
            "plane_admin",
            json!({"op": "purge", "recipient": "crew-a"}),
        ),
    );
    assert!(coradm.ok, "cortana purge failed: {:?}", coradm.error);
    assert_eq!(
        inbox.depth("crew-a").enqueued,
        0,
        "an apex purge flushed the queue"
    );
}
