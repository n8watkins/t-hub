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
//!     session. The opening scrollback frame is returned in [`PtyHandle`]; out/exit
//!     frames stream onto a channel; write/resize go back up. A dropped connection
//!     (e.g. the app restarting) auto-reattaches with backoff at the current
//!     geometry and re-emits the fresh scrollback so the consumer re-syncs.
//!
//! ## PTY framing (T13: v2 binary, v1 fallback)
//! The PTY plane is version-negotiated PER ATTACH (§1.2). This client asks for the
//! v2 length-prefixed BINARY framing (`attach_pty` arg `"binary": true`, `"v": 2`):
//! `[u8 type][u32 BE len][payload]` frames with no base64 and no JSON envelope on
//! the firehose. Against a server that predates v2 it downgrades automatically:
//! the handshake file's `protocol_version` is used as a hint to skip the attempt,
//! and if the attempt is made anyway (unknown version) a JSON rejection line from
//! the old `v != 1` gate triggers a same-connection v1 retry. The negotiated
//! framing is renegotiated on every reconnect (the server may have been upgraded
//! across an app restart). Consumers never see any of this: [`PtyFrame`] is
//! identical under both framings. Commands + events stay JSON always.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use crossbeam::channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use serde_json::{json, Value};

/// The highest control wire protocol version this client speaks (§1.2; matches the
/// server's `control::PROTOCOL_VERSION`). v2 (T13) added the opt-in binary PTY
/// framing; it is advertised only on the binary `attach_pty` attempt.
pub const PROTOCOL_VERSION: u32 = 2;

/// The v1 wire version, stamped on request / subscribe / v1-attach lines. Those
/// planes' semantics are unchanged since v1, and a v1 server's version gate
/// rejected any OTHER version (`!=`, relaxed to `>` only in v2 servers) - so
/// advertising v1 there keeps this client compatible with every server generation.
const PROTOCOL_V1: u32 = 1;

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
// Binary PTY framing (v2, T13) - client half of `pty::binframe` on the server
// ---------------------------------------------------------------------------

/// The PTY-plane framing negotiated at attach (§1.2), mirroring the server's
/// `pty::PtyFraming`. `V1Json` is base64-NDJSON; `V2Binary` is length-prefixed
/// binary frames. Exposed on [`PtyHandle::framing`] for diagnostics/measurement;
/// consumers of [`PtyFrame`] never need to look at it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PtyFraming {
    V1Json,
    V2Binary,
}

/// Binary PTY frame type tags (v2, §1.2) - mirror `pty::binframe` on the server.
/// `0x0_` are server→client, `0x1_` client→server. Wire layout of every frame:
/// `[u8 type][u32 big-endian length][length payload bytes]`.
pub mod binframe {
    /// server→client: a chunk of raw PTY output (payload = the bytes verbatim).
    pub const OUT: u8 = 0x01;
    /// server→client: the attach client exited. Payload is 4 big-endian bytes (an
    /// `i32` exit code) or EMPTY for unknown/signalled (the v1 `null`).
    pub const EXIT: u8 = 0x02;
    /// server→client: the opening scrollback seed (payload = raw capture bytes).
    pub const SCROLLBACK: u8 = 0x03;
    /// server→client: an attach error raised before the stream starts (UTF-8 message).
    pub const ERROR: u8 = 0x04;
    /// client→server: raw bytes for the PTY stdin (payload = the bytes verbatim).
    pub const WRITE: u8 = 0x10;
    /// client→server: a resize. Payload is 4 bytes: `[u16 BE cols][u16 BE rows]`.
    pub const RESIZE: u8 = 0x11;
}

/// The 5-byte binary frame header: `[u8 type][u32 big-endian length]`.
pub const BIN_HEADER_LEN: usize = 5;

/// Cap on a frame's declared length (16 MiB), mirroring the server's
/// `pty::BIN_MAX_FRAME`: inbound, a bigger declared length means a corrupt or
/// hostile peer (tear down + reconnect); outbound, [`PtyHandle::write`] chunks at
/// this size so a huge paste can never trip the server's identical inbound cap.
pub const BIN_MAX_FRAME: usize = 16 * 1024 * 1024;

/// Encode one binary frame: `[type][u32 BE len][payload]`. Callers keep payloads
/// at or under [`BIN_MAX_FRAME`] (write chunks, resize is 4 bytes), so the length
/// always fits u32; the assert makes that invariant explicit.
fn encode_bin_frame(ty: u8, payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= BIN_MAX_FRAME, "binary frame payload over cap");
    let mut frame = Vec::with_capacity(BIN_HEADER_LEN + payload.len());
    frame.push(ty);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// One step of draining binary frames from the reader's accumulation buffer.
#[derive(Debug, PartialEq)]
enum BinStep {
    /// The buffer does not yet hold one complete frame.
    Incomplete,
    /// One complete frame, drained off the front of the buffer.
    Frame(u8, Vec<u8>),
    /// The peer declared a length over [`BIN_MAX_FRAME`]: corrupt/hostile stream.
    Corrupt(usize),
}

