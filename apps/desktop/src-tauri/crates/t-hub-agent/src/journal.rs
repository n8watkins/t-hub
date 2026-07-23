//! The durable, append-only **event journal** (PLAN.md Workstream A, PRD §8).
//!
//! The journal is the authority for reconstruction *intent*: it survives the
//! Windows app closing (it lives on the WSL VHDX), and is replayed to the core
//! on every reconnect. It is an append-only file of newline-delimited JSON —
//! one [`EventJournalEntry`] per line — so it is crash-tolerant by construction
//! (a torn final line is detected and ignored on open).
//!
//! ## Durability
//! Each [`Journal::append`] acquires the journal's interprocess transaction
//! lock, allocates the next sequence from durable head state, writes one complete
//! line, and `fsync`s both the journal and head state before returning.
//! A stale or torn head state is rebuilt from complete journal lines.
//!
//! ## Why a file, not SQLite (here)
//! The agent's journal is a *write-mostly, append-only, replay-from-cursor* log;
//! a flat NDJSON file with `fsync` is the simplest thing that gives the needed
//! durability + ordered replay, and it is trivially inspectable for debugging.
//! (The Windows core keeps its own SQLite catalog; that is a separate concern.)

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use t_hub_protocol::{EventJournalEntry, JournalSource};

/// Default journal location relative to `$HOME`: `~/.t-hub/journal`.
const JOURNAL_SUBDIR: &str = ".t-hub/journal";
/// Environment override inherited by agents launched from an isolated runtime.
const JOURNAL_DIR_ENV: &str = "T_HUB_AGENT_JOURNAL_DIR";
/// The append-only log file name within the journal directory.
const JOURNAL_FILE: &str = "events.ndjson";
/// Interprocess lock shared by the long-lived agent and short-lived hooks.
const JOURNAL_LOCK_FILE: &str = "events.lock";
/// Atomically replaced allocation state for the journal head.
const JOURNAL_HEAD_FILE: &str = "head.json";
/// Hook processes must never wait indefinitely behind a stalled writer.
const JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const JOURNAL_LOCK_RETRY: Duration = Duration::from_millis(2);
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_HEAD_BYTES: usize = 128 * 1024;
const MAX_SCAN_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECENT_EVENT_IDS: usize = 1024;
const HEAD_VERSION: u8 = 2;

/// At agent startup, compact the journal once it exceeds this size. The
/// incremental tail (see [`Journal::tail_from`]) keeps live delivery cheap at
/// ANY size, so this only bounds *disk* growth from the high-frequency
/// statusline-snapshot stream. See [`Journal::compact_dropping_status`].
pub const COMPACT_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;

/// Pure journal path resolver.
/// An explicit CLI argument wins, followed by the environment override, followed
/// by the production default under the user's home directory.
fn resolve_journal_dir_from(
    override_dir: Option<&str>,
    env_dir: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(dir) = override_dir.filter(|dir| !dir.trim().is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = env_dir.filter(|dir| !dir.as_os_str().is_empty()) {
        if dir.is_absolute() {
            return dir.to_path_buf();
        }
        return home.map_or_else(|| dir.to_path_buf(), |home| home.join(dir));
    }
    home.map_or_else(
        || PathBuf::from(JOURNAL_SUBDIR),
        |home| home.join(JOURNAL_SUBDIR),
    )
}

/// Resolve the journal directory from the CLI, process environment, or the
/// unchanged production default.
pub fn resolve_journal_dir(override_dir: Option<&str>) -> PathBuf {
    let env_dir = std::env::var_os(JOURNAL_DIR_ENV).map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_journal_dir_from(override_dir, env_dir.as_deref(), home.as_deref())
}

/// An open append-only journal.
///
/// The internal mutex serializes threads sharing this handle.
/// A separate file lock serializes independent short-lived hook processes and
/// the long-lived agent across append and compaction transactions.
pub struct Journal {
    path: PathBuf,
    dir: PathBuf,
    lock_file: File,
    inner: Mutex<Inner>,
}

struct Inner {
    /// Highest sequence appended so far (0 = empty journal).
    head_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeadState {
    version: u8,
    head_seq: u64,
    journal_len: u64,
    #[serde(default)]
    recent_event_ids: Vec<RecentEventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecentEventId {
    event_id: String,
    seq: u64,
}

/// Result of one journal append transaction.
///
/// A duplicate carries the already-assigned sequence but was not written again.
#[derive(Debug, Clone)]
pub enum AppendOutcome {
    Appended(EventJournalEntry),
    Duplicate(EventJournalEntry),
}

impl AppendOutcome {
    pub fn is_appended(&self) -> bool {
        matches!(self, Self::Appended(_))
    }

    fn entry(&self) -> &EventJournalEntry {
        match self {
            Self::Appended(entry) | Self::Duplicate(entry) => entry,
        }
    }
}

impl std::ops::Deref for AppendOutcome {
    type Target = EventJournalEntry;

    fn deref(&self) -> &Self::Target {
        self.entry()
    }
}

struct BoundedLine {
    bytes: Vec<u8>,
    complete: bool,
}

struct InterprocessLock<'a> {
    #[cfg(unix)]
    file: &'a File,
    #[cfg(not(unix))]
    _file: std::marker::PhantomData<&'a File>,
}

impl Drop for InterprocessLock<'_> {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(std::os::fd::AsRawFd::as_raw_fd(self.file), libc::LOCK_UN);
        }
    }
}

impl Journal {
    /// Open (creating if needed) the journal under `dir`. Recovers the head
    /// sequence by scanning existing valid lines; a torn trailing line (from a
    /// crash mid-append) is tolerated — it is simply not counted, and the next
    /// append starts a clean line after it.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating journal dir {dir:?}"))?;
        let path = dir.join(JOURNAL_FILE);
        let lock_path = dir.join(JOURNAL_LOCK_FILE);
        let lock_file = Self::open_private_file(&lock_path, true)
            .with_context(|| format!("opening journal lock {lock_path:?}"))?;
        let journal = Self {
            path,
            dir: dir.to_path_buf(),
            lock_file,
            inner: Mutex::new(Inner { head_seq: 0 }),
        };

