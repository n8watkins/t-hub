//! Managed package-script runner for the per-project Run and Preview surface.
//!
//! The backend discovers typed targets from the root `package.json`, validates a
//! selected target again at start time, constructs executable arguments itself,
//! and owns authoritative generation-safe lifecycle snapshots. Frontend-provided
//! shell text is never executed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::thread::JoinHandle;

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
    pub script: String,
    pub label: String,
    pub package_manager: PackageManager,
    pub command_display: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageScriptTargetRef {
    pub kind: String,
    pub script: String,
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
    readers: Vec<JoinHandle<()>>,
}

impl DevProcess {
    /// Kill the child and join its reader thread. Best-effort: the child may have
    /// already exited on its own (the reader will have hit EOF and be exiting).
    fn stop(mut self) {
        // Killing the child closes its stdout pipe, so the reader thread hits EOF
        // and returns; then we join it so it never outlives the process.
        let _ = self.child.kill();
        let _ = self.child.wait();
        for handle in self.readers.drain(..) {
            let _ = handle.join();
        }
    }
}

#[derive(Default)]
struct DevRegistry {
    processes: HashMap<String, DevProcess>,
    snapshots: HashMap<String, DevServerSnapshot>,
    revision: u64,
}

static REGISTRY: LazyLock<Mutex<DevRegistry>> =
    LazyLock::new(|| Mutex::new(DevRegistry::default()));

#[derive(Debug, PartialEq, Eq)]
enum PollOutcome {
    Running,
    Exited(Option<i32>),
    Replaced,
}

