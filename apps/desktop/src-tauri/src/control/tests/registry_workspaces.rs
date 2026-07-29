use super::*;

#[test]
fn workspace_kind_migrates_and_reserved_workspace_is_canonical() {
    let legacy: TabRecord = serde_json::from_value(json!({
        "id": CAPTAIN_WORKSPACE_ID,
        "name": "Captains",
        "order": ["captain-a"]
    }))
    .unwrap();
    let wire = serde_json::to_value(&legacy).unwrap();
    assert_eq!(wire["schemaVersion"], WORKSPACE_SCHEMA_VERSION);
    assert_eq!(wire["kind"], "captain");
    assert_eq!(wire["name"], CAPTAIN_WORKSPACE_NAME);
    assert_eq!(wire["tileIds"], json!(["captain-a"]));

    let tabs = TabRegistry::new();
    tabs.replace(vec![
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: "Captains".into(),
            tile_ids: vec!["captain-a".into()],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: "duplicate".into(),
            tile_ids: vec!["captain-a".into(), "captain-b".into()],
        },
    ]);
    let snapshot = tabs.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].name, CAPTAIN_WORKSPACE_NAME);
    assert_eq!(snapshot[0].tile_ids, vec!["captain-a", "captain-b"]);
    assert!(tabs
        .rename_tab(CAPTAIN_WORKSPACE_ID, "Other")
        .unwrap_err()
        .contains("cannot be renamed"));
    assert!(tabs
        .remove_tab(CAPTAIN_WORKSPACE_ID, true)
        .unwrap_err()
        .contains("cannot be closed"));
    assert!(serde_json::from_value::<TabRecord>(json!({
        "schemaVersion": 1,
        "id": "work-a",
        "name": "Work A",
        "kind": "captain",
        "tileIds": []
    }))
    .unwrap_err()
    .to_string()
    .contains("conflicts"));
}

#[test]
fn legacy_crew_workspace_reconciliation_is_exact_or_needs_assignment() {
    let reg = CaptainsRegistry::new();
    reg.claim_test(
        "captain-a",
        Some("alpha"),
        vec!["work-a".into(), "work-b".into()],
    )
    .unwrap();
    reg.record_crew("captain-a", "crew-exact").unwrap();
    reg.record_crew("captain-a", "crew-ambiguous").unwrap();
    let mut tabs = vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec!["crew-exact".into()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec!["captain-a".into(), "crew-ambiguous".into()],
        },
    ];
    assert!(reg.reconcile_crew_workspaces(&mut tabs).unwrap());
    let captain = &reg.snapshot().captains[0];
    let exact = captain
        .crew
        .iter()
        .find(|crew| crew.terminal_id == "crew-exact")
        .unwrap();
    assert_eq!(exact.workspace_tab_id.as_deref(), Some("work-a"));
    assert_eq!(exact.state, CrewState::Active);
    let ambiguous = captain
        .crew
        .iter()
        .find(|crew| crew.terminal_id == "crew-ambiguous")
        .unwrap();
    assert!(matches!(ambiguous.state, CrewState::NeedsAssignment { .. }));
    assert!(tabs
        .iter()
        .all(|tab| !tab.tile_ids.iter().any(|id| id == "crew-ambiguous")));
    assert!(!reg.reconcile_crew_workspaces(&mut tabs).unwrap());
}

