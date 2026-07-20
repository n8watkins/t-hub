//! Agent connection state machine + transport internals.
//!
//! [`ConnectionState`] is contract (the UI health area renders it); the child
//! process + reader/writer threads + correlation map live here.
//!
//! ## Threading model
//!
//! ```text
//!  ┌──────────────┐   ChildStdin (Mutex)   ┌──────────────────┐
//!  │  caller       │ ──write CoreFrame──→  │  child process   │
//!  │  (any thread) │                        │  t-hub-agent   │
//!  │               │ ←─AgentFrame lines─── │  --stdio         │
//!  └──────────────┘   reader thread        └──────────────────┘
//!
//!  reader thread:
//!    BufReader::lines → decode_agent → dispatch:
//!      Ready       → store journal_head_seq, set state Live/Replaying
//!      Response    → deliver AgentResponse to a per-request mpsc Sender
//!      Journal     → call AgentBridge::consume_journal_entry (cloned handle)
//!      ReplayComplete → set state Live
//!      Pong        → update last_pong_nonce (best-effort; no blocking)
//!      Error       → eprintln
//! ```
//!
//! ## Correlation map
//!
//! A `Mutex<HashMap<RequestId, Sender<AgentResponse>>>` in [`TransportHandles`]
//! maps an in-flight request's id to the per-request one-shot channel. The
//! reader thread pops the entry and sends; the caller blocks on the receiver
//! with a 10-second timeout.
//!
//! ## T_HUB_AGENT_BIN escape hatch
//!
//! When the env var `T_HUB_AGENT_BIN` is set, its value overrides argv[0]
//! from [`super::launch_argv`]. This lets developers and tests point at a
//! freshly built binary (e.g. `target/debug/t-hub-agent`) without altering
//! `launch_argv` or PATH.
//!
//! ## Channel / Priority note
//!
//! Every [`t_hub_protocol::CoreFrame`] written by [`write_frame`] carries the
//! `channel` and every [`t_hub_protocol::CoreToAgent::Request`] carries
//! `priority`. Both are fully serialized and echoed by the agent. A future
//! priority scheduler can replace the direct `Mutex<ChildStdin>` write path
//! with a bounded `BinaryHeap` feeding a dedicated writer thread — all the
//! metadata it needs is already in-flight. No reordering is done today: requests
//! are served strictly in the order `request()` acquires the stdin lock.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{atomic::AtomicU64, mpsc::Sender, Arc},
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use t_hub_protocol::{decode_agent, encode_core, AgentResponse, AgentToCore, CoreFrame};

// Re-export AgentBridge so the reader thread can call consume_journal_entry
// without a circular import (the thread captures a clone of AgentBridge).
use super::AgentBridge;

// ---------------------------------------------------------------------------
// ConnectionState (fixed contract — do NOT alter)
// ---------------------------------------------------------------------------

/// The lifecycle of the core↔agent connection (PLAN.md §A; surfaced in §H
/// health). Reconnect is a first-class state because a `wsl --shutdown` tears
/// the agent down and the core must re-handshake + replay the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum ConnectionState {
    /// No agent process / pipe.
    #[default]
    Disconnected,
    /// Child spawned; `Hello` sent, awaiting `Ready`.
    Handshaking,
    /// Handshake done; replaying the journal up to the agent's head seq.
    Replaying,
    /// Live and serving requests + streaming the spine.
    Live,
    /// Lost the connection; backing off before a reconnect attempt.
    Reconnecting,
    /// Permanently failed (e.g. agent binary missing); needs user action.
    Failed,
}

// ---------------------------------------------------------------------------
// Transport handles (private; stored in BridgeInner via TransportState)
// ---------------------------------------------------------------------------

/// Correlation map: maps a pending [`t_hub_protocol::RequestId`] to the
/// one-shot sender that will deliver the [`AgentResponse`] to the waiting
/// caller.
pub(crate) type CorrelationMap = Mutex<HashMap<u64, Sender<AgentResponse>>>;

