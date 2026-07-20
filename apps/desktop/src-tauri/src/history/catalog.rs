use super::{
    bounded_text, codex_conversation_id_from_path, parse_claude_transcript, parse_codex_rollout,
    ActionCompatibility, ActionStatus, ContinuityState, Harness, HistoryActions, HistoryEntry,
    HISTORY_REASON_MAX_CHARS,
};
use chrono::{SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

const HISTORY_SCHEMA_VERSION: u32 = 1;
const DEFAULT_RESULT_LIMIT: usize = 100;
pub const HISTORY_ENTRY_LIMIT: usize = 500;
pub const HISTORY_SOURCE_LIMIT: usize = 32;
const MAX_FILES_PER_SOURCE: usize = 4_096;
const MAX_FILES_PER_GROUP: usize = 128;
const MAX_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_CANDIDATES_PER_DIRECTORY: usize = 256;
const MAX_DISCOVERY_CANDIDATES: usize = HISTORY_SOURCE_LIMIT * MAX_CANDIDATES_PER_DIRECTORY;
const TRANSCRIPT_WINDOW_BYTES: usize = 128 * 1024;
const TRANSCRIPT_GROUP_PREFIX_BYTES: usize = 4 * 1024;
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(15);
const MAX_DURABLE_RESUME_OPERATIONS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFilter {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub harness: Option<Harness>,
    #[serde(default = "include_archived_default")]
    pub include_archived: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn include_archived_default() -> bool {
    true
}

impl Default for HistoryFilter {
    fn default() -> Self {
        Self {
            query: None,
            harness: None,
            include_archived: true,
            limit: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationLiveness {
    Active,
    Inactive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryAssociation {
    pub harness: Harness,
    pub conversation_id: String,
    pub terminal_id: Option<String>,
    pub liveness: AssociationLiveness,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub captain_id: Option<String>,
    pub assignment_id: Option<String>,
    pub role: Option<String>,
    pub workspace_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryBinding {
    pub history_id: String,
    pub harness: Harness,
    pub conversation_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryResumeOperation {
    pub request_id: String,
    pub history_id: String,
    pub harness: Harness,
    pub conversation_id: String,
    pub terminal_id: String,
    pub target_tab_id: Option<String>,
    #[serde(default)]
    pub actual_tab_id: Option<String>,
    #[serde(default)]
    pub authorized_ship_slug: Option<String>,
    #[serde(default)]
    pub authorized_project_id: Option<String>,
    #[serde(default)]
    pub authorized_assignment_id: Option<String>,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryPendingResume {
    pub request_id: String,
    pub history_id: String,
    pub harness: Harness,
    pub conversation_id: String,
    pub terminal_id: String,
    pub target_tab_id: Option<String>,
    #[serde(default)]
    pub authorized_ship_slug: Option<String>,
    #[serde(default)]
    pub authorized_project_id: Option<String>,
    #[serde(default)]
    pub authorized_assignment_id: Option<String>,
    pub reserved_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryDurableState {
    schema_version: u32,
    #[serde(default)]
    bindings: BTreeMap<String, HistoryBinding>,
    #[serde(default)]
    resume_operations: BTreeMap<String, HistoryResumeOperation>,
    #[serde(default)]
    pending_resumes: BTreeMap<String, HistoryPendingResume>,
}

impl Default for HistoryDurableState {
    fn default() -> Self {
        Self {
            schema_version: HISTORY_SCHEMA_VERSION,
            bindings: BTreeMap::new(),
            resume_operations: BTreeMap::new(),
            pending_resumes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HistorySourceStatus {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySource {
    pub harness: Harness,
    pub status: HistorySourceStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryList {
    pub schema_version: u32,
    pub generated_at: String,
    pub revision: String,
    pub entries: Vec<HistoryEntry>,
    pub count: usize,
    pub total: usize,
    pub truncated: bool,
    pub sources: Vec<HistorySource>,
}

#[derive(Debug, Clone)]
struct CachedCatalog {
    refreshed_at: Instant,
    entries: Vec<HistoryEntry>,
    sources: Vec<HistorySource>,
}

#[derive(Debug)]
pub struct HistoryService {
    claude_root: PathBuf,
    codex_root: PathBuf,
    cache_ttl: Duration,
    cache: Mutex<Option<CachedCatalog>>,
    state_path: Option<PathBuf>,
    state_error: Option<String>,
    state: Mutex<HistoryDurableState>,
}

impl HistoryService {
    pub fn from_env() -> Arc<Self> {
        let fallback_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let provider_home = crate::files::user_home_path()
            .ok()
            .map(|home| crate::files::to_host_path(&home))
            .unwrap_or(fallback_home);
        let claude_root = std::env::var_os("T_HUB_HISTORY_CLAUDE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| provider_home.join(".claude").join("projects"));
        let codex_root = std::env::var_os("T_HUB_HISTORY_CODEX_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| provider_home.join(".codex").join("sessions"));
        let state_path = std::env::var_os("T_HUB_HISTORY_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| provider_home.join(".t-hub").join("history.json"));
        Arc::new(Self::new_persistent(
            claude_root,
            codex_root,
            DEFAULT_CACHE_TTL,
            state_path,
        ))
    }

    pub fn new(claude_root: PathBuf, codex_root: PathBuf, cache_ttl: Duration) -> Self {
        Self {
            claude_root,
            codex_root,
            cache_ttl,
            cache: Mutex::new(None),
            state_path: None,
            state_error: None,
            state: Mutex::new(HistoryDurableState::default()),
        }
    }

    fn new_persistent(
        claude_root: PathBuf,
        codex_root: PathBuf,
        cache_ttl: Duration,
        state_path: PathBuf,
    ) -> Self {
        let (state, state_error) = load_durable_state(&state_path);
        Self {
            claude_root,
            codex_root,
            cache_ttl,
            cache: Mutex::new(None),
            state_path: Some(state_path),
            state_error,
            state: Mutex::new(state),
        }
    }

    pub fn invalidate(&self) {
        *self.cache.lock() = None;
    }

    pub fn durable_state_error(&self) -> Option<String> {
        self.state_error.clone()
    }

    pub fn bindings(&self) -> Vec<HistoryBinding> {
        self.state.lock().bindings.values().cloned().collect()
    }

    pub fn resume_operations(&self) -> Vec<HistoryResumeOperation> {
        self.state
            .lock()
            .resume_operations
            .values()
            .cloned()
            .collect()
    }

    pub fn resume_operation(
        &self,
        request_id: &str,
    ) -> Result<Option<HistoryResumeOperation>, String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "history_recovery_required: durable History state is unavailable: {error}"
            ));
        }
        Ok(self.state.lock().resume_operations.get(request_id).cloned())
    }

    pub fn pending_resume(&self, request_id: &str) -> Result<Option<HistoryPendingResume>, String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "history_recovery_required: durable History state is unavailable: {error}"
            ));
        }
        Ok(self.state.lock().pending_resumes.get(request_id).cloned())
    }

    /// Snapshot every durable provider-resume intent for capacity accounting.
    /// The returned records are clones so the History lock is never held while
    /// the caller inspects tmux or provider liveness.
    pub fn pending_resumes(&self) -> Result<Vec<HistoryPendingResume>, String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "history_recovery_required: durable History state is unavailable: {error}"
            ));
        }
        Ok(self
            .state
            .lock()
            .pending_resumes
            .values()
            .cloned()
            .collect())
    }

    pub fn reserve_resume(&self, pending: HistoryPendingResume) -> Result<(), String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "history_recovery_required: durable History state is unavailable: {error}"
            ));
        }
        if pending.request_id.trim().is_empty()
            || pending.history_id.trim().is_empty()
            || pending.conversation_id.trim().is_empty()
            || !valid_terminal_id(&pending.terminal_id)
            || !durable_owner_valid(
                pending.authorized_ship_slug.as_deref(),
                pending.authorized_project_id.as_deref(),
                pending.authorized_assignment_id.as_deref(),
            )
        {
            return Err(
                "history_invalid_reservation: durable resume identity is incomplete".into(),
            );
        }
        let mut state = self.state.lock();
        if state.resume_operations.contains_key(&pending.request_id) {
            return Err(
                "request_conflict: requestId is already bound to a completed History resume".into(),
            );
        }
        if let Some(existing) = state.pending_resumes.get(&pending.request_id) {
            return if existing == &pending {
                Err(
                    "history_resume_in_flight: this request already has a durable resume reservation"
                        .into(),
                )
            } else {
                Err(
                    "request_conflict: requestId is already bound to a different History resume"
                        .into(),
                )
            };
        }
        if state
            .pending_resumes
            .values()
            .any(|existing| existing.history_id == pending.history_id)
        {
            return Err(
                "history_resume_in_flight: this conversation already has a durable resume reservation"
                    .into(),
            );
        }
        if state.resume_operations.len() + state.pending_resumes.len()
            >= MAX_DURABLE_RESUME_OPERATIONS
        {
            return Err(format!(
                "history_capacity: durable resume ledger reached its limit of {MAX_DURABLE_RESUME_OPERATIONS}; compact it before resuming another conversation"
            ));
        }
        let previous = state.clone();
        state
            .pending_resumes
            .insert(pending.request_id.clone(), pending);
        if let Some(path) = &self.state_path {
            if let Err(error) = persist_durable_state(path, &state) {
                *state = previous;
                return Err(format!(
                    "history_persistence_failed: could not reserve resume identity: {error}"
                ));
            }
        }
        Ok(())
    }

    pub fn cancel_resume_reservation(&self, pending: &HistoryPendingResume) -> Result<(), String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "history_recovery_required: durable History state is unavailable: {error}"
            ));
        }
        let mut state = self.state.lock();
        let Some(existing) = state.pending_resumes.get(&pending.request_id) else {
            return Ok(());
        };
        if existing != pending {
            return Err(
                "request_conflict: durable History reservation changed before cancellation".into(),
            );
        }
        let previous = state.clone();
        state.pending_resumes.remove(&pending.request_id);
        if let Some(path) = &self.state_path {
            if let Err(error) = persist_durable_state(path, &state) {
                *state = previous;
                return Err(format!(
                    "history_persistence_failed: could not cancel resume reservation: {error}"
                ));
            }
        }
        Ok(())
    }

    pub fn record_resume(
        &self,
        binding: HistoryBinding,
        operation: HistoryResumeOperation,
    ) -> Result<(), String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "history_recovery_required: durable History state is unavailable: {error}"
            ));
        }
        if operation.request_id.trim().is_empty()
            || operation.history_id.trim().is_empty()
            || operation.conversation_id.trim().is_empty()
            || !valid_terminal_id(&operation.terminal_id)
            || !durable_owner_valid(
                operation.authorized_ship_slug.as_deref(),
                operation.authorized_project_id.as_deref(),
                operation.authorized_assignment_id.as_deref(),
            )
        {
            return Err("history_invalid_binding: durable resume identity is incomplete".into());
        }
        let mut state = self.state.lock();
        if binding.history_id != operation.history_id
            || binding.harness != operation.harness
            || binding.conversation_id != operation.conversation_id
            || binding.terminal_id != operation.terminal_id
        {
            return Err(
                "history_invalid_binding: resume operation does not match its History binding"
                    .into(),
            );
        }
        if let Some(existing) = state.resume_operations.get(&operation.request_id) {
            return if existing == &operation {
                Ok(())
            } else {
                Err(
                    "request_conflict: requestId is already bound to a different History resume"
                        .into(),
                )
            };
        }
        let pending = state
            .pending_resumes
            .get(&operation.request_id)
            .ok_or_else(|| {
                "history_resume_unreserved: resume completion has no durable pre-spawn reservation"
                    .to_string()
            })?;
        if pending.history_id != operation.history_id
            || pending.harness != operation.harness
            || pending.conversation_id != operation.conversation_id
            || pending.terminal_id != operation.terminal_id
            || pending.target_tab_id != operation.target_tab_id
            || pending.authorized_ship_slug != operation.authorized_ship_slug
            || pending.authorized_project_id != operation.authorized_project_id
            || pending.authorized_assignment_id != operation.authorized_assignment_id
        {
            return Err(
                "history_invalid_binding: resume completion does not match its durable reservation"
                    .into(),
            );
        }
        let previous = state.clone();
        state.pending_resumes.remove(&operation.request_id);
        state.bindings.insert(binding.history_id.clone(), binding);
        state
            .resume_operations
            .insert(operation.request_id.clone(), operation);
        if let Some(path) = &self.state_path {
            if let Err(error) = persist_durable_state(path, &state) {
                *state = previous;
                return Err(format!(
                    "history_persistence_failed: could not persist resume identity: {error}"
                ));
            }
        }
        Ok(())
    }

    pub fn list(
        &self,
        filter: &HistoryFilter,
        associations: &[HistoryAssociation],
    ) -> Result<HistoryList, String> {
        self.list_scoped(filter, associations, None, None, None)
    }

    pub fn list_for_captain(
        &self,
        filter: &HistoryFilter,
        associations: &[HistoryAssociation],
        captain_id: &str,
    ) -> Result<HistoryList, String> {
        self.list_scoped(filter, associations, Some(captain_id), None, None)
    }

    pub fn list_for_assignment(
        &self,
        filter: &HistoryFilter,
        associations: &[HistoryAssociation],
        captain_id: &str,
        project_id: &str,
        assignment_id: &str,
    ) -> Result<HistoryList, String> {
        self.list_scoped(
            filter,
            associations,
            Some(captain_id),
            Some(project_id),
            Some(assignment_id),
        )
    }

    fn list_scoped(
        &self,
        filter: &HistoryFilter,
        associations: &[HistoryAssociation],
        captain_id: Option<&str>,
        project_id: Option<&str>,
        assignment_id: Option<&str>,
    ) -> Result<HistoryList, String> {
        validate_filter(filter)?;
        let catalog = self.catalog()?;
        let scoped_associations = associations
            .iter()
            .filter(|association| {
                captain_id
                    .is_none_or(|captain_id| association.captain_id.as_deref() == Some(captain_id))
                    && project_id.is_none_or(|project_id| {
                        association.project_id.as_deref() == Some(project_id)
                    })
                    && assignment_id.is_none_or(|assignment_id| {
                        association.assignment_id.as_deref() == Some(assignment_id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut entries = apply_associations(catalog.entries.clone(), &scoped_associations);
        entries.retain(|entry| {
            captain_id.is_none_or(|captain_id| entry.captain_id.as_deref() == Some(captain_id))
                && project_id
                    .is_none_or(|project_id| entry.project_id.as_deref() == Some(project_id))
                && filter
                    .harness
                    .is_none_or(|harness| harness == entry.harness)
                && (filter.include_archived || entry.continuity_state != ContinuityState::Archived)
                && filter.query.as_ref().is_none_or(|query| {
                    let query = query.trim().to_lowercase();
                    query.is_empty()
                        || entry.label.to_lowercase().contains(&query)
                        || entry.cwd.to_lowercase().contains(&query)
                        || entry
                            .last_text
                            .as_deref()
                            .is_some_and(|text| text.to_lowercase().contains(&query))
                })
        });
        let total = entries.len();
        let limit = filter.limit.unwrap_or(DEFAULT_RESULT_LIMIT);
        entries = fair_harnesses(entries, limit);
        entries.sort_by(history_order);
        let sources = catalog.sources.clone();
        let truncated = total > entries.len()
            || sources
                .iter()
                .any(|source| source.status != HistorySourceStatus::Ready);
        let revision = revision_for(&entries, &sources, total, truncated)?;
        Ok(HistoryList {
            schema_version: HISTORY_SCHEMA_VERSION,
            generated_at: now_rfc3339(),
            revision,
            count: entries.len(),
            total,
            truncated,
            entries,
            sources,
        })
    }

    pub fn find(
        &self,
        history_id: &str,
        associations: &[HistoryAssociation],
    ) -> Result<Option<HistoryEntry>, String> {
        let catalog = self.catalog()?;
        Ok(apply_associations(catalog.entries.clone(), associations)
            .into_iter()
            .find(|entry| entry.history_id == history_id))
    }

    fn catalog(&self) -> Result<CachedCatalog, String> {
        let mut cache = self.cache.lock();
        if let Some(cached) = cache.as_ref() {
            if cached.refreshed_at.elapsed() < self.cache_ttl {
                return Ok(cached.clone());
            }
        }
        let refreshed = scan_catalog(&self.claude_root, &self.codex_root);
        *cache = Some(refreshed.clone());
        Ok(refreshed)
    }
}

fn load_durable_state(path: &Path) -> (HistoryDurableState, Option<String>) {
    if !path.exists() {
        return (HistoryDurableState::default(), None);
    }
    let loaded = fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|body| {
            serde_json::from_slice::<HistoryDurableState>(&body).map_err(|error| error.to_string())
        })
        .and_then(|state| {
            if state.schema_version != HISTORY_SCHEMA_VERSION {
                return Err(format!(
                    "unsupported schemaVersion {}",
                    state.schema_version
                ));
            }
            let bindings_valid = state.bindings.iter().all(|(history_id, binding)| {
                history_id == &binding.history_id
                    && !binding.conversation_id.trim().is_empty()
                    && valid_terminal_id(&binding.terminal_id)
                    && state.resume_operations.values().any(|operation| {
                        operation.history_id == binding.history_id
                            && operation.harness == binding.harness
                            && operation.conversation_id == binding.conversation_id
                            && operation.terminal_id == binding.terminal_id
                    })
            });
            let operations_valid = state
                .resume_operations
                .iter()
                .all(|(request_id, operation)| {
                    request_id == &operation.request_id
                        && !operation.history_id.trim().is_empty()
                        && !operation.conversation_id.trim().is_empty()
                        && valid_terminal_id(&operation.terminal_id)
                        && durable_owner_valid(
                            operation.authorized_ship_slug.as_deref(),
                            operation.authorized_project_id.as_deref(),
                            operation.authorized_assignment_id.as_deref(),
                        )
                });
            let pending_valid = state.pending_resumes.iter().all(|(request_id, pending)| {
                request_id == &pending.request_id
                    && !pending.history_id.trim().is_empty()
                    && !pending.conversation_id.trim().is_empty()
                    && valid_terminal_id(&pending.terminal_id)
                    && durable_owner_valid(
                        pending.authorized_ship_slug.as_deref(),
                        pending.authorized_project_id.as_deref(),
                        pending.authorized_assignment_id.as_deref(),
                    )
            });
            let pending_histories_unique = state
                .pending_resumes
                .values()
                .map(|pending| pending.history_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == state.pending_resumes.len();
            let request_sets_disjoint = state
                .pending_resumes
                .keys()
                .all(|request_id| !state.resume_operations.contains_key(request_id));
            if !bindings_valid
                || !operations_valid
                || !pending_valid
                || !pending_histories_unique
                || !request_sets_disjoint
            {
                return Err("durable History state contains an invalid identity binding".into());
            }
            Ok(state)
        });
    match loaded {
        Ok(state) => (state, None),
        Err(error) => {
            eprintln!(
                "t-hub-history: preserving unreadable durable state at '{}': {error}",
                path.display()
            );
            (HistoryDurableState::default(), Some(error))
        }
    }
}

fn durable_owner_valid(
    ship_slug: Option<&str>,
    project_id: Option<&str>,
    assignment_id: Option<&str>,
) -> bool {
    match (ship_slug, project_id, assignment_id) {
        (None, None, None) => true,
        (Some(ship), Some(project), Some(assignment)) => {
            !ship.trim().is_empty() && !project.trim().is_empty() && !assignment.trim().is_empty()
        }
        _ => false,
    }
}

fn valid_terminal_id(terminal_id: &str) -> bool {
    terminal_id.len() == 8 && terminal_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn persist_durable_state(path: &Path, state: &HistoryDurableState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(state)?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn validate_filter(filter: &HistoryFilter) -> Result<(), String> {
    if filter
        .limit
        .is_some_and(|limit| limit == 0 || limit > HISTORY_ENTRY_LIMIT)
    {
        return Err(format!(
            "history_invalid_filter: limit must be between 1 and {HISTORY_ENTRY_LIMIT}"
        ));
    }
    Ok(())
}

fn scan_catalog(claude_root: &Path, codex_root: &Path) -> CachedCatalog {
    let (mut entries, mut sources) = (Vec::new(), Vec::new());
    for (harness, root) in [(Harness::Claude, claude_root), (Harness::Codex, codex_root)] {
        match scan_source(root, harness) {
            Ok((source_entries, reason)) => {
                entries.extend(source_entries);
                sources.push(HistorySource {
                    harness,
                    status: if reason.is_some() {
                        HistorySourceStatus::Degraded
                    } else {
                        HistorySourceStatus::Ready
                    },
                    reason,
                });
            }
            Err(error) => sources.push(HistorySource {
                harness,
                status: HistorySourceStatus::Unavailable,
                reason: Some(bounded_text(&error, HISTORY_REASON_MAX_CHARS)),
            }),
        }
    }
    sources.sort_by_key(|source| source.harness);
    CachedCatalog {
        refreshed_at: Instant::now(),
        entries,
        sources,
    }
}

#[derive(Debug)]
struct TranscriptCandidate {
    path: PathBuf,
    modified_epoch: i64,
    archived: bool,
}

#[derive(Debug)]
struct PendingDirectory {
    path: PathBuf,
    modified_epoch: i64,
    archived: bool,
}

fn scan_source(
    root: &Path,
    harness: Harness,
) -> Result<(Vec<HistoryEntry>, Option<String>), String> {
    if !root.is_dir() {
        return Err(format!(
            "{} History root is unavailable",
            harness.canonical()
        ));
    }
    let mut roots = vec![(root.to_path_buf(), false)];
    if harness == Harness::Claude {
        if let Some(parent) = root.parent() {
            let archived = parent.join("projects-archive");
            if archived.is_dir() {
                roots.push((archived, true));
            }
        }
    }
    let (candidates, mut scan_truncated) = collect_transcripts(&roots, harness)?;
    let candidate_count = candidates.len();
    let mut candidates = fair_transcript_candidates(candidates, harness, MAX_FILES_PER_SOURCE);
    scan_truncated |= candidate_count > candidates.len();
    candidates.sort_by(|left, right| {
        right
            .modified_epoch
            .cmp(&left.modified_epoch)
            .then_with(|| left.path.cmp(&right.path))
    });

    let mut groups = BTreeMap::<String, VecDeque<HistoryEntry>>::new();
    let mut unreadable = 0usize;
    let mut degraded = 0usize;
    let mut duplicates = 0usize;
    let mut identities = BTreeMap::<String, HistoryEntry>::new();
    for candidate in candidates {
        let transcript = match read_bounded(&candidate.path) {
            Ok(transcript) => transcript,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let parsed = match harness {
            Harness::Claude => parse_claude_transcript(
                &candidate.path,
                &transcript,
                candidate.modified_epoch,
                candidate.archived,
            ),
            Harness::Codex => {
                parse_codex_rollout(&candidate.path, &transcript, candidate.modified_epoch)
            }
        };
        let Ok(mut parsed) = parsed else {
            unreadable += 1;
            continue;
        };
        degraded += usize::from(parsed.degraded_reason.is_some());
        if let Some(existing) = identities.get_mut(&parsed.entry.history_id) {
            duplicates += 1;
            mark_recovery_required(existing, "Conflicting duplicate provider evidence.");
            continue;
        }
        set_resumable_actions(&mut parsed.entry);
        identities.insert(parsed.entry.history_id.clone(), parsed.entry);
    }
    let mut group_truncated = 0usize;
    for entry in identities.into_values() {
        groups
            .entry(entry.cwd.clone())
            .or_default()
            .push_back(entry);
    }
    for group in groups.values_mut() {
        group.make_contiguous().sort_by(history_order);
        if group.len() > MAX_FILES_PER_GROUP {
            group_truncated += group.len() - MAX_FILES_PER_GROUP;
            group.truncate(MAX_FILES_PER_GROUP);
        }
    }
    let entries = fair_groups(groups);
    let reason = (unreadable > 0
        || degraded > 0
        || duplicates > 0
        || group_truncated > 0
        || scan_truncated)
    .then(|| {
        bounded_text(
            &format!(
                "{} History skipped {unreadable} unreadable or unsupported transcript(s), retained {degraded} degraded transcript(s), found {duplicates} duplicate identity record(s), dropped {group_truncated} overrepresented Project transcript(s), and scanTruncated={scan_truncated}.",
                harness.canonical()
            ),
            HISTORY_REASON_MAX_CHARS,
        )
    });
    Ok((entries, reason))
}

fn fair_transcript_candidates(
    candidates: Vec<TranscriptCandidate>,
    harness: Harness,
    max: usize,
) -> Vec<TranscriptCandidate> {
    let mut grouped = BTreeMap::<String, Vec<TranscriptCandidate>>::new();
    for candidate in candidates {
        let key = transcript_project_key(&candidate.path, harness)
            .map(|cwd| format!("known:{cwd}"))
            .unwrap_or_else(|| "unknown".to_string());
        grouped.entry(key).or_default().push(candidate);
    }
    let mut groups = grouped
        .into_iter()
        .map(|(key, mut candidates)| {
            candidates.sort_by(|left, right| {
                right
                    .modified_epoch
                    .cmp(&left.modified_epoch)
                    .then_with(|| left.path.cmp(&right.path))
            });
            (key, VecDeque::from(candidates))
        })
        .collect::<Vec<_>>();
    groups.sort_by(|(left_key, left), (right_key, right)| {
        right
            .front()
            .map(|candidate| candidate.modified_epoch)
            .cmp(&left.front().map(|candidate| candidate.modified_epoch))
            .then_with(|| left_key.cmp(right_key))
    });
    let mut selected = Vec::new();
    while selected.len() < max {
        let mut advanced = false;
        for (_, group) in &mut groups {
            if let Some(candidate) = group.pop_front() {
                selected.push(candidate);
                advanced = true;
                if selected.len() == max {
                    break;
                }
            }
        }
        if !advanced {
            break;
        }
    }
    selected
}

fn transcript_project_key(path: &Path, harness: Harness) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut bytes = vec![0; TRANSCRIPT_GROUP_PREFIX_BYTES];
    let read = file.read(&mut bytes).ok()?;
    bytes.truncate(read);
    let prefix = String::from_utf8_lossy(&bytes);
    let expected_codex_id = (harness == Harness::Codex)
        .then(|| codex_conversation_id_from_path(path).ok())
        .flatten();
    for line in prefix.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if harness == Harness::Codex {
            if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
                continue;
            }
            let payload = value.get("payload")?;
            if payload.get("id").and_then(serde_json::Value::as_str) != expected_codex_id.as_deref()
            {
                continue;
            }
            return payload
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .map(str::to_string);
        }
        if let Some(cwd) = value
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|cwd| !cwd.is_empty())
        {
            return Some(cwd.to_string());
        }
    }
    None
}

fn collect_transcripts(
    roots: &[(PathBuf, bool)],
    harness: Harness,
) -> Result<(Vec<TranscriptCandidate>, bool), String> {
    let mut pending = roots
        .iter()
        .map(|(path, archived)| PendingDirectory {
            modified_epoch: modified_epoch(path),
            path: path.clone(),
            archived: *archived,
        })
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut directories_seen = 0usize;
    let mut truncated = false;
    while !pending.is_empty() && directories_seen < HISTORY_SOURCE_LIMIT {
        pending.sort_by(|left, right| {
            right
                .modified_epoch
                .cmp(&left.modified_epoch)
                .then_with(|| left.path.cmp(&right.path))
        });
        let directory = pending.remove(0);
        directories_seen += 1;
        let mut entries = fs::read_dir(&directory.path)
            .map_err(|error| format!("History cannot read provider source: {error}"))?
            .take(MAX_DIRECTORY_ENTRIES + 1)
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        if entries.len() > MAX_DIRECTORY_ENTRIES {
            entries.truncate(MAX_DIRECTORY_ENTRIES);
            truncated = true;
        }
        entries.sort_by_key(|left| left.path());
        let mut directory_files = Vec::new();
        for entry in entries {
            let Ok(file_type) = entry.file_type() else {
                truncated = true;
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(PendingDirectory {
                    modified_epoch: modified_epoch(&path),
                    path,
                    archived: directory.archived,
                });
            } else if file_type.is_file() && transcript_name_matches(&path, harness) {
                directory_files.push(TranscriptCandidate {
                    modified_epoch: modified_epoch(&path),
                    path,
                    archived: directory.archived,
                });
            }
        }
        directory_files.sort_by(|left, right| {
            right
                .modified_epoch
                .cmp(&left.modified_epoch)
                .then_with(|| left.path.cmp(&right.path))
        });
        if directory_files.len() > MAX_CANDIDATES_PER_DIRECTORY {
            directory_files.truncate(MAX_CANDIDATES_PER_DIRECTORY);
            truncated = true;
        }
        files.extend(directory_files);
        if files.len() >= MAX_DISCOVERY_CANDIDATES {
            files.truncate(MAX_DISCOVERY_CANDIDATES);
            truncated |= !pending.is_empty();
            break;
        }
    }
    truncated |= !pending.is_empty();
    Ok((files, truncated))
}

fn transcript_name_matches(path: &Path, harness: Harness) -> bool {
    if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
        return false;
    }
    match harness {
        Harness::Claude => true,
        Harness::Codex => path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("rollout-")),
    }
}

fn modified_epoch(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let len = file.metadata().map_err(|error| error.to_string())?.len();
    if len <= TRANSCRIPT_WINDOW_BYTES as u64 {
        let mut bytes = Vec::with_capacity(len as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let half = TRANSCRIPT_WINDOW_BYTES / 2;
    let mut head = vec![0; half];
    file.read_exact(&mut head)
        .map_err(|error| error.to_string())?;
    file.seek(SeekFrom::End(-(half as i64)))
        .map_err(|error| error.to_string())?;
    let mut tail = vec![0; half];
    file.read_exact(&mut tail)
        .map_err(|error| error.to_string())?;
    if let Some(last_newline) = head.iter().rposition(|byte| *byte == b'\n') {
        head.truncate(last_newline + 1);
    }
    if let Some(first_newline) = tail.iter().position(|byte| *byte == b'\n') {
        tail.drain(..=first_newline);
    }
    head.extend(tail);
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn apply_associations(
    mut entries: Vec<HistoryEntry>,
    associations: &[HistoryAssociation],
) -> Vec<HistoryEntry> {
    let mut exact = BTreeMap::<(Harness, String), Vec<&HistoryAssociation>>::new();
    for association in associations {
        exact
            .entry((association.harness, association.conversation_id.clone()))
            .or_default()
            .push(association);
    }
    for entry in &mut entries {
        let Some(matches) = exact.get(&(entry.harness, entry.conversation_id.clone())) else {
            continue;
        };
        if matches.len() != 1 {
            mark_recovery_required(entry, "Conversation has ambiguous durable bindings.");
            continue;
        }
        let association = matches[0];
        entry.project_id = association.project_id.clone();
        entry.project_name = association.project_name.clone();
        entry.captain_id = association.captain_id.clone();
        entry.role = association.role.clone();
        entry.workspace_id = association.workspace_id.clone();
        entry.worktree_id = association.worktree_id.clone();
        entry.branch = association.branch.clone();
        match association.liveness {
            AssociationLiveness::Active if association.terminal_id.is_some() => {
                entry.continuity_state = ContinuityState::Active;
                entry.actions = HistoryActions {
                    focus: supported(),
                    resume: unavailable("Conversation is active."),
                    recover: unavailable("Conversation does not require recovery."),
                    archive: unavailable("Active conversations cannot be archived."),
                    unarchive: unavailable("Conversation is not archived."),
                };
            }
            AssociationLiveness::Unknown => {
                mark_recovery_required(entry, "Conversation liveness could not be verified.");
            }
            _ => {}
        }
    }
    entries
}

fn set_resumable_actions(entry: &mut HistoryEntry) {
    if entry.continuity_state == ContinuityState::Archived {
        entry.actions = HistoryActions {
            focus: unavailable("Conversation is not active."),
            resume: unavailable("Archived conversations cannot be resumed directly."),
            recover: unavailable("Legacy archive recovery is not available."),
            archive: unavailable("Conversation is already archived."),
            unarchive: unavailable("Legacy archive mutation is not available."),
        };
        return;
    }
    entry.actions = HistoryActions {
        focus: unavailable("Conversation is not active."),
        resume: supported(),
        recover: unavailable("Conversation does not require recovery."),
        archive: unavailable("Per-conversation archive is not available."),
        unarchive: unavailable("Conversation is not archived."),
    };
}

fn mark_recovery_required(entry: &mut HistoryEntry, reason: &str) {
    entry.continuity_state = ContinuityState::RecoveryRequired;
    entry.actions = HistoryActions {
        focus: unavailable(reason),
        resume: unavailable(reason),
        recover: unavailable("Automated History recovery is not available."),
        archive: unavailable(reason),
        unarchive: unavailable(reason),
    };
}

pub fn mark_entry_recovery_required(entry: &mut HistoryEntry, reason: &str) {
    mark_recovery_required(entry, reason);
}

pub fn degrade_runtime_evidence(
    list: &mut HistoryList,
    harness: Harness,
    reason: &str,
) -> Result<(), String> {
    for entry in &mut list.entries {
        if entry.harness == harness && entry.continuity_state == ContinuityState::Resumable {
            mark_recovery_required(entry, reason);
        }
    }
    if let Some(source) = list
        .sources
        .iter_mut()
        .find(|source| source.harness == harness)
    {
        if source.status == HistorySourceStatus::Ready {
            source.status = HistorySourceStatus::Degraded;
        }
        source.reason = Some(bounded_text(reason, HISTORY_REASON_MAX_CHARS));
    }
    list.truncated = true;
    list.revision = revision_for(&list.entries, &list.sources, list.total, list.truncated)?;
    Ok(())
}

fn supported() -> ActionCompatibility {
    ActionCompatibility {
        status: ActionStatus::Supported,
        reason: None,
    }
}

fn unavailable(reason: &str) -> ActionCompatibility {
    ActionCompatibility {
        status: ActionStatus::Unavailable,
        reason: Some(bounded_text(reason, HISTORY_REASON_MAX_CHARS)),
    }
}

fn fair_groups(groups: BTreeMap<String, VecDeque<HistoryEntry>>) -> Vec<HistoryEntry> {
    let mut groups = groups.into_values().collect::<Vec<_>>();
    let mut output = Vec::new();
    loop {
        let mut advanced = false;
        for group in &mut groups {
            if let Some(entry) = group.pop_front() {
                output.push(entry);
                advanced = true;
            }
        }
        if !advanced {
            break;
        }
    }
    output
}

fn fair_harnesses(entries: Vec<HistoryEntry>, max: usize) -> Vec<HistoryEntry> {
    let mut groups = BTreeMap::<Harness, VecDeque<HistoryEntry>>::new();
    for entry in entries {
        groups.entry(entry.harness).or_default().push_back(entry);
    }
    // Each Harness is already project-fair from `fair_groups`. Preserve that
    // order here so a chatty Project cannot regain priority before the global
    // Harness fairness limit is applied.
    let mut groups = groups.into_values().collect::<Vec<_>>();
    let mut output = Vec::new();
    while output.len() < max {
        let mut advanced = false;
        for group in &mut groups {
            if let Some(entry) = group.pop_front() {
                output.push(entry);
                advanced = true;
                if output.len() == max {
                    break;
                }
            }
        }
        if !advanced {
            break;
        }
    }
    output
}

fn history_order(left: &HistoryEntry, right: &HistoryEntry) -> std::cmp::Ordering {
    right
        .last_seen_at
        .cmp(&left.last_seen_at)
        .then_with(|| left.history_id.cmp(&right.history_id))
}

fn revision_for(
    entries: &[HistoryEntry],
    sources: &[HistorySource],
    total: usize,
    truncated: bool,
) -> Result<String, String> {
    let normalized = serde_json::to_vec(&(entries, sources, total, truncated))
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "historyRevision:v1:{:x}",
        Sha256::digest(normalized)
    ))
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, HistoryService) {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join(".claude/projects");
        let codex = temp.path().join(".codex/sessions");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        let service = HistoryService::new(claude, codex, Duration::from_secs(60));
        (temp, service)
    }

    fn write_claude(root: &Path, project: &str, id: &str, cwd: &str, text: &str) {
        let directory = root.join(".claude/projects").join(project);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(format!("{id}.jsonl")),
            json!({"type":"user","cwd":cwd,"message":{"content":text}}).to_string(),
        )
        .unwrap();
    }

    fn write_codex(root: &Path, day: &str, id: &str, cwd: &str, text: &str) {
        let directory = root.join(".codex/sessions/2026/07").join(day);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("rollout-2026-07-{day}T10-00-00-{id}.jsonl"));
        let records = [
            json!({"type":"session_meta","payload":{"id":id,"cwd":cwd,"model_provider":"openai"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":text}}),
        ];
        fs::write(
            path,
            records
                .into_iter()
                .map(|record| record.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    }

    fn resume_operation(
        request_id: &str,
        history_id: &str,
        terminal_id: &str,
        target_tab_id: Option<&str>,
    ) -> (HistoryBinding, HistoryResumeOperation) {
        let conversation_id = "22222222-2222-4222-8222-222222222222";
        (
            HistoryBinding {
                history_id: history_id.to_string(),
                harness: Harness::Codex,
                conversation_id: conversation_id.to_string(),
                terminal_id: terminal_id.to_string(),
            },
            HistoryResumeOperation {
                request_id: request_id.to_string(),
                history_id: history_id.to_string(),
                harness: Harness::Codex,
                conversation_id: conversation_id.to_string(),
                terminal_id: terminal_id.to_string(),
                target_tab_id: target_tab_id.map(str::to_string),
                actual_tab_id: target_tab_id.map(str::to_string),
                authorized_ship_slug: Some("ship-one".to_string()),
                authorized_project_id: Some("project-one".to_string()),
                authorized_assignment_id: Some("assignment-one".to_string()),
                recorded_at_ms: 1_721_491_200_000,
            },
        )
    }

    fn pending_resume(operation: &HistoryResumeOperation) -> HistoryPendingResume {
        HistoryPendingResume {
            request_id: operation.request_id.clone(),
            history_id: operation.history_id.clone(),
            harness: operation.harness,
            conversation_id: operation.conversation_id.clone(),
            terminal_id: operation.terminal_id.clone(),
            target_tab_id: operation.target_tab_id.clone(),
            authorized_ship_slug: operation.authorized_ship_slug.clone(),
            authorized_project_id: operation.authorized_project_id.clone(),
            authorized_assignment_id: operation.authorized_assignment_id.clone(),
            reserved_at_ms: operation.recorded_at_ms.saturating_sub(1),
        }
    }

    #[test]
    fn durable_resume_ledger_survives_restart_and_rejects_request_rebinding() {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join(".claude/projects");
        let codex = temp.path().join(".codex/sessions");
        let state_path = temp.path().join("state/history.json");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        let history_id = "history:v1:durable";
        let (binding, operation) =
            resume_operation("request-one", history_id, "term0001", Some("tab-one"));
        let service = HistoryService::new_persistent(
            claude.clone(),
            codex.clone(),
            Duration::from_secs(60),
            state_path.clone(),
        );
        service.reserve_resume(pending_resume(&operation)).unwrap();
        service
            .record_resume(binding.clone(), operation.clone())
            .unwrap();
        service
            .record_resume(binding.clone(), operation.clone())
            .unwrap();
        drop(service);

        let reloaded =
            HistoryService::new_persistent(claude, codex, Duration::from_secs(60), state_path);
        assert_eq!(
            reloaded.resume_operation("request-one").unwrap(),
            Some(operation)
        );
        assert_eq!(reloaded.bindings(), vec![binding]);

        let (replacement_binding, replacement) =
            resume_operation("request-one", history_id, "term0002", Some("tab-two"));
        let error = reloaded
            .record_resume(replacement_binding, replacement)
            .unwrap_err();
        assert!(error.starts_with("request_conflict:"));
        assert_eq!(
            reloaded
                .resume_operation("request-one")
                .unwrap()
                .unwrap()
                .terminal_id,
            "term0001"
        );
    }

    #[test]
    fn corrupt_durable_resume_ledger_is_preserved_and_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join(".claude/projects");
        let codex = temp.path().join(".codex/sessions");
        let state_path = temp.path().join("history.json");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::write(&state_path, b"not-json").unwrap();
        let service = HistoryService::new_persistent(
            claude,
            codex,
            Duration::from_secs(60),
            state_path.clone(),
        );
        assert!(service.durable_state_error().is_some());
        assert!(service.resume_operation("request-one").is_err());
        let (binding, operation) =
            resume_operation("request-one", "history:v1:corrupt", "term0001", None);
        assert!(service.record_resume(binding, operation).is_err());
        assert_eq!(fs::read(&state_path).unwrap(), b"not-json");
    }

    #[test]
    fn mismatched_binding_and_operation_are_rejected_before_persistence() {
        let (_temp, service) = fixture();
        let (binding, mut operation) =
            resume_operation("request-one", "history:v1:binding", "term0001", None);
        service.reserve_resume(pending_resume(&operation)).unwrap();
        operation.conversation_id = "different-conversation".to_string();
        let error = service.record_resume(binding, operation).unwrap_err();
        assert!(error.starts_with("history_invalid_binding:"));
    }

    #[test]
    fn durable_pending_resume_survives_restart_and_blocks_duplicate_spawns() {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join(".claude/projects");
        let codex = temp.path().join(".codex/sessions");
        let state_path = temp.path().join("state/history.json");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        let (_, operation) = resume_operation(
            "request-one",
            "history:v1:pending",
            "term0001",
            Some("tab-one"),
        );
        let pending = pending_resume(&operation);
        let service = HistoryService::new_persistent(
            claude.clone(),
            codex.clone(),
            Duration::from_secs(60),
            state_path.clone(),
        );
        service.reserve_resume(pending.clone()).unwrap();
        drop(service);

        let reloaded =
            HistoryService::new_persistent(claude, codex, Duration::from_secs(60), state_path);
        assert_eq!(
            reloaded.pending_resume("request-one").unwrap(),
            Some(pending.clone())
        );
        let same_error = reloaded.reserve_resume(pending.clone()).unwrap_err();
        assert!(same_error.starts_with("history_resume_in_flight:"));

        let mut competing = pending;
        competing.request_id = "request-two".to_string();
        let competing_error = reloaded.reserve_resume(competing).unwrap_err();
        assert!(competing_error.starts_with("history_resume_in_flight:"));
    }

    #[test]
    fn completing_resume_atomically_replaces_pending_reservation() {
        let (_temp, service) = fixture();
        let (binding, operation) =
            resume_operation("request-one", "history:v1:complete", "term0001", None);
        service.reserve_resume(pending_resume(&operation)).unwrap();
        service
            .record_resume(binding.clone(), operation.clone())
            .unwrap();

        assert_eq!(service.pending_resume("request-one").unwrap(), None);
        assert_eq!(
            service.resume_operation("request-one").unwrap(),
            Some(operation)
        );
        assert_eq!(service.bindings(), vec![binding]);
    }

    #[test]
    fn cancelling_resume_persists_and_frees_the_history_identity() {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join(".claude/projects");
        let codex = temp.path().join(".codex/sessions");
        let state_path = temp.path().join("state/history.json");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        let (_, operation) = resume_operation(
            "request-one",
            "history:v1:cancelled",
            "term0001",
            Some("tab-gone"),
        );
        let pending = pending_resume(&operation);
        let service = HistoryService::new_persistent(
            claude.clone(),
            codex.clone(),
            Duration::from_secs(60),
            state_path.clone(),
        );
        service.reserve_resume(pending.clone()).unwrap();
        service.cancel_resume_reservation(&pending).unwrap();
        drop(service);

        let reloaded =
            HistoryService::new_persistent(claude, codex, Duration::from_secs(60), state_path);
        assert_eq!(reloaded.pending_resume("request-one").unwrap(), None);

        let (_, replacement) = resume_operation(
            "request-two",
            "history:v1:cancelled",
            "term0002",
            Some("tab-current"),
        );
        reloaded
            .reserve_resume(pending_resume(&replacement))
            .unwrap();
    }

    #[test]
    fn merges_harnesses_and_preserves_same_cwd_conversations() {
        let (temp, service) = fixture();
        write_claude(temp.path(), "repo", "claude-one", "/repo", "Claude task");
        write_claude(temp.path(), "repo", "claude-two", "/repo", "Another task");
        write_codex(
            temp.path(),
            "20",
            "22222222-2222-4222-8222-222222222222",
            "/repo",
            "Codex task",
        );
        let list = service
            .list(
                &HistoryFilter {
                    limit: Some(10),
                    ..HistoryFilter::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(list.entries.len(), 3);
        assert_eq!(
            list.entries
                .iter()
                .map(|entry| entry.history_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        assert!(list.entries.iter().all(|entry| entry.cwd == "/repo"));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.harness == Harness::Codex));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.harness == Harness::Claude));
    }

    #[test]
    fn malformed_provider_does_not_hide_healthy_provider() {
        let (temp, service) = fixture();
        write_claude(temp.path(), "repo", "healthy", "/repo", "Healthy task");
        let codex = temp.path().join(".codex/sessions/2026/07/20");
        fs::create_dir_all(&codex).unwrap();
        fs::write(
            codex.join("rollout-2026-07-20T10-00-00-22222222-2222-4222-8222-222222222222.jsonl"),
            "not json",
        )
        .unwrap();
        let list = service.list(&HistoryFilter::default(), &[]).unwrap();
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].conversation_id, "healthy");
        assert_eq!(
            list.sources
                .iter()
                .find(|source| source.harness == Harness::Codex)
                .unwrap()
                .status,
            HistorySourceStatus::Degraded
        );
    }

    #[test]
    fn exact_association_changes_one_harness_row_to_active() {
        let (temp, service) = fixture();
        let id = "22222222-2222-4222-8222-222222222222";
        write_claude(temp.path(), "repo", id, "/same", "Claude");
        write_codex(temp.path(), "20", id, "/same", "Codex");
        let associations = [HistoryAssociation {
            harness: Harness::Codex,
            conversation_id: id.to_string(),
            terminal_id: Some("tile-codex".to_string()),
            liveness: AssociationLiveness::Active,
            project_id: Some("project".to_string()),
            project_name: Some("Project".to_string()),
            captain_id: None,
            assignment_id: None,
            role: Some("crew".to_string()),
            workspace_id: None,
            worktree_id: None,
            branch: Some("crew/history".to_string()),
        }];
        let list = service
            .list(&HistoryFilter::default(), &associations)
            .unwrap();
        let codex = list
            .entries
            .iter()
            .find(|entry| entry.harness == Harness::Codex)
            .unwrap();
        let claude = list
            .entries
            .iter()
            .find(|entry| entry.harness == Harness::Claude)
            .unwrap();
        assert_eq!(codex.continuity_state, ContinuityState::Active);
        assert_eq!(codex.actions.focus.status, ActionStatus::Supported);
        assert_eq!(codex.actions.resume.status, ActionStatus::Unavailable);
        assert_eq!(codex.project_id.as_deref(), Some("project"));
        assert_eq!(claude.continuity_state, ContinuityState::Resumable);
    }

    #[test]
    fn assignment_scope_requires_ship_project_and_assignment_together() {
        let (temp, service) = fixture();
        let id = "22222222-2222-4222-8222-222222222222";
        write_codex(temp.path(), "20", id, "/repo", "Codex");
        let association = HistoryAssociation {
            harness: Harness::Codex,
            conversation_id: id.to_string(),
            terminal_id: Some("term0001".to_string()),
            liveness: AssociationLiveness::Inactive,
            project_id: Some("project-old".to_string()),
            project_name: Some("Old Project".to_string()),
            captain_id: Some("ship".to_string()),
            assignment_id: Some("assignment-old".to_string()),
            role: Some("crew".to_string()),
            workspace_id: Some("workspace-old".to_string()),
            worktree_id: None,
            branch: None,
        };

        let old = service
            .list_for_assignment(
                &HistoryFilter::default(),
                std::slice::from_ref(&association),
                "ship",
                "project-old",
                "assignment-old",
            )
            .unwrap();
        assert_eq!(old.entries.len(), 1);

        for (project, assignment) in [
            ("project-new", "assignment-old"),
            ("project-old", "assignment-new"),
        ] {
            let rebound = service
                .list_for_assignment(
                    &HistoryFilter::default(),
                    std::slice::from_ref(&association),
                    "ship",
                    project,
                    assignment,
                )
                .unwrap();
            assert!(rebound.entries.is_empty());
        }
    }

    #[test]
    fn ambiguous_exact_association_fails_closed_without_duplicate_rows() {
        let (temp, service) = fixture();
        let id = "22222222-2222-4222-8222-222222222222";
        write_codex(temp.path(), "20", id, "/repo", "Codex");
        let association = HistoryAssociation {
            harness: Harness::Codex,
            conversation_id: id.to_string(),
            terminal_id: Some("one".to_string()),
            liveness: AssociationLiveness::Active,
            project_id: None,
            project_name: None,
            captain_id: None,
            assignment_id: None,
            role: None,
            workspace_id: None,
            worktree_id: None,
            branch: None,
        };
        let list = service
            .list(
                &HistoryFilter::default(),
                &[association.clone(), association],
            )
            .unwrap();
        assert_eq!(list.entries.len(), 1);
        assert_eq!(
            list.entries[0].continuity_state,
            ContinuityState::RecoveryRequired
        );
        assert_eq!(
            list.entries[0].actions.resume.status,
            ActionStatus::Unavailable
        );
    }

    #[test]
    fn source_scan_is_directory_bounded_and_reports_degradation() {
        let (temp, service) = fixture();
        for day in 1..=HISTORY_SOURCE_LIMIT + 5 {
            write_codex(
                temp.path(),
                &format!("{day:02}"),
                &format!("22222222-2222-4222-8222-{day:012}"),
                &format!("/repo/{day}"),
                "task",
            );
        }
        let list = service
            .list(
                &HistoryFilter {
                    limit: Some(HISTORY_ENTRY_LIMIT),
                    ..HistoryFilter::default()
                },
                &[],
            )
            .unwrap();
        let codex_source = list
            .sources
            .iter()
            .find(|source| source.harness == Harness::Codex)
            .unwrap();
        assert_eq!(codex_source.status, HistorySourceStatus::Degraded);
        assert!(codex_source
            .reason
            .as_deref()
            .unwrap()
            .contains("scanTruncated=true"));
        assert!(list.entries.len() < HISTORY_SOURCE_LIMIT + 5);
    }

    #[test]
    fn overrepresented_project_is_bounded_and_reported_as_truncated() {
        let (temp, service) = fixture();
        for index in 0..=MAX_FILES_PER_GROUP {
            write_claude(
                temp.path(),
                "repo",
                &format!("conversation-{index:03}"),
                "/repo",
                "task",
            );
        }
        std::thread::sleep(Duration::from_millis(1_100));
        write_claude(
            temp.path(),
            "repo",
            &format!("conversation-{MAX_FILES_PER_GROUP:03}"),
            "/repo",
            "newest task",
        );
        let list = service
            .list(
                &HistoryFilter {
                    limit: Some(HISTORY_ENTRY_LIMIT),
                    ..HistoryFilter::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(list.total, MAX_FILES_PER_GROUP);
        assert!(
            list.entries
                .iter()
                .any(|entry| entry.conversation_id
                    == format!("conversation-{MAX_FILES_PER_GROUP:03}"))
        );
        assert!(list.truncated);
        let source = list
            .sources
            .iter()
            .find(|source| source.harness == Harness::Claude)
            .unwrap();
        assert_eq!(source.status, HistorySourceStatus::Degraded);
        assert!(source
            .reason
            .as_deref()
            .unwrap()
            .contains("dropped 1 overrepresented Project transcript"));
    }

    #[test]
    fn source_candidate_bound_cannot_erase_a_quiet_project() {
        let (temp, service) = fixture();
        for index in 0..=MAX_FILES_PER_SOURCE {
            write_claude(
                temp.path(),
                "chatty",
                &format!("chatty-{index:05}"),
                "/chatty",
                "chatty task",
            );
        }
        write_claude(temp.path(), "quiet", "quiet-one", "/quiet", "quiet task");

        let list = service
            .list(
                &HistoryFilter {
                    limit: Some(HISTORY_ENTRY_LIMIT),
                    ..HistoryFilter::default()
                },
                &[],
            )
            .unwrap();
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.conversation_id == "quiet-one"));
        assert!(list.truncated);
        assert_eq!(
            list.sources
                .iter()
                .find(|source| source.harness == Harness::Claude)
                .unwrap()
                .status,
            HistorySourceStatus::Degraded
        );
    }

    #[test]
    fn default_result_limit_preserves_project_fairness() {
        let (temp, service) = fixture();
        for index in 0..150 {
            write_claude(
                temp.path(),
                "chatty",
                &format!("chatty-{index:03}"),
                "/chatty",
                "chatty task",
            );
        }
        write_claude(temp.path(), "quiet", "quiet-one", "/quiet", "quiet task");

        let list = service.list(&HistoryFilter::default(), &[]).unwrap();

        assert_eq!(list.count, DEFAULT_RESULT_LIMIT);
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.conversation_id == "quiet-one"));
    }

    #[test]
    fn unknown_candidate_flood_cannot_hide_a_known_project() {
        let temp = tempfile::tempdir().unwrap();
        let healthy_path = temp.path().join("healthy.jsonl");
        fs::write(
            &healthy_path,
            json!({"type":"user","cwd":"/healthy","message":{"content":"healthy"}}).to_string(),
        )
        .unwrap();
        let mut candidates = (0..=MAX_FILES_PER_SOURCE)
            .map(|index| TranscriptCandidate {
                path: temp.path().join(format!("missing-{index:05}.jsonl")),
                modified_epoch: 10_000 + index as i64,
                archived: false,
            })
            .collect::<Vec<_>>();
        candidates.push(TranscriptCandidate {
            path: healthy_path.clone(),
            modified_epoch: 1,
            archived: false,
        });

        let selected =
            fair_transcript_candidates(candidates, Harness::Claude, MAX_FILES_PER_SOURCE);

        assert!(selected
            .iter()
            .any(|candidate| candidate.path == healthy_path));
    }

    #[test]
    fn harness_fairness_preserves_project_fair_input_order() {
        let make_entry = |id: &str, cwd: &str, epoch: i64| {
            let path = PathBuf::from(format!("{id}.jsonl"));
            let transcript = json!({
                "type": "user",
                "cwd": cwd,
                "message": {"content": id}
            })
            .to_string();
            let mut entry = parse_claude_transcript(&path, &transcript, epoch, false)
                .unwrap()
                .entry;
            set_resumable_actions(&mut entry);
            entry
        };
        let entries = vec![
            make_entry("project-a-new", "/a", 30),
            make_entry("project-b", "/b", 10),
            make_entry("project-a-next", "/a", 20),
        ];
        let result = fair_harnesses(entries, 3);
        assert_eq!(
            result
                .iter()
                .map(|entry| entry.conversation_id.as_str())
                .collect::<Vec<_>>(),
            vec!["project-a-new", "project-b", "project-a-next"]
        );
    }

    #[test]
    fn revision_changes_when_total_or_truncation_changes() {
        let sources = [HistorySource {
            harness: Harness::Codex,
            status: HistorySourceStatus::Ready,
            reason: None,
        }];
        let baseline = revision_for(&[], &sources, 0, false).unwrap();
        assert_ne!(baseline, revision_for(&[], &sources, 1, false).unwrap());
        assert_ne!(baseline, revision_for(&[], &sources, 0, true).unwrap());
    }
}