/// Drain one complete binary frame off the front of `acc`, if present.
fn take_bin_frame(acc: &mut Vec<u8>) -> BinStep {
    if acc.len() < BIN_HEADER_LEN {
        return BinStep::Incomplete;
    }
    let len = u32::from_be_bytes([acc[1], acc[2], acc[3], acc[4]]) as usize;
    if len > BIN_MAX_FRAME {
        return BinStep::Corrupt(len);
    }
    if acc.len() < BIN_HEADER_LEN + len {
        return BinStep::Incomplete;
    }
    let ty = acc[0];
    let payload = acc[BIN_HEADER_LEN..BIN_HEADER_LEN + len].to_vec();
    acc.drain(..BIN_HEADER_LEN + len);
    BinStep::Frame(ty, payload)
}

// ---------------------------------------------------------------------------
// Endpoint discovery
// ---------------------------------------------------------------------------

/// A resolved control endpoint: where to connect and the token to authenticate
/// every request/subscribe/attach with. §1.3. `protocol_version` is the server's
/// ADVERTISED version from the handshake file (T13): `Some(v) if v < 2` lets the
/// attach path skip the doomed binary attempt against an old server; `None`
/// (env-override discovery, or a pre-M2b handshake without the field) means
/// unknown - attempt v2 and rely on the fallback.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub addr: String,
    pub token: String,
    pub protocol_version: Option<u32>,
}

