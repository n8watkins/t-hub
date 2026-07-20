use super::{
    bounded_text, parse_claude_transcript, parse_codex_rollout, ActionCompatibility, ActionStatus,
    ContinuityState, Harness, HistoryActions, HistoryEntry, HISTORY_REASON_MAX_CHARS,
};
use chrono::{SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
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
const TRANSCRIPT_WINDOW_BYTES: usize = 128 * 1024;
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(15);

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
    pub role: Option<String>,
    pub workspace_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryBinding {
    pub history_id: String,
    pub harness: Harness,
    pub conversation_id: String,
    pub terminal_id: String,
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
    bindings: Mutex<BTreeMap<String, HistoryBinding>>,
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
        Arc::new(Self::new(claude_root, codex_root, DEFAULT_CACHE_TTL))
    }

    pub fn new(claude_root: PathBuf, codex_root: PathBuf, cache_ttl: Duration) -> Self {
        Self {
            claude_root,
            codex_root,
            cache_ttl,
            cache: Mutex::new(None),
            bindings: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn invalidate(&self) {
        *self.cache.lock() = None;
    }

    pub fn record_binding(&self, binding: HistoryBinding) {
        self.bindings
            .lock()
            .insert(binding.history_id.clone(), binding);
    }

    pub fn bindings(&self) -> Vec<HistoryBinding> {
        self.bindings.lock().values().cloned().collect()
    }

    pub fn list(
        &self,
        filter: &HistoryFilter,
        associations: &[HistoryAssociation],
    ) -> Result<HistoryList, String> {
        validate_filter(filter)?;
        let catalog = self.catalog()?;
        let mut entries = apply_associations(catalog.entries.clone(), associations);
        entries.retain(|entry| {
            filter
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
        entries = fair_harnesses(entries, HISTORY_ENTRY_LIMIT);
        entries.sort_by(history_order);
        let limit = filter.limit.unwrap_or(DEFAULT_RESULT_LIMIT);
        entries.truncate(limit);
        let sources = catalog.sources.clone();
        let revision = revision_for(&entries, &sources)?;
        Ok(HistoryList {
            schema_version: HISTORY_SCHEMA_VERSION,
            generated_at: now_rfc3339(),
            revision,
            count: entries.len(),
            total,
            truncated: total > entries.len()
                || sources
                    .iter()
                    .any(|source| source.status != HistorySourceStatus::Ready),
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
    let (mut candidates, scan_truncated) = collect_transcripts(&roots, harness)?;
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
    for entry in identities.into_values() {
        let group = groups.entry(entry.cwd.clone()).or_default();
        if group.len() < MAX_FILES_PER_GROUP {
            group.push_back(entry);
        }
    }
    for group in groups.values_mut() {
        group.make_contiguous().sort_by(history_order);
    }
    let entries = fair_groups(groups);
    let reason = (unreadable > 0 || degraded > 0 || duplicates > 0 || scan_truncated).then(|| {
        bounded_text(
            &format!(
                "{} History skipped {unreadable} unreadable or unsupported transcript(s), retained {degraded} degraded transcript(s), found {duplicates} duplicate identity record(s), and scanTruncated={scan_truncated}.",
                harness.canonical()
            ),
            HISTORY_REASON_MAX_CHARS,
        )
    });
    Ok((entries, reason))
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
                files.push(TranscriptCandidate {
                    modified_epoch: modified_epoch(&path),
                    path,
                    archived: directory.archived,
                });
                if files.len() == MAX_FILES_PER_SOURCE {
                    truncated = true;
                    break;
                }
            }
        }
        if files.len() == MAX_FILES_PER_SOURCE {
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
    for group in groups.values_mut() {
        group.make_contiguous().sort_by(history_order);
    }
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

fn revision_for(entries: &[HistoryEntry], sources: &[HistorySource]) -> Result<String, String> {
    let normalized = serde_json::to_vec(&(entries, sources)).map_err(|error| error.to_string())?;
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
}
