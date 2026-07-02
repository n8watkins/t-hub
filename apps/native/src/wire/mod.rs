//! **Wire protocol client** for the T-Hub control socket (native-pivot T4).
//!
//! This is the native client's implementation of the ControlClient contract in
//! `docs/NATIVE-PIVOT-EXECUTION.md` §1.3, speaking the wire in §1.2. It is a pure
//! port of the reference probe clients (`scripts/probes/t1_*.py`) and mirrors the
//! server framing the Tauri webview already speaks in
//! `apps/desktop/src-tauri/src/remote_pty.rs`.
//!
//! Three planes, each on its OWN TCP connection (matches the server model):
//!   - **request/response** ([`ControlClient::request`]): one tokened request line
//!     -> one response line. Backed by a single pooled connection guarded by a
//!     `Mutex`; a dropped connection is transparently redialed with backoff.
//!   - **events** ([`ControlClient::events`]): a dedicated connection that sends
//!     `__subscribe_events` once then streams `{"event","payload"}` frames onto a
//!     crossbeam channel. A background thread owns it and reconnects+resubscribes
//!     with backoff, invisibly to the consumer.
//!   - **PTY** ([`ControlClient::attach_pty`]): a dedicated connection per attached
//!     session. The opening `{"scrollback"}` frame is returned in [`PtyHandle`];
//!     `{"out"}`/`{"exit"}` stream onto a channel; `{"write"}`/`{"resize"}` go back
//!     up. A dropped connection (e.g. the app restarting) auto-reattaches with
//!     backoff at the current geometry and re-emits the fresh scrollback so the
//!     consumer re-syncs.
//!
//! **Reconnect-with-backoff lives entirely inside this module** - consumers (T5,
//! T8, T9, T11, T12) never see a disconnect. See §1.3: "Reconnect-with-backoff
//! belongs inside ControlClient, invisible to consumers."
//!
//! ## Discovery
//! [`Endpoint::discover`] reads `~/.t-hub/control.json` (`{addr, token, ...}`),
//! with per-field overrides from `T_HUB_REMOTE_ADDR` / `T_HUB_REMOTE_TOKEN` (and
//! `T_HUB_CONTROL_JSON` for the handshake path itself). The loopback port is
//! EPHEMERAL - it changes every time the app restarts - so every reconnect
//! re-discovers rather than reusing the seed endpoint; that is what lets an
//! attached session (and the event stream) survive an app restart.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use crossbeam::channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use serde_json::{json, Value};

/// Control wire protocol version (§1.2; must equal the server's
/// `control::PROTOCOL_VERSION`). Sent as `"v"` on every request.
pub const PROTOCOL_VERSION: u32 = 1;

/// The subscribe command that turns a connection into an event stream (§1.2).
const SUBSCRIBE_COMMAND: &str = "__subscribe_events";
/// The PTY attach command (§1.2), mirrored from `control::ATTACH_PTY_COMMAND`.
const ATTACH_PTY_COMMAND: &str = "attach_pty";

/// Connect timeout for every plane. Generous for a loopback round-trip; also fine
/// for a Tailscale-remote server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Write timeout on PTY writers, so a stalled remote peer surfaces an error rather
/// than blocking `write`/`resize` forever (mirrors `remote_pty::WRITE_TIMEOUT`).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Reconnect backoff bounds (exponential, capped). Applied by the events thread,
/// the PTY reader thread, and the request path's redial loop.
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How many redial attempts a single `request` rides out before giving up. With
/// the backoff schedule above this spans ~13s - long enough to ride an app
/// restart (new ephemeral port published to control.json) without erroring.
const REQUEST_MAX_ATTEMPTS: usize = 6;

/// Socket read size for the PTY plane. One read can carry several NDJSON frames,
/// which the reader parses out of its own accumulation buffer.
const RECV_BUF: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// Endpoint discovery
// ---------------------------------------------------------------------------

/// A resolved control endpoint: where to connect and the token to authenticate
/// every request/subscribe/attach with. §1.3.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub addr: String,
    pub token: String,
}

