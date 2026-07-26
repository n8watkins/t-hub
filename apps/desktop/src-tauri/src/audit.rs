//! Control-socket audit log with keyed tamper evidence and fail-closed writes.
//! This implements the durable audit requirements in
//! `docs/SOCKET-AUTH-DESIGN.md` section 6.
//!
//! Gives the aspirational `"audited": true` flag a real sink.
//! Organization-tier commands and authenticated governor refusals are recorded
//! best-effort, while ProcessChanging authorization must be recorded durably
//! before dispatch.
//! Records are JSON lines in `~/.t-hub/audit/control-YYYYMMDD.jsonl` (mode `0600`
//! on Unix).
//! Read-tier commands are not logged because they are not process-affecting and
//! would drown the signal.
//!
//! Each version 2 line is authenticated with HMAC-SHA256 under a persistent key
//! stored outside the log directory.
//! A separately authenticated head manifest anchors every day's count and final
//! hash, while the protected key state commits to the latest head generation.
//! This makes tail truncation, whole-day removal, and signed head rollback
//! detectable.
//! Process-changing commands use [`AuditLog::try_record`] before dispatch and are
//! refused when the authorization record cannot be made durable.
//! On Windows the key is DPAPI-sealed for the current user.
//! On Unix it is plaintext protected by mode `0600`, so a same-user attacker who
//! can also read the key can still forge records.
//! Removing a day from the manifest without the key invalidates the manifest MAC.
//!
//! `send_text` content is never written - only its length and a SHA-256 prefix -
//! so the log cannot become a secret-harvesting oracle.
//! `send_keys`
//!   key names ARE logged (they are exactly the kill-pattern signal we want).
//!
//! The live mirror of refusals onto the event fanout lives in `control.rs` (it
//! owns the fanout); this module owns only the durable record.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const AUDIT_FORMAT_VERSION: u64 = 2;
const HEAD_FORMAT_VERSION: u64 = 2;
const LEGACY_HEAD_FORMAT_VERSION: u64 = 1;
const KEY_STATE_FORMAT_VERSION: u64 = 1;
const COMMIT_JOURNAL_FORMAT_VERSION: u64 = 1;
const AUDIT_KEY_BYTES: usize = 32;
const VERIFY_CACHE_TTL: Duration = Duration::from_secs(5);

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve the audit directory: `$T_HUB_AUDIT_DIR` if set (dev-isolation / tests),
/// else `~/.t-hub/audit`. Mirrors `control::handshake_path`'s home resolution.
pub fn audit_dir() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_AUDIT_DIR") {
        return PathBuf::from(p);
    }
    home_dir().join(".t-hub").join("audit")
}

/// Resolve the persistent audit key-state path.
/// It holds the sealed key and latest head checkpoint outside the log directory.
pub fn audit_key_path() -> PathBuf {
    if let Ok(path) = std::env::var("T_HUB_AUDIT_KEY_FILE") {
        return PathBuf::from(path);
    }
    if std::env::var_os("T_HUB_AUDIT_DIR").is_some() {
        return key_path_for(&audit_dir());
    }
    home_dir().join(".t-hub").join("audit-hmac-key")
}

fn head_path_for(dir: &Path) -> PathBuf {
    dir.with_extension("head.json")
}

fn key_path_for(dir: &Path) -> PathBuf {
    dir.with_extension("key.json")
}

fn journal_path_for(key_path: &Path) -> PathBuf {
    key_path.with_extension("txn.json")
}

#[cfg(test)]
pub(crate) fn head_path_for_test(dir: &Path) -> PathBuf {
    head_path_for(dir)
}

#[cfg(test)]
pub(crate) fn key_path_for_test(dir: &Path) -> PathBuf {
    key_path_for(dir)
}

#[cfg(test)]
pub(crate) fn journal_path_for_test(dir: &Path) -> PathBuf {
    journal_path_for(&key_path_for(dir))
}

/// The append-only audit sink.
/// Construction remains free of filesystem access so it is safe in tests and in
/// `ControlContext::new`.
pub struct AuditLog {
    dir: PathBuf,
    head_path: PathBuf,
    key_path: PathBuf,
    journal_path: PathBuf,
    provided_key: Option<Vec<u8>>,
    inner: Mutex<Inner>,
}

