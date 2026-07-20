use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::mpsc;

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

fn cli_with_server(args: &[&str]) -> (Output, serde_json::Value) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        sender
            .send(serde_json::from_str::<serde_json::Value>(&line).unwrap())
            .unwrap();
        stream
            .write_all(b"{\"ok\":true,\"result\":{\"accepted\":true}}\n")
            .unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_th"))
        .env("T_HUB_CONTROL_ADDR", address)
        .env("T_HUB_CONTROL_TOKEN", "test-control-token")
        .env_remove("T_HUB_CONTROL_FILE")
        .args(args)
        .output()
        .expect("run th");
    let request = receiver.recv().unwrap();
    worker.join().unwrap();
    (output, request)
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
    assert_usage(
        &[
            "agents",
            "list",
            "--project",
            "project-1",
            "--state",
            "archived",
            "--json",
        ],
        "th agents list: --state must be active or removed",
    );
}

#[test]
fn dispatch_baseline_and_lane_inputs_are_validated_before_control_discovery() {
    assert_usage(
        &[
            "agents",
            "start",
            "--request-id",
            "request-1",
            "--captain",
            "captain-1",
            "--directory",
            "/tmp/worktree",
            "--assignment",
            "Implement the bounded change",
            "--source-commit",
            "not-a-commit",
            "--lane-id",
            "lane-1",
            "--json",
        ],
        "th agents start: --source-commit must be an exact 40- or 64-character hexadecimal commit",
    );
    assert_usage(
        &[
            "agents",
            "preflight",
            "--project",
            "project-1",
            "--lanes-json",
            "[]",
            "--json",
        ],
        "th agents preflight: --lanes-json must contain at least one lane",
    );
}

#[test]
fn delivery_state_and_evidence_shape_are_validated_before_control_discovery() {
    assert_usage(
        &[
            "agents",
            "delivery",
            "agent-1",
            "complete",
            "--evidence-json",
            "{}",
            "--json",
        ],
        "th agents delivery: state must be implemented, reviewed, tested, integrated, packaged, installed, or liveVerified",
    );
    assert_usage(
        &[
            "agents",
            "delivery",
            "agent-1",
            "tested",
            "--evidence-json",
            "[]",
            "--json",
        ],
        "th agents delivery: --evidence-json must be a JSON object",
    );
}

#[test]
fn start_sends_the_exact_baseline_and_adaptive_lane_contract() {
    let source_commit = "a".repeat(40);
    let (output, request) = cli_with_server(&[
        "agents",
        "start",
        "--request-id",
        "request-1",
        "--captain",
        "captain-1",
        "--directory",
        "/tmp/worktree",
        "--assignment",
        "Implement the bounded change",
        "--source-commit",
        &source_commit,
        "--lane-id",
        "lane-1",
        "--dependencies",
        "interface-first,baseline-ready",
        "--mutable-files",
        "src/a.rs,src/b.rs",
        "--mutable-schemas",
        "captains-v18",
        "--mutable-interfaces",
        "control-v2",
        "--integration-contracts-json",
        "[]",
        "--visible-product-bug",
        "--json",
    ]);
    assert!(output.status.success());
    assert_eq!(request["command"], "start_agent");
    assert_eq!(request["args"]["sourceCommit"], source_commit);
    assert_eq!(request["args"]["visibleProductBug"], true);
    assert_eq!(request["args"]["laneId"], "lane-1");
    assert_eq!(
        request["args"]["dependencies"],
        serde_json::json!(["baseline-ready", "interface-first"])
    );
    assert_eq!(
        request["args"]["mutableFiles"],
        serde_json::json!(["src/a.rs", "src/b.rs"])
    );
    assert_eq!(
        request["args"]["integrationContracts"],
        serde_json::json!([])
    );
    assert!(request["args"].get("capability").is_none());
    assert!(request["args"].get("admissionPurpose").is_none());
}

