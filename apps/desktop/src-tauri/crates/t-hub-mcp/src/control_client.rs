//! Client side of the loopback control channel: the bridge from `tools/call` to
//! the running T-Hub app.
//!
//! Discovery: read the handshake file the app wrote (`$T_HUB_CONTROL_FILE`, or
//! `~/.t-hub/control.json`) for the `addr` + `token`, or take both from
//! `$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN` (used by the proof harness and
//! when the app's path differs). These inputs are captured once into a
//! [`Discovery`] value at startup so the rest of the crate resolves endpoints
//! from an injected config rather than process-global env (which keeps the tests
//! hermetic under parallel execution). Each call opens a short-lived TCP
//! connection to `addr`, sends one NDJSON request line, and reads one NDJSON
//! response line. Connections are not pooled - `tools/call` is infrequent and a
//! fresh connection keeps the client stateless and robust to app restarts.

#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// One control operation, including discovery, connect, write, read, endpoint
/// invalidation, retry, bridge recovery, and ambiguous-response lookup, must
/// finish within this wall-clock budget.
const CONTROL_DEADLINE: Duration = Duration::from_secs(10);

/// A single endpoint gets only a short slice of the overall budget so an
/// inherited port that accepts but stays silent cannot consume the recovery
/// window before the current endpoint is tried.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// Every control client accepts at most 1 MiB before the NDJSON response newline.
/// This bounds memory, parsing work, and any structured error derived from a peer.
const MAX_RESPONSE_FRAME_BYTES: usize = 1024 * 1024;

/// Spawn-class commands whose retries must dedup via a client `requestId`
/// (mirrors the app-side `is_idempotent_command`).
fn is_idempotent_command(command: &str) -> bool {
    matches!(
        command,
        "spawn_terminal" | "create_worktree" | "commission_captain" | "dispatch_crew"
    )
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
#[derive(Debug, Clone)]
pub struct ControlEndpoint {
    pub addr: String,
    pub token: String,
}

/// The on-disk handshake the app writes. We only need `addr` + `token`.
#[derive(Debug, Deserialize)]
struct Handshake {
    addr: String,
    token: String,
}

/// The inputs used to locate the control channel, captured up front so that
/// resolution is a pure function of its fields rather than of process-global
/// environment variables. Production builds construct this once with
/// [`Discovery::from_env`]; tests construct it directly, which keeps them
/// hermetic (no shared `T_HUB_CONTROL_*` env mutation that could race across
/// threads when the suite runs in parallel).
#[derive(Debug, Clone, Default)]
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
        }
    }

    /// Resolve the control endpoint, explicit addr+token override first, then
    /// the handshake file.
    ///
    /// Returns a descriptive error (not a panic) when the app isn't running /
    /// the handshake file is missing, so the MCP server can surface "T-Hub is
    /// not running" as a tool error rather than crashing.
    pub fn resolve(&self) -> Result<ControlEndpoint, String> {
        // 1. Explicit addr + token override - used by the proof harness and the
        //    app's spawn-tree env injection (`T_HUB_CONTROL_ADDR` +
        //    `T_HUB_CONTROL_TOKEN`). The fast path while the app stays bound; if it
        //    later goes stale (restart onto a new port), `resolve_and_call` falls
        //    back to `resolve_from_file` instead of re-trusting this pair.
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
    /// This is the recovery path after a transport failure: the app rebinds to a
    /// fresh ephemeral port on every restart and rewrites control.json, but a
    /// session's MCP captured the old addr+token in its env at spawn time. Once
    /// that env pin points at a dead port, the live endpoint lives only in the
    /// file - so we re-read it here rather than re-trusting the stale env pair.
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
        Ok(ControlEndpoint {
            addr: hs.addr,
            token: hs.token,
        })
    }

    /// Whether an explicit env pin (`$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN`)
    /// is in force - i.e. [`resolve`](Self::resolve) returned that pair rather than the
    /// file. The env token is the credential the app injected at spawn (a control-tier
    /// session gets the FULL token), so it must be PRESERVED across a port rotation,
    /// never swapped for control.json's (read-only, under item-3 harden) token.
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
    /// This is the core stale-pin fix. A restart/install rotates the control PORT but
    /// not the token (adopt-first), while control.json under item-3 hardening carries
    /// only the READ token. The old recovery re-read BOTH fields wholesale
    /// ([`resolve_from_file`](Self::resolve_from_file)), so a fully-authorized control
    /// session silently degraded to read-only after any restart. Keeping the env token
    /// lets the control session reach the fresh port with its real capability; if that
    /// token is genuinely refused (a real rotation), the caller surfaces a loud error
    /// rather than a silent downgrade.
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
        let home = self
            .home
            .clone()
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".t-hub").join("control.json")
    }
}

