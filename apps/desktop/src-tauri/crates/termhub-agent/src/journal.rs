//! The durable, append-only **event journal** (PLAN.md Workstream A, PRD §8).
//!
//! The journal is the authority for reconstruction *intent*: it survives the
//! Windows app closing (it lives on the WSL VHDX), and is replayed to the core
//! on every reconnect. It is an append-only file of newline-delimited JSON —
//! one [`EventJournalEntry`] per line — so it is crash-tolerant by construction
//! (a torn final line is detected and ignored on open).
//!
//! ## Durability
//! Each [`Journal::append`] writes the line and `fsync`s the file before
//! returning, so an appended entry is durable the moment the call returns. The
//! sequence number is the entry's 1-based position; we recover the head
//! sequence on open by counting valid lines.
//!
//! ## Why a file, not SQLite (here)
//! The agent's journal is a *write-mostly, append-only, replay-from-cursor* log;
//! a flat NDJSON file with `fsync` is the simplest thing that gives the needed
//! durability + ordered replay, and it is trivially inspectable for debugging.
//! (The Windows core keeps its own SQLite catalog; that is a separate concern.)

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use termhub_protocol::EventJournalEntry;

/// Default journal location relative to `$HOME`: `~/.termhub/journal`.
const JOURNAL_SUBDIR: &str = ".termhub/journal";
/// The append-only log file name within the journal directory.
const JOURNAL_FILE: &str = "events.ndjson";

/// Resolve the journal directory: an explicit override, else `$HOME/.termhub/
/// journal`, else a process-relative fallback.
pub fn resolve_journal_dir(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir.filter(|d| !d.trim().is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join(JOURNAL_SUBDIR);
    }
    PathBuf::from(JOURNAL_SUBDIR)
}

/// An open append-only journal. Cheap to clone-share behind an `Arc`; all
/// mutation goes through the internal `Mutex<File>` so concurrent appends from
/// multiple request handlers are serialized and never interleave a line.
pub struct Journal {
    path: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    file: File,
    /// Highest sequence appended so far (0 = empty journal).
    head_seq: u64,
}

impl Journal {
    /// Open (creating if needed) the journal under `dir`. Recovers the head
    /// sequence by scanning existing valid lines; a torn trailing line (from a
    /// crash mid-append) is tolerated — it is simply not counted, and the next
    /// append starts a clean line after it.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating journal dir {dir:?}"))?;
        let path = dir.join(JOURNAL_FILE);

        // Count existing complete lines to recover head_seq. A "complete" line is
        // one terminated by `\n` AND parseable as an EventJournalEntry; anything
        // after the last good newline is treated as a torn tail.
        let head_seq = Self::recover_head_seq(&path)?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .with_context(|| format!("opening journal file {path:?}"))?;

