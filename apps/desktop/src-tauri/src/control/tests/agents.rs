use super::*;

#[test]
fn list_agents_returns_a_bounded_assignment_free_snapshot() {
    let ctx = test_ctx("secret");
    let v = dispatch(
        &ctx,
        "list_agents",
        &json!({"projectId": "project-1", "limit": 1}),
    )
    .unwrap();
    assert_eq!(v["agents"], json!([]));
    assert_eq!(v["count"], 0);
    assert_eq!(v["total"], 0);
    assert_eq!(v["hasMore"], false);
    assert_eq!(v["eventCursor"], 0);
    assert!(v["digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));
}

#[test]
fn get_agent_requires_an_existing_durable_agent_record() {
    let ctx = test_ctx("secret");
    let error = dispatch(
        &ctx,
        "get_agent",
        &json!({"agentSessionId": "missing-agent"}),
    )
    .unwrap_err();
    assert!(error.contains("agent 'missing-agent' was not found"));
}

#[test]
fn checkpoint_and_event_reads_fail_closed_for_unknown_agents() {
    let ctx = test_ctx("secret");
    let checkpoint_error = dispatch(
        &ctx,
        "agent_checkpoint",
        &json!({
            "agentSessionId": "missing-agent",
            "authorSessionId": "captain-1",
            "summary": "progress"
        }),
    )
    .unwrap_err();
    assert!(checkpoint_error.contains("agent 'missing-agent' was not found"));

    let events_error = dispatch(
        &ctx,
        "agent_events",
        &json!({"agentSessionId": "missing-agent", "cursor": "0"}),
    )
    .unwrap_err();
    assert!(events_error.contains("agent 'missing-agent' was not found"));
}

#[test]
fn durable_agent_checkpoint_persists_and_advances_the_event_cursor() {
    let ctx = test_ctx("secret");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-1".into(),
            name: "Project".into(),
            repo_root: "/tmp/project-1".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-1", Some("captain"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("captain", "project-1", "Assignment", "codex")
        .unwrap();
    let (lane_claim, dispatch_capacity) = test_dispatch_evidence("lane-checkpoint", "agent-1");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-1".into(),
            captain_session_id: "captain-1".into(),
            project_id: "project-1".into(),
            assignment: "Do the work".into(),
            directory: "/tmp/project-1".into(),
            worktree_path: None,
            branch: None,
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: crate::agent_session::RuntimeState::Starting,
            work_stage: crate::agent_session::WorkStage::Assigned,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                "1111111111111111111111111111111111111111",
                false,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let response = dispatch(
        &ctx,
        "agent_checkpoint",
        &json!({
            "agentSessionId": "agent-1",
            "authorSessionId": "captain-1",
            "summary": "finished the first slice"
        }),
    )
    .unwrap();
    assert_eq!(
        response["checkpoint"]["summary"],
        "finished the first slice"
    );
    assert!(response["eventCursor"]
        .as_u64()
        .is_some_and(|cursor| cursor > 0));
    let listed = dispatch(
        &ctx,
        "list_agents",
        &json!({"projectId": "project-1", "limit": 10}),
    )
    .unwrap();
    assert!(listed["eventCursor"].as_u64() >= response["eventCursor"].as_u64());
    let events = dispatch(
        &ctx,
        "agent_events",
        &json!({"agentSessionId": "agent-1", "cursor": "0", "limit": 10}),
    )
    .unwrap();
    assert!(events["count"].as_u64().is_some_and(|count| count >= 1));
    assert!(events["events"]
        .as_array()
        .is_some_and(|events| events.iter().any(|event| event["kind"] == "checkpoint")));
}

#[test]
fn authenticated_agent_followup_is_owned_durable_idempotent_and_scope_explicit() {
    let ctx = test_ctx("agent-followup");
    seed_starting_agent(&ctx, "followup-agent");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "foreign-project".into(),
            name: "Foreign Project".into(),
            repo_root: "/tmp/foreign-project".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("foreign-captain", Some("foreign-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "foreign-ship",
            "foreign-project",
            "Foreign Assignment",
            "codex",
        )
        .unwrap();
    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("capacity-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "capacity-captain")
        .unwrap();
    let foreign_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("foreign-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, "foreign-captain")
        .unwrap();
    let call = |request_id: &str, message: &str, replacement: Option<&str>| {
        let mut args = json!({
            "requestId": request_id,
            "captainSessionId": "capacity-captain",
            "shipSlug": "capacity-ship",
            "projectId": "capacity-project",
            "agentSessionId": "followup-agent",
            "message": message,
        });
        if let Some(replacement) = replacement {
            args["replacementAssignment"] = json!(replacement);
        }
        dispatch_authenticated(
            &ctx,
            req_session(
                "agent-followup",
                &captain_identity.secret,
                "agent_followup",
                args,
            ),
        )
    };

    let first = call("followup-1", "Continue the bounded repair.", None);
    assert!(first.ok, "got: {:?}", first.error);
    assert_eq!(
        first.result.as_ref().unwrap()["agentSessionId"],
        "followup-agent"
    );
    assert_eq!(first.result.as_ref().unwrap()["messageSeq"], 0);
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 1);
    assert_eq!(
        ctx.captains.snapshot().agent_sessions[0].assignment,
        "Pending durable start"
    );

    let replay = call("followup-1", "Continue the bounded repair.", None);
    assert!(replay.ok, "got: {:?}", replay.error);
    assert_eq!(replay.result.as_ref().unwrap()["idempotentReplay"], true);
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 1);
    let foreign_replay = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup",
            &foreign_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-1",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Continue the bounded repair."
            }),
        ),
    );
    assert!(!foreign_replay.ok, "foreign Captain replayed owner success");
    assert_eq!(foreign_replay.error_kind.as_deref(), Some("unauthorized"));

    let foreign_squat = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup",
            &foreign_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-squat",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Owner must still be able to send this."
            }),
        ),
    );
    assert!(!foreign_squat.ok);
    let owner_after_squat = call(
        "followup-squat",
        "Owner must still be able to send this.",
        None,
    );
    assert!(
        owner_after_squat.ok,
        "foreign Captain poisoned owner requestId: {:?}",
        owner_after_squat.error
    );
    let conflict = call("followup-1", "Changed retry payload.", None);
    assert!(!conflict.ok);
    assert_eq!(conflict.error_kind.as_deref(), Some("request_conflict"));
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 2);

    let replacement = call(
        "followup-2",
        "The reviewed scope is now explicit.",
        Some("Replacement bounded assignment"),
    );
    assert!(replacement.ok, "got: {:?}", replacement.error);
    assert_eq!(
        replacement.result.as_ref().unwrap()["assignmentChanged"],
        true
    );
    assert_eq!(
        ctx.captains.snapshot().agent_sessions[0].assignment,
        "Replacement bounded assignment"
    );
}