#[test]
fn work_workspace_ownership_is_globally_exclusive_sequentially_and_concurrently() {
    let sequential = CaptainsRegistry::new();
    sequential
        .claim_test("captain-a", Some("alpha"), vec!["shared-work".into()])
        .unwrap();
    let before = sequential.snapshot();
    let error = sequential
        .claim_test("captain-b", Some("beta"), vec!["shared-work".into()])
        .unwrap_err();
    assert!(error.contains("already owned"), "got: {error}");
    assert_eq!(sequential.snapshot().seq, before.seq);
    assert_eq!(sequential.snapshot().captains, before.captains);

    let concurrent = Arc::new(CaptainsRegistry::new());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let joins = [("captain-a", "alpha"), ("captain-b", "beta")]
        .into_iter()
        .map(|(terminal, ship)| {
            let registry = Arc::clone(&concurrent);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                registry.claim_test(terminal, Some(ship), vec!["shared-work".into()])
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let snapshot = concurrent.snapshot();
    assert_eq!(snapshot.captains.len(), 1);
    assert_eq!(snapshot.captains[0].workspace_tab_ids, vec!["shared-work"]);
}

#[test]
fn schema_load_rejects_duplicate_global_workspace_ownership() {
    let path = captains_tmp("duplicate-global-workspace-owner");
    let source = CaptainsRegistry::load(path.clone());
    source
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    source
        .claim_test("captain-b", Some("beta"), vec!["work-b".into()])
        .unwrap();
    let mut invalid = source.snapshot();
    invalid.captains[1].workspace_tab_ids = vec!["work-a".into()];
    std::fs::write(&path, serde_json::to_vec_pretty(&invalid).unwrap()).unwrap();
    let _ = std::fs::remove_file(path.with_extension("json.bak"));

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert!(
        restored.captains.is_empty(),
        "ambiguous persisted ownership must fail closed instead of selecting an owner"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn cross_project_workspace_reports_and_moves_are_rejected_without_effect() {
    let registry = Arc::new(CaptainsRegistry::new());
    for (project_id, name) in [("project-a", "A"), ("project-b", "B")] {
        registry
            .upsert_project(ProjectRecord {
                root_path: None,
                vcs_capability: None,
                git_main_root: None,
                project_id: project_id.into(),
                name: name.into(),
                repo_root: format!("/tmp/{project_id}"),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    registry
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    registry
        .claim_test("captain-b", Some("beta"), vec!["work-b".into()])
        .unwrap();
    registry
        .bind_ship_context("alpha", "project-a", "Assignment A", "codex")
        .unwrap();
    registry
        .bind_ship_context("beta", "project-b", "Assignment B", "codex")
        .unwrap();
    registry.record_crew("captain-b", "crew-b").unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: vec!["crew-b".into()],
        },
    ]);
    let ctx = test_ctx("cross-project-workspace")
        .with_captains_registry(Arc::clone(&registry))
        .with_tab_registry(Arc::clone(&tabs));
    let before_tabs = tabs.snapshot_full();
    let before_captains = registry.snapshot();

    let report_error = dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-b"]},
                    {"id": "work-b", "name": "Work B", "tileIds": []},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a", "captain-b"]}
                ]
            }),
        )
        .unwrap_err();
    assert!(report_error.contains("not owned"), "got: {report_error}");
    let after_report_tabs = tabs.snapshot_full();
    assert_eq!(after_report_tabs.seq, before_tabs.seq);
    assert_eq!(after_report_tabs.active_tab_id, before_tabs.active_tab_id);
    assert_eq!(
        serde_json::to_value(after_report_tabs.tabs).unwrap(),
        serde_json::to_value(&before_tabs.tabs).unwrap()
    );
    assert_eq!(registry.snapshot().seq, before_captains.seq);
    assert_eq!(registry.snapshot().captains, before_captains.captains);

    let move_error = dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": "crew-b", "tabId": "work-a"}),
    )
    .unwrap_err();
    assert!(move_error.contains("not owned"), "got: {move_error}");
    let after_move_tabs = tabs.snapshot_full();
    assert_eq!(after_move_tabs.seq, before_tabs.seq);
    assert_eq!(after_move_tabs.active_tab_id, before_tabs.active_tab_id);
    assert_eq!(
        serde_json::to_value(after_move_tabs.tabs).unwrap(),
        serde_json::to_value(&before_tabs.tabs).unwrap()
    );
    assert_eq!(registry.snapshot().seq, before_captains.seq);
}

