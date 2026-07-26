//! Client side of the loopback control channel: the bridge from `tools/call` to
//! the running T-Hub app.
//!
//! Discovery reads the stable authoritative handshake at `$T_HUB_CONTROL_FILE`.
//! Native legacy callers fall back to `~/.t-hub/control.json`; legacy WSL callers
//! select exactly one live Windows Production handshake and never use the stale
//! WSL HOME shadow. Address and ambient read authentication come from that file.
//! A durable Captain proves its `$T_HUB_SESSION_TOKEN` to acquire a short-lived
//! identity-bound control lease held only in this process. Legacy explicit address
//! and token overrides remain available for proof harnesses. Each call opens a
//! short-lived TCP
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
const CORTANA_RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(300);

fn response_timeout_for_command(command: &str) -> Duration {
    match command {
        "reconcile_cortana" => CORTANA_RECONCILIATION_TIMEOUT,
        "commission_captain" | "dispatch_crew" | "history_list" | "history_resume"
        | "start_agent" => LONG_ORCHESTRATION_TIMEOUT,
        _ => CONTROL_DEADLINE,
    }
}

/// A single endpoint gets only a short slice of the overall budget so an
/// inherited port that accepts but stays silent cannot consume the recovery
/// window before the current endpoint is tried.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
const WINDOWS_DISCOVERY_DEADLINE: Duration = Duration::from_secs(1);
const WINDOWS_DISCOVERY_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_WINDOWS_PROFILE_ENTRIES: usize = 64;
const MAX_HANDSHAKE_BYTES: u64 = 64 * 1024;
const DISCOVERY_PROOF_COMMAND: &str = "control_discovery_proof";

/// Every control client accepts at most 1 MiB before the NDJSON response newline.
/// This bounds memory, parsing work, and any structured error derived from a peer.
const MAX_RESPONSE_FRAME_BYTES: usize = 1024 * 1024;

/// Side-effecting commands whose retries must dedup via a client `requestId`
/// (mirrors the app-side `is_idempotent_command`).
const IDEMPOTENT_COMMANDS: &[&str] = &[
    "spawn_terminal",
    "create_worktree",
    "history_resume",
    "reconcile_cortana",
    "commission_captain",
    "dispatch_crew",
    "start_agent",
    "agent_followup",
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum EndpointIdentity {
    LegacyEnv,
    Handshake {
        path: PathBuf,
        protocol_version: u32,
        instance_id: Option<String>,
        listener_generation: Option<u64>,
    },
}

/// How T-Hub's control channel was located + authenticated.
#[derive(Clone)]
pub struct ControlEndpoint {
    pub addr: String,
    pub token: String,
    identity: EndpointIdentity,
}

impl std::fmt::Debug for ControlEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlEndpoint")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .field("identity", &self.identity)
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
    identity: EndpointIdentity,
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
    /// Home directory used to derive the native default handshake path. Production
    /// captures `$HOME`/`$USERPROFILE`; tests set it directly.
    pub home: Option<PathBuf>,
    /// Durable session credential used to prove identity during scoped lease
    /// renewal. Captured once from `T_HUB_SESSION_TOKEN`.
    pub session: Option<String>,
    /// Structurally detected WSL host state. Tests inject this directly so legacy
    /// discovery coverage does not depend on the test runner's environment.
    pub structural_wsl: bool,
    /// Windows user-profile root scanned only for legacy WSL discovery.
    /// Production uses `/mnt/c/Users`; tests inject a private fixture root.
    pub windows_users_root: Option<PathBuf>,
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
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            session: std::env::var("T_HUB_SESSION_TOKEN")
                .ok()
                .and_then(non_empty),
            structural_wsl: structurally_running_under_wsl(),
            windows_users_root: Some(PathBuf::from("/mnt/c/Users")),
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
        // 1. The explicit stable file is authoritative for all new sessions.
        if self.file.is_some() {
            return self.resolve_from_file();
        }

        // 2. Explicit addr + token override, retained for proof harnesses and
        //    already-running legacy sessions that predate the stable file.
        if let (Some(addr), Some(token)) = (&self.addr, &self.token) {
            if !addr.is_empty() && !token.is_empty() {
                return Ok(ControlEndpoint {
                    addr: addr.clone(),
                    token: token.clone(),
                    identity: EndpointIdentity::LegacyEnv,
                });
            }
        }

        // 3. Native HOME or legacy WSL Production discovery.
        self.resolve_from_file()
    }

    /// Read the endpoint from the handshake file ONLY, ignoring any
    /// `$T_HUB_CONTROL_ADDR`/`$T_HUB_CONTROL_TOKEN` override.
    ///
    /// This is the normal discovery path for new sessions and the recovery path
    /// after a legacy transport pin fails. The app atomically rewrites the file
    /// whenever the listener address changes.
    pub fn resolve_from_file(&self) -> Result<ControlEndpoint, String> {
        if let Some(path) = &self.file {
            return read_handshake_endpoint(path);
        }
        if self.structural_wsl {
            let users_root = self
                .windows_users_root
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new("/mnt/c/Users"));
            return resolve_unique_live_windows_production(users_root);
        }
        let path = self.native_handshake_path();
        read_handshake_endpoint(&path)
    }

    /// The native handshake file path. WSL legacy discovery deliberately does
    /// not call this helper because its HOME file can be a stale shadow of the
    /// authoritative Windows Production handshake.
    fn native_handshake_path(&self) -> PathBuf {
        let home = self
            .home
            .clone()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
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
        if lease.identity != endpoint.identity {
            return None;
        }
        Some(ControlEndpoint {
            addr: endpoint.addr.clone(),
            token: lease.token,
            identity: endpoint.identity.clone(),
        })
    }

    fn cache_lease(&self, endpoint: &ControlEndpoint, token: String, expires_at: u64) {
        *self
            .lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(CachedLease {
            token,
            expires_at,
            identity: endpoint.identity.clone(),
        });
    }

    /// Whether an explicit env pin (`$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN`)
    /// is in force - i.e. [`resolve`](Self::resolve) returned that pair rather than the
    /// file. This is compatibility state for sessions created before Package 0.
    pub fn has_env_pin(&self) -> bool {
        self.file.is_none()
            && matches!(
                (&self.addr, &self.token),
                (Some(a), Some(t)) if !a.is_empty() && !t.is_empty()
            )
    }

    /// The endpoint to retry after the pinned one failed: the fresh ADDRESS the
    /// running app just published in control.json, but KEEPING the env token when an
    /// env pin is in force.
    pub fn refreshed_endpoint(&self) -> Result<ControlEndpoint, String> {
        let file = self.resolve_from_file()?;
        if self.has_env_pin() {
            return Ok(ControlEndpoint {
                addr: file.addr,
                token: self.token.clone().unwrap_or_default(),
                identity: file.identity,
            });
        }
        Ok(file)
    }
}

