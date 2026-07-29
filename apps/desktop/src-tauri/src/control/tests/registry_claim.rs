use super::*;

#[test]
fn claim_registers_updates_and_bumps_seq() {
    let reg = CaptainsRegistry::new();
    let out = reg
        .claim(
            "cap-1",
            Some("Ship Alpha!"),
            FleetRole::Captain,
            None,
            vec!["tab-1".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::Created);
    let rec = out.record;
    assert_eq!(rec.ship_slug, "ship-alpha");
    assert_eq!(rec.terminal_id.as_deref(), Some("cap-1"));
    assert_eq!(rec.role, FleetRole::Captain);
    assert_eq!(rec.state, ClaimState::Active);
    assert_eq!(rec.workspace_tab_ids, vec!["tab-1".to_string()]);
    assert!(rec.crew.is_empty());
    assert_eq!(reg.snapshot().seq, 1);

    // Re-claim by the SAME terminal to a new ship is a re-designation: slug/tabs
    // refresh, crew kept, no duplicate record.
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    let out = reg
        .claim(
            "cap-1",
            Some("ship-beta"),
            FleetRole::Captain,
            None,
            vec!["tab-2".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let rec = out.record;
    assert_eq!(rec.ship_slug, "ship-beta");
    assert_eq!(rec.workspace_tab_ids, vec!["tab-2".to_string()]);
    assert_eq!(crew_tiles(&rec), vec!["crew-1".to_string()]);
    let snap = reg.snapshot();
    assert_eq!(
        snap.captains.len(),
        1,
        "re-designation must not duplicate the claim"
    );
    assert_eq!(snap.seq, 3);
}

#[test]
fn project_bound_same_terminal_redesignation_is_rejected_without_identity_drift() {
    let path = captains_tmp("project-bound-redesignation");
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-alpha".into(),
            name: "Alpha Project".into(),
            repo_root: "/tmp/project-alpha".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    registry
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    registry
        .bind_ship_context("alpha", "project-alpha", "Own Alpha", "codex")
        .unwrap();
    registry
        .rename_captain(Some("captain-a"), None, "Alpha Lead")
        .unwrap();
    registry.record_crew("captain-a", "crew-a").unwrap();
    let before = registry.snapshot();

    let error = registry
        .claim_provider(
            "captain-a",
            Some("beta"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec!["work-b".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(error.contains("project-bound"), "got: {error}");
    let after = registry.snapshot();
    assert_eq!(after.seq, before.seq);
    assert_eq!(after.captains, before.captains);
    let restarted = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restarted.seq, before.seq);
    assert_eq!(restarted.captains, before.captains);

    registry.release("captain-a").unwrap();
    let reused = registry
        .claim_provider(
            "captain-b",
            Some("alpha"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec!["work-a".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(reused.record.ship_slug, "alpha");
    assert_eq!(reused.record.terminal_id.as_deref(), Some("captain-b"));
    assert_eq!(reused.record.project_id.as_deref(), Some("project-alpha"));
    assert_eq!(
        reused.record.assignment_id,
        "assignment:project-alpha:alpha"
    );
    assert_eq!(reused.record.display_name, "Alpha Lead");
    assert_eq!(crew_tiles(&reused.record), vec!["crew-a"]);
    let reused_after_restart = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(reused_after_restart.captains.len(), 1);
    assert_eq!(
        reused_after_restart.captains[0].terminal_id.as_deref(),
        Some("captain-b")
    );
    assert_eq!(reused_after_restart.captains[0].ship_slug, "alpha");

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn claim_defaults_slug_and_a_live_ship_is_never_seized() {
    // The double-claim RACE / wedged-not-dead guard: a DIFFERENT terminal claiming
    // a slug held by a LIVE incumbent is REJECTED (a bypass - seizing a live ship
    // on a soft signal - would split-brain; HIGH-2/R1). A live tmux session is the
    // "wedged" case too: has_session true => not transfer-grade => reject.
    let reg = CaptainsRegistry::new();
    let out = reg.claim_test("cap-1", None, vec![]).unwrap();
    assert_eq!(out.record.ship_slug, "ship-cap-1");
    let err = reg
        .claim(
            "cap-2",
            Some("ship-cap-1"),
            FleetRole::Captain,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(
        err.contains("already captained by a LIVE session 'cap-1'"),
        "got: {err}"
    );
    // The incumbent is untouched; the refusal did not bump the revision.
    assert_eq!(only(&reg).terminal_id.as_deref(), Some("cap-1"));
    assert_eq!(reg.snapshot().seq, 1, "refusals must not bump the revision");
    // Empty session id is refused before touching the registry.
    assert!(reg.claim_test("  ", None, vec![]).is_err());
}

#[test]
fn corpse_holds_slug_auto_releases_on_unambiguous_death() {
    // R-H2 core: a captain's terminal is killed and the session migrates to a new
    // terminal. The corpse's claim would DEADLOCK the migrated re-claim today.
    // Re-keyed: `tmux::has_session == false` (the SOLE transfer-grade signal) auto-
    // releases the corpse and the new terminal takes the slug. Crew are preserved.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("t-hub-app"), vec![])
        .unwrap();
    assert!(reg.record_crew("cap-old", "crew-1").unwrap());
    // cap-old's pane is gone; cap-new re-claims the same ship (no UUID resolved).
    let dead_is_old = |tile: &str| tile == "cap-old";
    let out = reg
        .claim(
            "cap-new",
            Some("t-hub-app"),
            FleetRole::Captain,
            None,
            vec![],
            &dead_is_old,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::AutoReleasedDead);
    assert_eq!(out.record.terminal_id.as_deref(), Some("cap-new"));
    assert_eq!(
        crew_tiles(&out.record),
        vec!["crew-1".to_string()],
        "crew followed the ship"
    );
    assert_eq!(
        reg.snapshot().captains.len(),
        1,
        "no duplicate - the slug transferred"
    );
}

#[test]
fn timed_out_probe_never_seizes_an_incumbents_ship() {
    // De-conflation guard (spawn-wedge): the transfer decision must be driven by
    // the SAME production mapping the real claim uses -
    // `is_definitively_gone(session_liveness(..))` - so that an INDETERMINATE probe
    // (a 5s tmux timeout under a degraded spawn path) is NOT transfer-grade. Here
    // the injected predicate is that production mapping applied to an `Unknown`
    // probe result; the incumbent must be treated as a LIVE ship and the claim
    // REJECTED, never auto-released. The old `!has_session` conflation returned
    // `true` for a timeout and WOULD have seized the live ship - this trips it.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("t-hub-app"), vec![])
        .unwrap();
    assert!(reg.record_crew("cap-old", "crew-1").unwrap());
    let before_seq = reg.snapshot().seq;
    let probe_times_out = |_: &str| tmux::is_definitively_gone(tmux::SessionLiveness::Unknown);
    let err = reg
        .claim(
            "cap-new",
            Some("t-hub-app"),
            FleetRole::Captain,
            None,
            vec![],
            &probe_times_out,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(
        err.contains("already captained by a LIVE session 'cap-old'"),
        "an ambiguous (timed-out) probe must reject like a live ship, not seize; got: {err}"
    );
    // The incumbent and its crew are untouched; the refusal did not bump the seq.
    assert_eq!(only(&reg).terminal_id.as_deref(), Some("cap-old"));
    assert_eq!(crew_tiles(&only(&reg)), vec!["crew-1".to_string()]);
    assert_eq!(
        reg.snapshot().seq,
        before_seq,
        "a refused seize must not bump the revision"
    );
}

#[test]
fn matching_provider_id_cannot_seize_a_live_incumbent() {
    let reg = CaptainsRegistry::new();
    reg.claim(
        "cap-old",
        Some("shipx"),
        FleetRole::Captain,
        Some("uuid-1"),
        vec![],
        &all_alive,
        &crew_all_alive,
    )
    .unwrap();
    let error = reg
        .claim(
            "cap-new",
            Some("shipx"),
            FleetRole::Captain,
            Some("uuid-1"),
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(error.contains("already captained by a LIVE session"));
    assert_eq!(only(&reg).terminal_id.as_deref(), Some("cap-old"));
    assert_eq!(reg.snapshot().captains.len(), 1);
}

#[test]
fn provider_change_without_runtime_identity_clears_stale_conversation_fields() {
    let reg = CaptainsRegistry::new();
    reg.claim_provider(
        "cap-one",
        Some("shipx"),
        FleetRole::Captain,
        Some("claude"),
        Some("claude-session"),
        vec![],
        &all_alive,
        &crew_all_alive,
    )
    .unwrap();
    let changed = reg
        .claim_provider(
            "cap-one",
            Some("shipx"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap()
        .record;
    assert_eq!(changed.provider.as_deref(), Some("codex"));
    assert!(changed.provider_session_id.is_none());
    assert!(changed.conversation_id.is_none());
    assert!(changed.claude_uuid.is_none());
}

#[test]
fn orphaned_record_is_readopted_by_ship_slug_reclaim() {
    // D4 auto-rebind on resume: after the captain dies (Orphaned), a resumed
    // captain re-claiming the ship SLUG (the always-available trigger, no UUID
    // needed) re-adopts the record → Active and resurrects its Orphaned crew.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("shipx"), vec![]).unwrap();
    assert!(reg.record_crew("cap-old", "crew-1").unwrap());
    assert!(
        reg.remove_session("cap-old").unwrap(),
        "captain death marks orphaned"
    );
    assert!(matches!(only(&reg).state, ClaimState::Orphaned { .. }));

    let out = reg.claim_test("cap-new", Some("shipx"), vec![]).unwrap();
    assert_eq!(out.disposition, ClaimDisposition::ReadoptedOrphan);
    let rec = only(&reg);
    assert_eq!(rec.state, ClaimState::Active);
    assert_eq!(rec.terminal_id.as_deref(), Some("cap-new"));
    assert_eq!(
        rec.crew[0].state,
        CrewState::Active,
        "orphaned crew re-adopted"
    );
}

#[test]
fn readopt_is_gated_on_per_crew_liveness_never_blind_activates() {
    // audit BUG-1: a resumed captain must NOT blind-flip every Orphaned crew to
    // Active - it re-probes each and only re-adopts the ones actually Alive.
    // Alive -> Active, Gone (definitively absent) -> Removed, Unknown (ambiguous
    // probe) -> stays Orphaned (re-adoptable next resume). BYPASS-WOULD-FAIL:
    // restore the blind `cr.state = Active` and the Gone/Unknown crew come back
    // Active -> RED.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("shipx"), vec![]).unwrap();
    for c in ["crew-alive", "crew-gone", "crew-unknown"] {
        assert!(reg.record_crew("cap-old", c).unwrap());
    }
    assert!(
        reg.remove_session("cap-old").unwrap(),
        "captain death orphans the crew"
    );
    assert!(
        only(&reg)
            .crew
            .iter()
            .all(|c| matches!(c.state, CrewState::Orphaned { .. })),
        "all crew start Orphaned"
    );

    // The liveness seam the real handler precomputes lock-free: one verdict per
    // crew tile.
    let crew_liveness = |tile: &str| match tile {
        "crew-alive" => tmux::SessionLiveness::Alive,
        "crew-gone" => tmux::SessionLiveness::Gone,
        _ => tmux::SessionLiveness::Unknown,
    };
    let out = reg
        .claim(
            "cap-new",
            Some("shipx"),
            FleetRole::Captain,
            None,
            vec![],
            &all_alive,
            &crew_liveness,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::ReadoptedOrphan);

    let rec = only(&reg);
    assert_eq!(
        rec.state,
        ClaimState::Active,
        "the captain itself re-activates"
    );
    let state_of = |tile: &str| {
        rec.crew
            .iter()
            .find(|c| c.terminal_id == tile)
            .map(|c| c.state.clone())
            .unwrap()
    };
    assert_eq!(
        state_of("crew-alive"),
        CrewState::Active,
        "Alive -> re-adopted"
    );
    assert!(
        matches!(state_of("crew-gone"), CrewState::Removed { .. }),
        "Gone -> retired, never resurrected"
    );
    assert!(
        matches!(state_of("crew-unknown"), CrewState::Orphaned { .. }),
        "Unknown -> left Orphaned (ambiguous is never seized)"
    );
}

#[test]
fn dead_captain_orphans_crew_and_is_not_scrubbed() {
    // Phase B: death MARKS, it does not scrub (retiring the C4 silent leak). A dead
    // captain's record is retained Orphaned, un-pointed, with its crew Orphaned.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(reg.record_crew("cap-1", "crew-2").unwrap());
    assert!(reg.remove_session("cap-1").unwrap());
    let rec = only(&reg);
    assert!(
        matches!(rec.state, ClaimState::Orphaned { .. }),
        "retained, not scrubbed"
    );
    assert!(rec.terminal_id.is_none(), "un-pointed");
    assert!(
        rec.crew
            .iter()
            .all(|c| matches!(c.state, CrewState::Orphaned { .. })),
        "crew orphaned under the surviving ship, never dropped"
    );
}

#[test]
fn dead_crew_tile_is_marked_removed_not_scrubbed() {
    // A crew's OWN tile dying flips that ref to Removed (retained for telemetry),
    // leaving the live captain + sibling crew untouched.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(reg.record_crew("cap-1", "crew-2").unwrap());
    assert!(reg.remove_session("crew-1").unwrap());
    let rec = only(&reg);
    assert_eq!(rec.state, ClaimState::Active, "captain still alive");
    let c1 = rec.crew.iter().find(|c| c.terminal_id == "crew-1").unwrap();
    let c2 = rec.crew.iter().find(|c| c.terminal_id == "crew-2").unwrap();
    assert!(
        matches!(c1.state, CrewState::Removed { .. }),
        "dead crew retained as Removed"
    );
    assert_eq!(c2.state, CrewState::Active);
    // Removing an unknown session changes nothing (no revision bump).
    let seq = reg.snapshot().seq;
    assert!(!reg.remove_session("nobody").unwrap());
    assert_eq!(reg.snapshot().seq, seq);
}

#[test]
fn record_crew_dedupes_and_reactivates_a_removed_ref() {
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(
        !reg.record_crew("cap-ghost", "crew-1").unwrap(),
        "unclaimed spawner is a no-op"
    );
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(
        !reg.record_crew("cap-1", "crew-1").unwrap(),
        "duplicate Active crew must not re-add"
    );
    // A reused tile id after its ref was Removed re-activates (does not duplicate).
    assert!(reg.remove_session("crew-1").unwrap());
    assert!(
        reg.record_crew("cap-1", "crew-1").unwrap(),
        "reused tile reactivates"
    );
    let rec = only(&reg);
    assert_eq!(rec.crew.len(), 1);
    assert_eq!(rec.crew[0].state, CrewState::Active);
}

#[test]
fn cortana_is_a_first_class_singleton_role() {
    // D1: Cortana is a first-class role, unique registry-wide, NOT a slug hack. A
    // second Cortana claim by a LIVE competitor is rejected; only unambiguous death
    // (or the same session) yields the apex.
    let reg = CaptainsRegistry::new();
    let out = reg
        .claim(
            "cor-1",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.record.role, FleetRole::Cortana);
    assert_eq!(out.record.ship_slug, CORTANA_SLUG);
    // A different LIVE terminal cannot seize the singleton.
    let err = reg
        .claim(
            "cor-2",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(err.contains("LIVE"), "got: {err}");
    // The incumbent dying hands the apex to the resumed Cortana.
    let dead_is_1 = |t: &str| t == "cor-1";
    let out = reg
        .claim(
            "cor-2",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &dead_is_1,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::AutoReleasedDead);
    assert_eq!(out.record.terminal_id.as_deref(), Some("cor-2"));
    assert_eq!(
        reg.snapshot()
            .captains
            .iter()
            .filter(|c| c.role == FleetRole::Cortana)
            .count(),
        1
    );
}

#[test]
fn release_with_crew_becomes_vacant_childless_removes() {
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    // Release with crew: transition to Vacant (re-claimable), crew preserved.
    let released = reg.release("alpha").unwrap();
    assert_eq!(released.state, ClaimState::Vacant);
    assert!(released.terminal_id.is_none());
    assert_eq!(only(&reg).crew.len(), 1, "crew preserved for re-adoption");
    // Re-claiming the vacant ship re-adopts it.
    let out = reg.claim_test("cap-2", Some("alpha"), vec![]).unwrap();
    assert_eq!(out.disposition, ClaimDisposition::ReadoptedOrphan);

    // A childless claim hard-removes on release.
    reg.claim_test("cap-9", Some("beta"), vec![]).unwrap();
    assert_eq!(reg.release("beta").unwrap().ship_slug, "beta");
    assert!(reg
        .snapshot()
        .captains
        .iter()
        .all(|c| c.ship_slug != "beta"));
    // Unknown target is an error, not a silent no-op.
    assert!(reg
        .release("no-such")
        .unwrap_err()
        .contains("no claim matches"));
}

#[test]
fn ship_of_resolves_supervisor_and_crew_across_the_namespace() {
    // Phase D: the cross-ship ownership KEY resolves for both a supervisor terminal
    // and a crew tile (item-1 Phase 3 wires the ACL on top of this).
    let reg = CaptainsRegistry::new();
    reg.claim(
        "cap-1",
        Some("shipx"),
        FleetRole::Captain,
        None,
        vec![],
        &all_alive,
        &crew_all_alive,
    )
    .unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert_eq!(
        reg.ship_of("cap-1"),
        Some(ShipMembership::Supervisor {
            ship_slug: "shipx".into(),
            role: FleetRole::Captain
        })
    );
    assert_eq!(
        reg.ship_of("crew-1"),
        Some(ShipMembership::Crew {
            ship_slug: "shipx".into()
        })
    );
    assert_eq!(reg.ship_of("nobody"), None);
    // A Removed crew tile no longer resolves.
    assert!(reg.remove_session("crew-1").unwrap());
    assert_eq!(reg.ship_of("crew-1"), None);
}

#[test]
fn backfill_uuid_fills_only_a_none_anchor() {
    // MED-7: the async-resolved anchor is backfilled once, never overwritten.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("shipx"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(reg.backfill_uuid("cap-1", "uuid-cap").unwrap());
    assert!(reg.backfill_uuid("crew-1", "uuid-crew").unwrap());
    let rec = only(&reg);
    assert_eq!(rec.claude_uuid.as_deref(), Some("uuid-cap"));
    assert_eq!(rec.crew[0].claude_uuid.as_deref(), Some("uuid-crew"));
    // A second backfill of an already-resolved anchor is a no-op (no seq bump).
    let seq = reg.snapshot().seq;
    assert!(!reg.backfill_uuid("cap-1", "uuid-other").unwrap());
    assert_eq!(reg.snapshot().seq, seq);
    assert_eq!(only(&reg).claude_uuid.as_deref(), Some("uuid-cap"));
}