#[test]
fn start_forwards_only_a_valid_durable_admission_purpose() {
    let source_commit = "a".repeat(40);
    let (output, request) = cli_with_server(&[
        "agents",
        "start",
        "--request-id",
        "request-admin",
        "--captain",
        "captain-1",
        "--directory",
        "/tmp/worktree",
        "--assignment",
        "Perform delegated administration",
        "--source-commit",
        &source_commit,
        "--lane-id",
        "admin-lane",
        "--integration-contracts-json",
        "[]",
        "--admission-purpose",
        "fleet-admin",
        "--json",
    ]);
    assert!(output.status.success());
    assert_eq!(request["command"], "start_agent");
    assert_eq!(request["args"]["admissionPurpose"], "fleet-admin");
    assert!(request["args"].get("capability").is_none());
}

#[test]
fn start_rejects_unknown_admission_purpose_before_control_call() {
    let source_commit = "a".repeat(40);
    assert_usage(
        &[
            "agents",
            "start",
            "--request-id",
            "request-admin",
            "--captain",
            "captain-1",
            "--directory",
            "/tmp/worktree",
            "--assignment",
            "Perform delegated administration",
            "--source-commit",
            &source_commit,
            "--lane-id",
            "admin-lane",
            "--integration-contracts-json",
            "[]",
            "--admission-purpose",
            "captain",
            "--json",
        ],
        "th agents start: --admission-purpose must be ordinary, fleet-admin, ship-admin, or recovery",
    );
}

#[test]
fn delivery_forwards_the_ordered_integration_manifest_without_rewriting_it() {
    let baseline = "a".repeat(40);
    let interface_commit = "b".repeat(40);
    let result_commit = "c".repeat(40);
    let canonical_commit = "d".repeat(40);
    let evidence = serde_json::json!({
        "sourceCommit": result_commit,
        "canonicalBaseline": "main",
        "canonicalCommit": canonical_commit,
        "reference": "git://main/integration",
        "manifest": {
            "integrationOwnerIdentity": "captain-identity-1",
            "inputs": [
                {
                    "laneId": "shared-interface",
                    "agentSessionId": "agent-interface",
                    "sourceBaseline": baseline,
                    "resultingCommit": interface_commit
                },
                {
                    "laneId": "implementation",
                    "agentSessionId": "agent-implementation",
                    "sourceBaseline": baseline,
                    "resultingCommit": result_commit
                }
            ]
        }
    })
    .to_string();
    let (output, request) = cli_with_server(&[
        "agents",
        "delivery",
        "agent-implementation",
        "integrated",
        "--evidence-json",
        &evidence,
        "--json",
    ]);

    assert!(output.status.success());
    assert_eq!(request["command"], "record_agent_delivery");
    assert_eq!(request["args"]["state"], "integrated");
    assert_eq!(
        request["args"]["evidence"]["manifest"]["inputs"][0]["laneId"],
        "shared-interface"
    );
    assert_eq!(
        request["args"]["evidence"]["manifest"]["inputs"][1]["laneId"],
        "implementation"
    );
}

#[test]
fn delivery_forwards_the_complete_artifact_manifest_without_rewriting_it() {
    let source_commit = "c".repeat(40);
    let git_tree = "d".repeat(40);
    let installer_sha256 = "a".repeat(64);
    let evidence = serde_json::json!({
        "artifactId": "t-hub-dev-0.3.107",
        "sourceBaseline": source_commit,
        "reference": "artifact://windows/installer",
        "manifest": {
            "branch": "main",
            "sourceCommit": source_commit,
            "gitTree": git_tree,
            "version": "0.3.107",
            "installerSha256": installer_sha256,
            "builtAt": 1_784_525_600_000_u64,
            "signatureStatus": "verified"
        }
    })
    .to_string();
    let (output, request) = cli_with_server(&[
        "agents",
        "delivery",
        "agent-implementation",
        "packaged",
        "--evidence-json",
        &evidence,
        "--json",
    ]);

    assert!(output.status.success());
    assert_eq!(request["command"], "record_agent_delivery");
    assert_eq!(request["args"]["state"], "packaged");
    assert_eq!(
        request["args"]["evidence"]["manifest"]["installerSha256"],
        "a".repeat(64)
    );
    assert_eq!(
        request["args"]["evidence"]["manifest"]["signatureStatus"],
        "verified"
    );
}
