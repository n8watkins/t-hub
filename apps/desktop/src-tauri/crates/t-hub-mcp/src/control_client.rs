//! Client side of the loopback control channel: the bridge from `tools/call` to
//! the running T-Hub app.
//!
//! Discovery reads the stable authoritative handshake at `$T_HUB_CONTROL_FILE`
//! (falling back to `~/.t-hub/control.json` only for legacy callers). Address and
//! ambient read authentication come from that file. A durable Captain proves its
//! `$T_HUB_SESSION_TOKEN` to acquire a short-lived identity-bound control lease,
//! held only in this process. Legacy explicit address and token overrides remain
//! available for proof harnesses. Each call opens a short-lived TCP
//! connection to `addr`, sends one NDJSON request line, and reads one NDJSON
//! response line. Connections are not pooled - `tools/call` is infrequent and a
//! fresh connection keeps the client stateless and robust to app restarts.

#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// One control operation, including discovery, connect, write, read, endpoint
/// invalidation, retry, bridge recovery, and ambiguous-response lookup, must
/// finish within this wall-clock budget.
const CONTROL_DEADLINE: Duration = Duration::from_secs(10);
const LONG_ORCHESTRATION_TIMEOUT: Duration = Duration::from_secs(120);

fn response_timeout_for_command(command: &str) -> Duration {
    match command {
        "commission_captain" | "dispatch_crew" | "history_resume" | "reconcile_cortana"
        | "start_agent" => LONG_ORCHESTRATION_TIMEOUT,
        _ => CONTROL_DEADLINE,
    }
}

/// A single endpoint gets only a short slice of the overall budget so an
/// inherited port that accepts but stays silent cannot consume the recovery
/// window before the current endpoint is tried.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// Every control client accepts at most 1 MiB before the NDJSON response newline.
/// This bounds memory, parsing work, and any structured error derived from a peer.
const MAX_RESPONSE_FRAME_BYTES: usize = 1024 * 1024;

/// Spawn-class commands whose retries must dedup via a client `requestId`
/// (mirrors the app-side `is_idempotent_command`).
const IDEMPOTENT_COMMANDS: &[&str] = &[
    "spawn_terminal",
    "create_worktree",
    "history_resume",
    "reconcile_cortana",
    "commission_captain",
    "dispatch_crew",
    "start_agent",
];

fn is_idempotent_command(command: &str) -> bool {
    IDEMPOTENT_COMMANDS.contains(&command)
}

/// Mint a process-unique idempotency key without pulling in a uuid/rng dependency
/// (this crate is deliberately dependency-light). pid + a monotonic nanosecond
/// clock + a per-process counter is unique enough to key one launch's spawn
/// retries, which is all the server-side cache needs.
fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("mcp-{}-{}-{}", std::process::id(), nanos, n)
}

/// Ensure an idempotent command's args carry a `requestId`, returning the
/// (possibly augmented) args and the id to reuse for every retry of this call.
/// A non-idempotent command is passed through untouched with `None`.
fn ensure_request_id(command: &str, args: &Value) -> (Value, Option<String>) {
    if !is_idempotent_command(command) {
        return (args.clone(), None);
    }
    // Respect a caller-supplied id (e.g. the probe harness), else mint one.
    if let Some(existing) = args
        .get("requestId")
        .or_else(|| args.get("request_id"))
        .and_then(Value::as_str)
    {
        return (args.clone(), Some(existing.to_string()));
    }
    let id = new_request_id();
    let mut augmented = args.clone();
    match &mut augmented {
        Value::Object(map) => {
            map.insert("requestId".to_string(), Value::String(id.clone()));
        }
        // A non-object args (null / scalar): wrap into an object carrying the id.
        _ => {
            augmented = serde_json::json!({ "requestId": id });
        }
    }
    (augmented, Some(id))
}

/// How T-Hub's control channel was located + authenticated.
#[derive(Clone)]
pub struct ControlEndpoint {
    pub addr: String,
    pub token: String,
}

impl std::fmt::Debug for ControlEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlEndpoint")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// The on-disk handshake the app writes. We only need `addr` + `token`.
#[derive(Deserialize)]
struct Handshake {
    addr: String,
    token: String,
    #[serde(default)]
    protocol_version: u32,
    #[serde(default)]
    instance_id: String,
    #[serde(default)]
    listener_generation: u64,
    #[serde(default)]
    published_at: u64,
}

#[derive(Clone)]
pub(crate) struct CachedLease {
    token: String,
    expires_at: u64,
}

/// The inputs used to locate the control channel, captured up front so that
/// resolution is a pure function of its fields rather than of process-global
/// environment variables. Production builds construct this once with
/// [`Discovery::from_env`]; tests construct it directly, which keeps them
/// hermetic (no shared `T_HUB_CONTROL_*` env mutation that could race across
/// threads when the suite runs in parallel).
#[derive(Clone, Default)]
pub struct Discovery {
    /// Explicit control address override (`$T_HUB_CONTROL_ADDR`).
    pub addr: Option<String>,
    /// Explicit control token override (`$T_HUB_CONTROL_TOKEN`).
    pub token: Option<String>,
    /// Handshake file path override (`$T_HUB_CONTROL_FILE`); when `None`,
    /// resolution falls back to `~/.t-hub/control.json`.
    pub file: Option<PathBuf>,
    /// Home directory used to derive the default handshake path. When `None`,
    /// it is read from `$HOME`/`$USERPROFILE` at resolution time. Tests set this
    /// to keep the default-path branch off the real environment.
    pub home: Option<PathBuf>,
    /// Durable session credential used to prove identity during scoped lease
    /// renewal. Captured once from `T_HUB_SESSION_TOKEN`.
    pub session: Option<String>,
    /// Current identity-bound lease, held only in MCP process memory.
    pub(crate) lease: Arc<Mutex<Option<CachedLease>>>,
}

impl Discovery {
    /// Capture discovery inputs from the environment (the production path).
    /// Reading env once, here, means the rest of the crate never touches
    /// process-global state.
    pub fn from_env() -> Self {
        let non_empty = |v: String| if v.is_empty() { None } else { Some(v) };
        Discovery {
            addr: std::env::var("T_HUB_CONTROL_ADDR").ok().and_then(non_empty),
            token: std::env::var("T_HUB_CONTROL_TOKEN")
                .ok()
                .and_then(non_empty),
            file: std::env::var_os("T_HUB_CONTROL_FILE").map(PathBuf::from),
            // Resolved lazily in `handshake_path` so the default branch still
            // honors the live environment in production.
            home: None,
            session: std::env::var("T_HUB_SESSION_TOKEN")
                .ok()
                .and_then(non_empty),
            lease: Arc::new(Mutex::new(None)),
        }
    }

    /// Resolve the control endpoint, explicit addr+token override first, then
    /// the handshake file.
    ///
    /// Returns a descriptive error (not a panic) when the app isn't running /
    /// the handshake file is missing, so the MCP server can surface "T-Hub is
    /// not running" as a tool error rather than crashing.
    pub fn resolve(&self) -> Result<ControlEndpoint, String> {
        // 1. Explicit addr + token override, retained for proof harnesses and
        //    already-running legacy sessions. New app spawns use the stable file.
        if let (Some(addr), Some(token)) = (&self.addr, &self.token) {
            if !addr.is_empty() && !token.is_empty() {
                return Ok(ControlEndpoint {
                    addr: addr.clone(),
                    token: token.clone(),
                });
            }
        }

        // 2. The handshake file the running app wrote.
        self.resolve_from_file()
    }

    /// Read the endpoint from the handshake file ONLY, ignoring any
    /// `$T_HUB_CONTROL_ADDR`/`$T_HUB_CONTROL_TOKEN` override.
    ///
    /// This is the normal discovery path for new sessions and the recovery path
    /// after a legacy transport pin fails. The app atomically rewrites the file
    /// whenever the listener address changes.
    pub fn resolve_from_file(&self) -> Result<ControlEndpoint, String> {
        let path = self.handshake_path();
        let body = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "T-Hub control channel not found at {} ({e}). Is the T-Hub app \
                 running? (set T_HUB_CONTROL_ADDR + T_HUB_CONTROL_TOKEN to override.)",
                path.display()
            )
        })?;
        let hs: Handshake = serde_json::from_str(&body)
            .map_err(|e| format!("malformed control handshake at {}: {e}", path.display()))?;
        let socket: SocketAddr = hs.addr.parse().map_err(|_| {
            format!(
                "malformed control handshake at {}: addr is not a socket address",
                path.display()
            )
        })?;
        if !socket.ip().is_loopback() {
            return Err(format!(
                "unsafe control handshake at {}: addr is not loopback",
                path.display()
            ));
        }
        if hs.protocol_version > 2 {
            return Err(format!(
                "unsupported control handshake protocol {} at {}",
                hs.protocol_version,
                path.display()
            ));
        }
        if !hs.instance_id.is_empty() && hs.listener_generation == 0 {
            return Err(format!(
                "invalid control handshake at {}: listener generation is zero",
                path.display()
            ));
        }
        let now = epoch_ms();
        if hs.published_at > now.saturating_add(5 * 60 * 1000) {
            return Err(format!(
                "invalid control handshake at {}: publication time is in the future",
                path.display()
            ));
        }
        Ok(ControlEndpoint {
            addr: hs.addr,
            token: hs.token,
        })
    }

    /// Whether an explicit env pin (`$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN`)
    /// is in force - i.e. [`resolve`](Self::resolve) returned that pair rather than the
    /// file. This is compatibility state for sessions created before Package 0.
    pub fn has_env_pin(&self) -> bool {
        matches!(
            (&self.addr, &self.token),
            (Some(a), Some(t)) if !a.is_empty() && !t.is_empty()
        )
    }

    /// The endpoint to retry after the pinned one failed: the fresh ADDRESS the
    /// running app just published in control.json, but KEEPING the env token when an
    /// env pin is in force.
    ///
    /// Port-only legacy recovery keeps the pinned credential while adopting the
    /// fresh address. If the credential also rotated, a durable Captain exchanges
    /// its session identity for a scoped lease in memory.
    ///
    /// With NO env pin (the app's own / a probe path that never had one), there is no
    /// token to preserve, so the file's token is adopted as before.
    pub fn refreshed_endpoint(&self) -> Result<ControlEndpoint, String> {
        let file = self.resolve_from_file()?;
        if self.has_env_pin() {
            return Ok(ControlEndpoint {
                addr: file.addr,
                token: self.token.clone().unwrap_or_default(),
            });
        }
        Ok(file)
    }

    /// The handshake file path (mirrors `crate::control::handshake_path` on the
    /// app side): the `file` override, else `<home>/.t-hub/control.json`.
    fn handshake_path(&self) -> PathBuf {
        if let Some(p) = &self.file {
            return p.clone();
        }
        if let Some(home) = &self.home {
            return home.join(".t-hub").join("control.json");
        }
        // Existing WSL Captain processes may predate T_HUB_CONTROL_FILE. Their
        // inherited Windows USERPROFILE still identifies the authoritative host
        // home, while HOME points at the stale WSL shadow that caused Package 0.
        #[cfg(not(windows))]
        if let Some(profile) = std::env::var_os("USERPROFILE")
            .and_then(|value| windows_profile_to_wsl_path(&value.to_string_lossy()))
        {
            return profile.join(".t-hub").join("control.json");
        }
        #[cfg(not(windows))]
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            if let Some(path) = unique_windows_control_file(PathBuf::from("/mnt/c/Users")) {
                return path;
            }
            // Ambiguous or missing Windows discovery must fail closed. Falling
            // back to HOME here would resurrect the stale WSL shadow path.
            return PathBuf::from("/mnt/c/Users/.t-hub/control.json");
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".t-hub").join("control.json")
    }

    fn session_token(&self) -> &str {
        self.session.as_deref().unwrap_or("")
    }

    fn cached_lease_endpoint(&self, endpoint: &ControlEndpoint) -> Option<ControlEndpoint> {
        let lease = self
            .lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()?;
        if lease.expires_at <= epoch_ms().saturating_add(5_000) {
            return None;
        }
        Some(ControlEndpoint {
            addr: endpoint.addr.clone(),
            token: lease.token,
        })
    }

    fn cache_lease(&self, _endpoint: &ControlEndpoint, token: String, expires_at: u64) {
        *self
            .lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(CachedLease { token, expires_at });
    }
}

fn windows_profile_to_wsl_path(profile: &str) -> Option<PathBuf> {
    let normalized = profile.replace('\\', "/");
    let bytes = normalized.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'/' {
        return None;
    }
    let drive = (bytes[0] as char).to_ascii_lowercase();
    Some(PathBuf::from(format!(
        "/mnt/{drive}/{}",
        normalized[3..].trim_start_matches('/')
    )))
}

