use super::*;

// -----------------------------------------------------------------------
// Idempotency: RequestCache (ask #1)
// -----------------------------------------------------------------------

#[test]
fn server_idempotent_command_contract_is_complete() {
    assert_eq!(
        IDEMPOTENT_COMMANDS,
        [
            "spawn_terminal",
            "create_worktree",
            "history_resume",
            "reconcile_cortana",
            "commission_captain",
            "dispatch_crew",
            "start_agent",
            "agent_followup",
        ]
    );
    assert!(!is_idempotent_command("list_tabs"));
}

#[test]
fn request_cache_replays_a_completed_outcome() {
    let cache = RequestCache::new();
    // First sighting reserves the id and must run the command.
    assert!(matches!(cache.begin("r1"), BeginOutcome::Fresh));
    let stored = cache.finish("r1", Ok(json!({"id": "abc"})));
    assert_eq!(stored.unwrap()["id"], "abc");
    // A retry of the SAME id replays the stored outcome - it does NOT re-run.
    match cache.begin("r1") {
        BeginOutcome::Duplicate(Ok(v)) => assert_eq!(v["id"], "abc"),
        BeginOutcome::Duplicate(Err(e)) => panic!("expected Ok replay, got Err: {e}"),
        BeginOutcome::Fresh => panic!("a completed id must not be reserved Fresh again"),
        BeginOutcome::FreshAfterReap => {
            panic!("a completed id must replay, not reap-and-re-reserve")
        }
        BeginOutcome::InFlight => panic!("a completed id must replay, not report InFlight"),
    }
}

#[test]
fn request_cache_rejects_reusing_an_id_for_different_arguments() {
    let cache = RequestCache::new();
    assert!(matches!(
        cache.begin_bound("history-request", "resume:one"),
        BeginOutcome::Fresh
    ));
    cache
        .finish("history-request", Ok(json!({"terminalId": "one"})))
        .unwrap();
    match cache.begin_bound("history-request", "resume:two") {
        BeginOutcome::Duplicate(Err(error)) => {
            assert!(error.starts_with("request_conflict:"));
        }
        _ => panic!("a requestId must remain bound to its original arguments"),
    }
}

#[test]
fn request_cache_reports_in_flight_for_a_concurrent_duplicate() {
    let cache = RequestCache::new();
    // A first caller reserved the id and is still running (no finish yet).
    assert!(matches!(cache.begin("r2"), BeginOutcome::Fresh));
    // A retry that races the original must NOT run the command again.
    assert!(matches!(cache.begin("r2"), BeginOutcome::InFlight));
    // Once it completes, the same id replays the outcome.
    let _ = cache.finish("r2", Ok(json!({"ok": true})));
    assert!(matches!(cache.begin("r2"), BeginOutcome::Duplicate(_)));
}

#[test]
fn request_cache_cancel_frees_a_reservation_for_retry() {
    let cache = RequestCache::new();
    assert!(matches!(cache.begin("r3"), BeginOutcome::Fresh));
    // A governor refusal cancels the reservation (no outcome recorded)...
    cache.cancel("r3");
    // ...so a later retry is Fresh again (it can succeed once budget frees),
    // not stuck InFlight or replaying a refusal.
    assert!(matches!(cache.begin("r3"), BeginOutcome::Fresh));
}

#[test]
fn request_cache_status_reports_unknown_inflight_and_completed() {
    let cache = RequestCache::new();
    assert!(matches!(cache.status("nope"), RequestStatus::Unknown));
    cache.begin("r4");
    assert!(matches!(cache.status("r4"), RequestStatus::InFlight));
    let _ = cache.finish("r4", Err("boom".to_string()));
    match cache.status("r4") {
        RequestStatus::Completed(Err(e)) => assert_eq!(e, "boom"),
        _ => panic!("expected Completed(Err)"),
    }
}

#[test]
fn request_cache_evicts_oldest_completed_beyond_capacity() {
    let cache = RequestCache::with_bounds(
        2,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_secs(600),
    );
    for id in ["a", "b", "c"] {
        cache.begin(id);
        let _ = cache.finish(id, Ok(json!({"id": id})));
    }
    // "a" was evicted when "c" pushed past the capacity of 2.
    assert!(matches!(cache.status("a"), RequestStatus::Unknown));
    assert!(matches!(cache.status("b"), RequestStatus::Completed(_)));
    assert!(matches!(cache.status("c"), RequestStatus::Completed(_)));
}

#[test]
fn request_cache_evicts_a_done_entry_past_its_ttl() {
    // A completed outcome ages out of the cache after its TTL, keeping the cache
    // self-cleaning. (The same retain reaps a stale InFlight reservation past
    // REQUEST_INFLIGHT_REAP - the safety valve for a panicked/hung handler.)
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_millis(1),
        std::time::Duration::from_secs(600),
    );
    cache.begin("done");
    let _ = cache.finish("done", Ok(json!({})));
    std::thread::sleep(std::time::Duration::from_millis(5));
    // status() runs eviction; the expired Done entry is gone -> Unknown, so a
    // fresh retry would be safe.
    assert!(matches!(cache.status("done"), RequestStatus::Unknown));
}

