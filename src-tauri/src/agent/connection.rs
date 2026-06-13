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
//!  │  (any thread) │                        │  termhub-agent   │
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
//! ## TERMHUB_AGENT_BIN escape hatch
//!
//! When the env var `TERMHUB_AGENT_BIN` is set, its value overrides argv[0]
//! from [`super::launch_argv`]. This lets developers and tests point at a
//! freshly built binary (e.g. `target/debug/termhub-agent`) without altering
//! `launch_argv` or PATH.
//!
//! ## Channel / Priority note
//!
//! Every [`termhub_protocol::CoreFrame`] written by [`write_frame`] carries the
//! `channel` and every [`termhub_protocol::CoreToAgent::Request`] carries
//! `priority`. Both are fully serialized and echoed by the agent. A future
//! priority scheduler can replace the direct `Mutex<ChildStdin>` write path
//! with a bounded `BinaryHeap` feeding a dedicated writer thread — all the
//! metadata it needs is already in-flight. No reordering is done today: requests
//! are served strictly in the order `request()` acquires the stdin lock.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        mpsc::Sender,
        atomic::AtomicU64,
        Arc,
    },
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use termhub_protocol::{
    AgentResponse, AgentToCore, CoreFrame,
    encode_core, decode_agent,
};

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
pub enum ConnectionState {
    /// No agent process / pipe.
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

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Disconnected
    }
}

// ---------------------------------------------------------------------------
// Transport handles (private; stored in BridgeInner via TransportState)
// ---------------------------------------------------------------------------

/// Correlation map: maps a pending [`termhub_protocol::RequestId`] to the
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
    let line = encode_core(frame).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

// ---------------------------------------------------------------------------
// Child spawning
// ---------------------------------------------------------------------------

/// Resolve the program to exec and the remaining arguments from `argv`.
///
/// If the env var `TERMHUB_AGENT_BIN` is set, its value replaces `argv[0]`
/// (the bare `termhub-agent` or `wsl.exe` that `launch_argv` returns). This
/// lets tests and developers point at a freshly built binary without touching
/// PATH or `launch_argv`:
///
/// ```sh
/// TERMHUB_AGENT_BIN=/path/to/target/debug/termhub-agent cargo test
/// ```
pub(crate) fn spawn_child(argv: Vec<String>) -> std::io::Result<Child> {
    if argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "launch_argv returned empty argv",
        ));
    }

    // TERMHUB_AGENT_BIN: optional override for argv[0], documented above.
    let program = std::env::var("TERMHUB_AGENT_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| argv[0].clone());

    let args = &argv[1..];

    Command::new(&program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // agent diagnostics go to core's stderr
        .spawn()
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
    use std::time::Duration;

    use crate::agent::AgentBridge;

    /// Integration test: spin up the real termhub-agent binary, send Hello,
    /// list sessions, and fetch metrics. Skips gracefully when the binary is
    /// absent so CI never fails spuriously without a built agent.
    #[test]
    fn live_round_trip_with_real_agent() {
        // Locate the agent binary. We look for TERMHUB_AGENT_BIN first (already
        // set by this test's environment if the caller ran build first), then
        // fall back to the workspace-relative debug build.
        let bin_path: PathBuf = {
            // Walk up from the manifest dir to find the workspace target dir.
            // __FILE__ is inside src-tauri/src/agent/; target/ is a sibling of
            // src-tauri/.
            let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest.join("target/debug/termhub-agent")
        };

        if !bin_path.exists() {
            eprintln!(
                "transport_tests::live_round_trip_with_real_agent: \
                 binary not found at {bin_path:?} — skipping (run \
                 `cargo build -p termhub-agent` first or set TERMHUB_AGENT_BIN)"
            );
            return;
        }

        // Point the escape hatch at the known-good debug binary.
        std::env::set_var("TERMHUB_AGENT_BIN", &bin_path);

        let bridge = AgentBridge::new();
        bridge.connect("ignored").expect("connect() must succeed");

        // Give the reader thread a moment to complete the handshake + replay.
        std::thread::sleep(Duration::from_millis(100));

        // --- ListSessions ---
        let resp = bridge
            .request(termhub_protocol::AgentRequest::ListSessions)
            .expect("ListSessions must return a response");
        match resp {
            termhub_protocol::AgentResponse::Sessions { names } => {
                // The agent may or may not have active tmux sessions; the
                // important thing is that the response is the right variant.
                eprintln!("live_round_trip: sessions={names:?}");
            }
            other => panic!("expected Sessions, got {other:?}"),
        }

        // --- Metrics ---
        let resp = bridge
            .request(termhub_protocol::AgentRequest::Metrics)
            .expect("Metrics must return a response");
        match resp {
            termhub_protocol::AgentResponse::Metrics(m) => {
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
}
