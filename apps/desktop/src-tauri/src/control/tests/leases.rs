use super::*;

#[test]
fn durable_captain_renews_an_identity_bound_control_lease() {
    let (ctx, captains, identities, identity) = captain_lease_fixture(true);
    let before = captains.snapshot();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let result = renewed.result.unwrap();
    let lease = result["lease"].as_str().unwrap();
    let first_expires_at = result["expiresAt"].as_u64().unwrap();
    assert_ne!(lease, ctx.token);
    assert_ne!(lease, ctx.read_token);
    assert_eq!(result["terminalId"], "lease-captain");
    assert_eq!(result["scope"]["kind"], "captain");
    assert_eq!(result["scope"]["shipSlug"], "lease-ship");
    assert_eq!(result["scope"]["projectId"], "lease-project");
    assert_eq!(captains.snapshot().captains, before.captains);
    assert_eq!(captains.snapshot().projects, before.projects);

    let repeated = dispatch_authenticated(
        &ctx,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(repeated.ok, "{:?}", repeated.error);
    let repeated = repeated.result.unwrap();
    assert_eq!(repeated["lease"], lease);
    assert!(repeated["expiresAt"].as_u64().unwrap() > first_expires_at);
    let lease_state = ctx.control_leases.state.lock().unwrap();
    assert_eq!(lease_state.by_secret.len(), 1);
    assert_eq!(lease_state.by_identity.len(), 1);
    drop(lease_state);

    let capability = dispatch_authenticated(
        &ctx,
        req_session(lease, &identity.secret, "my_capability", json!({})),
    );
    assert_eq!(capability.result.unwrap()["capability"], "control");

    let foreign = identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("foreign-ship".into()),
            "foreign-captain",
        )
        .unwrap();
    let stolen = dispatch_authenticated(
        &ctx,
        req_session(lease, &foreign.secret, "my_capability", json!({})),
    );
    assert_eq!(
        stolen.error.as_deref(),
        Some("unauthorized: bad control token")
    );

    identities.revoke(&identity.id).unwrap();
    let revoked = dispatch_authenticated(
        &ctx,
        req_session(lease, &identity.secret, "my_capability", json!({})),
    );
    assert_eq!(
        revoked.error.as_deref(),
        Some("unauthorized: bad control token")
    );
}

