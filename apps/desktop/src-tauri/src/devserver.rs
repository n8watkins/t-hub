//! Managed typed-target runner for the per-project Run and Preview surface.
//!
//! The backend discovers package scripts and package-less static sites, validates
//! a selected target again at start time, constructs executable arguments or a
//! confined loopback server itself, and owns authoritative generation-safe
//! lifecycle snapshots. Frontend-provided shell text is never executed.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, LazyLock, Weak,
};
use std::thread::JoinHandle;

use cap_fs_ext::{
    ambient_authority, DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt,
};
use cap_std::fs::{Dir as CapDir, File as CapFile, OpenOptions as CapOpenOptions};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// The per-terminal event channel carrying dev-server output lines. The frontend
/// subscribes to `devserver://<terminal_id>` (see `src/ipc/devserver.ts`). Built
/// here so the channel name lives in exactly one place.
pub fn channel(terminal_id: &str) -> String {
    format!("devserver://{terminal_id}")
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Pnpm,
    Npm,
    Yarn,
    Bun,
}

impl PackageManager {
    fn executable(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunTarget {
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<PackageManager>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_root: Option<String>,
    pub command_display: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunTargetRef {
    pub kind: String,
    pub script: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunTargetDiscovery {
    pub state: String,
    pub targets: Vec<RunTarget>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DevServerSnapshot {
    pub terminal_id: String,
    pub run_id: Option<String>,
    pub revision: u64,
    pub state: String,
    pub target: Option<RunTarget>,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub preview_url: Option<String>,
    pub observed_at: u64,
}

/// One generation-tagged event from a managed runner.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevServerEvent {
    pub id: String,
    pub run_id: String,
    pub revision: u64,
    pub kind: String,
    pub line: String,
}

impl DevServerEvent {
    fn new(id: &str, run_id: &str, revision: u64, kind: &str, line: String) -> Self {
        Self {
            id: id.to_string(),
            run_id: run_id.to_string(),
            revision,
            kind: kind.to_string(),
            line,
        }
    }
}

/// A running managed dev server: the child process handle (so we can kill it) and
/// the reader thread draining its combined output (joined on stop so it can't
/// linger). Held in the global registry keyed by terminal id.
struct DevProcess {
    run_id: String,
    child: Child,
    stdin: Option<ChildStdin>,
    readers: Vec<JoinHandle<()>>,
    _job: Option<crate::engine_supervisor::platform::KillOnCloseJob>,
}

impl DevProcess {
    /// Close the process-tree lifeline, wait for its bounded TERM/KILL cleanup,
    /// and then reap the relay. Reader joins are also bounded so a broken child
    /// cannot wedge Stop by retaining one inherited pipe forever.
    fn stop(mut self) {
        self.stdin.take();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        let reader_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        for handle in self.readers.drain(..) {
            while !handle.is_finished() && std::time::Instant::now() < reader_deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

struct StaticServer {
    shutdown: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
    active_responses: Arc<AtomicUsize>,
}

impl StaticServer {
    fn stop(mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
            while self.active_responses.load(Ordering::SeqCst) > 0
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[derive(Default)]
struct DevRegistry {
    processes: HashMap<String, DevProcess>,
    static_servers: HashMap<String, StaticServer>,
    generations: HashMap<String, String>,
    operations: HashMap<String, u64>,
    snapshots: HashMap<String, DevServerSnapshot>,
    revision: u64,
    operation_sequence: u64,
}

static REGISTRY: LazyLock<Mutex<DevRegistry>> =
    LazyLock::new(|| Mutex::new(DevRegistry::default()));
static OPERATION_GATES: LazyLock<Mutex<HashMap<String, Weak<async_lock::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn operation_gate(terminal_id: &str) -> Arc<async_lock::Mutex<()>> {
    let mut gates = OPERATION_GATES.lock();
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(terminal_id).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(async_lock::Mutex::new(()));
    gates.insert(terminal_id.to_string(), Arc::downgrade(&gate));
    gate
}

fn reserve_operation(registry: &mut DevRegistry, terminal_id: &str) -> u64 {
    registry.operation_sequence = registry.operation_sequence.saturating_add(1);
    let token = registry.operation_sequence;
    registry.operations.insert(terminal_id.to_string(), token);
    token
}

fn owns_operation(registry: &DevRegistry, terminal_id: &str, token: u64) -> bool {
    registry.operations.get(terminal_id).copied() == Some(token)
}

#[derive(Debug, PartialEq, Eq)]
enum PollOutcome {
    Running,
    Exited(Option<i32>),
    Replaced,
}

fn poll_run(registry: &mut DevRegistry, terminal_id: &str, run_id: &str) -> PollOutcome {
    if registry.generations.get(terminal_id).map(String::as_str) != Some(run_id) {
        return PollOutcome::Replaced;
    }
    match registry.processes.get_mut(terminal_id) {
        Some(process) if process.run_id == run_id => match process.child.try_wait() {
            Ok(Some(status)) => PollOutcome::Exited(status.code()),
            Ok(None) => PollOutcome::Running,
            Err(_) => PollOutcome::Exited(None),
        },
        _ => PollOutcome::Replaced,
    }
}

fn owns_generation(registry: &DevRegistry, terminal_id: &str, run_id: &str) -> bool {
    registry.generations.get(terminal_id).map(String::as_str) == Some(run_id)
}

fn finish_validation_error(registry: &mut DevRegistry, terminal_id: &str, operation: u64) -> bool {
    if !owns_operation(registry, terminal_id, operation) {
        return false;
    }
    registry.operations.remove(terminal_id);
    true
}

fn publish_start_failure(
    registry: &mut DevRegistry,
    terminal_id: &str,
    operation: u64,
    run_id: &str,
    target: Option<RunTarget>,
    reason: &str,
) -> bool {
    if !owns_operation(registry, terminal_id, operation)
        || !owns_generation(registry, terminal_id, run_id)
    {
        return false;
    }
    registry.operations.remove(terminal_id);
    registry.generations.remove(terminal_id);
    let revision = next_revision(registry);
    registry.snapshots.insert(
        terminal_id.to_string(),
        DevServerSnapshot {
            terminal_id: terminal_id.to_string(),
            run_id: Some(run_id.to_string()),
            revision,
            state: "failed".to_string(),
            target,
            exit_code: None,
            reason: Some(reason.to_string()),
            preview_url: None,
            observed_at: observed_at(),
        },
    );
    true
}

#[cfg(all(test, not(windows)))]
fn take_process_for_stop(
    registry: &mut DevRegistry,
    terminal_id: &str,
    run_id: Option<&str>,
) -> Result<Option<DevProcess>, String> {
    if let (Some(expected), Some(active)) = (run_id, registry.processes.get(terminal_id)) {
        if active.run_id != expected {
            return Err("the requested run is no longer active".to_string());
        }
    }
    Ok(registry.processes.remove(terminal_id))
}

fn observed_at() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_revision(registry: &mut DevRegistry) -> u64 {
    registry.revision = registry.revision.saturating_add(1);
    registry.revision
}

fn idle_snapshot(terminal_id: &str, revision: u64) -> DevServerSnapshot {
    DevServerSnapshot {
        terminal_id: terminal_id.to_string(),
        run_id: None,
        revision,
        state: "idle".to_string(),
        target: None,
        exit_code: None,
        reason: None,
        preview_url: None,
        observed_at: observed_at(),
    }
}

fn finish_stop_snapshot(
    registry: &mut DevRegistry,
    terminal_id: &str,
    operation: u64,
) -> Result<DevServerSnapshot, String> {
    if !owns_operation(registry, terminal_id, operation) {
        return registry
            .snapshots
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| "the replacement run has no lifecycle snapshot".to_string());
    }
    registry.operations.remove(terminal_id);
    let revision = next_revision(registry);
    let snapshot = idle_snapshot(terminal_id, revision);
    registry
        .snapshots
        .insert(terminal_id.to_string(), snapshot.clone());
    Ok(snapshot)
}

fn parse_package_manager(value: &str) -> Option<PackageManager> {
    match value.split('@').next().unwrap_or(value) {
        "pnpm" => Some(PackageManager::Pnpm),
        "npm" => Some(PackageManager::Npm),
        "yarn" => Some(PackageManager::Yarn),
        "bun" => Some(PackageManager::Bun),
        _ => None,
    }
}

fn lockfile_manager(names: &[String]) -> PackageManager {
    let managers = [
        (PackageManager::Pnpm, ["pnpm-lock.yaml", ""]),
        (
            PackageManager::Npm,
            ["package-lock.json", "npm-shrinkwrap.json"],
        ),
        (PackageManager::Yarn, ["yarn.lock", ""]),
        (PackageManager::Bun, ["bun.lock", "bun.lockb"]),
    ];
    let matches: Vec<_> = managers
        .into_iter()
        .filter_map(|(manager, files)| {
            files
                .iter()
                .filter(|file| !file.is_empty())
                .any(|file| names.iter().any(|name| name == file))
                .then_some(manager)
        })
        .collect();
    if matches.len() == 1 {
        matches[0]
    } else {
        PackageManager::Npm
    }
}

fn parse_targets(text: &str, package_manager: PackageManager) -> Result<Vec<RunTarget>, String> {
    let package: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("invalid package.json: {error}"))?;
    let root = package
        .as_object()
        .ok_or_else(|| "package.json root must be an object".to_string())?;
    let Some(scripts) = root.get("scripts") else {
        return Ok(Vec::new());
    };
    let scripts = scripts
        .as_object()
        .ok_or_else(|| "package.json scripts must be an object".to_string())?;
    let priority = |script: &str| match script {
        "dev" => 0,
        "start" => 1,
        "serve" => 2,
        "preview" => 3,
        _ => 4,
    };
    let mut names: Vec<String> = scripts
        .iter()
        .filter_map(|(name, command)| command.is_string().then_some(name.clone()))
        .collect();
    names.sort_by(|left, right| {
        priority(left)
            .cmp(&priority(right))
            .then_with(|| left.cmp(right))
    });
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(index, script)| RunTarget {
            kind: "packageScript".to_string(),
            id: format!("package-script:{script}"),
            label: script.clone(),
            command_display: format!("{} run {script}", package_manager.executable()),
            package_manager: Some(package_manager),
            entrypoint: None,
            relative_root: None,
            recommended: index == 0,
            script: Some(script),
        })
        .collect())
}

fn static_target(cwd: &str) -> Result<Option<RunTarget>, String> {
    let entrypoint = crate::files::to_host_path(cwd).join("index.html");
    let metadata = match fs::symlink_metadata(&entrypoint) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to inspect index.html: {error}")),
    };
    if metadata.file_type().is_symlink() || has_reparse_point(&metadata) || !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some(RunTarget {
        kind: "staticSite".to_string(),
        id: "static-site:root".to_string(),
        script: None,
        label: "Static site".to_string(),
        package_manager: None,
        entrypoint: Some("index.html".to_string()),
        relative_root: Some(".".to_string()),
        command_display: "Serve ./index.html".to_string(),
        recommended: true,
    }))
}

fn select_target(targets: Vec<RunTarget>, target: &RunTargetRef) -> Option<RunTarget> {
    targets
        .into_iter()
        .find(|candidate| match target.kind.as_str() {
            "packageScript" => {
                candidate.kind == "packageScript"
                    && candidate.script.as_deref() == target.script.as_deref()
                    && target
                        .script
                        .as_deref()
                        .is_some_and(|script| !script.trim().is_empty())
            }
            "staticSite" => {
                candidate.kind == "staticSite"
                    && candidate.id == "static-site:root"
                    && target.id.as_deref() == Some("static-site:root")
            }
            _ => false,
        })
}

#[tauri::command]
pub async fn discover_run_targets(cwd: String) -> Result<RunTargetDiscovery, String> {
    if cwd.trim().is_empty() {
        return Ok(RunTargetDiscovery {
            state: "notFound".to_string(),
            targets: Vec::new(),
            message: Some("This tile has no project directory.".to_string()),
        });
    }
    let entries = match crate::files::list_dir(cwd.clone(), Some(true)).await {
        Ok(entries) => entries,
        Err(error) => {
            return Ok(RunTargetDiscovery {
                state: "unreadable".to_string(),
                targets: Vec::new(),
                message: Some(error),
            });
        }
    };
    let static_target = match static_target(&cwd) {
        Ok(target) => target,
        Err(error) => {
            return Ok(RunTargetDiscovery {
                state: "unreadable".to_string(),
                targets: Vec::new(),
                message: Some(error),
            });
        }
    };
    if !entries.iter().any(|entry| entry.name == "package.json") {
        if let Some(target) = static_target {
            return Ok(RunTargetDiscovery {
                state: "ready".to_string(),
                targets: vec![target],
                message: None,
            });
        }
        return Ok(RunTargetDiscovery {
            state: "notFound".to_string(),
            targets: Vec::new(),
            message: Some("No run target was found.".to_string()),
        });
    }
    let package_path = if cwd.starts_with('/') {
        format!("{}/package.json", cwd.trim_end_matches('/'))
    } else {
        std::path::PathBuf::from(&cwd)
            .join("package.json")
            .to_string_lossy()
            .into_owned()
    };
    let contents = match crate::files::read_text_file(package_path).await {
        Ok(contents) => contents,
        Err(error) => {
            return Ok(RunTargetDiscovery {
                state: "unreadable".to_string(),
                targets: Vec::new(),
                message: Some(error),
            });
        }
    };
    if contents.truncated {
        return Ok(RunTargetDiscovery {
            state: "invalid".to_string(),
            targets: Vec::new(),
            message: Some("package.json is too large to inspect safely.".to_string()),
        });
    }
    let package: serde_json::Value = match serde_json::from_str(&contents.text) {
        Ok(package) => package,
        Err(error) => {
            return Ok(RunTargetDiscovery {
                state: "invalid".to_string(),
                targets: Vec::new(),
                message: Some(format!("invalid package.json: {error}")),
            });
        }
    };
    let declared_manager = package
        .get("packageManager")
        .and_then(serde_json::Value::as_str);
    let package_manager = match declared_manager {
        Some(value) => match parse_package_manager(value) {
            Some(manager) => manager,
            None => {
                return Ok(RunTargetDiscovery {
                    state: "invalid".to_string(),
                    targets: Vec::new(),
                    message: Some(format!("unsupported packageManager: {value}")),
                });
            }
        },
        None => lockfile_manager(
            &entries
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
        ),
    };
    match parse_targets(&contents.text, package_manager) {
        Ok(mut targets) => {
            if let Some(mut target) = static_target {
                target.recommended = targets.is_empty();
                targets.push(target);
            }
            Ok(RunTargetDiscovery {
                state: "ready".to_string(),
                message: targets
                    .is_empty()
                    .then(|| "No run targets are defined.".to_string()),
                targets,
            })
        }
        Err(error) => Ok(RunTargetDiscovery {
            state: "invalid".to_string(),
            targets: Vec::new(),
            message: Some(error),
        }),
    }
}

/// Recover a POSIX/WSL path from a `\\wsl.localhost\<distro>\...` (or legacy
/// `\\wsl$\<distro>\...`) UNC path, or pass through a path that is already a bare
/// POSIX path. Returns `None` for a genuine Windows drive path (`C:\...`).
///
/// This replicates the minimal logic of `files.rs::unc_to_posix` (which is
/// private to that module) so the dev server can run natively inside WSL at the
/// project's cwd rather than over the slow UNC bridge.
#[cfg(windows)]
fn unc_to_posix(path: &str) -> Option<String> {
    // Already a bare POSIX path: pass through.
    if path.starts_with('/') {
        return Some(path.to_string());
    }
    // Peel a verbatim extended-length prefix first (`\\?\UNC\...` / `\\?\C:\...`).
    let s: std::borrow::Cow<str> = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
        std::borrow::Cow::Owned(format!("\\\\{rest}"))
    } else if let Some(rest) = path.strip_prefix("\\\\?\\") {
        std::borrow::Cow::Owned(rest.to_string())
    } else {
        std::borrow::Cow::Borrowed(path)
    };
    for prefix in ["\\\\wsl.localhost\\", "\\\\wsl$\\"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            // `rest` is `<distro>\home\natkins\...`; drop the distro segment.
            let tail = match rest.split_once('\\') {
                Some((_distro, tail)) => tail,
                None => "",
            };
            let posix = format!("/{}", tail.replace('\\', "/"));
            return Some(posix);
        }
    }
    None
}

/// Supervise the complete package-manager process group behind a stdin
/// lifeline. The package manager and validated script remain argv data after
/// the fixed shell program. EOF from T-Hub triggers TERM, a bounded grace
/// period, and KILL for the owned group. Natural child exit preserves its code.
const PROCESS_TREE_SCRIPT: &str = r#"set -u
MARKER="/tmp/t-hub-devserver-$1.pid"
shift
export HOST=0.0.0.0 HOSTNAME=0.0.0.0 NUXT_HOST=0.0.0.0 ASTRO_HOST=0.0.0.0 TAURI_DEV_HOST=0.0.0.0
exec 3<&0
setsid "$@" 3<&- </dev/null &
SRV=$!
echo "$SRV" > "$MARKER" 2>/dev/null || true
cleanup() {
  kill -TERM -- -"$SRV" 2>/dev/null || true
  i=0
  while kill -0 "$SRV" 2>/dev/null && [ "$i" -lt 20 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  kill -KILL -- -"$SRV" 2>/dev/null || true
  wait "$SRV" 2>/dev/null || true
  rm -f "$MARKER" 2>/dev/null || true
}
trap 'cleanup; exit 0' TERM INT HUP
(cat <&3 >/dev/null; kill -TERM "$$" 2>/dev/null || true) &
LIFE=$!
wait "$SRV"
CODE=$?
kill "$LIFE" 2>/dev/null || true
wait "$LIFE" 2>/dev/null || true
cleanup
exit "$CODE"
"#;

/// Wrap the user's dev command so the server binds to ALL interfaces
/// (`0.0.0.0`) rather than only the WSL loopback (`127.0.0.1`).
///
/// WHY (the core WSL2 preview bug): the dev server runs INSIDE WSL, but the
/// preview (a Windows WebView2 iframe) is a Windows process. With
/// `networkingMode=mirrored` — and, differently, with NAT's localhost relay —
/// a server bound to `127.0.0.1` listens only on WSL's loopback, which is a
/// SEPARATE loopback from Windows'. The Windows-side iframe then can't reach
/// `localhost:<port>` ("refuses to connect even on a host that exists"). A
/// server bound to `0.0.0.0` also listens on the shared/mirrored interface, so
/// the Windows iframe (pointed at the WSL interface IP, see [`preview_host`])
/// can reach it.
///
/// We do this WITHOUT mangling the command string: we `export` the bind-host env
/// vars the common frameworks read BEFORE running the user's command, so e.g.
/// `pnpm dev` runs verbatim afterwards. `HOST` is honoured by CRA, Next, Nuxt,
/// Remix, Astro, Gatsby and many custom servers; the framework-specific aliases
/// cover the rest. Tauri's standard Vite configuration reads `TAURI_DEV_HOST`,
/// so it binds all WSL interfaces rather than only WSL loopback. This is
/// important in mirrored mode, where the first address from `hostname -I` is
/// also owned by Windows and is not a valid Windows-to-WSL destination for the
/// listener. Unknown tools remain unchanged and receive no guessed CLI
/// arguments.
/// Build a package-manager invocation from backend-owned executable and argv.
/// The validated script name is always one argument and is never shell source.
fn build_command(
    cwd: &str,
    run_id: &str,
    package_manager: PackageManager,
    script: &str,
) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let posix_cwd = unc_to_posix(cwd).unwrap_or_else(|| cwd.to_string());
        let mut c = Command::new("wsl.exe");
        c.arg("-d").arg(crate::files::host_distro());
        if !posix_cwd.is_empty() {
            c.arg("--cd").arg(&posix_cwd);
        }
        c.arg("-e")
            .arg("bash")
            .arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg("t-hub-runner")
            .arg(run_id)
            .arg(package_manager.executable())
            .arg("run")
            .arg(script);
        c.creation_flags(0x0800_0000);
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("bash");
        c.arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg("t-hub-runner")
            .arg(run_id)
            .arg(package_manager.executable())
            .arg("run")
            .arg(script);
        if !cwd.is_empty() {
            c.current_dir(cwd);
        }
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        c
    }
}

