use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use serde_json::{json, Value};

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

struct MockControl {
    handshake: PathBuf,
    request: Receiver<Value>,
    server: thread::JoinHandle<()>,
}

impl MockControl {
    fn start(response: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock control listener");
        let addr = listener.local_addr().expect("mock control address");
        let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "th-cli-powder-contract-{}-{test_id}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create test directory");
        let handshake = dir.join("control.json");
        fs::write(
            &handshake,
            serde_json::to_vec(&json!({
                "addr": addr.to_string(),
                "token": "test-control-token",
                "protocol_version": 2
            }))
            .unwrap(),
        )
        .expect("write mock handshake");

        let (tx, request) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept CLI connection");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone mock stream"))
                .read_line(&mut line)
                .expect("read CLI request");
            let request_value = serde_json::from_str(&line).expect("request is JSON");
            tx.send(request_value).expect("publish request");
            let mut encoded = serde_json::to_vec(&response).expect("serialize mock response");
            encoded.push(b'\n');
            stream.write_all(&encoded).expect("write mock response");
        });
        Self {
            handshake,
            request,
            server,
        }
    }

    fn run(&self, args: &[&str], session_token: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_th"));
        command
            .args(args)
            .env("T_HUB_CONTROL_FILE", &self.handshake)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .env_remove("T_HUB_SESSION_TOKEN");
        if let Some(token) = session_token {
            command.env("T_HUB_SESSION_TOKEN", token);
        }
        command.output().expect("run th")
    }

    fn finish(self) -> Value {
        let request = self.request.recv().expect("receive CLI request");
        self.server.join().expect("join mock control server");
        if let Some(dir) = self.handshake.parent() {
            let _ = fs::remove_dir_all(dir);
        }
        request
    }
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not one JSON value: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_clean_json_success(output: &Output, command: &str) -> Value {
    assert!(output.status.success(), "output: {output:?}");
    assert!(
        output.stderr.is_empty(),
        "stderr must stay clean: {output:?}"
    );
    let envelope = stdout_json(output);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], command);
    assert!(envelope["data"].is_object());
    assert!(envelope["error"].is_null());
    envelope
}

#[test]
fn append_forwards_only_bound_message_and_session_identity() {
    let mock = MockControl::start(json!({
        "ok": true,
        "result": {
            "cardId": "card-bound",
            "runId": "run-bound",
            "entry": { "message": "focused tests pass" }
        }
    }));
    let output = mock.run(
        &[
            "powder",
            "work-log",
            "append",
            " focused tests pass ",
            "--json",
        ],
        Some("crew-session-token"),
    );
    let envelope = assert_clean_json_success(&output, "powder work-log append");
    assert_eq!(envelope["data"]["runId"], "run-bound");

    let request = mock.finish();
    assert_eq!(request["command"], "append_crew_powder_work_log");
    assert_eq!(request["session"], "crew-session-token");
    assert_eq!(
        request["args"],
        json!({ "message": " focused tests pass " })
    );
}

#[test]
fn evidence_defaults_to_a_deterministic_bounded_read() {
    let mock = MockControl::start(json!({
        "ok": true,
        "result": {
            "card": { "id": "card-bound" },
            "run": { "id": "run-bound" },
            "workLog": [],
            "count": 0,
            "limit": 20,
            "hasMore": false
        }
    }));
    let output = mock.run(&["powder", "evidence", "--json"], Some("crew-token"));
    let envelope = assert_clean_json_success(&output, "powder evidence");
    assert_eq!(envelope["data"]["count"], 0);
    assert_eq!(envelope["data"]["workLog"], json!([]));

    let request = mock.finish();
    assert_eq!(request["command"], "read_crew_powder_evidence");
    assert_eq!(request["args"], json!({ "limit": 20 }));
}

#[test]
fn captain_evidence_and_completion_target_only_a_crew_binding() {
    let evidence = MockControl::start(json!({
        "ok": true,
        "result": { "count": 1, "limit": 7, "hasMore": false, "workLog": [{}] }
    }));
    let output = evidence.run(
        &[
            "powder",
            "evidence",
            "--crew",
            "crew-123",
            "--limit=7",
            "--json",
        ],
        Some("captain-token"),
    );
    assert_clean_json_success(&output, "powder evidence");
    let request = evidence.finish();
    assert_eq!(
        request["args"],
        json!({ "crewSessionId": "crew-123", "limit": 7 })
    );

    let completion = MockControl::start(json!({
        "ok": true,
        "result": { "cardId": "card-bound", "runId": "run-bound", "status": "done" }
    }));
    let output = completion.run(
        &[
            "powder",
            "complete",
            "crew-123",
            "--proof",
            "tests: 42 passed",
            "--json",
        ],
        Some("captain-token"),
    );
    assert_clean_json_success(&output, "powder complete");
    let request = completion.finish();
    assert_eq!(request["command"], "complete_crew_powder");
    assert_eq!(
        request["args"],
        json!({ "crewSessionId": "crew-123", "proof": "tests: 42 passed" })
    );
}

