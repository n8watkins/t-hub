use super::*;

#[test]
fn history_list_control_contract_discovers_codex_without_hiding_claude() {
    let temp = tempfile::tempdir().unwrap();
    let claude_root = temp.path().join(".claude/projects/repo");
    let codex_root = temp.path().join(".codex/sessions/2026/07/20");
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::create_dir_all(&codex_root).unwrap();
    std::fs::write(
        claude_root.join("claude-control.jsonl"),
        r#"{"type":"user","cwd":"/same","message":{"content":"Claude control"}}"#,
    )
    .unwrap();
    let codex_id = "22222222-2222-4222-8222-222222222222";
    std::fs::write(
            codex_root.join(format!(
                "rollout-2026-07-20T10-00-00-{codex_id}.jsonl"
            )),
            format!(
                "{}\n{}",
                json!({"type":"session_meta","payload":{"id":codex_id,"cwd":"/same","model_provider":"openai"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Codex control"}})
            ),
        )
        .unwrap();
    let history = Arc::new(crate::history::HistoryService::new(
        temp.path().join(".claude/projects"),
        temp.path().join(".codex/sessions"),
        std::time::Duration::from_secs(60),
    ));
    let ctx = test_ctx("history-list").with_history_service(history);

    let value = dispatch(&ctx, "history_list", &json!({"limit": 10})).unwrap();

    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["count"], 2);
    assert_eq!(value["total"], 2);
    assert_eq!(value["entries"].as_array().unwrap().len(), 2);
    assert!(value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["harness"] == "codex" && entry["conversationId"] == codex_id));
    assert!(value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["harness"] == "claude" && entry["conversationId"] == "claude-control"));
    assert_eq!(value["sources"].as_array().unwrap().len(), 2);
}

#[test]
fn authenticated_history_list_is_scoped_to_an_active_captain_assignment() {
    let temp = tempfile::tempdir().unwrap();
    let claude_root = temp.path().join(".claude/projects");
    let codex_root = temp.path().join(".codex/sessions/2026/07/20");
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::create_dir_all(&codex_root).unwrap();
    let ids = [
        "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333",
    ];
    for (index, id) in ids.iter().enumerate() {
        std::fs::write(
                codex_root.join(format!(
                    "rollout-2026-07-20T10-00-0{index}-{id}.jsonl"
                )),
                format!(
                    "{}\n{}",
                    json!({"type":"session_meta","payload":{"id":id,"cwd":format!("/repo-{index}"),"model_provider":"openai"}}),
                    json!({"type":"event_msg","payload":{"type":"user_message","message":format!("Task {index}")}})
                ),
            )
            .unwrap();
    }
    let history = Arc::new(crate::history::HistoryService::new(
        claude_root,
        temp.path().join(".codex/sessions"),
        std::time::Duration::from_secs(60),
    ));
    let registry = Arc::new(CaptainsRegistry::new());
    for (index, (ship, terminal, id)) in [("ship-a", "cap-a", ids[0]), ("ship-b", "cap-b", ids[1])]
        .into_iter()
        .enumerate()
    {
        let project_id = format!("project-{index}");
        registry
            .upsert_project(ProjectRecord {
                root_path: None,
                vcs_capability: None,
                git_main_root: None,
                project_id: project_id.clone(),
                name: project_id.clone(),
                repo_root: format!("/repo-{index}"),
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 0,
                updated_at: 0,
            })
            .unwrap();
        registry
            .claim_provider(
                terminal,
                Some(ship),
                FleetRole::Captain,
                Some("codex"),
                Some(id),
                vec![],
                &|_| false,
                &|_| tmux::SessionLiveness::Gone,
            )
            .unwrap();
        registry
            .bind_ship_context(ship, &project_id, "History test", "codex")
            .unwrap();
    }
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let captain = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "ship-a",
        "cap-a",
    );
    let crew = mint_session(&identities, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_history_service(history)
        .with_captains_registry(Arc::clone(&registry))
        .with_identity_store(identities);

    let denied = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &crew, "history_list", json!({"limit": 10})),
    );
    assert!(!denied.ok);

    let scoped = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "history_list", json!({"limit": 10})),
    );
    assert!(scoped.ok, "Captain History list failed: {:?}", scoped.error);
    let entries = scoped.result.unwrap()["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["conversationId"], ids[0]);
    assert_eq!(entries[0]["captainId"], "ship-a");

    registry.release("ship-a").unwrap();
    let released = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "history_list", json!({"limit": 10})),
    );
    assert!(!released.ok, "released Captain retained History access");
}