#[cfg(not(windows))]
fn unique_windows_control_file(users_root: PathBuf) -> Option<PathBuf> {
    let mut candidates = std::fs::read_dir(users_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(".t-hub").join("control.json"))
        .filter(|path| path.is_file());
    let only = candidates.next()?;
    candidates.next().is_none().then_some(only)
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The app's response envelope: `{ok, result?, error?}`.
#[derive(Debug, Deserialize)]
struct ControlResponse {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(rename = "errorDetails", default)]
    error_details: Option<Value>,
    #[serde(rename = "errorKind", default)]
    error_kind: Option<String>,
    #[serde(default)]
    retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlCallError {
    pub message: String,
    pub retryable: bool,
    pub kind: Option<String>,
    pub details: Option<Value>,
}

impl ControlCallError {
    fn from_message(message: String) -> Self {
        Self {
            message,
            retryable: false,
            kind: None,
            details: None,
        }
    }
}

impl From<String> for ControlCallError {
    fn from(message: String) -> Self {
        Self::from_message(message)
    }
}

impl std::fmt::Display for ControlCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::ops::Deref for ControlCallError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl PartialEq<&str> for ControlCallError {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

/// Why a single control round-trip failed, so the retry layer can tell a moved
/// endpoint apart from a command the app deliberately rejected.
#[derive(Debug)]
enum CallError {
    /// Transport-level FAST failure: connect refused, the stream died, or spoke
    /// garbage. A restarted/rebound app on a new ephemeral port looks exactly like
    /// this (connect to the retired port refuses), so the caller re-reads
    /// control.json and retries - but this is NOT the relay-wedge signature.
    Transport(&'static str),
    /// The round-trip CONNECTED but no response arrived before the deadline. This is
    /// the relay-wedge signature: the WSL2 mirrored-loopback relay accepts the
    /// connect locally then never carries the flow, so the app (healthy, reachable
    /// Windows-side) never answers. Distinguished from [`Transport`] so the self-heal
    /// fires ONLY on a wedge, never on an app-down (which refuses fast).
    Timeout(&'static str),
    /// The app answered and rejected the command (bad token, unknown command,
    /// governor refusal). A different endpoint won't change the verdict.
    App {
        message: String,
        kind: Option<String>,
        details: Option<Value>,
    },
    RetryableApp {
        message: String,
        kind: Option<String>,
        details: Option<Value>,
    },
    /// The peer answered with a malformed protocol frame. Retrying on another
    /// endpoint would hide a compatibility failure.
    Protocol(String),
    /// The request was fully written, then the peer closed after sending only part
    /// of its response frame. A requestId-bearing mutation may have applied, so its
    /// caller must reconcile status rather than treating this as terminal protocol.
    PartialResponse,
}

impl CallError {
    fn into_message(self, command: &str, attempts: u8, endpoint_replaced: bool) -> String {
        match self {
            CallError::Transport(stage) => {
                unavailable_message(command, attempts, stage, endpoint_replaced)
            }
            CallError::Timeout(stage) => timeout_message(command, attempts, stage),
            CallError::App { message, .. }
            | CallError::RetryableApp { message, .. }
            | CallError::Protocol(message) => message,
            CallError::PartialResponse => partial_response_message(),
        }
    }

    /// Whether this failure is the relay-wedge signature (connected-but-silent), the
    /// only class the bridge self-heal should act on.
    fn is_timeout(&self) -> bool {
        matches!(self, CallError::Timeout(_))
    }

    fn stage(&self) -> &'static str {
        match self {
            CallError::Transport(stage) | CallError::Timeout(stage) => stage,
            CallError::App { .. } | CallError::RetryableApp { .. } => "server",
            CallError::Protocol(_) => "protocol",
            CallError::PartialResponse => "read",
        }
    }
}

fn partial_response_message() -> String {
    "control_protocol: unterminated response frame after request write".to_string()
}

fn unavailable_message(
    command: &str,
    attempts: u8,
    stage: &str,
    endpoint_replaced: bool,
) -> String {
    format!(
        "control_unavailable: command '{command}' failed during {stage} after {attempts} attempt(s); endpoint_replaced={endpoint_replaced}"
    )
}

fn timeout_message(command: &str, attempts: u8, stage: &str) -> String {
    format!(
        "control_timeout: command '{command}' failed within its {}s recovery deadline during {stage} after {attempts} attempt(s); retry_state=exhausted",
        response_timeout_for_command(command).as_secs()
    )
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

#[derive(Clone, Copy)]
struct CallBudget {
    deadline: Instant,
    attempt_timeout: Duration,
}

impl CallBudget {
    /// Keep two polling slices inside the same overall deadline for stale
    /// endpoint retry, idempotency-status reconciliation, or bridge recovery.
    fn initial_attempt(self) -> Self {
        let reserve = self.attempt_timeout.saturating_mul(2);
        Self {
            deadline: self.deadline.checked_sub(reserve).unwrap_or(self.deadline),
            attempt_timeout: self.attempt_timeout,
        }
    }
}

/// Consecutive same-endpoint transport failures before the relay-wedge self-heal
/// fires one bridge-triggered rebind. `1` = heal on the first confirmed failure: a
/// wedged round-trip already consumed one bounded attempt slice proving the
/// endpoint is unresponsive, so waiting for another full deadline only doubles the
/// outage. False positives (a genuinely-down app, or a rare slow command) are cheap
/// and self-correcting - the bridge attempt just fails/rate-limits and the episode
/// guard blocks any repeat until a success resets it.
const WEDGE_TRIGGER_AFTER: u32 = 1;

/// Detection state machine for the relay-wedge self-heal (cause 2 of the
/// control-socket wedge; see PR #49). Pure and unit-testable: `resolve_and_call`
/// feeds it round-trip outcomes and it decides when to attempt ONE heal per episode.
///
/// An "episode" is a run of consecutive transport failures against an UNCHANGED
/// endpoint (i.e. control.json still names the same addr, so it is NOT an
/// app-restart-onto-a-new-port case that the file re-read already recovers). The
/// heal is attempted at most once per episode; the next success clears the episode
/// so a later wedge can heal again.
#[derive(Debug, Default)]
struct WedgeDetector {
    consecutive_transport_failures: u32,
    heal_attempted_this_episode: bool,
}

impl WedgeDetector {
    /// A round-trip succeeded: the endpoint is healthy again, ending any episode.
    fn on_success(&mut self) {
        self.consecutive_transport_failures = 0;
        self.heal_attempted_this_episode = false;
    }

    /// A transport failure whose fresh control.json re-read named the SAME endpoint.
    /// Returns `true` at most ONCE per episode - when the consecutive count first
    /// reaches `trigger_after` - to signal "attempt one bridge-triggered rebind now".
    fn on_unchanged_transport_failure(&mut self, trigger_after: u32) -> bool {
        self.consecutive_transport_failures = self.consecutive_transport_failures.saturating_add(1);
        if !self.heal_attempted_this_episode && self.consecutive_transport_failures >= trigger_after
        {
            self.heal_attempted_this_episode = true;
            return true;
        }
        false
    }
}

/// Process-global detector: the MCP server targets one app, so one shared episode
/// state across all `tools/call`s is exactly right (and keeps the "one heal per
/// episode" guarantee across separate calls during a persistent wedge).
fn wedge_detector() -> std::sync::MutexGuard<'static, WedgeDetector> {
    use std::sync::{Mutex, OnceLock};
    static DETECTOR: OnceLock<Mutex<WedgeDetector>> = OnceLock::new();
    DETECTOR
        .get_or_init(|| Mutex::new(WedgeDetector::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn renew_captain_endpoint(
    discovery: &Discovery,
    budget: CallBudget,
) -> Result<ControlEndpoint, String> {
    if discovery.session_token().is_empty() {
        return Err("control_reauthentication_required: T_HUB_SESSION_TOKEN is unavailable".into());
    }
    let endpoint = discovery.resolve_from_file()?;
    let response = call_classified(
        &endpoint,
        "renew_captain_control_lease",
        &Value::Null,
        budget,
        Some(discovery),
    )
    .map_err(|error| error.into_message("renew_captain_control_lease", 1, false))?;
    let lease = response
        .get("lease")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("control_protocol: lease renewal omitted its scoped credential")?;
    let expires_at = response
        .get("expiresAt")
        .and_then(Value::as_u64)
        .ok_or("control_protocol: lease renewal omitted its expiry")?;
    discovery.cache_lease(&endpoint, lease.to_string(), expires_at);
    Ok(ControlEndpoint {
        addr: endpoint.addr,
        token: lease.to_string(),
    })
}

fn endpoint_with_available_lease(
    discovery: &Discovery,
    endpoint: ControlEndpoint,
    budget: CallBudget,
    renew: bool,
) -> ControlEndpoint {
    if let Some(cached) = discovery.cached_lease_endpoint(&endpoint) {
        return cached;
    }
    if renew {
        if let Ok(leased) = renew_captain_endpoint(discovery, budget) {
            return leased;
        }
    }
    endpoint
}

/// Recover after a credential rejection at a reachable endpoint. The ambient
/// read credential is tried first so read operations remain available. Only a
/// second authorization refusal triggers durable identity reauthentication.
fn recover_after_auth_rejection(
    discovery: &Discovery,
    command: &str,
    args: &Value,
    budget: CallBudget,
) -> Result<Value, ControlCallError> {
    let ambient = discovery
        .resolve_from_file()
        .map_err(ControlCallError::from)?;
    match call_classified(&ambient, command, args, budget, Some(discovery)) {
        Ok(value)
            if command == "my_capability"
                && value.get("capability").and_then(Value::as_str) == Some("read") =>
        {
            let leased = renew_captain_endpoint(discovery, budget)?;
            call_classified(&leased, command, args, budget, Some(discovery))
                .map_err(|error| call_error_to_control(error, command, 3, true))
        }
        Ok(value) => Ok(value),
        Err(CallError::App { message, .. }) if is_auth_rejection(&message) => {
            let leased =
                renew_captain_endpoint(discovery, budget).map_err(ControlCallError::from)?;
            call_classified(&leased, command, args, budget, Some(discovery))
                .map_err(|error| call_error_to_control(error, command, 3, true))
        }
        Err(error) => Err(call_error_to_control(error, command, 2, true)),
    }
}

fn call_error_to_control(
    error: CallError,
    command: &str,
    attempts: u8,
    endpoint_replaced: bool,
) -> ControlCallError {
    match error {
        CallError::App {
            message,
            kind,
            details,
        } => ControlCallError {
            message,
            retryable: false,
            kind,
            details,
        },
        CallError::RetryableApp {
            message,
            kind,
            details,
        } => ControlCallError {
            message,
            retryable: true,
            kind,
            details,
        },
        other => {
            ControlCallError::from_message(other.into_message(command, attempts, endpoint_replaced))
        }
    }
}

/// Resolve the control endpoint and run one command, transparently recovering
/// from an app restart.
///
/// The app rebinds to a fresh ephemeral port on every launch and rewrites
/// control.json, but a session's MCP captured the old addr+token in its env at
/// spawn time (see `elevation_env` on the app side). So when the resolved
/// endpoint is dead (a transport failure), we re-resolve the fresh ADDR from
/// control.json and retry once against it, instead of wrongly concluding "T-Hub
/// is down".
///
/// A legacy port-only retry keeps its pinned token for compatibility. A durable
/// Captain also reauthenticates through its session identity and replaces any
/// stale global credential with a short-lived scoped lease. The ambient token
/// from discovery is used only to reach that renewal operation.
pub fn resolve_and_call(
    discovery: &Discovery,
    command: &str,
    args: &Value,
) -> Result<Value, ControlCallError> {
    resolve_and_call_with_deadline(
        discovery,
        command,
        args,
        response_timeout_for_command(command),
        ATTEMPT_TIMEOUT,
    )
}

fn resolve_and_call_with_deadline(
    discovery: &Discovery,
    command: &str,
    args: &Value,
    overall: Duration,
    attempt_timeout: Duration,
) -> Result<Value, ControlCallError> {
    let budget = CallBudget {
        deadline: Instant::now() + overall,
        attempt_timeout,
    };
    // Idempotency (ask #1): a spawn-class command carries a `requestId` so every
    // retry below dedups server-side (a retry never double-applies; a completed
    // outcome is replayed). The SAME id is reused for the initial call and every
    // recovery path.
    let (args, request_id) = ensure_request_id(command, args);
    let endpoint = discovery.resolve()?;
    let endpoint = endpoint_with_available_lease(
        discovery,
        endpoint,
        budget.initial_attempt(),
        !discovery.has_env_pin() && !discovery.session_token().is_empty(),
    );
    if Instant::now() >= budget.deadline {
        return Err(timeout_message(command, 0, "discovery").into());
    }
    match call_classified(
        &endpoint,
        command,
        &args,
        budget.initial_attempt(),
        Some(discovery),
    ) {
        Ok(v) => {
            wedge_detector().on_success();
            Ok(v)
        }
        Err(CallError::App {
            message: msg,
            kind,
            details,
        }) => {
            // The app answered (rejected the command) - the transport is healthy, so
            // end any wedge episode.
            wedge_detector().on_success();
            if is_auth_rejection(&msg) && !discovery.session_token().is_empty() {
                recover_after_auth_rejection(discovery, command, &args, budget)
            } else {
                Err(ControlCallError {
                    message: msg,
                    retryable: false,
                    kind,
                    details,
                })
            }
        }
        Err(CallError::RetryableApp {
            message,
            kind,
            details,
        }) => {
            wedge_detector().on_success();
            Err(ControlCallError {
                message,
                retryable: true,
                kind,
                details,
            })
        }
        Err(CallError::Protocol(msg)) => {
            wedge_detector().on_success();
            Err(msg.into())
        }
        Err(CallError::PartialResponse) if request_id.is_none() => {
            wedge_detector().on_success();
            Err(partial_response_message().into())
        }
        Err(first) => {
            let first_is_timeout = first.is_timeout();
            let first_stage = first.stage();
            let first_error = call_error_to_control(first, command, 1, false);

            if Instant::now() >= budget.deadline {
                return Err(timeout_message(command, 1, first_stage).into());
            }

            // The endpoint we tried is unreachable/unresponsive. If control.json now
            // names a *different* addr (the app restarted or already rebound onto a new
            // port, so our env pin went stale), prefer the freshly-resolved endpoint -
            // which KEEPS the pinned env token (never adopts control.json's read-only
            // token under a control session; the stale-pin downgrade this fixes).
            let fresh = discovery
                .refreshed_endpoint()
                .ok()
                .filter(|f| f.addr != endpoint.addr || f.token != endpoint.token)
                .map(|fresh| {
                    endpoint_with_available_lease(discovery, fresh, budget.initial_attempt(), false)
                });

            // Spawn-class command: the transport failure is AMBIGUOUS (the command may
            // have applied server-side before the response leg died - Incident A/B/D),
            // so we resolve it authoritatively via get_request_status rather than
            // blindly re-running (the historical duplicate-maker).
            if let Some(id) = &request_id {
                let ep = match fresh {
                    // control.json names a different live endpoint (restart/rebind):
                    // resolve the ambiguity against it.
                    Some(f) => f,
                    // No different endpoint: the one we tried is live. If it TIMED OUT
                    // (relay wedge) and the detector fires, heal to a fresh port FIRST -
                    // otherwise get_request_status just hangs on the wedged endpoint for
                    // the full ambiguous-resolve deadline and fails UNHEALED (the round-1
                    // heal this spawn-class path must keep). The requestId dedup makes
                    // resolving/re-running against the healed port safe.
                    None => {
                        if first_is_timeout
                            && wedge_detector().on_unchanged_transport_failure(WEDGE_TRIGGER_AFTER)
                        {
                            try_bridge_rebind(discovery, &endpoint, budget.deadline)
                                .unwrap_or(endpoint)
                        } else {
                            endpoint
                        }
                    }
                };
                let r = resolve_ambiguous_request(
                    &ep,
                    command,
                    &args,
                    id,
                    first_error,
                    (discovery, discovery.has_env_pin()),
                    budget,
                );
                if r.is_ok() {
                    wedge_detector().on_success();
                }
                return r;
            }

            // Non-idempotent command. If control.json named a DIFFERENT live endpoint,
            // try it first (restart/rebind recovery). Whichever endpoint we end up
            // having ACTUALLY TRIED and still-failing is the one the wedge decision is
            // based on (F2: NOT the possibly-stale env pin we started from).
            if let Some(f) = fresh {
                match call_classified(&f, command, &args, budget, Some(discovery)) {
                    Ok(v) => {
                        wedge_detector().on_success();
                        Ok(v)
                    }
                    Err(CallError::App {
                        message: msg,
                        kind,
                        details,
                    }) => {
                        wedge_detector().on_success();
                        // We reached the fresh addr but the app rejected the call. When
                        // we kept an env token across the rotation and the rejection is
                        // an AUTH refusal, that means a REAL token rotation - surface the
                        // stale-pin cause loudly instead of the terse "unauthorized"
                        // (never a silent read-only slide onto control.json's token).
                        if !discovery.session_token().is_empty() && is_auth_rejection(&msg) {
                            recover_after_auth_rejection(discovery, command, &args, budget)
                        } else if discovery.has_env_pin() && is_auth_rejection(&msg) {
                            Err(stale_env_token_error(&msg).into())
                        } else {
                            Err(ControlCallError {
                                message: msg,
                                retryable: false,
                                kind,
                                details,
                            })
                        }
                    }
                    Err(CallError::RetryableApp {
                        message,
                        kind,
                        details,
                    }) => {
                        wedge_detector().on_success();
                        Err(ControlCallError {
                            message,
                            retryable: true,
                            kind,
                            details,
                        })
                    }
                    Err(CallError::Protocol(msg)) => Err(msg.into()),
                    Err(CallError::PartialResponse) => Err(partial_response_message().into()),
                    Err(e2) => {
                        let e2_is_timeout = e2.is_timeout();
                        maybe_heal_and_retry(
                            discovery,
                            command,
                            &args,
                            f,
                            call_error_to_control(e2, command, 2, true),
                            e2_is_timeout,
                            budget,
                        )
                    }
                }
            } else {
                // control.json named no different endpoint: the one we tried IS live.
                maybe_heal_and_retry(
                    discovery,
                    command,
                    &args,
                    endpoint,
                    first_error,
                    first_is_timeout,
                    budget,
                )
            }
        }
    }
}

/// RELAY-WEDGE SELF-HEAL (cause 2, F2-corrected): `tried` is the endpoint we
/// ACTUALLY tried (the live one control.json names, not a stale env pin) and it is
/// still failing. If that failure is the wedge signature (connected-but-silent
/// TIMEOUT, not a fast app-down refusal) and the detector's per-episode trigger
/// fires, send ONE `rebind_control` over the Windows powershell bridge - the path
/// that works mid-wedge - then resume on the fresh port the app publishes. A
/// successful retry resets the detector so a SECOND wedge on the rotated port can
/// heal again (the bug this replaces: the old `fresh.is_none()` guard was never true
/// under a stale env pin, so the detector was never re-consulted).
fn maybe_heal_and_retry(
    discovery: &Discovery,
    command: &str,
    args: &Value,
    tried: ControlEndpoint,
    err: ControlCallError,
    timeout_class: bool,
    budget: CallBudget,
) -> Result<Value, ControlCallError> {
    if timeout_class && wedge_detector().on_unchanged_transport_failure(WEDGE_TRIGGER_AFTER) {
        if let Some(healed) = try_bridge_rebind(discovery, &tried, budget.deadline) {
            return match call_classified(&healed, command, args, budget, Some(discovery)) {
                Ok(v) => {
                    wedge_detector().on_success();
                    Ok(v)
                }
                // The healed endpoint keeps the env token (see `try_bridge_rebind`),
                // so an AUTH refusal here means a REAL token rotation - name it loudly
                // rather than returning the terse "unauthorized" (mirrors the primary
                // stale-pin path; never a silent read-only slide).
                Err(CallError::App {
                    message: msg,
                    kind,
                    details,
                }) if is_auth_rejection(&msg) => {
                    if discovery.session_token().is_empty() {
                        if discovery.has_env_pin() {
                            Err(stale_env_token_error(&msg).into())
                        } else {
                            Err(ControlCallError {
                                message: msg,
                                retryable: false,
                                kind,
                                details,
                            })
                        }
                    } else {
                        let leased = renew_captain_endpoint(discovery, budget)
                            .map_err(ControlCallError::from)?;
                        call_classified(&leased, command, args, budget, Some(discovery))
                            .map_err(|error| call_error_to_control(error, command, 4, true))
                    }
                }
                Err(CallError::App {
                    message,
                    kind,
                    details,
                }) => Err(ControlCallError {
                    message,
                    retryable: false,
                    kind,
                    details,
                }),
                Err(CallError::RetryableApp {
                    message,
                    kind,
                    details,
                }) => Err(ControlCallError {
                    message,
                    retryable: true,
                    kind,
                    details,
                }),
                Err(CallError::Protocol(msg)) => Err(msg.into()),
                Err(CallError::PartialResponse) => Err(partial_response_message().into()),
                Err(other) => Err(call_error_to_control(other, command, 3, true)),
            };
        }
    }
    Err(err)
}

/// Whether an app rejection is an authentication/authorization failure - the token
/// itself was refused. Matches the control dispatcher's auth error strings
/// ("unauthorized: bad control token", "unauthorized: '<cmd>' requires the control
/// capability (this token is read-only)"). Both are prefixed `unauthorized`.
fn is_auth_rejection(msg: &str) -> bool {
    msg.starts_with("unauthorized")
}

/// Loud, cause-naming error for when the pinned env token is REFUSED at the
/// freshly-resolved addr: the app's control token actually rotated (a fresh install
/// or a token reset) since this session was spawned. We refuse to silently adopt
/// control.json's token - under item-3 hardening that is the READ-ONLY token, and
/// adopting it would silently drop this control session to read-only, the exact bug
/// this fix removes - and instead tell the operator to re-spawn/restart the session.
fn stale_env_token_error(app_msg: &str) -> String {
    format!(
        "T-Hub refused this session's pinned control token at the current control \
         address ({app_msg}). The app's control token was rotated (a fresh install or a \
         token reset) after this session was spawned, so the T_HUB_CONTROL_TOKEN in its \
         environment is stale. Re-spawn this session from the app (or restart it) to pick \
         up the live token. Refusing to fall back to control.json's token: under control \
         hardening that is the READ-ONLY token, and adopting it would silently drop this \
         control session to read-only."
    )
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum MutationReissueState {
    NotAttempted,
    Attempted,
}

fn unknown_after_reissue_message(command: &str, request_id: &str) -> String {
    format!(
        "control_request_unknown: command '{command}' remained unknown after one idempotent reissue; request_id='{request_id}'; retry_state=exhausted"
    )
}

fn pending_request_message(command: &str, request_id: &str, first_err: &str) -> String {
    format!(
        "PENDING: the request was accepted (requestId '{request_id}') and is \
         still materializing after {}s - re-issue '{command}' with the same \
         requestId for its final outcome (do NOT create a new requestId). \
         (Original client-deadline note: {first_err})",
        response_timeout_for_command(command).as_secs()
    )
}

fn status_error(status: &Value) -> ControlCallError {
    ControlCallError {
        message: status
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("control command failed (no error message)")
            .to_string(),
        retryable: status
            .get("retryable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        kind: status
            .get("errorKind")
            .and_then(Value::as_str)
            .map(str::to_string),
        details: status.get("errorDetails").cloned(),
    }
}

/// Resolve an ambiguous spawn-class transport failure (ask #1/#2): the command was
/// possibly accepted but its response leg failed. Query `get_request_status` for
/// the SAME `request_id` and act on the authoritative answer:
///
/// - completed(ok)  -> return the original result (the apply happened once)
/// - completed(err) -> return that error (it ran and failed; no ghost)
/// - inFlight       -> poll until it resolves or the deadline, then hand the caller
///   the requestId to poll themselves
/// - unknown        -> it never landed under this id: safe to re-run ONCE (the same
///   requestId keeps that retry idempotent)
///
/// If the status channel itself stays unreachable, we surface the original error.
fn resolve_ambiguous_request(
    endpoint: &ControlEndpoint,
    command: &str,
    args: &Value,
    request_id: &str,
    first_err: ControlCallError,
    auth: (&Discovery, bool),
    budget: CallBudget,
) -> Result<Value, ControlCallError> {
    let (discovery, has_env_pin) = auth;
    let status_args = serde_json::json!({ "requestId": request_id });
    let mut reissue_state = MutationReissueState::NotAttempted;
    loop {
        match call_classified(
            endpoint,
            "get_request_status",
            &status_args,
            budget,
            Some(discovery),
        ) {
            Ok(v) => match v.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                    return Err(status_error(&v));
                }
                Some("inFlight") => {
                    if Instant::now() >= budget.deadline {
                        // PENDING, not failed (ask #2): the app ACCEPTED the spawn and
                        // is still materializing it (e.g. a Windows memory trough slowed
                        // it past our deadline). Hand back the resolvable requestId with
                        // an unambiguous "accepted/pending" framing. MCP does not
                        // expose the internal status command, so recovery reissues
                        // the same command with the same idempotency key.
                        return Err(pending_request_message(
                            command,
                            request_id,
                            &first_err.message,
                        )
                        .into());
                    }
                    sleep_within(budget.deadline, Duration::from_millis(200));
                }
                // "unknown" (or a server that answered oddly): the command never
                // landed under this id. Permit exactly one idempotent mutation
                // reissue. If that reissue loses its response, return to status
                // resolution; a later unknown is authoritative and never mutates.
                _ => {
                    if reissue_state == MutationReissueState::Attempted {
                        return Err(unknown_after_reissue_message(command, request_id).into());
                    }
                    if Instant::now() >= budget.deadline {
                        return Err(format!(
                            "{}; request_id='{request_id}'",
                            timeout_message(command, 2, "request status")
                        )
                        .into());
                    }
                    reissue_state = MutationReissueState::Attempted;
                    match call_classified(endpoint, command, args, budget, Some(discovery)) {
                        Ok(value) => return Ok(value),
                        Err(CallError::App { message: msg, .. })
                            if has_env_pin && is_auth_rejection(&msg) =>
                        {
                            return Err(stale_env_token_error(&msg).into());
                        }
                        Err(error @ CallError::App { .. })
                        | Err(error @ CallError::RetryableApp { .. }) => {
                            return Err(call_error_to_control(error, command, 2, false));
                        }
                        Err(CallError::Protocol(msg)) => return Err(msg.into()),
                        Err(CallError::PartialResponse)
                        | Err(CallError::Transport(_))
                        | Err(CallError::Timeout(_)) => continue,
                    }
                }
            },
            // The app answered but rejected the STATUS query itself. Under a kept env
            // pin an AUTH refusal means a real token rotation (the env token no longer
            // authenticates) - name that cause loudly rather than the terse transport
            // error. Otherwise it is most likely an older app that predates
            // get_request_status (no server-side cache, so no idempotency guarantee):
            // don't guess, surface the original error.
            Err(CallError::App { message: msg, .. }) => {
                if has_env_pin && is_auth_rejection(&msg) && !discovery.session_token().is_empty() {
                    let leased = renew_captain_endpoint(discovery, budget)
                        .map_err(ControlCallError::from)?;
                    return resolve_ambiguous_request(
                        &leased,
                        command,
                        args,
                        request_id,
                        first_err,
                        (discovery, false),
                        budget,
                    );
                }
                if has_env_pin && is_auth_rejection(&msg) {
                    return Err(stale_env_token_error(&msg).into());
                }
                return Err(first_err);
            }
            Err(CallError::RetryableApp { .. }) => return Err(first_err),
            Err(CallError::Protocol(msg)) => return Err(msg.into()),
            Err(CallError::PartialResponse) => return Err(first_err),
            // The channel is still unreachable (fast transport failure) or wedged
            // (timeout): keep trying to reach the status endpoint until the deadline,
            // else give up with the original error.
            Err(CallError::Transport(_)) | Err(CallError::Timeout(_)) => {
                if Instant::now() >= budget.deadline {
                    return Err(format!(
                        "{}; request_id='{request_id}'",
                        timeout_message(command, 2, "request status")
                    )
                    .into());
                }
                sleep_within(budget.deadline, Duration::from_millis(200));
            }
        }
    }
}

fn sleep_within(deadline: Instant, desired: Duration) {
    if let Some(left) = remaining(deadline) {
        std::thread::sleep(left.min(desired));
    }
}

/// Forward one command to the app and return its `result` JSON, or an error
/// string - the single-shot primitive used by the crate's tests. Production code
/// goes through [`resolve_and_call`], which adds the restart-recovery retry.
#[cfg(test)]
fn call(endpoint: &ControlEndpoint, command: &str, args: &Value) -> Result<Value, String> {
    call_classified(
        endpoint,
        command,
        args,
        CallBudget {
            deadline: Instant::now() + CONTROL_DEADLINE,
            attempt_timeout: ATTEMPT_TIMEOUT,
        },
        None,
    )
    .map_err(|error| error.into_message(command, 1, false))
}

/// The single round-trip, with its failure classified so [`resolve_and_call`]
/// knows whether re-reading control.json could recover it.
fn call_classified(
    endpoint: &ControlEndpoint,
    command: &str,
    args: &Value,
    budget: CallBudget,
    discovery: Option<&Discovery>,
) -> Result<Value, CallError> {
    // Comms-plane Phase 3: present the caller session's PER-SESSION token
    // (`T_HUB_SESSION_TOKEN`, injected into this session's env at spawn) ALONGSIDE the
    // tier `token`, so the app can resolve WHICH session (role/ship) is calling and
    // enforce the plane ACLs against an unforgeable-across-sessions identity. Absent for
    // a session that never minted one (a legacy/host context) - the server then treats
    // the caller as the trusted control-token host and the cross-ship ACL fails open.
    let session = discovery
        .map(Discovery::session_token)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| std::env::var("T_HUB_SESSION_TOKEN").unwrap_or_default());
    let request = serde_json::json!({
        "token": endpoint.token,
        "session": session,
        "command": command,
        "args": args,
    });

    let socket: SocketAddr = endpoint.addr.parse().map_err(|_| {
        CallError::Protocol("control_protocol: malformed endpoint address".to_string())
    })?;
    let connect_budget = remaining(budget.deadline)
        .map(|left| left.min(budget.attempt_timeout))
        .filter(|budget| !budget.is_zero())
        .ok_or(CallError::Timeout("connect"))?;
    let stream = TcpStream::connect_timeout(&socket, connect_budget).map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            CallError::Timeout("connect")
        } else {
            CallError::Transport("connect")
        }
    })?;
    let io_budget = remaining(budget.deadline)
        .map(|left| left.min(budget.attempt_timeout))
        .filter(|budget| !budget.is_zero())
        .ok_or(CallError::Timeout("write"))?;
    let _ = stream.set_write_timeout(Some(io_budget));

    let mut writer = stream
        .try_clone()
        .map_err(|_| CallError::Transport("stream setup"))?;
    let mut line = serde_json::to_vec(&request)
        .map_err(|e| CallError::Protocol(format!("control_protocol: serialize failed: {e}")))?;
    line.push(b'\n');
    writer.write_all(&line).map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            CallError::Timeout("write")
        } else {
            CallError::Transport("write")
        }
    })?;
    writer.flush().map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            CallError::Timeout("write")
        } else {
            CallError::Transport("write")
        }
    })?;

    stream
        .set_nonblocking(true)
        .map_err(|_| CallError::Transport("stream setup"))?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut next_probe = Instant::now() + budget.attempt_timeout;
    loop {
        let now = Instant::now();
        if now >= budget.deadline {
            return Err(CallError::Timeout("read"));
        }
        if now >= next_probe {
            if discovery
                .and_then(|source| source.refreshed_endpoint().ok())
                .is_some_and(|fresh| fresh.addr != endpoint.addr || fresh.token != endpoint.token)
            {
                return Err(CallError::Timeout("read"));
            }
            next_probe = now + budget.attempt_timeout;
        }
        match (&stream).read(&mut chunk) {
            Ok(0) if response.is_empty() => return Err(CallError::Transport("read")),
            Ok(0) => return Err(CallError::PartialResponse),
            Ok(n) => {
                let received = &chunk[..n];
                let frame_bytes = received
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(received.len());
                if response.len().saturating_add(frame_bytes) > MAX_RESPONSE_FRAME_BYTES {
                    return Err(CallError::Protocol(format!(
                        "control_protocol: response frame exceeds {MAX_RESPONSE_FRAME_BYTES}-byte limit"
                    )));
                }
                response.extend_from_slice(&received[..frame_bytes]);
                if frame_bytes < received.len() {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                let wake_at = budget.deadline.min(next_probe);
                std::thread::sleep(
                    wake_at
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            Err(_) => return Err(CallError::Transport("read")),
        }
    }

    let resp_line = String::from_utf8(response).map_err(|_| {
        CallError::Protocol("control_protocol: response frame was not UTF-8".to_string())
    })?;
    let resp: ControlResponse = serde_json::from_str(resp_line.trim_end())
        .map_err(|e| CallError::Protocol(format!("control_protocol: malformed response: {e}")))?;

    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        let message = resp
            .error
            .unwrap_or_else(|| "control command failed (no error message)".to_string());
        if resp.retryable {
            Err(CallError::RetryableApp {
                message,
                kind: resp.error_kind,
                details: resp.error_details,
            })
        } else {
            Err(CallError::App {
                message,
                kind: resp.error_kind,
                details: resp.error_details,
            })
        }
    }
}

/// Whether the Windows-side powershell bridge is reachable (WSL interop present).
/// Gating on this keeps the bridge OFF on native Linux (CI, a Linux-hosted app) so a
/// heal attempt never spawns a missing `powershell.exe`; there the client degrades to
/// the existing file-re-read recovery.
fn wsl_powershell_available() -> bool {
    if cfg!(test) {
        return false;
    }
    std::env::var_os("WSL_INTEROP").is_some() || std::env::var_os("WSL_DISTRO_NAME").is_some()
}

#[cfg(test)]
thread_local! {
    static TEST_BRIDGE_RESULT: std::cell::RefCell<Option<ControlEndpoint>> =
        const { std::cell::RefCell::new(None) };
}

/// Attempt ONE relay-wedge self-heal: trigger an app-side `rebind_control` over the
/// Windows powershell bridge, then adopt the fresh endpoint the app just published.
/// Returns the new endpoint on success, or `None` (app genuinely down, rate-limited,
/// not under WSL, or the bridge failed) so the caller degrades gracefully. Even when
/// this returns `None` after a rebind our output-parse missed, the NEXT call
/// self-recovers: the stale env addr is now dead and control.json names the new port,
/// which the existing file-re-read path already handles.
///
/// The rebind request carries both the scoped lease and its bound durable session
/// identity. The endpoint used after the port-only rebind keeps that same lease.
fn try_bridge_rebind(
    discovery: &Discovery,
    stale: &ControlEndpoint,
    deadline: Instant,
) -> Option<ControlEndpoint> {
    #[cfg(test)]
    if let Some(endpoint) = TEST_BRIDGE_RESULT.with(|slot| slot.borrow_mut().take()) {
        return Some(endpoint);
    }
    if !wsl_powershell_available() {
        return None;
    }
    if !send_rebind_via_powershell(stale, discovery.session_token(), deadline) {
        return None;
    }
    healed_endpoint_after_rebind(discovery, stale)
}

/// Given a successful rebind, the endpoint to resume on: the fresh ADDR the app just
/// published, keeping the scoped credential that authenticated the rebind. Returns
/// `Some` only when the address actually moved.
fn healed_endpoint_after_rebind(
    discovery: &Discovery,
    stale: &ControlEndpoint,
) -> Option<ControlEndpoint> {
    let fresh = discovery.resolve_from_file().ok()?;
    (fresh.addr != stale.addr).then_some(ControlEndpoint {
        addr: fresh.addr,
        token: stale.token.clone(),
    })
}

/// Send a single `rebind_control` to the app via `powershell.exe` (a Windows-native
/// TcpClient), which reaches the app even while the WSL loopback relay is wedged.
///
/// The token/host/port are passed as ENVIRONMENT variables (never interpolated into
/// the `-Command` string) so there is no quoting/injection surface; the script builds
/// the one-line JSON request from them. Bounded by powershell's own 8s socket
/// timeouts so a hung bridge can't park the MCP server. Returns true iff the app
/// answered with a rebind (`"rebound"`), i.e. the port actually moved.
fn send_rebind_via_powershell(
    stale: &ControlEndpoint,
    session_token: &str,
    deadline: Instant,
) -> bool {
    let Some(request) = bridge_rebind_request(stale, session_token) else {
        return false;
    };
    // control.json addr is always loopback `host:port`; split from the right so a
    // stray host colon (there is none for 127.0.0.1) can't misparse the port.
    let (host, port) = match stale.addr.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => return false,
    };
    // Reject a non-numeric port up front (defensive; never spawn on garbage input).
    if port.parse::<u16>().is_err() {
        return false;
    }
    const SCRIPT: &str = r#"
$ErrorActionPreference='Stop'
try {
  $req = $env:THUB_REBIND_REQUEST + "`n"
  $c = New-Object System.Net.Sockets.TcpClient
  $c.ReceiveTimeout = 8000; $c.SendTimeout = 8000
  $c.Connect($env:THUB_REBIND_HOST, [int]$env:THUB_REBIND_PORT)
  $s = $c.GetStream()
  $b = [System.Text.Encoding]::UTF8.GetBytes($req)
  $s.Write($b, 0, $b.Length); $s.Flush()
  $buf = New-Object byte[] 65536
  $n = $s.Read($buf, 0, $buf.Length)
  [System.Text.Encoding]::UTF8.GetString($buf, 0, $n)
  $c.Close()
} catch { Write-Output ('ERR ' + $_.Exception.Message) }
"#;
    // F3: bound the subprocess with a RUST-side wall-clock timeout + kill.
    // PowerShell's own 8s socket timeouts do NOT cover `TcpClient.Connect()` or
    // process/JIT startup, so a hung bridge would otherwise park this tools/call
    // thread indefinitely (the parked-thread class #45/#48 killed). This kills the
    // child at the deadline instead of waiting on `.output()` forever.
    let child = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("THUB_REBIND_REQUEST", request)
        .env("THUB_REBIND_HOST", host)
        .env("THUB_REBIND_PORT", port)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return false, // powershell.exe not found / spawn failed
    };
    let Some(budget) = remaining(deadline).map(|left| left.min(BRIDGE_TIMEOUT)) else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };
    match wait_with_timeout(&mut child, budget) {
        Some(out) => out.contains("\"rebound\""),
        None => false, // timed out (child killed) or read failed
    }
}

