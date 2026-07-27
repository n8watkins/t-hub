//! Core-side **agent bridge** (PLAN.md Workstream A, core half).
//!
//! Owns the long-lived connection to the WSL-side `t-hub-agent`:
//!   - launches the WSL-side `t-hub-agent` on Windows, or the agent directly on
//!     a unix dev box ([`launch_argv`]);
//!   - performs the [`Hello`]/[`Ready`] handshake;
//!   - correlates [`AgentRequest`]s with [`AgentResponse`]s by [`RequestId`];
//!   - consumes streamed/replayed [`EventJournalEntry`]s, advances the journal
//!     cursor, feeds [`crate::supervision::Supervisor`], and fans entries out to
//!     the UI via the [`crate::events`] journal/agent channels;
//!   - exposes WSL metrics / git / registry RPCs to the rest of the core.
//!
//! This file defines the public bridge contract and owns the serialized
//! connection lifecycle, handshake, replay verification, request correlation,
//! and journal ingestion.
//! Child-process and reader-thread mechanics live in `connection.rs`.

mod connection;
pub mod emit;
mod install;

pub use connection::ConnectionState;
pub use emit::EventEmitter;
pub(crate) use install::bundled_agent_path;
#[cfg(windows)]
pub(crate) use install::deploy_bundled_agent;
#[cfg(any(windows, test))]
pub(crate) use install::DeployOutcome;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{mpsc, Arc, LazyLock, Weak},
};

use parking_lot::Mutex;
use t_hub_protocol::{
    AgentRequest, AgentResponse, Channel, CoreFrame, CoreToAgent, EventJournalEntry, GitInfo,
    Hello, HostMetrics, PreviewListenerOwnership, Priority, ResponseErrorKind, TerminalSnapshot,
    WorktreeInfo, PROTOCOL_VERSION,
};

use crate::supervision::Supervisor;
use connection::{spawn_child, spawn_reader, write_frame, ReaderJournalFlow, TransportHandles};
use emit::{
    JournalEventPayload, JournalVoiceAnnouncement, JournalVoiceAnnouncementKind,
    SessionStatusPayload, SessionTitlePayload, EVT_AGENT_STATE, EVT_JOURNAL, EVT_SESSION_STATUS,
    EVT_STATUS_SNAPSHOT, EVT_SUPERVISION, EVT_TITLE,
};

/// How the core reaches the agent on this platform.
///
/// On Windows the agent runs inside the distro via `wsl.exe`; on unix (dev) it
/// is spawned directly so the whole spine is exercisable in this shell.
///
/// ## Windows agent resolution
///
/// The bundled `t-hub-agent` is installed to `~/.local/bin/t-hub-agent`
/// inside the distro. The packaged app deploys and hash-verifies that exact
/// path before connecting. Launching a bare `t-hub-agent` through `PATH` could
/// select an older helper elsewhere in the user's profile, so the bridge
/// executes the installed path directly:
///
/// ```text
/// wsl.exe -d <distro> --cd ~ -e bash -lc \
///     "exec $HOME/.local/bin/t-hub-agent --stdio"
/// ```
///
/// `exec` replaces the shell with the agent so there is no extra process in the
/// tree and stdio is wired straight through. `$HOME` is expanded by WSL's bash,
/// keeping the native Windows process independent of the distro home path.
///
/// The `T_HUB_AGENT_BIN` escape hatch is honored on unix, Windows dev builds,
/// and in tests. It bypasses the login-shell hop entirely: when set, its value
/// is used **verbatim** as the program to spawn, so a developer can point the
/// bridge at an arbitrary binary without touching PATH or the distro. Packaged
/// Windows builds ignore it and always launch the deployed, verified helper.
///
/// Called by SUBAGENT(agent-bridge)'s transport when it spawns the child.
fn direct_agent_argv(program: &str, journal_dir: Option<&str>) -> Vec<String> {
    let mut argv = vec![program.to_string()];
    if let Some(dir) = journal_dir {
        argv.push("--journal-dir".to_string());
        argv.push(dir.to_string());
    }
    argv.push("--stdio".to_string());
    argv
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn windows_agent_argv(distro: &str, journal_dir: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "wsl.exe".to_string(),
        "-d".to_string(),
        distro.to_string(),
        "--cd".to_string(),
        "~".to_string(),
        "-e".to_string(),
        "bash".to_string(),
        "-lc".to_string(),
    ];
    if let Some(dir) = journal_dir {
        // The value is passed as a positional argument rather than interpolated
        // into shell source, so spaces and shell metacharacters remain inert.
        argv.extend([
            "exec $HOME/.local/bin/t-hub-agent --journal-dir \"$1\" --stdio".to_string(),
            "t-hub-agent".to_string(),
            dir.to_string(),
        ]);
    } else {
        argv.push("exec $HOME/.local/bin/t-hub-agent --stdio".to_string());
    }
    argv
}