/// The live transport: a locked child stdin for writing and the shared
/// correlation map for pairing requests with responses.
pub(crate) struct TransportHandles {
    /// Child stdin, locked so concurrent `request()` callers don't interleave
    /// frames. A future priority scheduler replaces this direct-write path with
    /// a priority queue feeding a dedicated writer thread.
    pub(crate) stdin: Mutex<ChildStdin>,
    /// In-flight request correlation: id → one-shot response sender.
    pub(crate) pending: Arc<CorrelationMap>,
    /// Monotonic request-id counter (starts at 1).
    pub(crate) next_id: Arc<AtomicU64>,
    /// The child handle, kept alive so the process isn't reaped.
    #[allow(dead_code)]
    pub(crate) child: Mutex<Child>,
}

// ---------------------------------------------------------------------------
// Frame writer
// ---------------------------------------------------------------------------

/// Write one [`CoreFrame`] as a single NDJSON line (`encode_core` + `\n`) and
/// flush immediately so the agent sees it without buffering.
///
/// **Channel / Priority note**: the `channel` field on the frame and the
/// `priority` field inside a `Request` body are fully serialized here. A future
/// priority scheduler can reorder outbound frames before they reach this
/// function; the wire format already carries all the metadata it needs.
pub(crate) fn write_frame(w: &mut impl Write, frame: &CoreFrame) -> std::io::Result<()> {
    let line =
        encode_core(frame).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

// ---------------------------------------------------------------------------
// Child spawning
// ---------------------------------------------------------------------------

/// Resolve the program to exec and the remaining arguments from `argv`.
///
/// If the env var `T_HUB_AGENT_BIN` is set, its value replaces `argv[0]`
/// (the bare `t-hub-agent` or `wsl.exe` that `launch_argv` returns). This
/// lets tests and developers point at a freshly built binary without touching
/// PATH or `launch_argv`:
///
/// ```sh
/// T_HUB_AGENT_BIN=/path/to/target/debug/t-hub-agent cargo test
/// ```
pub(crate) fn spawn_child(argv: Vec<String>) -> std::io::Result<Child> {
    if argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "launch_argv returned empty argv",
        ));
    }

    // T_HUB_AGENT_BIN: optional override for argv[0], documented above.
    let program = std::env::var("T_HUB_AGENT_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| argv[0].clone());

    let args = &argv[1..];

    let mut cmd = Command::new(&program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()); // agent diagnostics go to core's stderr

    // On Windows `program` is `wsl.exe` (from `launch_argv`); without
    // CREATE_NO_WINDOW that raw spawn flashes a console (CMD) window. Gate behind
    // cfg(windows) so the unix dev build (which spawns t-hub-agent directly) is
    // unaffected.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    cmd.spawn()
}

// ---------------------------------------------------------------------------
// Reader thread
// ---------------------------------------------------------------------------

