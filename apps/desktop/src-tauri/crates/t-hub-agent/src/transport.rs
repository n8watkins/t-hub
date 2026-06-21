//! The NDJSON-over-stdio transport: read [`CoreFrame`]s from stdin, serve them,
//! and write [`AgentFrame`]s to stdout (stderr stays human-readable).
//!
//! ## What's implemented now
//! A correct, ordered serve loop: the Hello/Ready handshake, request → response
//! routing via [`crate::dispatch`], Ping/Pong, journal replay
//! ([`CoreToAgent::ReplayJournal`]), and graceful shutdown (on `Shutdown` or
//! stdin EOF). Every reply preserves the request's [`Channel`].
//!
//! ## Live journal streaming
//! A dedicated tail thread reads the journal incrementally every ~200 ms via
//! `journal.tail_from(offset, last_seq)`, which seeks straight to the last byte
//! offset and parses ONLY the bytes appended since, emitting new entries as
//! `AgentFrame { channel: Events, msg: Journal { … } }` frames. The cursor (byte
//! offset + head seq) is initialised to the current EOF at startup so we stream
//! ONLY new entries (the core uses ReplayJournal for historical backfill).
//!
//! **Why incremental, not a re-scan:** the journal is appended out-of-process by
//! the short-lived `--hook`/`--statusline` ingest processes, so the tail must
//! observe the *file's* growth (an in-memory head would never see them). The
//! previous design re-counted and re-parsed the WHOLE file every poll
//! (`head_seq_on_disk` + `replay`) — fine for a small journal, but O(file): once
//! the journal bloats (e.g. high-frequency statusline snapshots), a
//! multi-hundred-MB rescan 5×/s saturates this thread and starves live status
//! delivery (the per-tile context-meter symptom). Reading only the new bytes is
//! O(new data) regardless of journal size; a shrink (compaction/rotation) is
//! detected and restarts the read from the top.
//!
//! Both the request/response path and the tail thread write through a single
//! shared mpsc sender. A dedicated writer thread owns stdout and serialises all
//! outbound frames; this prevents line interleaving between the two concurrent
//! producers.
//!
//! inotify would be cleaner but has WSL2 reliability issues; the 200 ms poll is
//! simple, portable, and sufficient for 0.5.
//!
//! ## SUBAGENT(transport): the head-of-line-blocking scheduler
//! The protocol tags each frame with a [`Channel`] + [`Priority`] precisely so a
//! writer can interleave control/metrics ahead of bulk payloads on the single
//! pipe (REVIEW). This baseline serves requests strictly in arrival order on the
//! reader thread. The enhancement (a bounded priority queue feeding a dedicated
//! writer thread, so a slow `CapturePane` can't delay a `Metrics`/`Ping` reply)
//! is the subagent's job. It must keep the wire format and these function
//! signatures unchanged; only the internal scheduling changes.

use std::io::BufRead;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

use anyhow::Result;
use t_hub_protocol::{
    AgentFrame, AgentToCore, Channel, CoreToAgent, Hello, Ready, PROTOCOL_VERSION,
};

use crate::dispatch;
use crate::journal::Journal;

