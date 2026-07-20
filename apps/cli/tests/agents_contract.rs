use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_th"));
    command
        .env_remove("T_HUB_CONTROL_ADDR")
        .env_remove("T_HUB_CONTROL_TOKEN")
        .args(args)
        .output()
        .expect("run th")
}

fn assert_usage(args: &[&str], message: &str) {
    let output = cli(args);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], 2);
    assert_eq!(envelope["error"]["kind"], "usage");
    assert_eq!(envelope["error"]["message"], message);
}

#[test]
fn unknown_flags_are_rejected_before_control_call() {
    assert_usage(
        &["agents", "show", "agent-1", "--bogus", "--json"],
        "unknown flag '--bogus'",
    );
}

#[test]
fn duplicate_flags_are_rejected() {
    assert_usage(
        &[
            "agents",
            "list",
            "--captain",
            "cap",
            "--captain",
            "other",
            "--json",
        ],
        "--captain may be provided only once",
    );
}

#[test]
fn missing_values_are_rejected() {
    assert_usage(
        &["agents", "events", "agent-1", "--limit", "--json"],
        "--limit expects a value",
    );
}

#[test]
fn extra_positionals_are_rejected() {
    assert_usage(
        &["agents", "show", "agent-1", "extra", "--json"],
        "usage: th agents show <agentSessionId> [--json] (expected 1 positional argument, got 2)",
    );
}

#[test]
fn stage_and_limit_values_are_validated() {
    assert_usage(
        &[
            "agents",
            "checkpoint",
            "agent-1",
            "summary",
            "--author",
            "author-1",
            "--stage",
            "invalid",
            "--json",
        ],
        "th agents checkpoint: --stage must be assigned, working, needsInput, readyForReview, awaitingIntegration, complete, or stopped",
    );
    assert_usage(
        &[
            "agents",
            "list",
            "--captain",
            "cap",
            "--limit",
            "0",
            "--json",
        ],
        "th agents list: --limit must be an integer from 1 to 100",
    );
    assert_usage(
        &["agents", "events", "agent-1", "--limit", "101", "--json"],
        "th agents events: --limit must be an integer from 1 to 100",
    );
}