/// The app's response envelope: `{ok, result?, error?}`.
#[derive(Debug, Deserialize)]
struct ControlResponse {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
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
    App(String),
    /// The peer answered with a malformed protocol frame. Retrying on another
    /// endpoint would hide a compatibility failure.
    Protocol(String),
}

impl CallError {
    fn into_message(self, command: &str, attempts: u8, endpoint_replaced: bool) -> String {
        match self {
            CallError::Transport(stage) => {
                unavailable_message(command, attempts, stage, endpoint_replaced)
            }
            CallError::Timeout(stage) => timeout_message(command, attempts, stage),
            CallError::App(message) | CallError::Protocol(message) => message,
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
            CallError::App(_) => "server",
            CallError::Protocol(_) => "protocol",
        }
    }
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
        CONTROL_DEADLINE.as_secs()
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
/// STALE-PIN FIX: the retry KEEPS the pinned env token (see
/// [`Discovery::refreshed_endpoint`]). A restart rotates the port but not the
/// token (adopt-first), while control.json - under item-3 hardening - publishes
/// only the READ token. The old recovery re-read BOTH fields wholesale, so a
/// fully-authorized control session silently degraded to read-only after any
/// restart. If the kept env token is genuinely REFUSED at the fresh addr (a real
/// token rotation), we surface a loud, cause-naming error rather than a silent
/// read-only downgrade. App-level rejections are otherwise returned verbatim (a
/// new endpoint won't change them).
pub fn resolve_and_call(
    discovery: &Discovery,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    resolve_and_call_with_deadline(discovery, command, args, CONTROL_DEADLINE, ATTEMPT_TIMEOUT)
}

fn resolve_and_call_with_deadline(
    discovery: &Discovery,
    command: &str,
    args: &Value,
    overall: Duration,
    attempt_timeout: Duration,
) -> Result<Value, String> {
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
    if Instant::now() >= budget.deadline {
        return Err(timeout_message(command, 0, "discovery"));
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
        Err(CallError::App(msg)) => {
            // The app answered (rejected the command) - the transport is healthy, so
            // end any wedge episode.
            wedge_detector().on_success();
            Err(msg)
        }
        Err(CallError::Protocol(msg)) => {
            wedge_detector().on_success();
            Err(msg)
        }
        Err(first) => {
            let first_is_timeout = first.is_timeout();
            let first_stage = first.stage();
            let first_msg = first.into_message(command, 1, false);

            if Instant::now() >= budget.deadline {
                return Err(timeout_message(command, 1, first_stage));
            }

            // The endpoint we tried is unreachable/unresponsive. If control.json now
            // names a *different* addr (the app restarted or already rebound onto a new
            // port, so our env pin went stale), prefer the freshly-resolved endpoint -
            // which KEEPS the pinned env token (never adopts control.json's read-only
            // token under a control session; the stale-pin downgrade this fixes).
            let fresh = discovery
                .refreshed_endpoint()
                .ok()
                .filter(|f| f.addr != endpoint.addr || f.token != endpoint.token);

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
                    first_msg,
                    discovery.has_env_pin(),
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
                match call_classified(&f, command, &args, budget, None) {
                    Ok(v) => {
                        wedge_detector().on_success();
                        Ok(v)
                    }
                    Err(CallError::App(msg)) => {
                        wedge_detector().on_success();
                        // We reached the fresh addr but the app rejected the call. When
                        // we kept an env token across the rotation and the rejection is
                        // an AUTH refusal, that means a REAL token rotation - surface the
                        // stale-pin cause loudly instead of the terse "unauthorized"
                        // (never a silent read-only slide onto control.json's token).
                        if discovery.has_env_pin() && is_auth_rejection(&msg) {
                            Err(stale_env_token_error(&msg))
                        } else {
                            Err(msg)
                        }
                    }
                    Err(CallError::Protocol(msg)) => Err(msg),
                    Err(e2) => {
                        let e2_is_timeout = e2.is_timeout();
                        let e2_msg = e2.into_message(command, 2, true);
                        maybe_heal_and_retry(
                            discovery,
                            command,
                            &args,
                            f,
                            e2_msg,
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
                    first_msg,
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
    err: String,
    timeout_class: bool,
    budget: CallBudget,
) -> Result<Value, String> {
    if timeout_class && wedge_detector().on_unchanged_transport_failure(WEDGE_TRIGGER_AFTER) {
        if let Some(healed) = try_bridge_rebind(discovery, &tried, budget.deadline) {
            return match call_classified(&healed, command, args, budget, None) {
                Ok(v) => {
                    wedge_detector().on_success();
                    Ok(v)
                }
                // The healed endpoint keeps the env token (see `try_bridge_rebind`),
                // so an AUTH refusal here means a REAL token rotation - name it loudly
                // rather than returning the terse "unauthorized" (mirrors the primary
                // stale-pin path; never a silent read-only slide).
                Err(CallError::App(msg)) if discovery.has_env_pin() && is_auth_rejection(&msg) => {
                    Err(stale_env_token_error(&msg))
                }
                Err(CallError::App(msg)) | Err(CallError::Protocol(msg)) => Err(msg),
                Err(other) => Err(other.into_message(command, 3, true)),
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
    first_err: String,
    has_env_pin: bool,
    budget: CallBudget,
) -> Result<Value, String> {
    let status_args = serde_json::json!({ "requestId": request_id });
    loop {
        match call_classified(endpoint, "get_request_status", &status_args, budget, None) {
            Ok(v) => match v.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                    return Err(v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("control command failed (no error message)")
                        .to_string());
                }
                Some("inFlight") => {
                    if Instant::now() >= budget.deadline {
                        // PENDING, not failed (ask #2): the app ACCEPTED the spawn and
                        // is still materializing it (e.g. a Windows memory trough slowed
                        // it past our deadline). Hand back the resolvable requestId with
                        // an unambiguous "accepted/pending" framing so the caller polls
                        // get_request_status rather than reading this as a spawn failure
                        // and guessing/retrying.
                        return Err(format!(
                            "PENDING: the request was accepted (requestId '{request_id}') and is \
                             still materializing after {}s - poll get_request_status with that \
                             requestId for its final outcome (do NOT re-issue the command). \
                             (Original client-deadline note: {first_err})",
                            CONTROL_DEADLINE.as_secs()
                        ));
                    }
                    sleep_within(budget.deadline, Duration::from_millis(200));
                }
                // "unknown" (or a server that answered oddly): the command never
                // landed under this id, so re-running it once is safe + idempotent.
                _ => {
                    return call_classified(endpoint, command, args, budget, None)
                        .map_err(|error| error.into_message(command, 2, false));
                }
            },
            // The app answered but rejected the STATUS query itself. Under a kept env
            // pin an AUTH refusal means a real token rotation (the env token no longer
            // authenticates) - name that cause loudly rather than the terse transport
            // error. Otherwise it is most likely an older app that predates
            // get_request_status (no server-side cache, so no idempotency guarantee):
            // don't guess, surface the original error.
            Err(CallError::App(msg)) => {
                if has_env_pin && is_auth_rejection(&msg) {
                    return Err(stale_env_token_error(&msg));
                }
                return Err(first_err);
            }
            Err(CallError::Protocol(msg)) => return Err(msg),
            // The channel is still unreachable (fast transport failure) or wedged
            // (timeout): keep trying to reach the status endpoint until the deadline,
            // else give up with the original error.
            Err(CallError::Transport(_)) | Err(CallError::Timeout(_)) => {
                if Instant::now() >= budget.deadline {
                    return Err(format!(
                        "{}; request_id='{request_id}'",
                        timeout_message(command, 2, "request status")
                    ));
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
    let session = std::env::var("T_HUB_SESSION_TOKEN").unwrap_or_default();
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
            Ok(0) => {
                return Err(CallError::Protocol(
                    "control_protocol: unterminated response frame".to_string(),
                ));
            }
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
        Err(CallError::App(resp.error.unwrap_or_else(|| {
            "control command failed (no error message)".to_string()
        })))
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
/// The rebind_control request itself is authenticated with `stale.token` (the env
/// token, control-tier under an env pin - correct, the app requires control for a
/// rebind). The endpoint to RESUME on then KEEPS that env token via
/// [`healed_endpoint_after_rebind`] rather than adopting control.json's (read-only,
/// under item-3 harden) token - closing the same silent read-only downgrade the
/// primary path fixes (P71-1).
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
    if !send_rebind_via_powershell(stale, deadline) {
        return None;
    }
    healed_endpoint_after_rebind(discovery, stale)
}

/// Given a successful rebind, the endpoint to resume on: the fresh ADDR the app just
/// published, KEEPING the env token when an env pin is in force (a rebind rotates the
/// port, not the token - the same invariant [`Discovery::refreshed_endpoint`] holds on
/// the primary stale-pin path). Returns `Some` only when the addr actually moved (the
/// rebind took effect). Split out of the powershell-spawning [`try_bridge_rebind`] so
/// this token-preservation is unit-testable without a live bridge.
fn healed_endpoint_after_rebind(
    discovery: &Discovery,
    stale: &ControlEndpoint,
) -> Option<ControlEndpoint> {
    let fresh = discovery.refreshed_endpoint().ok()?;
    (fresh.addr != stale.addr).then_some(fresh)
}

/// Send a single `rebind_control` to the app via `powershell.exe` (a Windows-native
/// TcpClient), which reaches the app even while the WSL loopback relay is wedged.
///
/// The token/host/port are passed as ENVIRONMENT variables (never interpolated into
/// the `-Command` string) so there is no quoting/injection surface; the script builds
/// the one-line JSON request from them. Bounded by powershell's own 8s socket
/// timeouts so a hung bridge can't park the MCP server. Returns true iff the app
/// answered with a rebind (`"rebound"`), i.e. the port actually moved.
fn send_rebind_via_powershell(stale: &ControlEndpoint, deadline: Instant) -> bool {
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
  $req = '{"token":"' + $env:THUB_REBIND_TOKEN + '","command":"rebind_control","args":{},"v":1}' + "`n"
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
        .env("THUB_REBIND_TOKEN", &stale.token)
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
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

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
            panic!("unterminated frame must be a protocol error");
        };
        assert!(message.contains("unterminated response frame"));
        assert!(!message.contains(secret));
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
            Instant::now() + Duration::from_millis(50),
        ));
        assert!(!send_rebind_via_powershell(
            &ControlEndpoint {
                addr: "127.0.0.1:not-a-port".to_string(),
                token: "t".to_string(),
            },
            Instant::now() + Duration::from_millis(50),
        ));
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
    fn resolve_and_call_reports_a_real_token_rotation_loudly() {
        // A REAL rotation (a fresh install / token reset), distinct from a mere port
        // rotation: the pinned env token no longer authenticates at the fresh addr. The
        // recovery must NOT silently adopt control.json's read-only token; it surfaces a
        // clear error naming the stale env pin so the operator restarts/re-spawns.
        let dir = std::env::temp_dir().join(format!("th-mcp-rot2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");

        // The live app on a fresh port refuses the (now rotated-away) env token.
        let (live_addr, _cap) = scripted_server(vec![Some(
            r#"{"ok":false,"error":"unauthorized: bad control token"}"#,
        )]);
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
            ..Default::default()
        };

        let err = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap_err();
        let lower = err.to_lowercase();
        assert!(
            lower.contains("stale") && lower.contains("read-only"),
            "must name the stale env pin + refuse the read-only fallback: {err}"
        );

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
    fn healed_endpoint_after_rebind_keeps_env_token_not_control_json_token() {
        // P71-1: the relay-wedge self-heal must resume on the fresh addr the rebind
        // published but KEEP the env token - never adopt control.json's (read-only,
        // under item-3 harden) token. Guards the exact silent read-only downgrade the
        // bridge-heal path used to have.
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

        // The control-tier session's env pin (FULL token) at the now-wedged old port.
        let discovery = Discovery {
            addr: Some("127.0.0.1:1".into()),
            token: Some("FULL-tok".into()),
            file: Some(file.clone()),
            ..Default::default()
        };
        let stale = ControlEndpoint {
            addr: "127.0.0.1:1".into(),
            token: "FULL-tok".into(),
        };

        let healed = healed_endpoint_after_rebind(&discovery, &stale).expect("addr moved -> Some");
        assert_eq!(healed.addr, "127.0.0.1:7777", "resumes on the rebound port");
        assert_eq!(
            healed.token, "FULL-tok",
            "the healed endpoint must keep the env token, not control.json's read-only token"
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