#[test]
fn invalid_flags_positionals_and_limits_fail_before_endpoint_discovery() {
    let missing = std::env::temp_dir().join(format!(
        "th-cli-missing-control-{}",
        NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
    ));
    for args in [
        vec!["powder", "evidence", "--card", "escape", "--json"],
        vec!["powder", "evidence", "extra", "--json"],
        vec!["powder", "evidence", "--limit", "21", "--json"],
        vec!["powder", "work-log", "append", "", "--json"],
        vec!["powder", "work-log", "append", " \t\n ", "--json"],
        vec!["powder", "complete", "crew-123", "--json"],
        vec![
            "powder", "complete", "crew-123", "--proof", " \t\n ", "--json",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_th"))
            .args(args)
            .env("T_HUB_CONTROL_FILE", &missing)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .output()
            .expect("run invalid th command");
        assert_eq!(output.status.code(), Some(2), "output: {output:?}");
        assert!(output.stderr.is_empty(), "JSON stderr: {output:?}");
        let envelope = stdout_json(&output);
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], 2);
        assert_eq!(envelope["error"]["kind"], "usage");
    }

    for args in [
        vec![
            "powder".to_string(),
            "work-log".to_string(),
            "append".to_string(),
            "é".repeat(8193),
            "--json".to_string(),
        ],
        vec![
            "powder".to_string(),
            "complete".to_string(),
            "crew-123".to_string(),
            "--proof".to_string(),
            "é".repeat(2049),
            "--json".to_string(),
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_th"))
            .args(args)
            .env("T_HUB_CONTROL_FILE", &missing)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .output()
            .expect("run oversized th command");
        assert_eq!(output.status.code(), Some(2), "output: {output:?}");
        assert!(output.stderr.is_empty(), "JSON stderr: {output:?}");
        let envelope = stdout_json(&output);
        assert_eq!(envelope["error"]["kind"], "usage");
        assert!(envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("byte UTF-8 limit"));
    }

    for forbidden in [
        "--card",
        "--card-id",
        "--run",
        "--run-id",
        "--profile",
        "--connection-profile",
        "--endpoint",
        "--repository",
        "--credential",
    ] {
        for mut args in [
            vec!["powder", "work-log", "append", "test evidence"],
            vec!["powder", "evidence"],
            vec!["powder", "complete", "crew-123", "--proof", "tests"],
        ] {
            args.extend([forbidden, "substitution", "--json"]);
            let output = Command::new(env!("CARGO_BIN_EXE_th"))
                .args(args)
                .env("T_HUB_CONTROL_FILE", &missing)
                .env_remove("T_HUB_CONTROL_ADDR")
                .env_remove("T_HUB_CONTROL_TOKEN")
                .output()
                .expect("run forbidden Powder authority substitution");
            assert_eq!(output.status.code(), Some(2), "output: {output:?}");
            assert!(output.stderr.is_empty(), "JSON stderr: {output:?}");
            let envelope = stdout_json(&output);
            assert_eq!(envelope["error"]["kind"], "usage");
            assert!(
                envelope["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("unknown flag"),
                "{forbidden} must be rejected as an authority escape: {envelope}"
            );
        }
    }
}

#[test]
fn backend_authorization_rejection_uses_the_gated_exit_taxonomy() {
    for (error, args) in [
        (
            "permission denied: only the owning Captain may complete this Crew card",
            vec![
                "powder",
                "complete",
                "foreign-crew",
                "--proof",
                "tests",
                "--json",
            ],
        ),
        (
            "unauthorized: 'append_crew_powder_work_log' requires the control capability (this token is read-only)",
            vec!["powder", "work-log", "append", "tests", "--json"],
        ),
        (
            "acl: read_crew_powder_evidence requires a valid Crew or Captain T_HUB_SESSION_TOKEN",
            vec!["powder", "evidence", "--json"],
        ),
    ] {
        let mock = MockControl::start(json!({ "ok": false, "error": error }));
        let output = mock.run(&args, Some("crew-token"));
        assert_eq!(output.status.code(), Some(5), "output: {output:?}");
        assert!(output.stderr.is_empty(), "JSON stderr: {output:?}");
        let envelope = stdout_json(&output);
        assert_eq!(envelope["ok"], false);
        assert!(envelope["data"].is_null());
        assert_eq!(envelope["error"]["code"], 5);
        assert_eq!(envelope["error"]["kind"], "gated");
        mock.finish();
    }
}

#[test]
fn powder_help_is_available_without_a_running_app() {
    for args in [
        vec!["powder", "--help"],
        vec!["powder", "work-log", "--help"],
        vec!["powder", "evidence", "--help"],
        vec!["powder", "complete", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_th"))
            .args(args)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .output()
            .expect("run th help");
        assert!(output.status.success(), "output: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("usage: th powder"));
    }
}
