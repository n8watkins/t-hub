use super::*;

// NOTE: the former `wait_for_status_captures_transient_edge_between_polls`
// test lived here. It drove A(working) → B(completed) → A(working) from a
// driver thread that slept 150ms hoping to land *inside* the poller's first
// 500ms `wait_for_status` window — a wall-clock race that slips on a loaded
// box (the driver can run before the dispatcher even captures its `consumed`
// watermark, or after the window it was aiming for). The semantics it tried to
// assert ("an edge logged strictly between two polls is still observed") can't
// be expressed at this control layer without that race: the dispatcher
// captures `consumed = current_seq()` *internally*, so any edge that is to land
// at `seq > consumed` must be logged by a concurrent thread after that capture,
// and the dispatcher exposes no hook to synchronize against.
//
// That edge-capture logic is `Supervisor::matched_since`, which `wait_for_status`
// calls directly — and it is already proven DETERMINISTICALLY (no threads, no
// sleeps) by `supervision::tests::transition_log_captures_transient_edge_through_b`,
// which drives the same A→B→A sequence and asserts `matched_since` recovers the
// transient Completed edge from the log. That is the real coverage; this
// duplicate was dropped rather than kept as a flaky wall-clock race.
//
// The deterministic dispatcher-level behaviours that DON'T need a race are still
// covered above: immediate current-status match (`wait_for_status_immediate_
// match_does_not_time_out`), target arrays, and the 0ms timeout path.

#[test]
fn read_terminal_requires_session_id() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "read_terminal", &Value::Null).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
}

#[test]
fn send_text_requires_session_and_text() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "send_text", &json!({"text": "hi"})).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
    let err = dispatch(&ctx, "send_text", &json!({"sessionId": "x"})).unwrap_err();
    assert!(err.contains("text"), "got: {err}");
}

#[test]
fn send_keys_requires_non_empty_keys() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "send_keys", &json!({"sessionId": "x", "keys": []})).unwrap_err();
    assert!(err.contains("keys"), "got: {err}");
}

#[test]
fn close_terminal_requires_session_id() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "close_terminal", &Value::Null).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
}

#[test]
fn send_to_missing_session_is_a_clear_error() {
    // No `th_*` session named this exists ⇒ a readable "no such session".
    let ctx = test_ctx("t");
    let err = dispatch(
        &ctx,
        "send_text",
        &json!({"sessionId": "definitely_absent_xyz", "text": "hi"}),
    )
    .unwrap_err();
    assert!(err.contains("no such session"), "got: {err}");
}

/// De-conflation guard (spawn-wedge): the direct-writer gate must map a
/// three-state probe correctly - `Alive` proceeds, a DEFINITIVE `Gone` is "no
/// such session", and an INDETERMINATE `Unknown` (a timed-out / failed probe) is
/// a RETRYABLE control-plane timeout that must NEVER read as "no such session".
/// That false negative is exactly what sent the fleet to raw-tmux break-glass on
/// 0.3.62; reverting the `Unknown` arm to the old `!has_session` conflation (so a
/// timeout falls into the Gone message) trips this test.
#[test]
fn writer_gate_timeout_is_retryable_never_no_such_session() {
    use tmux::SessionLiveness::*;
    // Alive proceeds.
    assert!(
        writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Alive).is_ok(),
        "a live session must proceed"
    );
    // Gone is a definitive "no such session".
    let gone = writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Gone).unwrap_err();
    assert!(
        gone.contains("no such session"),
        "a completed-absent probe is 'no such session'; got: {gone}"
    );
    // Unknown (a timed-out probe) is retryable and must NOT read as gone.
    let unknown =
        writer_liveness_gate("send_keys", "e05764f5", "th_e05764f5", Unknown).unwrap_err();
    assert!(
        !unknown.contains("no such session"),
        "a timed-out probe must NOT read as gone; got: {unknown}"
    );
    assert!(
        unknown.contains("timed out") && unknown.contains("retry"),
        "the Unknown arm must name the timeout and invite a retry; got: {unknown}"
    );
}

/// MED-1 guard (PR-58 review): the `close_terminal` `force` escape keeps a
/// genuinely-dead-but-`Unknown` session reapable, and never kills a session a
/// fresh re-probe CONFIRMS `Alive`. The name states exactly what is pinned - NOT
/// "never kills a live session" (a live-but-slow session whose re-probe also
/// times out is `Unknown`, indistinguishable from dead, and IS force-reaped; see
/// `plan_close`). `plan_close` is the pure decision; this pins every arm. Bypass:
/// make `force + Unknown + reprobe Alive` reap (drop the `RefuseForceOnLive` arm)
/// and the reprobe-Alive refusal assert trips.
#[test]
fn force_close_never_kills_a_session_that_probes_alive() {
    use tmux::SessionLiveness::*;
    // Default (no force): Alive/Gone reap normally; Unknown is a retryable refusal.
    assert!(matches!(
        plan_close(false, Alive, None),
        ClosePlan::Kill {
            existed: true,
            forced: false
        }
    ));
    assert!(matches!(
        plan_close(false, Gone, None),
        ClosePlan::Kill {
            existed: false,
            forced: false
        }
    ));
    assert!(matches!(
        plan_close(false, Unknown, None),
        ClosePlan::RetryableTimeout
    ));
    // force + Unknown, re-probe ALIVE => REFUSE (the load-bearing guarantee: a
    // session a fresh probe CONFIRMS Alive is never force-killed).
    assert!(matches!(
        plan_close(true, Unknown, Some(Alive)),
        ClosePlan::RefuseForceOnLive
    ));
    // force + Unknown, re-probe GONE => clean reap (now confirmed dead).
    assert!(matches!(
        plan_close(true, Unknown, Some(Gone)),
        ClosePlan::Kill {
            existed: false,
            forced: false
        }
    ));
    // force + Unknown, re-probe STILL Unknown => forced reap: a still-unreachable
    // session stays reapable (the whole point of the escape). Under a sustained
    // wedge this reaps a dead OR a live-but-unreachable tile - by design; force is
    // an explicit reap-during-wedge override.
    assert!(matches!(
        plan_close(true, Unknown, Some(Unknown)),
        ClosePlan::Kill {
            existed: false,
            forced: true
        }
    ));
}