/// Drain a piped reader line-by-line, emitting each line on the dev-server
/// channel for `id`. Used for both stdout and stderr (each on its own thread).
/// Lines are emitted as soon as they complete a newline; partial trailing data at
/// EOF is flushed too. Reads bytes (not `String`) and lossily decodes so a stray
/// non-UTF-8 byte can't kill the stream.
fn pump<R: std::io::Read>(app: &AppHandle, id: &str, run_id: &str, reader: R) {
    let ch = channel(id);
    let mut buf = BufReader::new(reader);
    let mut line = Vec::<u8>::new();
    loop {
        line.clear();
        // `read_until('\n')` returns 0 only at EOF; otherwise it includes the
        // newline (if any) in `line`.
        match buf.read_until(b'\n', &mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                // Strip the trailing CR/LF so the frontend gets clean lines.
                while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                    line.pop();
                }
                let text = String::from_utf8_lossy(&line).into_owned();
                let revision = {
                    let mut registry = REGISTRY.lock();
                    let is_current = registry
                        .processes
                        .get(id)
                        .is_some_and(|process| process.run_id == run_id);
                    if !is_current {
                        return;
                    }
                    let revision = next_revision(&mut registry);
                    if let Some(snapshot) = registry.snapshots.get_mut(id) {
                        if snapshot.run_id.as_deref() == Some(run_id) {
                            snapshot.revision = revision;
                            snapshot.observed_at = observed_at();
                        }
                    }
                    revision
                };
                let _ = app.emit(&ch, DevServerEvent::new(id, run_id, revision, "line", text));
            }
            Err(_) => break, // read error: treat as end-of-stream
        }
    }
}