fn poll_run(registry: &mut DevRegistry, terminal_id: &str, run_id: &str) -> PollOutcome {
    match registry.processes.get_mut(terminal_id) {
        Some(process) if process.run_id == run_id => match process.child.try_wait() {
            Ok(Some(status)) => PollOutcome::Exited(status.code()),
            Ok(None) => PollOutcome::Running,
            Err(_) => PollOutcome::Exited(None),
        },
        _ => PollOutcome::Replaced,
    }
}

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
        observed_at: observed_at(),
    }
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
            package_manager,
            recommended: index == 0,
            script,
        })
        .collect())
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
    if !entries.iter().any(|entry| entry.name == "package.json") {
        return Ok(RunTargetDiscovery {
            state: "notFound".to_string(),
            targets: Vec::new(),
            message: Some("No root package.json was found.".to_string()),
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
        Ok(targets) => Ok(RunTargetDiscovery {
            state: "ready".to_string(),
            message: targets
                .is_empty()
                .then(|| "No package scripts are defined.".to_string()),
            targets,
        }),
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
#[cfg(not(windows))]
fn apply_host_binding(command: &mut Command) {
    command
        .env("HOST", "0.0.0.0")
        .env("HOSTNAME", "0.0.0.0")
        .env("NUXT_HOST", "0.0.0.0")
        .env("ASTRO_HOST", "0.0.0.0")
        .env("TAURI_DEV_HOST", "0.0.0.0");
}

/// Build a package-manager invocation from backend-owned executable and argv.
/// The validated script name is always one argument and is never shell source.
fn build_command(cwd: &str, package_manager: PackageManager, script: &str) -> Command {
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
            .arg("-lc")
            .arg("export HOST=0.0.0.0 HOSTNAME=0.0.0.0 NUXT_HOST=0.0.0.0 ASTRO_HOST=0.0.0.0 TAURI_DEV_HOST=0.0.0.0; exec \"$@\"")
            .arg("t-hub-runner")
            .arg(package_manager.executable())
            .arg("run")
            .arg(script);
        c.creation_flags(0x0800_0000);
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new(package_manager.executable());
        c.arg("run").arg(script);
        if !cwd.is_empty() {
            c.current_dir(cwd);
        }
        apply_host_binding(&mut c);
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
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

#[tauri::command]
pub async fn start_dev_server(
    app: AppHandle,
    terminal_id: String,
    cwd: String,
    target: PackageScriptTargetRef,
) -> Result<DevServerSnapshot, String> {
    if target.kind != "packageScript" || target.script.trim().is_empty() {
        return Err("invalid run target".to_string());
    }
    let discovery = discover_run_targets(cwd.clone()).await?;
    if discovery.state != "ready" {
        return Err(discovery
            .message
            .unwrap_or_else(|| "run targets are unavailable".to_string()));
    }
    let selected = discovery
        .targets
        .into_iter()
        .find(|candidate| candidate.script == target.script)
        .ok_or_else(|| format!("package script no longer exists: {}", target.script))?;

    let existing = REGISTRY.lock().processes.remove(&terminal_id);
    if let Some(process) = existing {
        process.stop();
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    {
        let mut registry = REGISTRY.lock();
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
                observed_at: observed_at(),
            },
        );
    }
    let mut cmd = build_command(&cwd, selected.package_manager, &selected.script);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let reason = format!("failed to start dev server: {error}");
            let mut registry = REGISTRY.lock();
            let revision = next_revision(&mut registry);
            registry.snapshots.insert(
                terminal_id.clone(),
                DevServerSnapshot {
                    terminal_id,
                    run_id: Some(run_id),
                    revision,
                    state: "failed".to_string(),
                    target: Some(selected),
                    exit_code: None,
                    reason: Some(reason.clone()),
                    observed_at: observed_at(),
                },
            );
            return Err(reason);
        }
    };

    // Take the piped handles BEFORE moving `child` into the registry. Each is
    // drained on its own thread so stdout and stderr can't deadlock each other.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let snapshot = {
        let mut registry = REGISTRY.lock();
        let revision = next_revision(&mut registry);
        let snapshot = DevServerSnapshot {
            terminal_id: terminal_id.clone(),
            run_id: Some(run_id.clone()),
            revision,
            state: "running".to_string(),
            target: Some(selected),
            exit_code: None,
            reason: None,
            observed_at: observed_at(),
        };
        registry.processes.insert(
            terminal_id.clone(),
            DevProcess {
                run_id: run_id.clone(),
                child,
                readers: Vec::new(),
            },
        );
        registry
            .snapshots
            .insert(terminal_id.clone(), snapshot.clone());
        snapshot
    };

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
    let process = {
        let mut registry = REGISTRY.lock();
        take_process_for_stop(&mut registry, &terminal_id, run_id.as_deref())?
    };
    if let Some(process) = process {
        process.stop();
    }
    let mut registry = REGISTRY.lock();
    let revision = next_revision(&mut registry);
    let snapshot = idle_snapshot(&terminal_id, revision);
    registry
        .snapshots
        .insert(terminal_id.clone(), snapshot.clone());
    Ok(snapshot)
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
        assert_eq!(targets[0].script, "dev");
        assert!(targets[0].recommended);
        assert_eq!(targets[1].script, "preview");
        assert_eq!(targets[2].script, "odd; $name");
        assert_eq!(targets[2].command_display, "pnpm run odd; $name");
        assert_eq!(targets[3].script, "z");
    }

    #[test]
    fn invalid_package_shapes_are_rejected() {
        assert!(parse_targets("[]", PackageManager::Npm).is_err());
        assert!(parse_targets(r#"{"scripts":[]}"#, PackageManager::Npm).is_err());
        assert!(parse_targets("not json", PackageManager::Npm).is_err());
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
                readers: Vec::new(),
            },
        );

        assert_eq!(
            poll_run(&mut registry, "tile", "run-old"),
            PollOutcome::Replaced
        );
        assert!(take_process_for_stop(&mut registry, "tile", Some("run-old")).is_err());
        assert!(registry.processes.contains_key("tile"));

        let process = take_process_for_stop(&mut registry, "tile", Some("run-new"))
            .expect("matching stop should be valid")
            .expect("replacement should remain registered");
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
        let command = build_command("/tmp", PackageManager::Pnpm, "odd; $(unsafe) ' name");
        assert_eq!(command.get_program(), "pnpm");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["run", "odd; $(unsafe) ' name"]
        );
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "HOST" && value == Some(std::ffi::OsStr::new("0.0.0.0"))));
        assert!(command.get_envs().any(|(name, value)| {
            name == "TAURI_DEV_HOST" && value == Some(std::ffi::OsStr::new("0.0.0.0"))
        }));
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
