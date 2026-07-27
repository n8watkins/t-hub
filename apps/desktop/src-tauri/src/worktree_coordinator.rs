//! Durable coordination for artifact cleanup reservations.
//!
//! A reservation is intentionally separate from Git worktree removal.
//! It blocks new activity in one exact linked worktree while an external storage
//! provider reclaims Cargo artifacts.
//! Completed records remain durable for recovery and audit, but only active
//! records participate in admission decisions.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SCHEMA_VERSION: u32 = 1;
const PROVIDER_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const INSPECTION_OUTPUT_LIMIT: usize = 1024 * 1024;
const LAST_ERROR_LIMIT: usize = 8192;

const INSPECTION_SCRIPT: &str = r#"
import json
import os
import pathlib
import stat
import subprocess
import sys

def run_git(root, *args, check=True):
    result = subprocess.run(
        ["git", "-C", root, *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if check and result.returncode != 0:
        raise RuntimeError(
            "git " + " ".join(args) + " failed: " + result.stderr.strip()
        )
    return result

requested = sys.argv[1]
if not requested.startswith("/"):
    raise RuntimeError("worktree path must be absolute")
worktree = pathlib.Path(requested)
worktree_lstat = worktree.lstat()
if stat.S_ISLNK(worktree_lstat.st_mode) or not stat.S_ISDIR(worktree_lstat.st_mode):
    raise RuntimeError("worktree must be a real directory, not a symlink")
resolved = str(worktree.resolve(strict=True))
if resolved != requested.rstrip("/"):
    raise RuntimeError("worktree path must already be canonical")

root = run_git(resolved, "rev-parse", "--show-toplevel").stdout.strip()
if root != resolved:
    raise RuntimeError("path is not the exact Git worktree root")
head = run_git(resolved, "rev-parse", "--verify", "HEAD^{commit}").stdout.strip()
branch_result = run_git(
    resolved,
    "symbolic-ref",
    "--quiet",
    "--short",
    "HEAD",
    check=False,
)
branch = branch_result.stdout.strip() if branch_result.returncode == 0 else None
if branch is None:
    raise RuntimeError("detached worktrees are not eligible for Cargo cleanup")
dirty = bool(run_git(resolved, "status", "--porcelain", "-z").stdout)

porcelain = run_git(resolved, "worktree", "list", "--porcelain").stdout
listed = [
    line.removeprefix("worktree ")
    for line in porcelain.splitlines()
    if line.startswith("worktree ")
]
if resolved not in listed:
    raise RuntimeError("worktree is not present in Git's worktree registry")
is_linked = listed.index(resolved) != 0

remote_head = run_git(
    resolved,
    "symbolic-ref",
    "--quiet",
    "refs/remotes/origin/HEAD",
    check=False,
)
default_ref = remote_head.stdout.strip() if remote_head.returncode == 0 else None
if not default_ref:
    for candidate in (
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ):
        probe = run_git(
            resolved,
            "show-ref",
            "--verify",
            "--quiet",
            candidate,
            check=False,
        )
        if probe.returncode == 0:
            default_ref = candidate
            break
if not default_ref:
    raise RuntimeError("remote default branch is unavailable")
merged = run_git(
    resolved,
    "merge-base",
    "--is-ancestor",
    head,
    default_ref,
    check=False,
).returncode == 0

targets = []
for relative_root in ("apps/cli", "apps/desktop/src-tauri"):
    cargo_root = pathlib.Path(resolved, relative_root)
    if not cargo_root.is_dir():
        raise RuntimeError("required Cargo workspace root is missing: " + str(cargo_root))
    for candidate in sorted(cargo_root.iterdir()):
        if candidate.name != "target" and not candidate.name.startswith("target-"):
            continue
        candidate_lstat = candidate.lstat()
        if stat.S_ISLNK(candidate_lstat.st_mode):
            raise RuntimeError("Cargo target must not be a symlink: " + str(candidate))
        if not stat.S_ISDIR(candidate_lstat.st_mode):
            continue
        target_stat = candidate.stat()
        targets.append({
            "path": str(candidate),
            "device": target_stat.st_dev,
            "inode": target_stat.st_ino,
        })
if not targets:
    raise RuntimeError("worktree has no Cargo target directories to clean")

print(json.dumps({
    "worktree": {
        "path": resolved,
        "device": worktree_lstat.st_dev,
        "inode": worktree_lstat.st_ino,
        "head": head,
        "branch": branch,
    },
    "targets": targets,
    "dirty": dirty,
    "merged": merged,
    "isLinked": is_linked,
}))
"#;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetirementState {
    Reserved,
    Running,
    Succeeded,
    Failed,
    RecoveryRequired,
}

#[derive(Debug, PartialEq, Eq)]
enum ProviderCompletion {
    Succeeded,
    Failed(String),
    RecoveryRequired(String),
}

impl RetirementState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Reserved | Self::Running | Self::RecoveryRequired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRetirement {
    pub operation_id: String,
    pub worktree_path: String,
    pub request_path: String,
    pub state: RetirementState,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementReservation {
    pub operation_id: String,
    pub state: RetirementState,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedPathIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedWorktreeIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
    pub head: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementCleanupCapture {
    pub worktree: CapturedWorktreeIdentity,
    pub targets: Vec<CapturedPathIdentity>,
    pub dirty: bool,
    pub merged: bool,
    pub is_linked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetirementCleanupRequest {
    schema_version: u32,
    operation_id: String,
    project: &'static str,
    worktree: CapturedWorktreeIdentity,
    targets: Vec<CapturedPathIdentity>,
    allow_unmerged: bool,
    inventory_complete: bool,
}

impl From<&WorktreeRetirement> for RetirementReservation {
    fn from(record: &WorktreeRetirement) -> Self {
        Self {
            operation_id: record.operation_id.clone(),
            state: record.state,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRetirementSnapshot {
    schema_version: u32,
    retirements: BTreeMap<String, WorktreeRetirement>,
}

impl Default for WorktreeRetirementSnapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            retirements: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub enum WorktreeCoordinatorError {
    CorruptState(String),
    Io(String),
    Persistence(String),
    Conflict(String),
    UnknownOperation(String),
}

impl std::fmt::Display for WorktreeCoordinatorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CorruptState(reason) => {
                write!(formatter, "worktree retirement state is corrupt: {reason}")
            }
            Self::Io(reason) => write!(formatter, "worktree retirement I/O failed: {reason}"),
            Self::Persistence(reason) => {
                write!(
                    formatter,
                    "worktree retirement persistence failed: {reason}"
                )
            }
            Self::Conflict(reason) => write!(formatter, "{reason}"),
            Self::UnknownOperation(operation_id) => {
                write!(formatter, "unknown worktree retirement '{operation_id}'")
            }
        }
    }
}

impl std::error::Error for WorktreeCoordinatorError {}

#[derive(Debug)]
pub struct WorktreeCoordinator {
    path: Option<PathBuf>,
    inner: Mutex<WorktreeRetirementSnapshot>,
    workers: Mutex<BTreeSet<String>>,
}

impl WorktreeCoordinator {
    /// Load durable reservation state and fail closed if it cannot be decoded or
    /// validated.
    pub fn load(path: PathBuf) -> Result<Self, WorktreeCoordinatorError> {
        let snapshot = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<WorktreeRetirementSnapshot>(&bytes).map_err(|error| {
                    WorktreeCoordinatorError::CorruptState(format!("{}: {error}", path.display()))
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                WorktreeRetirementSnapshot::default()
            }
            Err(error) => {
                return Err(WorktreeCoordinatorError::Io(format!(
                    "{}: {error}",
                    path.display()
                )))
            }
        };
        validate_snapshot(&snapshot)?;
        Ok(Self {
            path: Some(path),
            inner: Mutex::new(snapshot),
            workers: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn load_default() -> Result<Self, WorktreeCoordinatorError> {
        Self::load(default_store_path())
    }

    pub fn ephemeral() -> Self {
        Self {
            path: None,
            inner: Mutex::new(WorktreeRetirementSnapshot::default()),
            workers: Mutex::new(BTreeSet::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WorktreeRetirementSnapshot> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn persist(
        &self,
        snapshot: &WorktreeRetirementSnapshot,
    ) -> Result<(), WorktreeCoordinatorError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_atomic(path, snapshot).map_err(|error| {
            WorktreeCoordinatorError::Persistence(format!("{}: {error}", path.display()))
        })
    }

    pub fn begin_retirement(
        &self,
        worktree_path: &str,
        request_path: &str,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        let worktree_path = normalize_path(worktree_path);
        if worktree_path.is_empty() {
            return Err(WorktreeCoordinatorError::Conflict(
                "cleanupWorktree requires a non-empty worktree path".into(),
            ));
        }
        if request_path.trim().is_empty() {
            return Err(WorktreeCoordinatorError::Conflict(
                "cleanupWorktree requires a durable provider request path".into(),
            ));
        }

        let mut snapshot = self.lock();
        if let Some(record) = matching_active_retirement(&snapshot, &worktree_path) {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{}' already has active retirement reservation '{}'",
                record.worktree_path, record.operation_id
            )));
        }
        let previous = snapshot.clone();
        let timestamp = now_ms();
        let record = WorktreeRetirement {
            operation_id: uuid::Uuid::new_v4().simple().to_string(),
            worktree_path,
            request_path: request_path.to_string(),
            state: RetirementState::Reserved,
            created_at: timestamp,
            updated_at: timestamp,
            last_error: None,
        };
        snapshot
            .retirements
            .insert(record.operation_id.clone(), record.clone());
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        Ok(record)
    }

    pub fn transition(
        &self,
        operation_id: &str,
        state: RetirementState,
        last_error: Option<String>,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        let mut snapshot = self.lock();
        let previous = snapshot.clone();
        let record = snapshot
            .retirements
            .get_mut(operation_id)
            .ok_or_else(|| WorktreeCoordinatorError::UnknownOperation(operation_id.to_string()))?;
        record.state = state;
        record.updated_at = now_ms();
        record.last_error = last_error;
        let updated = record.clone();
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        Ok(updated)
    }

    pub fn pending_retirements(&self) -> Vec<WorktreeRetirement> {
        self.lock()
            .retirements
            .values()
            .filter(|record| record.state.is_active())
            .cloned()
            .collect()
    }

    pub fn next_request_path(&self) -> PathBuf {
        let parent = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        parent
            .join("worktree-retirement-requests")
            .join(format!("{}.json", uuid::Uuid::new_v4().simple()))
    }

    pub fn write_provider_request(
        &self,
        record: &WorktreeRetirement,
        capture: RetirementCleanupCapture,
    ) -> Result<(), WorktreeCoordinatorError> {
        let request = RetirementCleanupRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: record.operation_id.clone(),
            project: "t-hub",
            worktree: capture.worktree,
            targets: capture.targets,
            allow_unmerged: false,
            inventory_complete: true,
        };
        write_json_atomic(Path::new(&record.request_path), &request).map_err(|error| {
            WorktreeCoordinatorError::Persistence(format!("{}: {error}", record.request_path))
        })
    }

    pub fn require_provider_configured(&self) -> Result<(), String> {
        configured_provider_command().map(|_| ())
    }

    pub fn start_provider_worker(
        self: &Arc<Self>,
        record: WorktreeRetirement,
    ) -> Result<bool, String> {
        {
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !workers.insert(record.operation_id.clone()) {
                return Ok(false);
            }
        }
        let coordinator = Arc::clone(self);
        let operation_id = record.operation_id.clone();
        let thread_name = format!(
            "t-hub-cargo-cleanup-{}",
            &operation_id[..operation_id.len().min(8)]
        );
        if let Err(error) = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                coordinator.run_provider_worker(record);
            })
        {
            self.release_worker(&operation_id);
            return Err(format!("could not start Cargo cleanup worker: {error}"));
        }
        Ok(true)
    }

    pub fn recover_pending(self: &Arc<Self>) {
        for record in self.pending_retirements() {
            if record.state == RetirementState::RecoveryRequired {
                continue;
            }
            if let Err(error) = self.start_provider_worker(record.clone()) {
                eprintln!(
                    "t-hub-cargo-cleanup: could not recover operation '{}': {error}",
                    record.operation_id
                );
            }
        }
    }

    fn run_provider_worker(self: &Arc<Self>, record: WorktreeRetirement) {
        let operation_id = record.operation_id.clone();
        let completion = (|| {
            if !Path::new(&record.request_path).is_file() {
                return ProviderCompletion::Failed(format!(
                    "durable provider request is missing: {}",
                    record.request_path
                ));
            }
            if let Err(error) = self.transition(&operation_id, RetirementState::Running, None) {
                return ProviderCompletion::RecoveryRequired(format!(
                    "could not persist the running provider state: {error}"
                ));
            }
            match run_provider(&record.request_path) {
                Ok(output) => classify_provider_output(&output),
                Err(error) => ProviderCompletion::RecoveryRequired(error),
            }
        })();
        let transition = match completion {
            ProviderCompletion::Succeeded => {
                self.transition(&operation_id, RetirementState::Succeeded, None)
            }
            ProviderCompletion::Failed(error) => self.transition(
                &operation_id,
                RetirementState::Failed,
                Some(bounded_last_error(&error)),
            ),
            ProviderCompletion::RecoveryRequired(error) => self.transition(
                &operation_id,
                RetirementState::RecoveryRequired,
                Some(bounded_last_error(&error)),
            ),
        };
        if let Err(error) = transition {
            eprintln!(
                "t-hub-cargo-cleanup: operation '{operation_id}' needs recovery because its terminal state could not be persisted: {error}"
            );
        }
        self.release_worker(&operation_id);
    }

    fn release_worker(&self, operation_id: &str) {
        self.workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(operation_id);
    }

    pub fn reservation_for(&self, worktree_path: &str) -> Option<RetirementReservation> {
        let worktree_path = normalize_path(worktree_path);
        self.lock()
            .retirements
            .values()
            .find(|record| {
                record.state.is_active() && normalize_path(&record.worktree_path) == worktree_path
            })
            .map(RetirementReservation::from)
    }

    pub fn ensure_available(&self, candidate_path: &str, operation: &str) -> Result<(), String> {
        let candidate_path = normalize_path(candidate_path);
        let snapshot = self.lock();
        let Some(record) = matching_active_retirement(&snapshot, &candidate_path) else {
            return Ok(());
        };
        Err(format!(
            "{operation}: worktree '{}' is reserved for Cargo cleanup by operation '{}'",
            record.worktree_path, record.operation_id
        ))
    }
}

fn classify_provider_output(output: &std::process::Output) -> ProviderCompletion {
    let exit = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "no exit code".into());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let report: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(report) => report,
        Err(error) => {
            return ProviderCompletion::RecoveryRequired(format!(
                "rust-storage retirement-clean exited with {exit} and returned invalid JSON: {error}; stderr: {}",
                stderr.trim()
            ));
        }
    };

    if output.status.success() {
        return if report.get("complete").and_then(serde_json::Value::as_bool) == Some(true) {
            ProviderCompletion::Succeeded
        } else {
            ProviderCompletion::RecoveryRequired(
                "rust-storage returned success without a complete report".into(),
            )
        };
    }

    let clean_refusal = report
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .filter(|actions| !actions.is_empty())
        .is_some_and(|actions| {
            actions.iter().all(|action| {
                action.get("status").and_then(serde_json::Value::as_str) == Some("refused")
                    && action
                        .get("recoveryState")
                        .and_then(serde_json::Value::as_str)
                        == Some("original")
                    && action
                        .get("quarantinePath")
                        .is_none_or(serde_json::Value::is_null)
            })
        });
    let error = format!(
        "rust-storage retirement-clean exited with {exit}: {}",
        stderr.trim()
    );
    if clean_refusal {
        ProviderCompletion::Failed(error)
    } else {
        ProviderCompletion::RecoveryRequired(error)
    }
}

fn bounded_last_error(error: &str) -> String {
    error.chars().take(LAST_ERROR_LIMIT).collect()
}

fn normalize_path(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    if normalized == "/" {
        normalized
    } else {
        normalized.trim_end_matches('/').to_string()
    }
}

fn path_within(candidate: &str, root: &str) -> bool {
    let candidate = normalize_path(candidate);
    let root = normalize_path(root);
    candidate == root || candidate.starts_with(&format!("{root}/"))
}

fn matching_active_retirement<'a>(
    snapshot: &'a WorktreeRetirementSnapshot,
    candidate_path: &str,
) -> Option<&'a WorktreeRetirement> {
    snapshot.retirements.values().find(|record| {
        record.state.is_active() && path_within(candidate_path, &record.worktree_path)
    })
}