impl Endpoint {
    /// Discover the live endpoint: `T_HUB_REMOTE_ADDR` / `T_HUB_REMOTE_TOKEN`
    /// override per field, otherwise `~/.t-hub/control.json` (`{addr, token}`).
    /// Called fresh on every (re)connect so an app restart's new port is picked up.
    pub fn discover() -> Result<Endpoint> {
        let env_addr = std::env::var("T_HUB_REMOTE_ADDR").ok().filter(|s| !s.is_empty());
        let env_token = std::env::var("T_HUB_REMOTE_TOKEN").ok().filter(|s| !s.is_empty());

        // Fast path: both overridden -> no file needed (matches t1 remote runs).
        if let (Some(addr), Some(token)) = (env_addr.clone(), env_token.clone()) {
            return Ok(Endpoint { addr, token });
        }

        let path = control_json_path();
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read control handshake {}", path.display()))?;
        let hs: Value = serde_json::from_str(&raw)
            .with_context(|| format!("parse control handshake {}", path.display()))?;

        let addr = env_addr
            .or_else(|| hs.get("addr").and_then(|v| v.as_str()).map(String::from))
            .ok_or_else(|| anyhow!("no 'addr' in {}", path.display()))?;
        let token = env_token
            .or_else(|| hs.get("token").and_then(|v| v.as_str()).map(String::from))
            .ok_or_else(|| anyhow!("no 'token' in {}", path.display()))?;
        Ok(Endpoint { addr, token })
    }
}

/// Path to the control handshake file. `T_HUB_CONTROL_JSON` overrides; otherwise
/// `$HOME/.t-hub/control.json` (WSL) or `%USERPROFILE%\.t-hub\control.json`
/// (Windows). See the T1 notes on the WSL symlink mirroring the Windows-home file.
fn control_json_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_CONTROL_JSON") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_default();
    let mut p = PathBuf::from(home);
    p.push(".t-hub");
    p.push("control.json");
    p
}

/// Re-discover the endpoint on reconnect, falling back to the seed if discovery
/// momentarily fails (e.g. control.json being rewritten during an app restart).
fn resolve(seed: &Endpoint) -> Endpoint {
    Endpoint::discover().unwrap_or_else(|_| seed.clone())
}

fn dial(ep: &Endpoint) -> Result<TcpStream> {
    let sock: SocketAddr = ep
        .addr
        .parse()
        .with_context(|| format!("bad control addr {:?}", ep.addr))?;
    TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT)
        .with_context(|| format!("connect to {} failed", ep.addr))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// One event frame off the `__subscribe_events` stream (§1.2/§1.3). `channel` is
/// e.g. `status://snapshot`, `session://status`, `supervision://tree`; `payload`
/// is the raw JSON body.
#[derive(Clone, Debug)]
pub struct Event {
    pub channel: String,
    pub payload: Value,
}

/// Parse one NDJSON event line into an [`Event`]. Non-event lines (the ack, blanks,
/// malformed frames) yield `None` so a single bad frame never tears the stream.
fn parse_event_frame(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let channel = v.get("event").and_then(|c| c.as_str())?.to_string();
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    Some(Event { channel, payload })
}

// ---------------------------------------------------------------------------
// PTY plane
// ---------------------------------------------------------------------------

/// One parsed PTY output frame (§1.3). `Out` carries decoded bytes (server base64
/// already undone); `Exit` carries the process exit code.
#[derive(Clone, Debug, PartialEq)]
pub enum PtyFrame {
    Out(Vec<u8>),
    Exit(i32),
}

/// One classified PTY wire frame off the attach stream. Distinct from [`PtyFrame`]
/// because `Ignore` (blanks / malformed / late scrollback) must not reach the
/// consumer, and the reader must tell `Exit` (terminal) apart from EOF (reconnect).
#[derive(Debug, PartialEq)]
enum PtyWire {
    Output(Vec<u8>),
    Exit(i32),
    Ignore,
}

/// Parse one NDJSON PTY line (no trailing newline). Mirrors
/// `remote_pty::parse_pty_frame`: a blank/malformed/un-decodable frame is
/// [`PtyWire::Ignore`], never a tear-down. A null/absent exit code maps to `-1`
/// (the §1.3 `PtyFrame::Exit(i32)` has no Option; unknown == -1).
fn parse_pty_frame(line: &[u8]) -> PtyWire {
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return PtyWire::Ignore;
    }
    let frame: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return PtyWire::Ignore,
    };
    if let Some(b64) = frame.get("out").and_then(|v| v.as_str()) {
        match STANDARD.decode(b64) {
            Ok(bytes) => PtyWire::Output(bytes),
            Err(_) => PtyWire::Ignore,
        }
    } else if let Some(exit) = frame.get("exit") {
        PtyWire::Exit(exit.as_i64().and_then(|c| i32::try_from(c).ok()).unwrap_or(-1))
    } else {
        PtyWire::Ignore
    }
}