#[test]
fn request_cache_reaps_a_stale_in_flight_reservation() {
    // The InFlight reap safety valve: a reservation that never finished (a
    // panicked/hung handler) is presumed dead after `inflight_reap` so a retry
    // is not blocked forever. Tiny reap window stands in for the 600s default.
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_millis(1),
    );
    cache.begin("stuck"); // reserved InFlight, never finished
    std::thread::sleep(std::time::Duration::from_millis(5));
    // A retry now sees FreshAfterReap (the dead reservation was reaped + re-
    // reserved), not a permanent InFlight. The `AfterReap` flavor tells dispatch
    // to RE-PROBE reality before re-applying (M1 full fix) - a genuinely-new id
    // would be plain Fresh.
    assert!(matches!(cache.begin("stuck"), BeginOutcome::FreshAfterReap));
}

#[test]
fn request_cache_reaped_id_yields_exactly_one_fresh_after_reap() {
    // F4 (one-reprobe-per-reap): after a reservation is reaped, TWO retries of
    // the same id must NOT both re-probe/re-apply. `begin` is atomic — the FIRST
    // retry consumes the reap (FreshAfterReap) AND re-reserves the id InFlight in
    // the same locked step, so the SECOND retry sees a live InFlight reservation,
    // not a second FreshAfterReap. That is what caps the M1 re-probe (and its
    // unbounded git worktree-list) at ONCE per reap: the loser is told InFlight
    // and polls/retries instead of issuing a duplicate reality probe + re-apply.
    //
    // A comfortably large reap window (relative to two back-to-back synchronous
    // `begin` calls) keeps this deterministic: the original ages PAST it, but the
    // freshly re-reserved slot is far YOUNGER than it when the second retry runs.
    let reap = std::time::Duration::from_millis(50);
    let cache = RequestCache::with_bounds(8, std::time::Duration::from_secs(600), reap);

    cache.begin("wt"); // original reservation, never finished (handler presumed dead)
    std::thread::sleep(reap * 2); // age it past the reap window

    // First retry: the dead reservation is reaped and re-reserved in one step.
    assert!(
        matches!(cache.begin("wt"), BeginOutcome::FreshAfterReap),
        "the first retry after a reap must re-probe reality (FreshAfterReap)"
    );
    // Second retry, immediately after: the just-re-reserved slot is still well
    // within the reap window, so this loser sees InFlight — NOT a second reprobe.
    assert!(
        matches!(cache.begin("wt"), BeginOutcome::InFlight),
        "a concurrent second retry must see InFlight, not a duplicate FreshAfterReap"
    );
    // And a third: still InFlight until the winner calls finish(). At no point
    // does a single reap yield two re-applies.
    assert!(matches!(cache.begin("wt"), BeginOutcome::InFlight));

    // Once the winner records the outcome, further retries replay it (Duplicate),
    // still never a second apply.
    let _ = cache.finish("wt", Ok(json!({"alreadyCreated": true})));
    assert!(matches!(cache.begin("wt"), BeginOutcome::Duplicate(_)));
}

#[test]
fn request_cache_never_seen_id_is_fresh_not_fresh_after_reap() {
    // A first-ever id must be plain Fresh (no reap happened), so dispatch does
    // NOT waste a reality re-probe on it - FreshAfterReap is reserved for a
    // retry whose prior reservation actually aged out.
    let cache = RequestCache::new();
    assert!(matches!(cache.begin("brand-new"), BeginOutcome::Fresh));
}

#[test]
fn request_cache_reap_after_completion_is_fresh_not_reap() {
    // A COMPLETED id that TTL-expires and is retried is a fresh apply, NOT a
    // reap: the reap flavor is strictly for an InFlight reservation that aged
    // out (the ambiguous "did it land?" case), not for a cleanly-finished one
    // whose cache entry simply expired.
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_millis(1), // TTL
        std::time::Duration::from_secs(600), // reap window (irrelevant here)
    );
    cache.begin("done");
    let _ = cache.finish("done", Ok(json!({"id": "done"})));
    std::thread::sleep(std::time::Duration::from_millis(5)); // outlive the TTL
    assert!(matches!(cache.begin("done"), BeginOutcome::Fresh));
}