#[test]
fn authenticated_workspace_mutations_are_scoped_to_exact_caller_assignment() {
    let captains = Arc::new(CaptainsRegistry::new());
    for (project_id, name) in [("project-a", "A"), ("project-b", "B")] {
        captains
            .upsert_project(ProjectRecord {
                root_path: None,
                vcs_capability: None,
                git_main_root: None,
                project_id: project_id.into(),
                name: name.into(),
                repo_root: format!("/tmp/{project_id}"),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    for (terminal, ship, project, workspace) in [
        ("captain-a", "alpha", "project-a", "work-a"),
        ("captain-b", "beta", "project-b", "work-b"),
    ] {
        captains
            .claim_test(terminal, Some(ship), vec![workspace.into()])
            .unwrap();
        captains
            .bind_ship_context(ship, project, &format!("Assignment {ship}"), "codex")
            .unwrap();
    }
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(captains.workspace_projection());
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let captain_a = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "alpha",
        "captain-a",
    );
    let context = test_ctx("workspace-project-auth")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_identity_store(identities);
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();

    for (command, args) in [
        ("focus_session", json!({"sessionId": "captain-b"})),
        ("focus_tab", json!({"tabId": "work-b"})),
        (
            "move_tile",
            json!({"terminalId": "ordinary-b", "tabId": "work-b"}),
        ),
        ("rename_tab", json!({"tabId": "work-b", "name": "Stolen"})),
        ("close_tab", json!({"tabId": "work-b"})),
        (
            "new_tab",
            json!({"name": "Foreign", "projectId": "project-b", "shipSlug": "beta"}),
        ),
        (
            "report_workspace_tabs",
            json!({"baseSeq": before_tabs.seq, "tabs": before_tabs.tabs}),
        ),
    ] {
        let response = dispatch_authenticated(
            &context,
            req_session("workspace-project-auth", &captain_a, command, args),
        );
        assert!(!response.ok, "{command} unexpectedly crossed Project scope");
        assert!(response.error.unwrap_or_default().contains("acl:"));
    }
    for (command, args) in [
        (
            "rename_tab",
            json!({"tabId": "work-b", "name": "No Session"}),
        ),
        ("close_tab", json!({"tabId": "work-b"})),
    ] {
        let response = dispatch_authenticated(
            &context,
            req_untrusted("workspace-project-auth", "", command, args),
        );
        assert!(
            !response.ok,
            "unattributed {command} unexpectedly succeeded"
        );
        assert!(response
            .error
            .unwrap_or_default()
            .contains("requires a valid T_HUB_SESSION_TOKEN"));
    }
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(captains.snapshot().captains, before_captains.captains);
    assert_eq!(captains.snapshot().workspaces, before_captains.workspaces);
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);

    let created = dispatch_authenticated(
        &context,
        req_session(
            "workspace-project-auth",
            &captain_a,
            "new_tab",
            json!({"name": "Owned A"}),
        ),
    );
    assert!(created.ok, "{:?}", created.error);
    let tab_id = created.result.unwrap()["tabId"]
        .as_str()
        .unwrap()
        .to_string();
    let durable = captains.snapshot();
    let workspace = durable
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tab_id)
        .unwrap();
    assert_eq!(
        workspace.owner.as_ref().unwrap(),
        &FleetWorkspaceOwner {
            project_id: "project-a".into(),
            assignment_id: "assignment:project-a:alpha".into(),
            ship_slug: "alpha".into(),
        }
    );
}

fn report_reconciliation_fixture(
    tag: &str,
) -> (ControlContext, Arc<CaptainsRegistry>, Arc<TabRegistry>) {
    let captains = Arc::new(CaptainsRegistry::load(captains_tmp(tag)));
    captains
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    captains.record_crew("captain-a", "crew-a").unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: vec!["crew-a".into()],
    }]);
    let context = test_ctx(tag)
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    (context, captains, tabs)
}

#[test]
fn workspace_reports_require_organization_capability_before_mutation() {
    let (base, captains, tabs) = report_reconciliation_fixture("report-read-tier");
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let crew = mint_session(&identities, crate::identity::Role::Crew, "alpha", "crew-a");
    let context = base.with_identity_store(identities);
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    let response = dispatch_authenticated(
        &context,
        req_session(
            "read-report-read-tier",
            &crew,
            "report_workspace_tabs",
            json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
                ]
            }),
        ),
    );
    assert!(!response.ok, "a read Crew must not mutate Workspace state");
    assert!(!response.error.unwrap_or_default().is_empty());
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);
}

