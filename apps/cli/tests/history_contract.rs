use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::mpsc;

use serde_json::{json, Value};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_th"))
        .env_remove("T_HUB_CONTROL_ADDR")
        .env_remove("T_HUB_CONTROL_TOKEN")
        .args(args)
        .output()
        .expect("run th")
}

fn assert_error(args: &[&str], code: i32, kind: &str) -> Value {
    let output = cli(args);
    assert_eq!(output.status.code(), Some(code));
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], code);
    assert_eq!(envelope["error"]["kind"], kind);
    envelope
}

fn call_with_server(args: &[&str], result: Value) -> (Output, Value) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        sender
            .send(serde_json::from_str::<Value>(&request).unwrap())
            .unwrap();
        writeln!(stream, "{}", json!({ "ok": true, "result": result })).unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_th"))
        .env("T_HUB_CONTROL_ADDR", address.to_string())
        .env("T_HUB_CONTROL_TOKEN", "test-token")
        .env_remove("T_HUB_CONTROL_FILE")
        .args(args)
        .output()
        .expect("run th against test server");
    let request = receiver.recv().unwrap();
    server.join().unwrap();
    (output, request)
}

fn call_with_error_server(args: &[&str], error: &str, retryable: bool) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let error = error.to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({ "ok": false, "error": error, "retryable": retryable })
        )
        .unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_th"))
        .env("T_HUB_CONTROL_ADDR", address.to_string())
        .env("T_HUB_CONTROL_TOKEN", "test-token")
        .env_remove("T_HUB_CONTROL_FILE")
        .args(args)
        .output()
        .expect("run th against test error server");
    server.join().unwrap();
    output
}

#[test]
fn history_list_all_requests_the_contract_maximum() {
    let (output, request) = call_with_server(
        &["history", "--all", "--json"],
        json!({
            "schemaVersion": 1,
            "entries": [],
            "count": 0,
            "total": 0,
            "truncated": false,
            "sources": []
        }),
    );
    assert!(output.status.success());
    assert_eq!(request["command"], "history_list");
    assert_eq!(request["args"]["includeArchived"], true);
    assert_eq!(request["args"]["limit"], 500);
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "history list");
    assert_eq!(envelope["ok"], true);
}

#[test]
fn history_resume_requires_confirmation_before_endpoint_discovery() {
    let envelope = assert_error(
        &[
            "history",
            "resume",
            "history:v1:one",
            "--request-id",
            "request-one",
            "--json",
        ],
        5,
        "gated",
    );
    assert!(envelope["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--confirm"));
}

#[test]
fn history_resume_forwards_only_exact_backend_identity_inputs() {
    let (output, request) = call_with_server(
        &[
            "history",
            "resume",
            "history:v1:one",
            "--request-id",
            "request-one",
            "--tab",
            "tab-one",
            "--confirm",
            "--json",
        ],
        json!({ "status": "active", "terminalId": "terminal-one" }),
    );
    assert!(output.status.success());
    assert_eq!(request["command"], "history_resume");
    assert_eq!(
        request["args"],
        json!({
            "historyId": "history:v1:one",
            "requestId": "request-one",
            "targetTabId": "tab-one"
        })
    );
}

#[test]
fn history_rejects_unknown_duplicate_and_extra_arguments() {
    assert_error(&["history", "list", "--mystery", "--json"], 2, "usage");
    assert_error(
        &["history", "list", "--limit=10", "--limit", "20", "--json"],
        2,
        "usage",
    );
    assert_error(&["history", "focus", "one", "extra", "--json"], 2, "usage");
}

#[test]
fn history_rejects_boolean_values_before_endpoint_discovery() {
    let envelope = assert_error(&["history", "--all=no", "--json"], 2, "usage");
    assert_eq!(envelope["command"], "history list");

    let output = cli(&["history", "list", "--json=false"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept a value"));
}

#[test]
fn history_list_errors_use_one_stable_command_label() {
    for args in [
        ["history", "ls", "--mystery", "--json"].as_slice(),
        ["history", "--mystery", "--json"].as_slice(),
    ] {
        let envelope = assert_error(args, 2, "usage");
        assert_eq!(envelope["command"], "history list");
    }
}

#[test]
fn history_resume_rejects_invalid_request_and_tab_ids_before_discovery() {
    for request_id in ["bad!", &"a".repeat(129)] {
        let envelope = assert_error(
            &[
                "history",
                "resume",
                "history:v1:one",
                "--request-id",
                request_id,
                "--confirm",
                "--json",
            ],
            2,
            "usage",
        );
        assert_eq!(envelope["command"], "history resume");
    }

    let envelope = assert_error(
        &[
            "history",
            "resume",
            "history:v1:one",
            "--request-id",
            "request-one",
            "--tab=",
            "--confirm",
            "--json",
        ],
        2,
        "usage",
    );
    assert_eq!(envelope["command"], "history resume");
}

#[test]
fn history_rejects_blank_history_ids_before_discovery() {
    for args in [
        ["history", "focus", "", "--json"].as_slice(),
        [
            "history",
            "resume",
            " ",
            "--request-id",
            "request-one",
            "--confirm",
            "--json",
        ]
        .as_slice(),
    ] {
        let envelope = assert_error(args, 2, "usage");
        assert!(envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("historyId must not be blank"));
    }
}

#[test]
fn history_resume_retryable_error_keeps_machine_flag_and_same_id_guidance() {
    let output = call_with_error_server(
        &[
            "history",
            "resume",
            "history:v1:one",
            "--request-id",
            "request-one",
            "--confirm",
            "--json",
        ],
        "history_resume_failed: spawned terminal has no Workspace placement",
        true,
    );
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["retryable"], true);
    assert!(envelope["error"]["message"]
        .as_str()
        .unwrap()
        .contains("same --request-id 'request-one'"));
}

#[test]
fn history_subcommand_help_does_not_discover_the_endpoint() {
    for args in [
        ["history", "--help"].as_slice(),
        ["history", "list", "--help"].as_slice(),
        ["history", "resume", "--help"].as_slice(),
    ] {
        let output = cli(args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("th history commands"));
    }
}