        {
            let _lock = journal.lock_exclusive()?;
            Self::open_private_file(&journal.path, true)
                .with_context(|| format!("opening journal file {:?}", journal.path))?;
            let head = journal.authoritative_head_locked()?;
            journal
                .inner
                .lock()
                .expect("journal mutex poisoned")
                .head_seq = head.head_seq;
        }
        Ok(journal)
    }

    #[cfg(unix)]
    fn open_private_file(path: &Path, create: bool) -> Result<File> {
        Self::open_private_file_with(path, create, false, false)
    }

    #[cfg(not(unix))]
    fn open_private_file(path: &Path, create: bool) -> Result<File> {
        OpenOptions::new()
            .create(create)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening private journal file {path:?}"))
    }

    #[cfg(unix)]
    fn open_private_file_with(
        path: &Path,
        create: bool,
        append: bool,
        truncate: bool,
    ) -> Result<File> {
        use std::os::unix::fs::OpenOptionsExt;

        let file = OpenOptions::new()
            .create(create)
            .read(true)
            .write(true)
            .append(append)
            .truncate(truncate)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("opening private journal file {path:?}"))?;
        Self::enforce_private_regular_file(&file, path)?;
        Ok(file)
    }

    #[cfg(not(unix))]
    fn open_private_file_with(
        path: &Path,
        create: bool,
        append: bool,
        truncate: bool,
    ) -> Result<File> {
        OpenOptions::new()
            .create(create)
            .read(true)
            .write(true)
            .append(append)
            .truncate(truncate)
            .open(path)
            .with_context(|| format!("opening private journal file {path:?}"))
    }

    #[cfg(unix)]
    fn enforce_private_regular_file(file: &File, path: &Path) -> Result<()> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = file
            .metadata()
            .with_context(|| format!("reading private journal metadata {path:?}"))?;
        anyhow::ensure!(
            metadata.file_type().is_file(),
            "private journal path is not a regular file: {path:?}"
        );
        anyhow::ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "private journal path is not owned by the current user: {path:?}"
        );
        let rc = unsafe { libc::fchmod(file.as_raw_fd(), 0o600) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("restricting private journal permissions {path:?}"));
        }
        let mode = file
            .metadata()
            .with_context(|| format!("verifying private journal permissions {path:?}"))?
            .permissions()
            .mode()
            & 0o777;
        anyhow::ensure!(
            mode == 0o600,
            "private journal permissions are {mode:o}, expected 600: {path:?}"
        );
        Ok(())
    }

    fn open_private_append(path: &Path) -> Result<File> {
        Self::open_private_file_with(path, true, true, false)
    }

    fn open_private_truncate(path: &Path) -> Result<File> {
        Self::open_private_file_with(path, true, false, true)
    }

    fn sync_directory(&self, context: &'static str) -> Result<()> {
        File::open(&self.dir)
            .and_then(|dir| dir.sync_all())
            .context(context)
    }

    fn lock_exclusive(&self) -> Result<InterprocessLock<'_>> {
        #[cfg(unix)]
        {
            self.lock_unix(libc::LOCK_EX)
        }
        #[cfg(not(unix))]
        {
            Ok(InterprocessLock {
                _file: std::marker::PhantomData,
            })
        }
    }

    fn lock_shared(&self) -> Result<InterprocessLock<'_>> {
        #[cfg(unix)]
        {
            self.lock_unix(libc::LOCK_SH)
        }
        #[cfg(not(unix))]
        {
            Ok(InterprocessLock {
                _file: std::marker::PhantomData,
            })
        }
    }

    #[cfg(unix)]
    fn lock_unix(&self, operation: libc::c_int) -> Result<InterprocessLock<'_>> {
        use std::os::fd::AsRawFd;

        let deadline = Instant::now() + JOURNAL_LOCK_TIMEOUT;
        loop {
            let rc = unsafe { libc::flock(self.lock_file.as_raw_fd(), operation | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(InterprocessLock {
                    file: &self.lock_file,
                });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error).context("acquiring journal transaction lock");
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "journal transaction lock remained busy for {}ms",
                    JOURNAL_LOCK_TIMEOUT.as_millis()
                );
            }
            std::thread::sleep(JOURNAL_LOCK_RETRY);
        }
    }

    /// Scan the file and return its complete, parseable line count plus the byte
    /// boundary before any torn trailing line.
    fn recover_head(path: &Path) -> Result<HeadState> {
        let file = match Self::open_private_file(path, false) {
            Ok(f) => f,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(HeadState {
                    version: HEAD_VERSION,
                    head_seq: 0,
                    journal_len: 0,
                    recent_event_ids: Vec::new(),
                });
            }
            Err(e) => return Err(e).with_context(|| format!("reading journal {path:?}")),
        };
        let file_len = file
            .metadata()
            .with_context(|| format!("reading journal metadata {path:?}"))?
            .len();
        Self::ensure_scan_bound(file_len)?;
        let mut reader = BufReader::new(file);
        let mut count: u64 = 0;
        let mut valid_len = 0_u64;
        let mut scanned = 0_u64;
        let mut recent_event_ids = Vec::new();
        loop {
            let Some(line) = Self::read_bounded_line(&mut reader, &mut scanned)? else {
                break;
            };
            if !line.complete {
                break;
            }
            valid_len = scanned;
            if line.bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Ok(entry) = serde_json::from_slice::<EventJournalEntry>(&line.bytes) {
                count += 1;
                if let Some(event_id) = entry.event_id.as_deref() {
                    Self::remember_event_id(&mut recent_event_ids, event_id, count);
                }
            }
        }
        Ok(HeadState {
            version: HEAD_VERSION,
            head_seq: count,
            journal_len: valid_len.min(file_len),
            recent_event_ids,
        })
    }

    fn valid_event_id(event_id: &str) -> bool {
        let Some(digest) = event_id.strip_prefix("provider-event:v1:") else {
            return false;
        };
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn valid_recent_event_ids(state: &HeadState) -> bool {
        state.recent_event_ids.len() <= MAX_RECENT_EVENT_IDS
            && state.recent_event_ids.iter().all(|recent| {
                recent.seq > 0
                    && recent.seq <= state.head_seq
                    && Self::valid_event_id(&recent.event_id)
            })
            && state
                .recent_event_ids
                .windows(2)
                .all(|window| window[0].seq < window[1].seq)
    }

    fn remember_event_id(recent: &mut Vec<RecentEventId>, event_id: &str, seq: u64) {
        if !Self::valid_event_id(event_id) {
            return;
        }
        if let Some(index) = recent.iter().position(|known| known.event_id == event_id) {
            recent.remove(index);
        }
        recent.push(RecentEventId {
            event_id: event_id.to_string(),
            seq,
        });
        let excess = recent.len().saturating_sub(MAX_RECENT_EVENT_IDS);
        if excess > 0 {
            recent.drain(..excess);
        }
    }

    fn ensure_scan_bound(len: u64) -> Result<()> {
        anyhow::ensure!(
            len <= MAX_SCAN_BYTES,
            "journal is {len} bytes, exceeding the {MAX_SCAN_BYTES}-byte scan limit"
        );
        Ok(())
    }

    fn read_bounded_line<R: BufRead>(
        reader: &mut R,
        scanned: &mut u64,
    ) -> Result<Option<BoundedLine>> {
        let mut bytes = Vec::new();
        let limit = MAX_ENTRY_BYTES
            .checked_add(2)
            .context("journal entry read limit overflow")?;
        let read = reader
            .take(limit as u64)
            .read_until(b'\n', &mut bytes)
            .context("reading journal line")?;
        if read == 0 {
            return Ok(None);
        }
        *scanned = scanned
            .checked_add(read as u64)
            .context("journal scan byte count overflow")?;
        anyhow::ensure!(
            *scanned <= MAX_SCAN_BYTES,
            "journal scan exceeded the {MAX_SCAN_BYTES}-byte limit"
        );
        let complete = bytes.last() == Some(&b'\n');
        let content_len = bytes.len().saturating_sub(usize::from(complete));
        anyhow::ensure!(
            content_len <= MAX_ENTRY_BYTES,
            "journal entry exceeds the {MAX_ENTRY_BYTES}-byte limit"
        );
        if complete {
            bytes.pop();
        }
        Ok(Some(BoundedLine { bytes, complete }))
    }

    fn read_head_state(&self) -> Option<HeadState> {
        let file = Self::open_private_file(&self.dir.join(JOURNAL_HEAD_FILE), false).ok()?;
        let mut bytes = Vec::new();
        file.take((MAX_HEAD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_HEAD_BYTES {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    fn authoritative_head_locked(&self) -> Result<HeadState> {
        let journal_len = std::fs::metadata(&self.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if let Some(state) = self.read_head_state().filter(|state| {
            state.version == HEAD_VERSION
                && state.journal_len == journal_len
                && Self::valid_recent_event_ids(state)
        }) {
            return Ok(state);
        }

        let mut state = Self::recover_head(&self.path)?;
        if state.journal_len < journal_len {
            let file = Self::open_private_file(&self.path, false)
                .context("opening journal to remove torn tail")?;
            file.set_len(state.journal_len)
                .context("truncating torn journal tail")?;
            file.sync_data().context("syncing repaired journal")?;
        }
        state.version = HEAD_VERSION;
        self.publish_head_state_locked(&state)?;
        Ok(state)
    }

    fn publish_head_state_locked(&self, state: &HeadState) -> Result<()> {
        let path = self.dir.join(JOURNAL_HEAD_FILE);
        let tmp = self
            .dir
            .join(format!("{JOURNAL_HEAD_FILE}.tmp.{}", std::process::id()));
        let bytes = serde_json::to_vec(&state).context("serializing journal head")?;
        anyhow::ensure!(
            bytes.len() <= MAX_HEAD_BYTES,
            "serialized journal head exceeds the {MAX_HEAD_BYTES}-byte limit"
        );
        let mut file = Self::open_private_truncate(&tmp)?;
        file.write_all(&bytes).context("writing journal head")?;
        file.flush().context("flushing journal head")?;
        file.sync_all().context("syncing journal head")?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("publishing journal head {path:?}"))?;
        self.sync_directory("syncing journal directory after head publication")?;
        Ok(())
    }

    /// The current head sequence (highest appended seq; 0 when empty).
    ///
    /// This is the **in-memory** head, bumped only by this process's
    /// [`Journal::append`] calls. It does NOT observe entries appended to the
    /// file by *other* processes (notably the short-lived `--hook` ingest
    /// processes, which are the live event spine's primary writers). For live
    /// tailing of cross-process appends use [`Journal::head_seq_on_disk`].
    pub fn head_seq(&self) -> u64 {
        self.inner.lock().expect("journal mutex poisoned").head_seq
    }

    /// The head sequence as observed **on disk**, re-scanning the file to count
    /// complete, parseable lines — so it sees entries appended by *other*
    /// processes (the `--hook` ingest path). Advances the in-memory `head_seq`
    /// to match (never backwards) so a subsequent [`Journal::replay`] /
    /// [`Journal::head_seq`] is consistent with what the tail just observed.
    ///
    /// This is what makes the hook → journal → agent → core spine truly *live*:
    /// a hook fired by Claude is a separate process that appends to the file; the
    /// long-lived `--stdio` agent's tail thread polls this to notice the growth.
    pub fn head_seq_on_disk(&self) -> u64 {
        let mut guard = self.inner.lock().expect("journal mutex poisoned");
        let Ok(_lock) = self.lock_exclusive() else {
            return guard.head_seq;
        };
        if let Ok(state) = self.authoritative_head_locked() {
            guard.head_seq = guard.head_seq.max(state.head_seq);
        }
        guard.head_seq
    }

    /// The on-disk path of the log file (for diagnostics / `--hook` ingest path).
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append `entry`, assign it the next sequence, fsync, and return the stored
    /// entry (with `seq` populated). The write is durable when this returns.
    pub fn append(&self, mut entry: EventJournalEntry) -> Result<AppendOutcome> {
        let mut guard = self.inner.lock().expect("journal mutex poisoned");
        let _lock = self.lock_exclusive()?;
        let mut state = self.authoritative_head_locked()?;
        if let Some(event_id) = entry.event_id.as_deref() {
            anyhow::ensure!(
                Self::valid_event_id(event_id),
                "journal event_id is not a canonical provider identity"
            );
            if let Some(known) = state
                .recent_event_ids
                .iter()
                .find(|known| known.event_id == event_id)
            {
                entry.seq = known.seq;
                return Ok(AppendOutcome::Duplicate(entry));
            }
        }
        let seq = state
            .head_seq
            .checked_add(1)
            .context("journal sequence exhausted")?;
        entry.seq = seq;

        let mut line = serde_json::to_vec(&entry).context("serializing journal entry")?;
        anyhow::ensure!(
            line.len() <= MAX_ENTRY_BYTES,
            "serialized journal entry exceeds the {MAX_ENTRY_BYTES}-byte limit"
        );
        line.push(b'\n');
        let next_len = state
            .journal_len
            .checked_add(line.len() as u64)
            .context("journal length overflow")?;
        Self::ensure_scan_bound(next_len)?;
        let mut file = Self::open_private_append(&self.path)
            .with_context(|| format!("opening journal append target {:?}", self.path))?;
        file.write_all(&line).context("writing journal line")?;
        file.flush().context("flushing journal")?;
        file.sync_data().context("fsync journal")?;
        #[cfg(test)]
        Self::pause_after_journal_sync_for_test();

        let journal_len = file
            .metadata()
            .context("reading appended journal metadata")?
            .len();
        state.version = HEAD_VERSION;
        state.head_seq = seq;
        state.journal_len = journal_len;
        if let Some(event_id) = entry.event_id.as_deref() {
            Self::remember_event_id(&mut state.recent_event_ids, event_id, seq);
        }
        self.publish_head_state_locked(&state)?;

        guard.head_seq = seq;
        Ok(AppendOutcome::Appended(entry))
    }

    /// Read back all entries with `seq > after_seq`, in order, for replay to the
    /// core. `after_seq == 0` replays the whole journal. Torn/garbage lines are
    /// skipped (same tolerance as recovery).
    pub fn replay(&self, after_seq: u64) -> Result<Vec<EventJournalEntry>> {
        // Take the lock to get a consistent view, then read from the start of the
        // file via a fresh handle so we don't disturb the append cursor.
        let _guard = self.inner.lock().expect("journal mutex poisoned");
        let _lock = self.lock_shared()?;
        let file = match Self::open_private_file(&self.path, false) {
            Ok(f) => f,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e).context("opening journal for replay"),
        };
        Self::ensure_scan_bound(
            file.metadata()
                .context("reading journal replay metadata")?
                .len(),
        )?;
        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(0))
            .context("seeking journal replay to start")?;

        let mut out = Vec::new();
        let mut seq: u64 = 0;
        let mut scanned = 0_u64;
        loop {
            let Some(line) = Self::read_bounded_line(&mut reader, &mut scanned)? else {
                break;
            };
            if !line.complete {
                break;
            }
            if line.bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<EventJournalEntry>(&line.bytes) {
                Ok(mut entry) => {
                    seq += 1;
                    if seq > after_seq {
                        entry.seq = seq;
                        out.push(entry);
                    }
                }
                Err(_) => {
                    // A complete unknown/future record does not consume an
                    // event sequence and must not hide later valid entries.
                    continue;
                }
            }
        }
        Ok(out)
    }

    /// The current on-disk size of the journal file in bytes (0 if absent).
    ///
    /// O(1) (a `stat`). Used by the live tail to seed its byte cursor at the
    /// current EOF so it streams only entries appended afterwards. See
    /// [`Journal::tail_from`].
    pub fn byte_len(&self) -> u64 {
        let _guard = self.inner.lock().expect("journal mutex poisoned");
        let Ok(_lock) = self.lock_shared() else {
            return std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        };
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    /// Incrementally read the complete entries appended *after* byte `offset`,
    /// numbering each as the next sequence after `last_seq`. Returns `(entries,
    /// new_offset, new_head_seq)` — feed `new_offset`/`new_head_seq` back in on
    /// the next call.
    ///
    /// This is the live-tail hot path. It seeks straight to `offset` and reads
    /// only the new bytes, so its cost is O(new data) no matter how large the
    /// journal has grown — unlike [`Journal::head_seq_on_disk`] / [`Journal::replay`],
    /// which re-scan and re-parse the *whole* file every call. (A bloated journal
    /// — e.g. one flooded with high-frequency statusline snapshots — makes that
    /// O(file) rescan saturate the tail thread and starve live delivery; reading
    /// only new bytes does not.) A torn final line (no trailing newline yet) is
    /// left unconsumed so a later call re-reads it once complete. If the file has
    /// shrunk below `offset` (compaction / rotation / truncation), reading
    /// restarts from the top with a fresh sequence so the renumbered contents are
    /// not skipped.
    pub fn tail_from(
        &self,
        offset: u64,
        last_seq: u64,
    ) -> Result<(Vec<EventJournalEntry>, u64, u64)> {
        let _guard = self.inner.lock().expect("journal mutex poisoned");
        let _lock = self.lock_shared()?;
        let file = match Self::open_private_file(&self.path, false) {
            Ok(f) => f,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok((Vec::new(), 0, 0));
            }
            Err(e) => return Err(e).context("opening journal for tail"),
        };
        let len = file
            .metadata()
            .context("reading journal tail metadata")?
            .len();
        Self::ensure_scan_bound(len)?;
        // Compaction/rotation/truncation: the file is smaller than where we were,
        // so our byte offset is stale — restart from the top with a fresh seq.
        let (mut pos, mut seq) = if len < offset {
            (0, 0)
        } else {
            (offset, last_seq)
        };

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(pos))
            .with_context(|| format!("seeking journal to {pos}"))?;

        let mut out = Vec::new();
        let mut scanned = pos;
        loop {
            let before = scanned;
            let Some(line) = Self::read_bounded_line(&mut reader, &mut scanned)? else {
                break;
            };
            // Only consume a line terminated by '\n'; leave a partial trailing
            // line for the next call (don't advance past it).
            if !line.complete {
                break;
            }
            pos = pos
                .checked_add(scanned - before)
                .context("journal tail offset overflow")?;
            if line.bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            // Skip torn/garbage lines but still advance past them (same tolerance
            // as recovery/replay); only count parseable entries toward `seq`.
            if let Ok(mut e) = serde_json::from_slice::<EventJournalEntry>(&line.bytes) {
                seq += 1;
                e.seq = seq;
                out.push(e);
            }
        }
        Ok((out, pos, seq))
    }

    /// Rewrite the journal keeping every entry EXCEPT ephemeral `Status`
    /// snapshots, shrinking it back down. Returns `(before_bytes, after_bytes,
    /// kept_entries)`.
    ///
    /// **When to call:** this renumbers sequences (they are 1-based line
    /// positions), so it MUST run while no core is attached — i.e. at agent
    /// startup, before [`crate::transport::serve_stdio`]. Running it
    /// mid-connection would push subsequent seqs *below* the core's replay cursor
    /// and silently stall delivery. At startup there is no cursor yet, so the
    /// core simply handshakes against the freshly-compacted head.
    ///
    /// Append and compaction use the same interprocess transaction lock.
    /// A hook that opened the journal before compaction therefore reopens the
    /// published path only after compaction completes and cannot write to the
    /// retired inode.
    /// Unparseable lines are kept and never silently drop unknown durable data.
    pub fn compact_dropping_status(&self) -> Result<(u64, u64, u64)> {
        let mut guard = self.inner.lock().expect("journal mutex poisoned");
        let _lock = self.lock_exclusive()?;
        let _ = self.authoritative_head_locked()?;
        let before = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);

        let src = match Self::open_private_file(&self.path, false) {
            Ok(f) => f,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok((0, 0, 0));
            }
            Err(e) => return Err(e).context("opening journal for compaction"),
        };
        Self::ensure_scan_bound(
            src.metadata()
                .context("reading journal compaction metadata")?
                .len(),
        )?;
        // Unique, pid-tagged temp name so a concurrent compaction in another
        // process can't clobber ours; the atomic rename publishes the result.
        let tmp = self
            .path
            .with_file_name(format!("{JOURNAL_FILE}.compact.{}", std::process::id()));

        let mut kept: u64 = 0;
        let mut recent_event_ids = Vec::new();
        {
            let file = Self::open_private_truncate(&tmp)
                .with_context(|| format!("creating compaction temp {tmp:?}"))?;
            let mut out = std::io::BufWriter::new(file);
            let mut reader = BufReader::new(src);
            let mut scanned = 0_u64;
            loop {
                let Some(line) = Self::read_bounded_line(&mut reader, &mut scanned)? else {
                    break;
                };
                if !line.complete {
                    break;
                }
                if line.bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let is_status = serde_json::from_slice::<EventJournalEntry>(&line.bytes)
                    .map(|e| e.source == JournalSource::Status)
                    .unwrap_or(false);
                if !is_status {
                    out.write_all(&line.bytes)
                        .context("writing compaction entry")?;
                    out.write_all(b"\n")?;
                    if let Ok(entry) = serde_json::from_slice::<EventJournalEntry>(&line.bytes) {
                        kept += 1;
                        if let Some(event_id) = entry.event_id.as_deref() {
                            Self::remember_event_id(&mut recent_event_ids, event_id, kept);
                        }
                    }
                }
            }
            out.flush().context("flushing compaction temp")?;
            let f = out.into_inner().context("finishing compaction temp")?;
            f.sync_all().context("syncing compaction temp")?;
        }

        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("publishing compacted journal {:?}", self.path))?;
        self.sync_directory("syncing journal directory after compaction")?;
        guard.head_seq = kept;

        let after = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        self.publish_head_state_locked(&HeadState {
            version: HEAD_VERSION,
            head_seq: kept,
            journal_len: after,
            recent_event_ids,
        })?;
        Ok((before, after, kept))
    }

    #[cfg(test)]
    fn pause_after_journal_sync_for_test() {
        let Some(marker) = std::env::var_os("T_HUB_TEST_PAUSE_AFTER_JOURNAL_SYNC") else {
            return;
        };
        std::fs::write(marker, b"synced").expect("writing journal sync test marker");
        loop {
            std::thread::park_timeout(Duration::from_secs(60));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Barrier};
    use t_hub_protocol::{JournalEventType, JournalSource};

    fn temp_dir(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("t-hub-journal-test-{tag}-{ts}"));
        dir
    }

    fn entry(kind: JournalEventType, entity: &str) -> EventJournalEntry {
        EventJournalEntry {
            seq: 0,
            timestamp_ms: 1,
            source: JournalSource::Hook,
            event_id: None,
            entity_id: Some(entity.to_string()),
            event_type: kind,
            payload: serde_json::json!({"k": entity}),
            result: None,
        }
    }

    fn identified_entry(event_id: &str, timestamp_ms: u64) -> EventJournalEntry {
        let mut entry = entry(JournalEventType::PermissionRequest, "identified");
        entry.event_id = Some(event_id.to_string());
        entry.timestamp_ms = timestamp_ms;
        entry
    }

    fn wait_for_path(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for test marker {path:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn spawn_helper(
        dir: &Path,
        operation: &str,
        entity: &str,
        opened: Option<&Path>,
        go: Option<&Path>,
        pause_after_sync: Option<&Path>,
        event_id: Option<&str>,
    ) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("journal::tests::journal_subprocess_helper")
            .arg("--nocapture")
            .env("T_HUB_TEST_JOURNAL_HELPER", operation)
            .env("T_HUB_TEST_JOURNAL_DIR", dir)
            .env("T_HUB_TEST_JOURNAL_ENTITY", entity)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(path) = opened {
            command.env("T_HUB_TEST_JOURNAL_OPENED", path);
        }
        if let Some(path) = go {
            command.env("T_HUB_TEST_JOURNAL_GO", path);
        }
        if let Some(path) = pause_after_sync {
            command.env("T_HUB_TEST_PAUSE_AFTER_JOURNAL_SYNC", path);
        }
        if let Some(event_id) = event_id {
            command.env("T_HUB_TEST_JOURNAL_EVENT_ID", event_id);
        }
        command.spawn().expect("spawning journal test helper")
    }

    #[test]
    fn journal_subprocess_helper() {
        let Ok(operation) = std::env::var("T_HUB_TEST_JOURNAL_HELPER") else {
            return;
        };
        let dir = PathBuf::from(std::env::var_os("T_HUB_TEST_JOURNAL_DIR").unwrap());
        let entity = std::env::var("T_HUB_TEST_JOURNAL_ENTITY").unwrap();
        let journal = Journal::open(&dir).unwrap();
        if let Some(path) = std::env::var_os("T_HUB_TEST_JOURNAL_OPENED") {
            std::fs::write(path, b"opened").unwrap();
        }
        if let Some(path) = std::env::var_os("T_HUB_TEST_JOURNAL_GO") {
            let path = PathBuf::from(path);
            wait_for_path(&path);
        }
        match operation.as_str() {
            "append" => {
                let mut entry = entry(JournalEventType::Notification, &entity);
                entry.event_id = std::env::var("T_HUB_TEST_JOURNAL_EVENT_ID").ok();
                journal.append(entry).unwrap();
            }
            other => panic!("unknown journal subprocess operation: {other}"),
        }
    }

    #[test]
    fn journal_path_precedence_and_relative_environment_resolution() {
        let home = Path::new("/home/tester");
        assert_eq!(
            resolve_journal_dir_from(
                Some("/explicit/journal"),
                Some(Path::new(".t-hub-dev/journal")),
                Some(home),
            ),
            PathBuf::from("/explicit/journal")
        );
        assert_eq!(
            resolve_journal_dir_from(None, Some(Path::new(".t-hub-dev/journal")), Some(home)),
            PathBuf::from("/home/tester/.t-hub-dev/journal")
        );
        assert_eq!(
            resolve_journal_dir_from(None, Some(Path::new("/var/tmp/journal")), Some(home)),
            PathBuf::from("/var/tmp/journal")
        );
        assert_eq!(
            resolve_journal_dir_from(None, None, Some(home)),
            PathBuf::from("/home/tester/.t-hub/journal")
        );
    }

    #[test]
    fn append_assigns_monotonic_seq_and_persists() {
        let dir = temp_dir("append");
        let j = Journal::open(&dir).unwrap();
        assert_eq!(j.head_seq(), 0);

        let a = j
            .append(entry(JournalEventType::SessionStart, "s1"))
            .unwrap();
        let b = j.append(entry(JournalEventType::Stop, "s1")).unwrap();
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(j.head_seq(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_and_out_of_order_provider_events_are_appended_once() {
        let dir = temp_dir("dedup-order");
        let journal = Journal::open(&dir).unwrap();
        let event_id =
            crate::event_identity::derive("codex", "permission_requested", &[("id", Some("1"))])
                .unwrap();

        let first = journal.append(identified_entry(&event_id, 200)).unwrap();
        let duplicate = journal.append(identified_entry(&event_id, 100)).unwrap();
        assert!(first.is_appended());
        assert!(!duplicate.is_appended());
        assert_eq!(duplicate.seq, first.seq);
        assert_eq!(journal.head_seq(), 1);

        let entries = journal.replay(0).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "a provider retry must not be re-journaled"
        );
        assert_eq!(entries[0].timestamp_ms, 200);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn provider_event_deduplication_survives_journal_restart() {
        let dir = temp_dir("dedup-restart");
        let event_id =
            crate::event_identity::derive("claude", "Stop", &[("event_id", Some("stop-1"))])
                .unwrap();
        Journal::open(&dir)
            .unwrap()
            .append(identified_entry(&event_id, 100))
            .unwrap();
        Journal::open(&dir)
            .unwrap()
            .append(identified_entry(&event_id, 200))
            .unwrap();

        assert_eq!(Journal::open(&dir).unwrap().replay(0).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_equivalent_provider_events_are_appended_once() {
        const WRITERS: usize = 8;
        let dir = temp_dir("dedup-concurrent");
        let event_id = crate::event_identity::derive(
            "codex",
            "turn_completed",
            &[("turn_id", Some("turn-1"))],
        )
        .unwrap();
        let writers = (0..WRITERS)
            .map(|_| Journal::open(&dir).unwrap())
            .collect::<Vec<_>>();
        let start = Arc::new(Barrier::new(WRITERS));
        let threads = writers
            .into_iter()
            .map(|journal| {
                let start = Arc::clone(&start);
                let event_id = event_id.clone();
                std::thread::spawn(move || {
                    start.wait();
                    journal.append(identified_entry(&event_id, 100)).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(Journal::open(&dir).unwrap().replay(0).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn events_without_provider_identity_remain_non_deduplicable() {
        let dir = temp_dir("no-identity");
        let journal = Journal::open(&dir).unwrap();
        journal
            .append(entry(JournalEventType::Stop, "opaque"))
            .unwrap();
        journal
            .append(entry(JournalEventType::Stop, "opaque"))
            .unwrap();
        assert_eq!(journal.replay(0).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stale_head_ledger_is_reconciled_from_the_journal() {
        let dir = temp_dir("dedup-reconcile");
        let event_id =
            crate::event_identity::derive("claude", "Stop", &[("event_id", Some("stop-1"))])
                .unwrap();
        Journal::open(&dir)
            .unwrap()
            .append(identified_entry(&event_id, 100))
            .unwrap();

        std::fs::write(
            dir.join(JOURNAL_HEAD_FILE),
            br#"{"version":1,"head_seq":1,"journal_len":0}"#,
        )
        .unwrap();
        let reopened = Journal::open(&dir).unwrap();
        let duplicate = reopened.append(identified_entry(&event_id, 200)).unwrap();
        assert!(!duplicate.is_appended());
        assert_eq!(reopened.replay(0).unwrap().len(), 1);

        let recovered: HeadState =
            serde_json::from_slice(&std::fs::read(dir.join(JOURNAL_HEAD_FILE)).unwrap()).unwrap();
        assert_eq!(recovered.version, HEAD_VERSION);
        assert_eq!(recovered.recent_event_ids.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recent_event_ledger_is_bounded_and_keeps_newest_identities() {
        let mut recent = Vec::new();
        for seq in 1..=(MAX_RECENT_EVENT_IDS as u64 + 2) {
            let provider_id = format!("provider-{seq}");
            let event_id = crate::event_identity::derive(
                "codex",
                "turn_completed",
                &[("turn_id", Some(&provider_id))],
            )
            .unwrap();
            Journal::remember_event_id(&mut recent, &event_id, seq);
        }
        assert_eq!(recent.len(), MAX_RECENT_EVENT_IDS);
        assert_eq!(recent.first().unwrap().seq, 3);
        assert_eq!(recent.last().unwrap().seq, MAX_RECENT_EVENT_IDS as u64 + 2);

        let serialized = serde_json::to_vec(&HeadState {
            version: HEAD_VERSION,
            head_seq: MAX_RECENT_EVENT_IDS as u64 + 2,
            journal_len: 0,
            recent_event_ids: recent,
        })
        .unwrap();
        assert!(serialized.len() <= MAX_HEAD_BYTES);
    }

    #[test]
    fn noisy_identity_window_has_matching_live_and_restart_eviction_policy() {
        let dir = temp_dir("dedup-window-policy");
        let journal = Journal::open(&dir).unwrap();
        let mut event_ids = Vec::new();
        for index in 0..=MAX_RECENT_EVENT_IDS {
            let provider_id = format!("noisy-event-{index}");
            let event_id = crate::event_identity::derive(
                "codex",
                "permission_requested",
                &[("request_id", Some(&provider_id))],
            )
            .unwrap();
            assert!(journal
                .append(identified_entry(&event_id, index as u64))
                .unwrap()
                .is_appended());
            event_ids.push(event_id);
        }

        let oldest_retry = journal
            .append(identified_entry(&event_ids[0], 10_000))
            .unwrap();
        assert!(
            oldest_retry.is_appended(),
            "an ID evicted from the bounded durable window must not remain in a live-only cache"
        );
        drop(journal);

        let restarted = Journal::open(&dir).unwrap();
        let next_evicted_retry = restarted
            .append(identified_entry(&event_ids[1], 10_001))
            .unwrap();
        assert!(
            next_evicted_retry.is_appended(),
            "restart must enforce the same bounded eviction policy"
        );
        let newest_retry = restarted
            .append(identified_entry(event_ids.last().unwrap(), 10_002))
            .unwrap();
        assert!(
            !newest_retry.is_appended(),
            "an identity still inside the durable window must be suppressed"
        );
        assert_eq!(restarted.replay(0).unwrap().len(), MAX_RECENT_EVENT_IDS + 3);
        assert!(
            std::fs::metadata(dir.join(JOURNAL_HEAD_FILE))
                .unwrap()
                .len()
                <= MAX_HEAD_BYTES as u64
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_provider_identity_is_rejected_without_journal_mutation() {
        let dir = temp_dir("invalid-event-id");
        let journal = Journal::open(&dir).unwrap();
        let mut invalid = entry(JournalEventType::Notification, "invalid");
        invalid.event_id = Some("raw-provider-secret-canary".to_string());
        let error = journal.append(invalid).unwrap_err();
        assert!(
            error.to_string().contains("canonical provider identity"),
            "unexpected invalid identity error: {error:#}"
        );
        assert_eq!(journal.byte_len(), 0);
        assert_eq!(journal.head_seq(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_short_lived_writers_allocate_one_monotonic_sequence() {
        const WRITERS: usize = 8;
        const ENTRIES_PER_WRITER: usize = 32;

        let dir = temp_dir("concurrent-writers");
        let writers = (0..WRITERS)
            .map(|_| Journal::open(&dir).unwrap())
            .collect::<Vec<_>>();
        let start = Arc::new(Barrier::new(WRITERS));
        let threads = writers
            .into_iter()
            .enumerate()
            .map(|(writer_index, journal)| {
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    for entry_index in 0..ENTRIES_PER_WRITER {
                        journal
                            .append(entry(
                                JournalEventType::Notification,
                                &format!("writer-{writer_index}-entry-{entry_index}"),
                            ))
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap();
        }

        let lines = std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap();
        let entries = lines
            .lines()
            .map(|line| serde_json::from_str::<EventJournalEntry>(line).unwrap())
            .collect::<Vec<_>>();
        let expected = WRITERS * ENTRIES_PER_WRITER;
        assert_eq!(
            entries.len(),
            expected,
            "no append may be lost or interleaved"
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<BTreeSet<_>>(),
            (1..=expected as u64).collect(),
            "every persisted sequence must be unique and gap-free"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_short_lived_processes_allocate_one_monotonic_sequence() {
        const WRITERS: usize = 12;

        let dir = temp_dir("concurrent-processes");
        let go = dir.join("go");
        std::fs::create_dir_all(&dir).unwrap();
        let mut children = (0..WRITERS)
            .map(|index| {
                spawn_helper(
                    &dir,
                    "append",
                    &format!("process-{index}"),
                    None,
                    Some(&go),
                    None,
                    None,
                )
            })
            .collect::<Vec<_>>();
        std::fs::write(&go, b"go").unwrap();

        for child in children.drain(..) {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "journal helper failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let entries = Journal::open(&dir).unwrap().replay(0).unwrap();
        assert_eq!(entries.len(), WRITERS);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<BTreeSet<_>>(),
            (1..=WRITERS as u64).collect()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_short_lived_processes_deduplicate_one_provider_event() {
        const WRITERS: usize = 12;

        let dir = temp_dir("concurrent-process-dedup");
        let go = dir.join("go");
        let event_id = crate::event_identity::derive(
            "codex",
            "permission_requested",
            &[("request_id", Some("shared-request"))],
        )
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let mut children = (0..WRITERS)
            .map(|index| {
                spawn_helper(
                    &dir,
                    "append",
                    &format!("process-{index}"),
                    None,
                    Some(&go),
                    None,
                    Some(&event_id),
                )
            })
            .collect::<Vec<_>>();
        std::fs::write(&go, b"go").unwrap();

        for child in children.drain(..) {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "journal helper failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let entries = Journal::open(&dir).unwrap().replay(0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[0].event_id.as_deref(), Some(event_id.as_str()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn noisy_hook_writer_fails_within_the_lock_budget() {
        let dir = temp_dir("bounded-lock");
        let blocker = Journal::open(&dir).unwrap();
        let writer = Journal::open(&dir).unwrap();
        let _held = blocker.lock_exclusive().unwrap();
        let started = Instant::now();
        let error = writer
            .append(entry(JournalEventType::Notification, "bounded-hook"))
            .unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            error.to_string().contains("remained busy"),
            "unexpected lock error: {error:#}"
        );
        assert!(
            elapsed >= JOURNAL_LOCK_TIMEOUT && elapsed < Duration::from_secs(6),
            "lock wait must be bounded near {:?}, got {elapsed:?}",
            JOURNAL_LOCK_TIMEOUT
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn existing_journal_files_are_restricted_to_owner_access() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let dir = temp_dir("private-permissions");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [JOURNAL_FILE, JOURNAL_LOCK_FILE, JOURNAL_HEAD_FILE] {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .mode(0o666)
                .open(dir.join(name))
                .unwrap();
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o666))
                .unwrap();
        }

        Journal::open(&dir)
            .unwrap()
            .append(entry(JournalEventType::Notification, "private"))
            .unwrap();

        for name in [JOURNAL_FILE, JOURNAL_LOCK_FILE, JOURNAL_HEAD_FILE] {
            let mode = std::fs::metadata(dir.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name} must remain private");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_append_is_rejected_without_mutating_the_journal() {
        let dir = temp_dir("oversized-append");
        let journal = Journal::open(&dir).unwrap();
        let before = journal.byte_len();
        let mut oversized = entry(JournalEventType::Notification, "oversized");
        oversized.payload = serde_json::json!({"data": "x".repeat(MAX_ENTRY_BYTES)});

        let error = journal.append(oversized).unwrap_err();
        assert!(
            error.to_string().contains("entry exceeds"),
            "unexpected oversized append error: {error:#}"
        );
        assert_eq!(journal.byte_len(), before);
        assert_eq!(journal.head_seq(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_persisted_line_is_rejected_during_recovery() {
        let dir = temp_dir("oversized-line");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(JOURNAL_FILE);
        let mut file = Journal::open_private_append(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_ENTRY_BYTES + 1]).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();

        let error = Journal::open(&dir).err().expect("oversized line must fail");
        assert!(
            error.to_string().contains("entry exceeds"),
            "unexpected oversized line error: {error:#}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_journal_is_rejected_before_scanning() {
        let dir = temp_dir("oversized-scan");
        std::fs::create_dir_all(&dir).unwrap();
        let file = Journal::open_private_file(&dir.join(JOURNAL_FILE), true).unwrap();
        file.set_len(MAX_SCAN_BYTES + 1).unwrap();

        let error = Journal::open(&dir)
            .err()
            .expect("oversized journal must fail");
        assert!(
            error.to_string().contains("scan limit"),
            "unexpected oversized journal error: {error:#}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopen_recovers_head_seq() {
        let dir = temp_dir("reopen");
        {
            let j = Journal::open(&dir).unwrap();
            j.append(entry(JournalEventType::SessionStart, "s1"))
                .unwrap();
            j.append(entry(JournalEventType::UserPromptSubmit, "s1"))
                .unwrap();
            j.append(entry(JournalEventType::Stop, "s1")).unwrap();
        }
        let j2 = Journal::open(&dir).unwrap();
        assert_eq!(j2.head_seq(), 3, "head_seq must survive reopen");
        let next = j2
            .append(entry(JournalEventType::SessionEnd, "s1"))
            .unwrap();
        assert_eq!(next.seq, 4);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replay_filters_by_cursor() {
        let dir = temp_dir("replay");
        let j = Journal::open(&dir).unwrap();
        for _ in 0..5 {
            j.append(entry(JournalEventType::Notification, "s1"))
                .unwrap();
        }
        let all = j.replay(0).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[4].seq, 5);

        let tail = j.replay(3).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].seq, 4);
        assert_eq!(tail[1].seq, 5);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn head_seq_on_disk_observes_other_process_appends() {
        // Two separate Journal handles on the same dir model the two processes:
        // `writer` is a short-lived --hook process; `tailer` is the long-lived
        // --stdio agent. The tailer's in-memory head must NOT see the writer's
        // appends, but head_seq_on_disk() must.
        let dir = temp_dir("disk-head");
        let tailer = Journal::open(&dir).unwrap();
        assert_eq!(tailer.head_seq(), 0);
        assert_eq!(tailer.head_seq_on_disk(), 0);

        {
            let writer = Journal::open(&dir).unwrap();
            writer
                .append(entry(JournalEventType::SessionStart, "s1"))
                .unwrap();
            writer.append(entry(JournalEventType::Stop, "s1")).unwrap();
        }

        // In-memory head is stale (this handle never appended).
        assert_eq!(
            tailer.head_seq(),
            0,
            "in-memory head must not see other-process appends"
        );
        // On-disk head observes the file growth...
        assert_eq!(
            tailer.head_seq_on_disk(),
            2,
            "disk head must see the 2 appended entries"
        );
        // ...and advances the in-memory head so a follow-up replay is consistent.
        assert_eq!(tailer.head_seq(), 2);
        let streamed = tailer.replay(0).unwrap();
        assert_eq!(streamed.len(), 2);
        assert_eq!(streamed[1].event_type, JournalEventType::Stop);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tail_from_reads_incrementally_and_handles_shrink() {
        let dir = temp_dir("tail-from");
        let j = Journal::open(&dir).unwrap();

        // Seed two entries; tailing from the start sees both and reaches EOF.
        j.append(entry(JournalEventType::SessionStart, "s1"))
            .unwrap();
        j.append(entry(JournalEventType::Stop, "s1")).unwrap();
        let (batch1, off1, seq1) = j.tail_from(0, 0).unwrap();
        assert_eq!(batch1.len(), 2);
        assert_eq!(seq1, 2);
        assert_eq!(off1, j.byte_len(), "offset must reach EOF");

        // No new data → empty result, cursor unchanged (the cheap hot path).
        let (none, off_none, seq_none) = j.tail_from(off1, seq1).unwrap();
        assert!(none.is_empty());
        assert_eq!((off_none, seq_none), (off1, seq1));

        // One more append → only that entry streams, seq continues.
        j.append(entry(JournalEventType::SessionEnd, "s1")).unwrap();
        let (batch2, off2, seq2) = j.tail_from(off1, seq1).unwrap();
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].event_type, JournalEventType::SessionEnd);
        assert_eq!(seq2, 3);
        assert!(off2 > off1);

        // Shrink (compaction/rotation): a stale offset past the new EOF restarts
        // from the top rather than skipping the renumbered contents.
        std::fs::write(dir.join(JOURNAL_FILE), b"").unwrap();
        let fresh = Journal::open(&dir).unwrap();
        fresh
            .append(entry(JournalEventType::Notification, "s2"))
            .unwrap();
        let (batch3, _off3, seq3) = j.tail_from(off2, seq2).unwrap();
        assert_eq!(batch3.len(), 1, "shrink must restart the read from the top");
        assert_eq!(seq3, 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compact_drops_status_keeps_durable_entries() {
        let status = |entity: &str| EventJournalEntry {
            seq: 0,
            timestamp_ms: 1,
            source: JournalSource::Status,
            event_id: None,
            entity_id: Some(entity.to_string()),
            event_type: JournalEventType::Unknown,
            payload: serde_json::json!({"status": {"context_window": {"used_percentage": 42}}}),
            result: None,
        };

        let dir = temp_dir("compact");
        let j = Journal::open(&dir).unwrap();
        // Interleave durable (Hook) and ephemeral (Status) entries.
        j.append(entry(JournalEventType::SessionStart, "s1"))
            .unwrap();
        j.append(status("s1")).unwrap();
        j.append(entry(JournalEventType::Stop, "s1")).unwrap();
        j.append(status("s1")).unwrap();
        let before_len = j.byte_len();

        let (before, after, kept) = j.compact_dropping_status().unwrap();
        assert_eq!(before, before_len);
        assert!(after < before, "file must shrink after dropping status");
        assert_eq!(kept, 2, "only the 2 durable entries remain");
        assert_eq!(j.head_seq(), 2, "in-memory head resyncs to the kept count");

        // The reopened handle still appends correctly, and no Status survives.
        let next = j.append(entry(JournalEventType::SessionEnd, "s1")).unwrap();
        assert_eq!(next.seq, 3);
        let remaining = j.replay(0).unwrap();
        assert_eq!(remaining.len(), 3);
        assert!(remaining.iter().all(|e| e.source != JournalSource::Status));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compaction_preserves_unknown_bytes_without_consuming_event_sequence() {
        let dir = temp_dir("compact-garbage-sequence");
        let journal = Journal::open(&dir).unwrap();
        journal
            .append(entry(JournalEventType::SessionStart, "first"))
            .unwrap();
        drop(journal);

        let path = dir.join(JOURNAL_FILE);
        let mut file = Journal::open_private_append(&path).unwrap();
        file.write_all(b"future-non-json-record\n").unwrap();
        file.sync_all().unwrap();
        drop(file);

        let journal = Journal::open(&dir).unwrap();
        let (_, _, kept) = journal.compact_dropping_status().unwrap();
        assert_eq!(kept, 1, "only parseable events consume sequence numbers");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("future-non-json-record"));

        let appended = journal
            .append(entry(JournalEventType::SessionEnd, "second"))
            .unwrap();
        assert_eq!(appended.seq, 2);
        let replayed = journal.replay(0).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[1].seq, appended.seq);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writer_opened_before_compaction_appends_to_published_journal() {
        let dir = temp_dir("compact-concurrent-writer");
        let compactor = Journal::open(&dir).unwrap();
        compactor
            .append(entry(JournalEventType::SessionStart, "seed"))
            .unwrap();

        // A short-lived hook can open the journal immediately before the
        // long-lived agent publishes a compacted replacement.
        let hook_writer = Journal::open(&dir).unwrap();
        compactor.compact_dropping_status().unwrap();
        hook_writer
            .append(entry(JournalEventType::Stop, "concurrent-hook"))
            .unwrap();

        let published = Journal::open(&dir).unwrap().replay(0).unwrap();
        assert!(
            published
                .iter()
                .any(|entry| entry.entity_id.as_deref() == Some("concurrent-hook")),
            "a hook append must never land on the retired pre-compaction inode"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn short_lived_process_opened_before_compaction_appends_to_published_journal() {
        let dir = temp_dir("compact-concurrent-process");
        let compactor = Journal::open(&dir).unwrap();
        compactor
            .append(entry(JournalEventType::SessionStart, "seed"))
            .unwrap();

        let opened = dir.join("opened");
        let go = dir.join("go");
        let child = spawn_helper(
            &dir,
            "append",
            "concurrent-process-hook",
            Some(&opened),
            Some(&go),
            None,
            None,
        );
        wait_for_path(&opened);
        compactor.compact_dropping_status().unwrap();
        std::fs::write(&go, b"go").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "journal helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let published = Journal::open(&dir).unwrap().replay(0).unwrap();
        assert!(
            published
                .iter()
                .any(|entry| entry.entity_id.as_deref() == Some("concurrent-process-hook")),
            "a subprocess append must never land on the retired pre-compaction inode"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_after_journal_sync_recovers_durable_entry_and_sequence() {
        let dir = temp_dir("sigkill-recovery");
        std::fs::create_dir_all(&dir).unwrap();
        let synced = dir.join("synced");
        let event_id = crate::event_identity::derive(
            "codex",
            "permission_requested",
            &[("request_id", Some("killed-request"))],
        )
        .unwrap();
        let mut child = spawn_helper(
            &dir,
            "append",
            "killed-after-sync",
            None,
            None,
            Some(&synced),
            Some(&event_id),
        );
        wait_for_path(&synced);
        let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
        assert_eq!(rc, 0, "failed to SIGKILL journal helper");
        let status = child.wait().unwrap();
        assert!(!status.success(), "SIGKILLed helper unexpectedly succeeded");

        let recovered = Journal::open(&dir).unwrap();
        assert_eq!(
            recovered.head_seq(),
            1,
            "the fsynced entry must survive a stale head"
        );
        let duplicate = recovered.append(identified_entry(&event_id, 2)).unwrap();
        assert!(
            !duplicate.is_appended(),
            "recovery must rebuild dedup state from the durable journal"
        );
        let next = recovered
            .append(entry(JournalEventType::SessionEnd, "after-kill"))
            .unwrap();
        assert_eq!(next.seq, 2);
        let entries = recovered.replay(0).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entity_id.as_deref(), Some("killed-after-sync"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn torn_trailing_line_is_tolerated_on_open() {
        let dir = temp_dir("torn");
        {
            let j = Journal::open(&dir).unwrap();
            j.append(entry(JournalEventType::SessionStart, "s1"))
                .unwrap();
        }
        // Simulate a crash mid-append: a partial, unterminated garbage line.
        let path = dir.join(JOURNAL_FILE);
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"seq\":2,\"timestamp_ms\":2,\"sour")
                .unwrap();
        }
        // Reopen: the torn tail must not be counted, and the next append is seq 2.
        let j2 = Journal::open(&dir).unwrap();
        assert_eq!(j2.head_seq(), 1, "torn tail must not inflate head_seq");
        let appended = j2
            .append(entry(JournalEventType::SessionEnd, "s1"))
            .unwrap();
        assert_eq!(appended.seq, 2);
        assert_eq!(
            j2.replay(0).unwrap().len(),
            2,
            "the repaired tail must not hide the next durable append"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
