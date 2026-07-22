//! Durable Preview profile storage.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::model::{PreviewOperation, PreviewScope, PreviewStatus, PreviewTarget, PreviewTargetId};

pub const PROFILE_SCHEMA_VERSION: u32 = 1;
const IDEMPOTENCY_JOURNAL_CAP: usize = 256;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedPreviewTarget {
    pub target_id: PreviewTargetId,
    pub discovery_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct PreviewIntent {
    pub request_id: String,
    pub scope: PreviewScope,
    pub operation: PreviewOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<PreviewTargetId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub phase: PreviewIntentPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_status: Option<PreviewStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

fn load_profiles(path: &Path) -> Result<PreviewProfilesV1, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreviewProfilesV1::default())
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
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
    if profiles.idempotency_journal.len() > IDEMPOTENCY_JOURNAL_CAP {
        return Err(format!(
            "Preview profiles {} exceed the idempotency journal bound",
            path.display()
        ));
    }
    Ok(profiles)
}

fn persist_profiles(path: &Path, state: &PreviewProfilesV1) -> Result<(), String> {
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
            run_id: Some("run-1".into()),
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
                None,
                None,
                2,
            )
            .unwrap();
        assert_eq!(store.recoverable_intents().len(), 1);
        store
            .advance_intent("recover-me", PreviewIntentPhase::Committed, None, None, 3)
            .unwrap();
        assert!(store.recoverable_intents().is_empty());
        assert!(store
            .advance_intent("recover-me", PreviewIntentPhase::Prepared, None, None, 4,)
            .is_err());
    }
}