#[test]
fn invalid_workspace_reports_leave_tabs_captains_and_sequences_unchanged() {
    for (tag, report) in [
        (
            "invalid-occupant",
            json!({"tabs": [
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": []}
            ]}),
        ),
        (
            "duplicate-id",
            json!({"tabs": [
                {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                {"id": "work-a", "name": "Duplicate", "tileIds": []},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
            ]}),
        ),
        (
            "future-schema",
            json!({"tabs": [
                {"schemaVersion": WORKSPACE_SCHEMA_VERSION + 1, "id": "work-a", "name": "Work A", "kind": "work", "tileIds": ["crew-a"]}
            ]}),
        ),
    ] {
        let (context, captains, tabs) = report_reconciliation_fixture(tag);
        let before_captains = captains.snapshot();
        let before_tabs = tabs.snapshot_full();
        let mut report = report;
        report["baseSeq"] = json!(before_tabs.seq);
        assert!(dispatch(&context, "report_workspace_tabs", &report).is_err());
        let after_captains = captains.snapshot();
        let after_tabs = tabs.snapshot_full();
        assert_eq!(after_captains.seq, before_captains.seq, "case {tag}");
        assert_eq!(
            after_captains.captains, before_captains.captains,
            "case {tag}"
        );
        assert_eq!(after_tabs.seq, before_tabs.seq, "case {tag}");
        assert_eq!(
            serde_json::to_value(after_tabs.tabs).unwrap(),
            serde_json::to_value(before_tabs.tabs).unwrap(),
            "case {tag}"
        );
    }
}

#[test]
fn stale_workspace_report_cas_cannot_commit_crew_reconciliation() {
    let (context, captains, tabs) = report_reconciliation_fixture("report-stale-cas");
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    tabs.insert_tab("racing-work", "Racing Work");

    let response = dispatch(
            &context,
            "report_workspace_tabs",
            &json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
                ]
            }),
        )
        .unwrap();
    assert_eq!(response["stale"], true);
    let after_captains = captains.snapshot();
    assert_eq!(after_captains.seq, before_captains.seq);
    assert_eq!(after_captains.captains, before_captains.captains);
    let after_tabs = tabs.snapshot_full();
    assert_eq!(after_tabs.seq, before_tabs.seq + 1);
    assert!(after_tabs.tabs.iter().any(|tab| tab.id == "racing-work"));
}

#[test]
fn workspace_report_persistence_failure_rolls_back_both_registries() {
    let (context, captains, tabs) = report_reconciliation_fixture("report-persist-fail");
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    captains.fail_next_persist("workspace report persistence failure");
    let error = dispatch(
            &context,
            "report_workspace_tabs",
            &json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
                ]
            }),
        )
        .unwrap_err();
    assert!(error.contains("workspace report persistence failure"));
    let after_captains = captains.snapshot();
    let after_tabs = tabs.snapshot_full();
    assert_eq!(after_captains.seq, before_captains.seq);
    assert_eq!(after_captains.captains, before_captains.captains);
    assert_eq!(after_tabs.seq, before_tabs.seq);
    assert_eq!(
        serde_json::to_value(after_tabs.tabs).unwrap(),
        serde_json::to_value(before_tabs.tabs).unwrap()
    );
}