fn read_handshake_endpoint(path: &std::path::Path) -> Result<ControlEndpoint, String> {
    let body = read_stable_handshake(path)?;
    parse_handshake_endpoint(path, &body)
}

fn parse_handshake_endpoint(path: &std::path::Path, body: &str) -> Result<ControlEndpoint, String> {
    let hs: Handshake = serde_json::from_str(body)
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
    if hs.token.is_empty() {
        return Err(format!(
            "invalid control handshake at {}: ambient credential is empty",
            path.display()
        ));
    }
    if hs.instance_id.len() > 128
        || (!hs.instance_id.is_empty() && hs.instance_id.chars().any(char::is_whitespace))
    {
        return Err(format!(
            "invalid control handshake at {}: listener instance is malformed",
            path.display()
        ));
    }
    if !hs.instance_id.is_empty() && hs.listener_generation == 0 {
        return Err(format!(
            "invalid control handshake at {}: listener generation is zero",
            path.display()
        ));
    }
    if hs.instance_id.is_empty() && hs.listener_generation != 0 {
        return Err(format!(
            "invalid control handshake at {}: listener generation has no instance",
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
        identity: EndpointIdentity::Handshake {
            path: path.to_path_buf(),
            protocol_version: hs.protocol_version,
            instance_id: (!hs.instance_id.is_empty()).then_some(hs.instance_id),
            listener_generation: (hs.listener_generation != 0).then_some(hs.listener_generation),
        },
    })
}

#[cfg(unix)]
fn open_handshake_no_follow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_handshake_no_follow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "symbolic link handshake is not allowed",
        ));
    }
    std::fs::OpenOptions::new().read(true).open(path)
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

fn read_stable_handshake(path: &std::path::Path) -> Result<String, String> {
    let path_metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "T-Hub control channel not found at {} ({error}). Is the T-Hub app running?",
            path.display()
        )
    })?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(format!(
            "unsafe control handshake at {}: expected a regular non-symlink file",
            path.display()
        ));
    }
    if path_metadata.len() > MAX_HANDSHAKE_BYTES {
        return Err(format!(
            "invalid control handshake at {}: file exceeds {MAX_HANDSHAKE_BYTES} bytes",
            path.display()
        ));
    }

    let file = open_handshake_no_follow(path).map_err(|error| {
        format!(
            "could not safely open control handshake at {} ({error})",
            path.display()
        )
    })?;
    let opened_metadata = file.metadata().map_err(|error| {
        format!(
            "could not inspect control handshake at {} ({error})",
            path.display()
        )
    })?;
    if !opened_metadata.is_file() || !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(format!(
            "control handshake changed while opening {}",
            path.display()
        ));
    }
    read_opened_handshake(path, file)
}

fn read_opened_handshake(
    path: &std::path::Path,
    mut file: std::fs::File,
) -> Result<String, String> {
    let opened_metadata = file.metadata().map_err(|error| {
        format!(
            "could not inspect control handshake at {} ({error})",
            path.display()
        )
    })?;
    if !opened_metadata.is_file() {
        return Err(format!(
            "unsafe control handshake at {}: expected a regular file",
            path.display()
        ));
    }
    if opened_metadata.len() > MAX_HANDSHAKE_BYTES {
        return Err(format!(
            "invalid control handshake at {}: file exceeds {MAX_HANDSHAKE_BYTES} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_HANDSHAKE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "could not read control handshake at {} ({error})",
                path.display()
            )
        })?;
    if bytes.len() as u64 > MAX_HANDSHAKE_BYTES {
        return Err(format!(
            "invalid control handshake at {}: file exceeds {MAX_HANDSHAKE_BYTES} bytes",
            path.display()
        ));
    }
    let after_metadata = file.metadata().map_err(|error| {
        format!(
            "could not re-inspect control handshake at {} ({error})",
            path.display()
        )
    })?;
    if !same_file_identity(&opened_metadata, &after_metadata) {
        return Err(format!(
            "control handshake changed while reading {}",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "malformed control handshake at {}: not UTF-8",
            path.display()
        )
    })
}

fn structurally_running_under_wsl() -> bool {
    if cfg!(windows) {
        return false;
    }
    ["/proc/sys/kernel/osrelease", "/proc/version"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|value| kernel_text_indicates_wsl(&value))
}

fn kernel_text_indicates_wsl(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("microsoft") || lower.contains("wsl")
}

#[cfg(unix)]
fn open_directory_no_follow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(unix)]
fn openat_no_follow(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path component contains NUL",
        )
    })?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    // SAFETY: `parent` owns a live directory descriptor, `name` is NUL-terminated,
    // and a successful descriptor is transferred exactly once into `File`.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor and no other owner exists.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn read_trusted_profile_handshake(
    users_root: &std::fs::File,
    entry: &std::fs::DirEntry,
    display_path: &std::path::Path,
) -> Result<ControlEndpoint, String> {
    let profile = openat_no_follow(users_root, &entry.file_name(), true)
        .map_err(|_| "Windows profile is not a trusted directory".to_string())?;
    let control_dir = openat_no_follow(&profile, std::ffi::OsStr::new(".t-hub"), true)
        .map_err(|_| "Windows profile has no trusted Production control directory".to_string())?;
    let handshake = openat_no_follow(&control_dir, std::ffi::OsStr::new("control.json"), false)
        .map_err(|_| "Windows profile has no trusted Production handshake".to_string())?;
    let body = read_opened_handshake(display_path, handshake)?;
    parse_handshake_endpoint(display_path, &body)
}