const MAX_STATIC_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_STATIC_REQUEST_BYTES: usize = 16 * 1024;
const MAX_STATIC_CONCURRENT_RESPONSES: usize = 8;
const MAX_GLOBAL_STATIC_RESPONSES: usize = 32;
const STATIC_SOCKET_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
static ACTIVE_GLOBAL_STATIC_RESPONSES: AtomicUsize = AtomicUsize::new(0);

struct StaticResponseGuard {
    local: Arc<AtomicUsize>,
}

impl StaticResponseGuard {
    fn reserve_counter(counter: &AtomicUsize, limit: usize) -> bool {
        let mut current = counter.load(Ordering::SeqCst);
        loop {
            if current >= limit {
                return false;
            }
            match counter.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn reserve(active: &Arc<AtomicUsize>) -> Option<Self> {
        if !Self::reserve_counter(&ACTIVE_GLOBAL_STATIC_RESPONSES, MAX_GLOBAL_STATIC_RESPONSES) {
            return None;
        }
        if !Self::reserve_counter(active, MAX_STATIC_CONCURRENT_RESPONSES) {
            ACTIVE_GLOBAL_STATIC_RESPONSES.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self {
            local: Arc::clone(active),
        })
    }
}

impl Drop for StaticResponseGuard {
    fn drop(&mut self) {
        self.local.fetch_sub(1, Ordering::SeqCst);
        ACTIVE_GLOBAL_STATIC_RESPONSES.fetch_sub(1, Ordering::SeqCst);
    }
}

fn has_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn decode_static_path(raw: &str) -> Result<String, ()> {
    let path = raw.split('?').next().unwrap_or(raw);
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(());
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map_err(|_| ())?
        .into_owned();
    if decoded.contains(['\0', '\\', '%']) {
        return Err(());
    }
    Ok(decoded)
}

fn cap_metadata_has_reparse_point(metadata: &cap_std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_fs_ext::OsMetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

struct OpenedStaticFile {
    file: CapFile,
    mime_path: PathBuf,
}

fn nofollow_options(maybe_dir: bool) -> CapOpenOptions {
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(maybe_dir);
    options
}

fn open_static_root(cwd: &str) -> Result<CapDir, String> {
    let path = crate::files::to_host_path(cwd);
    let file = CapFile::open_ambient_with(&path, &nofollow_options(true), ambient_authority())
        .map_err(|error| format!("failed to open static site root: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect static site root: {error}"))?;
    if !metadata.is_dir() || cap_metadata_has_reparse_point(&metadata) {
        return Err("the static site root is no longer a regular directory".to_string());
    }
    Ok(CapDir::from_std_file(file.into_std()))
}

fn open_regular_static_file(
    directory: &CapDir,
    name: &Path,
    mime_path: PathBuf,
) -> Result<OpenedStaticFile, ()> {
    let file = directory
        .open_with(name, &nofollow_options(false))
        .map_err(|_| ())?;
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file()
        || metadata.len() > MAX_STATIC_FILE_BYTES
        || cap_metadata_has_reparse_point(&metadata)
    {
        return Err(());
    }
    Ok(OpenedStaticFile { file, mime_path })
}

fn open_static_file(root: &CapDir, raw: &str) -> Result<OpenedStaticFile, ()> {
    let decoded = decode_static_path(raw)?;
    let mut names = Vec::new();
    for component in Path::new(decoded.trim_start_matches('/')).components() {
        let name = match component {
            Component::Normal(name) => name,
            Component::CurDir if names.is_empty() => continue,
            _ => return Err(()),
        };
        let text = name.to_str().ok_or(())?;
        if text.starts_with('.') || text.contains(':') {
            return Err(());
        }
        names.push(name.to_os_string());
    }

    if names.is_empty() {
        return open_regular_static_file(root, Path::new("index.html"), "index.html".into());
    }

    let mut directory = root.try_clone().map_err(|_| ())?;
    let mut mime_path = PathBuf::new();
    for name in &names[..names.len() - 1] {
        directory = directory.open_dir_nofollow(name).map_err(|_| ())?;
        let metadata = directory.dir_metadata().map_err(|_| ())?;
        if cap_metadata_has_reparse_point(&metadata) {
            return Err(());
        }
        mime_path.push(name);
    }

    let final_name = Path::new(names.last().ok_or(())?);
    mime_path.push(final_name);
    let opened = directory
        .open_with(final_name, &nofollow_options(true))
        .map_err(|_| ())?;
    let metadata = opened.metadata().map_err(|_| ())?;
    if cap_metadata_has_reparse_point(&metadata) {
        return Err(());
    }
    if metadata.is_dir() {
        let nested = CapDir::from_std_file(opened.into_std());
        mime_path.push("index.html");
        open_regular_static_file(&nested, Path::new("index.html"), mime_path)
    } else if metadata.is_file() && metadata.len() <= MAX_STATIC_FILE_BYTES {
        Ok(OpenedStaticFile {
            file: opened,
            mime_path,
        })
    } else {
        Err(())
    }
}

fn read_static_body(file: CapFile) -> Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    file.take(MAX_STATIC_FILE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| ())?;
    if body.len() as u64 > MAX_STATIC_FILE_BYTES {
        return Err(());
    }
    Ok(body)
}

fn write_static_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    send_body: bool,
    extra_headers: &str,
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nConnection: close\r\n{extra_headers}\r\n",
        body.len()
    );
    let deadline = std::time::Instant::now() + STATIC_SOCKET_TIMEOUT;
    write_before_deadline(stream, header.as_bytes(), deadline)?;
    if send_body {
        write_before_deadline(stream, body, deadline)?;
    }
    Ok(())
}