impl Endpoint {
    /// Discover the live endpoint: `T_HUB_REMOTE_ADDR` / `T_HUB_REMOTE_TOKEN`
    /// override per field, otherwise `~/.t-hub/control.json` (`{addr, token,
    /// protocol_version}`). Called fresh on every (re)connect so an app restart's
    /// new port is picked up.
    pub fn discover() -> Result<Endpoint> {
        let env_addr = std::env::var("T_HUB_REMOTE_ADDR").ok().filter(|s| !s.is_empty());
        let env_token = std::env::var("T_HUB_REMOTE_TOKEN").ok().filter(|s| !s.is_empty());

        // Fast path: both overridden -> no file needed (matches t1 remote runs).
        // The server version is unknown here; the attach fallback covers it.
        if let (Some(addr), Some(token)) = (env_addr.clone(), env_token.clone()) {
            return Ok(Endpoint { addr, token, protocol_version: None });
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
        let protocol_version = hs
            .get("protocol_version")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        Ok(Endpoint { addr, token, protocol_version })
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

/// Classify one v2 binary frame into [`PtyWire`]. EXIT's payload is 4 BE bytes
/// (an i32 code) or empty for unknown/signalled - both anything-else payloads and
/// the empty one map to `-1`, matching the v1 `null` rule. A late SCROLLBACK (the
/// opening one is consumed at attach) or an unknown type tag is skipped, mirroring
/// the v1 parser's leniency and the server's forward-compat rule.
fn classify_bin_frame(ty: u8, payload: Vec<u8>) -> PtyWire {
    match ty {
        binframe::OUT => PtyWire::Output(payload),
        binframe::EXIT => PtyWire::Exit(
            <[u8; 4]>::try_from(payload.as_slice())
                .map(i32::from_be_bytes)
                .unwrap_or(-1),
        ),
        _ => PtyWire::Ignore,
    }
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

/// The PTY write half plus the framing negotiated for its connection. Swapped as
/// one unit by the reader thread on reconnect - a reattach RENEGOTIATES, so the
/// framing can change across an app restart (server upgraded or downgraded).
struct PtyTx {
    stream: Option<TcpStream>,
    framing: PtyFraming,
}

/// Inbound PTY-plane byte counters (additive to §1.3, for the T13 measurement and
/// the debug overlay). `wire` counts raw socket bytes as received - framing
/// included; `payload` counts the decoded bytes delivered to the consumer
/// (scrollback seeds + `Out` frames).
#[derive(Default)]
struct WireStats {
    wire: AtomicU64,
    payload: AtomicU64,
}

/// A live attach to one session (§1.3). `scrollback` is the decoded opening seed;
/// `output` streams [`PtyFrame`]s. `write`/`resize` go back up the connection;
/// `detach` (or drop) shuts it down (the tmux SESSION survives). A mid-stream
/// disconnect auto-reattaches internally - `output` just keeps flowing. The wire
/// framing underneath (v2 binary vs v1 JSON) is invisible here except through the
/// diagnostic accessors ([`framing`](Self::framing), [`wire_bytes_in`](Self::wire_bytes_in)).
pub struct PtyHandle {
    pub scrollback: Vec<u8>,
    pub output: Receiver<PtyFrame>,
    /// The current write half + its negotiated framing. Swapped by the reader
    /// thread on reconnect, so `write`/`resize` always target the live connection
    /// in the framing it actually speaks.
    writer: Arc<Mutex<PtyTx>>,
    /// Last requested geometry, so a reconnect reattaches at the right size and
    /// `resize` can record it for the next reconnect.
    geom: Arc<Mutex<(u16, u16)>>,
    stats: Arc<WireStats>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl PtyHandle {
    /// Send keystrokes: a binary WRITE frame (v2) or `{"write":"<b64>"}` (v1).
    /// Best-effort - a dead connection is dropped and the reader thread will
    /// reconnect; the byte is logged and lost (matching how a webview keystroke
    /// into a momentarily-detached tile is lost). v2 chunks at [`BIN_MAX_FRAME`]
    /// so even a giant paste stays under the server's inbound frame cap.
    pub fn write(&self, b: &[u8]) {
        let mut guard = self.writer.lock();
        match guard.framing {
            PtyFraming::V1Json => {
                let frame = json!({ "write": STANDARD.encode(b) });
                send_json_line(&mut guard, &frame);
            }
            PtyFraming::V2Binary => {
                for chunk in b.chunks(BIN_MAX_FRAME) {
                    if !send_bytes(&mut guard, &encode_bin_frame(binframe::WRITE, chunk)) {
                        break;
                    }
                }
            }
        }
    }

    /// Resize: a binary RESIZE frame (v2, payload `[u16 BE cols][u16 BE rows]`) or
    /// `{"resize":{"cols":C,"rows":R}}` (v1). Records the geometry first so a
    /// concurrent/subsequent reconnect reattaches at the new size.
    pub fn resize(&self, c: u16, r: u16) {
        *self.geom.lock() = (c, r);
        let mut guard = self.writer.lock();
        match guard.framing {
            PtyFraming::V1Json => {
                let frame = json!({ "resize": { "cols": c, "rows": r } });
                send_json_line(&mut guard, &frame);
            }
            PtyFraming::V2Binary => {
                let mut payload = [0u8; 4];
                payload[..2].copy_from_slice(&c.to_be_bytes());
                payload[2..].copy_from_slice(&r.to_be_bytes());
                send_bytes(&mut guard, &encode_bin_frame(binframe::RESIZE, &payload));
            }
        }
    }

    /// The framing negotiated for the current (most recent) attach connection.
    /// Diagnostic - consumers of [`PtyFrame`] never need it.
    pub fn framing(&self) -> PtyFraming {
        self.writer.lock().framing
    }

    /// Raw inbound socket bytes received on the PTY plane so far (framing included).
    pub fn wire_bytes_in(&self) -> u64 {
        self.stats.wire.load(Ordering::Acquire)
    }

    /// Decoded payload bytes delivered so far (scrollback seeds + `Out` frames).
    pub fn payload_bytes_in(&self) -> u64 {
        self.stats.payload.load(Ordering::Acquire)
    }

    /// Detach: stop reconnecting, shut the socket (server detaches; tmux SESSION
    /// survives), and join the reader thread. Idempotent with `Drop`.
    pub fn detach(mut self) {
        self.shutdown_and_join();
    }

    fn shutdown_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(w) = self.writer.lock().stream.take() {
            let _ = w.shutdown(Shutdown::Both);
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// Serialize + send one NDJSON line on the current PTY connection (v1 upstream).
fn send_json_line(tx: &mut PtyTx, frame: &Value) {
    let mut line = match serde_json::to_vec(frame) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("pty: serialize frame failed: {e}");
            return;
        }
    };
    line.push(b'\n');
    send_bytes(tx, &line);
}

/// Send pre-encoded bytes on the current PTY connection. On a write failure the
/// dead stream is dropped (the reader loop's EOF/error triggers reattach) and
/// `false` is returned so a chunked caller stops early.
fn send_bytes(tx: &mut PtyTx, bytes: &[u8]) -> bool {
    let Some(w) = tx.stream.as_mut() else {
        return false;
    };
    if let Err(e) = w.write_all(bytes).and_then(|()| w.flush()) {
        log::warn!("pty: frame write failed ({e}); dropping conn for reconnect");
        tx.stream = None;
        return false;
    }
    true
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

/// One freshly negotiated PTY attach connection: the write half, the buffered
/// read half (may hold bytes past the opening frame), the decoded scrollback
/// seed, the framing the connection ended up speaking, and the exact inbound
/// wire bytes the opening consumed (for the stats counters).
struct AttachedPty {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    scrollback: Vec<u8>,
    framing: PtyFraming,
    opening_wire_bytes: u64,
}

/// Open a PTY attach connection, negotiate the framing, and read the opening
/// scrollback frame. Mirrors `remote_pty::RemotePty::connect` (v1) plus the T13
/// v2 negotiation of `scripts/probes/t13_binframe.py`:
///
/// 1. Unless `force_v1` or the discovered handshake advertises a server older
///    than v2, ask for binary framing (`"binary": true`, `"v": 2`).
/// 2. Classify the opening bytes: a binary SCROLLBACK/ERROR tag means the server
///    speaks v2; a JSON line means a v1 server - either a `{"scrollback"}` frame
///    (a pre-versioning server that ignored the unknown arg) or an `{"ok":false}`
///    rejection (the old `v != 1` gate, which leaves the connection open), in
///    which case the attach is re-sent as v1 ON THE SAME CONNECTION.
fn pty_attach(seed: &Endpoint, session: &str, cols: u16, rows: u16, force_v1: bool) -> Result<AttachedPty> {
    let ep = resolve(seed);
    let stream = dial(&ep)?;
    let writer = stream.try_clone().context("clone pty stream")?;
    let _ = writer.set_write_timeout(Some(WRITE_TIMEOUT));
    let mut handshake = stream.try_clone().context("clone pty stream")?;
    let mut reader = BufReader::new(stream);

    let server_predates_v2 = matches!(ep.protocol_version, Some(v) if v < 2);
    if force_v1 || server_predates_v2 {
        return attach_v1(&ep, &mut handshake, reader, writer, session, cols, rows);
    }

    // v2 attempt: opt in to binary framing.
    send_attach_request(&ep, &mut handshake, session, cols, rows, PtyFraming::V2Binary)?;

    // Peek the first opening byte to classify the server's answer. A v2 server
    // opens with a binary SCROLLBACK (or pre-stream ERROR) frame; every JSON
    // answer starts with '{'.
    let first = {
        let buf = reader.fill_buf().context("read opening attach byte")?;
        if buf.is_empty() {
            return Err(anyhow!("connection closed before the opening attach frame"));
        }
        buf[0]
    };
    match first {
        binframe::SCROLLBACK | binframe::ERROR => {
            let mut header = [0u8; BIN_HEADER_LEN];
            reader.read_exact(&mut header).context("read opening binary header")?;
            let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
            if len > BIN_MAX_FRAME {
                return Err(anyhow!("opening binary frame declares {len}B (over cap)"));
            }
            let mut payload = vec![0u8; len];
            reader.read_exact(&mut payload).context("read opening binary payload")?;
            if header[0] == binframe::ERROR {
                return Err(anyhow!("{}", String::from_utf8_lossy(&payload)));
            }
            Ok(AttachedPty {
                writer,
                reader,
                scrollback: payload,
                framing: PtyFraming::V2Binary,
                opening_wire_bytes: (BIN_HEADER_LEN + len) as u64,
            })
        }
        b'{' => {
            let mut line = String::new();
            let n = reader.read_line(&mut line).context("read opening attach frame")?;
            let opening: Value =
                serde_json::from_str(line.trim()).context("parse opening attach frame")?;
            if let Some(sb) = opening.get("scrollback").and_then(|v| v.as_str()) {
                // A pre-versioning v1 server ignored the `binary` arg and just
                // served v1. Carry on in v1 on this same, already-open stream.
                log::info!("pty[{session}]: server ignored binary opt-in; speaking v1");
                let scrollback = STANDARD.decode(sb).context("decode scrollback")?;
                return Ok(AttachedPty {
                    writer,
                    reader,
                    scrollback,
                    framing: PtyFraming::V1Json,
                    opening_wire_bytes: n as u64,
                });
            }
            // An {"ok":false} / {"error"} line. From a v1-gate server this is the
            // version rejection and the connection stays open for the next request
            // line - so retry the attach as v1 right here. A REAL error (bad
            // token, dead session) simply fails again with the same message,
            // which the v1 path then surfaces; one wasted round-trip, no
            // brittle error-message sniffing.
            log::info!(
                "pty[{session}]: v2 attach rejected ({}); retrying as v1",
                opening.get("error").and_then(|v| v.as_str()).unwrap_or("unknown")
            );
            attach_v1(&ep, &mut handshake, reader, writer, session, cols, rows)
        }
        other => Err(anyhow!("unrecognized opening attach byte 0x{other:02x}")),
    }
}

/// Send one `attach_pty` request line. v2 asks for binary framing and advertises
/// `v: 2`; v1 omits `binary` and advertises `v: 1` (accepted by every server).
fn send_attach_request(
    ep: &Endpoint,
    handshake: &mut TcpStream,
    session: &str,
    cols: u16,
    rows: u16,
    framing: PtyFraming,
) -> Result<()> {
    let (v, binary) = match framing {
        PtyFraming::V1Json => (PROTOCOL_V1, false),
        PtyFraming::V2Binary => (PROTOCOL_VERSION, true),
    };
    let mut args = json!({ "sessionId": session, "cols": cols, "rows": rows });
    if binary {
        args["binary"] = json!(true);
    }
    let mut frame = serde_json::to_vec(&json!({
        "token": ep.token,
        "command": ATTACH_PTY_COMMAND,
        "args": args,
        "v": v,
    }))?;
    frame.push(b'\n');
    handshake
        .write_all(&frame)
        .and_then(|()| handshake.flush())
        .context("write attach_pty handshake")
}

/// The v1 attach: send the request (no `binary`), read the opening
/// `{"scrollback"}` line. Mirrors the original T4 path byte-for-byte.
fn attach_v1(
    ep: &Endpoint,
    handshake: &mut TcpStream,
    mut reader: BufReader<TcpStream>,
    writer: TcpStream,
    session: &str,
    cols: u16,
    rows: u16,
) -> Result<AttachedPty> {
    send_attach_request(ep, handshake, session, cols, rows, PtyFraming::V1Json)?;

    let mut line = String::new();
    let n = reader.read_line(&mut line).context("read scrollback frame")?;
    if n == 0 {
        return Err(anyhow!("connection closed before the scrollback frame"));
    }
    let opening: Value =
        serde_json::from_str(line.trim()).context("parse opening attach frame")?;
    // A bad token comes back as a normal control response, not a frame; a
    // pre-stream attach failure is `{"ok":false,"error"}` too (since T13a).
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

    Ok(AttachedPty {
        writer,
        reader,
        scrollback,
        framing: PtyFraming::V1Json,
        opening_wire_bytes: n as u64,
    })
}

/// What the frame-drain pass decided the reader loop should do next.
enum Drain {
    /// All complete frames delivered; block for more bytes.
    Continue,
    /// Terminal: the consumer hung up or a real exit frame arrived.
    Stop,
    /// The stream is corrupt (an over-cap binary length); tear down + reconnect.
    Teardown,
}

/// Drain every COMPLETE frame in `acc` per `framing`, delivering `Out`/`Exit`
/// onto `out_tx` and counting decoded payload bytes. Incomplete tails stay in
/// `acc` for the next socket read.
fn drain_frames(
    acc: &mut Vec<u8>,
    framing: PtyFraming,
    out_tx: &Sender<PtyFrame>,
    stats: &WireStats,
) -> Drain {
    loop {
        let wire = match framing {
            PtyFraming::V1Json => match acc.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    let line: Vec<u8> = acc.drain(..=pos).collect();
                    parse_pty_frame(&line[..line.len() - 1])
                }
                None => return Drain::Continue,
            },
            PtyFraming::V2Binary => match take_bin_frame(acc) {
                BinStep::Frame(ty, payload) => classify_bin_frame(ty, payload),
                BinStep::Incomplete => return Drain::Continue,
                BinStep::Corrupt(len) => {
                    log::warn!("pty: binary frame declares {len}B (over cap); tearing down");
                    return Drain::Teardown;
                }
            },
        };
        match wire {
            PtyWire::Output(bytes) => {
                stats.payload.fetch_add(bytes.len() as u64, Ordering::AcqRel);
                if out_tx.send(PtyFrame::Out(bytes)).is_err() {
                    return Drain::Stop; // consumer gone
                }
            }
            PtyWire::Exit(code) => {
                let _ = out_tx.send(PtyFrame::Exit(code));
                return Drain::Stop; // real process exit: terminal, do not reconnect
            }
            PtyWire::Ignore => {}
        }
    }
}

/// The PTY reader thread: drain out/exit frames (in the connection's negotiated
/// framing) onto `out_tx`; on a mid-stream disconnect (EOF/error/corrupt stream,
/// not a real exit frame) reattach with backoff at the current geometry -
/// RENEGOTIATING the framing - swap the shared writer, and re-emit the fresh
/// scrollback as one `Out` frame so the consumer re-syncs. Stops on a real exit
/// or when `stop` is set (detach/Drop).
#[allow(clippy::too_many_arguments)] // one thread entry point, wired once from attach_pty
fn pty_reader_loop(
    seed: Endpoint,
    session: String,
    reader: BufReader<TcpStream>,
    mut framing: PtyFraming,
    force_v1: bool,
    out_tx: Sender<PtyFrame>,
    writer_slot: Arc<Mutex<PtyTx>>,
    geom: Arc<Mutex<(u16, u16)>>,
    stats: Arc<WireStats>,
    stop: Arc<AtomicBool>,
) {
    // Bytes the opening read may have pulled past the opening frame are wire
    // bytes too - count them as they enter the accumulator.
    let mut acc: Vec<u8> = reader.buffer().to_vec();
    stats.wire.fetch_add(acc.len() as u64, Ordering::AcqRel);
    let mut stream = reader.into_inner();
    let mut buf = [0u8; RECV_BUF];
    let mut backoff = BACKOFF_INITIAL;

    'outer: loop {
        // Drain complete frames already buffered, then block for more.
        loop {
            match drain_frames(&mut acc, framing, &out_tx, &stats) {
                Drain::Continue => {}
                Drain::Stop => return,
                Drain::Teardown => {
                    let _ = stream.shutdown(Shutdown::Both);
                    break;
                }
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            match (&stream).read(&mut buf) {
                Ok(0) => break, // EOF -> reconnect
                Ok(n) => {
                    stats.wire.fetch_add(n as u64, Ordering::AcqRel);
                    acc.extend_from_slice(&buf[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break, // torn-down -> reconnect
            }
        }

        // Disconnected mid-stream (no exit frame). Reconnect unless we're detaching.
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(backoff);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            let (cols, rows) = *geom.lock();
            match pty_attach(&seed, &session, cols, rows, force_v1) {
                Ok(att) => {
                    framing = att.framing;
                    stats.wire.fetch_add(att.opening_wire_bytes, Ordering::AcqRel);
                    *writer_slot.lock() = PtyTx {
                        stream: Some(att.writer),
                        framing,
                    };
                    if !att.scrollback.is_empty() {
                        // Re-emit the reattach seed so the consumer's grid re-syncs.
                        stats
                            .payload
                            .fetch_add(att.scrollback.len() as u64, Ordering::AcqRel);
                        let _ = out_tx.send(PtyFrame::Out(att.scrollback));
                    }
                    acc = att.reader.buffer().to_vec();
                    stats.wire.fetch_add(acc.len() as u64, Ordering::AcqRel);
                    stream = att.reader.into_inner();
                    backoff = BACKOFF_INITIAL;
                    log::info!("pty[{session}]: reattached after disconnect ({framing:?})");
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
            "v": PROTOCOL_V1,
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
    /// scrollback and a live output channel. §1.3. The wire framing (v2 binary
    /// against a current server, v1 against one that predates T13) is negotiated
    /// underneath and invisible to the frames.
    pub fn attach_pty(&self, session: &str, cols: u16, rows: u16) -> Result<PtyHandle> {
        self.attach_pty_inner(session, cols, rows, false)
    }

    /// Attach forcing the v1 base64-NDJSON framing. Additive, diagnostic-only:
    /// used by the T13 A/B bandwidth measurement and as a live regression check
    /// that the v1 path a webview-era server serves still works.
    pub fn attach_pty_v1(&self, session: &str, cols: u16, rows: u16) -> Result<PtyHandle> {
        self.attach_pty_inner(session, cols, rows, true)
    }

    fn attach_pty_inner(
        &self,
        session: &str,
        cols: u16,
        rows: u16,
        force_v1: bool,
    ) -> Result<PtyHandle> {
        let att = pty_attach(&self.seed, session, cols, rows, force_v1)?;
        let (out_tx, out_rx) = unbounded();
        let writer_slot = Arc::new(Mutex::new(PtyTx {
            stream: Some(att.writer),
            framing: att.framing,
        }));
        let geom = Arc::new(Mutex::new((cols, rows)));
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(WireStats::default());
        stats.wire.fetch_add(att.opening_wire_bytes, Ordering::AcqRel);
        stats
            .payload
            .fetch_add(att.scrollback.len() as u64, Ordering::AcqRel);

        let handle = {
            let seed = self.seed.clone();
            let session = session.to_string();
            let framing = att.framing;
            let reader = att.reader;
            let writer_slot = writer_slot.clone();
            let geom = geom.clone();
            let stop = stop.clone();
            let stats = stats.clone();
            thread::Builder::new()
                .name(format!("t-hub-native-pty-{session}"))
                .spawn(move || {
                    pty_reader_loop(
                        seed, session, reader, framing, force_v1, out_tx, writer_slot, geom,
                        stats, stop,
                    )
                })
                .context("spawn pty reader thread")?
        };

        Ok(PtyHandle {
            scrollback: att.scrollback,
            output: out_rx,
            writer: writer_slot,
            geom,
            stats,
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
        "v": PROTOCOL_V1,
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
    // The ack advertises the server's version (2 since T13a); log it for skew triage.
    let server_v = ack_v
        .pointer("/result/protocolVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    log::info!("events: subscribed (server protocolVersion {server_v})");

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

    /// Serializes tests that mutate the process-global `T_HUB_*` env, since
    /// cargo runs tests in parallel threads sharing one environment.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn out_frame(bytes: &[u8]) -> Vec<u8> {
        format!("{{\"out\":\"{}\"}}", STANDARD.encode(bytes)).into_bytes()
    }

    // -- binary frame codec (v2) ------------------------------------------

    #[test]
    fn bin_frames_round_trip_through_the_accumulator() {
        // Several frames, fed as ONE contiguous byte run (as off a socket).
        let mut acc = Vec::new();
        acc.extend_from_slice(&encode_bin_frame(binframe::SCROLLBACK, b"seed"));
        acc.extend_from_slice(&encode_bin_frame(binframe::OUT, b"hello \x1b[0m"));
        acc.extend_from_slice(&encode_bin_frame(binframe::OUT, b"")); // empty payload
        acc.extend_from_slice(&encode_bin_frame(binframe::EXIT, &7i32.to_be_bytes()));

        assert_eq!(
            take_bin_frame(&mut acc),
            BinStep::Frame(binframe::SCROLLBACK, b"seed".to_vec())
        );
        assert_eq!(
            take_bin_frame(&mut acc),
            BinStep::Frame(binframe::OUT, b"hello \x1b[0m".to_vec())
        );
        assert_eq!(take_bin_frame(&mut acc), BinStep::Frame(binframe::OUT, vec![]));
        assert_eq!(
            take_bin_frame(&mut acc),
            BinStep::Frame(binframe::EXIT, 7i32.to_be_bytes().to_vec())
        );
        assert_eq!(take_bin_frame(&mut acc), BinStep::Incomplete);
        assert!(acc.is_empty());
    }

    #[test]
    fn bin_frames_survive_arbitrary_read_boundaries() {
        // The same frames dribbled in 3-byte reads: every prefix short of a full
        // frame must yield Incomplete, and the frames must come out intact.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_bin_frame(binframe::OUT, b"chunk-one"));
        wire.extend_from_slice(&encode_bin_frame(binframe::EXIT, &(-9i32).to_be_bytes()));

        let mut acc = Vec::new();
        let mut got = Vec::new();
        for piece in wire.chunks(3) {
            acc.extend_from_slice(piece);
            loop {
                match take_bin_frame(&mut acc) {
                    BinStep::Frame(ty, payload) => got.push((ty, payload)),
                    BinStep::Incomplete => break,
                    BinStep::Corrupt(_) => panic!("stream misread as corrupt"),
                }
            }
        }
        assert_eq!(
            got,
            vec![
                (binframe::OUT, b"chunk-one".to_vec()),
                (binframe::EXIT, (-9i32).to_be_bytes().to_vec()),
            ]
        );
    }

    #[test]
    fn bin_frame_over_cap_is_corrupt() {
        let mut acc = vec![binframe::OUT];
        acc.extend_from_slice(&(BIN_MAX_FRAME as u32 + 1).to_be_bytes());
        assert_eq!(take_bin_frame(&mut acc), BinStep::Corrupt(BIN_MAX_FRAME + 1));
    }

    #[test]
    fn classify_maps_exit_payloads_like_v1_null() {
        assert_eq!(
            classify_bin_frame(binframe::EXIT, 137i32.to_be_bytes().to_vec()),
            PtyWire::Exit(137)
        );
        // Empty payload = unknown/signalled (the v1 `"exit":null`) -> -1.
        assert_eq!(classify_bin_frame(binframe::EXIT, vec![]), PtyWire::Exit(-1));
        // A malformed (wrong-size) payload also degrades to -1, never a panic.
        assert_eq!(classify_bin_frame(binframe::EXIT, vec![1, 2]), PtyWire::Exit(-1));
    }

    #[test]
    fn classify_skips_late_scrollback_and_unknown_tags() {
        assert_eq!(
            classify_bin_frame(binframe::SCROLLBACK, b"late".to_vec()),
            PtyWire::Ignore
        );
        assert_eq!(classify_bin_frame(0x7f, b"future".to_vec()), PtyWire::Ignore);
        assert_eq!(
            classify_bin_frame(binframe::OUT, b"bytes".to_vec()),
            PtyWire::Output(b"bytes".to_vec())
        );
    }

    #[test]
    fn encode_matches_the_wire_layout() {
        // [u8 type][u32 BE len][payload] - pinned against §1.2 byte-for-byte.
        assert_eq!(
            encode_bin_frame(binframe::WRITE, b"hi"),
            vec![0x10, 0, 0, 0, 2, b'h', b'i']
        );
        assert_eq!(encode_bin_frame(binframe::RESIZE, &[0, 90, 0, 25]), vec![
            0x11, 0, 0, 0, 4, 0, 90, 0, 25
        ]);
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

    // -- attach negotiation against a mock server ---------------------------

    /// The handler a mock server runs for each `attach_pty` connection: the first
    /// request line, plus the connection's read and write halves.
    type AttachHandler = Arc<dyn Fn(Value, &mut BufReader<TcpStream>, &mut TcpStream) + Send + Sync>;

    /// A mock control server for the attach-negotiation tests. Each accepted
    /// connection runs on its own thread: `__subscribe_events` is acked + parked
    /// (absorbing the client's background event stream), and the FIRST request
    /// line of an `attach_pty` connection is handed to `on_attach` along with the
    /// connection halves. Returns (addr, done-flag, server thread).
    fn spawn_mock_server(
        on_attach: AttachHandler,
    ) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let done = Arc::new(AtomicBool::new(false));
        let srv_done = done.clone();
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !srv_done.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        let on_attach = on_attach.clone();
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
                                Some(SUBSCRIBE_COMMAND) => {
                                    let _ = w.write_all(
                                        b"{\"ok\":true,\"result\":{\"subscribed\":true}}\n",
                                    );
                                    let _ = w.flush();
                                    thread::sleep(Duration::from_secs(2));
                                }
                                Some(ATTACH_PTY_COMMAND) => on_attach(v, &mut r, &mut w),
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
        (addr, done, server)
    }

    /// Read one binary frame off the mock's connection (blocking).
    fn read_bin_frame(r: &mut BufReader<TcpStream>) -> (u8, Vec<u8>) {
        let mut header = [0u8; BIN_HEADER_LEN];
        r.read_exact(&mut header).unwrap();
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).unwrap();
        (header[0], payload)
    }

    fn recv(rx: &Receiver<PtyFrame>) -> PtyFrame {
        rx.recv_timeout(Duration::from_secs(5)).expect("pty frame in time")
    }

    /// A v2 server: the client opts in with `binary:true, v:2`, gets a binary
    /// SCROLLBACK, round-trips binary WRITE -> OUT and RESIZE, and gets a binary
    /// EXIT - the full happy path, byte-for-byte, with the wire counters checked.
    #[test]
    fn attach_negotiates_v2_binary_end_to_end() {
        let _g = ENV_LOCK.lock();
        let on_attach = Arc::new(
            |req: Value, r: &mut BufReader<TcpStream>, w: &mut TcpStream| {
                assert_eq!(req["args"]["binary"], json!(true), "client must opt in");
                assert_eq!(req["v"], json!(2), "binary attempt advertises v2");
                w.write_all(&encode_bin_frame(binframe::SCROLLBACK, b"SEED-V2")).unwrap();
                w.write_all(&encode_bin_frame(binframe::OUT, b"hello ")).unwrap();
                w.flush().unwrap();
                // client -> server: a binary WRITE, then a binary RESIZE.
                assert_eq!(read_bin_frame(r), (binframe::WRITE, b"ping".to_vec()));
                assert_eq!(read_bin_frame(r), (binframe::RESIZE, vec![0, 120, 0, 40]));
                w.write_all(&encode_bin_frame(binframe::OUT, b"pong")).unwrap();
                w.write_all(&encode_bin_frame(binframe::EXIT, &7i32.to_be_bytes())).unwrap();
                w.flush().unwrap();
            },
        );
        let (addr, done, server) = spawn_mock_server(on_attach);

        std::env::set_var("T_HUB_REMOTE_ADDR", &addr);
        std::env::set_var("T_HUB_REMOTE_TOKEN", "mock");
        let client = ControlClient::connect(Endpoint::discover().unwrap()).unwrap();
        let handle = client.attach_pty("s1", 80, 24).unwrap();

        assert_eq!(handle.framing(), PtyFraming::V2Binary);
        assert_eq!(handle.scrollback, b"SEED-V2");
        assert_eq!(recv(&handle.output), PtyFrame::Out(b"hello ".to_vec()));
        handle.write(b"ping");
        handle.resize(120, 40);
        assert_eq!(recv(&handle.output), PtyFrame::Out(b"pong".to_vec()));
        assert_eq!(recv(&handle.output), PtyFrame::Exit(7));

        // Counters: payload = 7 (seed) + 6 + 4; wire adds a 5B header per frame.
        assert_eq!(handle.payload_bytes_in(), 17);
        assert_eq!(handle.wire_bytes_in(), 17 + 4 * BIN_HEADER_LEN as u64 + 4);

        std::env::remove_var("T_HUB_REMOTE_ADDR");
        std::env::remove_var("T_HUB_REMOTE_TOKEN");
        drop(handle);
        done.store(true, Ordering::Release);
        let _ = server.join();
    }

    /// A v1 server (the old `v != 1` gate): it REJECTS the v2 attempt with an
    /// `{"ok":false}` line but keeps the connection open - the client must retry
    /// as v1 on the same connection and the consumer must see identical frames.
    #[test]
    fn attach_falls_back_to_v1_when_the_server_rejects_v2() {
        let _g = ENV_LOCK.lock();
        let on_attach = Arc::new(
            |req: Value, r: &mut BufReader<TcpStream>, w: &mut TcpStream| {
                // First request: the v2 attempt. Reject it the way the old gate did.
                assert_eq!(req["v"], json!(2));
                w.write_all(
                    b"{\"ok\":false,\"error\":\"unsupported control protocol version\"}\n",
                )
                .unwrap();
                w.flush().unwrap();
                // Second request on the SAME connection: the v1 retry.
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let retry: Value = serde_json::from_str(line.trim()).unwrap();
                assert_eq!(retry["command"], json!(ATTACH_PTY_COMMAND));
                assert_eq!(retry["v"], json!(1));
                assert!(retry["args"].get("binary").is_none(), "v1 retry must not re-ask");
                let seed = format!("{{\"scrollback\":\"{}\"}}\n", STANDARD.encode(b"SEED-V1"));
                w.write_all(seed.as_bytes()).unwrap();
                w.flush().unwrap();
                // v1 upstream: the client's write arrives base64-JSON.
                let mut wline = String::new();
                r.read_line(&mut wline).unwrap();
                let wframe: Value = serde_json::from_str(wline.trim()).unwrap();
                assert_eq!(wframe["write"], json!(STANDARD.encode(b"ping")));
                let out = format!("{{\"out\":\"{}\"}}\n", STANDARD.encode(b"pong"));
                w.write_all(out.as_bytes()).unwrap();
                w.write_all(b"{\"exit\":0}\n").unwrap();
                w.flush().unwrap();
            },
        );
        let (addr, done, server) = spawn_mock_server(on_attach);

        std::env::set_var("T_HUB_REMOTE_ADDR", &addr);
        std::env::set_var("T_HUB_REMOTE_TOKEN", "mock");
        let client = ControlClient::connect(Endpoint::discover().unwrap()).unwrap();
        let handle = client.attach_pty("s1", 80, 24).unwrap();

        assert_eq!(handle.framing(), PtyFraming::V1Json);
        assert_eq!(handle.scrollback, b"SEED-V1");
        handle.write(b"ping");
        assert_eq!(recv(&handle.output), PtyFrame::Out(b"pong".to_vec()));
        assert_eq!(recv(&handle.output), PtyFrame::Exit(0));

        std::env::remove_var("T_HUB_REMOTE_ADDR");
        std::env::remove_var("T_HUB_REMOTE_TOKEN");
        drop(handle);
        done.store(true, Ordering::Release);
        let _ = server.join();
    }

    /// A handshake file advertising `protocol_version: 1` makes the client skip
    /// the v2 attempt entirely - the server sees ONE v1 attach, no rejection
    /// round-trip. (Also covers parsing the version out of control.json.)
    #[test]
    fn attach_skips_v2_when_the_handshake_advertises_v1() {
        let _g = ENV_LOCK.lock();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_srv = seen.clone();
        let on_attach = Arc::new(
            move |req: Value, _r: &mut BufReader<TcpStream>, w: &mut TcpStream| {
                seen_srv.lock().push(req);
                let seed = format!("{{\"scrollback\":\"{}\"}}\n", STANDARD.encode(b"OLD"));
                w.write_all(seed.as_bytes()).unwrap();
                w.write_all(b"{\"exit\":0}\n").unwrap();
                w.flush().unwrap();
            },
        );
        let (addr, done, server) = spawn_mock_server(on_attach);

        // Discovery via a handshake FILE (not env), so protocol_version flows in.
        let hs_path = std::env::temp_dir().join(format!(
            "t13b-hint-test-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &hs_path,
            format!("{{\"addr\":\"{addr}\",\"token\":\"mock\",\"pid\":1,\"protocol_version\":1}}"),
        )
        .unwrap();
        std::env::remove_var("T_HUB_REMOTE_ADDR");
        std::env::remove_var("T_HUB_REMOTE_TOKEN");
        std::env::set_var("T_HUB_CONTROL_JSON", &hs_path);

        let ep = Endpoint::discover().unwrap();
        assert_eq!(ep.protocol_version, Some(1));
        let client = ControlClient::connect(ep).unwrap();
        let handle = client.attach_pty("s1", 80, 24).unwrap();
        assert_eq!(handle.framing(), PtyFraming::V1Json);
        assert_eq!(handle.scrollback, b"OLD");
        assert_eq!(recv(&handle.output), PtyFrame::Exit(0));

        let reqs = seen.lock().clone();
        assert_eq!(reqs.len(), 1, "exactly one attach request, no v2 probe");
        assert_eq!(reqs[0]["v"], json!(1));
        assert!(reqs[0]["args"].get("binary").is_none());

        std::env::remove_var("T_HUB_CONTROL_JSON");
        let _ = std::fs::remove_file(&hs_path);
        drop(handle);
        done.store(true, Ordering::Release);
        let _ = server.join();
    }
}