#[allow(dead_code)]
pub fn launch_argv(distro: &str) -> Vec<String> {
    let journal_dir = std::env::var("T_HUB_AGENT_JOURNAL_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty());
    #[cfg(windows)]
    {
        // Escape hatch: if T_HUB_AGENT_BIN is set, spawn it verbatim (no
        // wsl.exe / login-shell hop). This keeps the override usable on Windows
        // where it would otherwise be misapplied as wsl.exe's argv[0].
        if let Some(bin) = agent_bin_override() {
            return direct_agent_argv(&bin, journal_dir.as_deref());
        }
        // Launch the exact path that packaged startup deployed and verified.
        // `-e` makes wsl.exe exec bash directly; a bare `--` routes the command
        // through the user's default login shell instead.
        windows_agent_argv(distro, journal_dir.as_deref())
    }
    #[cfg(unix)]
    {
        let _ = distro; // distro is irrelevant when launching directly.
        let _ = journal_dir; // inherited env resolves a relative path against HOME.
        direct_agent_argv("t-hub-agent", None)
    }
}

pub(crate) fn agent_bin_override() -> Option<String> {
    #[cfg(any(not(windows), test, feature = "devbuild"))]
    {
        std::env::var("T_HUB_AGENT_BIN")
            .ok()
            .filter(|value| !value.is_empty())
    }
    #[cfg(all(windows, not(test), not(feature = "devbuild")))]
    {
        None
    }
}

/// Shared handle to the agent connection + the supervision reducer it feeds.
/// Cloneable (`Arc` inside) so Tauri-managed state and the reader thread share
/// one connection.
#[derive(Clone)]
pub struct AgentBridge {
    inner: Arc<BridgeInner>,
}

static ACTIVE_BRIDGE: LazyLock<Mutex<Weak<BridgeInner>>> =
    LazyLock::new(|| Mutex::new(Weak::new()));

#[cfg(test)]
static AGENT_TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
struct TestEnvVar {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl TestEnvVar {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

#[cfg(test)]
impl Drop for TestEnvVar {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct BridgeInner {
    /// Serializes connection lifecycle changes so superseded attempts cannot
    /// publish state for a newer transport.
    lifecycle: Mutex<()>,
    bundled_agent: Mutex<Option<std::path::PathBuf>>,
    /// The supervision reducer, fed by incoming journal events. Shared so the
    /// supervision Tauri commands can read snapshots without a round-trip.
    supervisor: Mutex<Supervisor>,
    /// Connection state machine. SUBAGENT(agent-bridge) drives this from the
    /// transport threads.
    state: Mutex<ConnectionState>,
    /// Highest journal sequence the core has durably consumed (the replay
    /// cursor). Advanced as entries arrive; persisted by workstream G later.
    journal_cursor: Mutex<u64>,
    /// Cold-start replay rebuilds backend authority without forwarding every
    /// historical event into the webview. The latest title/status plus the set
    /// of affected sessions are flushed once after ReplayComplete.
    replay_accumulator: Mutex<ReplayAccumulator>,
    /// Live transport handles (stdin writer + correlation map). `None` when
    /// disconnected. Set by `connect()`, read by `request()`.
    transport: Mutex<Option<Arc<TransportHandles>>>,
    /// The live UI event sink. `None` until [`AgentBridge::set_emitter`] installs
    /// it from the Tauri `setup()` hook (the bridge is built before the
    /// `AppHandle` exists, and unit tests never install one). All emission goes
    /// through [`BridgeInner::emit`] / [`BridgeInner::emit_json`], which are
    /// no-ops while this is `None`.
    emitter: Mutex<Option<Arc<dyn EventEmitter>>>,
    /// The status bridge, so a `StatusSnapshot` journal entry can be ingested and
    /// re-emitted as `status://snapshot` from the single journal-consume path.
    /// `None` until wired in `setup()` (and under unit tests). Held as a trait-
    /// free `Arc<StatusBridge>` to avoid a cycle with `claude`.
    status: Mutex<Option<Arc<crate::claude::StatusBridge>>>,
    /// Optional observer invoked with `(session_uuid, status)` every time
    /// [`AgentBridge::emit_session`] emits a session's status. Wired in `setup()` to
    /// the fleet notifier so a supervised session's transition can wake the
    /// orchestrator. `None` pre-setup and under unit tests (then a no-op). Kept
    /// trait-free (a boxed closure) so the `agent` module needs no fleet types.
    status_observer: Mutex<Option<StatusObserver>>,
}

#[derive(Default)]
struct ReplayAccumulator {
    sessions: BTreeSet<String>,
    status_sessions: BTreeSet<String>,
    titles: BTreeMap<String, SessionTitlePayload>,
}

/// A callback fired on every session status emit: `(session_uuid, status)`. The
/// fleet notifier installs one via [`AgentBridge::set_status_observer`]; it is the
/// server-side seam that turns a supervised session's transition into an
/// orchestrator wake, without coupling the `agent` module to fleet concepts.
pub type StatusObserver = Arc<dyn Fn(&str, crate::model::SessionStatus) + Send + Sync>;

/// Stable failure categories for the GitInfo capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitInfoBridgeError {
    Disconnected(String),
    Unsupported(String),
    CommandFailed(String),
}

/// Stable failure categories for the terminal snapshot capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalSnapshotBridgeError {
    Disconnected(String),
    TimedOut(String),
    Unsupported(String),
    CommandFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequestError {
    Disconnected(String),
    WriteFailed(String),
    TimedOut(String),
}

fn classify_request_receive_error(id: u64, error: mpsc::RecvTimeoutError) -> AgentRequestError {
    match error {
        mpsc::RecvTimeoutError::Timeout => {
            AgentRequestError::TimedOut(format!("request id={id} timed out after 10 seconds"))
        }
        mpsc::RecvTimeoutError::Disconnected => AgentRequestError::Disconnected(format!(
            "agent bridge disconnected while awaiting request id={id}"
        )),
    }
}

impl std::fmt::Display for AgentRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected(message) | Self::WriteFailed(message) | Self::TimedOut(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl BridgeInner {
    /// Emit a `Serialize` payload on `channel` if an emitter is installed; a
    /// no-op otherwise (pre-`setup()` and under unit tests). Best-effort: the
    /// emitter swallows transport errors so UI delivery never blocks ingestion.
    fn emit<T: serde::Serialize>(&self, channel: &str, payload: &T) {
        let emitter = self.emitter.lock().clone();
        if let Some(e) = emitter {
            e.emit(channel, payload);
        }
    }
}

impl Default for AgentBridge {
    fn default() -> Self {
        let bridge = Self {
            inner: Arc::new(BridgeInner {
                lifecycle: Mutex::new(()),
                bundled_agent: Mutex::new(None),
                supervisor: Mutex::new(Supervisor::new()),
                state: Mutex::new(ConnectionState::Disconnected),
                journal_cursor: Mutex::new(0),
                replay_accumulator: Mutex::new(ReplayAccumulator::default()),
                transport: Mutex::new(None),
                emitter: Mutex::new(None),
                status: Mutex::new(None),
                status_observer: Mutex::new(None),
            }),
        };
        *ACTIVE_BRIDGE.lock() = Arc::downgrade(&bridge.inner);
        bridge
    }
}

impl AgentBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_bundled_agent_path(&self, path: Option<std::path::PathBuf>) {
        *self.inner.bundled_agent.lock() = path;
    }

    #[cfg(windows)]
    pub(crate) fn deploy_packaged_agent(&self, distro: &str) -> Result<DeployOutcome, String> {
        let resource = self
            .inner
            .bundled_agent
            .lock()
            .clone()
            .ok_or_else(|| "bundled WSL helper resource path is unavailable".to_string())?;
        deploy_bundled_agent(distro, &resource).map_err(|error| format!("{error:#}"))
    }

    /// Current connection state (for the UI health area / diagnostics).
    pub fn state(&self) -> ConnectionState {
        *self.inner.state.lock()
    }

    /// The core's journal replay cursor (highest consumed seq).
    pub fn journal_cursor(&self) -> u64 {
        *self.inner.journal_cursor.lock()
    }

    /// Advance the replay cursor to `seq` if it moves it forward. Returns whether
    /// the cursor actually advanced (so the caller can decide whether to emit an
    /// `agent://state` reflecting the new `journalCursor`). A late/duplicate lower
    /// seq is ignored (the cursor never regresses).
    fn advance_cursor(&self, seq: u64) -> bool {
        let mut cursor = self.inner.journal_cursor.lock();
        if seq > *cursor {
            *cursor = seq;
            true
        } else {
            false
        }
    }

    /// Run a closure against the supervision reducer (read or mutate). Used by
    /// the supervision Tauri commands and by the journal consumer.
    pub fn with_supervisor<R>(&self, f: impl FnOnce(&mut Supervisor) -> R) -> R {
        f(&mut self.inner.supervisor.lock())
    }

    /// Install the live UI event sink (called once from the Tauri `setup()` hook,
    /// after the `AppHandle` exists). Idempotent: a later call replaces the sink.
    /// Emits an initial `agent://state` so the UI reflects the current connection
    /// state the moment the sink is wired, without waiting for a transition.
    pub fn set_emitter(&self, emitter: Arc<dyn EventEmitter>) {
        *self.inner.emitter.lock() = Some(emitter);
        // Push the current state immediately so a UI that mounts after a connect
        // already happened still gets a live `agent://state`.
        self.emit_agent_state();
    }

    /// Wire the status bridge so `StatusSnapshot` journal entries flowing through
    /// [`AgentBridge::consume_journal_entry`] are ingested + re-emitted as
    /// `status://snapshot`. Called once from `setup()` alongside the emitter.
    pub fn set_status_bridge(&self, status: Arc<crate::claude::StatusBridge>) {
        *self.inner.status.lock() = Some(status);
    }

    /// Install the status observer (see [`StatusObserver`]). Called once from
    /// `setup()` after the fleet notifier is built. Replaces any prior observer.
    pub fn set_status_observer(&self, observer: StatusObserver) {
        *self.inner.status_observer.lock() = Some(observer);
    }

    /// Transition the connection state **and** emit `agent://state` so the UI's
    /// health area is always live. Centralizing the write here guarantees no
    /// transition can silently skip the emit (the historical bug was that
    /// `connect()` mutated `state` directly and nothing was emitted).
    fn set_state(&self, next: ConnectionState) {
        {
            let mut guard = self.inner.state.lock();
            if *guard == next {
                return; // no change → no emit (avoid event spam on reconnect loops)
            }
            *guard = next;
        }
        self.emit_agent_state();
    }

    /// Emit the current `agent://state` payload (connection + journal cursor).
    fn emit_agent_state(&self) {
        let payload = crate::commands_05::AgentStateInfo {
            connection: self.state(),
            journal_cursor: self.journal_cursor(),
        };
        self.inner.emit(EVT_AGENT_STATE, &payload);
    }

    /// Launch the agent and complete the handshake.
    ///
    /// Spawns the child from [`launch_argv`] with piped stdin/stdout (stderr
    /// inherited). Starts a reader thread that dispatches incoming
    /// [`t_hub_protocol::AgentToCore`] frames. Sends `Hello`, waits for
    /// `Ready`, verifies the exact protocol version, and if the agent's
    /// `journal_head_seq` is ahead of our cursor, sends `ReplayJournal`.
    /// Replayed state remains buffered until a durable `ReplayBoundary` and
    /// matching `ReplayComplete` reach the advertised head, after which the
    /// buffered entries are committed and the state becomes `Live`.
    ///
    /// The `T_HUB_AGENT_BIN` developer escape hatch is limited to Windows dev
    /// builds and tests, and overrides the child program on unix (see
    /// [`launch_argv`] and [`connection::spawn_child`]).
    pub fn connect(&self, distro: &str) -> Result<(), String> {
        self.connect_with_timeouts(
            distro,
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(30),
        )
    }

    fn connect_with_timeouts(
        &self,
        distro: &str,
        ready_timeout: std::time::Duration,
        replay_timeout: std::time::Duration,
    ) -> Result<(), String> {
        let _lifecycle = self.inner.lifecycle.lock();
        self.connect_locked(distro, ready_timeout, replay_timeout)
    }

    fn connect_locked(
        &self,
        distro: &str,
        ready_timeout: std::time::Duration,
        replay_timeout: std::time::Duration,
    ) -> Result<(), String> {
        if self.inner.transport.lock().is_some() {
            return Err("agent bridge already connected".to_string());
        }

        #[cfg(windows)]
        if agent_bin_override().is_none() {
            match self.deploy_packaged_agent(distro) {
                Ok(DeployOutcome::AlreadyCurrent) => {
                    eprintln!("t-hub: bundled WSL helper verified");
                }
                Ok(DeployOutcome::Installed) => {
                    eprintln!("t-hub: installed and verified bundled WSL helper");
                }
                Err(error) => {
                    self.set_state(ConnectionState::Failed);
                    return Err(format!(
                        "bundled WSL helper deployment failed; refusing to connect with an \
                         unverified helper: {error}"
                    ));
                }
            }
        }

        // Build argv and spawn child.
        let argv = launch_argv(distro);
        let mut child = match spawn_child(argv) {
            Ok(child) => child,
            Err(error) => {
                self.set_state(ConnectionState::Failed);
                return Err(format!("failed to spawn agent: {error}"));
            }
        };

        // Take ownership of the stdio handles before the child handle moves
        // into TransportHandles.
        let child_stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                self.set_state(ConnectionState::Failed);
                return Err("child has no stdin pipe".to_string());
            }
        };
        let child_stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                self.set_state(ConnectionState::Failed);
                return Err("child has no stdout pipe".to_string());
            }
        };

        // Build the shared correlation map and next-id counter.
        let pending = Arc::new(connection::CorrelationMap::new(
            std::collections::HashMap::new(),
        ));
        let next_id = Arc::new(std::sync::atomic::AtomicU64::new(1));

        // One-shot channels for the handshake/replay synchronisation.
        let (ready_tx, ready_rx) = mpsc::channel();
        let (replay_done_tx, replay_done_rx) = mpsc::channel::<Result<u64, String>>();
        let journal_flow = ReaderJournalFlow::new();

        let reader = match spawn_reader(
            child_stdout,
            Arc::clone(&pending),
            self.clone(),
            Arc::clone(&journal_flow),
            ready_tx,
            replay_done_tx,
        ) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                self.set_state(ConnectionState::Failed);
                return Err(format!("failed to spawn agent reader: {error}"));
            }
        };

        // Build transport handles (Arc so request() can clone a reference).
        let handles = Arc::new(TransportHandles {
            stdin: Mutex::new(child_stdin),
            pending: Arc::clone(&pending),
            next_id: Arc::clone(&next_id),
            child: Mutex::new(child),
            journal_flow: Arc::clone(&journal_flow),
            reader: Mutex::new(Some(reader)),
        });

        // Set state and store transport handles.
        self.set_state(ConnectionState::Handshaking);
        *self.inner.transport.lock() = Some(Arc::clone(&handles));

        // --- Handshake: send Hello ---
        {
            let hello = CoreFrame {
                channel: Channel::Control,
                msg: CoreToAgent::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    core_version: format!("t-hub {}", env!("CARGO_PKG_VERSION")),
                }),
            };
            let write_result = {
                let mut stdin_guard = handles.stdin.lock();
                write_frame(&mut *stdin_guard, &hello)
            };
            if let Err(error) = write_result {
                return Err(self.fail_connection(
                    &handles,
                    &journal_flow,
                    format!("failed to write Hello: {error}"),
                ));
            }
        }

        // Wait for Ready (10 s timeout). On failure, mark Failed (and emit) so
        // the UI shows the dead connection rather than a stuck "handshaking".
        let ready = match ready_rx.recv_timeout(ready_timeout) {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                return Err(self.fail_connection(
                    &handles,
                    &journal_flow,
                    format!("agent handshake failed: {error}"),
                ));
            }
            Err(_) => {
                return Err(self.fail_connection(
                    &handles,
                    &journal_flow,
                    "timed out waiting for Ready from agent".to_string(),
                ));
            }
        };
        if ready.protocol_version != PROTOCOL_VERSION {
            return Err(self.fail_connection(
                &handles,
                &journal_flow,
                format!(
                    "agent protocol version mismatch: expected {PROTOCOL_VERSION}, received {}",
                    ready.protocol_version
                ),
            ));
        }
        let journal_head_seq = ready.journal_head_seq;

        // If the agent has journal entries we haven't consumed, request replay.
        let cursor = self.journal_cursor();
        let replayed = journal_head_seq > cursor;
        if replayed {
            journal_flow.begin_replay(cursor);
            self.set_state(ConnectionState::Replaying);

            let replay_frame = CoreFrame {
                channel: Channel::Control,
                msg: CoreToAgent::ReplayJournal { after_seq: cursor },
            };
            let write_result = {
                let mut stdin_guard = handles.stdin.lock();
                write_frame(&mut *stdin_guard, &replay_frame)
            };
            if let Err(error) = write_result {
                return Err(self.fail_connection(
                    &handles,
                    &journal_flow,
                    format!("failed to write ReplayJournal: {error}"),
                ));
            }

            match replay_done_rx.recv_timeout(replay_timeout) {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    return Err(self.fail_connection(
                        &handles,
                        &journal_flow,
                        format!("incomplete journal replay: {error}"),
                    ));
                }
                Err(_) => {
                    return Err(self.fail_connection(
                        &handles,
                        &journal_flow,
                        "timed out waiting for ReplayComplete from agent".to_string(),
                    ));
                }
            }
            if let Err(error) = journal_flow.complete_replay(self, journal_head_seq) {
                return Err(self.fail_connection(
                    &handles,
                    &journal_flow,
                    format!("invalid journal replay: {error}"),
                ));
            }
        } else {
            journal_flow.complete_without_replay(self);
        }

        Ok(())
    }

    fn fail_connection(
        &self,
        handles: &Arc<TransportHandles>,
        journal_flow: &ReaderJournalFlow,
        error: String,
    ) -> String {
        journal_flow.cancel();
        let was_current = {
            let mut transport = self.inner.transport.lock();
            if transport
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, handles))
            {
                transport.take();
                true
            } else {
                false
            }
        };
        handles.shutdown();
        if was_current {
            self.set_state(ConnectionState::Failed);
        }
        error
    }

    fn fail_reader_transport(&self, journal_flow: &Arc<ReaderJournalFlow>, error: &str) {
        let handles = {
            let mut transport = self.inner.transport.lock();
            if transport
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.journal_flow, journal_flow))
            {
                transport.take()
            } else {
                None
            }
        };
        let Some(handles) = handles else {
            return;
        };

        handles.journal_flow.retire();
        handles.pending.lock().clear();
        {
            let mut child = handles.child.lock();
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(handles.reader.lock().take());
        self.set_state(ConnectionState::Failed);
        eprintln!("agent-bridge: live transport failed closed: {error}");
    }

    /// Tear down the live connection so a fresh [`connect`](Self::connect) can't
    /// leak the old reader thread or orphan in-flight senders. Safe to call when
    /// already disconnected (it's a no-op then). Used by [`reconnect`](Self::reconnect)
    /// and callable directly from a tray "reconnect" action.
    ///
    /// Order matters (an earlier audit flagged a reader-thread / pending-sender
    /// leak on reconnect), so we do, in sequence:
    ///   1. **Take** the old `TransportHandles` out of `self.inner.transport`
    ///      (leaving it `None`), so any concurrent `request()` immediately sees
    ///      "not connected" rather than writing to a dying stdin.
    ///   2. **Clear the `pending` correlation map.** Dropping each one-shot
    ///      `Sender` wakes its blocked `request()` caller right away (its
    ///      `recv_timeout` returns `Err` instead of hanging the full 10 s), so no
    ///      in-flight sender is orphaned waiting on a reply that can never come.
    ///   3. **Retire the reader flow, kill and reap the child, then join the
    ///      reader thread.** A replacement transport is not published until the
    ///      old reader can no longer mutate the shared reducer or journal cursor.
    pub fn disconnect(&self) {
        let _lifecycle = self.inner.lifecycle.lock();
        self.disconnect_locked();
    }

    fn disconnect_locked(&self) {
        // 1. Detach the live transport so new requests can't use it.
        let old = self.inner.transport.lock().take();
        let Some(handles) = old else {
            // Already disconnected: still normalize the state and bail.
            self.set_state(ConnectionState::Disconnected);
            return;
        };

        handles.shutdown();

        // Drop this Arc reference. If a concurrent request() still holds a clone,
        // the struct's memory outlives this call, but the child is already killed
        // + reaped above, so teardown is complete regardless; the new connect()
        // allocates entirely fresh handles either way.
        drop(handles);
        self.set_state(ConnectionState::Disconnected);
    }

    /// Re-establish the agent connection without touching terminals: safely tear
    /// the old connection down (see [`disconnect`](Self::disconnect)) and then
    /// [`connect`](Self::connect) again. Fixes a wedged bridge ("supervision /
    /// cost stopped updating") where the reader thread died or the agent went
    /// away, with no full app restart.
    ///
    /// The journal cursor is intentionally preserved across the reconnect, so the
    /// fresh handshake only replays entries newer than what we already consumed
    /// (no duplicate ingestion).
    pub fn reconnect(&self, distro: &str) -> Result<(), String> {
        let _lifecycle = self.inner.lifecycle.lock();
        self.disconnect_locked();
        self.connect_locked(
            distro,
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(30),
        )
    }

    /// Send a request and await its correlated response (blocking, 10 s timeout).
    ///
    /// Allocates the next [`t_hub_protocol::RequestId`] from an atomic
    /// counter, registers a one-shot [`mpsc`] sender in the correlation map,
    /// serializes the [`CoreFrame`] to the child's stdin (behind a `Mutex` so
    /// concurrent callers don't interleave bytes), then blocks on the receiver.
    ///
    /// **Channel / Priority**: `Channel::Control` and `Priority::Normal` are
    /// used for all requests today. A future scheduler can inspect the request
    /// body to select the appropriate channel and priority before writing.
    pub fn request(&self, req: AgentRequest) -> Result<AgentResponse, AgentRequestError> {
        // Grab the transport handles (returns an error if not connected).
        let handles = {
            let guard = self.inner.transport.lock();
            guard.as_ref().cloned().ok_or_else(|| {
                AgentRequestError::Disconnected("agent bridge not connected".to_string())
            })?
        };

        // Allocate a unique request id.
        let id = handles
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Register the one-shot channel before writing so the reader thread
        // can never race ahead of us.
        let (tx, rx) = mpsc::channel::<AgentResponse>();
        handles.pending.lock().insert(id, tx);

        // Build and write the request frame.
        // NOTE: Channel::Control and Priority::Normal are used for all ops
        // today. Channel and Priority are fully serialized and echoed by the
        // agent; a future priority scheduler uses them to reorder the outbound
        // queue without protocol changes.
        let frame = CoreFrame {
            channel: Channel::Control,
            msg: CoreToAgent::Request {
                id,
                priority: Priority::Normal,
                body: req,
            },
        };

        {
            let mut stdin_guard = handles.stdin.lock();
            write_frame(&mut *stdin_guard, &frame).map_err(|e| {
                // Remove the dangling correlation entry on write failure.
                handles.pending.lock().remove(&id);
                AgentRequestError::WriteFailed(format!("failed to write request id={id}: {e}"))
            })?;
        }

        // Block until the reader delivers the response or we time out.
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Clean up the correlation entry so the reader doesn't deliver
                // a stale response after we've given up.
                handles.pending.lock().remove(&id);
                Err(classify_request_receive_error(
                    id,
                    mpsc::RecvTimeoutError::Timeout,
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                handles.pending.lock().remove(&id);
                Err(classify_request_receive_error(
                    id,
                    mpsc::RecvTimeoutError::Disconnected,
                ))
            }
        }
    }

    /// Convenience: fetch a host metrics snapshot.
    pub fn metrics(&self) -> Result<HostMetrics, String> {
        match self
            .request(AgentRequest::Metrics)
            .map_err(|e| e.to_string())?
        {
            AgentResponse::Metrics(m) => Ok(m),
            other => Err(format!("unexpected response to metrics: {other:?}")),
        }
    }

    /// Convenience: derive the current git branch for `cwd` (statusline lacks it).
    pub fn git_branch(&self, cwd: &str) -> Result<Option<String>, String> {
        match self
            .request(AgentRequest::GitBranch {
                cwd: cwd.to_string(),
            })
            .map_err(|e| e.to_string())?
        {
            AgentResponse::GitBranch { branch } => Ok(branch),
            other => Err(format!("unexpected response to git_branch: {other:?}")),
        }
    }

    /// Convenience: list worktrees for the repo containing `cwd`.
    pub fn git_worktrees(&self, cwd: &str) -> Result<Vec<WorktreeInfo>, String> {
        match self
            .request(AgentRequest::GitWorktrees {
                cwd: cwd.to_string(),
            })
            .map_err(|e| e.to_string())?
        {
            AgentResponse::GitWorktrees { worktrees } => Ok(worktrees),
            other => Err(format!("unexpected response to git_worktrees: {other:?}")),
        }
    }

    /// Fetch the complete Files-panel git snapshot through the persistent agent.
    pub fn git_info(&self, cwd: &str) -> Result<GitInfo, GitInfoBridgeError> {
        let response = self
            .request(AgentRequest::GitInfo {
                cwd: cwd.to_string(),
            })
            .map_err(|error| GitInfoBridgeError::Disconnected(error.to_string()))?;
        map_git_info_response(response)
    }

    /// Inspect a managed Preview listener inside WSL through the persistent,
    /// correlated agent channel.
    ///
    /// Every echoed field must match the authenticated supervisor expectation.
    /// A stale or crossed response is refused even when the transport request id
    /// itself was valid.
    #[allow(dead_code)]
    pub(crate) fn inspect_preview_listener(
        &self,
        run_id: &str,
        generation: &str,
        port: u16,
        expected_process_group_id: u32,
        expected_process_group_started_at: u64,
    ) -> Result<Option<PreviewListenerOwnership>, String> {
        let response = self
            .request(AgentRequest::InspectPreviewListener {
                run_id: run_id.to_string(),
                generation: generation.to_string(),
                port,
                expected_process_group_id,
                expected_process_group_started_at,
            })
            .map_err(|error| error.to_string())?;
        map_preview_listener_response(
            response,
            run_id,
            generation,
            port,
            expected_process_group_id,
            expected_process_group_started_at,
        )
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn map_preview_listener_response(
    response: AgentResponse,
    run_id: &str,
    generation: &str,
    port: u16,
    expected_process_group_id: u32,
    expected_process_group_started_at: u64,
) -> Result<Option<PreviewListenerOwnership>, String> {
    match response {
        AgentResponse::PreviewListener {
            run_id: echoed_run_id,
            generation: echoed_generation,
            port: echoed_port,
            expected_process_group_id: echoed_group,
            expected_process_group_started_at: echoed_started,
            ownership,
        } if echoed_run_id == run_id
            && echoed_generation == generation
            && echoed_port == port
            && echoed_group == expected_process_group_id
            && echoed_started == expected_process_group_started_at =>
        {
            Ok(ownership)
        }
        AgentResponse::PreviewListener { .. } => {
            Err("WSL Preview listener response correlation changed".into())
        }
        AgentResponse::Error { kind, message } => Err(format!(
            "WSL Preview listener inspection {kind:?}: {message}"
        )),
        other => Err(format!(
            "unexpected response to WSL Preview listener inspection: {other:?}"
        )),
    }
}

impl AgentBridge {
    /// Fetch terminal reconciliation metadata through the persistent agent.
    pub fn terminal_snapshot(&self) -> Result<TerminalSnapshot, TerminalSnapshotBridgeError> {
        let response =
            self.request(AgentRequest::TerminalSnapshot)
                .map_err(|error| match error {
                    AgentRequestError::Disconnected(message)
                    | AgentRequestError::WriteFailed(message) => {
                        TerminalSnapshotBridgeError::Disconnected(message)
                    }
                    AgentRequestError::TimedOut(message) => {
                        TerminalSnapshotBridgeError::TimedOut(message)
                    }
                })?;
        map_terminal_snapshot_response(response)
    }

    /// Consume one journal entry: advance the cursor, feed supervision, emit the
    /// live UI events, and return the affected session id.
    ///
    /// This is the core's single ingestion point for the spine. It is where the
    /// previously-missing **live emit** happens — when an entry is consumed it
    /// now fans out:
    ///   - `agent://journal` (the entry, forwarded verbatim);
    ///   - `agent://state` (if the cursor advanced — the health area shows it);
    ///   - `supervision://tree` + `session://status` (for the affected session,
    ///     after the supervision reducer has updated);
    ///   - `status://snapshot` (when the entry is a `StatusSnapshot`, routed
    ///     through the status bridge if one is wired).
    ///
    /// All emission is best-effort and a no-op before [`AgentBridge::set_emitter`]
    /// (so the unit tests that call this directly still pass). Returns the
    /// affected session id for callers/tests that want it.
    pub fn consume_journal_entry(&self, entry: &EventJournalEntry) -> Option<String> {
        self.consume_journal_entry_with_provenance(entry, false)
    }

    fn consume_journal_entry_with_provenance(
        &self,
        entry: &EventJournalEntry,
        replayed: bool,
    ) -> Option<String> {
        // The journal sequence is the replay idempotency boundary. Reject an
        // already-consumed or out-of-order entry before every side effect,
        // including status ingestion, UI emission, title derivation, provider
        // binding, and supervision reduction. Otherwise a late SessionStart can
        // erase a newer pending permission and falsely report Working.
        if !self.advance_cursor(entry.seq) {
            return None;
        }
        // StatusSnapshot entries get a DEDICATED minimal path. The statusline
        // re-journals an IDENTICAL snapshot ~25x/sec/session (only `ingested_at_ms`
        // ticks). On that path the full fan-out below is pure waste and a sustained
        // webview flood (the "constant freeze", and what makes a window drag lock
        // up): a status snapshot NEVER advances the supervision reducer (its `_`
        // arm), so `supervision://tree` + `session://status` would carry identical
        // payloads; `agent://journal` has NO frontend consumer at all; and the
        // `agent://state` cursor bump is cosmetic. So we keep the status bridge +
        // replay cursor current and emit ONLY `status://snapshot`, and ONLY when the
        // snapshot meaningfully changed (the `same_status` gate in
        // `ingest_status_from_journal`). No other channel fires.
        if matches!(
            entry.event_type,
            t_hub_protocol::JournalEventType::StatusSnapshot
        ) {
            let affected = self.ingest_status_from_journal(entry, !replayed);
            if replayed {
                if let Some(session_id) = affected.as_deref() {
                    self.inner
                        .replay_accumulator
                        .lock()
                        .status_sessions
                        .insert(session_id.to_string());
                }
            }
            // Return the entry's own session id for callers/tests; no tree/status
            // emit (the reducer status is unchanged by a status snapshot).
            return affected;
        }

        // 1. The replay cursor moved, so the health area's journalCursor changed.
        if !replayed {
            self.emit_agent_state();
        }

        // NOTE: StatusSnapshot entries never reach here — they are short-circuited
        // at the top of this method onto a dedicated minimal path (status bridge +
        // cursor only, no fan-out). See the early return above.

        // 3b. Derive a credential-safe title for the session (GOAL NAMES) and
        //     emit `agent://title`. Current hook producers redact prompt text
        //     before persistence, so cwd basename is their safe title signal.
        //     Legacy entries that already contain a prompt remain readable.
        //     Carries `cwd` so the frontend can correlate the Claude session id
        //     to a T-Hub terminal.
        if let Some(title) = self.session_title_payload(entry) {
            if replayed {
                self.inner
                    .replay_accumulator
                    .lock()
                    .titles
                    .insert(title.session_id.clone(), title);
            } else {
                self.inner.emit(EVT_TITLE, &title);
            }
        }

        // 4. Feed the supervision reducer. Pull the subagent base fields out of
        //    the payload (hooks put `agent_id` / `agent_type` in stdin inside
        //    subagents — REVIEW base fields).
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let agent_id = entry.payload.get("agent_id").and_then(|v| v.as_str());
        let agent_type = entry.payload.get("agent_type").and_then(|v| v.as_str());
        // Claude Code's `Notification` hook carries a `notification_type`
        // discriminator (`idle_prompt`, `permission_prompt`, …). The reducer needs
        // it to tell the 60s idle ping from a real needs-input event; absent for
        // every other hook.
        let notification_type = entry
            .payload
            .get("notification_type")
            .and_then(|v| v.as_str());
        let previous_status = (!replayed)
            .then(|| session_id.map(|sid| self.with_supervisor(|s| s.status(sid))))
            .flatten();

        // Structured provider lifecycle events carry the exact tmux binding.
        // Feed that binding into the existing session-to-terminal authority so
        // fleet attention can resolve this provider session to its Crew tile.
        if entry.payload.get("provider").and_then(|v| v.as_str()) == Some("codex") {
            if let (Some(sid), Some(status_bridge)) = (session_id, self.inner.status.lock().clone())
            {
                status_bridge.ingest(sid, &entry.payload, entry.timestamp_ms);
                if replayed {
                    self.inner
                        .replay_accumulator
                        .lock()
                        .status_sessions
                        .insert(sid.to_string());
                }
            }
        }

        let affected = self.with_supervisor(|s| {
            s.ingest_with_payload(
                session_id,
                agent_id,
                agent_type,
                notification_type,
                entry.event_type,
                entry.timestamp_ms,
                Some(&entry.payload),
            )
        });

        if replayed {
            if let Some(session_id) = affected.as_deref().or(session_id) {
                self.inner
                    .replay_accumulator
                    .lock()
                    .sessions
                    .insert(session_id.to_string());
            }
            if matches!(
                entry.event_type,
                t_hub_protocol::JournalEventType::SessionEnd
            ) {
                if let (Some(sid), Some(status_bridge)) =
                    (session_id, self.inner.status.lock().clone())
                {
                    status_bridge.evict(sid);
                }
            }
            return affected;
        }

        // Emit the committed journal entry only after its exact reducer result
        // is known. Voice consumes this correlated authority instead of racing a
        // later session-status event. Completion is a status edge, so a Stop
        // with outstanding children is silent and the eventual child/task drain
        // owns the single completion announcement.
        let authority_session = affected.as_deref().or(session_id);
        let voice_announcement = authority_session.and_then(|sid| {
            let status = self.with_supervisor(|s| s.status(sid));
            let kind = if status == crate::model::SessionStatus::Failed
                && previous_status != Some(crate::model::SessionStatus::Failed)
            {
                Some(JournalVoiceAnnouncementKind::Failure)
            } else if status == crate::model::SessionStatus::Completed
                && previous_status != Some(crate::model::SessionStatus::Completed)
            {
                Some(JournalVoiceAnnouncementKind::Completion)
            } else if entry.event_type == t_hub_protocol::JournalEventType::PermissionRequest
                && status == crate::model::SessionStatus::NeedsPermission
            {
                Some(JournalVoiceAnnouncementKind::Permission)
            } else if entry.event_type == t_hub_protocol::JournalEventType::Elicitation
                && status == crate::model::SessionStatus::NeedsQuestion
            {
                Some(JournalVoiceAnnouncementKind::Question)
            } else {
                None
            };
            kind.map(|kind| JournalVoiceAnnouncement {
                kind,
                session_id: sid.to_string(),
                status,
            })
        });
        self.inner.emit(
            EVT_JOURNAL,
            &JournalEventPayload {
                entry,
                replayed,
                voice_announcement,
            },
        );

        // Emit the fresh tree + status for the affected session so the sidebar
        //    re-renders live (this is the headline FR-012 path).
        if let Some(sid) = affected.as_deref() {
            self.emit_session(sid);
        }

        // Preserve the terminal supervision status for the event emitted above,
        // then remove stale runtime identity evidence. History must never promote
        // an ended Harness to Active from an old statusline snapshot.
        if matches!(
            entry.event_type,
            t_hub_protocol::JournalEventType::SessionEnd
        ) {
            if let (Some(sid), Some(status_bridge)) = (session_id, self.inner.status.lock().clone())
            {
                status_bridge.evict(sid);
            }
        }

        affected
    }

    /// Emit `supervision://tree` and `session://status` for one session from the
    /// current reducer state. Public-in-crate so the status bridge / commands can
    /// re-emit a session after an out-of-band status change.
    pub(crate) fn emit_session(&self, session_id: &str) {
        let (tree, status, runtime_health, permission_request) = self.with_supervisor(|s| {
            (
                s.tree(session_id),
                s.status(session_id),
                s.runtime_health(session_id),
                s.permission_request(session_id),
            )
        });
        if let Some(tree) = tree {
            self.inner.emit(EVT_SUPERVISION, &tree);
        }
        self.inner.emit(
            EVT_SESSION_STATUS,
            &SessionStatusPayload {
                session_id: session_id.to_string(),
                status,
                runtime_health,
                permission_request,
            },
        );
        // Notify the fleet observer (if wired) so a supervised session's transition
        // can wake the orchestrator. Cloned out of the lock first so the observer's
        // own locking can't deadlock against this emit path.
        let observer = self.inner.status_observer.lock().clone();
        if let Some(obs) = observer {
            obs(session_id, status);
        }
    }

    /// Derive a Claude-suggested title from a title-bearing hook entry and emit
    /// `agent://title` for the session (GOAL NAMES). No-op for entries that carry
    /// no usable title signal, or that have no session id. Best-effort + behind
    /// the optional emitter, like every other emit on this path.
    fn session_title_payload(&self, entry: &EventJournalEntry) -> Option<SessionTitlePayload> {
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let sid = session_id?;
        let title = derive_session_title(entry.event_type, &entry.payload)?;
        let cwd = entry
            .payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());
        Some(SessionTitlePayload {
            session_id: sid.to_string(),
            cwd,
            title,
        })
    }

    /// Route a `StatusSnapshot` journal entry into the status bridge (if wired)
    /// and emit `status://snapshot`. The payload carries the raw statusline JSON
    /// (the hook/agent put it there); we ingest it under the entry's session id.
    fn ingest_status_from_journal(&self, entry: &EventJournalEntry, emit: bool) -> Option<String> {
        let Some(status_bridge) = self.inner.status.lock().clone() else {
            return None; // no status bridge wired (pre-setup / tests)
        };
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let sid = session_id?;
        // The raw statusline lives under `payload.status` when the agent wraps it;
        // fall back to the whole payload for forward-compat.
        let raw = entry.payload.get("status").unwrap_or(&entry.payload);
        // Capture the PREVIOUS snapshot, then ingest, then only emit when something
        // MEANINGFUL changed. The statusline re-ingests an IDENTICAL snapshot many
        // times/sec (only `ingested_at_ms` ticks); emitting each was ~25 events/sec
        // PER SESSION into the webview — a sustained event flood that pinned the UI
        // (the "constant freezing") even after the frontend stopped re-rendering on
        // them. ingest() still runs every time (the bridge/restore stay current); we
        // just skip the redundant emit.
        let prev = status_bridge.get(sid);
        let snap = status_bridge.ingest(sid, raw, entry.timestamp_ms);
        if emit && prev.as_ref().is_none_or(|p| !p.same_status(&snap)) {
            self.inner.emit(EVT_STATUS_SNAPSHOT, &snap);
        }
        Some(sid.to_string())
    }

    /// Publish one bounded summary after a cold journal replay.
    ///
    /// The reducer and status bridge consume every durable entry so authority is
    /// reconstructed exactly, but forwarding each historical transition into
    /// the webview can produce hundreds of thousands of events. Replay therefore
    /// records only the latest title/status and the affected session ids, then
    /// flushes one final snapshot of each after ReplayComplete.
    fn flush_replay(&self) {
        let replay = std::mem::take(&mut *self.inner.replay_accumulator.lock());

        for title in replay.titles.into_values() {
            self.inner.emit(EVT_TITLE, &title);
        }

        if let Some(status_bridge) = self.inner.status.lock().clone() {
            for session_id in replay.status_sessions {
                if let Some(snapshot) = status_bridge.get(&session_id) {
                    self.inner.emit(EVT_STATUS_SNAPSHOT, &snapshot);
                }
            }
        }

        for session_id in replay.sessions {
            self.emit_session(&session_id);
        }
    }
}