fn validate_snapshot(
    snapshot: &WorktreeRetirementSnapshot,
) -> Result<(), WorktreeCoordinatorError> {
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(WorktreeCoordinatorError::CorruptState(format!(
            "unsupported schema version {}",
            snapshot.schema_version
        )));
    }
    for (operation_id, record) in &snapshot.retirements {
        if operation_id.is_empty() || operation_id != &record.operation_id {
            return Err(WorktreeCoordinatorError::CorruptState(
                "retirement map key does not match its operationId".into(),
            ));
        }
        if normalize_path(&record.worktree_path).is_empty() {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' has an empty worktree path"
            )));
        }
        if record.request_path.trim().is_empty() {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' has an empty provider request path"
            )));
        }
        if record.updated_at < record.created_at {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' was updated before it was created"
            )));
        }
    }
    Ok(())
}

pub fn inspect_cleanup_candidate(worktree_path: &str) -> Result<RetirementCleanupCapture, String> {
    let command = inspection_command(worktree_path);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        Duration::from_secs(30),
        INSPECTION_OUTPUT_LIMIT,
    )
    .map_err(|error| format!("Cargo cleanup inspection failed: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Cargo cleanup inspection refused: {}",
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Cargo cleanup inspection returned invalid JSON: {error}"))
}

#[cfg(not(windows))]
fn inspection_command(worktree_path: &str) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command.args(["-c", INSPECTION_SCRIPT, worktree_path]);
    command
}

