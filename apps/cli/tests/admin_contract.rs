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

fn envelope(output: &Output) -> serde_json::Value {
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
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
fn cleanup_requires_exact_approval_and_confirmation_before_discovery() {
    let output = cli(&[
        "admin",
        "cleanup-session",
        "crew-1",
        "--approval",
        "approval-1",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(5));
    let response = envelope(&output);
    assert_eq!(response["command"], "admin cleanup-session");
    assert_eq!(response["error"]["kind"], "gated");
    assert_eq!(
        response["error"]["message"],
        "th admin cleanup-session requires --confirm before endpoint discovery or mutation"
    );
}

#[test]
fn role_and_operation_inputs_are_strict() {
    let output = cli(&[
        "admin",
        "appoint",
        "crew-1",
        "--role",
        "captain",
        "--operations",
        "inspectStatus",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        envelope(&output)["error"]["message"],
        "th admin appoint: --role must be shipAdmin or fleetAdmin"
    );

    let output = cli(&[
        "admin",
        "appoint",
        "crew-1",
        "--role",
        "shipAdmin",
        "--operations",
        "directImplementation",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        envelope(&output)["error"]["message"],
        "th admin appoint: unknown operation 'directImplementation'"
    );
}

#[test]
fn unknown_admin_flags_fail_before_discovery() {
    let output = cli(&["admin", "list", "--grant", "forged", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        envelope(&output)["error"]["message"],
        "unknown flag '--grant'"
    );
}

#[test]
fn exact_session_approval_and_cleanup_are_forwarded_without_authority_expansion() {
    let fabricated_scope = cli(&[
        "admin",
        "approve-session",
        "grant-1",
        "crew-1",
        "--ship",
        "alpha",
        "--json",
    ]);
    assert_eq!(fabricated_scope.status.code(), Some(2));
    assert_eq!(
        envelope(&fabricated_scope)["error"]["message"],
        "unknown flag '--ship'"
    );

    let (output, request) =
        cli_with_server(&["admin", "approve-session", "grant-1", "crew-1", "--json"]);
    assert!(output.status.success());
    assert_eq!(request["command"], "approve_admin_action");
    assert_eq!(
        request["args"],
        serde_json::json!({
            "grantId": "grant-1",
            "operation": "cleanupSession",
            "sessionId": "crew-1",
        })
    );

    let (output, request) = cli_with_server(&[
        "admin",
        "cleanup-session",
        "crew-1",
        "--approval",
        "approval-1",
        "--confirm",
        "--json",
    ]);
    assert!(output.status.success());
    assert_eq!(request["command"], "close_terminal");
    assert_eq!(request["args"]["sessionId"], "crew-1");
    assert_eq!(request["args"]["approvalId"], "approval-1");
    assert_eq!(request["args"]["force"], false);
}

#[test]
fn bounded_admin_operations_forward_typed_authoritative_targets() {
    let cases = [
        (
            vec!["admin", "maintain-session", "crew-1", "--json"],
            serde_json::json!({
                "operation": "maintainSession",
                "target": { "kind": "session", "sessionId": "crew-1" },
            }),
        ),
        (
            vec![
                "admin",
                "recover-resource",
                "worktree",
                "/tmp/worktree-1",
                "--json",
            ],
            serde_json::json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": "/tmp/worktree-1" },
            }),
        ),
        (
            vec!["admin", "prepare-retirement", "ship", "alpha", "--json"],
            serde_json::json!({
                "operation": "prepareRetirement",
                "target": { "kind": "ship", "shipSlug": "alpha" },
            }),
        ),
        (
            vec!["admin", "maintain-fleet-resource", "fleet", "--json"],
            serde_json::json!({
                "operation": "maintainFleetResource",
                "target": { "kind": "fleet" },
            }),
        ),
    ];

    for (args, expected) in cases {
        let (output, request) = cli_with_server(&args);
        assert!(output.status.success());
        assert_eq!(request["command"], "execute_admin_operation");
        assert_eq!(request["args"], expected);
    }
}

#[test]
fn admin_operation_kinds_are_strict_before_endpoint_discovery() {
    let output = cli(&[
        "admin",
        "recover-resource",
        "implementation",
        "assignment-1",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        envelope(&output)["error"]["message"],
        "th admin recover-resource: kind must be session, ship, worktree"
    );
}