#[test]
fn history_resume_command_is_selected_only_from_exact_backend_harness() {
    let codex_id = "22222222-2222-4222-8222-222222222222";
    let codex = crate::history::parse_codex_rollout(
        std::path::Path::new(
            "rollout-2026-07-20T10-00-00-22222222-2222-4222-8222-222222222222.jsonl",
        ),
        &json!({"type":"session_meta","payload":{"id":codex_id,"cwd":"/repo"}}).to_string(),
        1,
    )
    .unwrap()
    .entry;
    let claude = crate::history::parse_claude_transcript(
        std::path::Path::new("claude-exact.jsonl"),
        r#"{"type":"user","cwd":"/repo","message":{"content":"task"}}"#,
        1,
        false,
    )
    .unwrap()
    .entry;

    assert_eq!(
        history_resume_command(&codex),
        "codex resume '22222222-2222-4222-8222-222222222222'"
    );
    assert_eq!(
        history_resume_command(&claude),
        "claude --resume 'claude-exact'"
    );
}

#[test]
fn fresh_history_resume_scope_rejects_a_rebound_project_assignment() {
    let id = "22222222-2222-4222-8222-222222222222";
    let entry = crate::history::parse_codex_rollout(
        std::path::Path::new(
            "rollout-2026-07-20T10-00-00-22222222-2222-4222-8222-222222222222.jsonl",
        ),
        &json!({"type":"session_meta","payload":{"id":id,"cwd":"/repo-old"}}).to_string(),
        1,
    )
    .unwrap()
    .entry;
    let association = crate::history::HistoryAssociation {
        harness: crate::history::Harness::Codex,
        conversation_id: id.to_string(),
        terminal_id: Some("term0001".to_string()),
        liveness: crate::history::AssociationLiveness::Inactive,
        project_id: Some("project-old".to_string()),
        project_name: Some("Old Project".to_string()),
        captain_id: Some("ship".to_string()),
        assignment_id: Some("assignment-old".to_string()),
        role: Some("crew".to_string()),
        workspace_id: Some("workspace-old".to_string()),
        worktree_id: None,
        branch: None,
    };
    assert!(enforce_history_entry_owner(
        &WorkspaceMutationAuthority::Assignment(FleetWorkspaceOwner {
            project_id: "project-old".to_string(),
            assignment_id: "assignment-old".to_string(),
            ship_slug: "ship".to_string(),
        }),
        &entry,
        std::slice::from_ref(&association),
    )
    .is_ok());
    assert!(enforce_history_entry_owner(
        &WorkspaceMutationAuthority::Assignment(FleetWorkspaceOwner {
            project_id: "project-new".to_string(),
            assignment_id: "assignment-new".to_string(),
            ship_slug: "ship".to_string(),
        }),
        &entry,
        &[association],
    )
    .is_err());
}

#[test]
fn pending_history_runtime_proof_has_a_bounded_grace_period() {
    let pending = crate::history::HistoryPendingResume {
        request_id: "request-one".to_string(),
        history_id: "history:v1:one".to_string(),
        harness: crate::history::Harness::Codex,
        conversation_id: "22222222-2222-4222-8222-222222222222".to_string(),
        terminal_id: "term0001".to_string(),
        target_tab_id: Some("workspace".to_string()),
        authorized_ship_slug: None,
        authorized_project_id: None,
        authorized_assignment_id: None,
        reserved_at_ms: now_ms(),
    };
    assert!(!history_pending_runtime_proof_expired(&pending));
    let expired = crate::history::HistoryPendingResume {
        reserved_at_ms: now_ms().saturating_sub(HISTORY_PENDING_RUNTIME_PROOF_GRACE_MS),
        ..pending
    };
    assert!(history_pending_runtime_proof_expired(&expired));
}