fn write_before_deadline(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "static response deadline elapsed",
            ));
        }
        match stream.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "static response socket closed",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn request_has_expected_host(request: &[u8], expected_host: &str) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let Ok(headers) = std::str::from_utf8(&request[..header_end]) else {
        return false;
    };
    let mut host = None;
    for line in headers.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        if name.eq_ignore_ascii_case("host") {
            if host.is_some() {
                return false;
            }
            host = Some(value.trim());
        }
    }
    host == Some(expected_host)
}

fn respond_static(
    stream: &mut TcpStream,
    root: &CapDir,
    expected_host: &str,
    _response_guard: StaticResponseGuard,
) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let request_deadline = std::time::Instant::now() + STATIC_SOCKET_TIMEOUT;
    let mut request = Vec::new();
    let mut chunk = [0_u8; 2048];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        if std::time::Instant::now() >= request_deadline {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                request.extend_from_slice(&chunk[..read]);
                if request.len() > MAX_STATIC_REQUEST_BYTES {
                    let _ = write_static_response(
                        stream,
                        "431 Request Header Fields Too Large",
                        "text/plain; charset=utf-8",
                        b"Request headers too large",
                        true,
                        "",
                    );
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
    let Some(line_end) = request.windows(2).position(|window| window == b"\r\n") else {
        return;
    };
    let Ok(request_line) = std::str::from_utf8(&request[..line_end]) else {
        return;
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        let _ = write_static_response(
            stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"Bad request",
            true,
            "",
        );
        return;
    };
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return;
    }
    if !request_has_expected_host(&request, expected_host) {
        let _ = write_static_response(
            stream,
            "421 Misdirected Request",
            "text/plain; charset=utf-8",
            b"Misdirected request",
            method != "HEAD",
            "",
        );
        return;
    }
    if !matches!(method, "GET" | "HEAD") {
        let _ = write_static_response(
            stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"Method not allowed",
            method != "HEAD",
            "Allow: GET, HEAD\r\n",
        );
        return;
    }
    let opened = match open_static_file(root, path) {
        Ok(opened) => opened,
        Err(()) => {
            let _ = write_static_response(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"Not found",
                method == "GET",
                "",
            );
            return;
        }
    };
    let mime = mime_guess::from_path(&opened.mime_path).first_or_octet_stream();
    let body = match read_static_body(opened.file) {
        Ok(body) => body,
        Err(()) => {
            let _ = write_static_response(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"Not found",
                method == "GET",
                "",
            );
            return;
        }
    };
    let _ = write_static_response(stream, "200 OK", mime.as_ref(), &body, method == "GET", "");
}