        Ok(Self {
            path,
            inner: Mutex::new(Inner { file, head_seq }),
        })
    }

    /// Scan the file (if any) and return the number of complete, parseable
    /// lines — that is the recovered head sequence.
    fn recover_head_seq(path: &Path) -> Result<u64> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).with_context(|| format!("reading journal {path:?}")),
        };
        let reader = BufReader::new(file);
        let mut count: u64 = 0;
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                // An io error mid-scan (rare) — stop counting; treat the rest as
                // torn. We have a consistent prefix up to here.
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            // Only count lines that parse; a partial/garbage trailing line is
            // ignored (crash tolerance).
            if serde_json::from_str::<EventJournalEntry>(&line).is_ok() {
                count += 1;
            }
        }
        Ok(count)
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
        let on_disk = Self::recover_head_seq(&self.path).unwrap_or(0);
        let mut guard = self.inner.lock().expect("journal mutex poisoned");
        if on_disk > guard.head_seq {
            guard.head_seq = on_disk;
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
    pub fn append(&self, mut entry: EventJournalEntry) -> Result<EventJournalEntry> {
        let mut guard = self.inner.lock().expect("journal mutex poisoned");
        let seq = guard.head_seq + 1;
        entry.seq = seq;

        let line = serde_json::to_string(&entry).context("serializing journal entry")?;
        // One line, newline-terminated. write_all + flush + sync_data gives us
        // durability before we acknowledge.
        guard
            .file
            .write_all(line.as_bytes())
            .context("writing journal line")?;
        guard.file.write_all(b"\n").context("writing journal newline")?;
        guard.file.flush().context("flushing journal")?;
        guard.file.sync_data().context("fsync journal")?;

        guard.head_seq = seq;
        Ok(entry)
    }

    /// Read back all entries with `seq > after_seq`, in order, for replay to the
    /// core. `after_seq == 0` replays the whole journal. Torn/garbage lines are
    /// skipped (same tolerance as recovery).
    pub fn replay(&self, after_seq: u64) -> Result<Vec<EventJournalEntry>> {
        // Take the lock to get a consistent view, then read from the start of the
        // file via a fresh handle so we don't disturb the append cursor.
        let _guard = self.inner.lock().expect("journal mutex poisoned");
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).context("opening journal for replay"),
        };
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(0)).ok();

        let mut out = Vec::new();
        let mut seq: u64 = 0;
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EventJournalEntry>(&line) {
                Ok(mut entry) => {
                    seq += 1;
                    if seq > after_seq {
                        entry.seq = seq;
                        out.push(entry);
                    }
                }
                Err(_) => {
                    // Torn tail — stop; everything after is unreliable.
                    break;
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
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0, 0));
            }
            Err(e) => return Err(e).context("opening journal for tail"),
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        // Compaction/rotation/truncation: the file is smaller than where we were,
        // so our byte offset is stale — restart from the top with a fresh seq.
        let (mut pos, mut seq) = if len < offset { (0, 0) } else { (offset, last_seq) };

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(pos))
            .with_context(|| format!("seeking journal to {pos}"))?;

        let mut out = Vec::new();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break; // EOF
            }
            // Only consume a line terminated by '\n'; leave a partial trailing
            // line for the next call (don't advance past it).
            if buf.last() != Some(&b'\n') {
                break;
            }
            pos += n as u64;
            let line = std::str::from_utf8(&buf[..n - 1]).unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            // Skip torn/garbage lines but still advance past them (same tolerance
            // as recovery/replay); only count parseable entries toward `seq`.
            if let Ok(mut e) = serde_json::from_str::<EventJournalEntry>(line) {
                seq += 1;
                e.seq = seq;
                out.push(e);
            }
        }
        Ok((out, pos, seq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termhub_protocol::{JournalEventType, JournalSource};

    fn temp_dir(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("termhub-journal-test-{tag}-{ts}"));
        dir
    }

    fn entry(kind: JournalEventType, entity: &str) -> EventJournalEntry {
        EventJournalEntry {
            seq: 0,
            timestamp_ms: 1,
            source: JournalSource::Hook,
            entity_id: Some(entity.to_string()),
            event_type: kind,
            payload: serde_json::json!({"k": entity}),
            result: None,
        }
    }

    #[test]
    fn append_assigns_monotonic_seq_and_persists() {
        let dir = temp_dir("append");
        let j = Journal::open(&dir).unwrap();
        assert_eq!(j.head_seq(), 0);

        let a = j.append(entry(JournalEventType::SessionStart, "s1")).unwrap();
        let b = j.append(entry(JournalEventType::Stop, "s1")).unwrap();
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(j.head_seq(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopen_recovers_head_seq() {
        let dir = temp_dir("reopen");
        {
            let j = Journal::open(&dir).unwrap();
            j.append(entry(JournalEventType::SessionStart, "s1")).unwrap();
            j.append(entry(JournalEventType::UserPromptSubmit, "s1")).unwrap();
            j.append(entry(JournalEventType::Stop, "s1")).unwrap();
        }
        let j2 = Journal::open(&dir).unwrap();
        assert_eq!(j2.head_seq(), 3, "head_seq must survive reopen");
        let next = j2.append(entry(JournalEventType::SessionEnd, "s1")).unwrap();
        assert_eq!(next.seq, 4);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replay_filters_by_cursor() {
        let dir = temp_dir("replay");
        let j = Journal::open(&dir).unwrap();
        for _ in 0..5 {
            j.append(entry(JournalEventType::Notification, "s1")).unwrap();
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
            writer.append(entry(JournalEventType::SessionStart, "s1")).unwrap();
            writer.append(entry(JournalEventType::Stop, "s1")).unwrap();
        }

        // In-memory head is stale (this handle never appended).
        assert_eq!(tailer.head_seq(), 0, "in-memory head must not see other-process appends");
        // On-disk head observes the file growth...
        assert_eq!(tailer.head_seq_on_disk(), 2, "disk head must see the 2 appended entries");
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
        j.append(entry(JournalEventType::SessionStart, "s1")).unwrap();
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
        fresh.append(entry(JournalEventType::Notification, "s2")).unwrap();
        let (batch3, _off3, seq3) = j.tail_from(off2, seq2).unwrap();
        assert_eq!(batch3.len(), 1, "shrink must restart the read from the top");
        assert_eq!(seq3, 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn torn_trailing_line_is_tolerated_on_open() {
        let dir = temp_dir("torn");
        {
            let j = Journal::open(&dir).unwrap();
            j.append(entry(JournalEventType::SessionStart, "s1")).unwrap();
        }
        // Simulate a crash mid-append: a partial, unterminated garbage line.
        let path = dir.join(JOURNAL_FILE);
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"seq\":2,\"timestamp_ms\":2,\"sour").unwrap();
        }
        // Reopen: the torn tail must not be counted, and the next append is seq 2.
        let j2 = Journal::open(&dir).unwrap();
        assert_eq!(j2.head_seq(), 1, "torn tail must not inflate head_seq");

        std::fs::remove_dir_all(&dir).ok();
    }
}
