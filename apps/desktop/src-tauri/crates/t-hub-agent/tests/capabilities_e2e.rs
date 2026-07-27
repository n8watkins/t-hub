use std::process::Command;

use serde_json::Value;

#[test]
fn real_agent_reports_codex_hook_capability_without_runtime_state() {
    let output = Command::new(env!("CARGO_BIN_EXE_t-hub-agent"))
        .arg("--capabilities-json")
        .env_remove("HOME")
        .env_remove("T_HUB_AGENT_JOURNAL_DIR")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "capability probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schemaVersion"], 1);
    assert_eq!(report["agentVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        report["capabilities"],
        serde_json::json!(["codex-native-hooks-v1"])
    );
}