/// Fetch git facts through the current application bridge.
pub fn git_info(cwd: &str) -> Result<GitInfo, GitInfoBridgeError> {
    let inner = ACTIVE_BRIDGE
        .lock()
        .upgrade()
        .ok_or_else(|| GitInfoBridgeError::Disconnected("agent bridge unavailable".to_string()))?;
    AgentBridge { inner }.git_info(cwd)
}

/// Fetch terminal reconciliation metadata through the current application bridge.
pub fn terminal_snapshot() -> Result<TerminalSnapshot, TerminalSnapshotBridgeError> {
    let inner = ACTIVE_BRIDGE.lock().upgrade().ok_or_else(|| {
        TerminalSnapshotBridgeError::Disconnected("agent bridge unavailable".to_string())
    })?;
    AgentBridge { inner }.terminal_snapshot()
}

fn map_git_info_response(response: AgentResponse) -> Result<GitInfo, GitInfoBridgeError> {
    match response {
        AgentResponse::GitInfo(info) => Ok(info),
        AgentResponse::Error {
            kind: ResponseErrorKind::Unsupported,
            message,
        } => Err(GitInfoBridgeError::Unsupported(message)),
        AgentResponse::Error { kind, message } => Err(GitInfoBridgeError::CommandFailed(format!(
            "{kind:?}: {message}"
        ))),
        other => Err(GitInfoBridgeError::CommandFailed(format!(
            "unexpected response: {other:?}"
        ))),
    }
}