struct Inner {
    writer: Option<(String, BufWriter<File>)>,
    prev_hash: String,
    count: u64,
    verified_files: Option<BTreeMap<String, VerifiedAuditFile>>,
    key_state: Option<Result<KeyState, String>>,
    poisoned: Option<String>,
    verification_cache: Option<(Instant, VerifyReport)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuditFileStamp {
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    #[cfg(windows)]
    changed: i64,
    #[cfg(windows)]
    modified: i64,
    #[cfg(not(any(unix, windows)))]
    modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedAuditFile {
    stamp: AuditFileStamp,
    content_hash: [u8; 32],
}

impl AuditFileStamp {
    fn is_exact_append_of(&self, previous: &Self, appended_len: u64) -> bool {
        self.same_file(previous)
            && previous
                .len
                .checked_add(appended_len)
                .is_some_and(|expected_len| self.len == expected_len)
    }

    fn same_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(windows)]
        {
            self.volume == other.volume && self.file_id == other.file_id
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = other;
            true
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryStamp {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

static DURABLE_DIRECTORIES: OnceLock<Mutex<HashMap<PathBuf, DirectoryStamp>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadCheckpoint {
    generation: u64,
    manifest_mac: String,
}

#[derive(Clone)]
struct KeyState {
    key: Vec<u8>,
    sealed_key: String,
    checkpoint: Option<HeadCheckpoint>,
}

struct HeadAnchor {
    generation: u64,
    entries: BTreeMap<String, Value>,
    manifest_mac: String,
}

struct PendingCommit {
    previous_checkpoint: Option<HeadCheckpoint>,
    checkpoint: HeadCheckpoint,
    record: Option<(String, String)>,
    head: Value,
}

impl AuditLog {
    /// Build a log rooted at `dir` with an ephemeral key.
    /// Tests and isolated callers that need restart verification should use
    /// [`AuditLog::with_key`] with a stable key.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_key(dir, random_key())
    }

    /// Build a log rooted at `dir` with an explicit key.
    pub fn with_key(dir: PathBuf, key: Vec<u8>) -> Self {
        let head_path = head_path_for(&dir);
        let key_path = key_path_for(&dir);
        Self {
            journal_path: journal_path_for(&key_path),
            key_path,
            dir,
            head_path,
            provided_key: Some(key),
            inner: Mutex::new(Inner {
                writer: None,
                prev_hash: String::new(),
                count: 0,
                verified_files: None,
                key_state: None,
                poisoned: None,
                verification_cache: None,
            }),
        }
    }

    /// Build a log at the default location with persistent key and checkpoint state.
    /// Key loading is delayed until verification or the first write.
    pub fn from_env() -> Self {
        let dir = audit_dir();
        let key_path = audit_key_path();
        Self {
            head_path: head_path_for(&dir),
            dir,
            journal_path: journal_path_for(&key_path),
            key_path,
            provided_key: None,
            inner: Mutex::new(Inner {
                writer: None,
                prev_hash: String::new(),
                count: 0,
                verified_files: None,
                key_state: None,
                poisoned: None,
                verification_cache: None,
            }),
        }
    }

    /// Append a best-effort audit record.
    /// Process-changing authorization must use [`AuditLog::try_record`] instead.
    pub fn record(&self, command: &str, tier: &str, decision: &str, args: &Value, meta: AuditMeta) {
        if let Err(error) = self.try_record(command, tier, decision, args, meta) {
            eprintln!("t-hub-audit: failed to write audit record for '{command}': {error}");
        }
    }

    /// Append and durably anchor one record.
    /// Any failure is returned so process-changing dispatch can fail closed.
    pub fn try_record(
        &self,
        command: &str,
        tier: &str,
        decision: &str,
        args: &Value,
        meta: AuditMeta,
    ) -> std::io::Result<()> {
        self.record_inner(command, tier, decision, args, meta)
    }

    fn record_inner(
        &self,
        command: &str,
        tier: &str,
        decision: &str,
        args: &Value,
        meta: AuditMeta,
    ) -> std::io::Result<()> {
        let now = chrono::Local::now();
        let date = now.format("%Y%m%d").to_string();
        let ts = now.to_rfc3339();

        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = &guard.poisoned {
            return Err(std::io::Error::other(format!(
                "audit sink is quarantined after an integrity or durability failure: {reason}"
            )));
        }
        let key = match self.key_for(&mut guard) {
            Ok(key) => key,
            Err(error) => {
                guard.poisoned = Some(error.to_string());
                return Err(error);
            }
        };
        if let Err(error) = self.recover_pending_commit(&mut guard, &key) {
            guard.poisoned = Some(error.to_string());
            return Err(error);
        }

        let result = self.record_locked(
            &mut guard, &key, &date, &ts, command, tier, decision, args, meta,
        );
        if let Err(error) = &result {
            guard.writer = None;
            guard.poisoned = Some(error.to_string());
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn record_locked(
        &self,
        guard: &mut Inner,
        key: &[u8],
        date: &str,
        ts: &str,
        command: &str,
        tier: &str,
        decision: &str,
        args: &Value,
        meta: AuditMeta,
    ) -> std::io::Result<()> {
        let configured_key = self.key_for(guard)?;
        if !ct_eq(&configured_key, key) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "configured audit key does not match the requested key",
            ));
        }
        let need_open = match &guard.writer {
            Some((open_date, _)) => open_date != date,
            None => true,
        };
        let opening_path = if need_open {
            guard.writer = None;
            if let Some(report) = self.initialize_checkpoint(guard, key)? {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "audit integrity verification failed with {} break(s)",
                        report.breaks.len()
                    ),
                ));
            }
            let before_verification = audit_file_snapshot(&self.dir)?;
            let report =
                verify_with_checkpoint(&self.dir, &self.head_path, key, self.checkpoint_for(guard));
            if !report.ok() {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "audit integrity verification failed with {} break(s)",
                        report.breaks.len()
                    ),
                ));
            }
            let verified_files = verified_audit_file_snapshot(&self.dir)?;
            if verified_file_stamps(&verified_files) != before_verification {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    "audit files changed during integrity verification",
                ));
            }
            guard.verified_files = Some(verified_files);
            let path = self.dir.join(format!("control-{date}.jsonl"));
            let (seed, count) = last_hash_and_count(&path)?;
            guard.prev_hash = seed;
            guard.count = count;
            Some(path)
        } else {
            if tier == "process-changing" && decision == "allowed" {
                self.validate_audit_state(guard, key)?;
            }
            None
        };

        let mut record = json!({
            "v": AUDIT_FORMAT_VERSION,
            "ts": ts,
            "command": command,
            "tier": tier,
            "decision": decision,
            "peer": meta.peer,
            "tokenTier": meta.token_tier,
            "args": redact_args(command, args),
            "prev": guard.prev_hash,
        });
        if let Some(sid) = meta.session {
            record["sessionId"] = json!(sid);
        }
        if let Some(sb) = meta.spawned_by {
            record["spawnedBy"] = json!(sb);
        }
        if let Some(err) = meta.error {
            record["outcome"] = json!("error");
            record["error"] = redact_error(command, err);
        } else if decision == "allowed" {
            if tier == "process-changing" {
                record["phase"] = json!("authorization");
            } else {
                record["outcome"] = json!("ok");
            }
        }

        let body = serde_json::to_string(&record)?;
        let hash = hex(&mac(key, body.as_bytes()));
        record["hash"] = json!(hash);
        let line = serde_json::to_string(&record)?;

        let count = guard.count + 1;
        let pending =
            self.prepare_commit(key, self.checkpoint_for(guard), date, count, &hash, &line)?;
        self.write_pending_commit(key, &pending)?;
        let path = self.dir.join(format!("control-{date}.jsonl"));
        let previous_stamp = guard
            .verified_files
            .as_ref()
            .expect("audit files snapshotted before opening the writer")
            .get(date)
            .cloned();

        if let Some(path) = opening_path {
            create_dir_all_durable(&self.dir)?;
            let file = open_private_append(&path)?;
            guard.writer = Some((date.to_string(), BufWriter::new(file)));
        }
        let (_, writer) = guard.writer.as_mut().expect("writer opened above");
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_data()?;
        let writer_stamp = audit_file_stamp_for_file(writer.get_ref())?;
        let path_stamp = audit_file_stamp(&path)?;
        let verified_file = validate_append_transition(
            previous_stamp.as_ref(),
            &writer_stamp,
            &path_stamp,
            &path,
            format!("{line}\n").as_bytes(),
        )?;

        write_private_atomic(&self.head_path, &serde_json::to_vec(&pending.head)?)?;
        self.store_checkpoint(guard, pending.checkpoint)?;
        remove_durable(&self.journal_path)?;
        guard.count = count;
        guard.prev_hash = hash;
        guard
            .verified_files
            .as_mut()
            .expect("audit files snapshotted before opening the writer")
            .insert(date.to_string(), verified_file);
        guard.verification_cache = None;
        Ok(())
    }

    fn validate_audit_state(&self, guard: &Inner, key: &[u8]) -> std::io::Result<()> {
        let Some((date, _)) = &guard.writer else {
            return Ok(());
        };
        let anchor = read_head_anchor(&self.head_path, key)?;
        validate_checkpoint(anchor.as_ref(), self.checkpoint_for(guard))?;
        let expected_files = guard.verified_files.as_ref().ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "audit file metadata was not captured during integrity verification",
            )
        })?;
        if audit_file_snapshot(&self.dir)? != verified_file_stamps(expected_files) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "audit files changed after integrity verification",
            ));
        }
        let entry = anchor
            .as_ref()
            .and_then(|anchor| anchor.entries.get(date))
            .ok_or_else(|| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "live audit file is absent from the head anchor",
                )
            })?;
        if entry.get("count").and_then(Value::as_u64) != Some(guard.count)
            || entry.get("last").and_then(Value::as_str) != Some(guard.prev_hash.as_str())
        {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "live audit writer state does not match the durable head anchor",
            ));
        }
        Ok(())
    }

    fn key_for(&self, guard: &mut Inner) -> std::io::Result<Vec<u8>> {
        if guard.key_state.is_none() {
            guard.key_state = Some(
                load_or_create_audit_key(&self.key_path, self.provided_key.as_deref())
                    .map_err(|error| error.to_string()),
            );
        }
        match guard
            .key_state
            .as_ref()
            .expect("key state initialized above")
        {
            Ok(state) => Ok(state.key.clone()),
            Err(error) => Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                error.clone(),
            )),
        }
    }

    fn checkpoint_for<'a>(&self, guard: &'a Inner) -> Option<&'a HeadCheckpoint> {
        guard
            .key_state
            .as_ref()
            .and_then(|state| state.as_ref().ok())
            .and_then(|state| state.checkpoint.as_ref())
    }

    fn store_checkpoint(
        &self,
        guard: &mut Inner,
        checkpoint: HeadCheckpoint,
    ) -> std::io::Result<()> {
        let state = guard
            .key_state
            .as_mut()
            .and_then(|state| state.as_mut().ok())
            .expect("key state initialized before writing the head");
        let previous = state.checkpoint.replace(checkpoint);
        if let Err(error) = write_key_state(&self.key_path, state) {
            state.checkpoint = previous;
            return Err(error);
        }
        Ok(())
    }

    fn prepare_commit(
        &self,
        key: &[u8],
        checkpoint: Option<&HeadCheckpoint>,
        date: &str,
        count: u64,
        last: &str,
        line: &str,
    ) -> std::io::Result<PendingCommit> {
        let anchor = read_head_anchor(&self.head_path, key)?;
        validate_checkpoint(anchor.as_ref(), checkpoint)?;
        let mut entries = anchor
            .as_ref()
            .map(|anchor| anchor.entries.clone())
            .unwrap_or_default();
        entries.insert(
            date.to_string(),
            json!({
                "count": count,
                "last": last,
                "mac": head_entry_mac(key, date, count, last),
            }),
        );
        let generation = checkpoint
            .map(|checkpoint| checkpoint.generation + 1)
            .unwrap_or(1);
        let head = head_anchor_value(key, generation, &entries);
        let manifest_mac = head["mac"]
            .as_str()
            .expect("head manifest MAC is a string")
            .to_string();
        Ok(PendingCommit {
            previous_checkpoint: checkpoint.cloned(),
            checkpoint: HeadCheckpoint {
                generation,
                manifest_mac,
            },
            record: Some((date.to_string(), line.to_string())),
            head,
        })
    }

    fn write_pending_commit(&self, key: &[u8], pending: &PendingCommit) -> std::io::Result<()> {
        let value = pending_commit_value(key, pending);
        write_private_atomic(&self.journal_path, &serde_json::to_vec(&value)?)
    }

    fn recover_pending_commit(&self, guard: &mut Inner, key: &[u8]) -> std::io::Result<()> {
        let pending = match read_pending_commit(&self.journal_path, key)? {
            Some(pending) => pending,
            None => return Ok(()),
        };
        let current = self.checkpoint_for(guard).cloned();
        if current.as_ref() != pending.previous_checkpoint.as_ref()
            && current.as_ref() != Some(&pending.checkpoint)
        {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "pending audit commit does not extend the protected checkpoint",
            ));
        }

        if let Some((date, line)) = &pending.record {
            let path = self.dir.join(format!("control-{date}.jsonl"));
            recover_record_append(&path, line)?;
        }

        write_private_atomic(&self.head_path, &serde_json::to_vec(&pending.head)?)?;
        if current.as_ref() != Some(&pending.checkpoint) {
            self.store_checkpoint(guard, pending.checkpoint.clone())?;
        }
        remove_durable(&self.journal_path)?;
        guard.writer = None;
        guard.verified_files = None;
        guard.verification_cache = None;
        Ok(())
    }

    fn initialize_checkpoint(
        &self,
        guard: &mut Inner,
        key: &[u8],
    ) -> std::io::Result<Option<VerifyReport>> {
        if self.checkpoint_for(guard).is_some() {
            return Ok(None);
        }

        let anchor = read_head_anchor(&self.head_path, key)?;
        let allow_unanchored_legacy = anchor.is_none();
        let report = verify_core_mode(
            &self.dir,
            &self.head_path,
            key,
            None,
            allow_unanchored_legacy,
        );
        if !report.ok() {
            return Ok(Some(report));
        }

        let entries = match anchor {
            Some(anchor) => anchor.entries,
            None => legacy_manifest_entries(&self.dir, key)?,
        };
        let head = head_anchor_value(key, 1, &entries);
        let manifest_mac = head["mac"]
            .as_str()
            .expect("head manifest MAC is a string")
            .to_string();
        let pending = PendingCommit {
            previous_checkpoint: None,
            checkpoint: HeadCheckpoint {
                generation: 1,
                manifest_mac,
            },
            record: None,
            head,
        };
        self.write_pending_commit(key, &pending)?;
        write_private_atomic(&self.head_path, &serde_json::to_vec(&pending.head)?)?;
        self.store_checkpoint(guard, pending.checkpoint)?;
        remove_durable(&self.journal_path)?;
        Ok(None)
    }

    /// Verify this audit directory and its external head with the configured key.
    pub fn verify_self(&self) -> VerifyReport {
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        self.verify_locked(&mut guard, false)
    }

    pub fn verify_self_cached(&self) -> VerifyReport {
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        self.verify_locked(&mut guard, true)
    }

    fn verify_locked(&self, guard: &mut Inner, allow_cached: bool) -> VerifyReport {
        if allow_cached {
            if let Some((checked_at, report)) = &guard.verification_cache {
                if checked_at.elapsed() < VERIFY_CACHE_TTL {
                    return report.clone();
                }
            }
        }
        let report = match self.key_for(guard) {
            Ok(key) => match self
                .recover_pending_commit(guard, &key)
                .and_then(|()| self.initialize_checkpoint(guard, &key))
            {
                Ok(Some(report)) => report,
                Ok(None) => verify_with_checkpoint(
                    &self.dir,
                    &self.head_path,
                    &key,
                    self.checkpoint_for(guard),
                ),
                Err(error) => VerifyReport {
                    breaks: vec![ChainBreak {
                        file: self.head_path.display().to_string(),
                        line: 0,
                        kind: BreakKind::HeadTampered,
                        detail: format!("cannot initialize the audit head anchor: {error}"),
                    }],
                    ..VerifyReport::default()
                },
            },
            Err(error) => VerifyReport {
                breaks: vec![ChainBreak {
                    file: self.key_path.display().to_string(),
                    line: 0,
                    kind: BreakKind::KeyUnavailable,
                    detail: error.to_string(),
                }],
                ..VerifyReport::default()
            },
        };
        if !report.ok() {
            guard.writer = None;
            guard.poisoned = Some(format!(
                "integrity verification found {} break(s)",
                report.breaks.len()
            ));
        }
        guard.verification_cache = Some((Instant::now(), report.clone()));
        report
    }

    /// Verify at startup and report failures.
    /// Read-only control remains available so operators can query the report.
    pub fn startup_integrity_check(&self) -> VerifyReport {
        let report = self.verify_self();
        if !report.ok() {
            eprintln!(
                "t-hub-audit: INTEGRITY CHECK FAILED: {} break(s) across {} record(s) in {} file(s)",
                report.breaks.len(),
                report.records,
                report.files
            );
            for audit_break in &report.breaks {
                eprintln!(
                    "  {} line {}: {}: {}",
                    audit_break.file,
                    audit_break.line,
                    audit_break.kind.label(),
                    audit_break.detail
                );
            }
        } else if report.legacy > 0 {
            eprintln!(
                "t-hub-audit: integrity OK; {} valid legacy pre-v2 record(s) are unverifiable by the keyed scheme",
                report.legacy
            );
        }
        report
    }
}