/// Spawn the stdout-reader thread for the given child stdout. The thread:
///
/// 1. Reads lines from `child_stdout` via a `BufReader`.
/// 2. `decode_agent`s each line.
/// 3. Dispatches each [`AgentToCore`] variant:
///    - `Ready`          → stores `journal_head_seq`; the caller sets state.
///    - `Response`       → pops the sender from `pending` and delivers the body.
///    - `Journal`        → calls `bridge.consume_journal_entry(&entry)`.
///    - `ReplayComplete` → notifies the ready_tx channel so connect() can set Live.
///    - `Pong`           → no-op (RTT measurement is future work).
///    - `Error`          → `eprintln!`.
///    - `Unknown`        → ignored (forward-compat).
///
/// The thread exits when the agent's stdout is closed (EOF) or on any
/// unrecoverable read error.
pub(crate) fn spawn_reader(
    child_stdout: std::process::ChildStdout,
    pending: Arc<CorrelationMap>,
    bridge: AgentBridge,
    // Sent once when Ready arrives: carries the agent's journal_head_seq.
    ready_tx: Sender<u64>,
    // Sent once when ReplayComplete arrives.
    replay_done_tx: Sender<u64>,
) {
    std::thread::Builder::new()
        .name("agent-reader".into())
        .spawn(move || {
            let reader = BufReader::new(child_stdout);
            for line_result in reader.lines() {
                let line = match line_result {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("agent-bridge: reader I/O error: {e}");
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let frame = match decode_agent(&line) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("agent-bridge: skipping malformed frame: {e} (line={line:?})");
                        continue;
                    }
                };

                match frame.msg {
                    AgentToCore::Ready(ready) => {
                        eprintln!(
                            "agent-bridge: agent ready (version={}, journal_head={})",
                            ready.agent_version, ready.journal_head_seq
                        );
                        // Signal connect() with the journal head seq so it can
                        // decide whether to request a replay.
                        let _ = ready_tx.send(ready.journal_head_seq);
                    }

                    AgentToCore::Response { id, body } => {
                        let sender = {
                            let mut map = pending.lock();
                            map.remove(&id)
                        };
                        match sender {
                            Some(tx) => {
                                // Ignore send error: the caller may have timed out.
                                let _ = tx.send(body);
                            }
                            None => {
                                eprintln!(
                                    "agent-bridge: response for unknown/timed-out request id={id}"
                                );
                            }
                        }
                    }

                    AgentToCore::Journal { seq, entry } => {
                        bridge.consume_journal_entry(&entry);
                        let _ = seq; // cursor advancement is done inside consume_journal_entry
                    }

                    AgentToCore::ReplayComplete { last_seq } => {
                        eprintln!("agent-bridge: replay complete (last_seq={last_seq})");
                        let _ = replay_done_tx.send(last_seq);
                    }

                    AgentToCore::Pong { nonce: _ } => {
                        // RTT measurement / liveness tracking is future work.
                        // The nonce is available here when needed.
                    }

                    AgentToCore::Error { message } => {
                        eprintln!("agent-bridge: agent error: {message}");
                    }

                    AgentToCore::Unknown => {
                        // Forward-compat: newer agent speaking to older core.
                    }
                }
            }
            eprintln!("agent-bridge: reader thread exiting (agent stdout closed)");
        })
        .expect("failed to spawn agent-reader thread");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod transport_tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::agent::{AgentBridge, TestEnvVar, AGENT_TEST_ENV_LOCK};
    // `ConnectionState` is defined in this module (the parent of these tests).
    use super::ConnectionState;

    /// Poll `cond` every 10ms until it returns true or `deadline` elapses; returns
    /// whether the condition was met. Replaces bare "give the reader a moment"
    /// sleeps with a bounded wait on the actual observable state, so the tests
    /// don't depend on a fixed-time guess (fast machines don't waste the full nap,
    /// loaded machines aren't cut off early — they wait up to the deadline).
    fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        loop {
            if cond() {
                return true;
            }
            if start.elapsed() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Integration test: spin up the real t-hub-agent binary, send Hello,
    /// list sessions, and fetch metrics. Skips gracefully when the binary is
    /// absent so CI never fails spuriously without a built agent.
    #[test]
    fn live_round_trip_with_real_agent() {
        // Locate the agent binary. We look for T_HUB_AGENT_BIN first (already
        // set by this test's environment if the caller ran build first), then
        // fall back to the workspace-relative debug build.
        let bin_path: PathBuf = {
            // Walk up from the manifest dir to find the workspace target dir.
            // __FILE__ is inside src-tauri/src/agent/; target/ is a sibling of
            // src-tauri/.
            let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest.join("target/debug/t-hub-agent")
        };

        if !bin_path.exists() {
            eprintln!(
                "transport_tests::live_round_trip_with_real_agent: \
                 binary not found at {bin_path:?} — skipping (run \
                 `cargo build -p t-hub-agent` first or set T_HUB_AGENT_BIN)"
            );
            return;
        }

        // Point the escape hatch at the known-good debug binary only while the
        // child is spawned. Restore it immediately so parallel tests never
        // inherit process-global test configuration.
        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &bin_path);
        let bridge = AgentBridge::new();
        bridge.connect("ignored").expect("connect() must succeed");
        drop(agent_bin_env);
        drop(env_lock);

        // `connect()` blocks until the handshake + replay finish and the state is
        // Live, so this is normally already satisfied — but assert it via a bounded
        // wait on the observable state instead of a fixed "give the reader a moment"
        // nap, so the test never races a slow handshake and never sleeps blindly.
        assert!(
            wait_until(Duration::from_secs(5), || bridge.state()
                == ConnectionState::Live),
            "bridge should reach Live after connect(); got {:?}",
            bridge.state()
        );

        // --- ListSessions ---
        let resp = bridge
            .request(t_hub_protocol::AgentRequest::ListSessions)
            .expect("ListSessions must return a response");
        match resp {
            t_hub_protocol::AgentResponse::Sessions { names } => {
                // The agent may or may not have active tmux sessions; the
                // important thing is that the response is the right variant.
                eprintln!("live_round_trip: sessions={names:?}");
            }
            other => panic!("expected Sessions, got {other:?}"),
        }

        // --- TerminalSnapshot ---
        let resp = bridge
            .request(t_hub_protocol::AgentRequest::TerminalSnapshot)
            .expect("TerminalSnapshot must return a response");
        match resp {
            t_hub_protocol::AgentResponse::TerminalSnapshot(snapshot) => {
                eprintln!(
                    "live_round_trip: terminal_snapshot sessions={} panes={}",
                    snapshot.sessions.len(),
                    snapshot.panes.len()
                );
            }
            other => panic!("expected TerminalSnapshot, got {other:?}"),
        }

        // --- Metrics ---
        let resp = bridge
            .request(t_hub_protocol::AgentRequest::Metrics)
            .expect("Metrics must return a response");
        match resp {
            t_hub_protocol::AgentResponse::Metrics(m) => {
                eprintln!(
                    "live_round_trip: metrics cpu_count={} mem_total_kib={}",
                    m.cpu_count, m.mem_total_kib
                );
                assert!(m.cpu_count > 0, "cpu_count should be > 0");
                assert!(m.mem_total_kib > 0, "mem_total_kib should be > 0");
            }
            other => panic!("expected Metrics, got {other:?}"),
        }

        eprintln!("live_round_trip: all assertions passed");
    }

    /// **The live-emit demo** (deliverable item 4): drive the REAL hook→journal→
    /// agent→core→emit spine end-to-end and prove a live `supervision://tree`
    /// emit emerges from a `SessionStart → … → Stop` hook sequence.
    ///
    /// Hermetic: a private `$HOME` so the journal lives under a temp dir, shared
    /// by both the `--hook` ingest processes and the `--stdio` agent the bridge
    /// spawns. Skips when the binary isn't built (so CI never fails spuriously).
    ///
    /// Exercises BOTH emit paths:
    ///   - **replay**: hooks fired *before* connect → bridge replays the journal
    ///     on handshake → each replayed entry emits.
    ///   - **live tail**: a hook fired *after* connect → agent's tail thread
    ///     streams it → bridge consumes it → emits.
    #[test]
    fn live_emit_demo_hook_sequence_to_supervision_tree() {
        use parking_lot::Mutex as PMutex;
        use std::process::Command;
        use std::sync::Arc;

        let bin_path: PathBuf = {
            let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest.join("target/debug/t-hub-agent")
        };
        if !bin_path.exists() {
            eprintln!(
                "live_emit_demo: binary not found at {bin_path:?} — skipping \
                 (run `cargo build -p t-hub-agent` first)"
            );
            return;
        }

        // Hermetic private HOME → journal at $HOME/.t-hub/journal, shared by the
        // hook processes and the stdio agent (both honor $HOME).
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let home = std::env::temp_dir().join(format!("t-hub-live-emit-demo-{ts}"));
        std::fs::create_dir_all(&home).unwrap();

        // The exact production hook entrypoint: `t-hub-agent --hook <EVENT>`,
        // feeding the hook's JSON stdin (session_id + subagent base fields).
        let fire_hook = |event: &str, stdin_json: &str| {
            use std::io::Write;
            let mut child = Command::new(&bin_path)
                .arg("--hook")
                .arg(event)
                .env("HOME", &home)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn --hook");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin_json.as_bytes())
                .unwrap();
            let status = child.wait().expect("hook wait");
            assert!(
                status.success(),
                "hook {event} must exit 0 (never fail Claude)"
            );
        };

        // --- Phase 1: fire a hook sequence BEFORE connect (replay path) ---
        let sid = "demo-session-1";
        fire_hook(
            "SessionStart",
            &format!(r#"{{"session_id":"{sid}","cwd":"/w"}}"#),
        );
        fire_hook("UserPromptSubmit", &format!(r#"{{"session_id":"{sid}"}}"#));
        fire_hook(
            "SubagentStart",
            &format!(
                r#"{{"session_id":"{sid}","agent_id":"sub-a","agent_type":"general-purpose"}}"#
            ),
        );
        // Main agent Stop while the subagent is still running → WaitingOnSubagents.
        fire_hook("Stop", &format!(r#"{{"session_id":"{sid}"}}"#));

        // --- Connect the core bridge to a real --stdio agent (same HOME) ---
        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &bin_path);
        let home_env = TestEnvVar::set("HOME", &home);

        // Recording emitter to capture the live UI events.
        #[derive(Default, Clone)]
        struct Rec {
            events: Arc<PMutex<Vec<(String, serde_json::Value)>>>,
        }
        impl crate::agent::EventEmitter for Rec {
            fn emit_json(&self, channel: &str, payload: &serde_json::Value) {
                self.events
                    .lock()
                    .push((channel.to_string(), payload.clone()));
            }
        }
        let rec = Rec::default();

        let bridge = AgentBridge::new();
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.connect("ignored").expect("connect must succeed");
        drop(home_env);
        drop(agent_bin_env);
        drop(env_lock);

        // `connect()` returns once replay is complete, but the replayed entries are
        // emitted on the reader thread, so the emits may land just after connect()
        // returns. Wait (bounded) for the observable condition this phase asserts —
        // the replayed Stop's supervision://tree emit with WaitingOnSubagents — instead
        // of a fixed "give the reader a moment" sleep.
        let waiting_tree = |rec: &Rec| {
            rec.events.lock().iter().any(|(ch, p)| {
                ch == super::super::emit::EVT_SUPERVISION
                    && p["sessionId"] == sid
                    && p["status"] == "waitingOnSubagents"
            })
        };
        assert!(
            wait_until(Duration::from_secs(5), || waiting_tree(&rec)),
            "replay must emit a supervision://tree with waitingOnSubagents; got {:?}",
            *rec.events.lock()
        );

        // Also: agent://journal must have been emitted for the replayed entries.
        let journal_emits = rec
            .events
            .lock()
            .iter()
            .filter(|(ch, _)| ch == super::super::emit::EVT_JOURNAL)
            .count();
        assert!(
            journal_emits >= 4,
            "expected >=4 journal emits, got {journal_emits}"
        );

        eprintln!("live_emit_demo: replay path emitted waitingOnSubagents ✓");

        // --- Phase 2: fire a LIVE hook AFTER connect (tail-streaming path) ---
        rec.events.lock().clear();
        // The subagent finishes → with main already stopped, the orchestrator
        // transitions WaitingOnSubagents → Completed.
        fire_hook(
            "SubagentStop",
            &format!(r#"{{"session_id":"{sid}","agent_id":"sub-a"}}"#),
        );

        // Wait for the agent's ~200ms tail poll + stream + core consume + emit.
        // Bounded poll on the observable session-status emit (returns as soon as it
        // lands, up to a 3s ceiling) — no fixed end-to-end sleep.
        let completed = wait_until(Duration::from_secs(3), || {
            rec.events.lock().iter().any(|(ch, p)| {
                ch == super::super::emit::EVT_SESSION_STATUS
                    && p["sessionId"] == sid
                    && p["status"] == "completed"
            })
        });
        assert!(
            completed,
            "live tail path must stream SubagentStop and emit completed; got {:?}",
            *rec.events.lock()
        );
        eprintln!("live_emit_demo: live tail path emitted completed ✓");

        bridge.disconnect();
        std::fs::remove_dir_all(&home).ok();
    }
}
