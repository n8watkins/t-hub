use super::*;

#[test]
fn watch_fleet_requires_a_live_orchestrator_terminal() {
    let ctx = test_ctx("t");
    // No live tmux for this id -> the arm is refused so a bogus id can't arm a
    // watch that could never deliver.
    let err = watch_fleet(
        &ctx,
        &json!({ "orchestratorSessionId": "nolivetile" }),
        None,
        true,
    )
    .unwrap_err();
    assert!(err.contains("no live terminal"), "got: {err}");
    // And it requires the id at all.
    assert!(watch_fleet(&ctx, &json!({}), None, true)
        .unwrap_err()
        .contains("orchestratorSessionId"));
}

#[test]
fn unwatch_and_list_fleet_watches_on_empty_registry() {
    let ctx = test_ctx("t");
    let v = unwatch_fleet(
        &ctx,
        &json!({ "orchestratorSessionId": "whoever" }),
        None,
        true,
    )
    .unwrap();
    assert_eq!(v.get("removed").and_then(|x| x.as_bool()), Some(false));
    let list = list_fleet_watches(&ctx).unwrap();
    assert_eq!(list.get("count").and_then(|x| x.as_u64()), Some(0));
}

#[test]
fn arm_then_list_and_disarm_a_watch_via_the_registry() {
    // The command's tmux liveness guard needs a real session, so exercise the
    // arm/list/disarm round-trip through the shared registry directly (the
    // command is a thin validate-then-arm wrapper over exactly this).
    let ctx = test_ctx("t");
    ctx.fleet_watches
        .arm("orc12345", crate::fleet::WatchScope::Captains, vec![]);
    let list = list_fleet_watches(&ctx).unwrap();
    assert_eq!(list.get("count").and_then(|x| x.as_u64()), Some(1));
    let removed = unwatch_fleet(
        &ctx,
        &json!({ "orchestratorSessionId": "orc12345" }),
        None,
        true,
    )
    .unwrap();
    assert_eq!(removed.get("removed").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(
        list_fleet_watches(&ctx)
            .unwrap()
            .get("count")
            .and_then(|x| x.as_u64()),
        Some(0)
    );
}

#[test]
fn parse_watch_scope_accepts_captains_all_and_explicit_lists() {
    use crate::fleet::WatchScope;
    assert_eq!(parse_watch_scope(&json!({})).unwrap(), WatchScope::Captains);
    assert_eq!(
        parse_watch_scope(&json!({ "scope": "all" })).unwrap(),
        WatchScope::All
    );
    assert_eq!(
        parse_watch_scope(&json!({ "scope": ["a", "b"] })).unwrap(),
        WatchScope::Sessions(vec!["a".into(), "b".into()])
    );
    assert!(parse_watch_scope(&json!({ "scope": "bogus" })).is_err());
    assert!(parse_watch_scope(&json!({ "scope": [] })).is_err());
}

#[test]
fn scoped_captain_cannot_arm_or_remove_a_foreign_ship_watch() {
    let (ctx, captains, _, identity) = captain_lease_fixture(true);
    captains
        .claim_test("foreign-captain", Some("foreign-ship"), vec![])
        .unwrap();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    let lease = renewed.result.unwrap()["lease"]
        .as_str()
        .unwrap()
        .to_string();
    for command in ["watch_fleet", "unwatch_fleet"] {
        let response = dispatch_authenticated(
            &ctx,
            req_session(
                &lease,
                &identity.secret,
                command,
                json!({"orchestratorSessionId": "foreign-captain"}),
            ),
        );
        assert!(!response.ok, "{command} accepted a foreign watch");
        assert!(response
            .error
            .as_deref()
            .is_some_and(|error| error.contains("own or same-ship watch")));
    }
}