#[cfg(not(unix))]
fn read_trusted_profile_handshake(
    _users_root: &std::fs::File,
    entry: &std::fs::DirEntry,
    display_path: &std::path::Path,
) -> Result<ControlEndpoint, String> {
    let profile_metadata = std::fs::symlink_metadata(entry.path())
        .map_err(|_| "Windows profile cannot be inspected".to_string())?;
    if profile_metadata.file_type().is_symlink() || !profile_metadata.is_dir() {
        return Err("Windows profile is not a trusted directory".into());
    }
    let control_dir = entry.path().join(".t-hub");
    let control_metadata = std::fs::symlink_metadata(&control_dir)
        .map_err(|_| "Windows profile has no Production control directory".to_string())?;
    if control_metadata.file_type().is_symlink() || !control_metadata.is_dir() {
        return Err("Windows Production control directory is not trusted".into());
    }
    read_handshake_endpoint(display_path)
}

/// Legacy WSL recovery trusts only immediate, non-symlink profile directories
/// below the fixed Windows Users mount and non-symlink `.t-hub` directories.
/// The nonce proof rejects stale or unrelated listeners without disclosing the
/// durable session secret. It does not defend against a malicious process already
/// running as the same local Windows user, which can read that user's ambient
/// handshake credential; the control channel's local-user boundary treats that
/// principal as trusted. Any filesystem or proof uncertainty fails closed.
fn resolve_unique_live_windows_production(
    users_root: &std::path::Path,
) -> Result<ControlEndpoint, String> {
    let root_metadata = std::fs::symlink_metadata(users_root).map_err(|error| {
        format!(
            "T-Hub Windows Production discovery unavailable under {} ({error})",
            users_root.display()
        )
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!(
            "T-Hub Windows Production discovery root is not a trusted directory: {}",
            users_root.display()
        ));
    }
    #[cfg(unix)]
    let users_root_descriptor = open_directory_no_follow(users_root).map_err(|error| {
        format!(
            "T-Hub Windows Production discovery could not safely open {} ({error})",
            users_root.display()
        )
    })?;
    #[cfg(not(unix))]
    let users_root_descriptor = std::fs::File::open(users_root).map_err(|error| {
        format!(
            "T-Hub Windows Production discovery could not open {} ({error})",
            users_root.display()
        )
    })?;
    let opened_root_metadata = users_root_descriptor.metadata().map_err(|error| {
        format!(
            "T-Hub Windows Production discovery could not inspect {} ({error})",
            users_root.display()
        )
    })?;
    if !opened_root_metadata.is_dir() || !same_file_identity(&root_metadata, &opened_root_metadata)
    {
        return Err(format!(
            "T-Hub Windows Production discovery root changed while opening {}",
            users_root.display()
        ));
    }
    let entries = std::fs::read_dir(users_root).map_err(|error| {
        format!(
            "T-Hub Windows Production discovery unavailable under {} ({error})",
            users_root.display()
        )
    })?;
    let mut candidates = Vec::new();
    let mut entry_count = 0_usize;
    for entry_result in entries {
        entry_count = entry_count.saturating_add(1);
        if entry_count > MAX_WINDOWS_PROFILE_ENTRIES {
            return Err(format!(
                "T-Hub Windows Production discovery found too many profile entries under {}",
                users_root.display()
            ));
        }
        let Ok(entry) = entry_result else {
            continue;
        };
        candidates.push((entry.path().join(".t-hub/control.json"), entry));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));

    let deadline = Instant::now() + WINDOWS_DISCOVERY_DEADLINE;
    let mut live = Vec::new();
    for (path, entry) in candidates {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(
                "T-Hub Windows Production discovery did not complete bounded liveness validation"
                    .to_string(),
            );
        };
        let Ok(endpoint) = read_trusted_profile_handshake(&users_root_descriptor, &entry, &path)
        else {
            continue;
        };
        if let Ok(proven) = prove_windows_production_listener(
            endpoint,
            remaining.min(WINDOWS_DISCOVERY_CONNECT_TIMEOUT),
        ) {
            live.push(proven);
        }
    }
    match live.len() {
        1 => Ok(live.pop().expect("one live endpoint")),
        0 => Err(format!(
            "T-Hub Windows Production discovery found no live validated control handshake under {}",
            users_root.display()
        )),
        count => Err(format!(
            "T-Hub Windows Production discovery is ambiguous: found {count} live validated control handshakes"
        )),
    }
}