#[cfg(windows)]
fn inspection_command(worktree_path: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args([
        "-d",
        &crate::files::host_distro(),
        "--cd",
        "~",
        "-e",
        "/usr/bin/python3",
        "-c",
        INSPECTION_SCRIPT,
        worktree_path,
    ]);
    command
}

fn configured_provider_command() -> Result<Vec<String>, String> {
    let configured = std::env::var("T_HUB_RUST_STORAGE_COMMAND")
        .map_err(|_| "T_HUB_RUST_STORAGE_COMMAND is not configured".to_string())?;
    let command = shell_words::split(&configured)
        .map_err(|error| format!("T_HUB_RUST_STORAGE_COMMAND is invalid: {error}"))?;
    if command.is_empty() {
        return Err("T_HUB_RUST_STORAGE_COMMAND must not be empty".into());
    }
    Ok(command)
}

fn provider_timeout() -> Duration {
    let seconds = std::env::var("T_HUB_RUST_STORAGE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (60..=21_600).contains(seconds))
        .unwrap_or(7_200);
    Duration::from_secs(seconds)
}

fn run_provider(request_path: &str) -> Result<std::process::Output, String> {
    let configured = configured_provider_command()?;
    let mut command = Command::new(&configured[0]);
    command.args(&configured[1..]);
    let request_path = provider_request_path(request_path)?;
    command.args([
        "retirement-clean",
        "--request",
        &request_path,
        "--apply",
        "--confirm",
        "--json",
    ]);
    crate::bounded_exec::output_with_timeout_and_limit(
        command,
        provider_timeout(),
        PROVIDER_OUTPUT_LIMIT,
    )
    .map_err(|error| format!("rust-storage retirement-clean could not complete: {error}"))
}

#[cfg(not(windows))]
fn provider_request_path(request_path: &str) -> Result<String, String> {
    Ok(request_path.to_string())
}

#[cfg(windows)]
fn provider_request_path(request_path: &str) -> Result<String, String> {
    let mut command = Command::new("wsl.exe");
    command.args([
        "-d",
        &crate::files::host_distro(),
        "--cd",
        "~",
        "-e",
        "wslpath",
        "-a",
        request_path,
    ]);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        4096,
    )
    .map_err(|error| format!("could not translate provider request path into WSL: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not translate provider request path into WSL: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !path.starts_with('/') {
        return Err("wslpath returned a non-absolute provider request path".into());
    }
    Ok(path)
}

fn default_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("T_HUB_WORKTREE_RETIREMENTS_FILE") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("worktree-retirements.json")
}