fn redact_error(_command: &str, error: &str) -> Value {
    json!(error)
}

/// Caller context + dispatch outcome attached to an audit record. Kept separate
/// from the command args so the call site reads clearly.
pub struct AuditMeta<'a> {
    /// `"loopback"` or `"remote"` - the connection origin (`ControlContext::peer_is_loopback`).
    pub peer: &'a str,
    /// The capability tier resolved from the presented token.
    pub token_tier: &'a str,
    /// The target session id, when the command names one (send/close).
    pub session: Option<&'a str>,
    /// The `spawnedBy` captain id, when present (spawn).
    pub spawned_by: Option<&'a str>,
    /// The dispatch error, when an allowed command failed downstream.
    pub error: Option<&'a str>,
}

/// Redact an args object for the audit log. `send_text` content is replaced by a
/// length + SHA-256 prefix (never the literal text); `send_keys` names are kept;
/// `spawn_terminal` logs only the presence of a `startupCommand` (arbitrary shell
/// text, same secret risk as `send_text`). Other commands' args are small
/// identifiers (tab/session ids) and pass through as-is.
fn redact_args(command: &str, args: &Value) -> Value {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str());
    match command {
        "send_text" => {
            let text = s("text").unwrap_or("");
            json!({
                "sessionId": s("sessionId").or_else(|| s("session_id")),
                "enter": args.get("enter").and_then(|v| v.as_bool()).unwrap_or(true),
                "textLen": text.len(),
                "textSha256": &hex(&Sha256::digest(text.as_bytes()))[..16],
            })
        }
        "spawn_terminal" => json!({
            "cwd": s("cwd"),
            "name": s("name"),
            "shell": s("shell"),
            "tabId": s("tabId").or_else(|| s("tab_id")),
            "tabName": s("tabName").or_else(|| s("tab_name")),
            "spawnedBy": s("spawnedBy").or_else(|| s("spawned_by")),
            "hasStartupCommand": args.get("startupCommand").or_else(|| args.get("startup_command")).is_some(),
        }),
        // send_keys, close_terminal, and the Organization commands carry only
        // non-sensitive identifiers / key names - log them verbatim.
        _ => args.clone(),
    }
}

fn audit_file_snapshot(dir: &Path) -> std::io::Result<BTreeMap<String, AuditFileStamp>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error),
    };
    let mut files = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(date) = name
            .strip_prefix("control-")
            .and_then(|value| value.strip_suffix(".jsonl"))
        else {
            continue;
        };
        files.insert(date.to_string(), audit_file_stamp(&path)?);
    }
    Ok(files)
}

fn verified_audit_file_snapshot(
    dir: &Path,
) -> std::io::Result<BTreeMap<String, VerifiedAuditFile>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error),
    };
    let mut files = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(date) = name
            .strip_prefix("control-")
            .and_then(|value| value.strip_suffix(".jsonl"))
        else {
            continue;
        };
        files.insert(date.to_string(), verified_audit_file(&path)?);
    }
    Ok(files)
}

fn verified_file_stamps(
    files: &BTreeMap<String, VerifiedAuditFile>,
) -> BTreeMap<String, AuditFileStamp> {
    files
        .iter()
        .map(|(date, file)| (date.clone(), file.stamp.clone()))
        .collect()
}

fn verified_audit_file(path: &Path) -> std::io::Result<VerifiedAuditFile> {
    let path_stamp_before = audit_file_stamp(path)?;
    let mut file = File::open(path)?;
    let file_stamp_before = audit_file_stamp_for_file(&file)?;
    if path_stamp_before != file_stamp_before {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit file changed while capturing authenticated content",
        ));
    }
    let content_hash = hash_reader(&mut file)?;
    let file_stamp_after = audit_file_stamp_for_file(&file)?;
    let path_stamp_after = audit_file_stamp(path)?;
    if file_stamp_after != file_stamp_before || path_stamp_after != file_stamp_before {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit file changed while capturing authenticated content",
        ));
    }
    Ok(VerifiedAuditFile {
        stamp: file_stamp_after,
        content_hash,
    })
}

fn hash_reader(reader: &mut impl Read) -> std::io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finalize().into());
        }
        hasher.update(&buffer[..read]);
    }
}

fn audit_file_stamp(path: &Path) -> std::io::Result<AuditFileStamp> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("audit path is not a regular file: {}", path.display()),
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(AuditFileStamp {
            len: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(windows)]
    {
        let file = File::open(path)?;
        audit_file_stamp_for_file(&file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(AuditFileStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

fn audit_file_stamp_for_file(file: &File) -> std::io::Result<AuditFileStamp> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit writer does not reference a regular file",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(AuditFileStamp {
            len: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
        };

        let handle = HANDLE(file.as_raw_handle());
        let mut basic = FILE_BASIC_INFO::default();
        let mut identity = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileBasicInfo,
                std::ptr::from_mut(&mut basic).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
            .map_err(std::io::Error::other)?;
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                std::ptr::from_mut(&mut identity).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(AuditFileStamp {
            len: metadata.len(),
            volume: identity.VolumeSerialNumber,
            file_id: identity.FileId.Identifier,
            changed: basic.ChangeTime,
            modified: basic.LastWriteTime,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(AuditFileStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

fn validate_append_transition(
    previous: Option<&VerifiedAuditFile>,
    writer: &AuditFileStamp,
    path: &AuditFileStamp,
    audit_path: &Path,
    appended: &[u8],
) -> std::io::Result<VerifiedAuditFile> {
    let appended_len = u64::try_from(appended.len())
        .map_err(|_| std::io::Error::other("audit record length overflow"))?;
    let transition_valid = match previous {
        Some(previous) => writer.is_exact_append_of(&previous.stamp, appended_len),
        None => writer.len == appended_len,
    };
    if !transition_valid || path != writer {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "live audit file did not make the expected append transition",
        ));
    }

    let mut file = File::open(audit_path)?;
    if audit_file_stamp_for_file(&file)? != *writer {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "live audit file changed before append validation",
        ));
    }
    let previous_len = previous.map_or(0, |previous| previous.stamp.len);
    let mut prefix_hasher = Sha256::new();
    let mut remaining = previous_len;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded audit read size fits usize");
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "live audit file ended inside its authenticated prefix",
            ));
        }
        prefix_hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let mut full_hasher = prefix_hasher.clone();
    let prefix_hash: [u8; 32] = prefix_hasher.finalize().into();
    if previous.is_some_and(|previous| previous.content_hash != prefix_hash) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "live audit file authenticated prefix changed before append",
        ));
    }
    let mut actual_append = vec![0_u8; appended.len()];
    file.read_exact(&mut actual_append)?;
    if actual_append != appended {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "live audit file does not contain the expected appended record",
        ));
    }
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing)? != 0 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "live audit file contains data after the expected append",
        ));
    }
    full_hasher.update(appended);
    let file_stamp_after = audit_file_stamp_for_file(&file)?;
    let path_stamp_after = audit_file_stamp(audit_path)?;
    if file_stamp_after != *writer || path_stamp_after != *writer {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "live audit file changed during append validation",
        ));
    }
    Ok(VerifiedAuditFile {
        stamp: file_stamp_after,
        content_hash: full_hasher.finalize().into(),
    })
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    pub files: usize,
    pub records: usize,
    pub legacy: usize,
    pub breaks: Vec<ChainBreak>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.breaks.is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok(),
            "files": self.files,
            "records": self.records,
            "legacy": self.legacy,
            "breaks": self.breaks.iter().map(|audit_break| json!({
                "file": audit_break.file,
                "line": audit_break.line,
                "kind": audit_break.kind.label(),
                "detail": audit_break.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct ChainBreak {
    pub file: String,
    pub line: usize,
    pub kind: BreakKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakKind {
    BadMac,
    PrevMismatch,
    Malformed,
    Truncated,
    HeadTampered,
    HeadMissing,
    HeadMismatch,
    MissingFile,
    KeyUnavailable,
}

impl BreakKind {
    fn label(self) -> &'static str {
        match self {
            Self::BadMac => "bad_mac",
            Self::PrevMismatch => "prev_mismatch",
            Self::Malformed => "malformed",
            Self::Truncated => "truncated",
            Self::HeadTampered => "head_tampered",
            Self::HeadMissing => "head_missing",
            Self::HeadMismatch => "head_mismatch",
            Self::MissingFile => "missing_file",
            Self::KeyUnavailable => "key_unavailable",
        }
    }
}

#[cfg(test)]
pub fn verify(dir: &Path, key: &[u8]) -> VerifyReport {
    let key_path = key_path_for(dir);
    let checkpoint = match std::fs::read_to_string(&key_path) {
        Ok(raw) => match decode_audit_key(&key_path, &raw, Some(key)) {
            Ok(state) => state.checkpoint,
            Err(error) => {
                return VerifyReport {
                    breaks: vec![ChainBreak {
                        file: key_path.display().to_string(),
                        line: 0,
                        kind: BreakKind::KeyUnavailable,
                        detail: error.to_string(),
                    }],
                    ..VerifyReport::default()
                };
            }
        },
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return VerifyReport {
                breaks: vec![ChainBreak {
                    file: key_path.display().to_string(),
                    line: 0,
                    kind: BreakKind::KeyUnavailable,
                    detail: error.to_string(),
                }],
                ..VerifyReport::default()
            };
        }
    };
    verify_with_checkpoint(dir, &head_path_for(dir), key, checkpoint.as_ref())
}

pub fn verify_with_head(dir: &Path, head_path: &Path, key: &[u8]) -> VerifyReport {
    verify_core(dir, head_path, key, None)
}

fn verify_with_checkpoint(
    dir: &Path,
    head_path: &Path,
    key: &[u8],
    checkpoint: Option<&HeadCheckpoint>,
) -> VerifyReport {
    if checkpoint.is_none() {
        return verify_with_head(dir, head_path, key);
    }
    verify_core(dir, head_path, key, checkpoint)
}

fn verify_core(
    dir: &Path,
    head_path: &Path,
    key: &[u8],
    checkpoint: Option<&HeadCheckpoint>,
) -> VerifyReport {
    verify_core_mode(dir, head_path, key, checkpoint, false)
}

fn verify_core_mode(
    dir: &Path,
    head_path: &Path,
    key: &[u8],
    checkpoint: Option<&HeadCheckpoint>,
    allow_unanchored_legacy: bool,
) -> VerifyReport {
    let mut report = VerifyReport::default();
    let anchor = match read_head_anchor(head_path, key) {
        Ok(anchor) => anchor,
        Err(error) => {
            report.breaks.push(ChainBreak {
                file: head_path.display().to_string(),
                line: 0,
                kind: BreakKind::HeadTampered,
                detail: format!("cannot read or parse the head anchor: {error}"),
            });
            None
        }
    };
    if let Err(error) = validate_checkpoint(anchor.as_ref(), checkpoint) {
        report.breaks.push(ChainBreak {
            file: head_path.display().to_string(),
            line: 0,
            kind: if anchor.is_none() {
                BreakKind::HeadMissing
            } else {
                BreakKind::HeadTampered
            },
            detail: error.to_string(),
        });
    }
    let head = anchor.as_ref().map(|anchor| &anchor.entries);

    let mut files = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        report.breaks.push(ChainBreak {
                            file: dir.display().to_string(),
                            line: 0,
                            kind: BreakKind::Malformed,
                            detail: format!("cannot enumerate audit directory entry: {error}"),
                        });
                        continue;
                    }
                };
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Some(date) = name
                    .strip_prefix("control-")
                    .and_then(|value| value.strip_suffix(".jsonl"))
                else {
                    continue;
                };
                files.push((date.to_string(), path));
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            report.breaks.push(ChainBreak {
                file: dir.display().to_string(),
                line: 0,
                kind: BreakKind::Malformed,
                detail: format!("cannot read audit directory: {error}"),
            });
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut seen_dates = BTreeSet::new();
    for (date, path) in files {
        seen_dates.insert(date.clone());
        report.files += 1;
        verify_file(
            &date,
            &path,
            head,
            key,
            allow_unanchored_legacy,
            &mut report,
        );
    }

    if let Some(head) = head {
        for date in head.keys().filter(|date| !seen_dates.contains(*date)) {
            report.breaks.push(ChainBreak {
                file: format!("control-{date}.jsonl"),
                line: 0,
                kind: BreakKind::MissingFile,
                detail: "the head anchor names a day file that is missing".into(),
            });
        }
    }

    report
}