#[test]
fn agent_followup_rejects_foreign_and_exited_agents_with_structured_errors() {
    let ctx = test_ctx("agent-followup-errors");
    seed_starting_agent(&ctx, "followup-agent");
    let foreign_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("foreign-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, "foreign-captain")
        .unwrap();
    let args = json!({
        "requestId": "followup-foreign",
        "captainSessionId": "capacity-captain",
        "shipSlug": "capacity-ship",
        "projectId": "capacity-project",
        "agentSessionId": "followup-agent",
        "message": "Do not deliver this.",
    });
    let foreign = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-errors",
            &foreign_identity.secret,
            "agent_followup",
            args.clone(),
        ),
    );
    assert!(!foreign.ok);
    assert_eq!(foreign.error_kind.as_deref(), Some("unauthorized"));
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 0);

    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("capacity-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "capacity-captain")
        .unwrap();
    ctx.captains
        .reconcile_agent_runtime("followup-agent", RuntimeState::Exited, None)
        .unwrap();
    let mut exited_args = args;
    exited_args["requestId"] = json!("followup-exited");
    let exited = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-errors",
            &captain_identity.secret,
            "agent_followup",
            exited_args,
        ),
    );
    assert!(!exited.ok);
    assert_eq!(exited.error_kind.as_deref(), Some("agent_exited"));
    assert_eq!(
        exited.error_details.as_ref().unwrap()["operation"],
        "agent_followup"
    );
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 0);
}

