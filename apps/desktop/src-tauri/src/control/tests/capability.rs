use super::*;

#[test]
fn mcp_tier_annotations_match_control_required_tier() {
    // item-3 ledger #16: the drift-can't-recur guard. Every tool the MCP surface
    // advertises must carry the SAME tier the control server ENFORCES via
    // `required_tier`, or the annotation-vs-enforcement drift that motivated the
    // socket-gate work reopens. BYPASS-WOULD-FAIL: change one MCP tool's tier (or
    // its control-side arm) without the other and this test goes RED.
    for tool in t_hub_mcp::tools::catalog() {
        let expected = match tool.tier {
            t_hub_mcp::tools::Tier::Read => CommandTier::Read,
            t_hub_mcp::tools::Tier::Organization => CommandTier::Organization,
            t_hub_mcp::tools::Tier::ProcessChanging => CommandTier::ProcessChanging,
            // The theme get/set pair is a PARALLEL track forwarded by name (it does
            // not flow through `required_tier`'s capability gate), so it has no
            // control-side tier to mirror. Skip it explicitly.
            t_hub_mcp::tools::Tier::Theme => continue,
        };
        assert_eq!(
            required_tier(tool.name),
            expected,
            "tier drift: MCP tool '{}' is annotated {:?} but control enforces {:?}",
            tool.name,
            tool.tier,
            required_tier(tool.name),
        );
    }
}

#[test]
fn legit_spawn_send_close_through_gate_is_admitted_and_audited() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // End-to-end through dispatch_authenticated (governor + audit) against a
    // REAL tmux session: a legitimate crew spawn -> send_text -> close must all
    // be ADMITTED and audited allowed. This is the "legit orchestration still
    // works over the exact socket" guarantee, exercised through the gate.
    let dir = std::env::temp_dir().join("t-hub-gate-e2e");
    clean_audit(&dir);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("e2e")
        .with_apply_sink(sink.clone())
        .with_audit(Arc::new(AuditLog::new(dir.clone())));
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);

    // Spawn a real session through the authenticated gate.
    let sresp = dispatch_authenticated(
        &ctx,
        req(
            "e2e",
            "spawn_terminal",
            json!({"cwd": "/tmp", "name": "crew", "tabId": "tab-1"}),
        ),
    );
    assert!(
        sresp.ok,
        "legit spawn was refused by the gate: {:?}",
        sresp.error
    );
    let id = sresp.result.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let target = tmux::target_for_id(&id);
    assert!(
        tmux::has_session(&target),
        "the real tmux session should exist"
    );
    let _ = tmux::resize_window_for_tests(&target, 80, 24);

    // Type into it through the gate (send_text is not throttled).
    let tresp = dispatch_authenticated(
        &ctx,
        req(
            "e2e",
            "send_text",
            json!({"sessionId": id, "text": "echo GATE_E2E_OK", "enter": true}),
        ),
    );
    assert!(tresp.ok, "legit send_text was refused: {:?}", tresp.error);

    // Close it through the gate (destructive, but the first teardown is under
    // the burst of 10 so it is admitted).
    let cresp =
        dispatch_authenticated(&ctx, req("e2e", "close_terminal", json!({"sessionId": id})));
    assert!(
        cresp.ok,
        "legit close_terminal was refused: {:?}",
        cresp.error
    );
    assert!(
        !tmux::has_session(&target),
        "session should be gone after close"
    );

    // All three land in the audit log, allowed and hash-chained. send_text's
    // literal payload is NOT present (redacted).
    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 3, "expected spawn+send+close audited: {recs:?}");
    let cmds: Vec<&str> = recs
        .iter()
        .map(|r| r["command"].as_str().unwrap())
        .collect();
    assert_eq!(cmds, ["spawn_terminal", "send_text", "close_terminal"]);
    assert!(
        recs.iter().all(|r| r["decision"] == "allowed"),
        "a legit command was not allowed: {recs:?}"
    );
    for w in recs.windows(2) {
        assert_eq!(w[1]["prev"], w[0]["hash"], "hash chain broken");
    }
    let blob = serde_json::to_string(&recs).unwrap();
    assert!(
        !blob.contains("GATE_E2E_OK"),
        "send_text literal leaked into audit"
    );
    clean_audit(&dir);
}

// -----------------------------------------------------------------------
// socket-gate Phase 2/2b: capability-scoped tokens
// -----------------------------------------------------------------------