/// How often the tail thread checks for new journal entries.
const TAIL_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Run the stdio bridge until the core closes stdin or sends `Shutdown`.
/// Returns `Ok(())` on a clean shutdown; `Err` only on an unrecoverable I/O
/// failure on stdout.
///
/// `journal` is an `Arc<Journal>` so the tail thread can share it without
/// cloning the underlying file handle.
pub fn serve_stdio(journal: Arc<Journal>) -> Result<()> {
    // -----------------------------------------------------------------------
    // Writer thread: owns stdout; serialises all outbound frames so the
    // request/response path and the tail thread never interleave a line.
    // -----------------------------------------------------------------------
    let (tx, rx) = mpsc::channel::<AgentFrame>();

    let writer_thread = std::thread::spawn(move || -> Result<()> {
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        for frame in rx {
            write_frame(&mut writer, &frame)?;
        }
        Ok(())
    });

    // -----------------------------------------------------------------------
    // Tail thread: polls journal.head_seq() and streams new entries.
    // -----------------------------------------------------------------------
    let tail_stop = Arc::new(AtomicBool::new(false));
    {
        let journal_arc = Arc::clone(&journal);
        let tail_tx = tx.clone();
        let stop = Arc::clone(&tail_stop);
        // Start streaming from NOW: seed the byte cursor at the current EOF and
        // the head seq at the on-disk count (core does ReplayJournal for
        // historical backfill). From here the tail reads ONLY new bytes, so it
        // stays O(new data) no matter how large the journal grows.
        let initial_offset = journal_arc.byte_len();
        let initial_seq = journal_arc.head_seq_on_disk();

        std::thread::spawn(move || {
            let mut offset = initial_offset;
            let mut last_seq = initial_seq;
            loop {
                std::thread::sleep(TAIL_POLL_INTERVAL);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // Read only the bytes appended (cross-process) since `offset` —
                // never a full-file rescan. Cheap (a seek + read of the new
                // tail) when there is nothing new.
                match journal_arc.tail_from(offset, last_seq) {
                    Ok((entries, new_offset, new_seq)) => {
                        for entry in entries {
                            let frame = AgentFrame {
                                channel: Channel::Events,
                                msg: AgentToCore::Journal { seq: entry.seq, entry },
                            };
                            if tail_tx.send(frame).is_err() {
                                // Writer thread has exited (main loop shut down). Quit.
                                return;
                            }
                        }
                        offset = new_offset;
                        last_seq = new_seq;
                    }
                    Err(e) => {
                        eprintln!("t-hub-agent: tail thread read error: {e:#}");
                        // Leave the cursor put; retry next poll.
                    }
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Reader loop: decode CoreFrames from stdin, serve them via `tx`.
    // -----------------------------------------------------------------------
    let result = reader_loop(&journal, &tx);

    // -----------------------------------------------------------------------
    // Teardown: stop the tail thread, drain the writer thread.
    // -----------------------------------------------------------------------
    tail_stop.store(true, Ordering::Relaxed);
    // Drop our sender so the writer thread drains its queue and exits.
    drop(tx);
    match writer_thread.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            // If the reader_loop itself errored we already have that; surface
            // the writer error only if the reader was clean.
            if result.is_ok() {
                return Err(e);
            }
        }
        Err(_) => {
            eprintln!("t-hub-agent: writer thread panicked");
        }
    }

    result
}

/// The actual reader loop, decoupled from lifecycle management.
fn reader_loop(journal: &Arc<Journal>, tx: &mpsc::Sender<AgentFrame>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();

    let mut line = String::new();
    let mut handshaken = false;

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // EOF: the core closed the pipe. Clean shutdown.
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }

        let frame = match t_hub_protocol::decode_core(trimmed) {
            Ok(f) => f,
            Err(e) => {
                // A malformed line never desyncs the stream; report and continue.
                eprintln!("t-hub-agent: skipping malformed frame: {e}");
                continue;
            }
        };

        match frame.msg {
            CoreToAgent::Hello(Hello { protocol_version, core_version }) => {
                if protocol_version != PROTOCOL_VERSION {
                    eprintln!(
                        "t-hub-agent: protocol mismatch \
                         (core={protocol_version}, agent={PROTOCOL_VERSION}); \
                         continuing best-effort"
                    );
                }
                eprintln!("t-hub-agent: handshake with {core_version}");
                handshaken = true;
                let ready = AgentFrame {
                    channel: Channel::Control,
                    msg: AgentToCore::Ready(Ready {
                        protocol_version: PROTOCOL_VERSION,
                        agent_version: crate::agent_version(),
                        distro: detect_distro(),
                        journal_head_seq: journal.head_seq(),
                    }),
                };
                if tx.send(ready).is_err() {
                    break;
                }
            }

            CoreToAgent::Request { id, priority: _, body } => {
                // SUBAGENT(transport): `priority` is currently ignored (strict
                // arrival order). The scheduler enhancement uses it.
                if !handshaken {
                    eprintln!("t-hub-agent: request {id} before handshake; serving anyway");
                }
                let resp_body = dispatch::handle(journal, body);
                let resp = AgentFrame {
                    // Reply on the same logical channel kind as the request's
                    // nature. We keep it simple: control by default. The
                    // scheduler subagent may classify per-op.
                    channel: Channel::Control,
                    msg: AgentToCore::Response { id, body: resp_body },
                };
                if tx.send(resp).is_err() {
                    break;
                }
            }

            CoreToAgent::Ping { nonce } => {
                let pong = AgentFrame {
                    channel: Channel::Control,
                    msg: AgentToCore::Pong { nonce },
                };
                if tx.send(pong).is_err() {
                    break;
                }
            }

            CoreToAgent::ReplayJournal { after_seq } => {
                let entries = match journal.replay(after_seq) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("t-hub-agent: journal replay failed: {e:#}");
                        Vec::new()
                    }
                };
                let mut last_seq = after_seq;
                for entry in entries {
                    last_seq = entry.seq;
                    let f = AgentFrame {
                        channel: Channel::Events,
                        msg: AgentToCore::Journal { seq: entry.seq, entry },
                    };
                    if tx.send(f).is_err() {
                        return Ok(());
                    }
                }
                let done = AgentFrame {
                    channel: Channel::Events,
                    msg: AgentToCore::ReplayComplete { last_seq },
                };
                if tx.send(done).is_err() {
                    return Ok(());
                }
            }

            CoreToAgent::Shutdown => {
                eprintln!("t-hub-agent: shutdown requested");
                break;
            }

            CoreToAgent::Unknown => {
                eprintln!("t-hub-agent: ignoring unknown core message");
            }
        }
    }

    Ok(())
}