fn prove_windows_production_listener(
    mut endpoint: ControlEndpoint,
    timeout: Duration,
) -> Result<ControlEndpoint, String> {
    if timeout.is_zero() {
        return Err("control discovery proof deadline expired".into());
    }
    let nonce = new_request_id();
    // An explicit empty Discovery prevents `call_classified` from inheriting
    // T_HUB_SESSION_TOKEN. Candidate proof uses only the ambient read credential
    // from the candidate handshake.
    let unauthenticated_identity = Discovery::default();
    let proof = call_classified(
        &endpoint,
        DISCOVERY_PROOF_COMMAND,
        &serde_json::json!({ "nonce": nonce }),
        CallBudget {
            deadline: Instant::now() + timeout,
            attempt_timeout: timeout,
        },
        Some(&unauthenticated_identity),
    )
    .map_err(|_| "candidate did not provide a valid T-Hub discovery proof".to_string())?;
    if proof.get("nonce").and_then(Value::as_str) != Some(nonce.as_str()) {
        return Err("candidate discovery proof nonce mismatch".into());
    }
    let instance_id = proof
        .get("instanceId")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_whitespace)
        })
        .ok_or("candidate discovery proof omitted a valid listener instance")?;
    let listener_generation = proof
        .get("listenerGeneration")
        .and_then(Value::as_u64)
        .filter(|value| *value != 0)
        .ok_or("candidate discovery proof omitted a valid listener generation")?;
    let protocol_version = proof
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .filter(|value| *value != 0 && *value <= 2)
        .ok_or("candidate discovery proof omitted a supported protocol version")?
        as u32;
    if proof.get("listenerAddr").and_then(Value::as_str) != Some(endpoint.addr.as_str()) {
        return Err("candidate discovery proof listener address mismatch".into());
    }

    let EndpointIdentity::Handshake {
        path,
        protocol_version: expected_protocol,
        instance_id: expected_instance,
        listener_generation: expected_generation,
    } = &endpoint.identity
    else {
        return Err("candidate discovery proof requires a handshake endpoint".into());
    };
    if *expected_protocol != 0 && *expected_protocol != protocol_version {
        return Err("candidate discovery proof protocol mismatch".into());
    }
    if expected_instance
        .as_deref()
        .is_some_and(|expected| expected != instance_id)
        || expected_generation.is_some_and(|expected| expected != listener_generation)
    {
        return Err("candidate discovery proof listener identity mismatch".into());
    }
    endpoint.identity = EndpointIdentity::Handshake {
        path: path.clone(),
        protocol_version,
        instance_id: Some(instance_id.to_string()),
        listener_generation: Some(listener_generation),
    };
    Ok(endpoint)
}