#[test]
fn capability_resolution_maps_each_token() {
    // control token -> Full; read token -> ReadOnly; anything else -> None.
    let ctx = test_ctx("t"); // control="t", read="read-t"
    assert_eq!(resolve_capability(&ctx, "t"), Some(Capability::Full));
    assert_eq!(
        resolve_capability(&ctx, "read-t"),
        Some(Capability::ReadOnly)
    );
    assert_eq!(resolve_capability(&ctx, "nope"), None);
    assert_eq!(resolve_capability(&ctx, ""), None);
}

#[test]
fn empty_read_token_authorizes_nothing() {
    // A ctx with no read token configured must not let an empty presented token
    // resolve to ReadOnly (the empty==empty trap).
    let ctx = ControlContext::new(
        Arc::new(StatusBridge::new()),
        Arc::new(|_: &mut dyn FnMut(&Supervisor)| {}),
        "ctl".to_string(),
    );
    assert!(ctx.read_token.is_empty());
    assert_eq!(resolve_capability(&ctx, ""), None);
    assert_eq!(resolve_capability(&ctx, "ctl"), Some(Capability::Full));
}

#[test]
fn control_token_still_grants_full_power_backward_compat() {
    // THE make-or-break: the existing control token (published in control.json)
    // resolves to Full and is authorized for EVERY tier - zero client breakage.
    let ctx = test_ctx("t");
    assert!(Capability::Full.allows(CommandTier::Read));
    assert!(Capability::Full.allows(CommandTier::Organization));
    assert!(Capability::Full.allows(CommandTier::ProcessChanging));
    // Through the gate: a ProcessChanging command with the control token is NOT
    // authz-refused (it fails downstream only because this headless ctx has no
    // UI sink - proving authz passed).
    let resp = dispatch_authenticated(&ctx, req("t", "spawn_terminal", json!({"cwd": "/tmp"})));
    let err = resp.error.unwrap_or_default();
    assert!(
        !err.contains("requires the control capability"),
        "control token was authz-refused: {err}"
    );
    assert!(
        err.contains("no UI"),
        "expected the downstream no-UI failure, got: {err}"
    );
}

#[test]
fn read_token_reads_but_cannot_spawn_or_kill() {
    let dir = std::env::temp_dir().join("t-hub-p2-readonly");
    clean_audit(&dir);
    let ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));

    // Read tier: allowed (not authz-refused). May fail on tmux, but never authz.
    let r = dispatch_authenticated(&ctx, req("read-t", "list_terminals", json!({})));
    assert!(
        !r.error
            .clone()
            .unwrap_or_default()
            .contains("requires the control capability"),
        "read token was refused a Read command"
    );

    // ProcessChanging + Organization-destructive: authz-refused with the exact msg.
    for cmd in [
        "spawn_terminal",
        "send_text",
        "send_keys",
        "close_terminal",
        "create_worktree",
    ] {
        let resp = dispatch_authenticated(
            &ctx,
            req(
                "read-t",
                cmd,
                json!({"cwd": "/tmp", "sessionId": "x", "text": "y", "keys": ["C-c"]}),
            ),
        );
        let err = resp.error.unwrap_or_default();
        assert!(
            err == format!(
                "unauthorized: '{cmd}' requires the control capability (this token is read-only)"
            ),
            "read token should be authz-refused for {cmd}, got: {err}"
        );
    }

    // Every refusal is audited with tokenTier=read and decision=refused-authz.
    let recs = read_audit(&dir);
    assert!(!recs.is_empty());
    assert!(recs.iter().all(|r| r["decision"] == "refused-authz"));
    assert!(recs.iter().all(|r| r["tokenTier"] == "read"));
    clean_audit(&dir);
}

#[test]
fn generic_control_spawn_is_refused_without_recording_a_false_elevation() {
    let dir = std::env::temp_dir().join("t-hub-item3-ctlspawn");
    clean_audit(&dir);
    let mut ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));
    // A bound address enables stable discovery and identity minting.
    ctx.addr = "127.0.0.1:4242".to_string();

    // Default (untagged => READ) spawn: NO control-spawn audit record.
    let _ = spawn_env_with_identity(&ctx, &json!({"cwd": "/tmp"}), "spawn_terminal", None);
    let recs = read_audit(&dir);
    assert!(
        recs.iter().all(|r| r["decision"] != "control-spawn"),
        "a read-default spawn must NOT emit a control-spawn audit record"
    );

    // Explicit control is refused before identity mint or elevation audit.
    let refused = spawn_env_with_identity(
        &ctx,
        &json!({"cwd": "/tmp", "capability": "control"}),
        "spawn_terminal",
        None,
    )
    .unwrap_err();
    assert!(refused.contains("unsupported for generic Crew spawns"));
    let recs = read_audit(&dir);
    assert!(recs.iter().all(|r| r["decision"] != "control-spawn"));
    clean_audit(&dir);
}

