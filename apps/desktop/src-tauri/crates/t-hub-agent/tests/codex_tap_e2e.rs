use std::io::Write;
use std::process::{Command, Stdio};

use t_hub_protocol::{EventJournalEntry, JournalEventType};

fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "t-hub-codex-tap-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn codex_tap_process_persists_a_sanitized_permission_lifecycle() {
    let journal_dir = temp_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_t-hub-agent"))
        .args(["--codex-tap", "--journal-dir"])
        .arg(&journal_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(include_bytes!(
            "fixtures/codex-0.144.4-app-server-permission-lifecycle.jsonl"
        ))
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "tap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "machine stdout must remain clean");

    let journal = std::fs::read_to_string(journal_dir.join("events.ndjson")).unwrap();
    let entries: Vec<EventJournalEntry> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[1].event_type, JournalEventType::PermissionRequest);
    assert_eq!(
        entries[1].payload["permission_request"]["provider_request_id"],
        "approval-sanitized-1"
    );
    assert!(!journal.contains("credential-bearing-command"));
    assert!(!journal.contains("sanitized reason"));

    std::fs::remove_dir_all(journal_dir).ok();
}

#[test]
fn current_app_server_thread_started_persists_session_start() {
    let journal_dir = temp_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_t-hub-agent"))
        .args(["--codex-tap", "--journal-dir"])
        .arg(&journal_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(include_bytes!(
            "fixtures/codex-0.144.4-app-server-thread-started.jsonl"
        ))
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "tap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let journal = std::fs::read_to_string(journal_dir.join("events.ndjson")).unwrap();
    let entries: Vec<EventJournalEntry> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 1, "current thread start must not disappear");
    assert_eq!(entries[0].event_type, JournalEventType::SessionStart);
    assert_eq!(
        entries[0].entity_id.as_deref(),
        Some("00000000-0000-7000-8000-000000000144")
    );

    std::fs::remove_dir_all(journal_dir).ok();
}

#[test]
fn malformed_and_oversized_approvals_are_fail_closed_in_the_real_process() {
    let journal_dir = temp_dir();
    let shared_prefix = "p".repeat(512);
    let oversized_id = format!("{shared_prefix}x");
    let input = format!(
        "{{\"method\":\"thread/started\",\"params\":{{\"thread\":{{\"id\":\"thread-1\"}}}}}}\n\
         {{\"method\":\"turn/started\",\"params\":{{\"threadId\":\"thread-1\",\"turn\":{{\"id\":\"turn-1\"}}}}}}\n\
         {{\"method\":\"item/commandExecution/requestApproval\",\"id\":\"{shared_prefix}\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"itemId\":\"item-1\"}}}}\n\
         {{\"method\":\"item/commandExecution/requestApproval\",\"id\":\"{oversized_id}\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"command\":\"credential-bearing-command\"}}}}\n\
         {{\"method\":\"item/fileChange/requestApproval\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"reason\":\"credential-bearing-reason\"}}}}\n\
         {{\"method\":\"serverRequest/resolved\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"requestId\":\"{oversized_id}\"}}}}\n"
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_t-hub-agent"))
        .args(["--codex-tap", "--journal-dir"])
        .arg(&journal_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "tap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "machine stdout must remain clean");

    let journal = std::fs::read_to_string(journal_dir.join("events.ndjson")).unwrap();
    let entries: Vec<EventJournalEntry> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(entries.iter().any(|entry| {
        entry.payload["permission_request"]["provider_request_id"] == shared_prefix
    }));
    assert!(entries.iter().any(|entry| {
        entry.event_type == JournalEventType::PermissionRequest
            && entry.payload["telemetry"]["runtime_health"] == "degraded"
            && entry.payload.get("permission_request").is_none()
    }));
    assert!(!entries
        .iter()
        .any(|entry| entry.payload["lifecycle"] == "permission_resolved"));
    assert!(!journal.contains(&oversized_id));
    assert!(!journal.contains("credential-bearing-command"));
    assert!(!journal.contains("credential-bearing-reason"));

    std::fs::remove_dir_all(journal_dir).ok();
}

#[test]
fn unobserved_tui_process_records_degraded_health_from_its_tmux_tile() {
    let journal_dir = temp_dir();
    let socket = format!("t-hub-codex-degraded-{}", std::process::id());
    let session = "codexdegraded1";
    let status = Command::new("tmux")
        .args(["-L", &socket, "new-session", "-d", "-s", session])
        .arg(env!("CARGO_BIN_EXE_t-hub-agent"))
        .args(["--codex-unobserved", "--journal-dir"])
        .arg(&journal_dir)
        .status()
        .unwrap();
    assert!(status.success(), "isolated tmux launch failed");

    let path = journal_dir.join("events.ndjson");
    for _ in 0..100 {
        if path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .status();

    let journal = std::fs::read_to_string(path).unwrap();
    let entries: Vec<EventJournalEntry> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event_type, JournalEventType::AgentCommand);
    assert_eq!(entries[0].payload["tmux_session"], session);
    assert_eq!(
        entries[0].payload["telemetry"]["runtime_health"],
        "degraded"
    );
    assert_eq!(entries[0].payload["telemetry"]["transport"], "unavailable");
    assert_ne!(entries[0].event_type, JournalEventType::SessionStart);

    std::fs::remove_dir_all(journal_dir).ok();
}
