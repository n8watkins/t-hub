//! Durable Preview profile storage.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::endpoint::ManagedRunIdentity;
use super::model::{
    PreviewOperation, PreviewScope, PreviewStatus, PreviewTarget, PreviewTargetId,
    PreviewTargetKind, PreviewTargetRef, PreviewTargetSource,
};

pub const PROFILE_SCHEMA_VERSION: u32 = 1;
const IDEMPOTENCY_JOURNAL_CAP: usize = 256;
const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROJECTS: usize = 4096;
const MAX_ACCEPTED_TARGETS: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedPreviewTarget {
    pub target_id: PreviewTargetId,
    pub discovery_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectPreviewProfile {
    pub canonical_root_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_target: Option<SelectedPreviewTarget>,
    #[serde(default)]
    pub workspace_overrides: BTreeMap<String, SelectedPreviewTarget>,
    #[serde(default)]
    pub accepted_config_targets: Vec<PreviewTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewIntentPhase {
    Prepared,
    EffectObserved,
    Committed,
    RecoveryBlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewIntent {
    pub request_id: String,
    pub scope: PreviewScope,
    pub operation: PreviewOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<PreviewTargetId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_stop_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_run: Option<ManagedRunIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_stop_run: Option<ManagedRunIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_stop_target: Option<super::model::PreviewTargetRef>,
    pub phase: PreviewIntentPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_status: Option<PreviewStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewProfilesV1 {
    pub schema_version: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectPreviewProfile>,
    #[serde(default)]
    pub idempotency_journal: VecDeque<PreviewIntent>,
}

impl Default for PreviewProfilesV1 {
    fn default() -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            projects: BTreeMap::new(),
            idempotency_journal: VecDeque::new(),
        }
    }
}

pub struct PreviewProfileStore {
    path: PathBuf,
    state: Mutex<PreviewProfilesV1>,
}

impl PreviewProfileStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let state = load_profiles(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn snapshot(&self) -> PreviewProfilesV1 {
        self.state.lock().clone()
    }

    pub fn project(&self, project_id: &str) -> Option<ProjectPreviewProfile> {
        self.state.lock().projects.get(project_id).cloned()
    }

    pub fn put_project(
        &self,
        project_id: &str,
        profile: ProjectPreviewProfile,
    ) -> Result<(), String> {
        let mut guard = self.state.lock();
        let mut next = guard.clone();
        next.projects.insert(project_id.to_string(), profile);
        persist_profiles(&self.path, &next)?;
        *guard = next;
        Ok(())
    }

    pub fn update_project<F>(&self, project_id: &str, update: F) -> Result<(), String>
    where
        F: FnOnce(Option<ProjectPreviewProfile>) -> Result<ProjectPreviewProfile, String>,
    {
        let mut guard = self.state.lock();
        let profile = update(guard.projects.get(project_id).cloned())?;
        let mut next = guard.clone();
        next.projects.insert(project_id.to_string(), profile);
        persist_profiles(&self.path, &next)?;
        *guard = next;
        Ok(())
    }

    pub fn intent(&self, request_id: &str) -> Option<PreviewIntent> {
        self.state
            .lock()
            .idempotency_journal
            .iter()
            .find(|intent| intent.request_id == request_id)
            .cloned()
    }

    pub fn record_intent(&self, intent: PreviewIntent) -> Result<PreviewIntent, String> {
        if intent.request_id.is_empty() || intent.request_id.len() > 160 {
            return Err("Preview request id must contain 1 to 160 bytes".into());
        }
        let mut guard = self.state.lock();
        if let Some(existing) = guard
            .idempotency_journal
            .iter()
            .find(|entry| entry.request_id == intent.request_id)
        {
            if existing.scope != intent.scope
                || existing.operation != intent.operation
                || existing.target_id != intent.target_id
                || existing.discovery_fingerprint != intent.discovery_fingerprint
                || existing.run_id != intent.run_id
                || existing.requested_stop_run_id != intent.requested_stop_run_id
                || existing.expected_stop_run != intent.expected_stop_run
                || existing.expected_stop_target != intent.expected_stop_target
            {
                return Err("request id is already bound to a different Preview operation".into());
            }
            return Ok(existing.clone());
        }
        if intent.phase != PreviewIntentPhase::Prepared {
            return Err("a new Preview intent must begin in prepared state".into());
        }
        let mut next = guard.clone();
        next.idempotency_journal.push_back(intent.clone());
        while next.idempotency_journal.len() > IDEMPOTENCY_JOURNAL_CAP {
            let terminal = next.idempotency_journal.iter().position(|entry| {
                matches!(
                    entry.phase,
                    PreviewIntentPhase::Committed | PreviewIntentPhase::RecoveryBlocked
                )
            });
            let Some(terminal) = terminal else {
                return Err("Preview idempotency journal is full of incomplete intents".into());
            };
            next.idempotency_journal.remove(terminal);
        }
        persist_profiles(&self.path, &next)?;
        *guard = next;
        Ok(intent)
    }

    pub fn advance_intent(
        &self,
        request_id: &str,
        phase: PreviewIntentPhase,
        observed_status: Option<PreviewStatus>,
        detail: Option<String>,
        updated_at_ms: u64,
    ) -> Result<PreviewIntent, String> {
        let mut guard = self.state.lock();
        let mut next = guard.clone();
        let intent = next
            .idempotency_journal
            .iter_mut()
            .find(|entry| entry.request_id == request_id)
            .ok_or_else(|| "unknown Preview request id".to_string())?;
        if !valid_phase_transition(intent.phase, phase) {
            return Err(format!(
                "invalid Preview intent transition from {:?} to {:?}",
                intent.phase, phase
            ));
        }
        intent.phase = phase;
        intent.observed_status = observed_status;
        intent.detail = detail;
        intent.updated_at_ms = updated_at_ms;
        let result = intent.clone();
        persist_profiles(&self.path, &next)?;
        *guard = next;
        Ok(result)
    }

    pub fn observe_managed_run(
        &self,
        request_id: &str,
        managed_run: ManagedRunIdentity,
        observed_status: PreviewStatus,
        updated_at_ms: u64,
    ) -> Result<PreviewIntent, String> {
        managed_run.validate()?;
        let mut guard = self.state.lock();
        let mut next = guard.clone();
        let intent = next
            .idempotency_journal
            .iter_mut()
            .find(|entry| entry.request_id == request_id)
            .ok_or_else(|| "unknown Preview request id".to_string())?;
        if intent.run_id.as_deref() != Some(managed_run.run_id.as_str()) {
            return Err("managed Preview identity does not match prepared run id".into());
        }
        if !valid_phase_transition(intent.phase, PreviewIntentPhase::EffectObserved) {
            return Err("Preview intent cannot observe a managed run in this phase".into());
        }
        intent.phase = PreviewIntentPhase::EffectObserved;
        intent.managed_run = Some(managed_run);
        intent.observed_status = Some(observed_status);
        intent.updated_at_ms = updated_at_ms;
        let result = intent.clone();
        persist_profiles(&self.path, &next)?;
        *guard = next;
        Ok(result)
    }

    pub fn recoverable_intents(&self) -> Vec<PreviewIntent> {
        self.state
            .lock()
            .idempotency_journal
            .iter()
            .filter(|intent| {
                matches!(
                    intent.phase,
                    PreviewIntentPhase::Prepared | PreviewIntentPhase::EffectObserved
                )
            })
            .cloned()
            .collect()
    }
}

fn valid_phase_transition(from: PreviewIntentPhase, to: PreviewIntentPhase) -> bool {
    from == to
        || matches!(
            (from, to),
            (
                PreviewIntentPhase::Prepared,
                PreviewIntentPhase::EffectObserved
            ) | (
                PreviewIntentPhase::Prepared,
                PreviewIntentPhase::RecoveryBlocked
            ) | (
                PreviewIntentPhase::EffectObserved,
                PreviewIntentPhase::Committed
            ) | (
                PreviewIntentPhase::EffectObserved,
                PreviewIntentPhase::RecoveryBlocked
            )
        )
}

impl PreviewProfilesV1 {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported Preview profiles schemaVersion {}; expected {PROFILE_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.projects.len() > MAX_PROJECTS {
            return Err("Preview profiles exceed the project bound".into());
        }
        if self.idempotency_journal.len() > IDEMPOTENCY_JOURNAL_CAP {
            return Err("Preview profiles exceed the idempotency journal bound".into());
        }
        for (project_id, profile) in &self.projects {
            PreviewScope::new(project_id, None)?;
            validate_fingerprint(
                &profile.canonical_root_fingerprint,
                "canonical root fingerprint",
            )?;
            if let Some(selected) = &profile.selected_target {
                validate_selection(selected)?;
            }
            for (workspace_id, selected) in &profile.workspace_overrides {
                PreviewScope::new(project_id, Some(workspace_id.clone()))?;
                validate_selection(selected)?;
            }
            if profile.accepted_config_targets.len() > MAX_ACCEPTED_TARGETS {
                return Err("Preview profile exceeds the accepted target bound".into());
            }
            for target in &profile.accepted_config_targets {
                validate_accepted_target(target)?;
            }
        }
        let mut request_ids = std::collections::HashSet::new();
        for intent in &self.idempotency_journal {
            if !request_ids.insert(intent.request_id.as_str()) {
                return Err("Preview idempotency journal contains a duplicate request id".into());
            }
            validate_intent(intent)?;
        }
        Ok(())
    }
}

fn validate_selection(selected: &SelectedPreviewTarget) -> Result<(), String> {
    validate_fingerprint(
        &selected.discovery_fingerprint,
        "selected target discovery fingerprint",
    )
}

fn validate_accepted_target(target: &PreviewTarget) -> Result<(), String> {
    if target.source != PreviewTargetSource::Config {
        return Err("persisted accepted Preview targets must come from config".into());
    }
    if target.label.trim().is_empty() || target.label.len() > 200 {
        return Err("persisted Preview target label must contain 1 to 200 bytes".into());
    }
    validate_relative_path(&target.relative_root, true)?;
    match &target.kind {
        PreviewTargetKind::PackageScript { script, .. }
            if matches!(script.as_str(), "dev" | "preview" | "start") =>
        {
            Ok(())
        }
        PreviewTargetKind::PackageScript { .. } => {
            Err("persisted Preview package script is unsupported".into())
        }
        PreviewTargetKind::StaticSite { entrypoint } => validate_relative_path(entrypoint, false),
    }
}

fn validate_intent(intent: &PreviewIntent) -> Result<(), String> {
    validate_bounded_text(&intent.request_id, "Preview request id", 160, false)?;
    if !matches!(
        intent.operation,
        PreviewOperation::Select
            | PreviewOperation::Start
            | PreviewOperation::Stop
            | PreviewOperation::Restart
            | PreviewOperation::Open
            | PreviewOperation::Refresh
    ) {
        return Err("Preview operation cannot be stored in the idempotency journal".into());
    }
    if intent.target_id.is_some() != intent.discovery_fingerprint.is_some() {
        return Err("Preview intent target and discovery fingerprint must be paired".into());
    }
    if let Some(fingerprint) = intent.discovery_fingerprint.as_deref() {
        validate_fingerprint(fingerprint, "intent discovery fingerprint")?;
    }
    if matches!(
        intent.operation,
        PreviewOperation::Select | PreviewOperation::Start | PreviewOperation::Restart
    ) && intent.target_id.is_none()
    {
        return Err("Preview intent operation requires a target reference".into());
    }
    if matches!(
        intent.operation,
        PreviewOperation::Start | PreviewOperation::Restart
    ) && intent.run_id.is_none()
    {
        return Err("Preview start intent requires a run id".into());
    }
    if let Some(run_id) = intent.run_id.as_deref() {
        validate_bounded_text(run_id, "Preview run id", 160, false)?;
    }
    if let Some(run_id) = intent.requested_stop_run_id.as_deref() {
        if intent.operation != PreviewOperation::Stop {
            return Err("requested stop run id belongs to a non-stop operation".into());
        }
        validate_bounded_text(run_id, "requested Preview stop run id", 160, false)?;
    }
    if let Some(managed_run) = &intent.managed_run {
        managed_run.validate()?;
        if intent.run_id.as_deref() != Some(managed_run.run_id.as_str()) {
            return Err("persisted managed Preview identity does not match its run id".into());
        }
        if intent.phase == PreviewIntentPhase::Prepared {
            return Err("prepared Preview intent cannot contain a managed run".into());
        }
        if !matches!(
            intent.operation,
            PreviewOperation::Start | PreviewOperation::Restart
        ) {
            return Err("managed Preview identity belongs to a non-start operation".into());
        }
    }
    if intent.expected_stop_run.is_some() != intent.expected_stop_target.is_some() {
        return Err("Preview expected-stop identity and target must be paired".into());
    }
    if let Some(expected) = &intent.expected_stop_run {
        if !matches!(
            intent.operation,
            PreviewOperation::Start | PreviewOperation::Stop | PreviewOperation::Restart
        ) {
            return Err(
                "Preview expected-stop identity belongs to an unsupported operation".into(),
            );
        }
        expected.validate()?;
        if intent.operation == PreviewOperation::Stop
            && intent.run_id.as_deref() != Some(expected.run_id.as_str())
        {
            return Err("Preview stop intent does not match its expected run".into());
        }
    }
    if let Some(target) = &intent.expected_stop_target {
        validate_target_ref(target)?;
        if target.scope != intent.scope {
            return Err("Preview expected-stop target belongs to another scope".into());
        }
    }
    if let Some(status) = &intent.observed_status {
        validate_status(status)?;
        if status.scope != intent.scope {
            return Err("persisted Preview status belongs to another scope".into());
        }
    }
    if let Some(detail) = intent.detail.as_deref() {
        validate_bounded_text(detail, "Preview intent detail", MAX_TEXT_BYTES, true)?;
    }
    match intent.phase {
        PreviewIntentPhase::Prepared
            if intent.managed_run.is_some()
                || intent.observed_status.is_some()
                || intent.detail.is_some() =>
        {
            return Err("prepared Preview intent contains observed effect data".into());
        }
        PreviewIntentPhase::EffectObserved | PreviewIntentPhase::Committed
            if intent.observed_status.is_none() =>
        {
            return Err("observed Preview intent has no durable status".into());
        }
        PreviewIntentPhase::RecoveryBlocked
            if intent.detail.as_deref().is_none_or(str::is_empty) =>
        {
            return Err("blocked Preview recovery has no detail".into());
        }
        _ => {}
    }
    Ok(())
}

fn validate_target_ref(target: &PreviewTargetRef) -> Result<(), String> {
    validate_fingerprint(
        &target.discovery_fingerprint,
        "Preview target discovery fingerprint",
    )
}

fn validate_status(status: &PreviewStatus) -> Result<(), String> {
    if let Some(run_id) = status.run_id.as_deref() {
        validate_bounded_text(run_id, "Preview status run id", 160, false)?;
    }
    if let Some(url) = status.preview_url.as_deref() {
        validate_bounded_text(url, "Preview URL", MAX_TEXT_BYTES, false)?;
    }
    if let Some(reason) = status.reason.as_deref() {
        validate_bounded_text(reason, "Preview status reason", MAX_TEXT_BYTES, true)?;
    }
    Ok(())
}

fn validate_fingerprint(value: &str, field: &str) -> Result<(), String> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(format!("{field} must be a sha256 fingerprint"));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!(
            "{field} must contain 64 lowercase hexadecimal digits"
        ));
    }
    Ok(())
}