fn verify_file(
    date: &str,
    path: &Path,
    head: Option<&BTreeMap<String, Value>>,
    key: &[u8],
    allow_unanchored_legacy: bool,
    report: &mut VerifyReport,
) {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("?")
        .to_string();
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            report.breaks.push(ChainBreak {
                file: name,
                line: 0,
                kind: BreakKind::Malformed,
                detail: format!("cannot read file: {error}"),
            });
            return;
        }
    };
    if !content.is_empty() && !content.ends_with('\n') {
        report.breaks.push(ChainBreak {
            file: name,
            line: content.lines().count(),
            kind: BreakKind::Truncated,
            detail: "nonempty audit file does not end with a newline".into(),
        });
        return;
    }

    let mut prev = String::new();
    let mut count = 0_u64;
    let mut v2_count = 0_u64;
    let mut last_hash = String::new();
    for (index, raw) in content.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        report.records += 1;
        count += 1;
        let line = index + 1;
        let record: Value = match serde_json::from_str::<Value>(raw) {
            Ok(record) if record.is_object() => record,
            Ok(_) => {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line,
                    kind: BreakKind::Malformed,
                    detail: "record is not a JSON object".into(),
                });
                continue;
            }
            Err(error) => {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line,
                    kind: BreakKind::Malformed,
                    detail: format!("unparseable JSON: {error}"),
                });
                continue;
            }
        };
        let Some(stored) = record.get("hash").and_then(Value::as_str) else {
            report.breaks.push(ChainBreak {
                file: name.clone(),
                line,
                kind: BreakKind::Malformed,
                detail: "record has no string `hash` field".into(),
            });
            continue;
        };
        let stored = stored.to_string();

        let mut body = record.clone();
        body.as_object_mut()
            .expect("object checked above")
            .remove("hash");
        let body = match serde_json::to_vec(&body) {
            Ok(body) => body,
            Err(error) => {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line,
                    kind: BreakKind::Malformed,
                    detail: format!("cannot canonicalize record: {error}"),
                });
                continue;
            }
        };

        if record.get("v").and_then(Value::as_u64) == Some(AUDIT_FORMAT_VERSION) {
            v2_count += 1;
            let valid = unhex(&stored).is_some_and(|tag| verify_mac(key, &body, &tag));
            if !valid {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line,
                    kind: BreakKind::BadMac,
                    detail: "HMAC does not verify".into(),
                });
            }
        } else {
            let expected = hex(&Sha256::digest(&body));
            if !ct_eq(stored.as_bytes(), expected.as_bytes()) {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line,
                    kind: BreakKind::BadMac,
                    detail: "legacy SHA-256 record hash does not verify".into(),
                });
            } else {
                report.legacy += 1;
            }
        }

        let record_prev = record.get("prev").and_then(Value::as_str).unwrap_or("");
        if record_prev != prev {
            report.breaks.push(ChainBreak {
                file: name.clone(),
                line,
                kind: BreakKind::PrevMismatch,
                detail: format!("`prev` {record_prev:?} does not match prior hash {prev:?}"),
            });
        }
        prev = stored.clone();
        last_hash = stored;
    }

    match head.and_then(|head| head.get(date)) {
        Some(entry) => verify_head_entry(date, &name, entry, count, &last_hash, key, report),
        None if v2_count > 0 => report.breaks.push(ChainBreak {
            file: name,
            line: 0,
            kind: BreakKind::HeadMissing,
            detail: "keyed records exist without a head anchor entry".into(),
        }),
        None if allow_unanchored_legacy => {}
        None => report.breaks.push(ChainBreak {
            file: name,
            line: 0,
            kind: BreakKind::HeadMissing,
            detail: "legacy records exist without a head anchor entry".into(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_head_entry(
    date: &str,
    name: &str,
    entry: &Value,
    count: u64,
    last_hash: &str,
    key: &[u8],
    report: &mut VerifyReport,
) {
    let anchored_count = entry.get("count").and_then(Value::as_u64);
    let anchored_last = entry.get("last").and_then(Value::as_str);
    let anchored_mac = entry.get("mac").and_then(Value::as_str);
    let (Some(anchored_count), Some(anchored_last), Some(anchored_mac)) =
        (anchored_count, anchored_last, anchored_mac)
    else {
        report.breaks.push(ChainBreak {
            file: name.to_string(),
            line: 0,
            kind: BreakKind::HeadTampered,
            detail: "head anchor entry is malformed".into(),
        });
        return;
    };

    let expected_mac = head_entry_mac(key, date, anchored_count, anchored_last);
    if !ct_eq(anchored_mac.as_bytes(), expected_mac.as_bytes()) {
        report.breaks.push(ChainBreak {
            file: name.to_string(),
            line: 0,
            kind: BreakKind::HeadTampered,
            detail: "head anchor MAC does not verify".into(),
        });
        return;
    }
    if count < anchored_count {
        report.breaks.push(ChainBreak {
            file: name.to_string(),
            line: 0,
            kind: BreakKind::Truncated,
            detail: format!("file has {count} record(s), but the head anchors {anchored_count}"),
        });
    } else if count > anchored_count {
        report.breaks.push(ChainBreak {
            file: name.to_string(),
            line: 0,
            kind: BreakKind::HeadMismatch,
            detail: format!(
                "file has {count} record(s), but the head anchors only {anchored_count}"
            ),
        });
    } else if last_hash != anchored_last {
        report.breaks.push(ChainBreak {
            file: name.to_string(),
            line: 0,
            kind: BreakKind::HeadMismatch,
            detail: "final record hash does not match the head anchor".into(),
        });
    }
}

// ---------------------------------------------------------------------------
// Persistence and crypto helpers
// ---------------------------------------------------------------------------

fn pending_commit_body(pending: &PendingCommit) -> Value {
    json!({
        "v": COMMIT_JOURNAL_FORMAT_VERSION,
        "previousCheckpoint": pending.previous_checkpoint.as_ref().map(checkpoint_value),
        "checkpoint": checkpoint_value(&pending.checkpoint),
        "record": pending.record.as_ref().map(|(date, line)| json!({
            "date": date,
            "line": line,
        })),
        "head": pending.head,
    })
}

fn pending_commit_value(key: &[u8], pending: &PendingCommit) -> Value {
    let mut body = pending_commit_body(pending);
    let tag = hex(&mac(
        key,
        &serde_json::to_vec(&body).expect("pending commit is serializable"),
    ));
    body["mac"] = json!(tag);
    body
}

fn read_pending_commit(path: &Path, key: &[u8]) -> std::io::Result<Option<PendingCommit>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    set_owner_only(path)?;
    let mut value = serde_json::from_str::<Value>(&raw)?;
    let object = value.as_object().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "audit commit journal is not an object",
        )
    })?;
    if object.len() != 6
        || object.get("v").and_then(Value::as_u64) != Some(COMMIT_JOURNAL_FORMAT_VERSION)
    {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit commit journal has an unsupported format",
        ));
    }
    let stored_mac = object
        .get("mac")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidData, "audit commit journal has no MAC")
        })?
        .to_string();
    value
        .as_object_mut()
        .expect("object checked above")
        .remove("mac");
    let expected_mac = hex(&mac(key, &serde_json::to_vec(&value)?));
    if !ct_eq(stored_mac.as_bytes(), expected_mac.as_bytes()) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit commit journal MAC does not verify",
        ));
    }

    let previous_checkpoint = value
        .get("previousCheckpoint")
        .filter(|checkpoint| !checkpoint.is_null())
        .map(decode_plain_checkpoint)
        .transpose()?;
    let checkpoint = decode_plain_checkpoint(value.get("checkpoint").ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "audit commit journal has no checkpoint",
        )
    })?)?;
    let head = value.get("head").cloned().ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidData, "audit commit journal has no head")
    })?;
    let anchor = decode_head_anchor_value(&head, key)?;
    if anchor.generation != checkpoint.generation
        || !ct_eq(
            anchor.manifest_mac.as_bytes(),
            checkpoint.manifest_mac.as_bytes(),
        )
    {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit commit journal head does not match its checkpoint",
        ));
    }
    let record = value
        .get("record")
        .filter(|record| !record.is_null())
        .map(|record| {
            let date = record
                .get("date")
                .and_then(Value::as_str)
                .filter(|date| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
                .ok_or_else(|| {
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        "audit commit journal has an invalid record date",
                    )
                })?
                .to_string();
            let line = record
                .get("line")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        "audit commit journal has no record line",
                    )
                })?
                .to_string();
            validate_pending_record(&date, &line, &anchor, key)?;
            Ok::<_, std::io::Error>((date, line))
        })
        .transpose()?;
    Ok(Some(PendingCommit {
        previous_checkpoint,
        checkpoint,
        record,
        head,
    }))
}