#[test]
fn renewing_same_identity_lease_atomically_extends_both_deadlines() {
    let (ctx, _, identities, identity) = captain_lease_fixture(true);
    let authority = LeaseAuthority::Captain {
        ship_slug: "lease-ship".into(),
        project_id: "lease-project".into(),
        generation: ctx.captains.test_scoped_authority_generation(
            "lease-ship",
            "lease-captain",
            "lease-project",
        ),
    };
    let old_deadline = Instant::now() + Duration::from_millis(80);
    let old_epoch_deadline = now_ms().saturating_add(80);
    let (old_secret, old_expires_at) = ctx.control_leases.issue(CaptainControlLease {
        identity_id: identity.id.clone(),
        terminal_id: "lease-captain".into(),
        authority: authority.clone(),
        expires_at: old_deadline,
        expires_at_epoch_ms: old_epoch_deadline,
    });

    thread::sleep(Duration::from_millis(10));
    let renewed_deadline = Instant::now() + Duration::from_millis(250);
    let renewed_epoch_deadline = now_ms().saturating_add(250);
    let (renewed_secret, renewed_expires_at) = ctx.control_leases.issue(CaptainControlLease {
        identity_id: identity.id.clone(),
        terminal_id: "lease-captain".into(),
        authority: authority.clone(),
        expires_at: renewed_deadline,
        expires_at_epoch_ms: renewed_epoch_deadline,
    });

    assert_eq!(renewed_secret, old_secret);
    assert!(renewed_expires_at > old_expires_at);
    let renewed = ctx.control_leases.get(&renewed_secret).unwrap();
    assert!(renewed.expires_at > old_deadline);
    assert_eq!(renewed.authority, authority);
    let state = ctx.control_leases.state.lock().unwrap();
    assert_eq!(state.by_secret.len(), 1);
    assert_eq!(state.by_identity.len(), 1);
    assert_eq!(state.by_identity.get(&identity.id), Some(&renewed_secret));
    drop(state);

    thread::sleep(
        old_deadline
            .saturating_duration_since(Instant::now())
            .saturating_add(Duration::from_millis(20)),
    );
    let after_old_deadline = dispatch_authenticated(
        &ctx,
        req_session(
            &renewed_secret,
            &identity.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(after_old_deadline.result.unwrap()["capability"], "control");

    let foreign = identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("foreign-ship".into()),
            "foreign-captain",
        )
        .unwrap();
    let stolen = dispatch_authenticated(
        &ctx,
        req_session(
            &renewed_secret,
            &foreign.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(
        stolen.error.as_deref(),
        Some("unauthorized: bad control token")
    );

    identities.revoke(&identity.id).unwrap();
    let revoked = dispatch_authenticated(
        &ctx,
        req_session(
            &renewed_secret,
            &identity.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(
        revoked.error.as_deref(),
        Some("unauthorized: bad control token")
    );
}

#[test]
fn captain_control_lease_capacity_evicts_oldest_identity_binding() {
    let leases = CaptainControlLeases::default();
    let base = Instant::now() + Duration::from_secs(3_600);
    let mut oldest_secret = String::new();

    for index in 0..MAX_CAPTAIN_CONTROL_LEASES {
        let (secret, _) = leases.issue(CaptainControlLease {
            identity_id: format!("identity-{index}"),
            terminal_id: format!("terminal-{index}"),
            authority: LeaseAuthority::Cortana {
                generation: index as u64,
            },
            expires_at: base + Duration::from_millis(index as u64),
            expires_at_epoch_ms: 10_000 + index as u64,
        });
        if index == 0 {
            oldest_secret = secret;
        }
    }

    let (newest_secret, _) = leases.issue(CaptainControlLease {
        identity_id: "identity-newest".into(),
        terminal_id: "terminal-newest".into(),
        authority: LeaseAuthority::Cortana {
            generation: MAX_CAPTAIN_CONTROL_LEASES as u64,
        },
        expires_at: base + Duration::from_secs(1),
        expires_at_epoch_ms: 20_000,
    });

    assert!(leases.get(&oldest_secret).is_none());
    assert!(leases.get(&newest_secret).is_some());
    let state = leases.state.lock().unwrap();
    assert_eq!(state.by_secret.len(), MAX_CAPTAIN_CONTROL_LEASES);
    assert_eq!(state.by_identity.len(), MAX_CAPTAIN_CONTROL_LEASES);
    assert!(!state.by_identity.contains_key("identity-0"));
    assert_eq!(
        state.by_identity.get("identity-newest"),
        Some(&newest_secret)
    );
}

#[test]
fn authoritative_cortana_renews_a_fleet_scoped_lease_and_mutates() {
    let terminal_id = "lease-cortana";
    let live_target = tmux_target(terminal_id);
    let ctx = test_ctx("cortana-global").with_live_sessions(move || Ok(vec![live_target.clone()]));
    let secret = mint_current_cortana_session(&ctx.identity, &ctx.captains, terminal_id);
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let renewed = renewed.result.unwrap();
    assert_eq!(renewed["scope"]["kind"], "cortana");
    let lease = renewed["lease"].as_str().unwrap();
    let mutation = dispatch_authenticated(
        &ctx,
        req_session(lease, &secret, "new_tab", json!({"name": "Cortana Ops"})),
    );
    assert!(mutation.ok, "{:?}", mutation.error);
    assert!(ctx.tabs.id_for_name("Cortana Ops").is_some());
}

#[test]
fn captain_lease_renewal_rejects_dead_released_crew_and_duplicate_identities() {
    let (dead_ctx, _, _, dead_identity) = captain_lease_fixture(false);
    let dead = dispatch_authenticated(
        &dead_ctx,
        req_session(
            "read-global-control",
            &dead_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(dead
        .error
        .as_deref()
        .is_some_and(|error| error.contains("not alive")));

    let (released_ctx, captains, _, released_identity) = captain_lease_fixture(true);
    captains.release("lease-ship").unwrap();
    let released = dispatch_authenticated(
        &released_ctx,
        req_session(
            "read-global-control",
            &released_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(released
        .error
        .as_deref()
        .is_some_and(|error| error.contains("control_reauthentication_required")));

    let (duplicate_ctx, _, identities, identity) = captain_lease_fixture(true);
    identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("lease-ship".into()),
            "lease-captain",
        )
        .unwrap();
    let duplicate = dispatch_authenticated(
        &duplicate_ctx,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(duplicate
        .error
        .as_deref()
        .is_some_and(|error| error.contains("missing or ambiguous")));

    let crew_store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let crew = crew_store
        .mint_and_bind(
            crate::identity::Role::Crew,
            Some("lease-ship".into()),
            "lease-captain",
        )
        .unwrap();
    let crew_ctx = test_ctx("global-control")
        .with_identity_store(crew_store)
        .with_live_sessions(|| Ok(vec!["th_lease-captain".into()]));
    let crew_result = dispatch_authenticated(
        &crew_ctx,
        req_session(
            "read-global-control",
            &crew.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(crew_result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("control_reauthentication_required")));

    let (removed_ctx, _, removed_identities, removed_identity) = captain_lease_fixture(true);
    removed_identities.retire(&removed_identity.id).unwrap();
    let removed = dispatch_authenticated(
        &removed_ctx,
        req_session(
            "read-global-control",
            &removed_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(removed
        .error
        .as_deref()
        .is_some_and(|error| { error.contains("durable session identity could not be verified") }));

    let (expired_ctx, _, _, expired_identity) = captain_lease_fixture(true);
    expired_ctx.control_leases.insert_test(
        "expired-lease",
        CaptainControlLease {
            identity_id: expired_identity.id.clone(),
            terminal_id: "lease-captain".into(),
            authority: LeaseAuthority::Captain {
                ship_slug: "lease-ship".into(),
                project_id: "lease-project".into(),
                generation: expired_ctx.captains.test_scoped_authority_generation(
                    "lease-ship",
                    "lease-captain",
                    "lease-project",
                ),
            },
            expires_at: Instant::now() - Duration::from_secs(1),
            expires_at_epoch_ms: now_ms().saturating_sub(1),
        },
    );
    let expired = dispatch_authenticated(
        &expired_ctx,
        req_session(
            "expired-lease",
            &expired_identity.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(
        expired.error.as_deref(),
        Some("unauthorized: bad control token")
    );
}

#[test]
fn captain_identity_reacquires_after_control_context_restart_and_credential_rotation() {
    let (first, captains, identities, identity) = captain_lease_fixture(true);
    let initial = dispatch_authenticated(
        &first,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    let old_lease = initial.result.unwrap()["lease"]
        .as_str()
        .unwrap()
        .to_string();

    let restarted = test_ctx("rotated-global-control")
        .with_captains_registry(captains.clone())
        .with_identity_store(identities)
        .with_live_sessions(|| Ok(vec![tmux_target("lease-captain")]));
    let stale = dispatch_authenticated(
        &restarted,
        req_session(&old_lease, &identity.secret, "my_capability", Value::Null),
    );
    assert_eq!(
        stale.error.as_deref(),
        Some("unauthorized: bad control token")
    );

    let renewed = dispatch_authenticated(
        &restarted,
        req_session(
            "read-rotated-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let result = renewed.result.unwrap();
    let new_lease = result["lease"].as_str().unwrap();
    assert_ne!(new_lease, old_lease);
    let bootstrap = dispatch_authenticated(
        &restarted,
        req_session(
            new_lease,
            &identity.secret,
            "captain_bootstrap",
            json!({"captainSessionId": "lease-captain"}),
        ),
    );
    assert!(bootstrap.ok, "{:?}", bootstrap.error);
    let bootstrap = bootstrap.result.unwrap();
    assert_eq!(bootstrap["captain"]["terminalId"], "lease-captain");
    assert_eq!(bootstrap["captain"]["shipSlug"], "lease-ship");
    assert_eq!(bootstrap["captain"]["assignment"], "Package 0");
    assert_eq!(bootstrap["project"]["projectId"], "lease-project");
    assert_eq!(bootstrap["agentCount"], 0);
    assert_eq!(bootstrap["recoverySource"], "captains-registry");
    assert_eq!(captains.snapshot().captains.len(), 1);
    assert_eq!(captains.snapshot().projects.len(), 1);
}

#[test]
fn durable_cortana_stays_authoritative_across_reload_and_generic_release_denial() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captains_path = captains_tmp(&format!("cortana-apex-{nonce}"));
    let identities_path =
        std::env::temp_dir().join(format!("t-hub-cortana-apex-identities-{nonce}.json"));
    let current_tile = format!("co{}", &nonce[..6]);
    let stale_tile = format!("st{}", &nonce[..6]);
    let current_secret;
    let stale_secret;

    {
        let registry = CaptainsRegistry::load(captains_path.clone());
        let identities = crate::identity::IdentityStore::load(identities_path.clone());
        current_secret = mint_current_cortana_session(&identities, &registry, &current_tile);
        let stale = identities.mint(crate::identity::Role::Cortana).unwrap();
        identities.bind_tile(&stale.id, &stale_tile).unwrap();
        stale_secret = stale.secret;
    }

    // A reconnect after process reload preserves the exact durable bearer,
    // while a second mint-time Cortana identity is role-demoted and non-apex.
    {
        let ctx = test_ctx("cortana-apex-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )));
        let current = resolve_identity(&ctx, &current_secret).unwrap();
        assert_eq!(current.fleet_role, Some(FleetRole::Cortana));
        assert!(caller_is_apex(Some(&current), false));

        let stale = resolve_identity(&ctx, &stale_secret).unwrap();
        assert_eq!(stale.fleet_role, None);
        assert_eq!(stale.mint_role, crate::identity::Role::Unknown);
        assert!(!caller_is_apex(Some(&stale), false));
        let denied = dispatch_authenticated(
            &ctx,
            req_session(
                "cortana-apex-token",
                &stale_secret,
                "commission_captain",
                json!({}),
            ),
        );
        assert!(!denied.ok);
        let error = denied.error.unwrap_or_default();
        assert!(
            error.contains("General/Cortana"),
            "unexpected denial: {error}"
        );

        let denied_release = release_captain(
            &ctx,
            &json!({"captainSessionId": current_tile}),
            Some(&current),
            false,
        )
        .unwrap_err();
        assert!(denied_release.contains("durable backend-owned singleton"));

        let preserved = resolve_identity(&ctx, &current_secret).unwrap();
        assert_eq!(preserved.fleet_role, Some(FleetRole::Cortana));
        assert_eq!(preserved.mint_role, crate::identity::Role::Cortana);
        assert!(caller_is_apex(Some(&preserved), false));
        let snapshot = ctx.captains.snapshot();
        assert_eq!(
            snapshot.cortana.terminal_id.as_deref(),
            Some(current_tile.as_str())
        );
        assert_eq!(
            snapshot
                .captains
                .iter()
                .filter(|record| record.role == FleetRole::Cortana)
                .count(),
            1
        );
    }

    // The same durable identity, Fleet claim, and bearer survive reload.
    {
        let ctx = test_ctx("cortana-apex-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )));
        let preserved = resolve_identity(&ctx, &current_secret).unwrap();
        assert_eq!(preserved.fleet_role, Some(FleetRole::Cortana));
        assert_eq!(preserved.mint_role, crate::identity::Role::Cortana);
        assert!(caller_is_apex(Some(&preserved), false));
        let snapshot = ctx.captains.snapshot();
        assert_eq!(
            snapshot.cortana.terminal_id.as_deref(),
            Some(current_tile.as_str())
        );
        assert_eq!(
            snapshot
                .captains
                .iter()
                .filter(|record| record.role == FleetRole::Cortana)
                .count(),
            1
        );
    }

    for path in [
        captains_path.with_extension("json.bak"),
        captains_path,
        identities_path,
    ] {
        std::fs::remove_file(path).ok();
    }
}
