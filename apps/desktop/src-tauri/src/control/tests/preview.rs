use super::*;

#[test]
fn preview_commands_forward_registry_authorized_arguments_to_one_backend_adapter() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(StdMutex::new(Vec::<(String, Value)>::new()));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-control").with_preview_control(move |command, args, _root| {
        recorded
            .lock()
            .unwrap()
            .push((command.to_string(), args.clone()));
        Ok(json!({"command": command, "args": args}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let scoped = json!({
        "scope": {"projectId": "project-1"},
        "requestId": "request-1"
    });
    let rooted = json!({
        "rootPath": root_path,
        "scope": {"projectId": "project-1"},
        "requestId": "request-1"
    });
    for (command, args) in [
        ("preview_discover", json!({"rootPath": root_path})),
        ("preview_status", scoped.clone()),
        ("preview_select", rooted.clone()),
        ("preview_refresh", scoped.clone()),
        ("preview_open", scoped.clone()),
        ("preview_start", rooted.clone()),
        ("preview_stop", scoped.clone()),
        ("preview_restart", rooted.clone()),
    ] {
        let result = dispatch(&ctx, command, &args).unwrap();
        assert_eq!(result["command"], command);
        assert_eq!(result["args"], args);
    }
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 8);
}

#[test]
fn preview_control_rejects_unknown_projects_and_forged_roots_before_adapter() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-authority").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"unexpected": true}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();

    let unknown = dispatch(
        &ctx,
        "preview_status",
        &json!({"scope": {"projectId": "unknown"}}),
    )
    .unwrap_err();
    assert!(unknown.contains("unknown projectId"));
    let forged = dispatch(
        &ctx,
        "preview_start",
        &json!({
            "rootPath": "/tmp/not-the-registered-project",
            "scope": {"projectId": "project-1"},
            "requestId": "request-1"
        }),
    )
    .unwrap_err();
    assert!(forged.contains("does not match registered Project"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preview_control_allows_only_the_owning_project_captain() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-captain-authority").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"authorized": true}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-tile", Some("preview-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("preview-ship", "project-1", "Package 3", "codex")
        .unwrap();
    let captain = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| captain.ship_slug == "preview-ship")
        .unwrap();
    ctx.captains
        .create_workspace(
            "workspace-1",
            "Work",
            Some(&FleetWorkspaceOwner {
                project_id: "project-1".into(),
                assignment_id: captain.assignment_id,
                ship_slug: "preview-ship".into(),
            }),
        )
        .unwrap();
    let owning_captain = ResolvedIdentity {
        session_id: "captain-session".into(),
        mint_role: crate::identity::Role::Captain,
        tile: Some("captain-tile".into()),
        ship_slug: Some("preview-ship".into()),
        fleet_role: Some(FleetRole::Captain),
        claude_uuid: None,
    };
    let args = json!({"scope": {"projectId": "project-1"}});

    assert_eq!(
        preview_control(&ctx, "preview_status", &args, Some(&owning_captain), false).unwrap(),
        json!({"authorized": true})
    );
    let unrelated = ResolvedIdentity {
        ship_slug: Some("another-ship".into()),
        ..owning_captain.clone()
    };
    assert!(
        preview_control(&ctx, "preview_status", &args, Some(&unrelated), false)
            .unwrap_err()
            .contains("owning Project Captain")
    );
    assert!(preview_control(&ctx, "preview_status", &args, None, false)
        .unwrap_err()
        .contains("requires a Fleet identity"));
    let workspace_scope = json!({"projectId": "project-1", "workspaceId": "workspace-1"});
    for (command, args) in [
        ("preview_status", json!({"scope": workspace_scope})),
        (
            "preview_select",
            json!({"rootPath": root_path, "target": {"scope": workspace_scope}}),
        ),
        ("preview_refresh", json!({"scope": workspace_scope})),
        ("preview_open", json!({"scope": workspace_scope})),
        (
            "preview_start",
            json!({"rootPath": root_path, "scope": workspace_scope}),
        ),
        ("preview_stop", json!({"scope": workspace_scope})),
        (
            "preview_restart",
            json!({"rootPath": root_path, "scope": workspace_scope}),
        ),
    ] {
        preview_control(&ctx, command, &args, Some(&owning_captain), false).unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 8);

    {
        let mut registry = ctx.captains.lock();
        registry
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == "workspace-1")
            .unwrap()
            .owner
            .as_mut()
            .unwrap()
            .assignment_id = "different-assignment".into();
    }
    let error = preview_control(
        &ctx,
        "preview_status",
        &json!({"scope": workspace_scope}),
        Some(&owning_captain),
        false,
    )
    .unwrap_err();
    assert!(error.contains("another Captain Assignment"));
    assert_eq!(calls.load(Ordering::SeqCst), 8);
}