fn bridge_rebind_request(stale: &ControlEndpoint, session_token: &str) -> Option<String> {
    if stale.token.is_empty() || session_token.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::json!({
        "token": stale.token,
        "session": session_token,
        "command": "rebind_control",
        "args": {},
        "v": 1,
    }))
    .ok()
}

/// Total wall-clock bound for the powershell bridge subprocess (F3). Comfortably
/// above PowerShell's internal 8s socket timeout plus process/JIT startup, but finite
/// so a hung bridge can never park the calling thread.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(15);

/// Wait for `child` up to `budget`, returning its captured stdout on clean exit, or
/// `None` if it timed out (after killing it) or its output could not be read. Polls
/// `try_wait` rather than blocking on `wait`/`output`, so the timeout is enforced
/// Rust-side regardless of what the child does. The bridge's output is tiny (one
/// response line), so reading stdout after exit never risks a full-pipe deadlock.
fn wait_with_timeout(child: &mut std::process::Child, budget: Duration) -> Option<String> {
    use std::io::Read;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut out);
                }
                return Some(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    enum ScriptedReply {
        Line(&'static str),
        Partial(&'static str),
        Close,
    }

    /// A fake control server that services a scripted SEQUENCE of connections. For
    /// each entry it accepts one connection, reads the one request line (captured
    /// for assertions), then either writes `Some(reply)` or, on `None`, drops the
    /// connection WITHOUT responding - reproducing a failed response leg (Incident
    /// A/B/D). Returns its addr plus the shared capture of every request seen.
    fn scripted_server(replies: Vec<Option<&'static str>>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    if let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) {
                        cap.lock().unwrap().push(v);
                    }
                }
                if let Some(body) = reply {
                    let _ = writer.write_all(body.as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                }
                // `None`: drop the connection with no response (failed response leg).
            }
        });
        (addr, captured)
    }

    #[test]
    fn raw_wire_error_details_survive_mcp_control_adapter() {
        let (addr, _captured) = scripted_server(vec![Some(
            r#"{"ok":false,"error":"Git capability is required for baseline","errorKind":"git_required","errorDetails":{"code":"git_required","operation":"baseline","capability":"git","action":"initialize_git"},"retryable":false}"#,
        )]);
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("control-token".into()),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "baseline",
            &Value::Null,
            Duration::from_secs(1),
            Duration::from_millis(250),
        )
        .unwrap_err();

        assert_eq!(error.message, "Git capability is required for baseline");
        assert!(!error.retryable);
        assert_eq!(error.kind.as_deref(), Some("git_required"));
        assert_eq!(
            error.details,
            Some(json!({
                "code": "git_required",
                "operation": "baseline",
                "capability": "git",
                "action": "initialize_git"
            }))
        );
    }

    #[test]
    fn endpoint_replacement_preserves_native_error_details() {
        let (fresh_addr, _captured) = scripted_server(vec![Some(
            r#"{"ok":false,"error":"Git capability is required for baseline","errorKind":"git_required","errorDetails":{"code":"git_required","operation":"baseline","capability":"git","action":"initialize_git"},"retryable":false}"#,
        )]);
        let dir = std::env::temp_dir().join(format!("th-mcp-error-rebind-{}", epoch_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{fresh_addr}","token":"READ","pid":1}}"#),
        )
        .unwrap();
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);
        let discovery = Discovery {
            addr: Some(dead_addr),
            token: Some("STALE".into()),
            file: Some(file.clone()),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "baseline",
            &Value::Null,
            Duration::from_secs(1),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert_eq!(error.kind.as_deref(), Some("git_required"));
        assert_eq!(error.details.as_ref().unwrap()["operation"], "baseline");
        assert!(!error.retryable);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restart_rebind_preserves_native_error_details_after_fresh_endpoint_response() {
        let (fresh_addr, _captured) = scripted_server(vec![Some(
            r#"{"ok":false,"error":"Git capability is required for delivery","errorKind":"git_required","errorDetails":{"code":"git_required","operation":"delivery","capability":"git","action":"initialize_git"},"retryable":false}"#,
        )]);
        let dir = std::env::temp_dir().join(format!("th-mcp-error-restart-{}", epoch_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{fresh_addr}","token":"READ","pid":2}}"#),
        )
        .unwrap();
        let discovery = Discovery {
            addr: Some("127.0.0.1:9".into()),
            token: Some("STALE".into()),
            file: Some(file.clone()),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "delivery",
            &Value::Null,
            Duration::from_secs(1),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert_eq!(error.kind.as_deref(), Some("git_required"));
        assert_eq!(error.details.as_ref().unwrap()["operation"], "delivery");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn auth_recovery_preserves_native_error_details_from_reauthenticated_endpoint() {
        let (addr, _captured) = scripted_server(vec![
            Some(r#"{"ok":false,"error":"unauthorized: bad control token"}"#),
            Some(r#"{"ok":false,"error":"unauthorized: read token"}"#),
            Some(r#"{"ok":true,"result":{"lease":"SCOPED","expiresAt":9999999999999}}"#),
            Some(
                r#"{"ok":false,"error":"Git capability is required for integration","errorKind":"git_required","errorDetails":{"code":"git_required","operation":"integration","capability":"git","action":"initialize_git"},"retryable":false}"#,
            ),
        ]);
        let dir = std::env::temp_dir().join(format!("th-mcp-error-auth-{}", epoch_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{addr}","token":"READ","pid":3}}"#),
        )
        .unwrap();
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("STALE".into()),
            file: Some(file.clone()),
            session: Some("CAPTAIN".into()),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "integration",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert_eq!(error.kind.as_deref(), Some("git_required"));
        assert_eq!(error.details.as_ref().unwrap()["operation"], "integration");
        assert!(!error.retryable);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn byte_scripted_server(replies: Vec<ScriptedReply>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    if let Ok(value) = serde_json::from_str::<Value>(line.trim_end()) {
                        cap.lock().unwrap().push(value);
                    }
                }
                match reply {
                    ScriptedReply::Line(body) => {
                        let _ = writer.write_all(body.as_bytes());
                        let _ = writer.write_all(b"\n");
                        let _ = writer.flush();
                    }
                    ScriptedReply::Partial(body) => {
                        let _ = writer.write_all(body.as_bytes());
                        let _ = writer.flush();
                    }
                    ScriptedReply::Close => {}
                }
            }
        });
        (addr, captured)
    }

    fn mcp_binary() -> PathBuf {
        if let Some(path) = option_env!("CARGO_BIN_EXE_t-hub-mcp") {
            return PathBuf::from(path);
        }
        let test_exe = std::env::current_exe().unwrap();
        let debug_dir = test_exe.parent().and_then(|path| path.parent()).unwrap();
        let name = if cfg!(windows) {
            "t-hub-mcp.exe"
        } else {
            "t-hub-mcp"
        };
        let binary = debug_dir.join(name);
        assert!(
            binary.is_file(),
            "MCP process binary missing at {}; run `cargo build -p t-hub-mcp` before this focused test",
            binary.display()
        );
        binary
    }

    fn run_mcp_spawn_process(addr: &str, token: &str) -> (std::process::Output, Duration) {
        let mut child = Command::new(mcp_binary())
            .env("T_HUB_CONTROL_ADDR", addr)
            .env("T_HUB_CONTROL_TOKEN", token)
            .env("T_HUB_CONTROL_FILE", "/nonexistent/th-control.json")
            .env_remove("T_HUB_SESSION_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "spawn_terminal",
                "arguments": {
                    "cwd": "/tmp",
                    "requestId": "partial-eof-process-request"
                }
            }
        });
        let mut stdin = child.stdin.take().unwrap();
        serde_json::to_writer(&mut stdin, &request).unwrap();
        stdin.write_all(b"\n").unwrap();
        drop(stdin);

        let started = Instant::now();
        let deadline = started + CONTROL_DEADLINE + Duration::from_secs(2);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("MCP process exceeded test deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let elapsed = started.elapsed();
        (child.wait_with_output().unwrap(), elapsed)
    }

    fn assert_safe_mcp_process_output(output: &std::process::Output, addr: &str) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.stdout.len() < 4096);
        assert!(output.stderr.is_empty(), "stderr: {output:?}");
        assert!(!stdout.contains("process-control-token"));
        assert!(!stdout.contains("initial-cut"));
        assert!(!stdout.contains("retry-cut"));
        assert!(!stdout.contains(addr));
    }

    fn assert_single_reissue_sequence(requests: &[Value]) {
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0]["command"], "spawn_terminal");
        assert_eq!(requests[1]["command"], "get_request_status");
        assert_eq!(requests[2]["command"], "spawn_terminal");
        assert_eq!(requests[3]["command"], "get_request_status");
        let request_id = &requests[0]["args"]["requestId"];
        assert!(request_id.is_string());
        for request in &requests[1..] {
            assert_eq!(&request["args"]["requestId"], request_id);
        }
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["command"] == "spawn_terminal")
                .count(),
            2,
            "the mutation may be reissued at most once"
        );
    }

    fn silent_server(hold: Duration) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                if let Ok(value) = serde_json::from_str::<Value>(line.trim_end()) {
                    cap.lock().unwrap().push(value);
                }
            }
            std::thread::sleep(hold);
        });
        (addr, captured)
    }

    fn delayed_server(reply: &'static str, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            std::thread::sleep(delay);
            let mut writer = stream;
            writer.write_all(reply.as_bytes()).unwrap();
            writer.write_all(b"\n").unwrap();
        });
        addr
    }

    fn trickle_server(interval: Duration, writes: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut writer = stream;
            for _ in 0..writes {
                if writer.write_all(b"{").is_err() || writer.flush().is_err() {
                    break;
                }
                std::thread::sleep(interval);
            }
        });
        addr
    }

    fn raw_response_server(response: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let mut writer = stream;
            let _ = writer.write_all(&response);
            let _ = writer.flush();
        });
        addr
    }

    fn exact_limit_response() -> Vec<u8> {
        let mut response = br#"{"ok":true,"result":null}"#.to_vec();
        response.resize(MAX_RESPONSE_FRAME_BYTES, b' ');
        response.push(b'\n');
        response
    }

    fn silent_then_status_server(hold: Duration) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        std::thread::spawn(move || {
            let (first, _) = listener.accept().unwrap();
            let mut first_reader = BufReader::new(first);
            let mut first_line = String::new();
            first_reader.read_line(&mut first_line).unwrap();
            cap.lock()
                .unwrap()
                .push(serde_json::from_str(first_line.trim_end()).unwrap());
            std::thread::spawn(move || {
                std::thread::sleep(hold);
                drop(first_reader);
            });

            let (second, _) = listener.accept().unwrap();
            let mut second_reader = BufReader::new(second.try_clone().unwrap());
            let mut second_line = String::new();
            second_reader.read_line(&mut second_line).unwrap();
            cap.lock()
                .unwrap()
                .push(serde_json::from_str(second_line.trim_end()).unwrap());
            let mut writer = second;
            writer
                .write_all(
                    b"{\"ok\":true,\"result\":{\"status\":\"completed\",\"ok\":true,\"result\":{\"id\":\"resolved\"}}}\n",
                )
                .unwrap();
        });
        (addr, captured)
    }

    // ---- Relay-wedge self-heal: detection state machine (cause 2) --------------

    #[test]
    fn wedge_detector_triggers_at_threshold_and_only_once_per_episode() {
        let mut d = WedgeDetector::default();
        // trigger_after = 2: first unchanged failure arms but does not fire.
        assert!(
            !d.on_unchanged_transport_failure(2),
            "1st failure must not fire"
        );
        // Second consecutive failure fires exactly once.
        assert!(
            d.on_unchanged_transport_failure(2),
            "2nd failure must fire the heal"
        );
        // Further failures in the SAME episode never re-fire (one attempt per episode).
        assert!(
            !d.on_unchanged_transport_failure(2),
            "3rd failure must not re-fire"
        );
        assert!(
            !d.on_unchanged_transport_failure(2),
            "4th failure must not re-fire"
        );
    }

    #[test]
    fn wedge_detector_trigger_after_one_fires_on_first_failure() {
        let mut d = WedgeDetector::default();
        assert!(
            d.on_unchanged_transport_failure(1),
            "N=1 fires on the first failure"
        );
        assert!(
            !d.on_unchanged_transport_failure(1),
            "but only once per episode"
        );
    }

    #[test]
    fn wedge_detector_success_resets_the_episode() {
        let mut d = WedgeDetector::default();
        assert!(d.on_unchanged_transport_failure(1), "first episode fires");
        assert!(
            !d.on_unchanged_transport_failure(1),
            "same episode does not re-fire"
        );
        // A healthy round-trip ends the episode.
        d.on_success();
        // A later wedge is a NEW episode and may heal again.
        assert!(
            d.on_unchanged_transport_failure(1),
            "a new episode fires again after success"
        );
    }

    #[test]
    fn wedge_detector_success_clears_partial_count_below_threshold() {
        let mut d = WedgeDetector::default();
        assert!(!d.on_unchanged_transport_failure(2), "1/2 - armed");
        d.on_success(); // a success between failures must reset the run
        assert!(!d.on_unchanged_transport_failure(2), "back to 1/2, not 2/2");
        assert!(d.on_unchanged_transport_failure(2), "now 2/2 - fires");
    }

    #[test]
    fn wedge_detector_second_wedge_after_recovery_heals_again() {
        // F2 regression: the old `fresh.is_none()` guard meant a spawned crew's stale
        // env pin made `fresh` always Some, so the detector was never re-consulted and
        // a SECOND wedge on the rotated port could never heal. With the detection now
        // keyed to the endpoint actually tried + reset on the recovery success, the
        // sequence [wedge -> heal -> recover -> wedge again] heals BOTH times.
        let mut d = WedgeDetector::default();
        // First wedge episode: heals.
        assert!(d.on_unchanged_transport_failure(1), "first wedge heals");
        // Heal succeeded and the retry round-tripped -> episode ends.
        d.on_success();
        // Some healthy calls in between (each a success, no-op on an ended episode).
        d.on_success();
        // A SECOND wedge (on the now-rotated port) is a fresh episode and heals again.
        assert!(
            d.on_unchanged_transport_failure(1),
            "second wedge heals again after recovery"
        );
    }

    #[test]
    fn closed_connection_classifies_as_transport_not_timeout() {
        // The self-heal (on BOTH the read and the restored spawn-class path) fires
        // ONLY on the Timeout class = connected-but-silent, the relay-wedge signature.
        // A connection that CLOSES without responding (app down / old listener
        // retired) must classify as Transport so it recovers via the file re-read and
        // never triggers a spurious rebind. This guards that gate hermetically.
        let (addr, _captured) = scripted_server(vec![None]); // accept, read, drop, no reply
        let ep = ControlEndpoint {
            addr,
            token: "t".into(),
        };
        let err = call_classified(
            &ep,
            "list_terminals",
            &serde_json::json!({}),
            CallBudget {
                deadline: Instant::now() + Duration::from_millis(200),
                attempt_timeout: Duration::from_millis(50),
            },
            None,
        );
        assert!(
            matches!(err, Err(CallError::Transport(_))),
            "a connection closed without responding must be Transport (app-down class), \
             not Timeout - the wedge heal must not fire on it"
        );
    }

    #[test]
    fn connected_but_silent_inherited_port_recovers_via_current_endpoint() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-silent-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        let (stale_addr, _stale_requests) = silent_server(Duration::from_millis(180));
        let (fresh_addr, fresh_requests) = scripted_server(vec![Some(
            r#"{"ok":true,"result":{"capabilities":["read"]}}"#,
        )]);
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{fresh_addr}","token":"published-read"}}"#),
        )
        .unwrap();
        let discovery = Discovery {
            addr: Some(stale_addr),
            token: Some("inherited-control".into()),
            file: Some(file.clone()),
            ..Default::default()
        };
        let started = Instant::now();

        let value = resolve_and_call_with_deadline(
            &discovery,
            "capabilities",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();

        assert_eq!(value["capabilities"], serde_json::json!(["read"]));
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(
            fresh_requests.lock().unwrap()[0]["token"],
            "inherited-control"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn healthy_response_can_outlive_attempt_slice_within_overall_deadline() {
        let addr = delayed_server(
            r#"{"ok":true,"result":{"usage":"ready"}}"#,
            Duration::from_millis(90),
        );
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("control".into()),
            file: Some(PathBuf::from("/nonexistent/control.json")),
            ..Default::default()
        };

        let value = resolve_and_call_with_deadline(
            &discovery,
            "codex_usage",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();
        assert_eq!(value["usage"], "ready");
    }

    #[test]
    fn partial_frame_trickle_cannot_bypass_absolute_deadline() {
        let addr = trickle_server(Duration::from_millis(10), 30);
        let endpoint = ControlEndpoint {
            addr,
            token: "control".into(),
        };
        let started = Instant::now();

        let error = call_classified(
            &endpoint,
            "list_tabs",
            &Value::Null,
            CallBudget {
                deadline: Instant::now() + Duration::from_millis(70),
                attempt_timeout: Duration::from_millis(20),
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(error, CallError::Timeout("read")));
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn exact_limit_response_frame_is_accepted() {
        let addr = raw_response_server(exact_limit_response());
        let endpoint = ControlEndpoint {
            addr,
            token: "control".into(),
        };

        let value = call_classified(
            &endpoint,
            "list_tabs",
            &Value::Null,
            CallBudget {
                deadline: Instant::now() + Duration::from_secs(2),
                attempt_timeout: Duration::from_millis(100),
            },
            None,
        )
        .unwrap();
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn over_limit_response_frame_is_bounded_and_credential_safe() {
        let secret = "oversized-server-token-must-not-leak";
        let mut response = vec![b'x'; MAX_RESPONSE_FRAME_BYTES];
        response.extend_from_slice(secret.as_bytes());
        response.push(b'\n');
        let addr = raw_response_server(response);
        let endpoint = ControlEndpoint {
            addr: addr.clone(),
            token: "control-token-must-not-leak".into(),
        };

        let error = call_classified(
            &endpoint,
            "list_tabs",
            &Value::Null,
            CallBudget {
                deadline: Instant::now() + Duration::from_secs(2),
                attempt_timeout: Duration::from_millis(100),
            },
            None,
        )
        .unwrap_err();
        let CallError::Protocol(message) = error else {
            panic!("oversized frame must be a protocol error");
        };
        assert!(message.contains("response frame exceeds"));
        assert!(!message.contains(secret));
        assert!(!message.contains("control-token-must-not-leak"));
        assert!(!message.contains(&addr));
    }

    #[test]
    fn unterminated_response_frame_is_a_safe_protocol_error() {
        let secret = "unterminated-server-token-must-not-leak";
        let addr = raw_response_server(format!("{{\"ok\":true,\"{secret}\":").into_bytes());

        let error = resolve_and_call_with_deadline(
            &discovery_for(addr),
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.contains("unterminated response frame after request write"));
        assert!(!error.contains(secret));
    }

    #[test]
    fn malformed_response_frame_does_not_echo_peer_content() {
        let secret = "malformed-server-token-must-not-leak";
        let addr = raw_response_server(format!("{{not-json:{secret}}}\n").into_bytes());
        let endpoint = ControlEndpoint {
            addr,
            token: "control".into(),
        };

        let error = call_classified(
            &endpoint,
            "list_tabs",
            &Value::Null,
            CallBudget {
                deadline: Instant::now() + Duration::from_secs(2),
                attempt_timeout: Duration::from_millis(100),
            },
            None,
        )
        .unwrap_err();
        let CallError::Protocol(message) = error else {
            panic!("malformed frame must be a protocol error");
        };
        assert!(message.contains("malformed response"));
        assert!(!message.contains(secret));
    }

    #[test]
    fn unchanged_silent_idempotent_call_uses_reserved_status_budget() {
        let (addr, captured) = silent_then_status_server(Duration::from_millis(300));
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("control".into()),
            file: Some(PathBuf::from("/nonexistent/control.json")),
            ..Default::default()
        };
        let started = Instant::now();

        let value = resolve_and_call_with_deadline(
            &discovery,
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp"}),
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();

        assert_eq!(value["id"], "resolved");
        let requests = captured.lock().unwrap();
        assert_eq!(requests[0]["command"], "spawn_terminal");
        assert_eq!(requests[1]["command"], "get_request_status");
        assert!(started.elapsed() >= Duration::from_millis(140));
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn unchanged_silent_read_uses_reserved_maybe_heal_budget() {
        *wedge_detector() = WedgeDetector::default();
        let (silent_addr, _requests) = silent_server(Duration::from_millis(300));
        let (healed_addr, healed_requests) =
            scripted_server(vec![Some(r#"{"ok":true,"result":{"tabs":[]}}"#)]);
        TEST_BRIDGE_RESULT.with(|slot| {
            *slot.borrow_mut() = Some(ControlEndpoint {
                addr: healed_addr,
                token: "control".into(),
            });
        });
        let discovery = Discovery {
            addr: Some(silent_addr),
            token: Some("control".into()),
            file: Some(PathBuf::from("/nonexistent/control.json")),
            ..Default::default()
        };
        let started = Instant::now();

        let value = resolve_and_call_with_deadline(
            &discovery,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();

        assert_eq!(value["tabs"], serde_json::json!([]));
        assert_eq!(healed_requests.lock().unwrap()[0]["command"], "list_tabs");
        assert!(started.elapsed() >= Duration::from_millis(140));
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn stale_discovery_consumes_the_same_overall_budget() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-stale-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(&file, r#"{"addr":"127.0.0.1:1","token":"read"}"#).unwrap();
        let discovery = Discovery {
            file: Some(file),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "list_tabs",
            &Value::Null,
            Duration::ZERO,
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(error.contains("control_timeout"));
        assert!(error.contains("discovery"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovery_budget_exhaustion_is_classified_and_credential_safe() {
        let (addr, _requests) = silent_server(Duration::from_millis(180));
        let discovery = Discovery {
            addr: Some(addr.clone()),
            token: Some("inherited-control".into()),
            file: Some(PathBuf::from("/nonexistent/control.json")),
            ..Default::default()
        };
        let started = Instant::now();

        let error = resolve_and_call_with_deadline(
            &discovery,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(70),
            Duration::from_millis(60),
        )
        .unwrap_err();

        assert!(error.contains("control_timeout"), "error: {error}");
        assert!(error.contains("retry_state=exhausted"));
        assert!(!error.contains(&addr));
        assert!(!error.contains("inherited-control"));
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn send_rebind_via_powershell_rejects_malformed_addr_without_spawning() {
        // No colon and a non-numeric port both fail the parse guards BEFORE any
        // powershell spawn, so these are deterministic on any platform.
        assert!(!send_rebind_via_powershell(
            &ControlEndpoint {
                addr: "no-colon-here".to_string(),
                token: "t".to_string(),
            },
            "session",
            Instant::now() + Duration::from_millis(50),
        ));
        assert!(!send_rebind_via_powershell(
            &ControlEndpoint {
                addr: "127.0.0.1:not-a-port".to_string(),
                token: "t".to_string(),
            },
            "session",
            Instant::now() + Duration::from_millis(50),
        ));
        assert!(!send_rebind_via_powershell(
            &ControlEndpoint {
                addr: "127.0.0.1:1234".to_string(),
                token: "lease".to_string(),
            },
            "",
            Instant::now() + Duration::from_millis(50),
        ));
    }

    #[test]
    fn powershell_bridge_request_binds_scoped_lease_to_session_identity() {
        let request = bridge_rebind_request(
            &ControlEndpoint {
                addr: "127.0.0.1:1234".into(),
                token: "scoped-lease".into(),
            },
            "durable-session",
        )
        .unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["token"], "scoped-lease");
        assert_eq!(request["session"], "durable-session");
        assert_eq!(request["command"], "rebind_control");
        assert_eq!(request["args"], serde_json::json!({}));
        assert_eq!(request["v"], 1);
        assert!(bridge_rebind_request(
            &ControlEndpoint {
                addr: "127.0.0.1:1234".into(),
                token: "scoped-lease".into(),
            },
            "",
        )
        .is_none());
    }

    fn discovery_for(addr: String) -> Discovery {
        Discovery {
            addr: Some(addr),
            token: Some("tok".into()),
            // A file that does not exist so the restart-recovery re-read finds
            // nothing fresher and the ambiguity path reuses the same endpoint.
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        }
    }

    #[test]
    fn spawn_class_call_injects_a_request_id() {
        let (addr, captured) = scripted_server(vec![Some(r#"{"ok":true,"result":{"id":"s"}}"#)]);
        resolve_and_call(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp"}),
        )
        .unwrap();
        let reqs = captured.lock().unwrap();
        assert!(
            reqs[0]["args"]["requestId"].as_str().is_some(),
            "a spawn-class call must carry a requestId: {:?}",
            reqs[0]
        );
    }

    #[test]
    fn client_idempotent_command_contract_matches_the_server_contract() {
        assert_eq!(
            IDEMPOTENT_COMMANDS,
            [
                "spawn_terminal",
                "create_worktree",
                "history_resume",
                "reconcile_cortana",
                "commission_captain",
                "dispatch_crew",
                "start_agent",
            ]
        );
        for command in IDEMPOTENT_COMMANDS {
            let (_, request_id) = ensure_request_id(command, &Value::Null);
            assert!(
                request_id.is_some(),
                "{command} did not receive a requestId"
            );
        }
    }

    #[test]
    fn history_resume_keeps_its_request_id_and_long_response_window() {
        let args = serde_json::json!({
            "historyId": "history:v1:one",
            "requestId": "history-request-one"
        });
        let (normalized, request_id) = ensure_request_id("history_resume", &args);
        assert_eq!(normalized, args);
        assert_eq!(request_id.as_deref(), Some("history-request-one"));
        assert_eq!(
            response_timeout_for_command("history_resume"),
            LONG_ORCHESTRATION_TIMEOUT
        );
        assert_eq!(
            response_timeout_for_command("history_list"),
            CONTROL_DEADLINE
        );
        assert!(timeout_message("history_resume", 1, "read").contains("120s"));
        let pending = pending_request_message(
            "history_resume",
            "history-request-one",
            "control_timeout: response lost",
        );
        assert!(pending.contains("after 120s"));
        assert!(pending.contains("re-issue 'history_resume' with the same requestId"));
        assert!(!pending.contains("poll get_request_status"));
    }

    #[test]
    fn non_idempotent_call_does_not_inject_a_request_id() {
        let (addr, captured) = scripted_server(vec![Some(r#"{"ok":true,"result":{}}"#)]);
        resolve_and_call(&discovery_for(addr), "list_tabs", &Value::Null).unwrap();
        let reqs = captured.lock().unwrap();
        assert!(
            reqs[0]["args"].get("requestId").is_none(),
            "a read command must not get a requestId"
        );
    }

    #[test]
    fn ambiguous_response_leg_resolves_to_the_completed_outcome() {
        // The spawn's response leg fails (conn 1 closes with no reply), but the
        // command DID apply. The client resolves it via get_request_status (conn 2)
        // using the SAME requestId, and returns the original result - no duplicate.
        let (addr, captured) = scripted_server(vec![
            None, // spawn_terminal: accepted, response leg dies
            Some(
                r#"{"ok":true,"result":{"status":"completed","ok":true,"result":{"id":"sess-1"}}}"#,
            ),
        ]);
        let v = resolve_and_call(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp"}),
        )
        .unwrap();
        assert_eq!(v["id"], "sess-1", "returns the completed spawn's result");
        let reqs = captured.lock().unwrap();
        let rid = reqs[0]["args"]["requestId"].as_str().unwrap();
        assert_eq!(reqs[1]["command"], "get_request_status");
        assert_eq!(
            reqs[1]["args"]["requestId"].as_str().unwrap(),
            rid,
            "the status query reuses the original requestId"
        );
    }

    #[test]
    fn partial_eof_idempotent_call_resolves_completed_outcome() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Line(
                r#"{"ok":true,"result":{"status":"completed","ok":true,"result":{"id":"sess-partial"}}}"#,
            ),
        ]);

        let value = resolve_and_call_with_deadline(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp", "requestId": "partial-completed"}),
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();

        assert_eq!(value["id"], "sess-partial");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["command"], "spawn_terminal");
        assert_eq!(requests[1]["command"], "get_request_status");
        assert_eq!(
            requests[1]["args"]["requestId"],
            requests[0]["args"]["requestId"]
        );
    }

    #[test]
    fn partial_eof_idempotent_call_resolves_failed_outcome() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Line(
                r#"{"ok":true,"result":{"status":"completed","ok":false,"error":"spawn failed safely"}}"#,
            ),
        ]);

        let error = resolve_and_call_with_deadline(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp", "requestId": "partial-failed"}),
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap_err();

        assert_eq!(error, "spawn failed safely");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["command"], "get_request_status");
    }

    #[test]
    fn partial_eof_unknown_status_reruns_once_with_same_request_id() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"id":"sess-retried"}}"#),
        ]);

        let value = resolve_and_call_with_deadline(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp", "requestId": "partial-unknown"}),
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();

        assert_eq!(value["id"], "sess-retried");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1]["command"], "get_request_status");
        assert_eq!(requests[2]["command"], "spawn_terminal");
        assert_eq!(
            requests[2]["args"]["requestId"],
            requests[0]["args"]["requestId"]
        );
    }

    #[test]
    fn partial_eof_status_unavailable_exhausts_budget_without_duplicate_mutation() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Close,
        ]);
        let started = Instant::now();

        let error = resolve_and_call_with_deadline(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp", "requestId": "partial-unavailable"}),
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap_err();

        assert!(error.contains("control_timeout"));
        assert!(error.contains("request status"));
        assert!(error.contains("partial-unavailable"));
        assert!(!error.contains("cut"));
        assert!(started.elapsed() < Duration::from_millis(400));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["command"], "spawn_terminal");
        assert_eq!(requests[1]["command"], "get_request_status");
    }

    #[test]
    fn idempotent_pre_write_protocol_error_remains_fail_closed() {
        let discovery = Discovery {
            addr: Some("not-a-control-address".into()),
            token: Some("pre-write-token-must-not-leak".into()),
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp", "requestId": "pre-write-malformed"}),
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap_err();

        assert!(error.contains("malformed endpoint address"));
        assert!(!error.contains("pre-write-token-must-not-leak"));
    }

    #[test]
    fn process_partial_eof_resolves_completed_without_duplicate_mutation() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Line(
                r#"{"ok":true,"result":{"status":"completed","ok":true,"result":{"id":"sess-process"}}}"#,
            ),
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["id"],
            "sess-process"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.stdout.len() < 4096);
        assert!(output.stderr.is_empty(), "stderr: {output:?}");
        assert!(!stdout.contains("process-control-token"));
        assert!(!stdout.contains("cut"));
        assert!(!stdout.contains(&addr));
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["command"], "spawn_terminal");
        assert_eq!(requests[1]["command"], "get_request_status");
        assert_eq!(
            requests[1]["args"]["requestId"],
            requests[0]["args"]["requestId"]
        );
    }

    #[test]
    fn process_partial_eof_resolves_failed_without_duplicate_mutation() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Line(
                r#"{"ok":true,"result":{"status":"completed","ok":false,"error":"spawn failed safely"}}"#,
            ),
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "spawn failed safely"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.stdout.len() < 4096);
        assert!(output.stderr.is_empty(), "stderr: {output:?}");
        assert!(!stdout.contains("process-control-token"));
        assert!(!stdout.contains("cut"));
        assert!(!stdout.contains(&addr));
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["command"], "get_request_status");
    }

    #[test]
    fn process_partial_eof_unknown_reruns_once_with_same_request_id() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"id":"sess-process-retried"}}"#),
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["id"],
            "sess-process-retried"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.stdout.len() < 4096);
        assert!(output.stderr.is_empty(), "stderr: {output:?}");
        assert!(!stdout.contains("process-control-token"));
        assert!(!stdout.contains("cut"));
        assert!(!stdout.contains(&addr));
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1]["command"], "get_request_status");
        assert_eq!(requests[2]["command"], "spawn_terminal");
        assert_eq!(
            requests[2]["args"]["requestId"],
            requests[0]["args"]["requestId"]
        );
    }

    #[test]
    fn process_reissue_partial_then_completed_queries_status_again() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"initial-cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"retry-cut""#),
            ScriptedReply::Line(
                r#"{"ok":true,"result":{"status":"completed","ok":true,"result":{"id":"sess-after-reissue"}}}"#,
            ),
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["id"],
            "sess-after-reissue"
        );
        assert_safe_mcp_process_output(&output, &addr);
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        let requests = captured.lock().unwrap();
        assert_single_reissue_sequence(&requests);
    }

    #[test]
    fn process_reissue_partial_then_failed_queries_status_again() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"initial-cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"retry-cut""#),
            ScriptedReply::Line(
                r#"{"ok":true,"result":{"status":"completed","ok":false,"error":"spawn failed after reissue"}}"#,
            ),
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "spawn failed after reissue"
        );
        assert_safe_mcp_process_output(&output, &addr);
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        let requests = captured.lock().unwrap();
        assert_single_reissue_sequence(&requests);
    }

    #[test]
    fn process_reissue_partial_then_still_unknown_never_mutates_again() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"initial-cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"retry-cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], true);
        let message = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("control_request_unknown"));
        assert!(message.contains("partial-eof-process-request"));
        assert!(message.contains("retry_state=exhausted"));
        assert_safe_mcp_process_output(&output, &addr);
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        let requests = captured.lock().unwrap();
        assert_single_reissue_sequence(&requests);
    }

    #[test]
    fn process_reissue_partial_then_status_unavailable_is_bounded() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"initial-cut""#),
            ScriptedReply::Line(r#"{"ok":true,"result":{"status":"unknown"}}"#),
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"retry-cut""#),
            ScriptedReply::Close,
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], true);
        let message = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("control_timeout"));
        assert!(message.contains("request status"));
        assert!(message.contains("partial-eof-process-request"));
        assert_safe_mcp_process_output(&output, &addr);
        assert!(elapsed >= CONTROL_DEADLINE - Duration::from_secs(1));
        assert!(elapsed <= CONTROL_DEADLINE + Duration::from_secs(1));
        let requests = captured.lock().unwrap();
        assert_single_reissue_sequence(&requests);
    }

    #[test]
    fn process_partial_eof_status_unavailable_is_bounded_without_duplicate_mutation() {
        let (addr, captured) = byte_scripted_server(vec![
            ScriptedReply::Partial(r#"{"ok":true,"result":{"id":"cut""#),
            ScriptedReply::Close,
        ]);

        let (output, elapsed) = run_mcp_spawn_process(&addr, "process-control-token");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["isError"], true);
        let message = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("control_timeout"));
        assert!(message.contains("request status"));
        assert!(message.contains("partial-eof-process-request"));
        assert!(!message.contains("process-control-token"));
        assert!(!message.contains("cut"));
        assert!(!message.contains(&addr));
        assert!(output.stdout.len() < 4096);
        assert!(output.stderr.is_empty(), "stderr: {output:?}");
        assert!(elapsed >= CONTROL_DEADLINE - Duration::from_secs(1));
        assert!(elapsed <= CONTROL_DEADLINE + Duration::from_secs(1));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["command"], "spawn_terminal");
        assert_eq!(requests[1]["command"], "get_request_status");
    }

    #[test]
    fn ambiguous_response_leg_reruns_once_when_status_is_unknown() {
        // The spawn's response leg fails AND the server never saw it (status
        // unknown: it did not land). The client safely re-runs it ONCE with the
        // same requestId, which now succeeds.
        let (addr, captured) = scripted_server(vec![
            None,                                                 // spawn 1: response leg dies
            Some(r#"{"ok":true,"result":{"status":"unknown"}}"#), // status: never landed
            Some(r#"{"ok":true,"result":{"id":"sess-2","accepted":"spawn_terminal"}}"#), // retry ok
        ]);
        let v = resolve_and_call(
            &discovery_for(addr),
            "spawn_terminal",
            &serde_json::json!({"cwd": "/tmp"}),
        )
        .unwrap();
        assert_eq!(v["id"], "sess-2");
        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[2]["command"], "spawn_terminal");
        assert_eq!(
            reqs[2]["args"]["requestId"], reqs[0]["args"]["requestId"],
            "the re-run reuses the same requestId so it stays idempotent"
        );
    }

    /// Spin up a one-shot fake control server on loopback that asserts the token
    /// and echoes a canned response. Returns its addr.
    fn fake_server(expect_token: &str, reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let expect = expect_token.to_string();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(line.trim_end()).unwrap();
                assert_eq!(req["token"], expect, "server saw wrong token");
                writer.write_all(reply.as_bytes()).unwrap();
                writer.write_all(b"\n").unwrap();
                writer.flush().unwrap();
            }
        });
        addr
    }

    #[test]
    fn call_returns_result_on_ok() {
        let addr = fake_server("tok", r#"{"ok":true,"result":{"hello":"world"}}"#);
        let ep = ControlEndpoint {
            addr,
            token: "tok".into(),
        };
        let v = call(&ep, "list_tabs", &Value::Null).unwrap();
        assert_eq!(v["hello"], "world");
    }

    #[test]
    fn call_returns_err_on_error_envelope() {
        let addr = fake_server("tok", r#"{"ok":false,"error":"boom"}"#);
        let ep = ControlEndpoint {
            addr,
            token: "tok".into(),
        };
        let err = call(&ep, "list_tabs", &Value::Null).unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn resolve_and_call_preserves_retryable_error_metadata() {
        let addr = fake_server(
            "tok",
            r#"{"ok":false,"error":"history_resume_failed: placement uncertain","retryable":true}"#,
        );
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("tok".into()),
            ..Default::default()
        };

        let error = resolve_and_call(
            &discovery,
            "history_resume",
            &serde_json::json!({
                "historyId": "history:v1:one",
                "requestId": "request-one"
            }),
        )
        .unwrap_err();

        assert!(error.retryable);
        assert_eq!(error.message, "history_resume_failed: placement uncertain");
    }

    #[test]
    fn call_forwards_token_and_args() {
        // The fake server asserts the token; here we also confirm a result with
        // the args echoed back round-trips.
        let addr = fake_server("secret", r#"{"ok":true,"result":{"echoed":true}}"#);
        let ep = ControlEndpoint {
            addr,
            token: "secret".into(),
        };
        let v = call(&ep, "get_status", &serde_json::json!({"sessionId": "s1"})).unwrap();
        assert_eq!(v["echoed"], true);
    }

    #[test]
    fn resolve_endpoint_reads_handshake_file() {
        // Write a temp handshake and point a Discovery at it. No env mutation:
        // the config is injected, so this stays hermetic under parallel runs.
        let dir = std::env::temp_dir().join(format!("th-mcp-hs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:9999","token":"filetok","pid":1}"#,
        )
        .unwrap();

        let discovery = Discovery {
            file: Some(file),
            ..Default::default()
        };
        let ep = discovery.resolve().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:9999");
        assert_eq!(ep.token, "filetok");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_authoritative_file_ignores_stale_wsl_home_shadow_across_atomic_replace() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-cross-path-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let wsl_home = dir.join("wsl-home");
        let shadow = wsl_home.join(".t-hub/control.json");
        let authoritative = dir.join("windows-home/.t-hub/control.json");
        std::fs::create_dir_all(shadow.parent().unwrap()).unwrap();
        std::fs::create_dir_all(authoritative.parent().unwrap()).unwrap();
        std::fs::write(
            &shadow,
            r#"{"addr":"127.0.0.1:45949","token":"STALE","pid":1}"#,
        )
        .unwrap();
        std::fs::write(
            &authoritative,
            r#"{"addr":"127.0.0.1:56192","token":"CURRENT","pid":2}"#,
        )
        .unwrap();
        let discovery = Discovery {
            file: Some(authoritative.clone()),
            home: Some(wsl_home),
            ..Default::default()
        };
        let current = discovery.resolve_from_file().unwrap();
        assert_eq!(current.addr, "127.0.0.1:56192");
        assert_eq!(current.token, "CURRENT");

        let replacement = authoritative.with_extension("json.tmp.test");
        std::fs::write(
            &replacement,
            r#"{"addr":"127.0.0.1:56193","token":"CURRENT-2","pid":2}"#,
        )
        .unwrap();
        std::fs::rename(replacement, &authoritative).unwrap();
        let rebound = discovery.resolve_from_file().unwrap();
        assert_eq!(rebound.addr, "127.0.0.1:56193");
        assert_eq!(rebound.token, "CURRENT-2");
        assert_eq!(
            std::fs::read_to_string(shadow).unwrap(),
            r#"{"addr":"127.0.0.1:45949","token":"STALE","pid":1}"#
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn legacy_wsl_process_resolves_one_unambiguous_windows_control_file() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-windows-users-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let current = dir.join("natha/.t-hub/control.json");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::fs::write(&current, "{}").unwrap();
        assert_eq!(unique_windows_control_file(dir.clone()), Some(current));

        let foreign = dir.join("foreign/.t-hub/control.json");
        std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        std::fs::write(&foreign, "{}").unwrap();
        assert_eq!(unique_windows_control_file(dir.clone()), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_endpoint_prefers_addr_token_override() {
        let discovery = Discovery {
            addr: Some("127.0.0.1:1".into()),
            token: Some("envtok".into()),
            // File path is ignored when addr+token are present.
            file: Some(PathBuf::from("/nonexistent/control.json")),
            ..Default::default()
        };
        let ep = discovery.resolve().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:1");
        assert_eq!(ep.token, "envtok");
    }

    #[test]
    fn resolve_endpoint_missing_file_is_descriptive_error() {
        let discovery = Discovery {
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        };
        let err = discovery.resolve().unwrap_err();
        assert!(err.contains("control channel not found"), "err: {err}");
    }

    #[test]
    fn resolve_and_call_keeps_the_env_token_after_a_port_rotation() {
        // The stale-pin bug (the primary fix): a control session was spawned with a
        // FULL control token pinned in its env; the app then restarted onto a fresh
        // port (adopt-first: the token is UNCHANGED, only the port rotates) and, under
        // item-3 hardening, control.json now publishes only the READ token. The
        // recovery must re-resolve the fresh ADDR from control.json but KEEP the pinned
        // env token - never adopt the file's read-only token (the silent read-only
        // downgrade this fixes).
        //
        // BYPASS-WOULD-FAIL: revert `refreshed_endpoint` to the old wholesale
        // `resolve_from_file` and the app receives "READ-tok" instead of the env
        // "FULL-tok" - the captured-token assertion below goes RED.
        let dir = std::env::temp_dir().join(format!("th-mcp-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");

        // The restarted app on a fresh port; control.json points at it but publishes
        // only the READ token (hardening). scripted_server captures the request so we
        // can assert WHICH token the app actually saw.
        let (live_addr, captured) =
            scripted_server(vec![Some(r#"{"ok":true,"result":{"hello":"world"}}"#)]);
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{live_addr}","token":"READ-tok","pid":1}}"#),
        )
        .unwrap();

        // The dead pre-restart endpoint the session's env still pins: bind to grab a
        // port, then drop it so connects are refused (the old ephemeral port).
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);

        let discovery = Discovery {
            addr: Some(dead_addr.clone()),
            token: Some("FULL-tok".into()),
            file: Some(file.clone()),
            ..Default::default()
        };

        // Red path: the naive single-shot against the env-pinned endpoint fails,
        // because that port died when the app restarted.
        let stale = discovery.resolve().unwrap();
        assert_eq!(stale.addr, dead_addr, "resolve still prefers the env pin");
        assert!(
            call(&stale, "list_tabs", &Value::Null).is_err(),
            "the dead endpoint must fail to connect"
        );

        // Green path: resolve_and_call re-resolves the fresh addr from control.json but
        // keeps the FULL env token, and reaches the live post-restart endpoint.
        let v = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap();
        assert_eq!(v["hello"], "world");
        let reqs = captured.lock().unwrap();
        assert_eq!(
            reqs[0]["token"], "FULL-tok",
            "recovery must present the pinned env token, NOT control.json's read-only token"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_windows_handshake_acquires_scoped_lease_without_global_token_env() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-current-handshake-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        let (live_addr, captured) = scripted_server(vec![
            Some(r#"{"ok":true,"result":{"lease":"CURRENT-scoped","expiresAt":9999999999999}}"#),
            Some(r#"{"ok":true,"result":{"capability":"control"}}"#),
        ]);
        std::fs::write(
            &file,
            format!(
                r#"{{"addr":"{live_addr}","token":"CURRENT-read","pid":1,"protocol_version":2,"instance_id":"current","listener_generation":1}}"#
            ),
        )
        .unwrap();
        let discovery = Discovery {
            file: Some(file),
            session: Some("captain-session".into()),
            ..Default::default()
        };
        let result = resolve_and_call(&discovery, "my_capability", &Value::Null).unwrap();
        assert_eq!(result["capability"], "control");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["token"], "CURRENT-read");
        assert_eq!(requests[0]["command"], "renew_captain_control_lease");
        assert_eq!(requests[1]["token"], "CURRENT-scoped");
        assert!(requests.iter().all(|request| request["token"] != "global"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn port_only_rebind_reuses_identity_lease_at_fresh_address() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-lease-rebind-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        let (live_addr, captured) =
            scripted_server(vec![Some(r#"{"ok":true,"result":{"ok":true}}"#)]);
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{live_addr}","token":"READ","pid":1}}"#),
        )
        .unwrap();
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);
        let discovery = Discovery {
            addr: Some(dead_addr),
            token: Some("OLD-global".into()),
            file: Some(file),
            session: Some("captain-session".into()),
            ..Default::default()
        };
        discovery.cache_lease(
            &ControlEndpoint {
                addr: "127.0.0.1:1".into(),
                token: "ignored".into(),
            },
            "SCOPED-port-lease".into(),
            9_999_999_999_999,
        );
        let result = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap();
        assert_eq!(result["ok"], true);
        let requests = captured.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "port-only rebind must not mint a new lease"
        );
        assert_eq!(requests[0]["token"], "SCOPED-port-lease");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_and_call_reauthenticates_the_same_identity_after_real_token_rotation() {
        // A real global credential rotation must recover through the durable
        // session identity without returning or adopting the new global token.
        let dir = std::env::temp_dir().join(format!("th-mcp-rot2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");

        let (live_addr, captured) = scripted_server(vec![
            Some(r#"{"ok":false,"error":"unauthorized: bad control token"}"#),
            Some(r#"{"ok":true,"result":{"capability":"read"}}"#),
            Some(r#"{"ok":true,"result":{"lease":"SCOPED-lease","expiresAt":9999999999999}}"#),
            Some(r#"{"ok":true,"result":{"capability":"control"}}"#),
        ]);
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{live_addr}","token":"READ-tok","pid":1}}"#),
        )
        .unwrap();

        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);

        let discovery = Discovery {
            addr: Some(dead_addr),
            token: Some("STALE-tok".into()),
            file: Some(file.clone()),
            session: Some("durable-session-secret".into()),
            ..Default::default()
        };

        let result = resolve_and_call(&discovery, "my_capability", &Value::Null).unwrap();
        assert_eq!(result["capability"], "control");
        let requests = captured.lock().unwrap();
        assert_eq!(requests[0]["token"], "STALE-tok");
        assert_eq!(requests[1]["token"], "READ-tok");
        assert_eq!(requests[2]["command"], "renew_captain_control_lease");
        assert_eq!(requests[2]["token"], "READ-tok");
        assert_eq!(requests[2]["session"], "durable-session-secret");
        assert_eq!(requests[3]["token"], "SCOPED-lease");
        assert!(requests
            .iter()
            .all(|request| request["token"] != "NEW-global"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refreshed_endpoint_keeps_env_token_but_takes_fresh_addr() {
        // Unit-level guard on the core fix: with an env pin, refreshed_endpoint adopts
        // the file's addr yet keeps the env token; with NO env pin it takes both.
        let dir = std::env::temp_dir().join(format!("th-mcp-refe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:5555","token":"READ-tok","pid":1}"#,
        )
        .unwrap();

        let pinned = Discovery {
            addr: Some("127.0.0.1:1".into()),
            token: Some("FULL-tok".into()),
            file: Some(file.clone()),
            ..Default::default()
        };
        let ep = pinned.refreshed_endpoint().unwrap();
        assert_eq!(
            ep.addr, "127.0.0.1:5555",
            "takes the fresh addr from control.json"
        );
        assert_eq!(ep.token, "FULL-tok", "keeps the pinned env token");

        let file_only = Discovery {
            file: Some(file.clone()),
            ..Default::default()
        };
        let ep2 = file_only.refreshed_endpoint().unwrap();
        assert_eq!(ep2.addr, "127.0.0.1:5555");
        assert_eq!(
            ep2.token, "READ-tok",
            "no env pin: adopt the file token as before"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healed_endpoint_after_rebind_keeps_scoped_lease_with_stable_file_discovery() {
        // The relay-wedge self-heal resumes on the fresh address but keeps the
        // identity-bound scoped lease used to authenticate the bridge request.
        //
        // BYPASS-WOULD-FAIL: revert `healed_endpoint_after_rebind` to
        // `discovery.resolve_from_file()` and it returns "READ-tok" - the token
        // assertion below goes RED.
        let dir = std::env::temp_dir().join(format!("th-mcp-heal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        // control.json after the rebind: fresh port, only the READ token published.
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:7777","token":"READ-tok","pid":1}"#,
        )
        .unwrap();

        let discovery = Discovery {
            file: Some(file.clone()),
            session: Some("BOUND-session".into()),
            ..Default::default()
        };
        let stale = ControlEndpoint {
            addr: "127.0.0.1:1".into(),
            token: "SCOPED-lease".into(),
        };

        let healed = healed_endpoint_after_rebind(&discovery, &stale).expect("addr moved -> Some");
        assert_eq!(healed.addr, "127.0.0.1:7777", "resumes on the rebound port");
        assert_eq!(
            healed.token, "SCOPED-lease",
            "the healed endpoint must keep the scoped lease, not the ambient read token"
        );

        // No addr movement (control.json still names the stale addr) -> None (nothing
        // to heal to), regardless of the token.
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:1","token":"READ-tok","pid":1}"#,
        )
        .unwrap();
        assert!(
            healed_endpoint_after_rebind(&discovery, &stale).is_none(),
            "an unchanged addr yields no healed endpoint"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_and_call_app_error_is_not_retried() {
        // An app that answers with a rejection is NOT a moved endpoint: the error
        // surfaces verbatim without a control.json re-read/retry.
        let addr = fake_server("tok", r#"{"ok":false,"error":"boom"}"#);
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("tok".into()),
            // A file that does not exist: if this path retried on disk it would
            // change the error; asserting "boom" proves it did not.
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        };
        let err = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap_err();
        assert_eq!(err, "boom");
    }
}