fn start_static_server(cwd: &str, run_id: &str) -> Result<(StaticServer, String), String> {
    let root = open_static_root(cwd)?;
    open_static_file(&root, "/")
        .map_err(|()| "the static site entrypoint is no longer a regular file".to_string())?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("failed to bind static preview: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to configure static preview: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("failed to inspect static preview address: {error}"))?;
    let (shutdown, shutdown_rx) = mpsc::channel();
    let active_responses = Arc::new(AtomicUsize::new(0));
    let active_thread = Arc::clone(&active_responses);
    let expected_host = format!("127.0.0.1:{}", address.port());
    let name = format!("t-hub-static-preview-{run_id}");
    let thread = std::thread::Builder::new()
        .name(name)
        .spawn(move || loop {
            match shutdown_rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let Some(response_guard) = StaticResponseGuard::reserve(&active_thread) else {
                        continue;
                    };
                    let Ok(request_root) = root.try_clone() else {
                        continue;
                    };
                    let request_host = expected_host.clone();
                    let _ = std::thread::Builder::new()
                        .name("t-hub-static-response".to_string())
                        .spawn(move || {
                            respond_static(
                                &mut stream,
                                &request_root,
                                &request_host,
                                response_guard,
                            )
                        });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    match shutdown_rx.recv_timeout(std::time::Duration::from_millis(25)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
                Err(_) => break,
            }
        })
        .map_err(|error| format!("failed to start static preview thread: {error}"))?;
    Ok((
        StaticServer {
            shutdown,
            thread: Some(thread),
            active_responses,
        },
        format!("http://127.0.0.1:{}/", address.port()),
    ))
}

#[tauri::command]
pub async fn start_dev_server(
    app: AppHandle,
    terminal_id: String,
    cwd: String,
    target: RunTargetRef,
) -> Result<DevServerSnapshot, String> {
    if !matches!(target.kind.as_str(), "packageScript" | "staticSite") {
        return Err("invalid run target".to_string());
    }
    let gate = operation_gate(&terminal_id);
    let _operation_guard = gate.lock().await;
    let run_id = uuid::Uuid::new_v4().to_string();
    let operation = reserve_operation(&mut REGISTRY.lock(), &terminal_id);

    let discovery = match discover_run_targets(cwd.clone()).await {
        Ok(discovery) => discovery,
        Err(reason) => {
            let current = finish_validation_error(&mut REGISTRY.lock(), &terminal_id, operation);
            return Err(if current {
                reason
            } else {
                "the start request was superseded".to_string()
            });
        }
    };
    if discovery.state != "ready" {
        let reason = discovery
            .message
            .unwrap_or_else(|| "run targets are unavailable".to_string());
        let current = finish_validation_error(&mut REGISTRY.lock(), &terminal_id, operation);
        return Err(if current {
            reason
        } else {
            "the start request was superseded".to_string()
        });
    }
    let selected = discovery.targets;
    let selected = match select_target(selected, &target) {
        Some(selected) => selected,
        None => {
            let reason = "the selected run target no longer exists".to_string();
            let current = finish_validation_error(&mut REGISTRY.lock(), &terminal_id, operation);
            return Err(if current {
                reason
            } else {
                "the start request was superseded".to_string()
            });
        }
    };
    let (existing, existing_static) = {
        let mut registry = REGISTRY.lock();
        if !owns_operation(&registry, &terminal_id, operation) {
            return Err("the start request was superseded".to_string());
        }
        registry
            .generations
            .insert(terminal_id.clone(), run_id.clone());
        let revision = next_revision(&mut registry);
        registry.snapshots.insert(
            terminal_id.clone(),
            DevServerSnapshot {
                terminal_id: terminal_id.clone(),
                run_id: Some(run_id.clone()),
                revision,
                state: "starting".to_string(),
                target: Some(selected.clone()),
                exit_code: None,
                reason: None,
                preview_url: None,
                observed_at: observed_at(),
            },
        );
        (
            registry.processes.remove(&terminal_id),
            registry.static_servers.remove(&terminal_id),
        )
    };
    if let Some(process) = existing {
        process.stop();
    }
    if let Some(server) = existing_static {
        server.stop();
    }
    if !owns_operation(&REGISTRY.lock(), &terminal_id, operation) {
        return Err("the start request was superseded".to_string());
    }
    if selected.kind == "staticSite" {
        let (server, preview_url) = match start_static_server(&cwd, &run_id) {
            Ok(started) => started,
            Err(reason) => {
                publish_start_failure(
                    &mut REGISTRY.lock(),
                    &terminal_id,
                    operation,
                    &run_id,
                    Some(selected),
                    &reason,
                );
                return Err(reason);
            }
        };
        let mut server = Some(server);
        let snapshot = {
            let mut registry = REGISTRY.lock();
            if !owns_operation(&registry, &terminal_id, operation)
                || !owns_generation(&registry, &terminal_id, &run_id)
            {
                None
            } else {
                registry.operations.remove(&terminal_id);
                let revision = next_revision(&mut registry);
                let snapshot = DevServerSnapshot {
                    terminal_id: terminal_id.clone(),
                    run_id: Some(run_id.clone()),
                    revision,
                    state: "running".to_string(),
                    target: Some(selected),
                    exit_code: None,
                    reason: None,
                    preview_url: Some(preview_url),
                    observed_at: observed_at(),
                };
                registry.static_servers.insert(
                    terminal_id.clone(),
                    server.take().expect("static server is pending"),
                );
                registry
                    .snapshots
                    .insert(terminal_id.clone(), snapshot.clone());
                let _ = app.emit(
                    &channel(&terminal_id),
                    DevServerEvent::new(
                        &terminal_id,
                        &run_id,
                        snapshot.revision,
                        "started",
                        String::new(),
                    ),
                );
                Some(snapshot)
            }
        };
        if let Some(server) = server {
            server.stop();
        }
        return snapshot.ok_or_else(|| "the start request was superseded".to_string());
    }
    let Some(package_manager) = selected.package_manager else {
        let reason = "package script is missing its package manager".to_string();
        publish_start_failure(
            &mut REGISTRY.lock(),
            &terminal_id,
            operation,
            &run_id,
            Some(selected),
            &reason,
        );
        return Err(reason);
    };
    let Some(script) = selected.script.as_deref() else {
        let reason = "package script is missing its script name".to_string();
        publish_start_failure(
            &mut REGISTRY.lock(),
            &terminal_id,
            operation,
            &run_id,
            Some(selected),
            &reason,
        );
        return Err(reason);
    };
    let mut cmd = build_command(&cwd, &run_id, package_manager, script);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let reason = format!("failed to start dev server: {error}");
            publish_start_failure(
                &mut REGISTRY.lock(),
                &terminal_id,
                operation,
                &run_id,
                Some(selected),
                &reason,
            );
            return Err(reason);
        }
    };

    // Take the piped handles BEFORE moving `child` into the registry. Each is
    // drained on its own thread so stdout and stderr can't deadlock each other.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let job = crate::engine_supervisor::platform::assign_kill_on_close_job(&child).ok();

    let mut pending_process = Some(DevProcess {
        run_id: run_id.clone(),
        child,
        stdin,
        readers: Vec::new(),
        _job: job,
    });
    let snapshot = {
        let mut registry = REGISTRY.lock();
        if !owns_operation(&registry, &terminal_id, operation)
            || !owns_generation(&registry, &terminal_id, &run_id)
        {
            None
        } else {
            registry.operations.remove(&terminal_id);
            let revision = next_revision(&mut registry);
            let snapshot = DevServerSnapshot {
                terminal_id: terminal_id.clone(),
                run_id: Some(run_id.clone()),
                revision,
                state: "running".to_string(),
                target: Some(selected),
                exit_code: None,
                reason: None,
                preview_url: None,
                observed_at: observed_at(),
            };
            registry.processes.insert(
                terminal_id.clone(),
                pending_process
                    .take()
                    .expect("managed process is pending registration"),
            );
            registry
                .snapshots
                .insert(terminal_id.clone(), snapshot.clone());
            let _ = app.emit(
                &channel(&terminal_id),
                DevServerEvent::new(
                    &terminal_id,
                    &run_id,
                    snapshot.revision,
                    "started",
                    String::new(),
                ),
            );
            Some(snapshot)
        }
    };
    if let Some(process) = pending_process {
        process.stop();
        return Err("the start request was superseded".to_string());
    }
    let snapshot = snapshot.expect("registered process has a snapshot");

    let mut readers = Vec::new();
    if let Some(stream) = stdout {
        let app_reader = app.clone();
        let id_reader = terminal_id.clone();
        let run_reader = run_id.clone();
        if let Ok(handle) = std::thread::Builder::new()
            .name(format!("t-hub-devserver-out-{terminal_id}"))
            .spawn(move || pump(&app_reader, &id_reader, &run_reader, stream))
        {
            readers.push(handle);
        }
    }
    if let Some(stream) = stderr {
        let app_reader = app.clone();
        let id_reader = terminal_id.clone();
        let run_reader = run_id.clone();
        if let Ok(handle) = std::thread::Builder::new()
            .name(format!("t-hub-devserver-err-{terminal_id}"))
            .spawn(move || pump(&app_reader, &id_reader, &run_reader, stream))
        {
            readers.push(handle);
        }
    }
    let mut registry = REGISTRY.lock();
    if let Some(process) = registry.processes.get_mut(&terminal_id) {
        if process.run_id == run_id {
            process.readers.append(&mut readers);
        }
    }
    drop(registry);
    for handle in readers {
        let _ = handle.join();
    }
    // A waiter thread reaps the child if it exits ON ITS OWN (crash, or a dev
    // server that runs-then-quits) and emits an `exited` event so the Dev tab can
    // flip back to idle. It only acts if THIS child is still the registered one
    // (a restart/stop already removed+killed it, so we must not double-report).
    let app_wait = app.clone();
    let id_wait = terminal_id.clone();
    std::thread::Builder::new()
        .name(format!("t-hub-devserver-wait-{terminal_id}"))
        .spawn(move || {
            // Poll for natural exit without holding the registry lock across the
            // wait. We can't `child.wait()` here (the registry owns the child), so
            // we periodically try_wait on it under a short lock.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let mut registry = REGISTRY.lock();
                let code = match poll_run(&mut registry, &id_wait, &run_id) {
                    PollOutcome::Running => continue,
                    PollOutcome::Replaced => return,
                    PollOutcome::Exited(code) => code,
                };
                let summary = match code {
                    Some(c) => format!("dev server exited (code {c})"),
                    None => "dev server exited".to_string(),
                };
                let process = registry.processes.remove(&id_wait);
                registry.generations.remove(&id_wait);
                let revision = next_revision(&mut registry);
                let target = registry
                    .snapshots
                    .get(&id_wait)
                    .and_then(|snapshot| snapshot.target.clone());
                registry.snapshots.insert(
                    id_wait.clone(),
                    DevServerSnapshot {
                        terminal_id: id_wait.clone(),
                        run_id: Some(run_id.clone()),
                        revision,
                        state: "exited".to_string(),
                        target,
                        exit_code: code,
                        reason: Some(summary.clone()),
                        preview_url: None,
                        observed_at: observed_at(),
                    },
                );
                drop(registry);
                if let Some(process) = process {
                    process.stop();
                }
                let _ = app_wait.emit(
                    &channel(&id_wait),
                    DevServerEvent::new(&id_wait, &run_id, revision, "exited", summary),
                );
                return;
            }
        })
        .ok();

    Ok(snapshot)
}

#[tauri::command]
pub async fn stop_dev_server(
    terminal_id: String,
    run_id: Option<String>,
) -> Result<DevServerSnapshot, String> {
    let gate = operation_gate(&terminal_id);
    let _operation_guard = gate.lock().await;
    let (operation, process, static_server) = {
        let mut registry = REGISTRY.lock();
        if let Some(expected) = run_id.as_deref() {
            let active = registry
                .generations
                .get(&terminal_id)
                .or_else(|| {
                    registry
                        .snapshots
                        .get(&terminal_id)
                        .and_then(|snapshot| snapshot.run_id.as_ref())
                })
                .map(String::as_str);
            if active.is_some_and(|active| active != expected) {
                return Err("the requested run is no longer active".to_string());
            }
        }
        let operation = reserve_operation(&mut registry, &terminal_id);
        if let Some(mut snapshot) = registry.snapshots.get(&terminal_id).cloned() {
            if snapshot.run_id.is_some() {
                let revision = next_revision(&mut registry);
                snapshot.revision = revision;
                snapshot.state = "stopping".to_string();
                snapshot.observed_at = observed_at();
                registry.snapshots.insert(terminal_id.clone(), snapshot);
            }
        }
        registry.generations.remove(&terminal_id);
        (
            operation,
            registry.processes.remove(&terminal_id),
            registry.static_servers.remove(&terminal_id),
        )
    };
    if let Some(process) = process {
        process.stop();
    }
    if let Some(server) = static_server {
        server.stop();
    }
    finish_stop_snapshot(&mut REGISTRY.lock(), &terminal_id, operation)
}

#[tauri::command]
pub async fn dev_server_snapshot(terminal_id: String) -> Result<DevServerSnapshot, String> {
    let mut registry = REGISTRY.lock();
    if let Some(snapshot) = registry.snapshots.get(&terminal_id) {
        return Ok(snapshot.clone());
    }
    let revision = next_revision(&mut registry);
    let snapshot = idle_snapshot(&terminal_id, revision);
    registry.snapshots.insert(terminal_id, snapshot.clone());
    Ok(snapshot)
}

// ---------------------------------------------------------------------------
// Preview reachability (the WSL2 localhost fix, host-resolution half).
//
// A dev server runs INSIDE WSL; the preview iframe is a WINDOWS process. The
// frontend asks the backend, once, for the host it should substitute for a
// detected/typed `localhost`/`127.0.0.1` URL so the iframe actually reaches the
// server. On unix (the WSL dev build, and Linux/macOS native) `localhost` is
// already correct, so we return None and the frontend leaves the URL alone.
// On Windows we return the WSL distro's interface IP (its `eth0` address as
// seen on the shared/mirrored network), which IS reachable from Windows for a
// server bound to `0.0.0.0` (see `host_binding_prefix`).
// ---------------------------------------------------------------------------