/// A live attach to one session (§1.3). `scrollback` is the decoded opening seed;
/// `output` streams [`PtyFrame`]s. `write`/`resize` go back up the connection;
/// `detach` (or drop) shuts it down (the tmux SESSION survives). A mid-stream
/// disconnect auto-reattaches internally - `output` just keeps flowing.
pub struct PtyHandle {
    pub scrollback: Vec<u8>,
    pub output: Receiver<PtyFrame>,
    /// The current write half. Swapped by the reader thread on reconnect, so
    /// `write`/`resize` always target the live connection.
    writer: Arc<Mutex<Option<TcpStream>>>,
    /// Last requested geometry, so a reconnect reattaches at the right size and
    /// `resize` can record it for the next reconnect.
    geom: Arc<Mutex<(u16, u16)>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl PtyHandle {
    /// Send keystrokes: `{"write":"<b64>"}`. Best-effort - a dead connection is
    /// dropped and the reader thread will reconnect; the byte is logged and lost
    /// (matching how a webview keystroke into a momentarily-detached tile is lost).
    pub fn write(&self, b: &[u8]) {
        let frame = json!({ "write": STANDARD.encode(b) });
        self.send_frame(&frame);
    }

    /// Resize: `{"resize":{"cols":C,"rows":R}}`. Records the geometry first so a
    /// concurrent/subsequent reconnect reattaches at the new size.
    pub fn resize(&self, c: u16, r: u16) {
        *self.geom.lock() = (c, r);
        let frame = json!({ "resize": { "cols": c, "rows": r } });
        self.send_frame(&frame);
    }

    fn send_frame(&self, frame: &Value) {
        let mut guard = self.writer.lock();
        if let Some(w) = guard.as_mut() {
            let mut line = match serde_json::to_vec(frame) {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("pty: serialize frame failed: {e}");
                    return;
                }
            };
            line.push(b'\n');
            if let Err(e) = w.write_all(&line).and_then(|()| w.flush()) {
                log::warn!("pty: frame write failed ({e}); dropping conn for reconnect");
                // Drop the dead writer; the reader loop's EOF/error triggers reattach.
                *guard = None;
            }
        }
    }

    /// Detach: stop reconnecting, shut the socket (server detaches; tmux SESSION
    /// survives), and join the reader thread. Idempotent with `Drop`.
    pub fn detach(mut self) {
        self.shutdown_and_join();
    }

    fn shutdown_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(w) = self.writer.lock().take() {
            let _ = w.shutdown(Shutdown::Both);
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

/// Open a PTY attach connection, send the handshake, and read the opening frame.
/// Returns (writer half, buffered reader, decoded scrollback). Mirrors
/// `remote_pty::RemotePty::connect`.
fn pty_attach(
    seed: &Endpoint,
    session: &str,
    cols: u16,
    rows: u16,
) -> Result<(TcpStream, BufReader<TcpStream>, Vec<u8>)> {
    let ep = resolve(seed);
    let stream = dial(&ep)?;
    let writer = stream.try_clone().context("clone pty stream")?;
    let _ = writer.set_write_timeout(Some(WRITE_TIMEOUT));

    let mut handshake = stream.try_clone().context("clone pty stream")?;
    let mut frame = serde_json::to_vec(&json!({
        "token": ep.token,
        "command": ATTACH_PTY_COMMAND,
        "args": { "sessionId": session, "cols": cols, "rows": rows },
        "v": PROTOCOL_VERSION,
    }))?;
    frame.push(b'\n');
    handshake
        .write_all(&frame)
        .and_then(|()| handshake.flush())
        .context("write attach_pty handshake")?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).context("read scrollback frame")?;
    if n == 0 {
        return Err(anyhow!("connection closed before the scrollback frame"));
    }
    let opening: Value =
        serde_json::from_str(line.trim()).context("parse opening attach frame")?;
    // A bad token comes back as a normal control response, not a frame.
    if opening.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        return Err(anyhow!(
            "{}",
            opening
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("attach_pty rejected")
        ));
    }
    if let Some(err) = opening.get("error").and_then(|v| v.as_str()) {
        return Err(anyhow!("{err}"));
    }
    let scrollback = opening
        .get("scrollback")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("expected a scrollback frame, got: {}", line.trim()))
        .and_then(|b64| STANDARD.decode(b64).context("decode scrollback"))?;

    Ok((writer, reader, scrollback))
}

