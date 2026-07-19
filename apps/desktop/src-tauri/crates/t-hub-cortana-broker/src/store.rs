use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use t_hub_cortana_broker_protocol::{BrokerJournalSnapshot, BrokerRefusal, FakeBrokerJournal};

const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalCommit {
    Durable,
    CommittedUnsynced,
}

#[derive(Debug)]
pub enum JournalStoreError {
    Unavailable,
    Incompatible,
    CommittedUnsynced,
}

pub struct PrivateJournalStore {
    path: PathBuf,
    expected_uid: u32,
}

impl PrivateJournalStore {
    pub fn new(path: PathBuf, expected_uid: u32) -> Self {
        Self { path, expected_uid }
    }

    pub fn load(&self) -> Result<FakeBrokerJournal, JournalStoreError> {
        if !self.path.exists() {
            self.validate_parent()?;
            return Ok(FakeBrokerJournal::default());
        }
        let metadata =
            std::fs::symlink_metadata(&self.path).map_err(|_| JournalStoreError::Unavailable)?;
        if !metadata.file_type().is_file()
            || metadata.uid() != self.expected_uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() == 0
            || metadata.len() > MAX_JOURNAL_BYTES
        {
            return Err(JournalStoreError::Incompatible);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
            .map_err(|_| JournalStoreError::Unavailable)?;
        let opened = file
            .metadata()
            .map_err(|_| JournalStoreError::Unavailable)?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(JournalStoreError::Incompatible);
        }
        let mut bytes = Vec::with_capacity(opened.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|_| JournalStoreError::Unavailable)?;
        if bytes.len() as u64 != opened.len() {
            return Err(JournalStoreError::Incompatible);
        }
        let snapshot: BrokerJournalSnapshot =
            serde_json::from_slice(&bytes).map_err(|_| JournalStoreError::Incompatible)?;
        FakeBrokerJournal::from_snapshot(snapshot).map_err(|_| JournalStoreError::Incompatible)
    }

    pub fn save(
        &self,
        snapshot: &BrokerJournalSnapshot,
    ) -> Result<JournalCommit, JournalStoreError> {
        FakeBrokerJournal::from_snapshot(snapshot.clone())
            .map_err(|_| JournalStoreError::Incompatible)?;
        self.validate_parent()?;
        let bytes = serde_json::to_vec(snapshot).map_err(|_| JournalStoreError::Incompatible)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(JournalStoreError::Incompatible);
        }
        let parent = self.path.parent().ok_or(JournalStoreError::Incompatible)?;
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(JournalStoreError::Incompatible)?;
        let temp = parent.join(format!(
            ".{name}.{}.{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)
            .map_err(|_| JournalStoreError::Unavailable)?;
        let before_rename = (|| {
            file.write_all(&bytes)
                .map_err(|_| JournalStoreError::Unavailable)?;
            file.sync_all()
                .map_err(|_| JournalStoreError::Unavailable)?;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| JournalStoreError::Unavailable)?;
            std::fs::rename(&temp, &self.path).map_err(|_| JournalStoreError::Unavailable)
        })();
        if let Err(error) = before_rename {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
        let directory = File::open(parent).map_err(|_| JournalStoreError::CommittedUnsynced)?;
        if directory.sync_all().is_err() {
            return Ok(JournalCommit::CommittedUnsynced);
        }
        Ok(JournalCommit::Durable)
    }

    fn validate_parent(&self) -> Result<(), JournalStoreError> {
        let parent = self.path.parent().ok_or(JournalStoreError::Incompatible)?;
        validate_directory_chain(parent, self.expected_uid)
    }
}

fn validate_directory_chain(path: &Path, expected_uid: u32) -> Result<(), JournalStoreError> {
    if !path.is_absolute() {
        return Err(JournalStoreError::Incompatible);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| JournalStoreError::Unavailable)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != expected_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(JournalStoreError::Incompatible);
    }
    Ok(())
}

impl From<BrokerRefusal> for JournalStoreError {
    fn from(_: BrokerRefusal) -> Self {
        Self::Incompatible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use t_hub_cortana_broker_protocol::BROKER_JOURNAL_SCHEMA_VERSION;

    fn store(root: &Path) -> PrivateJournalStore {
        PrivateJournalStore::new(root.join("journal.json"), unsafe { libc::geteuid() })
    }

    #[test]
    fn private_journal_roundtrips_with_exact_permissions() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = store(temp.path());
        let journal = FakeBrokerJournal::default();
        assert_eq!(
            store.save(&journal.snapshot()).unwrap(),
            JournalCommit::Durable
        );
        assert_eq!(store.load().unwrap().snapshot(), journal.snapshot());
        let metadata = std::fs::metadata(temp.path().join("journal.json")).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }

    #[test]
    fn malformed_future_and_permission_failure_preserve_source_bytes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = store(temp.path());
        let path = temp.path().join("journal.json");
        let future = serde_json::to_vec(&BrokerJournalSnapshot {
            schema_version: BROKER_JOURNAL_SCHEMA_VERSION + 1,
            authority_generation: 0,
            operations: Default::default(),
        })
        .unwrap();
        std::fs::write(&path, &future).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(store.load(), Err(JournalStoreError::Incompatible)));
        assert_eq!(std::fs::read(&path).unwrap(), future);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(store.load(), Err(JournalStoreError::Incompatible)));
        assert_eq!(std::fs::read(&path).unwrap(), future);
    }

    #[test]
    fn writable_parent_blocks_first_write_without_artifact() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let store = store(temp.path());
        assert!(matches!(
            store.save(&FakeBrokerJournal::default().snapshot()),
            Err(JournalStoreError::Incompatible)
        ));
        assert!(!temp.path().join("journal.json").exists());
    }
}