/// The WSL distro's primary IPv4 address as seen from the Windows host (the
/// shared interface in mirrored mode; the NAT'd `eth0` otherwise). Queried via
/// `wsl.exe -e bash -lc 'hostname -I'` and trimmed to the first address. `None` if the
/// lookup fails (the frontend then keeps `localhost`, which is still correct in
/// mirrored mode for a `0.0.0.0`-bound server).
#[cfg(windows)]
fn wsl_host_ip() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("wsl.exe");
    // `-e` (exec) runs bash DIRECTLY. A bare `--` re-joins the tail through the
    // default shell, splitting the quoted `hostname -I` script into separate
    // words (see the note on tmux.rs::pane_info_command).
    c.arg("-d")
        .arg(crate::files::host_distro())
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        // `hostname -I` lists this host's addresses (space-separated); the first
        // is the primary interface. `ip route get 1` would also work but this is
        // simpler and matches how the rest of T-Hub probes WSL.
        .arg("hostname -I");
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                                   // Bounded (WSL_PROBE): a trivial `hostname -I`; a cold/wedged WSL must not park
                                   // the `preview_host` handler this runs on.
    let out =
        crate::bounded_exec::output_with_timeout(c, crate::bounded_exec::WSL_PROBE_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.split_whitespace().next()?.trim();
    // Sanity: looks like a dotted IPv4 and isn't loopback (which wouldn't help).
    if first.is_empty() || first.starts_with("127.") || !first.contains('.') {
        return None;
    }
    Some(first.to_string())
}

/// Return the host the preview iframe should use in place of `localhost` /
/// `127.0.0.1` to reach a WSL-bound dev server, or `None` when no rewrite is
/// needed (unix builds, where the WebView and the server share a loopback).
///
/// On Windows this is the WSL interface IP. Cached for the process lifetime —
/// the address is stable for a WSL session and the lookup spawns `wsl.exe`.
#[tauri::command]
pub async fn preview_host() -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        Ok(CACHE.get_or_init(wsl_host_ip).clone())
    }
    #[cfg(not(windows))]
    {
        // Linux/macOS (incl. the WSL dev build): the dev server and the WebView
        // are on the same loopback; `localhost` already reaches it.
        Ok(None)
    }
}