fn checkpoint_value(checkpoint: &HeadCheckpoint) -> Value {
    json!({
        "generation": checkpoint.generation,
        "manifestMac": checkpoint.manifest_mac,
    })
}

fn decode_plain_checkpoint(value: &Value) -> std::io::Result<HeadCheckpoint> {
    let object = value.as_object().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "journal checkpoint is not an object",
        )
    })?;
    if object.len() != 2 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "journal checkpoint has unexpected fields",
        ));
    }
    Ok(HeadCheckpoint {
        generation: object
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "journal checkpoint has no generation",
                )
            })?,
        manifest_mac: object
            .get("manifestMac")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "journal checkpoint has no manifest MAC",
                )
            })?
            .to_string(),
    })
}

fn validate_pending_record(
    date: &str,
    line: &str,
    anchor: &HeadAnchor,
    key: &[u8],
) -> std::io::Result<()> {
    let mut record = serde_json::from_str::<Value>(line)?;
    let object = record.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "pending audit record is not an object",
        )
    })?;
    let stored_hash = object
        .remove("hash")
        .and_then(|hash| hash.as_str().map(str::to_string))
        .ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidData, "pending audit record has no hash")
        })?;
    if record.get("v").and_then(Value::as_u64) != Some(AUDIT_FORMAT_VERSION)
        || !unhex(&stored_hash).is_some_and(|tag| {
            verify_mac(key, &serde_json::to_vec(&record).unwrap_or_default(), &tag)
        })
    {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "pending audit record MAC does not verify",
        ));
    }
    let entry = anchor.entries.get(date).ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "pending audit record is absent from its head",
        )
    })?;
    if entry.get("last").and_then(Value::as_str) != Some(stored_hash.as_str()) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "pending audit record does not match its head",
        ));
    }
    Ok(())
}

fn recover_record_append(path: &Path, line: &str) -> std::io::Result<()> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let mut complete_line = line.as_bytes().to_vec();
    complete_line.push(b'\n');
    if content.ends_with(&complete_line) {
        return Ok(());
    }

    let complete_len = content
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let partial = &content[complete_len..];
    if !line.as_bytes().starts_with(partial) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit log tail does not match the pending commit",
        ));
    }
    let previous_hash = serde_json::from_str::<Value>(line)?
        .get("prev")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let complete = &content[..complete_len];
    let last = complete
        .split(|byte| *byte == b'\n')
        .rev()
        .find(|candidate| !candidate.is_empty())
        .map(|candidate| serde_json::from_slice::<Value>(candidate))
        .transpose()?
        .and_then(|record| {
            record
                .get("hash")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if last != previous_hash {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "pending audit record does not extend the durable log tail",
        ));
    }

    if let Some(parent) = path.parent() {
        create_dir_all_durable(parent)?;
    }
    let mut file = open_private_append(path)?;
    file.set_len(complete_len as u64)?;
    file.write_all(&complete_line)?;
    file.sync_data()?;
    Ok(())
}

fn random_key() -> Vec<u8> {
    let mut key = Vec::with_capacity(AUDIT_KEY_BYTES);
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    key
}

fn load_or_create_audit_key(path: &Path, provided_key: Option<&[u8]>) -> std::io::Result<KeyState> {
    match std::fs::read_to_string(path) {
        Ok(raw) => decode_audit_key(path, &raw, provided_key),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let key = provided_key.map_or_else(random_key, <[u8]>::to_vec);
            let sealed = crate::secret_seal::seal_str(&hex(&key));
            if crate::secret_seal::sealing_active() && !crate::secret_seal::is_sealed(&sealed) {
                return Err(std::io::Error::other(
                    "DPAPI failed to seal the new audit key",
                ));
            }
            let state = KeyState {
                key,
                sealed_key: sealed,
                checkpoint: None,
            };
            let body = serde_json::to_vec(&key_state_value(&state))?;
            match write_private_create_new(path, &body) {
                Ok(()) => Ok(state),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    let raw = std::fs::read_to_string(path)?;
                    decode_audit_key(path, &raw, provided_key)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn decode_audit_key(
    path: &Path,
    raw: &str,
    provided_key: Option<&[u8]>,
) -> std::io::Result<KeyState> {
    set_owner_only(path)?;
    let parsed = serde_json::from_str::<Value>(raw).ok();
    let (sealed_key, checkpoint) = if let Some(object) = parsed.as_ref().and_then(Value::as_object)
    {
        if object.get("v").and_then(Value::as_u64) != Some(KEY_STATE_FORMAT_VERSION) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "audit key state has an unsupported format",
            ));
        }
        let sealed_key = object
            .get("sealedKey")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                std::io::Error::new(ErrorKind::InvalidData, "audit key state has no sealed key")
            })?
            .to_string();
        (
            sealed_key,
            object
                .get("checkpoint")
                .filter(|checkpoint| !checkpoint.is_null())
                .cloned(),
        )
    } else {
        (raw.trim().to_string(), None)
    };
    let unsealed = crate::secret_seal::unseal_str(&sealed_key).ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("audit key at {} cannot be unsealed", path.display()),
        )
    })?;
    let key = unhex(unsealed.trim()).ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("audit key at {} is not valid hex", path.display()),
        )
    })?;
    if key.len() != AUDIT_KEY_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "audit key at {} has {} bytes, expected {AUDIT_KEY_BYTES}",
                path.display(),
                key.len()
            ),
        ));
    }
    if provided_key.is_some_and(|provided| !ct_eq(provided, &key)) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "audit key at {} does not match the configured key",
                path.display()
            ),
        ));
    }
    let checkpoint = checkpoint
        .map(|checkpoint| decode_checkpoint(&checkpoint, &key))
        .transpose()?;
    Ok(KeyState {
        key,
        sealed_key,
        checkpoint,
    })
}

fn write_key_state(path: &Path, state: &KeyState) -> std::io::Result<()> {
    let body = serde_json::to_vec(&key_state_value(state))?;
    write_private_atomic(path, &body)
}

fn key_state_value(state: &KeyState) -> Value {
    json!({
        "v": KEY_STATE_FORMAT_VERSION,
        "sealedKey": state.sealed_key,
        "checkpoint": state.checkpoint.as_ref().map(|checkpoint| json!({
            "generation": checkpoint.generation,
            "manifestMac": checkpoint.manifest_mac,
            "mac": checkpoint_mac(
                &state.key,
                checkpoint.generation,
                &checkpoint.manifest_mac,
            ),
        })),
    })
}

fn decode_checkpoint(value: &Value, key: &[u8]) -> std::io::Result<HeadCheckpoint> {
    let generation = value
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidData, "audit checkpoint has no generation")
        })?;
    let manifest_mac = value
        .get("manifestMac")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "audit checkpoint has no manifest MAC",
            )
        })?
        .to_string();
    let stored_mac = value.get("mac").and_then(Value::as_str).ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidData, "audit checkpoint has no MAC")
    })?;
    let expected_mac = checkpoint_mac(key, generation, &manifest_mac);
    if !ct_eq(stored_mac.as_bytes(), expected_mac.as_bytes()) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "audit checkpoint MAC does not verify",
        ));
    }
    Ok(HeadCheckpoint {
        generation,
        manifest_mac,
    })
}

fn open_private_append(path: &Path) -> std::io::Result<File> {
    let mut create_options = OpenOptions::new();
    create_options.create_new(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create_options.mode(0o600);
    }
    let (file, created) = match create_options.open(path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            (OpenOptions::new().append(true).open(path)?, false)
        }
        Err(error) => return Err(error),
    };
    set_owner_only(path)?;
    if created {
        file.sync_all()?;
        sync_parent(path.parent().unwrap_or_else(|| Path::new(".")))?;
    }
    Ok(file)
}

fn write_private_create_new(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_all_durable(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body)?;
    file.sync_all()?;
    sync_parent(parent)?;
    Ok(())
}