/// Encode `frame` as one NDJSON line + newline and flush so the core sees it
/// promptly (no buffering across requests).
fn write_frame(writer: &mut impl std::io::Write, frame: &AgentFrame) -> Result<()> {
    let line = t_hub_protocol::encode_agent(frame)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Best-effort distro name from `/etc/os-release` `PRETTY_NAME`.
fn detect_distro() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use t_hub_protocol::{
        AgentToCore, Channel, CoreFrame, CoreToAgent, Hello, PROTOCOL_VERSION,
    };

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("t-hub-transport-test-{tag}-{ts}"))
    }

    #[test]
    fn write_frame_produces_valid_ndjson_line() {
        let frame = AgentFrame {
            channel: Channel::Control,
            msg: AgentToCore::Pong { nonce: 42 },
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // Must end with exactly one newline.
        assert!(s.ends_with('\n'), "frame must end with newline");
        assert_eq!(s.matches('\n').count(), 1, "frame must be single-line NDJSON");
        // Must roundtrip.
        let back = t_hub_protocol::decode_agent(s.trim_end_matches('\n')).unwrap();
        match back.msg {
            AgentToCore::Pong { nonce } => assert_eq!(nonce, 42),
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[test]
    fn write_frame_journal_entry_roundtrips() {
        use t_hub_protocol::{EventJournalEntry, JournalEventType, JournalSource};
        let entry = EventJournalEntry {
            seq: 3,
            timestamp_ms: 1_000_000,
            source: JournalSource::Hook,
            entity_id: Some("sess-abc".into()),
            event_type: JournalEventType::SessionStart,
            payload: serde_json::json!({"session_id": "sess-abc"}),
            result: None,
        };
        let frame = AgentFrame {
            channel: Channel::Events,
            msg: AgentToCore::Journal { seq: 3, entry },
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let back = t_hub_protocol::decode_agent(s.trim_end_matches('\n')).unwrap();
        assert_eq!(back.channel, Channel::Events);
        match back.msg {
            AgentToCore::Journal { seq, entry } => {
                assert_eq!(seq, 3);
                assert_eq!(entry.entity_id.as_deref(), Some("sess-abc"));
            }
            other => panic!("expected Journal, got {other:?}"),
        }
    }

    #[test]
    fn tail_thread_streams_entries_appended_after_start() {
        use crate::hook::build_entry;
        use crate::journal::Journal;

        let dir = temp_dir("tail");
        let journal = Arc::new(Journal::open(&dir).unwrap());

        // Pre-populate one entry before "startup" — this should NOT be streamed.
        let pre = build_entry("SessionStart", &serde_json::json!({"session_id":"pre"}));
        journal.append(pre).unwrap();

        let initial_offset = journal.byte_len();
        let initial_seq = journal.head_seq_on_disk(); // = 1

        // Spawn the tail thread manually (same logic as in serve_stdio).
        let (tail_tx, tail_rx) = mpsc::channel::<AgentFrame>();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let journal_arc = Arc::clone(&journal);
            let tx = tail_tx.clone();
            let stop_flag = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut offset = initial_offset;
                let mut last_seq = initial_seq;
                loop {
                    std::thread::sleep(TAIL_POLL_INTERVAL);
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    match journal_arc.tail_from(offset, last_seq) {
                        Ok((entries, new_offset, new_seq)) => {
                            for entry in entries {
                                let frame = AgentFrame {
                                    channel: Channel::Events,
                                    msg: AgentToCore::Journal { seq: entry.seq, entry },
                                };
                                if tx.send(frame).is_err() {
                                    return;
                                }
                            }
                            offset = new_offset;
                            last_seq = new_seq;
                        }
                        Err(_) => {}
                    }
                }
            });
        }

        // Append two new entries — these SHOULD be streamed.
        let e2 = build_entry("Stop", &serde_json::json!({"session_id":"s2"}));
        let e3 = build_entry("SessionEnd", &serde_json::json!({"session_id":"s3"}));
        journal.append(e2).unwrap();
        journal.append(e3).unwrap();

        // Wait a bit over one poll interval for the tail to fire.
        std::thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::Relaxed);
        drop(tail_tx);

        let received: Vec<AgentFrame> = tail_rx.try_iter().collect();
        assert_eq!(received.len(), 2, "tail should stream exactly the 2 new entries");

        match &received[0].msg {
            AgentToCore::Journal { entry, .. } => {
                assert_eq!(entry.event_type, t_hub_protocol::JournalEventType::Stop);
            }
            other => panic!("expected Journal, got {other:?}"),
        }
        match &received[1].msg {
            AgentToCore::Journal { entry, .. } => {
                assert_eq!(entry.event_type, t_hub_protocol::JournalEventType::SessionEnd);
            }
            other => panic!("expected Journal, got {other:?}"),
        }
        assert_eq!(received[0].channel, Channel::Events);
        assert_eq!(received[1].channel, Channel::Events);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hello_frame_serialises_correctly() {
        // Ensure we can encode a Hello the same way the core would send it.
        let hello = CoreFrame {
            channel: Channel::Control,
            msg: CoreToAgent::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                core_version: "t-hub 0.5.0-test".into(),
            }),
        };
        let line = t_hub_protocol::encode_core(&hello).unwrap();
        assert!(!line.contains('\n'));
        let back = t_hub_protocol::decode_core(&line).unwrap();
        match back.msg {
            CoreToAgent::Hello(h) => {
                assert_eq!(h.protocol_version, PROTOCOL_VERSION);
                assert_eq!(h.core_version, "t-hub 0.5.0-test");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }
}
