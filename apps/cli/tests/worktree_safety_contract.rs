use std::process::Command;

#[test]
fn prune_execution_fails_closed_before_repository_inspection() {
    let output = Command::new(env!("CARGO_BIN_EXE_th"))
        .env_remove("T_HUB_CONTROL_ADDR")
        .env_remove("T_HUB_CONTROL_TOKEN")
        .args(["worktree", "prune", "/does/not/exist", "--yes", "--json"])
        .output()
        .expect("run th");
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stderr.is_empty());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["command"], "worktree prune");
    assert_eq!(response["error"]["kind"], "gated");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("authoritative worktree safety service"));
}