#[test]
fn request_cache_stale_completion_cannot_overwrite_replacement_reservation() {
    // A handler that outlives the reap window must not complete the replacement
    // reservation created for the same request ID.
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_millis(1),
    );
    let (first, first_reservation) = cache.begin_bound_with_reservation("x", "resume:one");
    assert!(matches!(first, BeginOutcome::Fresh));
    let first_reservation = first_reservation.unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (replacement, replacement_reservation) =
        cache.begin_bound_with_reservation("x", "resume:one");
    assert!(matches!(replacement, BeginOutcome::FreshAfterReap));
    let replacement_reservation = replacement_reservation.unwrap();

    let _ = cache.finish_reserved(
        "x",
        first_reservation,
        "resume:one",
        Ok(json!({"id": "stale"})),
    );
    assert!(
        matches!(cache.status("x"), RequestStatus::InFlight),
        "a stale completion must leave the replacement reservation in flight"
    );
    cache.cancel_reserved("x", first_reservation);
    assert!(matches!(cache.status("x"), RequestStatus::InFlight));

    let _ = cache.finish_reserved(
        "x",
        replacement_reservation,
        "resume:one",
        Ok(json!({"id": "replacement"})),
    );
    match cache.status("x") {
        RequestStatus::Completed(Ok(value)) => {
            assert_eq!(value["id"], "replacement");
        }
        _ => panic!("the replacement reservation must own the completed outcome"),
    }
}

#[test]
fn request_cache_preserves_a_late_completion_when_no_replacement_owns_the_id() {
    let cache = RequestCache::with_bounds(
        1,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_millis(1),
    );
    let (begin, reservation) = cache.begin_bound_with_reservation("late", "reconcile:one");
    assert!(matches!(begin, BeginOutcome::Fresh));
    let reservation = reservation.expect("fresh request reservation");

    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(matches!(cache.status("late"), RequestStatus::Unknown));
    let _ = cache.finish_reserved(
        "late",
        reservation,
        "reconcile:one",
        Ok(json!({"id": "late"})),
    );

    assert!(matches!(
        cache.begin_bound("late", "reconcile:one"),
        BeginOutcome::Duplicate(Ok(_))
    ));
    assert!(matches!(
        cache.begin_bound("late", "reconcile:other"),
        BeginOutcome::Duplicate(Err(_))
    ));

    cache.begin("new");
    let _ = cache.finish("new", Ok(json!({"id": "new"})));
    assert!(matches!(cache.status("late"), RequestStatus::Unknown));
}

#[test]
fn spawn_terminal_retry_with_same_request_id_does_not_duplicate() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // Repro of Incident A/B at the dispatch layer: a spawn that is RETRIED with
    // the same requestId (the client's recovery from an ambiguous response leg)
    // must apply exactly once - one tmux session, one tile, one UI forward - and
    // the retry must replay the original outcome, never spawn a second session.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let args = json!({"cwd": "/tmp", "requestId": "spawn-retry-1"});
    let first = dispatch_authenticated(
        &ctx,
        ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args: args.clone(),
            session: String::new(),
            host: "t".into(),
            v: None,
        },
    );
    assert!(first.ok, "first spawn failed: {:?}", first.error);
    let id = first.result.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The retry: identical requestId. It must NOT spawn again.
    let retry = dispatch_authenticated(
        &ctx,
        ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args,
            session: String::new(),
            host: "t".into(),
            v: None,
        },
    );
    assert!(retry.ok, "retry failed: {:?}", retry.error);
    let retry_result = retry.result.unwrap();
    assert_eq!(
        retry_result["id"].as_str().unwrap(),
        id,
        "retry replays the same id"
    );
    assert_eq!(
        retry_result["idempotentReplay"],
        json!(true),
        "retry is tagged a replay"
    );

    // Exactly ONE real session materialized, and ONE UI forward was emitted.
    let live: Vec<String> = tmux::list_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s == &format!("th_{id}"))
        .collect();
    assert_eq!(live.len(), 1, "exactly one tmux session for the id");
    assert_eq!(
        sink.calls.lock().unwrap().len(),
        1,
        "the retry did NOT re-forward a spawn"
    );

    // Reap the real session.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn get_request_status_command_resolves_a_completed_spawn() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // The queryable half of ask #1: after a spawn with a requestId, a caller
    // whose response leg failed can learn the outcome (and the real id) without
    // guessing. An unknown id reports unknown (safe to retry).
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink);
    let spawn = dispatch_authenticated(
        &ctx,
        ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args: json!({"cwd": "/tmp", "requestId": "spawn-status-1"}),
            session: String::new(),
            host: "t".into(),
            v: None,
        },
    );
    assert!(spawn.ok);
    let id = spawn.result.unwrap()["id"].as_str().unwrap().to_string();

    let status = dispatch(
        &ctx,
        "get_request_status",
        &json!({"requestId": "spawn-status-1"}),
    )
    .unwrap();
    assert_eq!(status["status"], "completed");
    assert_eq!(status["ok"], true);
    assert_eq!(status["result"]["id"].as_str().unwrap(), id);

    let unknown = dispatch(
        &ctx,
        "get_request_status",
        &json!({"requestId": "never-seen"}),
    )
    .unwrap();
    assert_eq!(unknown["status"], "unknown");

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}