/// The PTY reader thread: drain `{"out"}`/`{"exit"}` frames onto `out_tx`; on a
/// mid-stream disconnect (EOF/error, not a real `{"exit"}`) reattach with backoff
/// at the current geometry, swap the shared writer, and re-emit the fresh
/// scrollback as one `Out` frame so the consumer re-syncs. Stops on `{"exit"}`
/// (real process exit) or when `stop` is set (detach/Drop).
fn pty_reader_loop(
    seed: Endpoint,
    session: String,
    reader: BufReader<TcpStream>,
    out_tx: Sender<PtyFrame>,
    writer_slot: Arc<Mutex<Option<TcpStream>>>,
    geom: Arc<Mutex<(u16, u16)>>,
    stop: Arc<AtomicBool>,
) {
    let mut acc: Vec<u8> = reader.buffer().to_vec();
    let mut stream = reader.into_inner();
    let mut buf = [0u8; RECV_BUF];
    let mut backoff = BACKOFF_INITIAL;

    'outer: loop {
        // Drain complete lines already buffered, then block for more.
        loop {
            while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = acc.drain(..=pos).collect();
                match parse_pty_frame(&line[..line.len() - 1]) {
                    PtyWire::Output(bytes) => {
                        if out_tx.send(PtyFrame::Out(bytes)).is_err() {
                            return; // consumer gone
                        }
                    }
                    PtyWire::Exit(code) => {
                        let _ = out_tx.send(PtyFrame::Exit(code));
                        return; // real process exit: terminal, do not reconnect
                    }
                    PtyWire::Ignore => {}
                }
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            match (&stream).read(&mut buf) {
                Ok(0) => break,                                   // EOF -> reconnect
                Ok(n) => acc.extend_from_slice(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,                                  // torn-down -> reconnect
            }
        }

        // Disconnected mid-stream (no {"exit"}). Reconnect unless we're detaching.
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(backoff);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            let (cols, rows) = *geom.lock();
            match pty_attach(&seed, &session, cols, rows) {
                Ok((w, r, sb)) => {
                    *writer_slot.lock() = Some(w);
                    if !sb.is_empty() {
                        // Re-emit the reattach seed so the consumer's grid re-syncs.
                        let _ = out_tx.send(PtyFrame::Out(sb));
                    }
                    let new_reader = r;
                    acc = new_reader.buffer().to_vec();
                    stream = new_reader.into_inner();
                    backoff = BACKOFF_INITIAL;
                    log::info!("pty[{session}]: reattached after disconnect");
                    continue 'outer;
                }
                Err(e) => {
                    log::debug!("pty[{session}]: reattach failed ({e}); backing off");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request/response connection
// ---------------------------------------------------------------------------

/// A pooled request/response connection: a buffered read half and a write-half
/// clone, plus the token it authenticates with.
struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    token: String,
}

impl Conn {
    fn dial(seed: &Endpoint) -> Result<Conn> {
        let ep = resolve(seed);
        let stream = dial(&ep)?;
        let writer = stream.try_clone().context("clone request stream")?;
        Ok(Conn {
            reader: BufReader::new(stream),
            writer,
            token: ep.token,
        })
    }

    fn roundtrip(&mut self, command: &str, args: &Value) -> Result<Value> {
        let mut line = serde_json::to_vec(&json!({
            "token": self.token,
            "command": command,
            "args": args,
            "v": PROTOCOL_VERSION,
        }))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .and_then(|()| self.writer.flush())
            .context("write request")?;

        let mut resp = String::new();
        let n = self.reader.read_line(&mut resp).context("read response")?;
        if n == 0 {
            return Err(anyhow!("connection closed"));
        }
        let v: Value = serde_json::from_str(resp.trim()).context("parse response")?;
        if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(anyhow!(
                "{}",
                v.get("error").and_then(|e| e.as_str()).unwrap_or("request failed")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// ControlClient
// ---------------------------------------------------------------------------

/// The §1.3 control client: request/response pool + a background event stream,
/// with reconnect-with-backoff baked in. Cheap to `Arc`-share across the app.
pub struct ControlClient {
    seed: Endpoint,
    req: Mutex<Option<Conn>>,
    events_rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
    events_thread: Mutex<Option<JoinHandle<()>>>,
}

impl ControlClient {
    /// Discover the live endpoint (control.json + env overrides). §1.3.
    pub fn discover() -> Result<Endpoint> {
        Endpoint::discover()
    }

    /// Connect: validate the request plane (one eager dial) and spin up the event
    /// stream thread. The `ep` seeds discovery; reconnects re-discover so the
    /// client rides app restarts (new ephemeral port). §1.3.
    pub fn connect(ep: Endpoint) -> Result<Self> {
        let conn = Conn::dial(&ep).context("initial control connect")?;
        let (tx, rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));

        let ev_seed = ep.clone();
        let ev_stop = stop.clone();
        let handle = thread::Builder::new()
            .name("t-hub-native-events".into())
            .spawn(move || events_loop(ev_seed, tx, ev_stop))
            .context("spawn events thread")?;

        Ok(ControlClient {
            seed: ep,
            req: Mutex::new(Some(conn)),
            events_rx: rx,
            stop,
            events_thread: Mutex::new(Some(handle)),
        })
    }

    /// Discover + connect in one step.
    pub fn connect_discovered() -> Result<Self> {
        Self::connect(Self::discover()?)
    }

    /// One request/response round-trip (§1.2). Transparently redials with backoff
    /// on a dropped connection, so an app restart mid-call recovers rather than
    /// erroring. §1.3.
    pub fn request(&self, command: &str, args: Value) -> Result<Value> {
        let mut guard = self.req.lock();
        let mut backoff = BACKOFF_INITIAL;
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..REQUEST_MAX_ATTEMPTS {
            if guard.is_none() {
                match Conn::dial(&self.seed) {
                    Ok(c) => *guard = Some(c),
                    Err(e) => {
                        last_err = Some(e);
                        if attempt + 1 < REQUEST_MAX_ATTEMPTS {
                            thread::sleep(backoff);
                            backoff = (backoff * 2).min(BACKOFF_MAX);
                        }
                        continue;
                    }
                }
            }
            let conn = guard.as_mut().expect("just dialed");
            match conn.roundtrip(command, &args) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    // Drop the (probably dead) conn; the next attempt redials.
                    *guard = None;
                    last_err = Some(e);
                    if attempt + 1 < REQUEST_MAX_ATTEMPTS {
                        thread::sleep(backoff);
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("request '{command}' failed")))
    }

    /// The live event stream (§1.3). The receiver stays valid across reconnects -
    /// the background thread resubscribes and keeps pushing.
    pub fn events(&self) -> Receiver<Event> {
        self.events_rx.clone()
    }

    /// Attach a PTY to `session` (a tmux session id, `th_`-prefixed or bare - the
    /// server derivation is idempotent). Returns a [`PtyHandle`] with the opening
    /// scrollback and a live output channel. §1.3.
    pub fn attach_pty(&self, session: &str, cols: u16, rows: u16) -> Result<PtyHandle> {
        let (writer, reader, scrollback) = pty_attach(&self.seed, session, cols, rows)?;
        let (out_tx, out_rx) = unbounded();
        let writer_slot = Arc::new(Mutex::new(Some(writer)));
        let geom = Arc::new(Mutex::new((cols, rows)));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let seed = self.seed.clone();
            let session = session.to_string();
            let writer_slot = writer_slot.clone();
            let geom = geom.clone();
            let stop = stop.clone();
            thread::Builder::new()
                .name(format!("t-hub-native-pty-{session}"))
                .spawn(move || {
                    pty_reader_loop(seed, session, reader, out_tx, writer_slot, geom, stop)
                })
                .context("spawn pty reader thread")?
        };

        Ok(PtyHandle {
            scrollback,
            output: out_rx,
            writer: writer_slot,
            geom,
            stop,
            reader: Some(handle),
        })
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Unblock the events thread's blocking read by shutting its connection is
        // not directly reachable here; it observes `stop` on its next loop turn or
        // frame. We detach the handle rather than block Drop on a network read.
        if let Some(h) = self.events_thread.lock().take() {
            // Best-effort: give it a moment, but do not hang Drop on a slow socket.
            drop(h);
        }
    }
}

/// The event stream thread: (re)connect, subscribe, and pump frames onto `tx`
/// until the connection drops, then back off and retry. Resets backoff after each
/// successful subscribe so a stable stream that later drops reconnects promptly.
fn events_loop(seed: Endpoint, tx: Sender<Event>, stop: Arc<AtomicBool>) {
    let mut backoff = BACKOFF_INITIAL;
    while !stop.load(Ordering::Acquire) {
        let subscribed = run_event_stream(&seed, &tx, &stop).unwrap_or_else(|e| {
            log::debug!("events: stream ended: {e}");
            false
        });
        if stop.load(Ordering::Acquire) {
            break;
        }
        if subscribed {
            backoff = BACKOFF_INITIAL;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Connect, subscribe, and stream events until the connection closes. Returns
/// `Ok(true)` if it got as far as a successful subscribe (so the caller resets
/// backoff even though the stream later dropped).
fn run_event_stream(seed: &Endpoint, tx: &Sender<Event>, stop: &AtomicBool) -> Result<bool> {
    let ep = resolve(seed);
    let stream = dial(&ep)?;
    let mut writer = stream.try_clone().context("clone events stream")?;
    let mut reader = BufReader::new(stream);

    let mut sub = serde_json::to_vec(&json!({
        "token": ep.token,
        "command": SUBSCRIBE_COMMAND,
        "args": {},
        "v": PROTOCOL_VERSION,
    }))?;
    sub.push(b'\n');
    writer
        .write_all(&sub)
        .and_then(|()| writer.flush())
        .context("write subscribe")?;

    // Ack line: {"ok":true,"result":{"subscribed":true,...}}.
    let mut ack = String::new();
    let n = reader.read_line(&mut ack).context("read subscribe ack")?;
    if n == 0 {
        return Err(anyhow!("connection closed before subscribe ack"));
    }
    let ack_v: Value = serde_json::from_str(ack.trim()).context("parse subscribe ack")?;
    if ack_v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        return Err(anyhow!(
            "subscribe rejected: {}",
            ack_v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")
        ));
    }
    log::info!("events: subscribed (protocolVersion {PROTOCOL_VERSION})");

    // Stream frames until EOF/error or stop.
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(true);
        }
        let mut line = String::new();
        let n = reader.read_line(&mut line).context("read event frame")?;
        if n == 0 {
            return Ok(true); // EOF: subscribed earlier, so caller resets backoff
        }
        if let Some(ev) = parse_event_frame(&line) {
            if tx.send(ev).is_err() {
                return Ok(true); // consumer gone
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience: typed session listing off `list_terminals`
// ---------------------------------------------------------------------------

/// One session from `list_terminals` (§ control.rs `list_terminals`).
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub tmux_session: String,
    pub title: String,
    pub state: String,
}

impl ControlClient {
    /// List live sessions via `list_terminals`, decoded into [`SessionInfo`].
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let result = self.request("list_terminals", json!({}))?;
        let arr = result
            .get("terminals")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .into_iter()
            .map(|t| SessionInfo {
                id: t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                tmux_session: t
                    .get("tmuxSession")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: t.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                state: t.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            })
            .collect())
    }
}

/// A small ring buffer of recent log/event lines for the debug overlay. Kept here
/// so both the GUI and the probe can reuse the cap.
pub fn push_capped(buf: &mut VecDeque<String>, line: String, cap: usize) {
    if buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serializes tests that mutate the process-global `T_HUB_REMOTE_*` env, since
    /// cargo runs tests in parallel threads sharing one environment.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn out_frame(bytes: &[u8]) -> Vec<u8> {
        format!("{{\"out\":\"{}\"}}", STANDARD.encode(bytes)).into_bytes()
    }

    #[test]
    fn parses_out_frame_decoding_base64() {
        assert_eq!(
            parse_pty_frame(&out_frame(b"hi\x1b[0m")),
            PtyWire::Output(b"hi\x1b[0m".to_vec())
        );
    }

    #[test]
    fn parses_exit_frame_including_null_as_minus_one() {
        assert_eq!(parse_pty_frame(br#"{"exit":0}"#), PtyWire::Exit(0));
        assert_eq!(parse_pty_frame(br#"{"exit":137}"#), PtyWire::Exit(137));
        assert_eq!(parse_pty_frame(br#"{"exit":null}"#), PtyWire::Exit(-1));
    }

    #[test]
    fn ignores_blank_malformed_and_other_frames() {
        assert_eq!(parse_pty_frame(b""), PtyWire::Ignore);
        assert_eq!(parse_pty_frame(b"   \t"), PtyWire::Ignore);
        assert_eq!(parse_pty_frame(b"not json"), PtyWire::Ignore);
        assert_eq!(parse_pty_frame(br#"{"out":"!!!"}"#), PtyWire::Ignore);
        assert_eq!(parse_pty_frame(br#"{"scrollback":"x"}"#), PtyWire::Ignore);
    }

    #[test]
    fn parses_event_frame_and_skips_non_events() {
        let ev = parse_event_frame(r#"{"event":"status://snapshot","payload":{"a":1}}"#).unwrap();
        assert_eq!(ev.channel, "status://snapshot");
        assert_eq!(ev.payload["a"], 1);
        assert!(parse_event_frame(r#"{"ok":true,"result":{"subscribed":true}}"#).is_none());
        assert!(parse_event_frame("garbage").is_none());
    }

    #[test]
    fn discover_honors_env_overrides() {
        let _g = ENV_LOCK.lock();
        // Set both overrides -> no file read needed.
        std::env::set_var("T_HUB_REMOTE_ADDR", "127.0.0.1:9");
        std::env::set_var("T_HUB_REMOTE_TOKEN", "tok-xyz");
        let ep = Endpoint::discover().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:9");
        assert_eq!(ep.token, "tok-xyz");
        std::env::remove_var("T_HUB_REMOTE_ADDR");
        std::env::remove_var("T_HUB_REMOTE_TOKEN");
    }

    /// Reconnect-with-backoff proof against a mock control server, WITHOUT the live
    /// app: the FIRST `list_terminals` connection is dropped mid-round-trip (no
    /// response), and `request` must redial and succeed on the next. The mock
    /// handles each accepted connection generically (a per-conn thread) so it also
    /// absorbs the client's separate event-subscribe connection without racing.
    #[test]
    fn request_redials_after_a_dropped_connection() {
        use std::sync::atomic::AtomicUsize;

        let _g = ENV_LOCK.lock();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let list_reqs = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));

        let srv_list = list_reqs.clone();
        let srv_done = done.clone();
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !srv_done.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        let list = srv_list.clone();
                        thread::spawn(move || {
                            let mut w = conn.try_clone().unwrap();
                            let mut r = BufReader::new(conn);
                            let mut line = String::new();
                            if r.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            let v: Value = match serde_json::from_str(line.trim()) {
                                Ok(v) => v,
                                Err(_) => return,
                            };
                            match v.get("command").and_then(|c| c.as_str()) {
                                Some("__subscribe_events") => {
                                    // Ack and hold the connection open (no frames).
                                    let _ = w.write_all(
                                        b"{\"ok\":true,\"result\":{\"subscribed\":true}}\n",
                                    );
                                    let _ = w.flush();
                                    thread::sleep(Duration::from_secs(2));
                                }
                                Some("list_terminals") => {
                                    let n = list.fetch_add(1, Ordering::AcqRel);
                                    if n == 0 {
                                        // First attempt: drop without responding.
                                        let _ = w.shutdown(Shutdown::Both);
                                    } else {
                                        let _ = w.write_all(
                                            b"{\"ok\":true,\"result\":{\"terminals\":[],\"count\":0}}\n",
                                        );
                                        let _ = w.flush();
                                    }
                                }
                                _ => {}
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        std::env::set_var("T_HUB_REMOTE_ADDR", &addr);
        std::env::set_var("T_HUB_REMOTE_TOKEN", "mock");
        let ep = Endpoint::discover().unwrap();
        let client = ControlClient::connect(ep).unwrap();
        // First round-trip's connection is dropped by the mock; request() redials.
        let result = client.request("list_terminals", json!({})).unwrap();
        assert_eq!(result["count"], 0);
        assert!(list_reqs.load(Ordering::Acquire) >= 2, "expected a redial");

        std::env::remove_var("T_HUB_REMOTE_ADDR");
        std::env::remove_var("T_HUB_REMOTE_TOKEN");
        done.store(true, Ordering::Release);
        let _ = server.join();
    }
}