fn write_private_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_all_durable(parent)?;
    let temp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        write_private_create_new(&temp, body)?;
        replace_file(&temp, path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn remove_durable(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_parent(path.parent().unwrap_or_else(|| Path::new("."))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn create_dir_all_durable(path: &Path) -> std::io::Result<()> {
    let cache = DURABLE_DIRECTORIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    create_dir_all_durable_cached(path, &mut cache)
}

fn create_dir_all_durable_cached(
    path: &Path,
    cache: &mut HashMap<PathBuf, DirectoryStamp>,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let stamp = directory_stamp(&metadata);
            if cache.get(path) == Some(&stamp) {
                return Ok(());
            }
            if parent != path {
                create_dir_all_durable_cached(parent, cache)?;
            }
            sync_parent(parent)?;
            cache.insert(path.to_path_buf(), stamp);
            return Ok(());
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!("{} exists and is not a directory", path.display()),
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    if parent != path {
        create_dir_all_durable_cached(parent, cache)?;
    }

    match std::fs::create_dir(path) {
        Ok(()) => {
            sync_parent(parent)?;
            cache.insert(
                path.to_path_buf(),
                directory_stamp(&std::fs::metadata(path)?),
            );
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata = std::fs::metadata(path)?;
            if metadata.is_dir() {
                sync_parent(parent)?;
                cache.insert(path.to_path_buf(), directory_stamp(&metadata));
                Ok(())
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

fn directory_stamp(metadata: &std::fs::Metadata) -> DirectoryStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        DirectoryStamp {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        DirectoryStamp {
            modified: metadata.modified().ok(),
        }
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> std::io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> std::io::Result<()> {
    Ok(())
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

fn set_owner_only(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn last_hash_and_count(path: &Path) -> std::io::Result<(String, u64)> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok((String::new(), 0)),
        Err(error) => return Err(error),
    };
    let reader = BufReader::new(file);
    let mut last = None;
    let mut count = 0_u64;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(&line).map_err(|error| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                format!("cannot seed from malformed audit record: {error}"),
            )
        })?;
        let hash = record.get("hash").and_then(Value::as_str).ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "cannot seed from audit record without a string `hash`",
            )
        })?;
        count += 1;
        last = Some(hash.to_string());
    }
    Ok((last.unwrap_or_default(), count))
}

fn legacy_manifest_entries(dir: &Path, key: &[u8]) -> std::io::Result<BTreeMap<String, Value>> {
    let mut entries = BTreeMap::new();
    let directory = match std::fs::read_dir(dir) {
        Ok(directory) => directory,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(error),
    };
    for entry in directory {
        let path = entry?.path();
        let Some(date) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("control-"))
            .and_then(|name| name.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let (last, count) = last_hash_and_count(&path)?;
        let entry_mac = head_entry_mac(key, date, count, &last);
        entries.insert(
            date.to_string(),
            json!({
                "count": count,
                "last": last,
                "mac": entry_mac,
            }),
        );
    }
    Ok(entries)
}

fn read_head_anchor(path: &Path, key: &[u8]) -> std::io::Result<Option<HeadAnchor>> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let anchor = serde_json::from_str::<Value>(&body)?;
    decode_head_anchor_value(&anchor, key).map(Some)
}

fn decode_head_anchor_value(anchor: &Value, key: &[u8]) -> std::io::Result<HeadAnchor> {
    let object = anchor.as_object().ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidData, "head anchor is not a JSON object")
    })?;
    let version = object.get("v").and_then(Value::as_u64);
    let generation = match version {
        Some(HEAD_FORMAT_VERSION) if object.len() == 4 => object
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                std::io::Error::new(ErrorKind::InvalidData, "head anchor has no generation")
            })?,
        Some(LEGACY_HEAD_FORMAT_VERSION) if object.len() == 3 => 0,
        _ => {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "head anchor has an unsupported format",
            ));
        }
    };
    if generation == 0 && version == Some(HEAD_FORMAT_VERSION) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "head anchor generation must be positive",
        ));
    }
    let entries = object
        .get("entries")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "head anchor entries are not a JSON object",
            )
        })?
        .iter()
        .map(|(date, entry)| (date.clone(), entry.clone()))
        .collect::<BTreeMap<_, _>>();
    let stored_mac = object.get("mac").and_then(Value::as_str).ok_or_else(|| {
        std::io::Error::new(ErrorKind::InvalidData, "head anchor has no manifest MAC")
    })?;
    let expected_mac = head_manifest_mac(key, version.unwrap(), generation, &entries);
    if !ct_eq(stored_mac.as_bytes(), expected_mac.as_bytes()) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "head anchor manifest MAC does not verify",
        ));
    }
    Ok(HeadAnchor {
        generation,
        entries,
        manifest_mac: stored_mac.to_string(),
    })
}

fn validate_checkpoint(
    anchor: Option<&HeadAnchor>,
    checkpoint: Option<&HeadCheckpoint>,
) -> std::io::Result<()> {
    match (anchor, checkpoint) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "the protected audit checkpoint exists but the head anchor is missing",
        )),
        (Some(anchor), None) if anchor.generation == 0 => Ok(()),
        (Some(_), None) => Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "the versioned head anchor has no protected audit checkpoint",
        )),
        (Some(anchor), Some(checkpoint))
            if anchor.generation == checkpoint.generation
                && ct_eq(
                    anchor.manifest_mac.as_bytes(),
                    checkpoint.manifest_mac.as_bytes(),
                ) =>
        {
            Ok(())
        }
        (Some(anchor), Some(checkpoint)) => Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "head anchor generation {} does not match protected checkpoint generation {}",
                anchor.generation, checkpoint.generation
            ),
        )),
    }
}

fn head_anchor_value(key: &[u8], generation: u64, entries: &BTreeMap<String, Value>) -> Value {
    json!({
        "v": HEAD_FORMAT_VERSION,
        "generation": generation,
        "entries": entries,
        "mac": head_manifest_mac(key, HEAD_FORMAT_VERSION, generation, entries),
    })
}

fn head_manifest_mac(
    key: &[u8],
    version: u64,
    generation: u64,
    entries: &BTreeMap<String, Value>,
) -> String {
    let body = if version == LEGACY_HEAD_FORMAT_VERSION {
        serde_json::to_vec(&json!({
            "v": version,
            "entries": entries,
        }))
    } else {
        serde_json::to_vec(&json!({
            "v": version,
            "generation": generation,
            "entries": entries,
        }))
    }
    .expect("head manifest is serializable");
    hex(&mac(key, &body))
}

fn checkpoint_mac(key: &[u8], generation: u64, manifest_mac: &str) -> String {
    hex(&mac(
        key,
        format!("checkpoint|{generation}|{manifest_mac}").as_bytes(),
    ))
}

fn head_entry_mac(key: &[u8], date: &str, count: u64, last: &str) -> String {
    hex(&mac(key, format!("{date}|{count}|{last}").as_bytes()))
}

fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut hmac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key size");
    hmac.update(data);
    hmac.finalize().into_bytes().to_vec()
}

fn verify_mac(key: &[u8], data: &[u8], tag: &[u8]) -> bool {
    let mut hmac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key size");
    hmac.update(data);
    hmac.verify_slice(tag).is_ok()
}