fn map_terminal_snapshot_response(
    response: AgentResponse,
) -> Result<TerminalSnapshot, TerminalSnapshotBridgeError> {
    match response {
        AgentResponse::TerminalSnapshot(snapshot) => Ok(snapshot),
        AgentResponse::Error {
            kind: ResponseErrorKind::Unsupported,
            message,
        } => Err(TerminalSnapshotBridgeError::Unsupported(message)),
        AgentResponse::Error { kind, message } => Err(TerminalSnapshotBridgeError::CommandFailed(
            format!("{kind:?}: {message}"),
        )),
        other => Err(TerminalSnapshotBridgeError::CommandFailed(format!(
            "unexpected response: {other:?}"
        ))),
    }
}

/// Max characters for a derived session title before we ellipsize it. Long
/// enough to carry a useful task summary, short enough to fit a tile/tab label.
const TITLE_MAX_CHARS: usize = 60;

/// Derive a short, human-readable title for a session from a lifecycle hook
/// payload (GOAL NAMES). Pure (no I/O), so it is unit-tested directly.
///
/// Signal preference:
///   - **`UserPromptSubmit`**: a legacy stored `prompt`, when present; otherwise
///     the project basename from credential-safe `cwd` metadata.
///   - **`SessionStart`**: the project basename from `cwd`.
///
/// Returns `None` for events we don't title or when no usable text is present
/// (the caller then emits nothing and the existing command·cwd label stands).
///
// TODO(claude-title): when a provider supplies a credential-safe one-line title
// distinct from prompt/tool content, prefer it over cwd basename.
fn derive_session_title(
    event_type: t_hub_protocol::JournalEventType,
    payload: &serde_json::Value,
) -> Option<String> {
    use t_hub_protocol::JournalEventType as E;
    let raw = match event_type {
        E::UserPromptSubmit => {
            if let Some(prompt) = payload.get("prompt").and_then(|v| v.as_str()) {
                Some(prompt)
            } else {
                let cwd = payload.get("cwd").and_then(|v| v.as_str())?;
                return cwd_basename(cwd).map(str::to_string);
            }
        }
        E::SessionStart => {
            // Fallback to the project (cwd basename) so a brand-new session is
            // labelled before the user's first prompt arrives.
            let cwd = payload.get("cwd").and_then(|v| v.as_str())?;
            return cwd_basename(cwd).map(|s| s.to_string());
        }
        _ => None,
    }?;

    // First non-empty line, collapsed whitespace, capped length.
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(cap_title(line))
}

/// The last non-empty path segment of `cwd` (POSIX or Windows separators), or
/// `None` if there is none or it is just `~`.
fn cwd_basename(cwd: &str) -> Option<&str> {
    let last = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty() && *s != "~")?;
    Some(last)
}

