use std::process::Command;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_th"));
    command.env_remove("T_HUB_CONTROL_ADDR");
    command.env_remove("T_HUB_CONTROL_TOKEN");
    command
}

#[test]
fn powder_is_a_local_retirement_tombstone() {
    let output = cli()
        .args(["powder", "evidence", "--json"])
        .output()
        .expect("run retired Powder command");

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "powder evidence");
    assert_eq!(envelope["error"]["code"], 4);
    assert_eq!(envelope["error"]["kind"], "powder_retired");
    assert!(envelope["error"]["message"]
        .as_str()
        .unwrap()
        .contains("th agents"));
}

#[test]
fn powder_help_remains_available_without_endpoint_discovery() {
    let output = cli()
        .args(["powder", "--help"])
        .output()
        .expect("run Powder help");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("usage: th powder"));
}
