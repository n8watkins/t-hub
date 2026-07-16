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