#[test]
fn tmux_target_maps_id_and_is_idempotent() {
    assert_eq!(tmux_target("abc"), "th_abc");
    assert_eq!(tmux_target("th_abc"), "th_abc");
}

#[test]
fn send_text_break_glass_emits_loud_marker() {
    // comms-plane Phase 1: `send_text` is DEMOTED to break-glass. Using it must
    // emit a live `control://break-glass` marker (D11a) so the deviation from
    // the plane primary path is visible. The marker fires even though this
    // send_text ultimately errors (no such tmux session) - a break-glass USE is
    // logged on attempt, not only on success.
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    let mut reader = subscribe_test_reader(&fanout);

    let _ = dispatch(
        &ctx,
        "send_text",
        &json!({ "sessionId": "no-such-session", "text": "hello" }),
    );

    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["event"], "control://break-glass");
    assert_eq!(frame["payload"]["command"], "send_text");
    assert_eq!(frame["payload"]["breakGlass"], true);
    assert_eq!(frame["payload"]["sessionId"], "no-such-session");
    // Byte length only - the marker must NOT leak the payload content.
    assert_eq!(frame["payload"]["bytes"], 5);
    assert!(
        frame["payload"].get("text").is_none(),
        "must not leak text: {frame}"
    );
}

#[test]
fn send_keys_break_glass_emits_loud_marker() {
    // The demoted twin: `send_keys` also emits the break-glass marker.
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    let mut reader = subscribe_test_reader(&fanout);

    let _ = dispatch(
        &ctx,
        "send_keys",
        &json!({ "sessionId": "no-such-session", "keys": ["C-c", "Escape"] }),
    );

    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["event"], "control://break-glass");
    assert_eq!(frame["payload"]["command"], "send_keys");
    assert_eq!(frame["payload"]["breakGlass"], true);
    // send_keys carries its payload in `keys`, not `text`: the marker must
    // report the joined key-name length ("C-c Escape" = 10), not bytes=0.
    assert_eq!(frame["payload"]["bytes"], 10);
}

#[test]
fn close_terminal_retires_legacy_powder_without_network() {
    if !std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!(
                "rollback_close_retains_cleanup_pending_crew_when_powder_release_fails: tmux not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let crew_id = format!("rollback-{}", uuid::Uuid::new_v4().simple());
    let target = tmux_target(&crew_id);
    create_test_tmux_session(&target).unwrap();

    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "rollback-project".into(),
            name: "Rollback Project".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: None,
            powder: Some(PowderProjectBinding {
                connection_profile: format!("missing-{}", uuid::Uuid::new_v4().simple()),
                repository: "rollback-project".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    registry
        .claim_test("rollback-captain", Some("rollback-ship"), vec![])
        .unwrap();
    registry
        .bind_ship_context(
            "rollback-ship",
            "rollback-project",
            "Test rollback",
            "codex",
        )
        .unwrap();
    registry.record_crew("rollback-captain", &crew_id).unwrap();
    registry
        .bind_crew_context(
            "rollback-captain",
            &crew_id,
            "Test failed release",
            "codex",
            Some("/tmp"),
            Some("card-1"),
            PowderWorkBinding {
                card_id: "card-1".into(),
                run_id: "run-1".into(),
                agent: None,
                claim_expires_at: Some(1),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
        )
        .unwrap();
    let ctx = test_ctx("secret").with_captains_registry(registry.clone());

    let closed =
        close_terminal_with_policy(&ctx, &json!({ "sessionId": crew_id }), true, None).unwrap();

    assert_eq!(closed["powderRelease"]["outcome"], "retired");
    assert_eq!(closed["powderRelease"]["released"], false);
    assert_eq!(tmux::session_liveness(&target), tmux::SessionLiveness::Gone);
    let snapshot = registry.snapshot();
    assert!(snapshot.pending_dispatch_releases.is_empty());
    assert!(matches!(
        snapshot.captains[0].crew[0].state,
        CrewState::Removed { .. }
    ));
    // The retired binding remains on the historical tombstone for
    // deserialization compatibility; no remote release was attempted.
    assert!(snapshot.captains[0].crew[0].powder_work.is_some());
}

// -----------------------------------------------------------------------
// Registry-vs-reality: close_terminal outcome (ask #3, Incident C)
// -----------------------------------------------------------------------

#[test]
fn close_terminal_reports_already_gone_for_a_phantom() {
    // Incident C: closing a session that never existed must not look like a real
    // kill. ok:true (idempotent) stays, but the outcome discriminates it.
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "close_terminal", &json!({"sessionId": "f0f3207b"})).unwrap();
    assert_eq!(v["accepted"], "close_terminal");
    assert_eq!(v["outcome"], "already_gone");
}

#[test]
fn close_terminal_reports_killed_for_a_live_session() {
    // A real session reports outcome=killed, so a caller can tell a genuine kill
    // from a phantom close.
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink);
    let spawn = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap();
    let id = spawn["id"].as_str().unwrap().to_string();
    let closed = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    assert_eq!(closed["outcome"], "killed");
}