fn validate_relative_path(value: &str, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && value.is_empty()) || value.contains('\\') || Path::new(value).is_absolute()
    {
        return Err("persisted Preview path must be canonical-root-relative".into());
    }
    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| "persisted Preview path must be valid UTF-8".to_string())?,
            ),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err("persisted Preview path must be canonical-root-relative".into());
            }
        }
    }
    if parts.join("/") != value {
        return Err("persisted Preview path is not normalized".into());
    }
    Ok(())
}

fn validate_bounded_text(
    value: &str,
    field: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), String> {
    if (!allow_empty && value.is_empty()) || value.len() > max_bytes {
        return Err(format!("{field} exceeds its length bound"));
    }
    Ok(())
}

fn load_profiles(path: &Path) -> Result<PreviewProfilesV1, String> {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreviewProfilesV1::default())
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_PROFILE_BYTES {
        return Err(format!(
            "Preview profiles {} are not regular or exceed the size bound",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_PROFILE_BYTES {
        return Err(format!(
            "Preview profiles {} exceed the size bound",
            path.display()
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("corrupt Preview profiles {}: {error}", path.display()))?;
    let version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("Preview profiles {} have no schemaVersion", path.display()))?;
    if version != u64::from(PROFILE_SCHEMA_VERSION) {
        return Err(format!(
            "unsupported Preview profiles schemaVersion {version}; expected {PROFILE_SCHEMA_VERSION}"
        ));
    }
    let profiles: PreviewProfilesV1 = serde_json::from_value(value)
        .map_err(|error| format!("invalid Preview profiles {}: {error}", path.display()))?;
    profiles
        .validate()
        .map_err(|error| format!("invalid Preview profiles {}: {error}", path.display()))?;
    Ok(profiles)
}

fn persist_profiles(path: &Path, state: &PreviewProfilesV1) -> Result<(), String> {
    state.validate()?;
    let parent = path
        .parent()
        .ok_or_else(|| "Preview profiles path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("serialize Preview profiles: {error}"))?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<(), String> {
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("create {}: {error}", temp.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("write {}: {error}", temp.display()))?;
        file.sync_all()
            .map_err(|error| format!("sync {}: {error}", temp.display()))?;
        #[cfg(unix)]
        fs::set_permissions(
            &temp,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .map_err(|error| format!("chmod {}: {error}", temp.display()))?;
        replace_file(&temp, path)
            .map_err(|error| format!("publish {}: {error}", path.display()))?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync {}: {error}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
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

    fn temp_path(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "t-hub-preview-profile-{tag}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root.join("preview-profiles.json")
    }

    fn prepared(request_id: &str) -> PreviewIntent {
        PreviewIntent {
            request_id: request_id.into(),
            scope: PreviewScope::new("project-1", None).unwrap(),
            operation: PreviewOperation::Start,
            target_id: Some(PreviewTargetId::parse("root:dev").unwrap()),
            discovery_fingerprint: Some(format!("sha256:{}", "a".repeat(64))),
            run_id: Some("run-1".into()),
            requested_stop_run_id: None,
            managed_run: None,
            expected_stop_run: None,
            expected_stop_target: None,
            phase: PreviewIntentPhase::Prepared,
            observed_status: None,
            detail: None,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn missing_store_starts_at_v1_and_round_trips_atomically() {
        let path = temp_path("roundtrip");
        let store = PreviewProfileStore::open(&path).unwrap();
        assert_eq!(store.snapshot().schema_version, 1);
        store.record_intent(prepared("request-1")).unwrap();
        let reopened = PreviewProfileStore::open(&path).unwrap();
        assert_eq!(
            reopened.intent("request-1").unwrap().phase,
            PreviewIntentPhase::Prepared
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corrupt_and_newer_profiles_fail_closed_without_rewrite() {
        let corrupt = temp_path("corrupt");
        fs::write(&corrupt, b"not json").unwrap();
        assert!(PreviewProfileStore::open(&corrupt).is_err());
        assert_eq!(fs::read(&corrupt).unwrap(), b"not json");

        let newer = temp_path("newer");
        fs::write(&newer, br#"{"schemaVersion":99,"projects":{}}"#).unwrap();
        assert!(PreviewProfileStore::open(&newer).is_err());
        assert_eq!(
            fs::read(&newer).unwrap(),
            br#"{"schemaVersion":99,"projects":{}}"#
        );
    }

    #[test]
    fn unknown_fields_and_invalid_persisted_identifiers_fail_closed() {
        let unknown = temp_path("unknown-field");
        let unknown_bytes =
            br#"{"schemaVersion":1,"projects":{},"idempotencyJournal":[],"extra":true}"#;
        fs::write(&unknown, unknown_bytes).unwrap();
        assert!(PreviewProfileStore::open(&unknown).is_err());
        assert_eq!(fs::read(&unknown).unwrap(), unknown_bytes);

        let invalid_scope = temp_path("invalid-scope");
        let mut profiles = PreviewProfilesV1::default();
        profiles
            .idempotency_journal
            .push_back(prepared("invalid-scope-request"));
        let mut value = serde_json::to_value(&profiles).unwrap();
        value["idempotencyJournal"][0]["scope"]["projectId"] =
            serde_json::Value::String("../outside".into());
        let invalid_scope_bytes = serde_json::to_vec(&value).unwrap();
        fs::write(&invalid_scope, &invalid_scope_bytes).unwrap();
        assert!(PreviewProfileStore::open(&invalid_scope).is_err());
        assert_eq!(fs::read(&invalid_scope).unwrap(), invalid_scope_bytes);
    }

    #[test]
    fn oversized_profile_file_fails_before_json_parsing() {
        let path = temp_path("oversized-file");
        let bytes = vec![b' '; MAX_PROFILE_BYTES as usize + 1];
        fs::write(&path, &bytes).unwrap();
        let error = PreviewProfileStore::open(&path).err().unwrap();
        assert!(error.contains("size bound"));
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_PROFILE_BYTES + 1);
    }

    #[test]
    fn duplicate_request_ids_and_invalid_fingerprints_fail_closed() {
        let duplicate = temp_path("duplicate-request");
        let mut profiles = PreviewProfilesV1::default();
        profiles.idempotency_journal.push_back(prepared("same"));
        profiles.idempotency_journal.push_back(prepared("same"));
        fs::write(&duplicate, serde_json::to_vec(&profiles).unwrap()).unwrap();
        assert!(PreviewProfileStore::open(&duplicate).is_err());

        let invalid_fingerprint = temp_path("invalid-fingerprint");
        let mut profiles = PreviewProfilesV1::default();
        let mut intent = prepared("bad-fingerprint");
        intent.discovery_fingerprint = Some("sha256:not-a-digest".into());
        profiles.idempotency_journal.push_back(intent);
        fs::write(&invalid_fingerprint, serde_json::to_vec(&profiles).unwrap()).unwrap();
        assert!(PreviewProfileStore::open(&invalid_fingerprint).is_err());
    }

    #[test]
    fn oversized_journal_fails_closed_without_dropping_incomplete_intents() {
        let path = temp_path("oversized");
        let mut profiles = PreviewProfilesV1::default();
        for index in 0..=IDEMPOTENCY_JOURNAL_CAP {
            profiles
                .idempotency_journal
                .push_back(prepared(&format!("request-{index}")));
        }
        let original = serde_json::to_vec(&profiles).unwrap();
        fs::write(&path, &original).unwrap();
        assert!(PreviewProfileStore::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn request_ids_are_idempotent_and_conflicts_fail_closed() {
        let store = PreviewProfileStore::open(temp_path("idempotent")).unwrap();
        let first = prepared("same-request");
        assert_eq!(store.record_intent(first.clone()).unwrap(), first);
        assert_eq!(store.record_intent(first.clone()).unwrap(), first);
        let mut conflict = first;
        conflict.operation = PreviewOperation::Stop;
        assert!(store.record_intent(conflict).is_err());
        let mut conflicting_run = prepared("same-request");
        conflicting_run.run_id = Some("run-2".into());
        assert!(store.record_intent(conflicting_run).is_err());
        assert_eq!(store.snapshot().idempotency_journal.len(), 1);
    }

    #[test]
    fn crash_phases_are_recoverable_until_commit_or_block() {
        let store = PreviewProfileStore::open(temp_path("recovery")).unwrap();
        store.record_intent(prepared("recover-me")).unwrap();
        assert_eq!(store.recoverable_intents().len(), 1);
        store
            .advance_intent(
                "recover-me",
                PreviewIntentPhase::EffectObserved,
                Some(PreviewStatus::stopped(
                    PreviewScope::new("project-1", None).unwrap(),
                    2,
                )),
                None,
                2,
            )
            .unwrap();
        assert_eq!(store.recoverable_intents().len(), 1);
        store
            .advance_intent(
                "recover-me",
                PreviewIntentPhase::Committed,
                Some(PreviewStatus::stopped(
                    PreviewScope::new("project-1", None).unwrap(),
                    3,
                )),
                None,
                3,
            )
            .unwrap();
        assert!(store.recoverable_intents().is_empty());
        assert!(store
            .advance_intent("recover-me", PreviewIntentPhase::Prepared, None, None, 4,)
            .is_err());
    }
}
