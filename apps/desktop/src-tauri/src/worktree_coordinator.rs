//! Durable coordination for artifact cleanup reservations.
//!
//! A reservation is intentionally separate from Git worktree removal.
//! It blocks new activity in one exact linked worktree while an external storage
//! provider reclaims Cargo artifacts.
//! Completed records remain durable for recovery and audit, but only active
//! records participate in admission decisions.

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetirementCleanupRequest {
    schema_version: u32,
    operation_id: String,
    project: String,
    worktree: CapturedWorktreeIdentity,
    targets: Vec<CapturedPathIdentity>,
    allow_unmerged: bool,
    inventory_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CleanupCommitToken {
    operation_id: String,
    worktree_path: String,
    request_digest: String,
    prepare_token: String,
    consumed: bool,
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
    admissions: Mutex<BTreeMap<String, usize>>,
    workers: Mutex<BTreeSet<String>>,
    lease_boundary: Mutex<()>,
    commit_tokens: Mutex<BTreeMap<String, CleanupCommitToken>>,
}

pub struct WorktreeAdmissionGuard {
    coordinator: Arc<WorktreeCoordinator>,
    path: String,
}

impl WorktreeAdmissionGuard {
    pub fn canonical_path(&self) -> &str {
        &self.path
    }
}

impl Drop for WorktreeAdmissionGuard {
    fn drop(&mut self) {
        let mut admissions = self
            .coordinator
            .admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let remove = admissions.get_mut(&self.path).is_some_and(|count| {
            *count -= 1;
            *count == 0
        });
        if remove {
            admissions.remove(&self.path);
        }
    }
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
            admissions: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(BTreeSet::new()),
            lease_boundary: Mutex::new(()),
            commit_tokens: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn load_default() -> Result<Self, WorktreeCoordinatorError> {
        Self::load(default_store_path())
    }

    pub fn ephemeral() -> Self {
        Self {
            path: None,
            inner: Mutex::new(WorktreeRetirementSnapshot::default()),
            admissions: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(BTreeSet::new()),
            lease_boundary: Mutex::new(()),
            commit_tokens: Mutex::new(BTreeMap::new()),
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
        self.begin_retirement_if_idle(worktree_path, request_path, |_| Ok(Vec::new()))
    }

    pub fn begin_retirement_if_idle<F>(
        &self,
        worktree_path: &str,
        request_path: &str,
        inspect_activity: F,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError>
    where
        F: FnOnce(&str) -> Result<Vec<String>, String>,
    {
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

        let _lease_boundary = self
            .lease_boundary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admissions = self
            .admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut snapshot = self.lock();
        if let Some(record) = matching_active_retirement(&snapshot, &worktree_path) {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{}' already has active retirement reservation '{}'",
                record.worktree_path, record.operation_id
            )));
        }
        if admissions
            .keys()
            .any(|candidate_path| path_within(candidate_path, &worktree_path))
        {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{worktree_path}' has activity being admitted"
            )));
        }
        let live_activity =
            inspect_activity(&worktree_path).map_err(WorktreeCoordinatorError::Conflict)?;
        if !live_activity.is_empty() {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{worktree_path}' has live sessions: {}",
                live_activity.join(", ")
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
            project: "t-hub".into(),
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
        self.start_provider_worker_for_state(record, None)
    }

    fn start_provider_worker_for_state(
        self: &Arc<Self>,
        record: WorktreeRetirement,
        required_state: Option<RetirementState>,
    ) -> Result<bool, String> {
        {
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !workers.insert(record.operation_id.clone()) {
                return Ok(false);
            }
            let snapshot = self.lock();
            let Some(current) = snapshot
                .retirements
                .get(&record.operation_id)
                .filter(|current| **current == record)
            else {
                workers.remove(&record.operation_id);
                return Err("Cargo cleanup reservation changed before worker ownership".into());
            };
            if required_state.is_some_and(|state| current.state != state) {
                workers.remove(&record.operation_id);
                return Err("Cargo cleanup reservation is not recoverable".into());
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

    pub fn recovery_record(
        &self,
        operation_id: &str,
        worktree_path: &str,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        let worktree_path = normalize_path(worktree_path);
        let snapshot = self.lock();
        let record = snapshot
            .retirements
            .get(operation_id)
            .ok_or_else(|| WorktreeCoordinatorError::UnknownOperation(operation_id.to_string()))?;
        if record.state != RetirementState::RecoveryRequired
            || record.worktree_path != worktree_path
            || record.last_error.as_deref().is_none_or(str::is_empty)
        {
            return Err(WorktreeCoordinatorError::Conflict(
                "cleanup recovery requires one exact recoveryRequired reservation".into(),
            ));
        }
        Ok(record.clone())
    }

    pub fn validate_recovery_capture(
        &self,
        record: &WorktreeRetirement,
        capture: &RetirementCleanupCapture,
    ) -> Result<(), String> {
        if capture.dirty || !capture.merged || !capture.is_linked {
            return Err(
                "cleanup recovery requires a clean, merged, linked non-primary worktree".into(),
            );
        }
        let request = read_provider_request(record)?;
        if request.worktree != capture.worktree || request.targets != capture.targets {
            return Err(
                "cleanup recovery target or complete Cargo inventory changed since reservation"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn resume_recovery_worker(
        self: &Arc<Self>,
        record: WorktreeRetirement,
    ) -> Result<bool, String> {
        self.start_provider_worker_for_state(record, Some(RetirementState::RecoveryRequired))
    }

    pub fn recover_pending(self: &Arc<Self>) {
        for record in self.pending_retirements() {
            match record.state {
                RetirementState::Reserved => {
                    if let Err(error) = self.start_provider_worker(record.clone()) {
                        eprintln!(
                            "t-hub-cargo-cleanup: could not recover operation '{}': {error}",
                            record.operation_id
                        );
                    }
                }
                RetirementState::Running => {
                    if let Err(error) = self.transition(
                        &record.operation_id,
                        RetirementState::RecoveryRequired,
                        Some("Cargo cleanup was interrupted during its provider commit".into()),
                    ) {
                        eprintln!(
                            "t-hub-cargo-cleanup: could not preserve interrupted operation '{}': {error}",
                            record.operation_id
                        );
                    }
                }
                RetirementState::RecoveryRequired => {}
                RetirementState::Succeeded | RetirementState::Failed => unreachable!(),
            }
        }
    }

    fn run_provider_worker(self: &Arc<Self>, record: WorktreeRetirement) {
        let operation_id = record.operation_id.clone();
        let completion = (|| {
            if !Path::new(&record.request_path).is_file() {
                return missing_request_completion(&record);
            }
            let request = match read_provider_request(&record) {
                Ok(request) => request,
                Err(error) => return ProviderCompletion::RecoveryRequired(error),
            };
            if let Err(error) = self.transition(&operation_id, RetirementState::Running, None) {
                return ProviderCompletion::RecoveryRequired(format!(
                    "could not persist the running provider state: {error}"
                ));
            }
            match self.run_provider(&record, &request) {
                Ok(output) => classify_provider_output(&output, &request.targets),
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
        let worktree_path = crate::files::canonical_posix_path_allow_missing(worktree_path)
            .map(|path| normalize_path(&path))
            .unwrap_or_else(|_| normalize_path(worktree_path));
        self.lock()
            .retirements
            .values()
            .find(|record| {
                record.state.is_active() && normalize_path(&record.worktree_path) == worktree_path
            })
            .map(RetirementReservation::from)
    }

    pub fn admit_activity(
        self: &Arc<Self>,
        candidate_path: &str,
        operation: &str,
    ) -> Result<WorktreeAdmissionGuard, String> {
        let candidate_path = crate::files::canonical_posix_path_allow_missing(candidate_path)
            .map(|path| normalize_path(&path))
            .map_err(|error| {
                format!("{operation}: could not resolve worktree activity: {error}")
            })?;
        let _lease_boundary = self
            .lease_boundary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut admissions = self
            .admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = self.lock();
        if let Some(record) = matching_active_retirement(&snapshot, &candidate_path) {
            return Err(format!(
                "{operation}: worktree '{}' is reserved for Cargo cleanup by operation '{}'",
                record.worktree_path, record.operation_id
            ));
        }
        *admissions.entry(candidate_path.clone()).or_default() += 1;
        Ok(WorktreeAdmissionGuard {
            coordinator: Arc::clone(self),
            path: candidate_path,
        })
    }

    fn run_provider(
        &self,
        record: &WorktreeRetirement,
        request: &RetirementCleanupRequest,
    ) -> Result<std::process::Output, String> {
        let request_digest = provider_request_digest(request)?;
        let prepared = run_provider_prepare(&record.request_path)?;
        validate_prepared_report(&prepared, request, &request_digest)?;
        let prepare_token = prepared
            .get("prepareToken")
            .and_then(serde_json::Value::as_str)
            .filter(|token| {
                (16..=256).contains(&token.len())
                    && token
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
            .ok_or("rust-storage prepare report omitted its exact prepare token")?;
        let _lease_boundary = self
            .lease_boundary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let leased = crate::tmux::pane_info()
            .map_err(|error| format!("Cargo cleanup lease inspection failed: {error}"))?
            .into_iter()
            .any(|pane| path_within(&pane.cwd, &record.worktree_path));
        if leased {
            return Err(
                "Cargo cleanup commit refused because a live worktree session exists".into(),
            );
        }
        if self
            .admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .any(|candidate| path_within(candidate, &record.worktree_path))
        {
            return Err(
                "Cargo cleanup commit refused because worktree activity is admitted".into(),
            );
        }

        let commit_token = self.issue_commit_token(record, &request_digest, prepare_token)?;
        self.consume_commit_token(&commit_token, record, &request_digest, prepare_token)?;
        let result = run_provider_commit(
            &record.request_path,
            &record.worktree_path,
            prepare_token,
            &commit_token,
            &request_digest,
        );
        self.revoke_commit_token(&commit_token);
        let output = result?;
        validate_commit_report_binding(
            &output,
            &record.operation_id,
            prepare_token,
            &commit_token,
            &request_digest,
        )?;
        Ok(output)
    }

    fn issue_commit_token(
        &self,
        record: &WorktreeRetirement,
        request_digest: &str,
        prepare_token: &str,
    ) -> Result<String, String> {
        let mut tokens = self
            .commit_tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tokens
            .values()
            .any(|token| token.operation_id == record.operation_id)
        {
            return Err("Cargo cleanup commit token is duplicated".into());
        }
        let token_id = uuid::Uuid::new_v4().simple().to_string();
        tokens.insert(
            token_id.clone(),
            CleanupCommitToken {
                operation_id: record.operation_id.clone(),
                worktree_path: record.worktree_path.clone(),
                request_digest: request_digest.to_string(),
                prepare_token: prepare_token.to_string(),
                consumed: false,
            },
        );
        Ok(token_id)
    }

    fn consume_commit_token(
        &self,
        token_id: &str,
        record: &WorktreeRetirement,
        request_digest: &str,
        prepare_token: &str,
    ) -> Result<(), String> {
        let mut tokens = self
            .commit_tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let token = tokens
            .get_mut(token_id)
            .ok_or("Cargo cleanup commit token is missing or revoked")?;
        if token.consumed
            || token.operation_id != record.operation_id
            || token.worktree_path != record.worktree_path
            || token.request_digest != request_digest
            || token.prepare_token != prepare_token
        {
            return Err("Cargo cleanup commit token is stale, mismatched, or consumed".into());
        }
        token.consumed = true;
        Ok(())
    }

    fn revoke_commit_token(&self, token_id: &str) {
        self.commit_tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(token_id);
    }
}

fn missing_request_completion(record: &WorktreeRetirement) -> ProviderCompletion {
    ProviderCompletion::RecoveryRequired(format!(
        "durable provider request is missing: {}",
        record.request_path
    ))
}

fn read_provider_request(record: &WorktreeRetirement) -> Result<RetirementCleanupRequest, String> {
    let request: RetirementCleanupRequest = serde_json::from_slice(
        &std::fs::read(&record.request_path)
            .map_err(|error| format!("could not read durable provider request: {error}"))?,
    )
    .map_err(|error| format!("durable provider request is invalid: {error}"))?;
    if request.schema_version != SCHEMA_VERSION
        || request.operation_id != record.operation_id
        || request.project != "t-hub"
        || request.worktree.path != record.worktree_path
        || request.targets.is_empty()
        || request.allow_unmerged
        || !request.inventory_complete
    {
        return Err("durable provider request does not match its retirement reservation".into());
    }
    Ok(request)
}

fn classify_provider_output(
    output: &std::process::Output,
    expected_targets: &[CapturedPathIdentity],
) -> ProviderCompletion {
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
    let expected_inventory = expected_targets
        .iter()
        .map(path_identity)
        .collect::<BTreeSet<_>>();
    let reported_inventory = report
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .and_then(|actions| {
            actions
                .iter()
                .map(|action| {
                    let identity = action.get("target").unwrap_or(action);
                    serde_json::from_value::<CapturedPathIdentity>(identity.clone())
                        .ok()
                        .map(|identity| path_identity(&identity))
                })
                .collect::<Option<BTreeSet<_>>>()
        });
    if reported_inventory.as_ref() != Some(&expected_inventory) {
        return ProviderCompletion::RecoveryRequired(
            "rust-storage returned a report without the complete target inventory".into(),
        );
    }

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

fn path_identity(identity: &CapturedPathIdentity) -> (String, u64, u64) {
    (
        normalize_path(&identity.path),
        identity.device,
        identity.inode,
    )
}

fn bounded_last_error(error: &str) -> String {
    error.chars().take(LAST_ERROR_LIMIT).collect()
}

fn normalize_path(path: &str) -> String {
    let replaced = path.trim().replace('\\', "/");
    let mut lexical = PathBuf::new();
    for component in Path::new(&replaced).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            component => lexical.push(component.as_os_str()),
        }
    }
    let normalized = lexical.to_string_lossy().trim_end_matches('/').to_string();
    if normalized.is_empty() && replaced.starts_with('/') {
        "/".into()
    } else {
        normalized
    }
}

pub(crate) fn path_within(candidate: &str, root: &str) -> bool {
    let canonical = |path: &str| {
        crate::files::canonical_posix_path_allow_missing(path).map(|path| normalize_path(&path))
    };
    match (canonical(candidate), canonical(root)) {
        (Ok(candidate), Ok(root)) => {
            candidate == root || candidate.starts_with(&format!("{root}/"))
        }
        _ => true,
    }
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

fn provider_command(request_path: &str) -> Result<Command, String> {
    let configured = configured_provider_command()?;
    let mut command = Command::new(&configured[0]);
    command.args(&configured[1..]);
    let request_path = provider_request_path(request_path)?;
    command.args(["retirement-clean", "--request", &request_path]);
    Ok(command)
}

fn run_provider_prepare(request_path: &str) -> Result<serde_json::Value, String> {
    let mut command = provider_command(request_path)?;
    command.args(["--prepare", "--json"]);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        provider_timeout(),
        PROVIDER_OUTPUT_LIMIT,
    )
    .map_err(|error| format!("rust-storage retirement-clean prepare failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "rust-storage retirement-clean prepare refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("rust-storage prepare returned invalid JSON: {error}"))
}

fn validate_prepared_report(
    report: &serde_json::Value,
    request: &RetirementCleanupRequest,
    request_digest: &str,
) -> Result<(), String> {
    let prepared = report.get("prepared").and_then(serde_json::Value::as_bool) == Some(true);
    let operation_matches = report
        .get("operationId")
        .and_then(serde_json::Value::as_str)
        == Some(request.operation_id.as_str());
    let worktree_matches = report
        .get("worktree")
        .and_then(|worktree| {
            serde_json::from_value::<CapturedWorktreeIdentity>(worktree.clone()).ok()
        })
        .as_ref()
        == Some(&request.worktree);
    let targets = report.get("targets").and_then(|targets| {
        serde_json::from_value::<Vec<CapturedPathIdentity>>(targets.clone()).ok()
    });
    let digest_matches = report
        .get("requestDigest")
        .and_then(serde_json::Value::as_str)
        == Some(request_digest);
    if !prepared
        || !operation_matches
        || !worktree_matches
        || targets.as_ref() != Some(&request.targets)
        || !digest_matches
    {
        return Err(
            "rust-storage prepare report does not match the exact cleanup request and inventory"
                .into(),
        );
    }
    Ok(())
}

fn provider_request_digest(request: &RetirementCleanupRequest) -> Result<String, String> {
    serde_json::to_vec(request)
        .map(|bytes| format!("sha256:{:x}", sha2::Sha256::digest(bytes)))
        .map_err(|error| format!("Cargo cleanup request digest failed: {error}"))
}

fn run_provider_commit(
    request_path: &str,
    worktree_path: &str,
    prepare_token: &str,
    commit_token: &str,
    request_digest: &str,
) -> Result<std::process::Output, String> {
    let mut command = provider_command(request_path)?;
    command.args([
        "--commit",
        "--prepare-token",
        prepare_token,
        "--commit-token",
        commit_token,
        "--request-digest",
        request_digest,
        "--apply",
        "--confirm",
        "--json",
    ]);
    let inspect_leases = || {
        crate::tmux::pane_info()
            .map_err(|error| format!("Cargo cleanup lease inspection failed: {error}"))
            .map(|panes| {
                panes
                    .into_iter()
                    .any(|pane| path_within(&pane.cwd, worktree_path))
            })
    };
    run_provider_with_lease_inspector(command, provider_timeout(), inspect_leases)
}

fn run_provider_with_lease_inspector(
    mut command: Command,
    timeout: Duration,
    mut inspect_leases: impl FnMut() -> Result<bool, String>,
) -> Result<std::process::Output, String> {
    if inspect_leases()? {
        return Err("Cargo cleanup refused because a live worktree session is present".into());
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("rust-storage retirement-clean could not start: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("rust-storage retirement-clean stdout was unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("rust-storage retirement-clean stderr was unavailable")?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        let leased = match inspect_leases() {
            Ok(leased) => leased,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if leased {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cargo cleanup stopped because a live worktree session appeared".into());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("rust-storage retirement-clean wait failed: {error}"))?
        {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("rust-storage retirement-clean timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "rust-storage retirement-clean stdout reader failed")??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "rust-storage retirement-clean stderr reader failed")??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn validate_commit_report_binding(
    output: &std::process::Output,
    operation_id: &str,
    prepare_token: &str,
    commit_token: &str,
    request_digest: &str,
) -> Result<(), String> {
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("rust-storage commit returned invalid JSON: {error}"))?;
    if report
        .get("operationId")
        .and_then(serde_json::Value::as_str)
        != Some(operation_id)
        || report
            .get("prepareToken")
            .and_then(serde_json::Value::as_str)
            != Some(prepare_token)
        || report
            .get("commitToken")
            .and_then(serde_json::Value::as_str)
            != Some(commit_token)
        || report
            .get("requestDigest")
            .and_then(serde_json::Value::as_str)
            != Some(request_digest)
    {
        return Err(
            "rust-storage commit report is missing its exact operation and commit binding".into(),
        );
    }
    Ok(())
}

fn read_bounded(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .by_ref()
        .take((PROVIDER_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|error| format!("rust-storage retirement-clean output read failed: {error}"))?;
    if output.len() > PROVIDER_OUTPUT_LIMIT {
        return Err("rust-storage retirement-clean output exceeded the limit".into());
    }
    Ok(output)
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
            let coordinator = Arc::new(WorktreeCoordinator::load(path.clone()).unwrap());
            let record = coordinator
                .begin_retirement("/repo/worktrees/clean", "/requests/one.json")
                .unwrap();
            assert!(coordinator
                .admit_activity("/repo/worktrees/clean/apps/cli", "spawn_terminal")
                .is_err());
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
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
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
            .admit_activity("/repo/worktrees/clean/apps/cli", "start_agent")
            .unwrap();
    }

    #[test]
    fn admitted_activity_blocks_retirement_until_runtime_creation_finishes() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let admission = coordinator
            .admit_activity("/repo/worktrees/clean/apps/cli", "spawn_terminal")
            .unwrap();

        assert!(matches!(
            coordinator.begin_retirement("/repo/worktrees/clean", "/requests/while-admitted.json"),
            Err(WorktreeCoordinatorError::Conflict(_))
        ));

        drop(admission);
        coordinator
            .begin_retirement("/repo/worktrees/clean", "/requests/after-admission.json")
            .unwrap();
    }

    #[test]
    fn live_activity_check_blocks_retirement_before_reservation() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());

        let error = coordinator
            .begin_retirement_if_idle("/repo/worktrees/clean", "/requests/while-live.json", |_| {
                Ok(vec!["th_live".into()])
            })
            .unwrap_err();

        assert!(matches!(error, WorktreeCoordinatorError::Conflict(_)));
        assert!(coordinator
            .reservation_for("/repo/worktrees/clean")
            .is_none());
    }

    #[test]
    fn path_matching_collapses_repeated_separators() {
        assert!(path_within(
            "/repo//worktrees/clean/apps/cli",
            "/repo/worktrees/clean"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn path_matching_resolves_symlinked_worktrees() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let alias = directory.path().join("worktree-link");
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();

        assert!(path_within(
            alias.join("apps/cli").to_str().unwrap(),
            worktree.to_str().unwrap()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn admission_resolves_symlinks_before_parent_components() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        let nested = real.join("nested");
        let worktree = real.join("worktree");
        let alias = directory.path().join("alias");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(&nested, &alias).unwrap();
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        coordinator
            .begin_retirement(worktree.to_str().unwrap(), "/requests/one.json")
            .unwrap();

        let candidate = alias.join("../worktree");
        assert!(coordinator
            .admit_activity(candidate.to_str().unwrap(), "spawn_terminal")
            .is_err());
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
        let target = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "target": target.clone(),
                    "status": "refused",
                    "recoveryState": "original",
                    "quarantinePath": null,
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output, &[target]),
            ProviderCompletion::Failed(_)
        ));
    }

    #[test]
    fn missing_reserved_request_requires_recovery() {
        let record = WorktreeCoordinator::ephemeral()
            .begin_retirement("/repo/worktree", "/missing/request.json")
            .unwrap();
        assert!(matches!(
            missing_request_completion(&record),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }

    #[test]
    fn recovery_requires_exact_operation_target_request_and_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let request_path = directory.path().join("request.json");
        let coordinator =
            WorktreeCoordinator::load(directory.path().join("retirements.json")).unwrap();
        let record = coordinator
            .begin_retirement("/repo/worktree", request_path.to_str().unwrap())
            .unwrap();
        let capture = RetirementCleanupCapture {
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
        };
        coordinator
            .write_provider_request(&record, capture.clone())
            .unwrap();
        coordinator
            .transition(
                &record.operation_id,
                RetirementState::RecoveryRequired,
                Some("ambiguous provider result".into()),
            )
            .unwrap();

        assert!(coordinator
            .recovery_record(&record.operation_id, "/repo/other")
            .is_err());
        let recovered = coordinator
            .recovery_record(&record.operation_id, "/repo/worktree")
            .unwrap();
        coordinator
            .validate_recovery_capture(&recovered, &capture)
            .unwrap();
        let mut changed = capture;
        changed.targets[0].inode += 1;
        assert!(coordinator
            .validate_recovery_capture(&recovered, &changed)
            .is_err());

        let restarted =
            WorktreeCoordinator::load(directory.path().join("retirements.json")).unwrap();
        assert_eq!(
            restarted
                .recovery_record(&record.operation_id, "/repo/worktree")
                .unwrap()
                .state,
            RetirementState::RecoveryRequired
        );
    }

    #[test]
    fn restart_preserves_interrupted_commit_for_explicit_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("retirements.json");
        let operation_id = {
            let coordinator = WorktreeCoordinator::load(store.clone()).unwrap();
            let record = coordinator
                .begin_retirement("/repo/worktree", "/requests/one.json")
                .unwrap();
            coordinator
                .transition(&record.operation_id, RetirementState::Running, None)
                .unwrap();
            record.operation_id
        };

        let restarted = Arc::new(WorktreeCoordinator::load(store).unwrap());
        restarted.recover_pending();

        let recovered = restarted
            .recovery_record(&operation_id, "/repo/worktree")
            .unwrap();
        assert_eq!(recovered.state, RetirementState::RecoveryRequired);
        assert!(recovered
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("interrupted")));
    }

    #[cfg(unix)]
    #[test]
    fn late_lease_stops_provider_before_its_filesystem_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("provider-ready");
        let mutation = directory.path().join("provider-mutated");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!(
                "touch '{}'; sleep 30; touch '{}'",
                ready.display(),
                mutation.display()
            ),
        ]);
        let inspections = std::sync::atomic::AtomicUsize::new(0);
        let result = run_provider_with_lease_inspector(command, Duration::from_secs(5), || {
            let attempt = inspections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempt == 0 {
                return Ok(false);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !ready.exists() && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            Ok(ready.exists())
        });

        assert!(result
            .unwrap_err()
            .contains("live worktree session appeared"));
        assert!(!mutation.exists());
    }

    #[cfg(unix)]
    #[test]
    fn lease_before_commit_prevents_provider_spawn_and_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let mutation = directory.path().join("provider-mutated");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &format!("touch '{}'", mutation.display())]);

        let result =
            run_provider_with_lease_inspector(command, Duration::from_secs(5), || Ok(true));

        assert!(result
            .unwrap_err()
            .contains("live worktree session is present"));
        assert!(!mutation.exists());
    }

    #[test]
    fn commit_tokens_fail_closed_on_duplicate_mismatch_reuse_and_revocation() {
        let coordinator = WorktreeCoordinator::ephemeral();
        let record = coordinator
            .begin_retirement("/repo/worktree", "/requests/one.json")
            .unwrap();
        let token = coordinator
            .issue_commit_token(&record, "sha256:request", "prepare-1")
            .unwrap();
        assert!(coordinator
            .issue_commit_token(&record, "sha256:request", "prepare-1")
            .is_err());
        assert!(coordinator
            .consume_commit_token(&token, &record, "sha256:changed", "prepare-1")
            .is_err());
        coordinator
            .consume_commit_token(&token, &record, "sha256:request", "prepare-1")
            .unwrap();
        assert!(coordinator
            .consume_commit_token(&token, &record, "sha256:request", "prepare-1")
            .is_err());
        coordinator.revoke_commit_token(&token);
        assert!(coordinator
            .consume_commit_token(&token, &record, "sha256:request", "prepare-1")
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn committed_cleanup_boundary_rejects_late_symlink_activity() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let alias = directory.path().join("worktree-alias");
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        coordinator
            .begin_retirement(worktree.to_str().unwrap(), "/requests/one.json")
            .unwrap();

        let held = coordinator
            .lease_boundary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let attempt_coordinator = Arc::clone(&coordinator);
        let candidate = alias.join("apps/cli");
        let attempt = std::thread::spawn(move || {
            attempt_coordinator.admit_activity(candidate.to_str().unwrap(), "terminal_cwd")
        });
        std::thread::sleep(Duration::from_millis(20));
        assert!(!attempt.is_finished());
        drop(held);

        assert!(attempt.join().unwrap().is_err());
    }

    #[test]
    fn prepared_report_requires_exact_operation_target_and_inventory() {
        let request = RetirementCleanupRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: "operation-1".into(),
            project: "t-hub".into(),
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
            allow_unmerged: false,
            inventory_complete: true,
        };
        let report = serde_json::json!({
            "prepared": true,
            "prepareToken": "prepare-1",
            "operationId": request.operation_id.clone(),
            "worktree": request.worktree.clone(),
            "targets": request.targets.clone(),
            "requestDigest": "sha256:request",
        });
        validate_prepared_report(&report, &request, "sha256:request").unwrap();

        let mut mismatched = report;
        mismatched["operationId"] = serde_json::json!("operation-2");
        assert!(validate_prepared_report(&mismatched, &request, "sha256:request").is_err());
    }

    #[test]
    fn commit_report_requires_exact_operation_token_and_request_binding() {
        let output = provider_output(
            true,
            0,
            serde_json::json!({
                "operationId": "operation-1",
                "prepareToken": "prepare-token-1",
                "commitToken": "commit-token-1",
                "requestDigest": "sha256:request",
            }),
        );
        validate_commit_report_binding(
            &output,
            "operation-1",
            "prepare-token-1",
            "commit-token-1",
            "sha256:request",
        )
        .unwrap();
        assert!(validate_commit_report_binding(
            &output,
            "operation-1",
            "prepare-token-1",
            "commit-token-2",
            "sha256:request",
        )
        .is_err());
    }

    #[test]
    fn quarantined_provider_refusal_requires_recovery() {
        let target = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "target": target.clone(),
                    "status": "refused",
                    "recoveryState": "quarantined",
                    "quarantinePath": "/repo/worktree/apps/cli/.target-quarantine",
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output, &[target]),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }

    #[test]
    fn incomplete_provider_inventory_requires_recovery() {
        let first = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let second = CapturedPathIdentity {
            path: "/repo/worktree/apps/desktop/src-tauri/target".into(),
            device: 7,
            inode: 13,
        };
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "target": first.clone(),
                    "status": "refused",
                    "recoveryState": "original",
                    "quarantinePath": null,
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output, &[first, second]),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }
}