fn write_atomic(path: &Path, snapshot: &WorktreeRetirementSnapshot) -> std::io::Result<()> {
    write_json_atomic(path, snapshot)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(value)?;
    let temp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)?;
        #[cfg(unix)]
        {
            let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn reservation_is_durable_and_blocks_descendant_activity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("worktree-retirements.json");
        let operation_id = {
            let coordinator = WorktreeCoordinator::load(path.clone()).unwrap();
            let record = coordinator
                .begin_retirement("/repo/worktrees/clean", "/requests/one.json")
                .unwrap();
            coordinator
                .ensure_available("/repo/worktrees/clean/apps/cli", "spawn_terminal")
                .unwrap_err();
            record.operation_id
        };

        let coordinator = WorktreeCoordinator::load(path).unwrap();
        let reservation = coordinator
            .reservation_for("/repo/worktrees/clean")
            .unwrap();
        assert_eq!(reservation.operation_id, operation_id);
        assert_eq!(reservation.state, RetirementState::Reserved);
    }

    #[test]
    fn completed_reservation_no_longer_blocks_activity() {
        let coordinator = WorktreeCoordinator::ephemeral();
        let record = coordinator
            .begin_retirement("/repo/worktrees/clean", "/requests/one.json")
            .unwrap();
        coordinator
            .transition(&record.operation_id, RetirementState::Succeeded, None)
            .unwrap();

        assert!(coordinator
            .reservation_for("/repo/worktrees/clean")
            .is_none());
        coordinator
            .ensure_available("/repo/worktrees/clean/apps/cli", "start_agent")
            .unwrap();
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("worktree-retirements.json");
        std::fs::write(&path, br#"{"schemaVersion":99,"retirements":{}}"#).unwrap();

        assert!(matches!(
            WorktreeCoordinator::load(path),
            Err(WorktreeCoordinatorError::CorruptState(_))
        ));
    }

    #[test]
    fn inspection_captures_exact_linked_merged_cargo_targets() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        let worktree = directory.path().join("linked");
        std::fs::create_dir_all(repository.join("apps/cli")).unwrap();
        std::fs::create_dir_all(repository.join("apps/desktop/src-tauri")).unwrap();
        std::fs::write(repository.join(".gitignore"), b"target\ntarget-*\n").unwrap();
        std::fs::write(repository.join("apps/cli/Cargo.toml"), b"[workspace]\n").unwrap();
        std::fs::write(
            repository.join("apps/desktop/src-tauri/Cargo.toml"),
            b"[workspace]\n",
        )
        .unwrap();
        git(directory.path(), &["init", "-b", "main", "repository"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["config", "user.name", "Test User"]);
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "initial"]);
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        git(
            &repository,
            &[
                "update-ref",
                "refs/remotes/origin/main",
                "refs/heads/feature",
            ],
        );
        std::fs::create_dir_all(worktree.join("apps/cli/target")).unwrap();
        std::fs::create_dir_all(worktree.join("apps/desktop/src-tauri/target-windows")).unwrap();

        let capture = inspect_cleanup_candidate(worktree.to_str().unwrap()).unwrap();

        assert!(capture.is_linked);
        assert!(capture.merged);
        assert!(!capture.dirty);
        assert_eq!(capture.targets.len(), 2);
        assert_eq!(capture.worktree.path, worktree.to_str().unwrap());
        assert!(capture.worktree.inode > 0);
        assert!(capture.targets.iter().all(|target| target.inode > 0));
    }

    #[test]
    fn provider_request_has_the_exact_rust_storage_schema() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator =
            WorktreeCoordinator::load(directory.path().join("retirements.json")).unwrap();
        let request_path = directory.path().join("request.json");
        let record = coordinator
            .begin_retirement("/repo/worktree", request_path.to_str().unwrap())
            .unwrap();
        coordinator
            .write_provider_request(
                &record,
                RetirementCleanupCapture {
                    worktree: CapturedWorktreeIdentity {
                        path: "/repo/worktree".into(),
                        device: 7,
                        inode: 11,
                        head: "1234567890123456789012345678901234567890".into(),
                        branch: "feature".into(),
                    },
                    targets: vec![CapturedPathIdentity {
                        path: "/repo/worktree/apps/cli/target".into(),
                        device: 7,
                        inode: 12,
                    }],
                    dirty: false,
                    merged: true,
                    is_linked: true,
                },
            )
            .unwrap();
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(request_path).unwrap()).unwrap();

        assert_eq!(
            request
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            [
                "allowUnmerged",
                "inventoryComplete",
                "operationId",
                "project",
                "schemaVersion",
                "targets",
                "worktree",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        );
        assert_eq!(request["operationId"], record.operation_id);
        assert_eq!(request["project"], "t-hub");
        assert_eq!(request["allowUnmerged"], false);
        assert_eq!(request["inventoryComplete"], true);
    }

    fn provider_output(
        success: bool,
        code: i32,
        report: serde_json::Value,
    ) -> std::process::Output {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(if success { 0 } else { code << 8 })
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(if success { 0 } else { code as u32 })
        };
        std::process::Output {
            status,
            stdout: serde_json::to_vec(&report).unwrap(),
            stderr: b"provider detail".to_vec(),
        }
    }

    #[test]
    fn clean_provider_refusal_releases_the_reservation_as_failed() {
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "status": "refused",
                    "recoveryState": "original",
                    "quarantinePath": null,
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output),
            ProviderCompletion::Failed(_)
        ));
    }

    #[test]
    fn quarantined_provider_refusal_requires_recovery() {
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "status": "refused",
                    "recoveryState": "quarantined",
                    "quarantinePath": "/repo/worktree/apps/cli/.target-quarantine",
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }

    #[test]
    fn incomplete_success_requires_recovery() {
        let output = provider_output(
            true,
            0,
            serde_json::json!({
                "complete": false,
                "actions": [],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }
}