fn ct_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn unhex(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        decoded.push((high * 16 + low) as u8);
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let uniq = format!(
            "t-hub-audit-test-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        std::env::temp_dir().join(uniq)
    }

    fn clean(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(head_path_for(dir));
        let _ = std::fs::remove_file(key_path_for(dir));
        let _ = std::fs::remove_file(journal_path_for(&key_path_for(dir)));
    }

    fn meta() -> AuditMeta<'static> {
        AuditMeta {
            peer: "loopback",
            token_tier: "control",
            session: None,
            spawned_by: None,
            error: None,
        }
    }

    fn read_lines(dir: &Path) -> Vec<Value> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                if !line.trim().is_empty() {
                    out.push(serde_json::from_str(line).unwrap());
                }
            }
        }
        out
    }

    const TEST_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn send_text_content_is_redacted() {
        let dir = temp_dir("redact");
        clean(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "send_text",
            "process-changing",
            "allowed",
            &json!({"sessionId": "abc", "text": "SECRET password 123", "enter": true}),
            AuditMeta {
                session: Some("abc"),
                ..meta()
            },
        );
        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 1);
        let rec = &lines[0];
        // The literal text must NOT appear anywhere in the record.
        assert!(!serde_json::to_string(rec).unwrap().contains("SECRET"));
        assert_eq!(rec["args"]["textLen"], 19);
        assert!(rec["args"]["textSha256"].as_str().unwrap().len() == 16);
        assert_eq!(rec["command"], "send_text");
        assert_eq!(rec["decision"], "allowed");
        clean(&dir);
    }

    #[test]
    fn send_keys_names_are_kept() {
        let dir = temp_dir("keys");
        clean(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "send_keys",
            "process-changing",
            "allowed",
            &json!({"sessionId": "abc", "keys": ["C-c", "Enter"]}),
            AuditMeta {
                session: Some("abc"),
                ..meta()
            },
        );
        let lines = read_lines(&dir);
        assert_eq!(lines[0]["args"]["keys"][0], "C-c");
        clean(&dir);
    }

    #[test]
    fn keyed_hash_chain_links_and_verifies() {
        let dir = temp_dir("chain");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        for i in 0..3 {
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": format!("s{i}")}),
                meta(),
            );
        }
        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["prev"], "");
        assert_eq!(lines[1]["prev"], lines[0]["hash"]);
        assert_eq!(lines[2]["prev"], lines[1]["hash"]);
        let report = verify(&dir, TEST_KEY);
        println!("AUDIT_RECORDS {}", serde_json::to_string(&lines).unwrap());
        println!(
            "EXTERNAL_HEAD {}",
            std::fs::read_to_string(head_path_for(&dir)).unwrap()
        );
        println!("VERIFY_REPORT {}", report.to_json());
        assert!(report.ok(), "{:?}", report.breaks);
        assert_eq!(report.records, 3);
        clean(&dir);
    }

    #[test]
    fn refusal_is_recorded() {
        let dir = temp_dir("refuse");
        clean(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "spawn_terminal",
            "process-changing",
            "refused-cap",
            &json!({"cwd": "/tmp"}),
            meta(),
        );
        let lines = read_lines(&dir);
        assert_eq!(lines[0]["decision"], "refused-cap");
        clean(&dir);
    }

    #[test]
    fn unkeyed_forgery_is_rejected() {
        let dir = temp_dir("forgery");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record(
                "spawn_terminal",
                "process-changing",
                "refused-cap",
                &json!({"cwd": "/tmp"}),
                meta(),
            );
        }

        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut record: Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        record["decision"] = json!("allowed");
        let mut body = record.clone();
        body.as_object_mut().unwrap().remove("hash");
        record["hash"] = json!(hex(&Sha256::digest(serde_json::to_vec(&body).unwrap())));
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();

        let report = verify(&dir, TEST_KEY);
        println!("UNKEYED_FORGERY_VERIFY_REPORT {}", report.to_json());
        assert!(!report.ok());
        assert!(report
            .breaks
            .iter()
            .any(|audit_break| audit_break.kind == BreakKind::BadMac));
        clean(&dir);
    }

    #[test]
    fn tail_truncation_is_detected_by_head_anchor() {
        let dir = temp_dir("truncation");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            for index in 0..3 {
                log.record(
                    "close_terminal",
                    "process-changing",
                    "allowed",
                    &json!({"sessionId": format!("s{index}")}),
                    meta(),
                );
            }
        }

        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let content = std::fs::read_to_string(&path).unwrap();
        let first = content.lines().next().unwrap();
        std::fs::write(&path, format!("{first}\n")).unwrap();

        let report = verify(&dir, TEST_KEY);
        println!("TRUNCATION_VERIFY_REPORT {}", report.to_json());
        assert!(
            report
                .breaks
                .iter()
                .any(|audit_break| audit_break.kind == BreakKind::Truncated),
            "{:?}",
            report.breaks
        );
        clean(&dir);
    }

    #[test]
    fn missing_terminal_newline_is_detected_as_truncation() {
        let dir = temp_dir("missing-terminal-newline");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "first"}),
                meta(),
            );
        }

        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut content = std::fs::read(&path).unwrap();
        assert_eq!(content.pop(), Some(b'\n'));
        std::fs::write(&path, content).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(report.breaks.iter().any(|audit_break| {
            audit_break.kind == BreakKind::Truncated
                && audit_break.detail.contains("does not end with a newline")
        }));
        clean(&dir);
    }

    #[test]
    fn whole_day_removal_is_detected_by_head_manifest() {
        let dir = temp_dir("whole-day-truncation");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let mut guard = log.inner.lock().unwrap();
        for date in ["20260724", "20260725"] {
            log.record_locked(
                &mut guard,
                TEST_KEY,
                date,
                &format!("{date}T00:00:00Z"),
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": date}),
                meta(),
            )
            .unwrap();
        }
        drop(guard);

        std::fs::remove_file(dir.join("control-20260725.jsonl")).unwrap();
        let head_path = head_path_for(&dir);
        let mut head: Value =
            serde_json::from_str(&std::fs::read_to_string(&head_path).unwrap()).unwrap();
        head["entries"].as_object_mut().unwrap().remove("20260725");
        std::fs::write(&head_path, serde_json::to_vec(&head).unwrap()).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(report
            .breaks
            .iter()
            .any(|audit_break| audit_break.kind == BreakKind::HeadTampered));
        clean(&dir);
    }

    #[test]
    fn signed_head_rollback_is_detected_by_protected_checkpoint() {
        let dir = temp_dir("signed-head-rollback");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.try_record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "first"}),
            meta(),
        )
        .unwrap();
        let old_head = std::fs::read(head_path_for(&dir)).unwrap();
        log.try_record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "second"}),
            meta(),
        )
        .unwrap();

        let log_path = dir.join(format!(
            "control-{}.jsonl",
            chrono::Local::now().format("%Y%m%d")
        ));
        let first_line = std::fs::read_to_string(&log_path)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        std::fs::write(&log_path, format!("{first_line}\n")).unwrap();
        std::fs::write(head_path_for(&dir), old_head).unwrap();

        let restarted = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let report = restarted.verify_self();
        assert!(report.breaks.iter().any(|audit_break| {
            audit_break.kind == BreakKind::HeadTampered
                && audit_break.detail.contains("protected checkpoint")
        }));
        clean(&dir);
    }

    #[test]
    fn deleting_all_audit_files_is_detected_by_protected_checkpoint() {
        let dir = temp_dir("complete-deletion");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.try_record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "first"}),
            meta(),
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_file(head_path_for(&dir)).unwrap();

        let restarted = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let report = restarted.verify_self();
        assert!(report
            .breaks
            .iter()
            .any(|audit_break| audit_break.kind == BreakKind::HeadMissing));
        clean(&dir);
    }

    #[test]
    fn live_write_rejects_a_modified_head_manifest() {
        let dir = temp_dir("live-manifest-tamper");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let mut guard = log.inner.lock().unwrap();
        for date in ["20260724", "20260725"] {
            log.record_locked(
                &mut guard,
                TEST_KEY,
                date,
                &format!("{date}T00:00:00Z"),
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": date}),
                meta(),
            )
            .unwrap();
        }

        let head_path = head_path_for(&dir);
        let mut head: Value =
            serde_json::from_str(&std::fs::read_to_string(&head_path).unwrap()).unwrap();
        head["entries"].as_object_mut().unwrap().remove("20260724");
        std::fs::write(&head_path, serde_json::to_vec(&head).unwrap()).unwrap();

        let error = log
            .record_locked(
                &mut guard,
                TEST_KEY,
                "20260725",
                "20260725T00:00:01Z",
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "second"}),
                meta(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("manifest MAC"));
        drop(guard);
        clean(&dir);
    }

    #[test]
    fn live_process_authorization_rejects_modified_or_truncated_day_file() {
        for tamper in ["modified", "truncated"] {
            let dir = temp_dir(&format!("live-day-{tamper}"));
            clean(&dir);
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.try_record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "first"}),
                meta(),
            )
            .unwrap();
            let path = std::fs::read_dir(&dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let original = std::fs::read_to_string(&path).unwrap();
            let damaged = if tamper == "modified" {
                original.replace("\"sessionId\":\"first\"", "\"sessionId\":\"other\"")
            } else {
                String::new()
            };
            std::fs::write(&path, &damaged).unwrap();

            let error = log
                .try_record(
                    "close_terminal",
                    "process-changing",
                    "allowed",
                    &json!({"sessionId": "second"}),
                    meta(),
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("audit files changed after integrity verification"),
                "{tamper}: {error}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged);
            clean(&dir);
        }
    }

    #[test]
    fn live_process_authorization_rejects_historical_day_tampering() {
        let dir = temp_dir("live-historical-day");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let mut guard = log.inner.lock().unwrap();
        for date in ["20260724", "20260725"] {
            log.record_locked(
                &mut guard,
                TEST_KEY,
                date,
                &format!("{date}T00:00:00Z"),
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": date}),
                meta(),
            )
            .unwrap();
        }
        let historical_path = dir.join("control-20260724.jsonl");
        let content = std::fs::read_to_string(&historical_path).unwrap();
        std::fs::write(
            &historical_path,
            content.replace("\"sessionId\":\"20260724\"", "\"sessionId\":\"tampered\""),
        )
        .unwrap();

        let error = log
            .record_locked(
                &mut guard,
                TEST_KEY,
                "20260725",
                "20260725T00:00:01Z",
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "second"}),
                meta(),
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("audit files changed after integrity verification"));
        drop(guard);
        clean(&dir);
    }

    #[test]
    fn append_transition_rejects_a_replaced_live_path() {
        let dir = temp_dir("replaced-live-path");
        clean(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control-20260725.jsonl");
        std::fs::write(&path, b"first\n").unwrap();
        let previous = verified_audit_file(&path).unwrap();

        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(b"second\n").unwrap();
        writer.sync_data().unwrap();
        let writer_stamp = audit_file_stamp_for_file(&writer).unwrap();
        assert!(validate_append_transition(
            Some(&previous),
            &writer_stamp,
            &audit_file_stamp(&path).unwrap(),
            &path,
            b"second\n",
        )
        .is_ok());
        drop(writer);

        std::fs::rename(&path, dir.join("displaced.jsonl")).unwrap();
        std::fs::write(&path, vec![b'x'; writer_stamp.len as usize]).unwrap();
        let replacement_stamp = audit_file_stamp(&path).unwrap();
        assert_eq!(replacement_stamp.len, writer_stamp.len);
        assert!(validate_append_transition(
            Some(&previous),
            &writer_stamp,
            &replacement_stamp,
            &path,
            b"second\n",
        )
        .is_err());
        clean(&dir);
    }

    #[test]
    fn append_transition_rejects_same_length_prefix_tampering() {
        let dir = temp_dir("modified-live-prefix");
        clean(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control-20260725.jsonl");
        std::fs::write(&path, b"first\n").unwrap();
        let previous = verified_audit_file(&path).unwrap();

        std::fs::write(&path, b"other\n").unwrap();
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(b"second\n").unwrap();
        writer.sync_data().unwrap();
        let writer_stamp = audit_file_stamp_for_file(&writer).unwrap();

        let error = validate_append_transition(
            Some(&previous),
            &writer_stamp,
            &audit_file_stamp(&path).unwrap(),
            &path,
            b"second\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("authenticated prefix changed"));
        clean(&dir);
    }

    #[test]
    fn missing_head_is_detected_for_keyed_records() {
        let dir = temp_dir("missing-head");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "a"}),
                meta(),
            );
        }
        std::fs::remove_file(head_path_for(&dir)).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(report
            .breaks
            .iter()
            .any(|audit_break| audit_break.kind == BreakKind::HeadMissing));
        clean(&dir);
    }

    #[test]
    fn chain_survives_restart_with_same_key() {
        let dir = temp_dir("restart");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "a"}),
                meta(),
            );
        }
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "b"}),
                meta(),
            );
        }

        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["prev"], lines[0]["hash"]);
        assert!(verify(&dir, TEST_KEY).ok());
        clean(&dir);
    }

    #[test]
    fn pending_commit_rolls_forward_after_each_durable_stage() {
        let source = temp_dir("pending-source");
        clean(&source);
        let log = AuditLog::with_key(source.clone(), TEST_KEY.to_vec());
        log.record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "first"}),
            meta(),
        );
        let day_path = std::fs::read_dir(&source)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .unwrap();
        let old_log = std::fs::read(&day_path).unwrap();
        let old_head = std::fs::read(head_path_for(&source)).unwrap();
        let old_key = std::fs::read(key_path_for(&source)).unwrap();
        let old_state = decode_audit_key(
            &key_path_for(&source),
            std::str::from_utf8(&old_key).unwrap(),
            Some(TEST_KEY),
        )
        .unwrap();

        log.record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "second"}),
            meta(),
        );
        let new_log = std::fs::read(&day_path).unwrap();
        let new_head = std::fs::read(head_path_for(&source)).unwrap();
        let new_key = std::fs::read(key_path_for(&source)).unwrap();
        let new_state = decode_audit_key(
            &key_path_for(&source),
            std::str::from_utf8(&new_key).unwrap(),
            Some(TEST_KEY),
        )
        .unwrap();
        let date = day_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("control-"))
            .and_then(|name| name.strip_suffix(".jsonl"))
            .unwrap()
            .to_string();
        let second_line = std::str::from_utf8(&new_log[old_log.len()..])
            .unwrap()
            .trim_end()
            .to_string();
        let pending = PendingCommit {
            previous_checkpoint: old_state.checkpoint,
            checkpoint: new_state.checkpoint.unwrap(),
            record: Some((date, second_line.clone())),
            head: serde_json::from_slice(&new_head).unwrap(),
        };
        log.write_pending_commit(TEST_KEY, &pending).unwrap();
        let journal = std::fs::read(journal_path_for(&key_path_for(&source))).unwrap();
        drop(log);

        for (tag, staged_log, staged_head) in [
            ("before-record", old_log.clone(), old_head.clone()),
            ("record", new_log.clone(), old_head.clone()),
            (
                "partial-record",
                {
                    let mut partial = old_log.clone();
                    partial.extend_from_slice(&second_line.as_bytes()[..second_line.len() / 2]);
                    partial
                },
                old_head.clone(),
            ),
            ("head", new_log.clone(), new_head.clone()),
        ] {
            let dir = temp_dir(tag);
            clean(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let staged_day = dir.join(day_path.file_name().unwrap());
            std::fs::write(&staged_day, staged_log).unwrap();
            std::fs::write(head_path_for(&dir), staged_head).unwrap();
            std::fs::write(key_path_for(&dir), &old_key).unwrap();
            std::fs::write(journal_path_for(&key_path_for(&dir)), &journal).unwrap();

            let restarted = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            let report = restarted.verify_self();
            assert!(report.ok(), "{tag}: {:?}", report.breaks);
            assert_eq!(std::fs::read(&staged_day).unwrap(), new_log, "{tag}");
            assert_eq!(
                std::fs::read(head_path_for(&dir)).unwrap(),
                new_head,
                "{tag}"
            );
            assert_eq!(std::fs::read(key_path_for(&dir)).unwrap(), new_key, "{tag}");
            assert!(!journal_path_for(&key_path_for(&dir)).exists(), "{tag}");
            clean(&dir);
        }
        clean(&source);
    }

    #[test]
    fn cached_verification_avoids_repeated_full_scans() {
        let dir = temp_dir("verify-cache");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "a"}),
            meta(),
        );
        assert!(log.verify_self_cached().ok());
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            content.replace("\"sessionId\":\"a\"", "\"sessionId\":\"b\""),
        )
        .unwrap();

        assert!(log.verify_self_cached().ok());
        assert!(!log.verify_self().ok());
        clean(&dir);
    }

    #[test]
    fn failed_verification_is_cached() {
        let dir = temp_dir("failed-verify-cache");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "a"}),
            meta(),
        );
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let original = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            original.replace("\"sessionId\":\"a\"", "\"sessionId\":\"b\""),
        )
        .unwrap();

        assert!(!log.verify_self_cached().ok());
        std::fs::write(&path, original).unwrap();
        assert!(!log.verify_self_cached().ok());
        assert!(log.verify_self().ok());
        clean(&dir);
    }

    #[test]
    fn startup_tamper_check_quarantines_future_writes() {
        let dir = temp_dir("startup-quarantine");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "a"}),
                meta(),
            );
        }
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            content.replace("\"sessionId\":\"a\"", "\"sessionId\":\"b\""),
        )
        .unwrap();

        let restarted = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let report = restarted.startup_integrity_check();
        println!("STARTUP_VERIFY_REPORT {}", report.to_json());
        assert!(!report.ok());
        let error = restarted
            .try_record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "c"}),
                meta(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("audit sink is quarantined"));
        clean(&dir);
    }

    #[test]
    fn on_demand_tamper_check_quarantines_future_writes() {
        let dir = temp_dir("on-demand-quarantine");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.record(
            "close_terminal",
            "process-changing",
            "allowed",
            &json!({"sessionId": "a"}),
            meta(),
        );
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            content.replace("\"sessionId\":\"a\"", "\"sessionId\":\"b\""),
        )
        .unwrap();

        let report = log.verify_self();
        println!("ON_DEMAND_VERIFY_REPORT {}", report.to_json());
        assert!(!report.ok());
        let error = log
            .try_record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": "c"}),
                meta(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("audit sink is quarantined"));
        clean(&dir);
    }

    #[test]
    fn try_record_surfaces_and_quarantines_sink_failure() {
        let base = temp_dir("sink-failure");
        clean(&base);
        std::fs::write(&base, b"not a directory").unwrap();
        let log = AuditLog::with_key(base.join("audit"), TEST_KEY.to_vec());

        let first = log.try_record(
            "spawn_terminal",
            "process-changing",
            "authorized",
            &json!({"cwd": "/tmp"}),
            meta(),
        );
        assert!(first.is_err());
        let second = log.try_record(
            "spawn_terminal",
            "process-changing",
            "authorized",
            &json!({"cwd": "/tmp"}),
            meta(),
        );
        assert!(second
            .unwrap_err()
            .to_string()
            .contains("audit sink is quarantined"));
        let _ = std::fs::remove_file(&base);
    }

    #[test]
    fn legacy_records_are_migrated_and_new_unanchored_files_are_rejected() {
        let dir = temp_dir("legacy");
        clean(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for date in ["20260101", "20260102"] {
            let mut legacy = json!({
                "ts": format!("{date}T00:00:00Z"),
                "command": "close_terminal",
                "decision": "allowed",
                "prev": "",
                "args": {"sessionId": date},
            });
            let body = serde_json::to_vec(&legacy).unwrap();
            legacy["hash"] = json!(hex(&Sha256::digest(&body)));
            std::fs::write(
                dir.join(format!("control-{date}.jsonl")),
                format!("{}\n", serde_json::to_string(&legacy).unwrap()),
            )
            .unwrap();
        }

        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        let report = log.verify_self();
        assert!(report.ok(), "{:?}", report.breaks);
        assert_eq!(report.legacy, 2);
        let head: Value =
            serde_json::from_str(&std::fs::read_to_string(head_path_for(&dir)).unwrap()).unwrap();
        assert_eq!(head["entries"].as_object().unwrap().len(), 2);

        let mut injected = json!({
            "ts": "2026-01-03T00:00:00Z",
            "command": "close_terminal",
            "decision": "allowed",
            "prev": "",
            "args": {"sessionId": "injected"},
        });
        let body = serde_json::to_vec(&injected).unwrap();
        injected["hash"] = json!(hex(&Sha256::digest(&body)));
        std::fs::write(
            dir.join("control-20260103.jsonl"),
            format!("{}\n", serde_json::to_string(&injected).unwrap()),
        )
        .unwrap();

        let report = log.verify_self();
        assert!(report
            .breaks
            .iter()
            .any(|audit_break| audit_break.kind == BreakKind::HeadMissing));
        clean(&dir);
    }

    #[test]
    fn persistent_key_is_created_once_with_exact_length() {
        let dir = temp_dir("persistent-key");
        clean(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit-hmac-key");
        let first = load_or_create_audit_key(&path, None).unwrap();
        let second = load_or_create_audit_key(&path, None).unwrap();
        println!(
            "PERSISTENT_KEY {}",
            json!({
                "bytes": first.key.len(),
                "reused": first.key == second.key,
                "storedOutsideAuditLog": true,
            })
        );
        assert_eq!(first.key.len(), AUDIT_KEY_BYTES);
        assert_eq!(first.key, second.key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        clean(&dir);
    }

    #[test]
    fn persistent_key_creation_durably_creates_nested_directories() {
        let root = temp_dir("durable-nested-directory");
        clean(&root);
        let path = root.join("profile").join("state").join("audit-hmac-key");

        let state = load_or_create_audit_key(&path, Some(TEST_KEY)).unwrap();
        create_dir_all_durable(path.parent().unwrap()).unwrap();

        assert_eq!(state.key, TEST_KEY);
        assert!(path.is_file());
        clean(&root);
    }

    #[test]
    fn durable_directory_cache_refreshes_changed_identities() {
        let root = temp_dir("durable-directory-cache");
        clean(&root);
        let path = root.join("profile").join("state");
        std::fs::create_dir_all(&path).unwrap();
        let actual = directory_stamp(&std::fs::metadata(&path).unwrap());
        let mut cache = HashMap::new();
        #[cfg(unix)]
        cache.insert(
            path.clone(),
            DirectoryStamp {
                device: actual.device.wrapping_add(1),
                inode: actual.inode.wrapping_add(1),
            },
        );
        #[cfg(not(unix))]
        cache.insert(path.clone(), DirectoryStamp { modified: None });

        create_dir_all_durable_cached(&path, &mut cache).unwrap();

        assert_eq!(cache.get(&path), Some(&actual));
        clean(&root);
    }

    #[test]
    fn unhex_roundtrips_and_rejects_invalid_input() {
        assert_eq!(
            unhex(&hex(&[0x00, 0xab, 0xff])),
            Some(vec![0x00, 0xab, 0xff])
        );
        assert_eq!(unhex("xyz"), None);
        assert_eq!(unhex("abc"), None);
    }
}