fn endpoint_replaced(previous: &ControlEndpoint, current: &ControlEndpoint) -> bool {
    previous.addr != current.addr || previous.identity != current.identity
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
        identity: endpoint.identity,
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
    attempted: &ControlEndpoint,
    command: &str,
    args: &Value,
    budget: CallBudget,
) -> Result<Value, ControlCallError> {
    let ambient = discovery
        .resolve_from_file()
        .map_err(ControlCallError::from)?;
    let replacement = endpoint_replaced(attempted, &ambient);
    match call_classified(&ambient, command, args, budget, Some(discovery)) {
        Ok(value)
            if command == "my_capability"
                && value.get("capability").and_then(Value::as_str) == Some("read") =>
        {
            let leased = renew_captain_endpoint(discovery, budget)?;
            call_classified(&leased, command, args, budget, Some(discovery))
                .map_err(|error| call_error_to_control(error, command, 3, replacement))
        }
        Ok(value) => Ok(value),
        Err(CallError::App { message, .. }) if is_auth_rejection(&message) => {
            let leased =
                renew_captain_endpoint(discovery, budget).map_err(ControlCallError::from)?;
            call_classified(&leased, command, args, budget, Some(discovery))
                .map_err(|error| call_error_to_control(error, command, 3, replacement))
        }
        Err(error) => Err(call_error_to_control(error, command, 2, replacement)),
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
                recover_after_auth_rejection(discovery, &endpoint, command, &args, budget)
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
                .filter(|fresh| endpoint_replaced(&endpoint, fresh))
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
                            recover_after_auth_rejection(discovery, &f, command, &args, budget)
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
        .map(|source| source.session_token().to_string())
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
                .is_some_and(|fresh| endpoint_replaced(endpoint, &fresh))
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
        identity: fresh.identity,
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

    struct TestControlHandshake {
        dir: PathBuf,
        file: PathBuf,
    }

    impl TestControlHandshake {
        fn new(addr: &str, token: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("t-hub-mcp-process-handshake-{}", new_request_id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("control.json");
            std::fs::write(
                &file,
                serde_json::to_vec(&serde_json::json!({
                    "addr": addr,
                    "token": token,
                    "pid": std::process::id(),
                }))
                .unwrap(),
            )
            .unwrap();
            Self { dir, file }
        }
    }

    impl Drop for TestControlHandshake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
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

    fn discovery_proof_server(
        instance_id: &'static str,
        listener_generation: u64,
        replies: Vec<Option<&'static str>>,
        max_connections: usize,
    ) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let proof_addr = addr.clone();
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        std::thread::spawn(move || {
            let mut replies = replies.into_iter();
            for _ in 0..max_connections {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Value>(line.trim_end()) else {
                    continue;
                };
                cap.lock().unwrap().push(request.clone());
                if request["command"] == DISCOVERY_PROOF_COMMAND {
                    assert!(request["token"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()));
                    assert_eq!(
                        request["session"], "",
                        "discovery proof must not send the durable session secret"
                    );
                    let response = serde_json::json!({
                        "ok": true,
                        "result": {
                            "nonce": request["args"]["nonce"],
                            "protocolVersion": 2,
                            "instanceId": instance_id,
                            "listenerGeneration": listener_generation,
                            "listenerAddr": proof_addr,
                        }
                    });
                    let _ = writer.write_all(serde_json::to_string(&response).unwrap().as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                    continue;
                }
                if let Some(Some(body)) = replies.next() {
                    let _ = writer.write_all(body.as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                }
            }
        });
        (addr, captured)
    }

    fn write_test_handshake(
        path: &std::path::Path,
        addr: &str,
        instance_id: Option<&str>,
        listener_generation: u64,
    ) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = serde_json::json!({
            "addr": addr,
            "token": "test-ambient",
        });
        if let Some(instance_id) = instance_id {
            body["protocol_version"] = Value::from(2);
            body["instance_id"] = Value::from(instance_id);
            body["listener_generation"] = Value::from(listener_generation);
            body["published_at"] = Value::from(epoch_ms());
        }
        std::fs::write(path, serde_json::to_vec(&body).unwrap()).unwrap();
    }

    #[test]
    fn raw_wire_error_details_survive_mcp_control_adapter() {
        let fixture =
            include_str!("../tests/fixtures/explicit-none-dispatch-preflight-response.json");
        let (addr, _captured) = scripted_server(vec![Some(fixture)]);
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("control-token".into()),
            ..Default::default()
        };

        let error = resolve_and_call_with_deadline(
            &discovery,
            "dispatch_preflight",
            &Value::Null,
            Duration::from_secs(1),
            Duration::from_millis(250),
        )
        .unwrap_err();

        assert_eq!(
            error.message,
            "Git capability is required for dispatch_preflight; initialize Git with initialize_git"
        );
        assert!(!error.retryable);
        assert_eq!(error.kind.as_deref(), Some("git_required"));
        assert_eq!(
            error.details,
            Some(json!({
                "code": "git_required",
                "operation": "dispatch_preflight",
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
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
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
            home: Some(dir.clone()),
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
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{addr}","token":"READ","pid":3}}"#),
        )
        .unwrap();
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("STALE".into()),
            home: Some(dir.clone()),
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
        // Pin both initial discovery and transport-recovery discovery to this
        // process-local server. Otherwise a WSL developer machine with the
        // production app running can redirect the recovery leg to its live
        // Windows control.json, making this process E2E test non-hermetic.
        let handshake = TestControlHandshake::new(addr, token);
        let mut child = Command::new(mcp_binary())
            .env("T_HUB_CONTROL_ADDR", addr)
            .env("T_HUB_CONTROL_TOKEN", token)
            .env("T_HUB_CONTROL_FILE", &handshake.file)
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
            identity: EndpointIdentity::LegacyEnv,
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
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
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
            home: Some(dir.clone()),
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
            home: Some(PathBuf::from("/nonexistent")),
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
            identity: EndpointIdentity::LegacyEnv,
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
            identity: EndpointIdentity::LegacyEnv,
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
            identity: EndpointIdentity::LegacyEnv,
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
            identity: EndpointIdentity::LegacyEnv,
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
            home: Some(PathBuf::from("/nonexistent")),
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
                identity: EndpointIdentity::LegacyEnv,
            });
        });
        let discovery = Discovery {
            addr: Some(silent_addr),
            token: Some("control".into()),
            home: Some(PathBuf::from("/nonexistent")),
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
    fn bridge_rebind_preserves_native_error_details() {
        *wedge_detector() = WedgeDetector::default();
        let (healed_addr, _requests) = scripted_server(vec![Some(
            r#"{"ok":false,"error":"Git capability is required for baseline","errorKind":"git_required","errorDetails":{"code":"git_required","operation":"baseline","capability":"git","action":"initialize_git"},"retryable":false}"#,
        )]);
        TEST_BRIDGE_RESULT.with(|slot| {
            *slot.borrow_mut() = Some(ControlEndpoint {
                addr: healed_addr,
                token: "control".into(),
                identity: EndpointIdentity::LegacyEnv,
            });
        });
        let discovery = Discovery {
            addr: Some("127.0.0.1:9".into()),
            token: Some("control".into()),
            home: Some(PathBuf::from("/nonexistent")),
            ..Default::default()
        };
        let stale = ControlEndpoint {
            addr: "127.0.0.1:9".into(),
            token: "control".into(),
            identity: EndpointIdentity::LegacyEnv,
        };
        let error = maybe_heal_and_retry(
            &discovery,
            "baseline",
            &Value::Null,
            stale,
            ControlCallError::from_message("control_timeout: stale endpoint".into()),
            true,
            CallBudget {
                deadline: Instant::now() + Duration::from_secs(1),
                attempt_timeout: Duration::from_millis(100),
            },
        )
        .unwrap_err();

        assert_eq!(error.kind.as_deref(), Some("git_required"));
        assert_eq!(
            error.details.as_ref().unwrap(),
            &json!({
                "code": "git_required",
                "operation": "baseline",
                "capability": "git",
                "action": "initialize_git"
            })
        );
        assert!(!error.retryable);
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
            home: Some(PathBuf::from("/nonexistent")),
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
                identity: EndpointIdentity::LegacyEnv,
            },
            "session",
            Instant::now() + Duration::from_millis(50),
        ));
        assert!(!send_rebind_via_powershell(
            &ControlEndpoint {
                addr: "127.0.0.1:not-a-port".to_string(),
                token: "t".to_string(),
                identity: EndpointIdentity::LegacyEnv,
            },
            "session",
            Instant::now() + Duration::from_millis(50),
        ));
        assert!(!send_rebind_via_powershell(
            &ControlEndpoint {
                addr: "127.0.0.1:1234".to_string(),
                token: "lease".to_string(),
                identity: EndpointIdentity::LegacyEnv,
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
                identity: EndpointIdentity::LegacyEnv,
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
                identity: EndpointIdentity::LegacyEnv,
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
            home: Some(PathBuf::from("/nonexistent")),
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
                "agent_followup",
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
            LONG_ORCHESTRATION_TIMEOUT
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
            home: Some(PathBuf::from("/nonexistent")),
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
            identity: EndpointIdentity::LegacyEnv,
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
            identity: EndpointIdentity::LegacyEnv,
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
            identity: EndpointIdentity::LegacyEnv,
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
    fn legacy_wsl_home_only_session_reaches_live_windows_production_handshake() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-legacy-wsl-e2e-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let home = dir.join("wsl-home");
        let shadow = home.join(".t-hub/control.json");
        let windows_users = dir.join("windows-users");
        let production = windows_users.join("natha/.t-hub/control.json");
        std::fs::create_dir_all(shadow.parent().unwrap()).unwrap();
        std::fs::create_dir_all(production.parent().unwrap()).unwrap();

        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);
        std::fs::write(
            &shadow,
            format!(r#"{{"addr":"{dead_addr}","token":"stale-shadow"}}"#),
        )
        .unwrap();

        // Discovery and lease renewal each perform a nonce-bound listener proof.
        // The remaining two connections model the scoped lease and command.
        let (live_addr, captured) = discovery_proof_server(
            "production-instance",
            1,
            vec![
                Some(r#"{"ok":true,"result":{"lease":"scoped-lease","expiresAt":9999999999999}}"#),
                Some(r#"{"ok":true,"result":{"capability":"control"}}"#),
            ],
            4,
        );
        std::fs::write(
            &production,
            format!(
                r#"{{"addr":"{live_addr}","token":"ambient-read","protocol_version":2,"instance_id":"production-instance","listener_generation":1,"published_at":{}}}"#,
                epoch_ms()
            ),
        )
        .unwrap();

        let discovery = Discovery {
            home: Some(home),
            session: Some("durable-captain-session".into()),
            structural_wsl: true,
            windows_users_root: Some(windows_users),
            ..Default::default()
        };
        let value = resolve_and_call(&discovery, "my_capability", &Value::Null).unwrap();

        assert_eq!(value["capability"], "control");
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0]["command"], DISCOVERY_PROOF_COMMAND);
        assert_eq!(requests[1]["command"], DISCOVERY_PROOF_COMMAND);
        assert_eq!(requests[2]["command"], "renew_captain_control_lease");
        assert_eq!(requests[3]["command"], "my_capability");
        assert!(requests
            .iter()
            .all(|request| request["token"] != "durable-captain-session"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn structural_wsl_detection_uses_kernel_identity_not_environment() {
        assert!(kernel_text_indicates_wsl(
            "6.6.87.2-microsoft-standard-WSL2"
        ));
        assert!(kernel_text_indicates_wsl(
            "Linux version 5.15.153.1-Microsoft-standard"
        ));
        assert!(!kernel_text_indicates_wsl("6.8.0-63-generic"));
    }

    #[test]
    fn native_linux_discovery_retains_home_handshake_behavior() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-native-home-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let file = dir.join(".t-hub/control.json");
        write_test_handshake(&file, "127.0.0.1:41991", None, 0);
        let discovery = Discovery {
            home: Some(dir.clone()),
            structural_wsl: false,
            ..Default::default()
        };

        let endpoint = discovery.resolve_from_file().unwrap();
        assert_eq!(endpoint.addr, "127.0.0.1:41991");
        assert!(matches!(
            endpoint.identity,
            EndpointIdentity::Handshake {
                path,
                instance_id: None,
                listener_generation: None,
                ..
            } if path == file
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn wsl_discovery_ignores_dev_and_selects_only_production() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-prod-dev-isolation-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let users = dir.join("users");
        let production = users.join("natha/.t-hub/control.json");
        let dev = users.join("natha/.t-hub-dev/control.json");
        let (production_addr, _captured) = discovery_proof_server("production", 1, vec![], 1);
        let dev_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dev_addr = dev_listener.local_addr().unwrap().to_string();
        write_test_handshake(&production, &production_addr, Some("production"), 1);
        write_test_handshake(&dev, &dev_addr, Some("development"), 1);
        let discovery = Discovery {
            home: Some(dir.join("shadow-home")),
            structural_wsl: true,
            windows_users_root: Some(users),
            ..Default::default()
        };

        let endpoint = discovery.resolve_from_file().unwrap();
        assert_eq!(endpoint.addr, production_addr);
        assert_ne!(endpoint.addr, dev_addr);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn explicit_dev_file_remains_higher_priority_than_wsl_production_discovery() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-explicit-dev-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let users = dir.join("users");
        let production = users.join("natha/.t-hub/control.json");
        let dev = users.join("natha/.t-hub-dev/control.json");
        let production_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let production_addr = production_listener.local_addr().unwrap().to_string();
        write_test_handshake(&production, &production_addr, Some("production"), 1);
        write_test_handshake(&dev, "127.0.0.1:41992", Some("development"), 1);
        let discovery = Discovery {
            file: Some(dev.clone()),
            structural_wsl: true,
            windows_users_root: Some(users),
            ..Default::default()
        };

        let endpoint = discovery.resolve_from_file().unwrap();
        assert_eq!(endpoint.addr, "127.0.0.1:41992");
        assert!(matches!(
            endpoint.identity,
            EndpointIdentity::Handshake { path, .. } if path == dev
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn wsl_discovery_selects_one_live_production_candidate_among_stale_files() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-live-production-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let users = dir.join("users");
        let stale = users.join("stale/.t-hub/control.json");
        let live = users.join("current/.t-hub/control.json");
        let dead_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead_listener.local_addr().unwrap().to_string();
        drop(dead_listener);
        let (live_addr, _captured) = discovery_proof_server("live-instance", 1, vec![], 1);
        write_test_handshake(&stale, &dead_addr, Some("stale-instance"), 1);
        write_test_handshake(&live, &live_addr, Some("live-instance"), 1);
        let discovery = Discovery {
            structural_wsl: true,
            windows_users_root: Some(users),
            ..Default::default()
        };

        let endpoint = discovery.resolve_from_file().unwrap();
        assert_eq!(endpoint.addr, live_addr);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn wsl_discovery_fails_closed_when_multiple_production_candidates_are_live() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-ambiguous-production-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let users = dir.join("users");
        let (first_addr, _first) = discovery_proof_server("first-instance", 1, vec![], 1);
        let (second_addr, _second) = discovery_proof_server("second-instance", 1, vec![], 1);
        write_test_handshake(
            &users.join("first/.t-hub/control.json"),
            &first_addr,
            Some("first-instance"),
            1,
        );
        write_test_handshake(
            &users.join("second/.t-hub/control.json"),
            &second_addr,
            Some("second-instance"),
            1,
        );
        let discovery = Discovery {
            structural_wsl: true,
            windows_users_root: Some(users),
            ..Default::default()
        };

        let error = discovery.resolve_from_file().unwrap_err();
        assert!(error.contains("ambiguous"));
        assert!(error.contains("2 live validated"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn wsl_discovery_rejects_a_non_t_hub_listener_on_a_reused_port() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-fake-listener-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let users = dir.join("users");
        let file = users.join("natha/.t-hub/control.json");
        let (addr, captured) = scripted_server(vec![Some(
            r#"{"ok":true,"result":{"service":"not-t-hub"}}"#,
        )]);
        write_test_handshake(&file, &addr, Some("expected-instance"), 1);
        let discovery = Discovery {
            structural_wsl: true,
            windows_users_root: Some(users),
            session: Some("must-not-cross-proof".into()),
            ..Default::default()
        };

        let error = discovery.resolve_from_file().unwrap_err();
        assert!(error.contains("no live validated"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["command"], DISCOVERY_PROOF_COMMAND);
        assert_eq!(requests[0]["session"], "");
        assert_ne!(requests[0]["session"], "must-not-cross-proof");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn wsl_discovery_rejects_replayed_nonce_and_cross_instance_proofs() {
        let replay_dir = std::env::temp_dir().join(format!(
            "th-mcp-replayed-proof-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let replay_users = replay_dir.join("users");
        let replay_file = replay_users.join("natha/.t-hub/control.json");
        let (replay_addr, _captured) = scripted_server(vec![Some(
            r#"{"ok":true,"result":{"nonce":"old-nonce","protocolVersion":2,"instanceId":"expected-instance","listenerGeneration":1}}"#,
        )]);
        write_test_handshake(&replay_file, &replay_addr, Some("expected-instance"), 1);
        let replay = Discovery {
            structural_wsl: true,
            windows_users_root: Some(replay_users),
            ..Default::default()
        };
        assert!(replay
            .resolve_from_file()
            .unwrap_err()
            .contains("no live validated"));

        let cross_dir = std::env::temp_dir().join(format!(
            "th-mcp-cross-instance-proof-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let cross_users = cross_dir.join("users");
        let cross_file = cross_users.join("natha/.t-hub/control.json");
        let (cross_addr, _captured) = discovery_proof_server("different-instance", 1, vec![], 1);
        write_test_handshake(&cross_file, &cross_addr, Some("expected-instance"), 1);
        let cross = Discovery {
            structural_wsl: true,
            windows_users_root: Some(cross_users),
            ..Default::default()
        };
        assert!(cross
            .resolve_from_file()
            .unwrap_err()
            .contains("no live validated"));
        let _ = std::fs::remove_dir_all(replay_dir);
        let _ = std::fs::remove_dir_all(cross_dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn wsl_discovery_adopts_proven_identity_for_a_legacy_handshake_shape() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-legacy-proof-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let users = dir.join("users");
        let file = users.join("natha/.t-hub/control.json");
        let (addr, _captured) = discovery_proof_server("proven-instance", 9, vec![], 1);
        write_test_handshake(&file, &addr, None, 0);
        let discovery = Discovery {
            structural_wsl: true,
            windows_users_root: Some(users),
            ..Default::default()
        };

        let endpoint = discovery.resolve_from_file().unwrap();
        assert!(matches!(
            endpoint.identity,
            EndpointIdentity::Handshake {
                protocol_version: 2,
                instance_id: Some(ref instance),
                listener_generation: Some(9),
                ..
            } if instance == "proven-instance"
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn handshake_reader_rejects_symlink_nonregular_and_oversize_inputs() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "th-mcp-handshake-file-safety-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let regular = dir.join("regular.json");
        write_test_handshake(&regular, "127.0.0.1:41998", None, 0);
        let linked = dir.join("linked.json");
        symlink(&regular, &linked).unwrap();
        assert!(read_handshake_endpoint(&linked)
            .unwrap_err()
            .contains("regular non-symlink"));

        let directory = dir.join("directory.json");
        std::fs::create_dir_all(&directory).unwrap();
        assert!(read_handshake_endpoint(&directory)
            .unwrap_err()
            .contains("regular non-symlink"));

        let oversized = dir.join("oversized.json");
        std::fs::write(&oversized, vec![b'x'; MAX_HANDSHAKE_BYTES as usize + 1]).unwrap();
        assert!(read_handshake_endpoint(&oversized)
            .unwrap_err()
            .contains("exceeds"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn wsl_discovery_rejects_untrusted_profile_links_and_entry_floods() {
        use std::os::unix::fs::symlink;

        let link_dir = std::env::temp_dir().join(format!(
            "th-mcp-profile-link-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let link_users = link_dir.join("users");
        let outside = link_dir.join("outside");
        std::fs::create_dir_all(outside.join(".t-hub")).unwrap();
        std::fs::create_dir_all(&link_users).unwrap();
        symlink(&outside, link_users.join("linked-profile")).unwrap();
        let real_profile = link_users.join("real-profile");
        std::fs::create_dir_all(&real_profile).unwrap();
        symlink(outside.join(".t-hub"), real_profile.join(".t-hub")).unwrap();
        let linked = Discovery {
            structural_wsl: true,
            windows_users_root: Some(link_users),
            ..Default::default()
        };
        assert!(linked
            .resolve_from_file()
            .unwrap_err()
            .contains("no live validated"));

        let flood_dir = std::env::temp_dir().join(format!(
            "th-mcp-profile-flood-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let flood_users = flood_dir.join("users");
        std::fs::create_dir_all(&flood_users).unwrap();
        for index in 0..=MAX_WINDOWS_PROFILE_ENTRIES {
            std::fs::write(flood_users.join(format!("entry-{index}")), b"x").unwrap();
        }
        let flooded = Discovery {
            structural_wsl: true,
            windows_users_root: Some(flood_users),
            ..Default::default()
        };
        assert!(flooded
            .resolve_from_file()
            .unwrap_err()
            .contains("too many profile entries"));
        let _ = std::fs::remove_dir_all(link_dir);
        let _ = std::fs::remove_dir_all(flood_dir);
    }

    #[test]
    fn handshake_validation_rejects_generation_without_instance() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-invalid-instance-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let file = dir.join("control.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:41993","token":"test-ambient","listener_generation":2}"#,
        )
        .unwrap();
        let error = read_handshake_endpoint(&file).unwrap_err();
        assert!(error.contains("generation has no instance"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn endpoint_replacement_ignores_credential_rotation_only() {
        let identity = EndpointIdentity::Handshake {
            path: PathBuf::from("/tmp/control.json"),
            protocol_version: 2,
            instance_id: Some("instance".into()),
            listener_generation: Some(3),
        };
        let previous = ControlEndpoint {
            addr: "127.0.0.1:41994".into(),
            token: "credential-one".into(),
            identity: identity.clone(),
        };
        let rotated = ControlEndpoint {
            addr: previous.addr.clone(),
            token: "credential-two".into(),
            identity,
        };
        assert!(!endpoint_replaced(&previous, &rotated));
    }

    #[test]
    fn token_only_lease_rotation_does_not_interrupt_a_healthy_response() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-token-only-rotation-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let file = dir.join("control.json");
        let addr = delayed_server(
            r#"{"ok":true,"result":{"status":"healthy"}}"#,
            Duration::from_millis(90),
        );
        write_test_handshake(&file, &addr, Some("stable-instance"), 4);
        let discovery = Discovery {
            file: Some(file),
            ..Default::default()
        };
        let ambient = discovery.resolve_from_file().unwrap();
        discovery.cache_lease(
            &ambient,
            "rotated-scoped-credential".into(),
            9_999_999_999_999,
        );

        let value = resolve_and_call_with_deadline(
            &discovery,
            "get_status",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(30),
        )
        .unwrap();
        assert_eq!(value["status"], "healthy");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cached_lease_is_not_reused_across_endpoint_identity_changes() {
        let discovery = Discovery::default();
        let first = ControlEndpoint {
            addr: "127.0.0.1:41999".into(),
            token: "ambient-one".into(),
            identity: EndpointIdentity::Handshake {
                path: PathBuf::from("/tmp/control.json"),
                protocol_version: 2,
                instance_id: Some("instance-one".into()),
                listener_generation: Some(1),
            },
        };
        discovery.cache_lease(&first, "scoped-first".into(), 9_999_999_999_999);
        assert_eq!(
            discovery.cached_lease_endpoint(&first).unwrap().token,
            "scoped-first"
        );

        let mut replacement = first.clone();
        replacement.identity = EndpointIdentity::Handshake {
            path: PathBuf::from("/tmp/control.json"),
            protocol_version: 2,
            instance_id: Some("instance-two".into()),
            listener_generation: Some(1),
        };
        assert!(discovery.cached_lease_endpoint(&replacement).is_none());
    }

    #[test]
    fn endpoint_replacement_tracks_path_listener_instance_and_generation() {
        let previous = ControlEndpoint {
            addr: "127.0.0.1:41995".into(),
            token: "test-ambient".into(),
            identity: EndpointIdentity::Handshake {
                path: PathBuf::from("/tmp/one/control.json"),
                protocol_version: 2,
                instance_id: Some("instance-one".into()),
                listener_generation: Some(1),
            },
        };
        let mut changed = previous.clone();
        changed.addr = "127.0.0.1:41996".into();
        assert!(endpoint_replaced(&previous, &changed));

        changed = previous.clone();
        changed.identity = EndpointIdentity::Handshake {
            path: PathBuf::from("/tmp/two/control.json"),
            protocol_version: 2,
            instance_id: Some("instance-one".into()),
            listener_generation: Some(1),
        };
        assert!(endpoint_replaced(&previous, &changed));

        changed = previous.clone();
        changed.identity = EndpointIdentity::Handshake {
            path: PathBuf::from("/tmp/one/control.json"),
            protocol_version: 2,
            instance_id: Some("instance-two".into()),
            listener_generation: Some(1),
        };
        assert!(endpoint_replaced(&previous, &changed));

        changed = previous.clone();
        changed.identity = EndpointIdentity::Handshake {
            path: PathBuf::from("/tmp/one/control.json"),
            protocol_version: 2,
            instance_id: Some("instance-one".into()),
            listener_generation: Some(2),
        };
        assert!(endpoint_replaced(&previous, &changed));
    }

    #[test]
    fn explicit_control_file_has_priority_over_legacy_addr_token_override() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-explicit-priority-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let file = dir.join("control.json");
        write_test_handshake(&file, "127.0.0.1:41997", Some("explicit-instance"), 1);
        let discovery = Discovery {
            addr: Some("127.0.0.1:1".into()),
            token: Some("legacy-credential".into()),
            file: Some(file),
            ..Default::default()
        };
        let ep = discovery.resolve().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:41997");
        assert_eq!(ep.token, "test-ambient");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_endpoint_missing_file_is_descriptive_error() {
        let discovery = Discovery {
            home: Some(PathBuf::from("/nonexistent")),
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
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();

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
            home: Some(dir.clone()),
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
    fn port_only_rebind_rejects_a_cached_lease_from_another_endpoint_identity() {
        let dir = std::env::temp_dir().join(format!(
            "th-mcp-lease-rebind-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
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
            home: Some(dir.clone()),
            session: Some("captain-session".into()),
            ..Default::default()
        };
        discovery.cache_lease(
            &ControlEndpoint {
                addr: "127.0.0.1:1".into(),
                token: "ignored".into(),
                identity: EndpointIdentity::LegacyEnv,
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
        assert_eq!(
            requests[0]["token"], "OLD-global",
            "a lease cached against the legacy endpoint must not cross to the file endpoint"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_and_call_reauthenticates_the_same_identity_after_real_token_rotation() {
        // A real global credential rotation must recover through the durable
        // session identity without returning or adopting the new global token.
        let dir = std::env::temp_dir().join(format!("th-mcp-rot2-{}", std::process::id()));
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();

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
            home: Some(dir.clone()),
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
        let file = dir.join(".t-hub/control.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:5555","token":"READ-tok","pid":1}"#,
        )
        .unwrap();

        let pinned = Discovery {
            addr: Some("127.0.0.1:1".into()),
            token: Some("FULL-tok".into()),
            home: Some(dir.clone()),
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
            identity: EndpointIdentity::LegacyEnv,
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
            // A HOME handshake that does not exist: if this path retried on disk it would
            // change the error; asserting "boom" proves it did not.
            home: Some(PathBuf::from("/nonexistent")),
            ..Default::default()
        };
        let err = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap_err();
        assert_eq!(err, "boom");
    }
}