#[test]
fn remote_peer_is_capped_to_read_even_with_control_token() {
    // Belt-and-suspenders (open Q4): a non-loopback peer presenting the CONTROL
    // token is capped to ReadOnly, so it cannot spawn/kill over the network bind.
    let mut ctx = test_ctx("t");
    ctx.peer_is_loopback = false;
    assert_eq!(resolve_capability(&ctx, "t"), Some(Capability::ReadOnly));
    // Read still works remotely; ProcessChanging is authz-refused.
    let spawn = dispatch_authenticated(&ctx, req("t", "spawn_terminal", json!({"cwd": "/tmp"})));
    assert!(spawn
        .error
        .unwrap()
        .contains("requires the control capability"));
    let read = dispatch_authenticated(&ctx, req("t", "list_terminals", json!({})));
    assert!(!read
        .error
        .clone()
        .unwrap_or_default()
        .contains("requires the control capability"));
}

#[test]
fn read_token_is_valid_for_subscribe() {
    // token_is_valid (the event-subscribe gate) accepts either capability so a
    // least-privilege monitor can subscribe; a bad token is rejected.
    let ctx = test_ctx("t");
    assert!(token_is_valid(&ctx, "t"));
    assert!(token_is_valid(&ctx, "read-t"));
    assert!(!token_is_valid(&ctx, "bad"));
}

#[test]
fn phase3_flag_is_on_by_default_and_selects_read_token() {
    // item-3 flip #2 (ratified 2026-07-10): Phase 3 hardening is ON by default, so
    // `control.json` publishes only the READ token and an ambient scraper is
    // read-only. `T_HUB_CONTROL_HARDEN=0`/`false` is the instant rollback. This is
    // a BYPASS-WOULD-FAIL guard: revert the default to OFF and the first assert
    // goes RED. This mutates a process-global env var; it is saved/restored around
    // the mutation to stay hermetic.
    let saved = std::env::var("T_HUB_CONTROL_HARDEN").ok();
    std::env::remove_var("T_HUB_CONTROL_HARDEN");
    assert!(
        phase3_harden_enabled(),
        "harden flag must default ON (item-3 flip #2)"
    );
    std::env::set_var("T_HUB_CONTROL_HARDEN", "0");
    assert!(
        !phase3_harden_enabled(),
        "'0' is the rollback (hardening OFF)"
    );
    std::env::set_var("T_HUB_CONTROL_HARDEN", "false");
    assert!(
        !phase3_harden_enabled(),
        "'false' is the rollback (hardening OFF)"
    );
    std::env::set_var("T_HUB_CONTROL_HARDEN", "1");
    assert!(phase3_harden_enabled(), "'1' stays ON");
    std::env::set_var("T_HUB_CONTROL_HARDEN", "true");
    assert!(phase3_harden_enabled(), "'true' stays ON");
    std::env::set_var("T_HUB_CONTROL_HARDEN", "yes");
    assert!(phase3_harden_enabled(), "any non-0/false value stays ON");
    match saved {
        Some(v) => std::env::set_var("T_HUB_CONTROL_HARDEN", v),
        None => std::env::remove_var("T_HUB_CONTROL_HARDEN"),
    }

    // The pure selector: ON ⇒ read token, OFF ⇒ control token.
    assert_eq!(select_published_token("ctl", "rd", true), "rd");
    assert_eq!(select_published_token("ctl", "rd", false), "ctl");
    // Never an empty read token (falls back to control so a context that never
    // minted a read token is not locked out).
    assert_eq!(select_published_token("ctl", "", true), "ctl");
}

#[test]
fn my_capability_reports_the_callers_resolved_capability() {
    // item-3 Pillar C: the gate resolves its own class from the unspoofable token.
    // A control token reports "control"; the read token reports "read".
    let ctx = test_ctx("t");
    let control = dispatch_authenticated(&ctx, req("t", "my_capability", json!({})));
    assert_eq!(control.result.unwrap()["capability"], "control");
    let read = dispatch_authenticated(&ctx, req("read-t", "my_capability", json!({})));
    assert_eq!(read.result.unwrap()["capability"], "read");
}
