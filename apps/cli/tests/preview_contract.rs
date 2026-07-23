use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::mpsc;

use serde_json::{json, Value};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_th"))
        .env_remove("T_HUB_CONTROL_ADDR")
        .env_remove("T_HUB_CONTROL_TOKEN")
        .env_remove("T_HUB_CONTROL_FILE")
        .args(args)
        .output()
        .expect("run th")
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

#[test]
fn process_changes_require_confirmation_before_endpoint_discovery() {
    for args in [
        vec![
            "preview",
            "start",
            "/tmp/project",
            "--project-id",
            "project-1",
            "--request-id",
            "request-1",
            "--json",
        ],
        vec![
            "preview",
            "stop",
            "--project-id",
            "project-1",
            "--request-id",
            "request-1",
            "--json",
        ],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(5));
        assert!(output.stderr.is_empty());
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["kind"], "gated");
        assert!(envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--confirm"));
    }
}

#[test]
fn discover_forwards_only_the_typed_root() {
    let (output, request) = call_with_server(
        &["preview", "discover", "/tmp/project", "--json"],
        json!({
            "canonicalRoot": "/tmp/project",
            "discoveryFingerprint": "sha256:test",
            "targets": [],
            "count": 0
        }),
    );
    assert!(output.status.success());
    assert_eq!(request["command"], "preview_discover");
    assert_eq!(request["args"], json!({ "rootPath": "/tmp/project" }));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "preview discover");
    assert_eq!(envelope["ok"], true);
}

#[test]
fn start_forwards_exact_scope_target_and_idempotency_identity() {
    let authoritative_url = "http://172.30.1.3:5173/app";
    let (output, request) = call_with_server(
        &[
            "preview",
            "start",
            "/tmp/project",
            "--project-id",
            "project-1",
            "--workspace-id",
            "workspace-1",
            "--target-id",
            "workspace:web:dev",
            "--fingerprint",
            "sha256:test",
            "--request-id",
            "request-1",
            "--confirm",
            "--json",
        ],
        json!({
            "outcome": "applied",
            "status": {
                "state": "running",
                "previewUrl": authoritative_url
            }
        }),
    );
    assert!(output.status.success());
    assert_eq!(request["command"], "preview_start");
    assert_eq!(
        request["args"],
        json!({
            "rootPath": "/tmp/project",
            "scope": {
                "projectId": "project-1",
                "workspaceId": "workspace-1"
            },
            "target": {
                "scope": {
                    "projectId": "project-1",
                    "workspaceId": "workspace-1"
                },
                "targetId": "workspace:web:dev",
                "discoveryFingerprint": "sha256:test"
            },
            "requestId": "request-1",
        })
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["data"]["status"]["previewUrl"],
        authoritative_url
    );
}

#[test]
fn preview_rejects_unknown_flags_and_incomplete_target_identity() {
    for args in [
        vec!["preview", "status", "--mystery", "--json"],
        vec![
            "preview",
            "start",
            "/tmp/project",
            "--project-id",
            "project-1",
            "--target-id",
            "root:dev",
            "--request-id",
            "request-1",
            "--confirm",
            "--json",
        ],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["kind"], "usage");
    }
}
