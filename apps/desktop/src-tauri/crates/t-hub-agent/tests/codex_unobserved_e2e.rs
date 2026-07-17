#[cfg(unix)]
mod unix {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use t_hub_protocol::{EventJournalEntry, JournalEventType, JournalSource};

    #[test]
    fn real_agent_records_bounded_credential_safe_codex_unobserved_marker() {
        let fixture = std::env::temp_dir().join(format!(
            "t-hub-agent-codex-unobserved-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin_dir = fixture.join("bin");
        let journal_dir = fixture.join("journal");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let fake_tmux = bin_dir.join("tmux");
        let mut script = std::fs::File::create(&fake_tmux).unwrap();
        writeln!(
            script,
            "#!/bin/sh\nprintf '%s\\n' 'th_marker\t$17\t123456\t@9\t%42\t4242'"
        )
        .unwrap();
        drop(script);
        std::fs::set_permissions(&fake_tmux, std::fs::Permissions::from_mode(0o700)).unwrap();

        let output = Command::new(env!("CARGO_BIN_EXE_t-hub-agent"))
            .args(["--codex-unobserved", "--journal-dir"])
            .arg(&journal_dir)
            .env("PATH", &bin_dir)
            .env("TMUX_PANE", "%42")
            .env("T_HUB_CONTROL_TOKEN", "must-not-be-journaled")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "real t-hub-agent marker failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());

        let journal = std::fs::read_to_string(journal_dir.join("events.ndjson")).unwrap();
        let entries = journal
            .lines()
            .map(|line| serde_json::from_str::<EventJournalEntry>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.source, JournalSource::Agent);
        assert_eq!(entry.event_type, JournalEventType::CoreAction);
        assert_eq!(entry.entity_id.as_deref(), Some("codex-unobserved:$17:%42"));
        assert_eq!(entry.payload["schema"], "t-hub.codex.unobserved.v1");
        assert_eq!(entry.payload["provider"], "codex");
        assert_eq!(entry.payload["runtime_health"], "degraded");
        assert_eq!(entry.payload["agent_status"], "unknown");
        assert_eq!(entry.payload["transport"], "unavailable");
        assert_eq!(entry.payload["tmux_session"], "th_marker");
        assert_eq!(entry.payload["telemetry"]["runtime_health"], "degraded");
        assert_eq!(entry.payload["telemetry"]["transport"], "unavailable");
        assert_eq!(entry.payload["tmux"]["session_name"], "th_marker");
        assert_eq!(entry.payload["tmux"]["session_id"], "$17");
        assert_eq!(entry.payload["tmux"]["session_created"], 123456);
        assert_eq!(entry.payload["tmux"]["window_id"], "@9");
        assert_eq!(entry.payload["tmux"]["pane_id"], "%42");
        assert_eq!(entry.payload["tmux"]["pane_pid"], 4242);
        assert!(!journal.contains("must-not-be-journaled"));

        std::fs::remove_dir_all(fixture).ok();
    }
}