/// Collapse internal whitespace runs to single spaces and cap to
/// [`TITLE_MAX_CHARS`] characters (ellipsizing on a char boundary).
fn cap_title(s: &str) -> String {
    let collapsed: String = {
        let mut out = String::with_capacity(s.len());
        let mut prev_ws = false;
        for ch in s.chars() {
            if ch.is_whitespace() {
                if !prev_ws {
                    out.push(' ');
                }
                prev_ws = true;
            } else {
                out.push(ch);
                prev_ws = false;
            }
        }
        out.trim().to_string()
    };
    if collapsed.chars().count() <= TITLE_MAX_CHARS {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(TITLE_MAX_CHARS - 1).collect();
    format!("{}…", truncated.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use t_hub_protocol::{JournalEventType, JournalSource};

    #[test]
    fn preview_listener_response_requires_every_echoed_correlation_field() {
        let response = |generation: &str| AgentResponse::PreviewListener {
            run_id: "run-1".into(),
            generation: generation.into(),
            port: 4177,
            expected_process_group_id: 42,
            expected_process_group_started_at: 99,
            ownership: Some(PreviewListenerOwnership {
                process_group_id: 42,
                process_group_started_at: 99,
            }),
        };
        assert_eq!(
            map_preview_listener_response(response("a"), "run-1", "a", 4177, 42, 99).unwrap(),
            Some(PreviewListenerOwnership {
                process_group_id: 42,
                process_group_started_at: 99,
            })
        );
        assert!(map_preview_listener_response(response("b"), "run-1", "a", 4177, 42, 99).is_err());
        assert!(map_preview_listener_response(response("a"), "run-2", "a", 4177, 42, 99).is_err());
        assert!(map_preview_listener_response(response("a"), "run-1", "a", 4178, 42, 99).is_err());
        assert!(map_preview_listener_response(response("a"), "run-1", "a", 4177, 41, 99).is_err());
        assert!(map_preview_listener_response(response("a"), "run-1", "a", 4177, 42, 98).is_err());
    }

    fn entry(
        seq: u64,
        session: &str,
        agent: Option<&str>,
        ev: JournalEventType,
    ) -> EventJournalEntry {
        let mut payload = serde_json::json!({ "session_id": session });
        if let Some(a) = agent {
            payload["agent_id"] = serde_json::json!(a);
        }
        EventJournalEntry {
            seq,
            timestamp_ms: seq,
            source: JournalSource::Hook,
            event_id: None,
            entity_id: Some(session.to_string()),
            event_type: ev,
            payload,
            result: None,
        }
    }

    #[test]
    fn launch_argv_shape() {
        #[cfg(unix)]
        {
            let argv = launch_argv("Ubuntu-24.04");
            assert_eq!(argv, vec!["t-hub-agent", "--stdio"]);
        }
        #[cfg(windows)]
        {
            // Default Windows path: execute the deployed helper directly.
            std::env::remove_var("T_HUB_AGENT_BIN");
            let argv = launch_argv("Ubuntu-24.04");
            assert_eq!(
                argv,
                vec![
                    "wsl.exe",
                    "-d",
                    "Ubuntu-24.04",
                    "--cd",
                    "~",
                    "-e",
                    "bash",
                    "-lc",
                    "exec $HOME/.local/bin/t-hub-agent --stdio",
                ]
            );

            // Escape hatch: T_HUB_AGENT_BIN is spawned verbatim (no wsl.exe).
            std::env::set_var("T_HUB_AGENT_BIN", "C:/tmp/t-hub-agent.exe");
            let argv = launch_argv("Ubuntu-24.04");
            assert_eq!(argv, vec!["C:/tmp/t-hub-agent.exe", "--stdio"]);
            std::env::remove_var("T_HUB_AGENT_BIN");
        }
    }

    #[test]
    fn isolated_journal_is_forwarded_without_shell_interpolation() {
        assert_eq!(
            direct_agent_argv("t-hub-agent", Some(".t-hub-dev/journal dir")),
            vec![
                "t-hub-agent",
                "--journal-dir",
                ".t-hub-dev/journal dir",
                "--stdio",
            ]
        );
        assert_eq!(
            windows_agent_argv("Ubuntu-24.04", Some(".t-hub-dev/journal dir")),
            vec![
                "wsl.exe",
                "-d",
                "Ubuntu-24.04",
                "--cd",
                "~",
                "-e",
                "bash",
                "-lc",
                "exec $HOME/.local/bin/t-hub-agent --journal-dir \"$1\" --stdio",
                "t-hub-agent",
                ".t-hub-dev/journal dir",
            ]
        );
    }

    #[test]
    fn request_receive_errors_preserve_timeout_and_disconnect_categories() {
        assert!(matches!(
            classify_request_receive_error(7, mpsc::RecvTimeoutError::Timeout),
            AgentRequestError::TimedOut(message) if message.contains("id=7")
        ));
        assert!(matches!(
            classify_request_receive_error(9, mpsc::RecvTimeoutError::Disconnected),
            AgentRequestError::Disconnected(message) if message.contains("id=9")
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ready_timeout_terminates_helper_and_discards_transport() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-stalled-agent-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("stalled-agent");
        let pid_file = temp_dir.join("pid");
        std::fs::write(
            &helper,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$T_HUB_TEST_PID_FILE\"\nwhile IFS= read -r _; do :; done\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let pid_file_env = TestEnvVar::set("T_HUB_TEST_PID_FILE", &pid_file);
        let bridge = AgentBridge::new();
        let error = bridge
            .connect_with_timeouts(
                "ignored",
                std::time::Duration::from_millis(300),
                std::time::Duration::from_millis(300),
            )
            .unwrap_err();
        drop(pid_file_env);
        drop(agent_bin_env);
        drop(env_lock);

        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(error.contains("timed out waiting for Ready"));
        assert_eq!(bridge.state(), ConnectionState::Failed);
        assert!(bridge.inner.transport.lock().is_none());
        assert!(
            !std::path::Path::new("/proc").join(pid.trim()).exists(),
            "stalled helper process {pid} survived the failed handshake"
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn incomplete_replay_terminates_helper_and_fails_connection() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-incomplete-replay-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("incomplete-replay-agent");
        let pid_file = temp_dir.join("pid");
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
printf '%s' "$$" > "$T_HUB_TEST_PID_FILE"
IFS= read -r _
printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{PROTOCOL_VERSION},"agent_version":"test","journal_head_seq":10}}'
IFS= read -r _
printf '%s\n' '{{"channel":"control","type":"replay_complete","last_seq":10}}'
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let pid_file_env = TestEnvVar::set("T_HUB_TEST_PID_FILE", &pid_file);
        let bridge = AgentBridge::new();
        let result = bridge.connect_with_timeouts(
            "ignored",
            std::time::Duration::from_millis(300),
            std::time::Duration::from_millis(300),
        );
        let state = bridge.state();
        let has_transport = bridge.inner.transport.lock().is_some();
        if result.is_ok() {
            bridge.disconnect();
        }
        drop(pid_file_env);
        drop(agent_bin_env);
        drop(env_lock);

        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let error = result.unwrap_err();
        assert!(error.contains("incomplete journal replay"));
        assert_eq!(state, ConnectionState::Failed);
        assert!(!has_transport);
        assert!(
            !std::path::Path::new("/proc").join(pid.trim()).exists(),
            "incomplete replay helper process {pid} survived the failed handshake"
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn compacted_replay_boundary_advances_cursor_without_event() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-compacted-replay-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("compacted-replay-agent");
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
IFS= read -r _
printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{PROTOCOL_VERSION},"agent_version":"test","journal_head_seq":10}}'
IFS= read -r _
printf '%s\n' '{{"channel":"events","type":"replay_boundary","last_seq":10}}'
printf '%s\n' '{{"channel":"events","type":"replay_complete","last_seq":10}}'
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let bridge = AgentBridge::new();
        bridge
            .connect_with_timeouts(
                "ignored",
                std::time::Duration::from_millis(300),
                std::time::Duration::from_millis(300),
            )
            .unwrap();
        assert_eq!(bridge.state(), ConnectionState::Live);
        assert_eq!(bridge.journal_cursor(), 10);
        bridge.disconnect();
        drop(agent_bin_env);
        drop(env_lock);

        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replay_only_frame_after_completion_fails_the_live_transport() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-late-replay-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("late-replay-agent");
        let pid_file = temp_dir.join("pid");
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
printf '%s' "$$" > "$T_HUB_TEST_PID_FILE"
IFS= read -r _
printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{PROTOCOL_VERSION},"agent_version":"test","journal_head_seq":1}}'
IFS= read -r _
printf '%s\n' '{{"channel":"events","type":"journal","seq":1,"entry":{{"seq":1,"timestamp_ms":1,"source":"agent","entity_id":"verified-session","event_type":"user_prompt_submit","payload":{{"session_id":"verified-session","prompt":"Verified"}}}},"replayed":true}}'
printf '%s\n' '{{"channel":"events","type":"replay_boundary","last_seq":1}}'
printf '%s\n' '{{"channel":"events","type":"replay_complete","last_seq":1}}'
sleep 0.1
printf '%s\n' '{{"channel":"events","type":"journal","seq":2,"entry":{{"seq":2,"timestamp_ms":2,"source":"agent","entity_id":"injected-session","event_type":"user_prompt_submit","payload":{{"session_id":"injected-session","prompt":"Must not commit"}}}},"replayed":true}}'
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let pid_file_env = TestEnvVar::set("T_HUB_TEST_PID_FILE", &pid_file);
        let bridge = AgentBridge::new();
        bridge
            .connect_with_timeouts(
                "ignored",
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while bridge.state() == ConnectionState::Live && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let state = bridge.state();
        let cursor = bridge.journal_cursor();
        let injected = bridge
            .with_supervisor(|supervisor| supervisor.tree("injected-session"))
            .is_some();
        let has_transport = bridge.inner.transport.lock().is_some();
        if has_transport {
            bridge.disconnect();
        }
        drop(pid_file_env);
        drop(agent_bin_env);
        drop(env_lock);

        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert_eq!(state, ConnectionState::Failed);
        assert_eq!(cursor, 1);
        assert!(!injected);
        assert!(!has_transport);
        assert!(
            !std::path::Path::new("/proc").join(pid.trim()).exists(),
            "protocol-violating helper process {pid} survived transport failure"
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn protocol_mismatch_terminates_helper_and_fails_connection() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-protocol-mismatch-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("protocol-mismatch-agent");
        let pid_file = temp_dir.join("pid");
        let incompatible_version = PROTOCOL_VERSION.saturating_add(1);
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
printf '%s' "$$" > "$T_HUB_TEST_PID_FILE"
IFS= read -r _
printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{incompatible_version},"agent_version":"test","journal_head_seq":0}}'
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let pid_file_env = TestEnvVar::set("T_HUB_TEST_PID_FILE", &pid_file);
        let bridge = AgentBridge::new();
        let result = bridge.connect_with_timeouts(
            "ignored",
            std::time::Duration::from_millis(300),
            std::time::Duration::from_millis(300),
        );
        let state = bridge.state();
        let has_transport = bridge.inner.transport.lock().is_some();
        if result.is_ok() {
            bridge.disconnect();
        }
        drop(pid_file_env);
        drop(agent_bin_env);
        drop(env_lock);

        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let error = result.unwrap_err();
        assert!(error.contains("agent protocol version mismatch"));
        assert!(error.contains(&format!("expected {PROTOCOL_VERSION}")));
        assert!(error.contains(&format!("received {incompatible_version}")));
        assert_eq!(state, ConnectionState::Failed);
        assert!(!has_transport);
        assert!(
            !std::path::Path::new("/proc").join(pid.trim()).exists(),
            "protocol mismatch helper process {pid} survived the failed handshake"
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn concurrent_connects_publish_only_one_authoritative_transport() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-concurrent-connect-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("concurrent-connect-agent");
        let pid_file = temp_dir.join("pids");
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$$" >> "$T_HUB_TEST_PID_FILE"
IFS= read -r _
sleep 0.2
printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{PROTOCOL_VERSION},"agent_version":"test","journal_head_seq":0}}'
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let pid_file_env = TestEnvVar::set("T_HUB_TEST_PID_FILE", &pid_file);
        let bridge = AgentBridge::new();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let attempts = (0..2)
            .map(|_| {
                let bridge = bridge.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    bridge.connect_with_timeouts(
                        "ignored",
                        std::time::Duration::from_secs(1),
                        std::time::Duration::from_secs(1),
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = attempts
            .into_iter()
            .map(|attempt| attempt.join().unwrap())
            .collect::<Vec<_>>();
        let state = bridge.state();
        let has_transport = bridge.inner.transport.lock().is_some();
        bridge.disconnect();
        drop(pid_file_env);
        drop(agent_bin_env);
        drop(env_lock);

        let pids = std::fs::read_to_string(&pid_file)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter_map(|result| result.as_ref().err())
                .filter(|error| error.contains("already connected"))
                .count(),
            1
        );
        assert_eq!(pids.len(), 1, "only one helper may be spawned: {pids:?}");
        assert_eq!(state, ConnectionState::Live);
        assert!(has_transport);
        assert!(
            pids.iter()
                .all(|pid| !std::path::Path::new("/proc").join(pid).exists()),
            "authoritative helper survived disconnect: {pids:?}"
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_reconnect_discards_the_replacement_transport() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-failed-reconnect-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("replacement-agent");
        let attempts_file = temp_dir.join("attempts");
        let pid_file = temp_dir.join("pids");
        let incompatible_version = PROTOCOL_VERSION.saturating_add(1);
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$$" >> "$T_HUB_TEST_PID_FILE"
attempts=0
if [ -f "$T_HUB_TEST_ATTEMPTS_FILE" ]; then
  attempts=$(cat "$T_HUB_TEST_ATTEMPTS_FILE")
fi
attempts=$((attempts + 1))
printf '%s' "$attempts" > "$T_HUB_TEST_ATTEMPTS_FILE"
IFS= read -r _
if [ "$attempts" -eq 1 ]; then
  printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{PROTOCOL_VERSION},"agent_version":"test","journal_head_seq":0}}'
else
  printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{incompatible_version},"agent_version":"test","journal_head_seq":0}}'
fi
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let pid_file_env = TestEnvVar::set("T_HUB_TEST_PID_FILE", &pid_file);
        let attempts_file_env = TestEnvVar::set("T_HUB_TEST_ATTEMPTS_FILE", &attempts_file);
        let bridge = AgentBridge::new();
        bridge
            .connect_with_timeouts(
                "ignored",
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(bridge.state(), ConnectionState::Live);

        let error = bridge.reconnect("ignored").unwrap_err();
        let state = bridge.state();
        let has_transport = bridge.inner.transport.lock().is_some();
        drop(attempts_file_env);
        drop(pid_file_env);
        drop(agent_bin_env);
        drop(env_lock);

        let pids = std::fs::read_to_string(&pid_file)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(error.contains("agent protocol version mismatch"));
        assert_eq!(pids.len(), 2, "expected original and replacement helpers");
        assert_eq!(state, ConnectionState::Failed);
        assert!(!has_transport);
        assert!(
            pids.iter()
                .all(|pid| !std::path::Path::new("/proc").join(pid).exists()),
            "a helper survived the failed replacement: {pids:?}"
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn malformed_frame_fails_an_active_journal_replay() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("t-hub-malformed-replay-{unique}"));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let helper = temp_dir.join("malformed-replay-agent");
        std::fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
IFS= read -r _
printf '%s\n' '{{"channel":"control","type":"ready","protocol_version":{PROTOCOL_VERSION},"agent_version":"test","journal_head_seq":1}}'
IFS= read -r _
printf '%s\n' '{{"channel":"events","type":"journal","seq":1,"entry":{{"seq":1,"timestamp_ms":1,"source":"agent","entity_id":"failed-session","event_type":"user_prompt_submit","payload":{{"session_id":"failed-session","prompt":"Must not commit"}}}},"replayed":true}}'
printf '%s\n' '{{malformed'
printf '%s\n' '{{"channel":"control","type":"replay_boundary","last_seq":1}}'
printf '%s\n' '{{"channel":"control","type":"replay_complete","last_seq":1}}'
while IFS= read -r _; do :; done
"#
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &helper);
        let bridge = AgentBridge::new();
        let error = bridge
            .connect_with_timeouts(
                "ignored",
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1),
            )
            .unwrap_err();
        drop(agent_bin_env);
        drop(env_lock);

        assert!(error.contains("incomplete journal replay: malformed agent frame"));
        assert_eq!(bridge.state(), ConnectionState::Failed);
        assert_eq!(bridge.journal_cursor(), 0);
        assert!(bridge
            .with_supervisor(|supervisor| supervisor.tree("failed-session"))
            .is_none());
        assert!(bridge.inner.transport.lock().is_none());
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    fn git_info_response_reports_unsupported_agent_capability() {
        let error = super::map_git_info_response(AgentResponse::Error {
            kind: ResponseErrorKind::Unsupported,
            message: "unsupported request op".to_string(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            GitInfoBridgeError::Unsupported("unsupported request op".to_string())
        );
    }

    #[test]
    fn git_info_response_reports_agent_command_failure() {
        let error = super::map_git_info_response(AgentResponse::Error {
            kind: ResponseErrorKind::CommandFailed,
            message: "git timed out".to_string(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            GitInfoBridgeError::CommandFailed("CommandFailed: git timed out".to_string())
        );
    }

    #[test]
    fn terminal_snapshot_response_reports_unsupported_agent_capability() {
        let error = super::map_terminal_snapshot_response(AgentResponse::Error {
            kind: ResponseErrorKind::Unsupported,
            message: "unsupported request op".to_string(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            TerminalSnapshotBridgeError::Unsupported("unsupported request op".to_string())
        );
    }

    #[test]
    fn terminal_snapshot_response_reports_agent_command_failure() {
        let error = super::map_terminal_snapshot_response(AgentResponse::Error {
            kind: ResponseErrorKind::CommandFailed,
            message: "tmux timed out".to_string(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            TerminalSnapshotBridgeError::CommandFailed("CommandFailed: tmux timed out".to_string())
        );
    }

    #[test]
    fn live_stdio_git_info_round_trip_with_real_agent() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let agent_bin = manifest.join("target/debug/t-hub-agent");
        if !agent_bin.exists() {
            eprintln!(
                "live_stdio_git_info_round_trip_with_real_agent: binary missing; run cargo build -p t-hub-agent"
            );
            return;
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repo = std::env::temp_dir().join(format!("t-hub-bridge-git-info-{unique}"));
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "T-Hub Test"],
            vec!["config", "user.email", "t-hub@example.test"],
        ] {
            assert!(std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(repo.join("tracked.txt"), "initial\n").unwrap();
        assert!(std::process::Command::new("git")
            .current_dir(&repo)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-m", "initial"])
            .status()
            .unwrap()
            .success());
        std::fs::write(repo.join("tracked.txt"), "changed\n").unwrap();

        let env_lock = AGENT_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let journal = tempfile::tempdir().expect("create private agent journal");
        let agent_bin_env = TestEnvVar::set("T_HUB_AGENT_BIN", &agent_bin);
        let journal_env = TestEnvVar::set("T_HUB_AGENT_JOURNAL_DIR", journal.path());
        let bridge = AgentBridge::new();
        bridge.connect("ignored").expect("real agent must connect");
        drop(journal_env);
        drop(agent_bin_env);
        drop(env_lock);
        let info = bridge
            .git_info(repo.to_str().unwrap())
            .expect("real stdio GitInfo request must succeed");
        assert!(info.is_repo);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.worktree_root.as_deref(), repo.to_str());
        assert_eq!(info.dirty_count, 1);
        assert!(info.head_commit.is_some());
        bridge.disconnect();
        std::fs::remove_dir_all(repo).ok();
    }

    #[test]
    fn consume_journal_advances_cursor_and_feeds_supervision() {
        let bridge = AgentBridge::new();
        assert_eq!(bridge.journal_cursor(), 0);

        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(2, "o1", Some("a1"), JournalEventType::SubagentStart));
        let affected = bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::Stop));
        assert_eq!(affected.as_deref(), Some("o1"));
        assert_eq!(bridge.journal_cursor(), 3);

        // Supervision saw the events → WaitingOnSubagents.
        let tree = bridge.with_supervisor(|s| s.tree("o1")).unwrap();
        assert_eq!(tree.status, crate::model::SessionStatus::WaitingOnSubagents);
        assert_eq!(tree.children.len(), 1);
    }

    #[test]
    fn cursor_does_not_regress_on_out_of_order_seq() {
        let bridge = AgentBridge::new();
        bridge.consume_journal_entry(&entry(5, "o1", None, JournalEventType::SessionStart));
        assert_eq!(bridge.journal_cursor(), 5);
        // A late/duplicate lower seq must not move the cursor backwards.
        assert!(bridge
            .consume_journal_entry(&entry(3, "o1", None, JournalEventType::UserPromptSubmit))
            .is_none());
        assert_eq!(bridge.journal_cursor(), 5);
    }

    #[test]
    fn replay_restart_and_late_session_start_cannot_clear_a_permission_need() {
        fn permission(seq: u64) -> EventJournalEntry {
            EventJournalEntry {
                seq,
                timestamp_ms: seq,
                source: JournalSource::Agent,
                event_id: None,
                entity_id: Some("thread-1".to_string()),
                event_type: JournalEventType::PermissionRequest,
                payload: serde_json::json!({
                    "provider": "codex",
                    "session_id": "thread-1",
                    "turn_id": "turn-1",
                    "lifecycle": "permission_requested",
                    "permission_request_id": "request-1",
                    "permission_request": {
                        "schema_version": "t-hub.permission-request.v1",
                        "id": "request-1",
                        "kind": "command_execution",
                        "provider": "codex",
                        "provider_request_id": "request-1",
                        "session_id": "thread-1",
                        "turn_id": "turn-1",
                        "item_id": "item-1",
                        "tool_name": "Bash",
                        "requested_at_ms": 2
                    },
                    "telemetry": {
                        "transport": "structured",
                        "quality": "authoritative",
                        "runtime_health": "ready"
                    }
                }),
                result: None,
            }
        }

        let replay = [
            entry(1, "thread-1", None, JournalEventType::SessionStart),
            permission(2),
        ];
        for restarted in [false, true] {
            let bridge = AgentBridge::new();
            let rec = RecordingEmitter::default();
            bridge.set_emitter(Arc::new(rec.clone()));
            rec.events.lock().clear();
            for journal_entry in &replay {
                bridge.consume_journal_entry(journal_entry);
            }
            assert_eq!(
                bridge.with_supervisor(|supervisor| supervisor.status("thread-1")),
                crate::model::SessionStatus::NeedsPermission,
                "replay must restore the permission need (restarted={restarted})"
            );
            assert!(bridge
                .with_supervisor(|supervisor| supervisor.permission_request("thread-1"))
                .is_some());

            let emitted_before_late = rec.events.lock().len();
            let late_start = entry(1, "thread-1", None, JournalEventType::SessionStart);
            assert!(bridge.consume_journal_entry(&late_start).is_none());
            let duplicate_start = entry(2, "thread-1", None, JournalEventType::SessionStart);
            assert!(bridge.consume_journal_entry(&duplicate_start).is_none());
            assert_eq!(rec.events.lock().len(), emitted_before_late);
            assert_eq!(
                bridge.with_supervisor(|supervisor| supervisor.status("thread-1")),
                crate::model::SessionStatus::NeedsPermission
            );
            assert!(bridge
                .with_supervisor(|supervisor| supervisor.permission_request("thread-1"))
                .is_some());
        }
    }

    /// A recording emitter for the live-emit tests: captures (channel, payload).
    #[derive(Default, Clone)]
    struct RecordingEmitter {
        events: Arc<parking_lot::Mutex<Vec<(String, serde_json::Value)>>>,
    }
    impl super::EventEmitter for RecordingEmitter {
        fn emit_json(&self, channel: &str, payload: &serde_json::Value) {
            self.events
                .lock()
                .push((channel.to_string(), payload.clone()));
        }
    }

    #[test]
    fn consume_emits_journal_supervision_and_status_live() {
        // The #1 0.5 gap: consuming a journal entry must fan out live events.
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));

        // set_emitter pushes an initial agent://state.
        {
            let evs = rec.events.lock();
            assert_eq!(evs.len(), 1, "set_emitter should emit one agent://state");
            assert_eq!(evs[0].0, super::EVT_AGENT_STATE);
        }
        rec.events.lock().clear();

        // SessionStart → Working: expect journal + agent://state (cursor moved) +
        // supervision://tree + session://status.
        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        let channels: Vec<String> = rec.events.lock().iter().map(|(c, _)| c.clone()).collect();
        assert!(
            channels.contains(&super::EVT_JOURNAL.to_string()),
            "journal: {channels:?}"
        );
        assert!(
            channels.contains(&super::EVT_AGENT_STATE.to_string()),
            "state: {channels:?}"
        );
        assert!(
            channels.contains(&super::EVT_SUPERVISION.to_string()),
            "tree: {channels:?}"
        );
        assert!(
            channels.contains(&super::EVT_SESSION_STATUS.to_string()),
            "status: {channels:?}"
        );

        // The session://status payload must carry the camelCase status string.
        let status_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(status_ev.1["sessionId"], "o1");
        assert_eq!(status_ev.1["status"], "working");

        // The supervision://tree payload must carry the session + status.
        let tree_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_SUPERVISION)
            .cloned()
            .unwrap();
        assert_eq!(tree_ev.1["sessionId"], "o1");
        assert_eq!(tree_ev.1["status"], "working");
    }

    #[test]
    fn interleaved_replay_and_live_frames_keep_per_frame_provenance() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();

        bridge.consume_journal_entry_with_provenance(
            &entry(1, "o1", None, JournalEventType::PermissionRequest),
            true,
        );
        bridge.consume_journal_entry_with_provenance(
            &entry(2, "o1", None, JournalEventType::Elicitation),
            false,
        );
        bridge.flush_replay();

        let journal_flags = rec
            .events
            .lock()
            .iter()
            .filter(|(channel, _)| channel == super::EVT_JOURNAL)
            .map(|(_, payload)| payload["replayed"].as_bool().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            journal_flags,
            vec![false],
            "historical replay stays silent while an interleaved live frame stays live"
        );
    }

    #[test]
    fn cold_replay_coalesces_large_history_into_bounded_session_snapshots() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status));
        rec.events.lock().clear();

        let mut seq = 0;
        for iteration in 0..2_000 {
            seq += 1;
            bridge.consume_journal_entry_with_provenance(
                &EventJournalEntry {
                    seq,
                    timestamp_ms: seq,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "cwd": "/workspace/project",
                        "prompt": format!("Replay prompt {iteration}")
                    }),
                    result: None,
                },
                true,
            );
            seq += 1;
            bridge.consume_journal_entry_with_provenance(
                &EventJournalEntry {
                    seq,
                    timestamp_ms: seq,
                    source: JournalSource::Status,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::StatusSnapshot,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "status": {
                            "context_window": {
                                "used_percentage": iteration as f64 / 20.0
                            }
                        }
                    }),
                    result: None,
                },
                true,
            );
        }

        assert!(
            rec.events.lock().is_empty(),
            "historical replay must not emit per-entry UI events"
        );
        assert_eq!(bridge.journal_cursor(), 4_000);

        bridge.flush_replay();
        let events = rec.events.lock().clone();
        let channels = events
            .iter()
            .map(|(channel, _)| channel.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            channels
                .iter()
                .filter(|channel| **channel == super::EVT_TITLE)
                .count(),
            1
        );
        assert_eq!(
            channels
                .iter()
                .filter(|channel| **channel == super::EVT_STATUS_SNAPSHOT)
                .count(),
            1
        );
        assert_eq!(
            channels
                .iter()
                .filter(|channel| **channel == super::EVT_SESSION_STATUS)
                .count(),
            1
        );
        assert_eq!(
            channels
                .iter()
                .filter(|channel| **channel == super::EVT_JOURNAL)
                .count(),
            0
        );
        assert!(
            events.len() <= 4,
            "4,000 replay entries must coalesce to at most four UI events, got {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .find(|(channel, _)| channel == super::EVT_TITLE)
                .unwrap()
                .1["title"],
            "Replay prompt 1999"
        );
        assert_eq!(
            status.get("session-1").unwrap().context_used_pct,
            Some(99.95)
        );
    }

    #[test]
    fn replay_boundary_orders_historical_publication_before_buffered_live_frames() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();
        let journal_flow = ReaderJournalFlow::new();
        journal_flow.begin_replay(0);

        journal_flow
            .ingest(
                &bridge,
                3,
                EventJournalEntry {
                    seq: 3,
                    timestamp_ms: 3,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "prompt": "Live title"
                    }),
                    result: None,
                },
                false,
            )
            .unwrap();
        journal_flow
            .ingest(
                &bridge,
                1,
                EventJournalEntry {
                    seq: 1,
                    timestamp_ms: 1,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "prompt": "Recovered title"
                    }),
                    result: None,
                },
                true,
            )
            .unwrap();

        assert_eq!(bridge.journal_cursor(), 0);
        assert!(rec.events.lock().is_empty());

        journal_flow.observe_replay_boundary(1).unwrap();
        assert_eq!(journal_flow.finish_replay(1), Ok(1));
        journal_flow.complete_replay(&bridge, 1).unwrap();

        let titles = rec
            .events
            .lock()
            .iter()
            .filter(|(channel, _)| channel == super::EVT_TITLE)
            .map(|(_, payload)| payload["title"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["Recovered title", "Live title"]);
        assert_eq!(bridge.journal_cursor(), 3);
        assert_eq!(bridge.state(), ConnectionState::Live);
    }

    #[test]
    fn replay_buffers_all_effects_until_verification() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();
        let journal_flow = ReaderJournalFlow::new();
        journal_flow.begin_replay(0);

        journal_flow
            .ingest(
                &bridge,
                1,
                EventJournalEntry {
                    seq: 1,
                    timestamp_ms: 1,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "prompt": "Recovered title"
                    }),
                    result: None,
                },
                true,
            )
            .unwrap();
        assert_eq!(bridge.journal_cursor(), 0);
        assert!(rec.events.lock().is_empty());

        journal_flow
            .ingest(
                &bridge,
                2,
                EventJournalEntry {
                    seq: 2,
                    timestamp_ms: 2,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "prompt": "Live title"
                    }),
                    result: None,
                },
                false,
            )
            .unwrap();
        assert_eq!(bridge.journal_cursor(), 0);
        assert!(bridge
            .with_supervisor(|supervisor| supervisor.tree("session-1"))
            .is_none());

        journal_flow.observe_replay_boundary(1).unwrap();
        assert_eq!(journal_flow.finish_replay(1), Ok(1));
        journal_flow.complete_replay(&bridge, 1).unwrap();

        let titles = rec
            .events
            .lock()
            .iter()
            .filter(|(channel, _)| channel == super::EVT_TITLE)
            .map(|(_, payload)| payload["title"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["Recovered title", "Live title"]);
        assert_eq!(bridge.journal_cursor(), 2);
    }

    #[test]
    fn cancelled_replay_cannot_publish_or_restore_live_state() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();
        let journal_flow = ReaderJournalFlow::new();
        journal_flow.begin_replay(0);
        bridge.set_state(ConnectionState::Replaying);

        journal_flow
            .ingest(
                &bridge,
                1,
                EventJournalEntry {
                    seq: 1,
                    timestamp_ms: 1,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "prompt": "Recovered title"
                    }),
                    result: None,
                },
                true,
            )
            .unwrap();

        assert!(journal_flow.cancel());
        bridge.set_state(ConnectionState::Failed);
        assert!(journal_flow.complete_replay(&bridge, 1).is_err());

        assert_eq!(bridge.state(), ConnectionState::Failed);
        assert_eq!(bridge.journal_cursor(), 0);
        assert!(bridge
            .with_supervisor(|supervisor| supervisor.tree("session-1"))
            .is_none());
        assert!(rec
            .events
            .lock()
            .iter()
            .all(|(channel, _)| channel != super::EVT_TITLE));
    }

    #[test]
    fn completed_replay_wins_a_simultaneous_timeout_check() {
        let bridge = AgentBridge::new();
        let journal_flow = ReaderJournalFlow::new();
        journal_flow.begin_replay(0);

        journal_flow.observe_replay_boundary(0).unwrap();
        assert_eq!(journal_flow.finish_replay(0), Ok(0));
        journal_flow.complete_replay(&bridge, 0).unwrap();

        assert!(!journal_flow.cancel());
        assert_eq!(bridge.state(), ConnectionState::Live);
    }

    #[test]
    fn retired_reader_flow_cannot_mutate_the_shared_bridge() {
        let bridge = AgentBridge::new();
        let journal_flow = ReaderJournalFlow::new();
        journal_flow.complete_without_replay(&bridge);
        journal_flow.retire();

        journal_flow
            .ingest(
                &bridge,
                1,
                EventJournalEntry {
                    seq: 1,
                    timestamp_ms: 1,
                    source: JournalSource::Agent,
                    event_id: None,
                    entity_id: Some("session-1".to_string()),
                    event_type: JournalEventType::UserPromptSubmit,
                    payload: serde_json::json!({
                        "session_id": "session-1",
                        "prompt": "Superseded title"
                    }),
                    result: None,
                },
                false,
            )
            .unwrap();

        assert_eq!(bridge.journal_cursor(), 0);
        assert!(bridge
            .with_supervisor(|supervisor| supervisor.tree("session-1"))
            .is_none());
    }

    #[test]
    fn codex_permission_lifecycle_emits_typed_need_health_and_tile_binding() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status_bridge = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status_bridge));
        rec.events.lock().clear();

        let make_entry = |seq, event_type, payload| EventJournalEntry {
            seq,
            timestamp_ms: seq * 10,
            source: JournalSource::Agent,
            event_id: None,
            entity_id: Some("thread-1".to_string()),
            event_type,
            payload,
            result: None,
        };
        let request = make_entry(
            1,
            JournalEventType::PermissionRequest,
            serde_json::json!({
                "provider": "codex",
                "provider_version": "0.144.4",
                "session_id": "thread-1",
                "turn_id": "turn-1",
                "lifecycle": "permission_requested",
                "cwd": "/worktree",
                "tmux_session": "th_crew0001",
                "permission_request_id": "request-1",
                "permission_request": {
                    "schema_version": "t-hub.permission-request.v1",
                    "id": "request-1",
                    "kind": "command_execution",
                    "provider": "codex",
                    "provider_request_id": "request-1",
                    "session_id": "thread-1",
                    "turn_id": "turn-1",
                    "item_id": "item-1",
                    "tool_name": "Bash",
                    "requested_at_ms": 10
                },
                "telemetry": {
                    "transport": "structured",
                    "quality": "authoritative",
                    "runtime_health": "ready"
                }
            }),
        );
        bridge.consume_journal_entry(&request);

        let permission_status = rec
            .events
            .lock()
            .iter()
            .rfind(|(channel, _)| channel == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(permission_status.1["status"], "needsPermission");
        assert_eq!(
            permission_status.1["permissionRequest"]["providerRequestId"],
            "request-1"
        );
        assert_eq!(permission_status.1["runtimeHealth"]["health"], "ready");
        assert_eq!(
            status_bridge.terminal_for_session("thread-1").as_deref(),
            Some("crew0001")
        );

        let disconnected = make_entry(
            2,
            JournalEventType::CoreAction,
            serde_json::json!({
                "provider": "codex",
                "session_id": "thread-1",
                "lifecycle": "telemetry_health",
                "telemetry": {
                    "transport": "structured",
                    "quality": "stale",
                    "runtime_health": "disconnected",
                    "detail": "structured_stream_ended_mid_turn"
                }
            }),
        );
        bridge.consume_journal_entry(&disconnected);
        let degraded_status = rec
            .events
            .lock()
            .iter()
            .rfind(|(channel, _)| channel == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(degraded_status.1["status"], "needsPermission");
        assert_eq!(degraded_status.1["runtimeHealth"]["health"], "disconnected");

        let resolved = make_entry(
            3,
            JournalEventType::CoreAction,
            serde_json::json!({
                "provider": "codex",
                "session_id": "thread-1",
                "lifecycle": "permission_resolved",
                "permission_request_id": "request-1",
                "telemetry": {
                    "transport": "structured",
                    "quality": "authoritative",
                    "runtime_health": "ready"
                }
            }),
        );
        bridge.consume_journal_entry(&resolved);
        let resolved_status = rec
            .events
            .lock()
            .iter()
            .rfind(|(channel, _)| channel == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(resolved_status.1["status"], "working");
        assert!(resolved_status.1.get("permissionRequest").is_none());
    }

    #[test]
    fn malformed_codex_permission_cannot_cross_clear_a_valid_request() {
        let bridge = AgentBridge::new();
        let request_id = "p".repeat(512);
        let entry = |seq, event_type, payload| EventJournalEntry {
            seq,
            timestamp_ms: seq * 10,
            source: JournalSource::Agent,
            event_id: None,
            entity_id: Some("thread-1".to_string()),
            event_type,
            payload,
            result: None,
        };
        bridge.consume_journal_entry(&entry(
            1,
            JournalEventType::PermissionRequest,
            serde_json::json!({
                "provider": "codex",
                "session_id": "thread-1",
                "turn_id": "turn-1",
                "lifecycle": "permission_requested",
                "permission_request_id": request_id,
                "permission_request": {
                    "schema_version": "t-hub.permission-request.v1",
                    "id": request_id,
                    "kind": "command_execution",
                    "provider": "codex",
                    "provider_request_id": request_id,
                    "session_id": "thread-1",
                    "turn_id": "turn-1",
                    "item_id": "item-1",
                    "tool_name": "Bash",
                    "requested_at_ms": 10
                },
                "telemetry": {
                    "transport": "structured",
                    "quality": "authoritative",
                    "runtime_health": "ready"
                }
            }),
        ));
        bridge.consume_journal_entry(&entry(
            2,
            JournalEventType::PermissionRequest,
            serde_json::json!({
                "provider": "codex",
                "session_id": "thread-1",
                "turn_id": "turn-1",
                "lifecycle": "permission_requested",
                "permission_observation": {
                    "schema_version": "t-hub.permission-request.v1",
                    "kind": "command_execution",
                    "provider": "codex",
                    "valid": false
                },
                "telemetry": {
                    "transport": "structured",
                    "quality": "stale",
                    "runtime_health": "degraded",
                    "detail": "invalid_permission_request_identity"
                }
            }),
        ));
        bridge.consume_journal_entry(&entry(
            3,
            JournalEventType::CoreAction,
            serde_json::json!({
                "provider": "codex",
                "session_id": "thread-1",
                "turn_id": "turn-1",
                "lifecycle": "telemetry_health",
                "telemetry": {
                    "transport": "structured",
                    "quality": "stale",
                    "runtime_health": "degraded",
                    "detail": "invalid_permission_resolution_identity"
                }
            }),
        ));

        assert_eq!(
            bridge.with_supervisor(|supervisor| supervisor.status("thread-1")),
            crate::model::SessionStatus::NeedsPermission
        );
        assert!(bridge
            .with_supervisor(|supervisor| supervisor.permission_request("thread-1"))
            .is_none());
        assert_eq!(
            bridge
                .with_supervisor(|supervisor| supervisor.runtime_health("thread-1"))
                .unwrap()
                .health,
            crate::supervision::RuntimeHealth::Degraded
        );

        let replayed = entry(3, JournalEventType::SessionStart, serde_json::json!({}));
        assert!(bridge.consume_journal_entry(&replayed).is_none());
        assert_eq!(
            bridge.with_supervisor(|supervisor| supervisor.status("thread-1")),
            crate::model::SessionStatus::NeedsPermission
        );

        bridge.consume_journal_entry(&entry(
            4,
            JournalEventType::UserPromptSubmit,
            serde_json::json!({}),
        ));
        assert_eq!(
            bridge.with_supervisor(|supervisor| supervisor.status("thread-1")),
            crate::model::SessionStatus::Working
        );
    }

    #[test]
    fn unobserved_interactive_codex_is_degraded_never_false_working() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status_bridge = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status_bridge));
        rec.events.lock().clear();

        bridge.consume_journal_entry(&EventJournalEntry {
            seq: 1,
            timestamp_ms: 10,
            source: JournalSource::Agent,
            event_id: None,
            entity_id: Some("codex-unobserved:th_crew0001".to_string()),
            event_type: JournalEventType::AgentCommand,
            payload: serde_json::json!({
                "provider": "codex",
                "provider_version": "0.144.4",
                "session_id": "codex-unobserved:th_crew0001",
                "lifecycle": "telemetry_health",
                "cwd": "/worktree",
                "tmux_session": "th_crew0001",
                "telemetry": {
                    "transport": "unavailable",
                    "quality": "stale",
                    "runtime_health": "degraded",
                    "detail": "interactive_tui_lifecycle_unsupported"
                }
            }),
            result: None,
        });

        let status = rec
            .events
            .lock()
            .iter()
            .rfind(|(channel, _)| channel == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(status.1["status"], "unknown");
        assert_ne!(status.1["status"], "working");
        assert_eq!(status.1["runtimeHealth"]["health"], "degraded");
        assert_eq!(status.1["runtimeHealth"]["source"], "unavailable");
        assert_eq!(
            status_bridge
                .terminal_for_session("codex-unobserved:th_crew0001")
                .as_deref(),
            Some("crew0001")
        );
    }

    /// The fleet status observer fires on the REAL journal-consume path (the
    /// wiring the orchestrator wake depends on): every `emit_session` invokes the
    /// observer with `(session_uuid, status)`, and the terminal edge is the
    /// `Completed` the notifier wakes on. This closes the loop between the
    /// supervision reducer and `crate::fleet::FleetNotifier`.
    #[test]
    fn status_observer_fires_on_the_journal_consume_path() {
        use crate::model::SessionStatus;
        let bridge = AgentBridge::new();
        let seen: Arc<Mutex<Vec<(String, SessionStatus)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        bridge.set_status_observer(Arc::new(move |uuid: &str, status| {
            sink.lock().push((uuid.to_string(), status));
        }));

        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(2, "o1", None, JournalEventType::Stop));

        let got = seen.lock().clone();
        assert!(!got.is_empty(), "observer must fire on the emit path");
        assert!(
            got.iter().all(|(u, _)| u == "o1"),
            "always the affected session"
        );
        assert_eq!(
            got.last().map(|(_, s)| *s),
            Some(SessionStatus::Completed),
            "the terminal edge is Completed - what the fleet notifier wakes on"
        );
    }

    #[test]
    fn waiting_on_subagents_surfaces_via_session_status_emit() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();

        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(2, "o1", Some("a1"), JournalEventType::SubagentStart));
        // Main agent Stop while the subagent is still running → WaitingOnSubagents.
        bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::Stop));

        // The last session://status emit must be waitingOnSubagents (FR-012).
        let last_status = rec
            .events
            .lock()
            .iter()
            .rfind(|(c, _)| c == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(last_status.1["status"], "waitingOnSubagents");

        let stop_journal = rec
            .events
            .lock()
            .iter()
            .rfind(|(channel, payload)| {
                channel == super::EVT_JOURNAL && payload["entry"]["seq"] == 3
            })
            .cloned()
            .expect("Stop must emit its correlated journal payload");
        assert!(
            stop_journal.1.get("voice_announcement").is_none(),
            "Stop cannot announce completion while children are still running"
        );

        bridge.consume_journal_entry(&entry(4, "o1", Some("a1"), JournalEventType::SubagentStop));
        let drain_journal = rec
            .events
            .lock()
            .iter()
            .rfind(|(channel, payload)| {
                channel == super::EVT_JOURNAL && payload["entry"]["seq"] == 4
            })
            .cloned()
            .expect("the child drain must emit its correlated journal payload");
        assert_eq!(
            drain_journal.1["voice_announcement"],
            serde_json::json!({
                "kind": "completion",
                "sessionId": "o1",
                "status": "completed"
            }),
            "the exact drain event owns completion authority"
        );
    }

    #[test]
    fn consume_session_end_emits_terminal_status_not_unknown() {
        // REGRESSION (HIGH): evicting the session on `SessionEnd` made
        // `emit_session` read `status()` for an absent session — which defaults to
        // `Unknown` — so the UI's last `session://status` was `unknown` instead of
        // the real terminal status. The supervision reducer tests passed because
        // they assert on `ingest()`'s `status()` in isolation; this asserts on the
        // EMITTED payload from the `consume_journal_entry` → `emit_session` path.
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status_bridge = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status_bridge));
        rec.events.lock().clear();

        // Clean-completed path: a main-agent Stop with no outstanding subagents
        // classifies Completed, and SessionEnd keeps that terminal status.
        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        status_bridge.ingest("o1", &serde_json::json!({"cwd":"/repo"}), 1);
        bridge.consume_journal_entry(&entry(2, "o1", None, JournalEventType::Stop));
        bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::SessionEnd));
        assert!(status_bridge.get("o1").is_none());

        // The LAST session://status emit for this session — the one the UI renders
        // after the session ends — must be the terminal status, never `unknown`.
        let last_status = rec
            .events
            .lock()
            .iter()
            .rfind(|(c, _)| c == super::EVT_SESSION_STATUS)
            .cloned()
            .expect("a session://status must be emitted on SessionEnd");
        assert_eq!(last_status.1["sessionId"], "o1");
        assert_eq!(
            last_status.1["status"], "completed",
            "SessionEnd after a clean Stop must emit the terminal Completed status"
        );
        assert_ne!(
            last_status.1["status"], "unknown",
            "evicting on SessionEnd must never let emit_session broadcast Unknown"
        );

        // Abnormal path: a Stop with an outstanding subagent is WaitingOnSubagents,
        // so SessionEnd downgrades to Failed (non-Completed → Failed). Still never
        // `unknown` on the emitted payload.
        let rec2 = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec2.clone()));
        rec2.events.lock().clear();

        bridge.consume_journal_entry(&entry(4, "o2", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(5, "o2", Some("a1"), JournalEventType::SubagentStart));
        bridge.consume_journal_entry(&entry(6, "o2", None, JournalEventType::Stop));
        bridge.consume_journal_entry(&entry(7, "o2", None, JournalEventType::SessionEnd));

        let last_status2 = rec2
            .events
            .lock()
            .iter()
            .rfind(|(c, _)| c == super::EVT_SESSION_STATUS)
            .cloned()
            .expect("a session://status must be emitted on SessionEnd");
        assert_eq!(last_status2.1["sessionId"], "o2");
        assert_eq!(
            last_status2.1["status"], "failed",
            "SessionEnd while waiting on a subagent must emit the terminal Failed status"
        );
        assert_ne!(last_status2.1["status"], "unknown");
    }

    #[test]
    fn status_snapshot_journal_entry_routes_to_status_bridge_and_emits() {
        use t_hub_protocol::{EventJournalEntry, JournalSource};
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status));
        rec.events.lock().clear();

        // A StatusSnapshot journal entry whose payload carries the raw statusline.
        let entry = EventJournalEntry {
            seq: 1,
            timestamp_ms: 100,
            source: JournalSource::Status,
            event_id: None,
            entity_id: Some("o1".to_string()),
            event_type: JournalEventType::StatusSnapshot,
            payload: serde_json::json!({
                "session_id": "o1",
                "status": { "context_window": { "used_percentage": 55.0 } }
            }),
            result: None,
        };
        bridge.consume_journal_entry(&entry);

        // status://snapshot emitted with the derived context %.
        let snap_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_STATUS_SNAPSHOT)
            .cloned()
            .expect("status://snapshot must be emitted");
        assert_eq!(snap_ev.1["sessionId"], "o1");
        assert_eq!(snap_ev.1["contextUsedPct"], 55.0);
        // And the status bridge holds it (queryable via the command).
        assert_eq!(status.get("o1").unwrap().context_used_pct, Some(55.0));
    }

    #[test]
    fn status_snapshot_does_not_flood_journal_or_supervision_channels() {
        // FREEZE REGRESSION: the statusline re-journals a near-identical
        // StatusSnapshot ~25x/sec/session (only the timestamp ticks). Consuming one
        // must NOT fan out the journal/state/supervision/session channels — that was
        // a sustained ~hundreds/sec webview flood that pinned the UI ("constant
        // freeze") and locked up the window drag. Only `status://snapshot` may fire,
        // and only on a meaningful change; a no-op resend must emit NOTHING.
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status));
        rec.events.lock().clear();

        let snap = |seq: u64, ts: u64| EventJournalEntry {
            seq,
            timestamp_ms: ts,
            source: JournalSource::Status,
            event_id: None,
            entity_id: Some("o1".to_string()),
            event_type: JournalEventType::StatusSnapshot,
            payload: serde_json::json!({
                "session_id": "o1",
                "status": { "context_window": { "used_percentage": 42.0 } }
            }),
            result: None,
        };

        // First snapshot: exactly ONE emit, on status://snapshot. No journal /
        // agent://state / supervision://tree / session://status.
        bridge.consume_journal_entry(&snap(1, 100));
        let channels: Vec<String> = rec.events.lock().iter().map(|(c, _)| c.clone()).collect();
        assert_eq!(
            channels,
            vec![super::EVT_STATUS_SNAPSHOT.to_string()],
            "a status snapshot must emit ONLY status://snapshot, got {channels:?}"
        );
        rec.events.lock().clear();

        // Identical resend (only the timestamp ticks): ZERO emits.
        bridge.consume_journal_entry(&snap(2, 200));
        let after = rec.events.lock().clone();
        assert!(
            after.is_empty(),
            "a no-op status snapshot resend must emit nothing, got {after:?}"
        );
        // ...but the replay cursor still advanced (durability is preserved).
        assert_eq!(bridge.journal_cursor(), 2);
    }

    // -----------------------------------------------------------------------
    // GOAL NAMES: derive_session_title + agent://title emit
    // -----------------------------------------------------------------------

    #[test]
    fn user_prompt_submit_titles_from_first_prompt_line() {
        let p = serde_json::json!({
            "session_id": "s1",
            "prompt": "Fix the WSL hooks install path\n\nlots of detail follows"
        });
        let t = super::derive_session_title(JournalEventType::UserPromptSubmit, &p).unwrap();
        assert_eq!(t, "Fix the WSL hooks install path");
    }

    #[test]
    fn user_prompt_submit_caps_long_prompts() {
        let long = "a ".repeat(80);
        let p = serde_json::json!({ "session_id": "s1", "prompt": long });
        let t = super::derive_session_title(JournalEventType::UserPromptSubmit, &p).unwrap();
        assert!(
            t.chars().count() <= super::TITLE_MAX_CHARS,
            "got {} chars",
            t.chars().count()
        );
        assert!(t.ends_with('…'));
    }

    #[test]
    fn session_start_falls_back_to_cwd_basename() {
        let p = serde_json::json!({ "session_id": "s1", "cwd": "/home/natkins/n8builds/tools/" });
        let t = super::derive_session_title(JournalEventType::SessionStart, &p).unwrap();
        assert_eq!(t, "tools");
    }

    #[test]
    fn redacted_user_prompt_falls_back_to_cwd_basename() {
        let payload = serde_json::json!({
            "session_id": "s1",
            "cwd": "/home/natkins/projects/t-hub/",
            "redacted_field_count": 1
        });
        let title =
            super::derive_session_title(JournalEventType::UserPromptSubmit, &payload).unwrap();
        assert_eq!(title, "t-hub");
        assert!(!serde_json::to_string(&payload)
            .unwrap()
            .contains("prompt-secret-canary"));
    }

    #[test]
    fn no_title_for_unrelated_events_or_empty_signal() {
        // Stop carries no title signal.
        assert!(super::derive_session_title(
            JournalEventType::Stop,
            &serde_json::json!({ "session_id": "s1" })
        )
        .is_none());
        // Empty prompt -> no title.
        assert!(super::derive_session_title(
            JournalEventType::UserPromptSubmit,
            &serde_json::json!({ "session_id": "s1", "prompt": "   \n  " })
        )
        .is_none());
        // SessionStart with no cwd -> no title.
        assert!(super::derive_session_title(
            JournalEventType::SessionStart,
            &serde_json::json!({ "session_id": "s1" })
        )
        .is_none());
    }

    #[test]
    fn consume_emits_agent_title_with_cwd_for_correlation() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));

        let entry = EventJournalEntry {
            seq: 1,
            timestamp_ms: 1,
            source: JournalSource::Hook,
            event_id: None,
            entity_id: Some("sess-7".into()),
            event_type: JournalEventType::UserPromptSubmit,
            payload: serde_json::json!({
                "session_id": "sess-7",
                "cwd": "/home/u/proj",
                "redacted_field_count": 1
            }),
            result: None,
        };
        bridge.consume_journal_entry(&entry);

        let title_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_TITLE)
            .cloned()
            .expect("agent://title must be emitted");
        assert_eq!(title_ev.1["sessionId"], "sess-7");
        assert_eq!(title_ev.1["cwd"], "/home/u/proj");
        assert_eq!(title_ev.1["title"], "proj");
    }
}
