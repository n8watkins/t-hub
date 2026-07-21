use std::process::Command;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_th"));
    command.env_remove("T_HUB_CONTROL_ADDR");
    command.env_remove("T_HUB_CONTROL_TOKEN");
    command
}

#[test]
fn project_registration_requires_a_nonempty_name_before_endpoint_discovery() {
    for args in [
        vec!["projects", "register", "/home/natkins/project", "--json"],
        vec![
            "projects",
            "register",
            "/home/natkins/project",
            "--name",
            " ",
            "--json",
        ],
        vec!["projects", "init", "/home/natkins/project", "--json"],
    ] {
        let output = cli().args(args).output().expect("run projects command");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["data"], serde_json::Value::Null);
        assert_eq!(envelope["error"]["code"], 2);
        assert_eq!(envelope["error"]["kind"], "usage");
        assert_eq!(envelope["error"]["retryable"], serde_json::Value::Null);
        assert_eq!(envelope["error"]["suggestion"], serde_json::Value::Null);
        assert_eq!(envelope["error"]["details"], serde_json::Value::Null);
        assert!(envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--name must be non-empty"));
    }
}