/// Core of [`probe_tcp`]: does `host:port` accept a TCP connection within
/// `timeout_ms`? Split out (sync) so the command is a thin wrapper and the unit
/// test can exercise it without an async runtime.
fn tcp_reachable(host: &str, port: u16, timeout_ms: u64) -> Result<bool, String> {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let host = host.trim();
    if host.is_empty() {
        return Err("empty host".to_string());
    }
    // Resolve the host:port to socket addresses (handles "localhost", IPv4, and
    // IPv6); try each until one connects within the budget.
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {host}:{port}: {e}"))?;
    let budget = Duration::from_millis(timeout_ms.clamp(50, 10_000));
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, budget).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Probe whether `host:port` accepts a TCP connection within `timeout_ms`,
/// from the SAME process/host as the WebView (so the result reflects what the
/// preview iframe would see). Lets the frontend tell "connection refused / not
/// up" apart from "up but refused framing", and surface a precise message
/// instead of the silent watchdog "blocked".
///
/// Returns `Ok(true)` if the TCP handshake succeeds, `Ok(false)` if it is
/// refused or times out. A malformed `host`/`port` is an `Err`.
#[tauri::command]
pub async fn probe_tcp(host: String, port: u16, timeout_ms: u64) -> Result<bool, String> {
    tcp_reachable(&host, port, timeout_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name_is_per_terminal() {
        assert_eq!(channel("abc123"), "devserver://abc123");
    }

    #[test]
    fn event_kinds_are_tagged() {
        let event = DevServerEvent::new("x", "run-1", 7, "line", "hi".into());
        assert_eq!(event.kind, "line");
        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.revision, 7);
        assert_eq!(event.line, "hi");
    }

    #[test]
    fn package_manager_accepts_versioned_declarations() {
        assert_eq!(
            parse_package_manager("pnpm@9.15.0"),
            Some(PackageManager::Pnpm)
        );
        assert_eq!(parse_package_manager("npm@11"), Some(PackageManager::Npm));
        assert_eq!(parse_package_manager("unknown@1"), None);
    }

    #[test]
    fn lockfile_fallback_requires_one_unambiguous_manager() {
        assert_eq!(
            lockfile_manager(&["pnpm-lock.yaml".into()]),
            PackageManager::Pnpm
        );
        assert_eq!(
            lockfile_manager(&["yarn.lock".into(), "package-lock.json".into()]),
            PackageManager::Npm
        );
        assert_eq!(lockfile_manager(&[]), PackageManager::Npm);
    }

    #[test]
    fn targets_are_ranked_and_script_names_remain_data() {
        let targets = parse_targets(
            r#"{"scripts":{"z":"echo z","preview":"vite preview","dev":"vite","odd; $name":"echo safe"}}"#,
            PackageManager::Pnpm,
        )
        .expect("valid targets");
        assert_eq!(targets[0].script.as_deref(), Some("dev"));
        assert!(targets[0].recommended);
        assert_eq!(targets[1].script.as_deref(), Some("preview"));
        assert_eq!(targets[2].script.as_deref(), Some("odd; $name"));
        assert_eq!(targets[2].command_display, "pnpm run odd; $name");
        assert_eq!(targets[3].script.as_deref(), Some("z"));
    }

    #[test]
    fn invalid_package_shapes_are_rejected() {
        assert!(parse_targets("[]", PackageManager::Npm).is_err());
        assert!(parse_targets(r#"{"scripts":[]}"#, PackageManager::Npm).is_err());
        assert!(parse_targets("not json", PackageManager::Npm).is_err());
    }

    #[test]
    fn regular_root_index_produces_a_typed_static_target() {
        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "STATIC SENTINEL").expect("write index");
        let target = static_target(root.path().to_str().expect("utf8 path"))
            .expect("inspect target")
            .expect("static target");
        assert_eq!(target.kind, "staticSite");
        assert_eq!(target.id, "static-site:root");
        assert_eq!(target.entrypoint.as_deref(), Some("index.html"));
        assert_eq!(target.relative_root.as_deref(), Some("."));
        assert!(target.script.is_none());
        assert!(target.package_manager.is_none());
    }

    #[test]
    fn package_and_static_targets_coexist_with_package_priority() {
        let root = tempfile::tempdir().expect("combined fixture root");
        fs::write(
            root.path().join("package.json"),
            r#"{"scripts":{"dev":"vite"}}"#,
        )
        .expect("write package");
        fs::write(root.path().join("index.html"), "STATIC SENTINEL").expect("write index");
        let discovery = tauri::async_runtime::block_on(discover_run_targets(
            root.path().to_string_lossy().into_owned(),
        ))
        .expect("discover combined targets");
        assert_eq!(discovery.state, "ready");
        assert_eq!(discovery.targets.len(), 2);
        assert_eq!(discovery.targets[0].kind, "packageScript");
        assert!(discovery.targets[0].recommended);
        assert_eq!(discovery.targets[1].kind, "staticSite");
        assert!(!discovery.targets[1].recommended);
    }

    #[test]
    fn typed_target_selection_rejects_forged_static_and_package_references() {
        let package = parse_targets(r#"{"scripts":{"dev":"vite"}}"#, PackageManager::Pnpm)
            .expect("package target")
            .remove(0);
        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "STATIC SENTINEL").expect("write index");
        let static_site = static_target(root.path().to_str().unwrap())
            .expect("inspect static")
            .expect("static target");
        let targets = vec![package, static_site];

        assert!(select_target(
            targets.clone(),
            &RunTargetRef {
                kind: "staticSite".to_string(),
                script: None,
                id: Some("static-site:other".to_string()),
            },
        )
        .is_none());
        assert!(select_target(
            targets,
            &RunTargetRef {
                kind: "packageScript".to_string(),
                script: Some("missing".to_string()),
                id: None,
            },
        )
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn directory_and_symlink_entrypoints_are_not_advertised() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("static fixture root");
        fs::create_dir(root.path().join("index.html")).expect("directory entrypoint");
        assert!(static_target(root.path().to_str().unwrap())
            .expect("inspect directory")
            .is_none());
        fs::remove_dir(root.path().join("index.html")).expect("remove directory");
        let outside = root.path().join("outside.html");
        fs::write(&outside, "OUTSIDE").expect("outside file");
        symlink(&outside, root.path().join("index.html")).expect("symlink entrypoint");
        assert!(static_target(root.path().to_str().unwrap())
            .expect("inspect symlink")
            .is_none());
    }

    #[test]
    fn static_file_open_rejects_traversal_hidden_and_oversized_files() {
        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "INDEX").expect("write index");
        fs::write(root.path().join(".env"), "SECRET").expect("write hidden");
        let oversized = root.path().join("large.bin");
        let file = fs::File::create(&oversized).expect("create oversized fixture");
        file.set_len(MAX_STATIC_FILE_BYTES + 1)
            .expect("size oversized fixture");
        let root_handle = open_static_root(root.path().to_str().unwrap()).expect("open root");

        let index = open_static_file(&root_handle, "/").expect("open index");
        assert_eq!(read_static_body(index.file).unwrap(), b"INDEX");
        for path in [
            "/../outside",
            "/%2e%2e/outside",
            "/%252e%252e/outside",
            "/.env",
            "/%2eenv",
            "/a\\b",
            "/C:/secret",
            "/large.bin",
        ] {
            assert!(open_static_file(&root_handle, path).is_err(), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn static_file_open_rejects_final_parent_and_root_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("static fixture root");
        let outside = tempfile::tempdir().expect("outside fixture root");
        fs::write(outside.path().join("secret.txt"), "SECRET").expect("outside sentinel");
        symlink(outside.path(), root.path().join("escape")).expect("escape symlink");
        symlink(
            outside.path().join("secret.txt"),
            root.path().join("final.txt"),
        )
        .expect("final symlink");
        let root_handle = open_static_root(root.path().to_str().unwrap()).expect("open root");
        assert!(open_static_file(&root_handle, "/escape/secret.txt").is_err());
        assert!(open_static_file(&root_handle, "/final.txt").is_err());

        let links = tempfile::tempdir().expect("root-link fixture");
        let root_link = links.path().join("root-link");
        symlink(root.path(), &root_link).expect("root symlink");
        assert!(open_static_root(root_link.to_str().unwrap()).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn static_file_open_rejects_a_directory_junction() {
        let root = tempfile::tempdir().expect("static fixture root");
        let outside = tempfile::tempdir().expect("outside fixture root");
        fs::write(outside.path().join("secret.txt"), "SECRET").expect("outside sentinel");
        let junction = root.path().join("escape");
        let output = Command::new("cmd.exe")
            .arg("/D")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&junction)
            .arg(outside.path())
            .output()
            .expect("create junction");
        assert!(
            output.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let root_handle = open_static_root(root.path().to_str().unwrap()).expect("open root");
        assert!(open_static_file(&root_handle, "/escape/secret.txt").is_err());
    }

    #[test]
    fn opened_static_file_keeps_the_validated_handle_after_path_replacement() {
        let root = tempfile::tempdir().expect("static fixture root");
        let slot = root.path().join("slot");
        fs::create_dir(&slot).expect("create slot");
        fs::write(slot.join("asset.txt"), "SAFE").expect("write safe asset");
        let root_handle = open_static_root(root.path().to_str().unwrap()).expect("open root");
        let opened = open_static_file(&root_handle, "/slot/asset.txt").expect("open asset");

        let old_slot = root.path().join("old-slot");
        match fs::rename(&slot, &old_slot) {
            Ok(()) => {
                fs::create_dir(&slot).expect("replace slot");
                fs::write(slot.join("asset.txt"), "REPLACEMENT").expect("replacement asset");
            }
            Err(error) if cfg!(windows) => {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            }
            Err(error) => panic!("replace opened parent: {error}"),
        }

        assert_eq!(read_static_body(opened.file).unwrap(), b"SAFE");
    }

    #[test]
    fn bounded_static_read_rejects_growth_after_open() {
        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "INDEX").expect("write index");
        let asset = root.path().join("growing.bin");
        fs::write(&asset, "small").expect("write growing asset");
        let root_handle = open_static_root(root.path().to_str().unwrap()).expect("open root");
        let opened = open_static_file(&root_handle, "/growing.bin").expect("open asset");
        fs::OpenOptions::new()
            .write(true)
            .open(asset)
            .expect("reopen growing asset")
            .set_len(MAX_STATIC_FILE_BYTES + 1)
            .expect("grow asset");
        assert!(read_static_body(opened.file).is_err());
    }

    #[test]
    fn static_server_serves_get_head_and_mime_then_stops_its_port() {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        fn raw_request(port: u16, request: &str) -> String {
            let mut stream =
                TcpStream::connect(("127.0.0.1", port)).expect("connect static server");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            stream.write_all(request.as_bytes()).expect("write request");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("read response");
            response
        }

        fn request(port: u16, method: &str, path: &str) -> String {
            raw_request(
                port,
                &format!(
                    "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                ),
            )
        }

        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "STATIC SENTINEL").expect("write index");
        fs::write(root.path().join("style.css"), "body { color: red; }").expect("write css");
        let (server, url) = start_static_server(root.path().to_str().unwrap(), "static-test")
            .expect("start static server");
        let port = url
            .trim_end_matches('/')
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .expect("static port");

        let index = request(port, "GET", "/?v=1");
        assert!(index.starts_with("HTTP/1.1 200"));
        assert!(index.contains("STATIC SENTINEL"));
        assert!(index
            .to_ascii_lowercase()
            .contains("x-content-type-options: nosniff"));
        assert!(index
            .to_ascii_lowercase()
            .contains("cache-control: no-store"));
        let css = request(port, "GET", "/style.css");
        assert!(css.to_ascii_lowercase().contains("content-type: text/css"));
        let head = request(port, "HEAD", "/index.html");
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(!head.contains("STATIC SENTINEL"));
        assert!(request(port, "POST", "/").starts_with("HTTP/1.1 405"));
        assert!(request(port, "GET", "/.env").starts_with("HTTP/1.1 404"));
        for bad_host_request in [
            "GET / HTTP/1.1\r\nConnection: close\r\n\r\n".to_string(),
            "GET / HTTP/1.1\r\nHost: attacker.example\r\nConnection: close\r\n\r\n"
                .to_string(),
            format!(
                "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            ),
        ] {
            assert!(raw_request(port, &bad_host_request).starts_with("HTTP/1.1 421"));
        }

        fs::remove_file(root.path().join("index.html")).expect("remove live entrypoint");
        assert!(request(port, "GET", "/").starts_with("HTTP/1.1 404"));
        server.stop();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "static preview port remained reachable after Stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn static_server_stop_is_bounded_by_a_nonreading_client() {
        use std::io::Write;
        use std::net::TcpStream;
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "INDEX").expect("write index");
        let large = fs::File::create(root.path().join("large.bin")).expect("large asset");
        large
            .set_len(MAX_STATIC_FILE_BYTES)
            .expect("size large asset");
        let (server, url) = start_static_server(root.path().to_str().unwrap(), "blocked-test")
            .expect("start static server");
        let port = url
            .trim_end_matches('/')
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .expect("static port");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect client");
        socket2::SockRef::from(&client)
            .set_recv_buffer_size(1024)
            .expect("shrink client receive buffer");
        client
            .write_all(
                format!(
                    "GET /large.bin HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("send blocked request");
        let active_responses = Arc::clone(&server.active_responses);
        let accepted_deadline = Instant::now() + Duration::from_secs(2);
        while active_responses.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < accepted_deadline,
                "request was not accepted"
            );
            std::thread::yield_now();
        }

        let started = Instant::now();
        server.stop();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "static Stop exceeded its response timeout"
        );
        assert_eq!(active_responses.load(Ordering::SeqCst), 0);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        drop(client);
    }

    #[test]
    fn static_server_stop_is_bounded_by_a_trickled_request() {
        use std::io::Write;
        use std::net::TcpStream;
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().expect("static fixture root");
        fs::write(root.path().join("index.html"), "INDEX").expect("write index");
        let (server, url) = start_static_server(root.path().to_str().unwrap(), "slowloris-test")
            .expect("start static server");
        let port = url
            .trim_end_matches('/')
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .expect("static port");
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect client");
        client.write_all(b"G").expect("start trickled request");
        let active_responses = Arc::clone(&server.active_responses);
        let accepted_deadline = Instant::now() + Duration::from_secs(2);
        while active_responses.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < accepted_deadline,
                "request was not accepted"
            );
            std::thread::yield_now();
        }
        let writer = std::thread::spawn(move || {
            for byte in b"ET / HTTP/1.1\r\nHost: 127.0.0.1" {
                std::thread::sleep(Duration::from_millis(100));
                if client.write_all(&[*byte]).is_err() {
                    break;
                }
            }
        });

        let started = Instant::now();
        server.stop();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "static Stop exceeded its absolute request deadline"
        );
        assert_eq!(active_responses.load(Ordering::SeqCst), 0);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        writer.join().expect("join trickle writer");
    }

    #[test]
    fn static_response_admission_is_bounded_per_server() {
        let active = Arc::new(AtomicUsize::new(0));
        let guards = (0..MAX_STATIC_CONCURRENT_RESPONSES)
            .map(|_| StaticResponseGuard::reserve(&active).expect("reserve response worker"))
            .collect::<Vec<_>>();
        assert!(StaticResponseGuard::reserve(&active).is_none());
        assert_eq!(
            active.load(Ordering::SeqCst),
            MAX_STATIC_CONCURRENT_RESPONSES
        );
        drop(guards);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stale_start_failure_cannot_overwrite_a_replacement_generation() {
        let mut registry = DevRegistry::default();
        registry
            .generations
            .insert("tile".to_string(), "run-new".to_string());
        registry.operations.insert("tile".to_string(), 2);
        let mut replacement = idle_snapshot("tile", 7);
        replacement.run_id = Some("run-new".to_string());
        replacement.state = "starting".to_string();
        registry
            .snapshots
            .insert("tile".to_string(), replacement.clone());

        publish_start_failure(
            &mut registry,
            "tile",
            1,
            "run-old",
            None,
            "old start failed",
        );

        assert!(owns_generation(&registry, "tile", "run-new"));
        assert_eq!(registry.snapshots.get("tile"), Some(&replacement));
    }

    #[test]
    fn validation_failure_preserves_the_active_run() {
        let mut registry = DevRegistry::default();
        registry
            .generations
            .insert("tile".to_string(), "run-active".to_string());
        let mut active = idle_snapshot("tile", 5);
        active.run_id = Some("run-active".to_string());
        active.state = "running".to_string();
        registry
            .snapshots
            .insert("tile".to_string(), active.clone());
        let operation = reserve_operation(&mut registry, "tile");

        assert!(finish_validation_error(&mut registry, "tile", operation));
        assert!(owns_generation(&registry, "tile", "run-active"));
        assert_eq!(registry.snapshots.get("tile"), Some(&active));
        assert!(!registry.operations.contains_key("tile"));
    }

    #[test]
    fn operation_gate_serializes_one_terminal_without_blocking_another() {
        use std::sync::mpsc;
        use std::time::Duration;

        let terminal = format!("gate-{}", uuid::Uuid::new_v4());
        let other_terminal = format!("gate-{}", uuid::Uuid::new_v4());
        let gate = operation_gate(&terminal);
        let same_gate = operation_gate(&terminal);
        let other_gate = operation_gate(&other_terminal);
        assert!(Arc::ptr_eq(&gate, &same_gate));
        let held = tauri::async_runtime::block_on(gate.lock());
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tauri::async_runtime::block_on(async {
                let _guard = same_gate.lock().await;
                acquired_tx.send(()).expect("report same-terminal lock");
            });
        });

        assert!(other_gate.try_lock().is_some());
        assert!(
            acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "same-terminal operation was not serialized"
        );
        drop(held);
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("same-terminal waiter should acquire after release");
        waiter.join().expect("join operation waiter");
    }

    #[test]
    fn blocking_stop_completion_cannot_overwrite_a_replacement_snapshot() {
        let mut registry = DevRegistry::default();
        registry
            .generations
            .insert("tile".to_string(), "run-new".to_string());
        registry.operations.insert("tile".to_string(), 2);
        let mut replacement = idle_snapshot("tile", 9);
        replacement.run_id = Some("run-new".to_string());
        replacement.state = "running".to_string();
        registry
            .snapshots
            .insert("tile".to_string(), replacement.clone());

        let returned = finish_stop_snapshot(&mut registry, "tile", 1).expect("finish stale stop");

        assert_eq!(returned, replacement);
        assert_eq!(registry.snapshots.get("tile"), Some(&replacement));
        assert!(owns_generation(&registry, "tile", "run-new"));
    }

    #[cfg(not(windows))]
    #[test]
    fn stale_waiter_and_stop_cannot_own_a_replacement_run() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .expect("spawn replacement fixture");
        let mut registry = DevRegistry::default();
        registry.processes.insert(
            "tile".to_string(),
            DevProcess {
                run_id: "run-new".to_string(),
                child,
                stdin: None,
                readers: Vec::new(),
                _job: None,
            },
        );

        assert_eq!(
            poll_run(&mut registry, "tile", "run-old"),
            PollOutcome::Replaced
        );
        assert!(take_process_for_stop(&mut registry, "tile", Some("run-old")).is_err());
        assert!(registry.processes.contains_key("tile"));

        let mut process = take_process_for_stop(&mut registry, "tile", Some("run-new"))
            .expect("matching stop should be valid")
            .expect("replacement should remain registered");
        let _ = process.child.kill();
        process.stop();
    }

    #[cfg(windows)]
    #[test]
    fn unc_to_posix_recovers_wsl_paths() {
        assert_eq!(
            unc_to_posix("\\\\wsl.localhost\\Ubuntu-24.04\\home\\natkins\\proj"),
            Some("/home/natkins/proj".to_string())
        );
        // Bare POSIX passes through; a real Windows drive path does not map.
        assert_eq!(unc_to_posix("/home/x"), Some("/home/x".to_string()));
        assert_eq!(unc_to_posix("C:\\Users\\natha"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn build_command_keeps_script_as_one_argument() {
        let command = build_command(
            "/tmp",
            "run-test",
            PackageManager::Pnpm,
            "odd; $(unsafe) ' name",
        );
        assert_eq!(command.get_program(), "bash");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-c",
                PROCESS_TREE_SCRIPT,
                "t-hub-runner",
                "run-test",
                "pnpm",
                "run",
                "odd; $(unsafe) ' name",
            ]
        );
        assert!(PROCESS_TREE_SCRIPT.contains("TAURI_DEV_HOST=0.0.0.0"));
        assert!(PROCESS_TREE_SCRIPT.contains("setsid \"$@\" 3<&- </dev/null &"));
        assert!(PROCESS_TREE_SCRIPT.contains("kill -TERM -- -\"$SRV\""));
        assert!(PROCESS_TREE_SCRIPT.contains("kill -KILL -- -\"$SRV\""));
    }

    #[cfg(not(windows))]
    #[test]
    fn stop_reaps_a_term_ignoring_descendant_and_unblocks_its_reader() {
        use std::io::Read;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg("t-hub-runner")
            .arg("tree-test")
            .arg("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30 & echo $!; wait")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn supervised fixture");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("fixture stdout");
        let mut stderr = child.stderr.take().expect("fixture stderr");
        let (pid_tx, pid_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(stdout);
            let mut first = String::new();
            output.read_line(&mut first).expect("read descendant pid");
            if first.trim().is_empty() {
                let mut error = String::new();
                let _ = stderr.read_to_string(&mut error);
                panic!("fixture exited before reporting its descendant: {error}");
            }
            pid_tx
                .send(first.trim().parse::<u32>().expect("numeric descendant pid"))
                .expect("send descendant pid");
            let mut rest = Vec::new();
            let _ = output.read_to_end(&mut rest);
        });
        let descendant = pid_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("fixture should report its descendant");
        assert!(std::path::Path::new(&format!("/proc/{descendant}")).exists());

        let started = Instant::now();
        DevProcess {
            run_id: "tree-test".to_string(),
            child,
            stdin,
            readers: vec![reader],
            _job: None,
        }
        .stop();

        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(!std::path::Path::new(&format!("/proc/{descendant}")).exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn natural_parent_exit_reaps_its_surviving_descendant() {
        use std::time::{Duration, Instant};

        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg("t-hub-runner")
            .arg("early-exit-test")
            .arg("sh")
            .arg("-c")
            .arg("sleep 30 & echo $!; exit 0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("spawn early-exit fixture");
        let _stdin = child.stdin.take();
        let mut output = BufReader::new(child.stdout.take().expect("fixture stdout"));
        let mut first = String::new();
        output.read_line(&mut first).expect("read descendant pid");
        let descendant = first.trim().parse::<u32>().expect("numeric descendant pid");

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success());
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                other => {
                    let _ = child.kill();
                    panic!("supervisor did not reap an early-exit tree: {other:?}");
                }
            }
        }
        assert!(!std::path::Path::new(&format!("/proc/{descendant}")).exists());
    }

    /// The TCP probe should connect to a port we open and report it refused once
    /// closed. Uses an ephemeral listener so the test is hermetic.
    ///
    /// De-flaked: instead of a single probe per phase (which assumes the OS has
    /// already settled the socket into the expected state), each phase polls
    /// `tcp_reachable` with a deadline until the expected reachability is observed.
    /// The open phase is normally instant; the *closed* phase is the one that can
    /// lag — dropping the listener releases the port asynchronously, so a fresh
    /// probe can momentarily still connect (e.g. to a half-open socket) on a loaded
    /// box. Polling until refused (or a short timeout) removes the fixed-time
    /// assumption while still asserting the same open→closed transition.
    #[test]
    fn tcp_reachable_detects_open_then_closed() {
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        // Poll `tcp_reachable` until it returns `want`, or fail after `deadline`.
        // Each probe carries a tight connect budget so the loop is responsive; the
        // overall deadline (not any single probe) bounds the wait.
        fn poll_until_reachable(host: &str, port: u16, want: bool, deadline: Duration) -> bool {
            let start = Instant::now();
            loop {
                if tcp_reachable(host, port, 50).unwrap() == want {
                    return true;
                }
                if start.elapsed() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().unwrap().port();

        // Open: a listener is accepting, so the handshake succeeds (effectively
        // immediate, but poll for symmetry / to absorb any scheduling hiccup).
        assert!(
            poll_until_reachable("127.0.0.1", port, true, Duration::from_secs(2)),
            "expected the open port to accept a connection"
        );

        // Closed: drop the listener, then poll until a fresh probe is refused. The
        // refusal may not be observable on the very first probe after drop, so we
        // wait (bounded) for the port to be released rather than assuming a fixed
        // settle time.
        drop(listener);
        assert!(
            poll_until_reachable("127.0.0.1", port, false, Duration::from_secs(2)),
            "expected the closed port to refuse once the listener is released"
        );
    }
}