#[test]
fn preview_root_keeps_posix_identity_separate_from_host_open_path() {
    let project = ProjectRecord {
        project_id: "project-1".into(),
        name: "Preview Project".into(),
        repo_root: "/home/natkins/project".into(),
        root_path: Some("/home/natkins/project".into()),
        vcs_capability: Some("none".into()),
        git_main_root: None,
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 1,
        updated_at: 1,
    };
    let authority = preview_root_authority_with(&project, |identity| {
        assert_eq!(identity, "/home/natkins/project");
        PathBuf::from(r"\\wsl.localhost\Ubuntu-24.04\home\natkins\project")
    })
    .unwrap();
    assert_eq!(authority.posix_identity, "/home/natkins/project");
    assert_eq!(
        authority.host_open_path,
        PathBuf::from(r"\\wsl.localhost\Ubuntu-24.04\home\natkins\project")
    );
}

#[test]
fn preview_scoped_commands_refuse_unknown_and_foreign_durable_workspaces() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-workspace-authority").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"unexpected": true}))
    });
    for (index, root) in roots.iter().enumerate() {
        let project_id = format!("project-{}", index + 1);
        let ship_slug = format!("ship-{}", index + 1);
        let terminal_id = format!("captain-{}", index + 1);
        let root_path = root.path().to_string_lossy().into_owned();
        ctx.captains
            .upsert_project(ProjectRecord {
                project_id: project_id.clone(),
                name: project_id.clone(),
                repo_root: root_path.clone(),
                root_path: Some(root_path),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        ctx.captains
            .claim_test(&terminal_id, Some(&ship_slug), vec![])
            .unwrap();
        ctx.captains
            .bind_ship_context(&ship_slug, &project_id, "Package 3", "codex")
            .unwrap();
        let captain = ctx
            .captains
            .snapshot()
            .captains
            .into_iter()
            .find(|captain| captain.ship_slug == ship_slug)
            .unwrap();
        ctx.captains
            .create_workspace(
                &format!("workspace-{}", index + 1),
                "Work",
                Some(&FleetWorkspaceOwner {
                    project_id,
                    assignment_id: captain.assignment_id,
                    ship_slug,
                }),
            )
            .unwrap();
    }
    let root_path = roots[0].path().to_string_lossy().into_owned();
    let scoped = |workspace_id: &str| {
        json!({
            "scope": {
                "projectId": "project-1",
                "workspaceId": workspace_id
            },
            "requestId": "request-1"
        })
    };
    let rooted = |workspace_id: &str| {
        json!({
            "rootPath": root_path,
            "scope": {
                "projectId": "project-1",
                "workspaceId": workspace_id
            },
            "requestId": "request-1"
        })
    };
    for workspace_id in ["missing-workspace", "workspace-2"] {
        for (command, args) in [
            ("preview_status", scoped(workspace_id)),
            (
                "preview_select",
                json!({
                    "rootPath": root_path,
                    "target": {
                        "scope": {
                            "projectId": "project-1",
                            "workspaceId": workspace_id
                        }
                    }
                }),
            ),
            ("preview_refresh", scoped(workspace_id)),
            ("preview_open", scoped(workspace_id)),
            ("preview_start", rooted(workspace_id)),
            ("preview_stop", scoped(workspace_id)),
            ("preview_restart", rooted(workspace_id)),
        ] {
            let error = dispatch(&ctx, command, &args).unwrap_err();
            assert!(
                error.contains("unknown durable workspaceId")
                    || error.contains("belongs to another Project"),
                "{command}: {error}"
            );
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preview_control_refuses_mismatched_top_level_and_target_scopes() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-scope-match").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"unexpected": true}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();

    for target_scope in [
        json!({"projectId": "another-project"}),
        json!({"projectId": "project-1", "workspaceId": "another-workspace"}),
    ] {
        let error = dispatch(
            &ctx,
            "preview_select",
            &json!({
                "rootPath": root_path,
                "scope": {"projectId": "project-1"},
                "target": {"scope": target_scope}
            }),
        )
        .unwrap_err();
        assert!(error.contains("scopes must match exactly"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