#[test]
fn agent_followup_assignment_persist_failure_never_makes_new_scope_deliverable() {
    let path = captains_tmp("agent-followup-assignment-failure");
    let registry = Arc::new(CaptainsRegistry::load(path.clone()));
    let ctx =
        test_ctx("agent-followup-assignment-failure").with_captains_registry(Arc::clone(&registry));
    seed_starting_agent(&ctx, "followup-agent");
    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("capacity-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "capacity-captain")
        .unwrap();
    registry.fail_next_persist("injected Assignment persistence failure");

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-assignment-failure",
            &captain_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-failed-assignment",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Act on the replacement Assignment only.",
                "replacementAssignment": "Replacement Assignment"
            }),
        ),
    );
    assert!(!response.ok);
    assert_eq!(response.error_kind.as_deref(), Some("persistence_failed"));
    assert_eq!(
        registry.snapshot().agent_sessions[0].assignment,
        "Pending durable start"
    );
    assert_eq!(
        ctx.inbox.drain_one("followup-agent", |_| Ok(())),
        crate::inbox::DrainOutcome::Empty,
        "failed Assignment persistence exposed a deliverable wrong-scope instruction"
    );

    let retry = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-assignment-failure",
            &captain_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-failed-assignment",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Act on the replacement Assignment only.",
                "replacementAssignment": "Replacement Assignment"
            }),
        ),
    );
    assert!(retry.ok, "retry did not converge: {:?}", retry.error);
    assert_eq!(
        registry.snapshot().agent_sessions[0].assignment,
        "Replacement Assignment"
    );
    assert_eq!(
        ctx.inbox.drain_one("followup-agent", |_| Ok(())),
        crate::inbox::DrainOutcome::Delivered { seq: 0 }
    );

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn agent_delivery_command_keeps_completion_and_release_states_distinct() {
    let ctx = test_ctx("agent-delivery");
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let repo_path = repo_root.to_string_lossy().to_string();
    let run = |args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(&repo_path, args).unwrap();
        assert!(ok, "git {args:?} failed: {stderr}");
        stdout
    };
    run(&["branch", "-M", "main"]);
    let baseline = exact_head(&repo_root);
    let commit_file = |name: &str, content: &str| {
        std::fs::write(repo_root.join(name), content).unwrap();
        run(&["add", name]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            name,
        ]);
        exact_head(&repo_root)
    };
    let interface_result = commit_file("interface.txt", "shared interface\n");
    let result = commit_file("implementation.txt", "lane result\n");
    let incomplete_result = commit_file("incomplete.txt", "incomplete lane\n");
    let canonical = commit_file("integration.txt", "canonical integration\n");
    let worktree_path = worktree.to_string_lossy().to_string();
    let run_worktree = |args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(&worktree_path, args).unwrap();
        assert!(ok, "git {args:?} failed: {stderr}");
        stdout
    };
    std::fs::write(worktree.join("divergent.txt"), "divergent lane\n").unwrap();
    run_worktree(&["add", "divergent.txt"]);
    run_worktree(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "divergent lane",
    ]);
    let divergent_result = exact_head(&worktree);
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-delivery".into(),
            name: "Delivery".into(),
            repo_root: repo_path,
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-delivery", Some("delivery-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "delivery-ship",
            "project-delivery",
            "Review delivery",
            "codex",
        )
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-delivery")
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-interface")
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-incomplete")
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-divergent")
        .unwrap();
    let mut interface_delivery = crate::agent_session::DeliveryProvenance::new(&baseline, false);
    interface_delivery
        .record_implementation(&interface_result)
        .unwrap();
    interface_delivery
        .record_review(crate::agent_session::ReviewEvidence {
            commit: interface_result.clone(),
            reviewer_identity: "reviewer-interface".into(),
            reference: "review://interface".into(),
            recorded_at: 2,
        })
        .unwrap();
    interface_delivery
        .record_acceptance_test(crate::agent_session::AcceptanceTestEvidence {
            commit: interface_result.clone(),
            runner_identity: "tester-interface".into(),
            reference: "test://interface".into(),
            environment: crate::agent_session::AcceptanceEnvironment::Source,
            recorded_at: 2,
        })
        .unwrap();
    let (interface_lane_claim, interface_dispatch_capacity) =
        test_dispatch_evidence("shared-interface", "agent-interface");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-interface".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Define the shared interface".into(),
            directory: "/tmp/project-delivery-interface".into(),
            worktree_path: None,
            branch: Some("shared-interface".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Exited,
            work_stage: crate::agent_session::WorkStage::Complete,
            delivery: Some(interface_delivery),
            lane_claim: Some(interface_lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(interface_dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let (divergent_lane_claim, divergent_dispatch_capacity) =
        test_dispatch_evidence("divergent-lane", "agent-divergent");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-divergent".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Build a divergent lane".into(),
            directory: worktree_path.clone(),
            worktree_path: Some(files::posix_form(&worktree_path)),
            branch: Some("wt".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Exited,
            work_stage: crate::agent_session::WorkStage::Complete,
            delivery: Some(completed_delivery(&baseline, &divergent_result)),
            lane_claim: Some(divergent_lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(divergent_dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let mut incomplete_delivery = crate::agent_session::DeliveryProvenance::new(&baseline, false);
    incomplete_delivery
        .record_implementation(&incomplete_result)
        .unwrap();
    let (incomplete_lane_claim, incomplete_dispatch_capacity) =
        test_dispatch_evidence("incomplete-lane", "agent-incomplete");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-incomplete".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Incomplete lane".into(),
            directory: "/tmp/project-delivery-incomplete".into(),
            worktree_path: None,
            branch: Some("incomplete-lane".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Idle,
            work_stage: crate::agent_session::WorkStage::Working,
            delivery: Some(incomplete_delivery),
            lane_claim: Some(incomplete_lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(incomplete_dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("delivery-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-delivery")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let integration_owner_identity = captain.session_id.clone();
    let (lane_claim, dispatch_capacity) = test_dispatch_evidence("lane-delivery", "agent-delivery");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-delivery".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Implement one scope".into(),
            directory: "/tmp/project-delivery".into(),
            worktree_path: None,
            branch: Some("agent-delivery".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Running,
            work_stage: crate::agent_session::WorkStage::Working,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                &baseline, true,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: vec![
                crate::governor::IntegrationContract {
                    contract_id: "delivery-integration".into(),
                    integration_owner: integration_owner_identity.clone(),
                    ordered_lane_ids: vec!["shared-interface".into(), "lane-delivery".into()],
                },
                crate::governor::IntegrationContract {
                    contract_id: "incomplete-integration-test".into(),
                    integration_owner: integration_owner_identity.clone(),
                    ordered_lane_ids: vec!["incomplete-lane".into(), "lane-delivery".into()],
                },
                crate::governor::IntegrationContract {
                    contract_id: "divergent-integration-test".into(),
                    integration_owner: integration_owner_identity.clone(),
                    ordered_lane_ids: vec!["divergent-lane".into(), "lane-delivery".into()],
                },
            ],
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let agent_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Crew, Some("delivery-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&agent_identity.id, "agent-delivery")
        .unwrap();
    let agent = resolve_identity(&ctx, &agent_identity.secret).unwrap();

    let self_discard = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-delivery",
            &agent_identity.secret,
            "agent_checkpoint",
            json!({
                "agentSessionId": "agent-delivery",
                "authorSessionId": agent.session_id,
                "summary": "attempt self discard",
                "stage": "stopped"
            }),
        ),
    );
    assert!(!self_discard.ok);
    assert!(
        self_discard
            .error
            .as_deref()
            .is_some_and(|error| error.contains("stage is not permitted")),
        "got: {:?}",
        self_discard.error
    );
    assert!(
        active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "incomplete-lane")
    );
    let discard = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-delivery",
            &captain_identity.secret,
            "agent_checkpoint",
            json!({
                "agentSessionId": "agent-incomplete",
                "authorSessionId": captain.session_id,
                "summary": "discard abandoned lane",
                "stage": "stopped"
            }),
        ),
    );
    assert!(discard.ok, "got: {:?}", discard.error);
    assert!(
        !active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "incomplete-lane")
    );
    let resume_discarded = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-delivery",
            &captain_identity.secret,
            "agent_checkpoint",
            json!({
                "agentSessionId": "agent-incomplete",
                "authorSessionId": captain.session_id,
                "summary": "attempt to resume discarded lane",
                "stage": "working"
            }),
        ),
    );
    assert!(!resume_discarded.ok);
    assert!(resume_discarded
        .error
        .as_deref()
        .is_some_and(|error| error.contains("terminal work stage")));
    let update_discarded = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-incomplete",
            "state": "reviewed",
            "evidence": {
                "commit": incomplete_result,
                "reference": "review://discarded"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(update_discarded.contains("stopped lane is discarded"));

    let implemented = dispatch_authenticated(
        &ctx,
        req_session(
            "read-agent-delivery",
            &agent_identity.secret,
            "record_agent_delivery",
            json!({
            "agentSessionId": "agent-delivery",
            "state": "implemented",
            "evidence": { "commit": result }
            }),
        ),
    );
    assert!(implemented.ok, "got: {:?}", implemented.error);
    let self_review = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "reviewed",
            "evidence": { "commit": result, "reference": "review://self" }
        }),
        Some(&agent),
        false,
    )
    .unwrap_err();
    assert!(self_review.contains("implementing agent"));
    dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "reviewed",
            "evidence": { "commit": result, "reference": "review://captain" }
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let complete = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "tested",
            "evidence": {
                "commit": result,
                "reference": "test://acceptance",
                "environment": {
                    "kind": "packagedGuiE2e",
                    "artifactId": "candidate-installer-1",
                    "sourceCommit": result,
                    "installationTarget": "C:\\T-Hub-Candidate"
                }
            }
        }),
        Some(&agent),
        false,
    )
    .unwrap();
    assert_eq!(complete["deliveryStates"]["complete"], true);
    assert_eq!(complete["deliveryStates"]["integrated"], false);
    assert_eq!(complete["deliveryStates"]["installed"], false);
    assert_eq!(complete["agent"]["workStage"], "complete");
    assert!(
        active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "lane-delivery")
    );
    let mut explicitly_stopped = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == "agent-delivery")
        .unwrap();
    explicitly_stopped.work_stage = crate::agent_session::WorkStage::Stopped;
    assert!(!agent_retains_lane_ownership(&explicitly_stopped));

    let missing_manifest = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": {
                "sourceCommit": result,
                "canonicalBaseline": "main",
                "canonicalCommit": canonical,
                "reference": "git://integration"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(missing_manifest.contains("manifest"));

    let integration_evidence = |owner: &str| {
        json!({
            "sourceCommit": result,
            "canonicalBaseline": "main",
            "canonicalCommit": canonical,
            "reference": "git://integration",
            "manifest": {
                "integrationOwnerIdentity": owner,
                "inputs": [
                    {
                        "laneId": "shared-interface",
                        "agentSessionId": "agent-interface",
                        "sourceBaseline": baseline,
                        "resultingCommit": interface_result
                    },
                    {
                        "laneId": "lane-delivery",
                        "agentSessionId": "agent-delivery",
                        "sourceBaseline": baseline,
                        "resultingCommit": result
                    }
                ]
            }
        })
    };
    let manifest = serde_json::from_value::<crate::agent_session::IntegrationManifest>(
        integration_evidence(&integration_owner_identity)["manifest"].clone(),
    )
    .unwrap();
    let mut ambiguous_target = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == "agent-delivery")
        .unwrap();
    let mut duplicate_contract = ambiguous_target.integration_contracts[0].clone();
    duplicate_contract.contract_id = "duplicate-delivery-integration".into();
    ambiguous_target
        .integration_contracts
        .push(duplicate_contract);
    assert!(enforce_recorded_integration_contract(
        &ambiguous_target,
        &manifest,
        &integration_owner_identity,
    )
    .unwrap_err()
    .contains("matches multiple durable integration contracts"));
    let forged_owner = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": integration_evidence("forged-owner")
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(forged_owner.contains("authenticated actor identity"));

    let general_identity = ctx.identity.mint(crate::identity::Role::General).unwrap();
    let general = resolve_identity(&ctx, &general_identity.secret).unwrap();
    let wrong_designated_owner = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": integration_evidence(&general.session_id)
        }),
        Some(&general),
        false,
    )
    .unwrap_err();
    assert!(
        wrong_designated_owner.contains("designates integration owner"),
        "got: {wrong_designated_owner}"
    );

    let mut omitted_lane = integration_evidence(&integration_owner_identity);
    omitted_lane["manifest"]["inputs"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let omitted_lane = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": omitted_lane
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(omitted_lane.contains("exactly match one durable integration contract"));

    let mut reordered_lanes = integration_evidence(&integration_owner_identity);
    reordered_lanes["manifest"]["inputs"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    let reordered_lanes = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": reordered_lanes
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(reordered_lanes.contains("exactly match one durable integration contract"));
    assert!(
        !ctx.captains
            .snapshot()
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == "agent-delivery")
            .unwrap()
            .delivery_states()
            .unwrap()
            .integrated
    );

    let mut invented_agent = integration_evidence(&integration_owner_identity);
    invented_agent["manifest"]["inputs"][0]["agentSessionId"] = json!("invented-agent");
    let invented_agent = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": invented_agent
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(invented_agent.contains("is not registered"));

    let mut wrong_lane = integration_evidence(&integration_owner_identity);
    wrong_lane["manifest"]["inputs"][0]["laneId"] = json!("invented-lane");
    let wrong_lane = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": wrong_lane
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(wrong_lane.contains("exactly match one durable integration contract"));

    let mut wrong_commits = integration_evidence(&integration_owner_identity);
    wrong_commits["manifest"]["inputs"][0]["sourceBaseline"] =
        json!("9999999999999999999999999999999999999999");
    let wrong_commits = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": wrong_commits
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(wrong_commits.contains("commits do not match"));

    let mut incomplete_input = integration_evidence(&integration_owner_identity);
    incomplete_input["manifest"]["inputs"][0]["laneId"] = json!("incomplete-lane");
    incomplete_input["manifest"]["inputs"][0]["agentSessionId"] = json!("agent-incomplete");
    incomplete_input["manifest"]["inputs"][0]["resultingCommit"] = json!(incomplete_result);
    let incomplete_input = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": incomplete_input
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(incomplete_input.contains("is not complete"));

    let mut wrong_canonical_tip = integration_evidence(&integration_owner_identity);
    wrong_canonical_tip["canonicalCommit"] = json!(result);
    let wrong_canonical_tip = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": wrong_canonical_tip
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(
        wrong_canonical_tip.contains("canonical baseline rejected"),
        "got: {wrong_canonical_tip}"
    );
    assert!(
        !ctx.captains
            .snapshot()
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == "agent-delivery")
            .unwrap()
            .delivery_states()
            .unwrap()
            .integrated
    );

    let mut divergent_input = integration_evidence(&integration_owner_identity);
    divergent_input["manifest"]["inputs"][0]["laneId"] = json!("divergent-lane");
    divergent_input["manifest"]["inputs"][0]["agentSessionId"] = json!("agent-divergent");
    divergent_input["manifest"]["inputs"][0]["resultingCommit"] = json!(divergent_result);
    let divergent_input = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": divergent_input
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(
        divergent_input.contains("not incorporated"),
        "got: {divergent_input}"
    );
    assert!(
        !ctx.captains
            .snapshot()
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == "agent-delivery")
            .unwrap()
            .delivery_states()
            .unwrap()
            .integrated
    );

    let integrated = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": integration_evidence(&integration_owner_identity)
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(integrated["deliveryStates"]["complete"], true);
    assert_eq!(integrated["deliveryStates"]["integrated"], true);
    assert_eq!(integrated["deliveryStates"]["packaged"], false);
    assert!(
        !active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "lane-delivery")
    );
    assert_eq!(
        integrated["agent"]["delivery"]["integration"]["manifest"]["inputs"][0]["laneId"],
        "shared-interface"
    );
    let integration_recorded_at = integrated["agent"]["delivery"]["integration"]["recordedAt"]
        .as_u64()
        .unwrap();

    let missing_artifact_manifest = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "packaged",
            "evidence": {
                "artifactId": "installer-1",
                "sourceBaseline": canonical,
                "reference": "artifact://windows/installer"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(missing_artifact_manifest.contains("manifest"));

    let packaged = dispatch_with_caller(
            &ctx,
            "record_agent_delivery",
            &json!({
                "agentSessionId": "agent-delivery",
                "state": "packaged",
                "evidence": {
                    "artifactId": "installer-1",
                    "sourceBaseline": canonical,
                    "reference": "artifact://windows/installer",
                    "manifest": {
                        "branch": "main",
                        "sourceCommit": canonical,
                        "gitTree": "5555555555555555555555555555555555555555",
                        "version": "0.3.107",
                        "installerSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "builtAt": integration_recorded_at,
                        "signatureStatus": "verified"
                    }
                }
            }),
            Some(&captain),
            false,
        )
        .unwrap();
    assert_eq!(packaged["deliveryStates"]["integrated"], true);
    assert_eq!(packaged["deliveryStates"]["packaged"], true);
    assert_eq!(packaged["deliveryStates"]["installed"], false);

    let installed = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "installed",
            "evidence": {
                "artifactId": "installer-1",
                "target": "C:\\Program Files\\T-Hub",
                "reference": "install://windows/release"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(installed["deliveryStates"]["installed"], true);
    assert_eq!(
        installed["agent"]["delivery"]["acceptanceTest"]["environment"]["artifact_id"],
        "candidate-installer-1"
    );
    assert_eq!(
        installed["agent"]["delivery"]["artifact"]["artifactId"],
        "installer-1"
    );

    let live_verified = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "liveVerified",
            "evidence": {
                "artifactId": "installer-1",
                "target": "C:\\Program Files\\T-Hub",
                "verifierKind": "aiAgent",
                "reference": "verification://windows/release"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(live_verified["deliveryStates"]["liveVerified"], true);

    let status = get_agent(
        &ctx,
        &json!({ "agentSessionId": "agent-delivery" }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(status["deliveryStates"]["complete"], true);
    assert_eq!(status["deliveryStates"]["integrated"], true);
    assert_eq!(status["deliveryStates"]["packaged"], true);
    assert_eq!(status["deliveryStates"]["liveVerified"], true);
    let events = agent_events(
        &ctx,
        &json!({ "agentSessionId": "agent-delivery", "cursor": "0" }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert!(events["events"].as_array().is_some_and(|events| events
        .iter()
        .any(|event| event["kind"] == "delivery_evidence"
            && event["deliveryStates"]["complete"] == true)));
    std::fs::remove_dir_all(base).ok();
}