#[test]
fn empty_backend_restart_report_reconciles_a_durable_captain_from_stale_work_placement() {
    let path = captains_tmp("captain-relocation-crash");
    CaptainsRegistry::load(path.clone())
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = test_ctx("captain-relocation-crash")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_apply_sink(sink.clone());
    let startup = tabs.snapshot_full();
    assert!(
        startup.tabs.is_empty(),
        "production starts with no backend tabs"
    );

    let response = dispatch(
        &context,
        "report_workspace_tabs",
        &json!({
            "baseSeq": startup.seq,
            "tabs": [
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": []}
            ],
            "activeTabId": "work-a"
        }),
    )
    .unwrap();
    assert_eq!(response["stale"], true);
    assert_eq!(
        response["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tab| tab["id"] == CAPTAIN_WORKSPACE_ID)
            .unwrap()["tileIds"],
        json!(["captain-a"])
    );
    let converged = tabs.snapshot_full();
    assert!(!converged.tabs[0].tile_ids.contains(&"captain-a".into()));
    assert_eq!(
        converged
            .tabs
            .iter()
            .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
            .unwrap()
            .tile_ids,
        vec!["captain-a".to_string()]
    );
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "sync_captains");
    assert!(calls[0].1["sync"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .any(|workspace| workspace["id"] == "work-a"));

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn empty_backend_restart_rejects_foreign_or_duplicate_supervisor_placement_without_effect() {
    for (tag, reported_tabs) in [
        (
            "foreign",
            json!([
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["foreign-captain"]}
            ]),
        ),
        (
            "duplicate",
            json!([
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
            ]),
        ),
    ] {
        let path = captains_tmp(&format!("empty-restart-{tag}"));
        CaptainsRegistry::load(path.clone())
            .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
            .unwrap();
        let captains = Arc::new(CaptainsRegistry::load(path.clone()));
        let tabs = Arc::new(TabRegistry::new());
        let context = test_ctx(&format!("empty-restart-{tag}"))
            .with_captains_registry(Arc::clone(&captains))
            .with_tab_registry(Arc::clone(&tabs));
        let before_captains = captains.snapshot();
        let before_tabs = tabs.snapshot_full();

        assert!(dispatch(
            &context,
            "report_workspace_tabs",
            &json!({"baseSeq": 0, "tabs": reported_tabs}),
        )
        .is_err());
        assert_eq!(captains.snapshot().seq, before_captains.seq, "case {tag}");
        assert_eq!(
            captains.snapshot().captains,
            before_captains.captains,
            "case {tag}"
        );
        assert_eq!(tabs.snapshot_full().seq, before_tabs.seq, "case {tag}");
        assert_eq!(
            serde_json::to_value(tabs.snapshot_full().tabs).unwrap(),
            serde_json::to_value(before_tabs.tabs).unwrap(),
            "case {tag}"
        );

        let _ = std::fs::remove_file(path.with_extension("json.bak"));
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn durable_fleet_workspaces_seed_list_tabs_before_the_first_frontend_report() {
    let path = captains_tmp("durable-workspace-projection");
    let initial = CaptainsRegistry::load(path.clone());
    initial
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-a".into(),
            name: "Project A".into(),
            repo_root: "/tmp/project-a".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    initial
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    initial
        .bind_ship_context("alpha", "project-a", "Assignment A", "codex")
        .unwrap();
    initial.record_crew("captain-a", "crew-a").unwrap();
    initial
        .bind_crew_context_exact(
            "captain-a",
            "crew-a",
            "durable placement",
            "codex",
            None,
            None,
            Some("work-a"),
            PowderWorkBinding {
                card_id: "card-a".into(),
                run_id: "run-a".into(),
                agent: Some("agent-a".into()),
                claim_expires_at: Some(1),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
            None,
            None,
        )
        .unwrap();
    drop(initial);

    let restarted = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(restarted.workspace_projection());
    let context = test_ctx("durable-workspace-projection")
        .with_captains_registry(Arc::clone(&restarted))
        .with_tab_registry(Arc::clone(&tabs));
    let listed = dispatch(&context, "list_tabs", &Value::Null).unwrap();
    assert!(listed["tabs"].as_array().unwrap().iter().any(|workspace| {
        workspace["id"] == "work-a" && workspace["tileIds"] == json!(["crew-a"])
    }));
    assert!(listed["tabs"].as_array().unwrap().iter().any(|workspace| {
        workspace["id"] == CAPTAIN_WORKSPACE_ID
            && workspace["name"] == CAPTAIN_WORKSPACE_NAME
            && workspace["tileIds"] == json!(["captain-a"])
    }));
    let durable = restarted.snapshot();
    let owner = durable
        .workspaces
        .iter()
        .find(|workspace| workspace.id == "work-a")
        .unwrap()
        .owner
        .as_ref()
        .unwrap();
    assert_eq!(owner.project_id, "project-a");
    assert_eq!(owner.assignment_id, "assignment:project-a:alpha");
    assert_eq!(owner.ship_slug, "alpha");
    assert!(durable.pending_fleet_operations.is_empty());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

fn durable_close_workspace_fixture(
    tag: &str,
    workspace_ids: &[&str],
) -> (
    PathBuf,
    Arc<CaptainsRegistry>,
    Arc<TabRegistry>,
    ControlContext,
) {
    let path = captains_tmp(tag);
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-a".into(),
            name: "Project A".into(),
            repo_root: "/tmp/project-a".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    captains
        .claim_test(
            "captain-a",
            Some("alpha"),
            workspace_ids.iter().map(|id| (*id).to_string()).collect(),
        )
        .unwrap();
    captains
        .bind_ship_context("alpha", "project-a", "Assignment A", "codex")
        .unwrap();
    captains.record_crew("captain-a", "crew-a").unwrap();
    captains
        .bind_crew_context_exact(
            "captain-a",
            "crew-a",
            "close Workspace recovery",
            "codex",
            None,
            None,
            Some("work-a"),
            PowderWorkBinding {
                card_id: "card-a".into(),
                run_id: "run-a".into(),
                agent: Some("agent-a".into()),
                claim_expires_at: Some(1),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
            None,
            None,
        )
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(captains.workspace_projection());
    let context = test_ctx(tag)
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    (path, captains, tabs, context)
}

#[test]
fn force_close_atomically_rehomes_crew_and_restart_projects_the_committed_state() {
    let (path, captains, tabs, context) =
        durable_close_workspace_fixture("force-close-rehome", &["work-a", "work-b"]);
    let before_projection = tabs.snapshot_full();

    let error = dispatch(
        &context,
        "close_tab",
        &json!({
            "tabId": "work-a",
            "force": true,
            "testCrashAfterFleetCommit": true,
        }),
    )
    .unwrap_err();
    assert!(error.contains("injected crash"));
    assert_eq!(tabs.snapshot_full().seq, before_projection.seq);
    assert!(tabs
        .snapshot()
        .iter()
        .any(|workspace| workspace.id == "work-a"));

    let committed = captains.snapshot();
    assert!(committed
        .workspaces
        .iter()
        .all(|workspace| workspace.id != "work-a"));
    let crew = &committed.captains[0].crew[0];
    assert_eq!(crew.workspace_tab_id.as_deref(), Some("work-b"));
    assert!(matches!(crew.state, CrewState::Active));

    drop(context);
    drop(tabs);
    drop(captains);
    let restarted = CaptainsRegistry::load(path.clone());
    let restarted_tabs = TabRegistry::new();
    restarted_tabs.replace(restarted.workspace_projection());
    let projected = restarted_tabs.snapshot();
    assert!(projected.iter().all(|workspace| workspace.id != "work-a"));
    let work_b = projected
        .iter()
        .find(|workspace| workspace.id == "work-b")
        .unwrap();
    assert_eq!(work_b.tile_ids, vec!["crew-a"]);

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn force_close_with_ambiguous_rehome_persists_needs_assignment_and_rolls_back_on_failure() {
    let (path, captains, tabs, context) =
        durable_close_workspace_fixture("force-close-ambiguous", &["work-a", "work-b", "work-c"]);
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    captains.fail_next_persist("force close persistence failure");
    let error = dispatch(
        &context,
        "close_tab",
        &json!({"tabId": "work-a", "force": true}),
    )
    .unwrap_err();
    assert!(error.contains("force close persistence failure"));
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(captains.snapshot().captains, before_captains.captains);
    assert_eq!(captains.snapshot().workspaces, before_captains.workspaces);
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);

    dispatch(
        &context,
        "close_tab",
        &json!({"tabId": "work-a", "force": true}),
    )
    .unwrap();
    let committed = captains.snapshot();
    let crew = &committed.captains[0].crew[0];
    assert_eq!(crew.workspace_tab_id, None);
    assert!(matches!(crew.state, CrewState::NeedsAssignment { .. }));
    assert!(tabs
        .snapshot()
        .iter()
        .all(|workspace| !workspace.tile_ids.contains(&"crew-a".to_string())));

    drop(context);
    drop(tabs);
    drop(captains);
    let restarted = CaptainsRegistry::load(path.clone());
    let crew = &restarted.snapshot().captains[0].crew[0];
    assert_eq!(crew.workspace_tab_id, None);
    assert!(matches!(crew.state, CrewState::NeedsAssignment { .. }));

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn startup_prune_keeps_projection_live_only_across_persistence_failure() {
    let path = captains_tmp("startup-workspace-tile-prune");
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_test("captain-gone", Some("startup-failure-ship"), vec![])
        .unwrap();
    registry
        .adopt_unowned_workspace_projection(&[
            TabRecord {
                id: "work-a".into(),
                name: "Work A".into(),
                tile_ids: vec!["live-a".into(), "gone-a".into()],
            },
            TabRecord {
                id: "work-b".into(),
                name: "Work B".into(),
                tile_ids: vec!["gone-b".into()],
            },
        ])
        .unwrap();

    assert_eq!(
        registry.snapshot().captains[0].terminal_id.as_deref(),
        Some("captain-gone")
    );
    registry.fail_next_persist("startup workspace prune persistence failure");
    let error = registry
        .prune_gone_workspace_tiles(|tile| tile == "live-a")
        .unwrap_err();
    assert!(error.contains("startup workspace prune persistence failure"));
    let filtered_snapshot = registry.snapshot();
    assert!(filtered_snapshot.captains[0].terminal_id.is_none());
    assert!(matches!(
        filtered_snapshot.captains[0].state,
        ClaimState::Orphaned { .. }
    ));
    let rolled_back_projection = registry.workspace_projection();
    assert_eq!(
        rolled_back_projection
            .iter()
            .find(|workspace| workspace.id == "work-a")
            .unwrap()
            .tile_ids,
        vec!["live-a"]
    );
    assert!(rolled_back_projection
        .iter()
        .find(|workspace| workspace.id == "work-b")
        .unwrap()
        .tile_ids
        .is_empty());

    registry
        .rename_workspace("work-a", "Work A renamed")
        .unwrap();
    let committed_seq = registry.snapshot().seq;
    assert!(registry
        .prune_gone_workspace_tiles(|tile| tile == "live-a")
        .unwrap()
        .is_empty());
    assert_eq!(registry.snapshot().seq, committed_seq);

    drop(registry);
    let restarted = CaptainsRegistry::load(path.clone());
    let tabs = TabRegistry::new();
    tabs.replace(restarted.workspace_projection());
    let projection = tabs.snapshot();
    assert_eq!(
        projection
            .iter()
            .find(|workspace| workspace.id == "work-a")
            .unwrap()
            .tile_ids,
        vec!["live-a"]
    );
    assert!(projection
        .iter()
        .find(|workspace| workspace.id == "work-b")
        .unwrap()
        .tile_ids
        .is_empty());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn startup_reconciliation_retries_when_workspaces_change_after_liveness_snapshot() {
    let registry = CaptainsRegistry::new();
    registry
        .claim_test("old-live", Some("old-live-ship"), vec![])
        .unwrap();
    let stale_basis = registry.startup_workspace_reconciliation_basis();
    registry
        .claim_test("new-live", Some("new-live-ship"), vec![])
        .unwrap();

    let published = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        registry
            .reconcile_startup_workspace_tiles(
                &stale_basis,
                |tile| tile == "old-live",
                |_| published.store(true, Ordering::Release),
            )
            .unwrap(),
        None
    );
    assert!(!published.load(Ordering::Acquire));
    assert!(registry
        .workspace_projection()
        .iter()
        .flat_map(|workspace| workspace.tile_ids.iter())
        .any(|tile| tile == "new-live"));

    let current_basis = registry.startup_workspace_reconciliation_basis();
    assert_eq!(
        registry
            .reconcile_startup_workspace_tiles(
                &current_basis,
                |tile| matches!(tile, "old-live" | "new-live"),
                |_| published.store(true, Ordering::Release),
            )
            .unwrap(),
        Some(Vec::new())
    );
    assert!(published.load(Ordering::Acquire));
}

#[test]
fn startup_reconciliation_ignores_unrelated_registry_changes() {
    let registry = CaptainsRegistry::new();
    registry
        .claim_test("captain-live", Some("live-ship"), vec![])
        .unwrap();
    let basis = registry.startup_workspace_reconciliation_basis();
    registry
        .checkpoint(
            Some("captain-live"),
            None,
            None,
            None,
            Some("active checkpoint"),
        )
        .unwrap();

    let published = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        registry
            .reconcile_startup_workspace_tiles(
                &basis,
                |tile| tile == "captain-live",
                |_| published.store(true, Ordering::Release),
            )
            .unwrap(),
        Some(Vec::new())
    );
    assert!(published.load(Ordering::Acquire));
}

#[test]
fn startup_prune_reconciles_gone_managed_tiles_and_preserves_live_crew() {
    let path = captains_tmp("startup-managed-workspace-tile-prune");
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .upsert_project(ProjectRecord {
            project_id: "project-startup-prune".into(),
            name: "Startup prune".into(),
            repo_root: "/tmp/startup-prune".into(),
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    registry
        .claim_test("captain-gone", Some("startup-ship"), vec![])
        .unwrap();
    registry
        .bind_ship_context(
            "startup-ship",
            "project-startup-prune",
            "Reconcile startup",
            "codex",
        )
        .unwrap();
    let captain = registry.snapshot().captains[0].clone();
    registry
        .create_workspace(
            "work-managed",
            "Managed work",
            Some(&FleetWorkspaceOwner {
                project_id: captain.project_id.clone().unwrap(),
                assignment_id: captain.assignment_id.clone(),
                ship_slug: captain.ship_slug.clone(),
            }),
        )
        .unwrap();
    for crew in ["crew-gone", "crew-live"] {
        registry.record_crew("captain-gone", crew).unwrap();
        registry.move_workspace_tile(crew, "work-managed").unwrap();
    }

    assert_eq!(
        registry
            .prune_gone_workspace_tiles(|tile| tile == "crew-live")
            .unwrap(),
        vec!["captain-gone".to_string(), "crew-gone".to_string()]
    );
    let snapshot = registry.snapshot();
    let captain = &snapshot.captains[0];
    assert!(captain.terminal_id.is_none());
    assert!(matches!(captain.state, ClaimState::Orphaned { .. }));
    let gone = captain
        .crew
        .iter()
        .find(|crew| crew.terminal_id == "crew-gone")
        .unwrap();
    assert!(matches!(gone.state, CrewState::Removed { .. }));
    assert!(gone.workspace_tab_id.is_none());
    let live = captain
        .crew
        .iter()
        .find(|crew| crew.terminal_id == "crew-live")
        .unwrap();
    assert!(matches!(live.state, CrewState::Orphaned { .. }));
    assert_eq!(live.workspace_tab_id.as_deref(), Some("work-managed"));
    let projection = registry.workspace_projection();
    assert!(projection
        .iter()
        .find(|workspace| workspace.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .is_empty());
    assert_eq!(
        projection
            .iter()
            .find(|workspace| workspace.id == "work-managed")
            .unwrap()
            .tile_ids,
        vec!["crew-live"]
    );

    drop(registry);
    let restarted = CaptainsRegistry::load(path.clone());
    assert_eq!(
        restarted
            .workspace_projection()
            .iter()
            .find(|workspace| workspace.id == "work-managed")
            .unwrap()
            .tile_ids,
        vec!["crew-live"]
    );

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn startup_prune_orphans_a_gone_cortana_before_projection() {
    let path = captains_tmp("startup-gone-cortana-prune");
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim(
            "cortana-gone",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();

    assert_eq!(
        registry.prune_gone_workspace_tiles(|_| false).unwrap(),
        vec!["cortana-gone".to_string()]
    );
    let snapshot = registry.snapshot();
    let cortana = snapshot
        .captains
        .iter()
        .find(|captain| captain.role == FleetRole::Cortana)
        .unwrap();
    assert!(matches!(cortana.state, ClaimState::Orphaned { .. }));
    assert!(cortana.terminal_id.is_none());
    assert!(registry
        .workspace_projection()
        .iter()
        .find(|workspace| workspace.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .is_empty());

    drop(registry);
    let restarted = CaptainsRegistry::load(path.clone());
    let cortana = restarted
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| captain.role == FleetRole::Cortana)
        .unwrap();
    assert!(matches!(cortana.state, ClaimState::Orphaned { .. }));
    assert!(cortana.terminal_id.is_none());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn startup_prune_preserves_cleanup_recovery_without_restoring_crew_tile() {
    let path = captains_tmp("startup-cleanup-pending-tile-prune");
    let source = powder_lifecycle_registry(None);
    let mut snapshot = source.snapshot();
    snapshot.captains[0].workspace_tab_ids = vec!["work-recovery".into()];
    let crew = &mut snapshot.captains[0].crew[0];
    crew.workspace_tab_id = Some("work-recovery".into());
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: "profile-that-does-not-exist-for-control-tests".into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "startup-prune-release".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    snapshot.workspaces =
        CaptainsRegistry::reconcile_durable_workspaces(&snapshot.captains, snapshot.workspaces);
    snapshot.seq = snapshot.seq.saturating_add(1);
    CaptainsRegistry::validate_snapshot(&snapshot).unwrap();
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert_eq!(
        registry
            .prune_gone_workspace_tiles(|tile| tile == "captain-powder")
            .unwrap(),
        vec!["crew-powder".to_string()]
    );
    let committed = registry.snapshot();
    let crew = &committed.captains[0].crew[0];
    assert!(matches!(crew.state, CrewState::CleanupPending { .. }));
    assert!(crew.workspace_tab_id.is_none());
    assert_eq!(committed.pending_dispatch_releases.len(), 1);
    assert!(registry
        .workspace_projection()
        .iter()
        .find(|workspace| workspace.id == "work-recovery")
        .unwrap()
        .tile_ids
        .is_empty());

    registry
        .rename_workspace("work-recovery", "Recovery renamed")
        .unwrap();
    assert!(registry
        .workspace_projection()
        .iter()
        .find(|workspace| workspace.id == "work-recovery")
        .unwrap()
        .tile_ids
        .is_empty());
    drop(registry);
    let restarted = CaptainsRegistry::load(path.clone());
    let crew = &restarted.snapshot().captains[0].crew[0];
    assert!(matches!(crew.state, CrewState::CleanupPending { .. }));
    assert!(crew.workspace_tab_id.is_none());
    assert!(restarted
        .workspace_projection()
        .iter()
        .find(|workspace| workspace.id == "work-recovery")
        .unwrap()
        .tile_ids
        .is_empty());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}
