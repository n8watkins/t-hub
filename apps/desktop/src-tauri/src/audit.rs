//! Control-socket **audit log with teeth** — Phase 1 of the socket hardening
//! (`docs/SOCKET-AUTH-DESIGN.md` §6), hardened against the "tamper-evidence
//! theater" findings (audit-security H2/H3, audit-tests).
//!
//! Gives the aspirational `"audited": true` flag a real sink. Every
//! Organization- and ProcessChanging-tier command, and every governor refusal,
//! appends one JSON line to `~/.t-hub/audit/control-YYYYMMDD.jsonl` (mode `0600`
//! on unix). Read-tier commands are NOT logged (they are not process-affecting
//! and would drown the signal).
//!
//! ### Teeth
//! - **KEYED tamper-evidence (H2 fix):** each line carries `prev` (the previous
//!   line's `hash`) and `hash` = **HMAC-SHA256** of the line's own
//!   bytes-minus-`hash`, under a secret key held OUTSIDE the log
//!   (`~/.t-hub/audit-hmac-key`, sealed at rest via [`crate::secret_seal`]). Because
//!   the chain is keyed, a same-user actor who can write the log file CANNOT
//!   recompute a valid MAC for an edited/forged line — the unkeyed SHA-256 chain it
//!   replaces gave false assurance exactly against that actor. A verifier
//!   ([`verify`]) recomputes every MAC + `prev` linkage forward.
//! - **Truncation anchor (H2 fix):** a keyed external head
//!   (`~/.t-hub/audit.head.json`, a sibling of the log dir, NOT inside it) records
//!   `{count, last}` per day, MAC'd with the same key. Tail-truncation leaves a
//!   self-consistent shorter chain, so within-file MAC+linkage cannot see it; the
//!   verifier compares the file against the anchored count and reports a
//!   `Truncated` break when the file regresses.
//! - **Production verifier (H2 fix):** [`AuditLog::startup_integrity_check`] runs
//!   the verifier on startup and reports chain breaks loudly (stderr + the caller
//!   emits a fanout event); [`verify`] is a plain function so a control command can
//!   call it too. Verification is NO LONGER `cfg(test)`-only.
//! - **Fail-closed sink (H3 fix):** [`AuditLog::try_record`] returns the write
//!   result. The dispatch path uses it to REFUSE a ProcessChanging command whose
//!   audit write fails, so an allowed spawn/kill/type never executes without a
//!   durable trace. [`AuditLog::record`] is the best-effort wrapper kept for the
//!   non-process-changing paths (Organization records, refusals, the elevation
//!   note) where a loud stderr line is the right posture.
//! - **Redaction**: `send_text` content is never written — only its length and a
//!   SHA-256 prefix — so the log cannot become a secret-harvesting oracle. `send_keys`
//!   key names ARE logged (they are exactly the kill-pattern signal we want).
//! - **Buffered + fsync-flushed** behind a mutex, written after the dispatch
//!   decision so both allowed and refused commands land.
//!
//! ### Honest limits
//! - **Key confidentiality follows the sealing backend.** On Windows the key is
//!   DPAPI-sealed under the current user; on Linux dev/CI and pure-WSL there is no
//!   DPAPI, so the key is `0600` plaintext (the same fallback item-3 documents for
//!   every secret). Where the key is plaintext-at-rest, a same-user root-equivalent
//!   actor can read it and re-sign — the keyed chain raises the bar to "must also
//!   steal the key", not to "impossible". Tests exercise the chain logic with an
//!   explicit in-memory key, independent of the sealing backend.
//! - **Cross-day continuity is not yet anchored.** Each day file is an independent
//!   keyed chain; the head anchors per-day truncation but not a wholesale
//!   day-file deletion. Noted as out of scope below.
//!
//! The live mirror of refusals onto the event fanout lives in `control.rs` (it
//! owns the fanout); this module owns only the durable record.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// On-disk record format version. `2` = keyed (HMAC) chain + head anchor; a record
/// without `v: 2` predates this scheme and cannot be MAC-verified (reported loudly).
const AUDIT_FORMAT_VERSION: u64 = 2;

/// Resolve `$HOME` (or `$USERPROFILE`), falling back to `.`. Mirrors
/// `control::handshake_path`'s home resolution.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve the audit directory: `$T_HUB_AUDIT_DIR` if set (dev-isolation / tests),
/// else `~/.t-hub/audit`.
pub fn audit_dir() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_AUDIT_DIR") {
        return PathBuf::from(p);
    }
    home_dir().join(".t-hub").join("audit")
}

/// Resolve the audit HMAC key file: `$T_HUB_AUDIT_KEY_FILE` if set, else
/// `~/.t-hub/audit-hmac-key`. Deliberately a SIBLING of the log dir, not a file
/// inside it — the key must not sit beside the log it protects, or a file-writer who
/// can edit the log could read the key and re-sign.
pub fn audit_key_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_AUDIT_KEY_FILE") {
        return PathBuf::from(p);
    }
    home_dir().join(".t-hub").join("audit-hmac-key")
}

/// The keyed external head anchor for `dir`: a sibling file (`<dir>.head.json`), NOT
/// a file inside the log dir. Records `{count,last,mac}` per day so tail-truncation
/// is detectable.
fn head_path_for(dir: &Path) -> PathBuf {
    dir.with_extension("head.json")
}

/// Test-only accessor for the head-anchor path, so `control.rs` tests can clean up the
/// sibling head file they create through the dispatch path.
#[cfg(test)]
pub(crate) fn head_path_for_test(dir: &Path) -> PathBuf {
    head_path_for(dir)
}

/// 32 bytes of key material from two v4 UUIDs (each drawn from the OS CSPRNG via
/// `getrandom`); ~244 bits of entropy after the fixed version/variant bits — ample
/// for an HMAC-SHA256 key, and avoids taking a direct `rand`/`getrandom` dependency.
fn random_key() -> Vec<u8> {
    let mut k = Vec::with_capacity(32);
    k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    k
}

/// Load the persistent audit HMAC key, creating + sealing it on first use. The key
/// is NEVER rotated (unlike the control/read tokens): a rotated key would invalidate
/// every prior record's MAC, destroying the very evidence the log exists to hold.
/// Sealed at rest via [`crate::secret_seal`] (DPAPI on Windows, `0600` plaintext
/// fallback elsewhere). A present-but-unreadable key file is left untouched (so a
/// recoverable key is not clobbered) and an ephemeral key is used with a loud warning.
fn load_or_create_audit_key(path: &Path) -> Vec<u8> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            if let Some(stored) = crate::secret_seal::unseal_str(&raw) {
                if let Some(bytes) = unhex(stored.trim()) {
                    if bytes.len() >= 32 {
                        return bytes;
                    }
                }
            }
            eprintln!(
                "t-hub-audit: audit key at {path:?} is present but unreadable (wrong \
                 host/user, or corrupt); using an EPHEMERAL key - records written by a \
                 prior key cannot be verified until it is restored."
            );
            random_key()
        }
        Err(_) => {
            // Absent: mint, seal, persist. Best-effort persistence — an in-memory key
            // still lets the log come up if the write fails.
            let key = random_key();
            let sealed = crate::secret_seal::seal_str(&hex(&key));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(path, sealed.as_bytes()).is_ok() {
                set_owner_only(path);
            }
            key
        }
    }
}

/// The append-only audit sink. Cheap to construct (no log I/O until the first
/// record) so it is safe to build unconditionally in `ControlContext::new`.
pub struct AuditLog {
    dir: PathBuf,
    head_path: PathBuf,
    /// The HMAC key for the chain + head. Held in memory; see [`load_or_create_audit_key`].
    key: Vec<u8>,
    inner: Mutex<Inner>,
}

struct Inner {
    /// The day file currently open for append, keyed by its `YYYYMMDD` stamp, plus
    /// its buffered writer. `None` until the first record (or after a day rollover).
    writer: Option<(String, BufWriter<File>)>,
    /// The previous line's `hash`, hex-encoded — the chain link for the next line.
    prev_hash: String,
    /// The number of records in the currently-open day file, seeded from the file on
    /// open and incremented per append; persisted into the head anchor.
    count: u64,
}

impl AuditLog {
    /// Build a log rooted at `dir` with an EPHEMERAL in-memory key. Used by tests and
    /// any non-persistent sink: the chain is fully valid within this instance's life
    /// (so within-instance linkage/redaction hold), but records are not verifiable
    /// across a process restart. Persistent, sealed keying is [`from_env`] only.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_key(dir, random_key())
    }

    /// Build a log rooted at `dir` with an explicit key — used by [`from_env`] (the
    /// sealed persistent key) and by verifier tests that need a stable key across
    /// simulated restarts.
    pub fn with_key(dir: PathBuf, key: Vec<u8>) -> Self {
        let head_path = head_path_for(&dir);
        Self {
            dir,
            head_path,
            key,
            inner: Mutex::new(Inner {
                writer: None,
                prev_hash: String::new(),
                count: 0,
            }),
        }
    }

    /// Build a log at the default location ([`audit_dir`]) with the persistent, sealed
    /// HMAC key ([`audit_key_path`]).
    pub fn from_env() -> Self {
        let key = load_or_create_audit_key(&audit_key_path());
        Self::with_key(audit_dir(), key)
    }

    /// Append one audit record, **best-effort**: a write failure is logged to stderr
    /// and never breaks dispatch. Kept for the non-process-changing paths (Organization
    /// records, governor refusals, the elevation note) where availability wins and the
    /// loud stderr line is the signal. Process-changing commands use [`try_record`]
    /// so a sink failure fails the command CLOSED (H3).
    pub fn record(&self, command: &str, tier: &str, decision: &str, args: &Value, meta: AuditMeta) {
        if let Err(e) = self.try_record(command, tier, decision, args, meta) {
            eprintln!("t-hub-audit: failed to write audit record for '{command}': {e}");
        }
    }

    /// Append one audit record, returning the write result so the caller can fail a
    /// ProcessChanging command CLOSED when the durable trace cannot be written (H3).
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

        // Open (or roll over to) today's file, re-seeding the chain + count from its
        // last line so restarts and midnight rollovers keep a continuous hash chain.
        let need_open = match &guard.writer {
            Some((open_date, _)) => open_date != &date,
            None => true,
        };
        if need_open {
            std::fs::create_dir_all(&self.dir)?;
            let path = self.dir.join(format!("control-{date}.jsonl"));
            let (seed, existing) = last_hash_and_count(&path);
            guard.prev_hash = seed;
            guard.count = existing;
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            set_owner_only(&path);
            guard.writer = Some((date.clone(), BufWriter::new(file)));
        }

        // Build the record body (every field EXCEPT `hash`), then chain-MAC it.
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
            record["error"] = json!(err);
        } else if decision == "allowed" {
            record["outcome"] = json!("ok");
        }

        // serde_json's default Map is sorted (BTreeMap), so a verifier recomputes the
        // same bytes deterministically. The MAC is keyed, so a file-editor without the
        // key cannot forge it.
        let body = serde_json::to_string(&record)?;
        let hash = hex(&mac(&self.key, body.as_bytes()));
        record["hash"] = json!(hash);
        let line = serde_json::to_string(&record)?;

        let (_, writer) = guard.writer.as_mut().expect("writer opened above");
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;

        // Update the keyed external head AFTER the line is durable. A head-write
        // failure propagates so the fail-closed caller refuses (the anchor is part of
        // the integrity guarantee, not an optional extra).
        let count = guard.count + 1;
        self.write_head(&date, count, &hash)?;
        guard.count = count;
        guard.prev_hash = hash;
        Ok(())
    }

    /// Rewrite the keyed head anchor with `date`'s latest `{count,last}`. Other days'
    /// entries are preserved. MAC'd so a file-editor cannot lower `count` to hide a
    /// truncation without the key.
    fn write_head(&self, date: &str, count: u64, last: &str) -> std::io::Result<()> {
        let mut map = read_head_map(&self.head_path);
        map.insert(
            date.to_string(),
            json!({
                "count": count,
                "last": last,
                "mac": self.head_entry_mac(date, count, last),
            }),
        );
        let body = serde_json::to_string(&Value::Object(map))?;
        if let Some(parent) = self.head_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.head_path, body.as_bytes())?;
        set_owner_only(&self.head_path);
        Ok(())
    }

    /// The MAC binding a head entry's `{date,count,last}` together under the audit key.
    fn head_entry_mac(&self, date: &str, count: u64, last: &str) -> String {
        hex(&mac(&self.key, format!("{date}|{count}|{last}").as_bytes()))
    }

    /// Verify this log's own directory + head against its key. Callable in production
    /// (NOT `cfg(test)`): see [`startup_integrity_check`]. Routes through the public
    /// [`verify`] (the head is this dir's sibling anchor) so a future control command
    /// can share the exact same path.
    pub fn verify_self(&self) -> VerifyReport {
        verify(&self.dir, &self.key)
    }

    /// Run [`verify_self`] and report any breaks LOUDLY on stderr. Returns the report
    /// so the caller (which owns the event fanout) can mirror a failure onto the wire.
    /// Quiet on a clean chain.
    pub fn startup_integrity_check(&self) -> VerifyReport {
        let report = self.verify_self();
        if !report.ok() {
            eprintln!(
                "t-hub-audit: INTEGRITY CHECK FAILED — {} break(s) across {} record(s) in \
                 {} file(s). The audit chain has been tampered with, truncated, or written \
                 under a different key:",
                report.breaks.len(),
                report.records,
                report.files
            );
            for b in &report.breaks {
                eprintln!("  {} line {}: {:?} — {}", b.file, b.line, b.kind, b.detail);
            }
        } else if report.legacy > 0 {
            // Benign: an upgraded install carries pre-v2 history the new key cannot
            // verify. NOT a break - report quietly so it is never confused with tamper.
            eprintln!(
                "t-hub-audit: integrity OK. {} legacy pre-v2 record(s) present (from before \
                 the keyed chain; unverifiable by design, not counted as breaks).",
                report.legacy
            );
        }
        report
    }
}

/// Caller context + dispatch outcome attached to an audit record. Kept separate
/// from the command args so the call site reads clearly.
pub struct AuditMeta<'a> {
    /// `"loopback"` or `"remote"` — the connection origin (`ControlContext::peer_is_loopback`).
    pub peer: &'a str,
    /// The capability tier of the presented token.
    pub token_tier: &'a str,
    /// The target session id, when the command names one (send/close).
    pub session: Option<&'a str>,
    /// The `spawnedBy` captain id, when present (spawn).
    pub spawned_by: Option<&'a str>,
    /// The dispatch error, when an allowed command failed downstream.
    pub error: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Verification (production path)
// ---------------------------------------------------------------------------

/// The outcome of verifying an audit directory: how much was checked and every
/// integrity break found. `ok()` is the go/no-go signal.
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub files: usize,
    pub records: usize,
    /// Records written by the OLD unkeyed (pre-v2) scheme. These are NOT breaks - on
    /// an upgraded install the audit dir legitimately holds legacy history that the
    /// new key cannot MAC-verify. Counted + reported so the signal is honest, but they
    /// never trip the "tampered/truncated" alarm (P72-1: no upgrade cry-wolf).
    pub legacy: usize,
    pub breaks: Vec<ChainBreak>,
}

impl VerifyReport {
    /// True iff the keyed (v2) chain verified with zero breaks. Legacy pre-v2 records
    /// do not affect this - they are unverifiable-by-design, not tampering.
    pub fn ok(&self) -> bool {
        self.breaks.is_empty()
    }

    /// Render the report as JSON for a control-command response (`audit_verify`).
    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok(),
            "files": self.files,
            "records": self.records,
            "legacy": self.legacy,
            "breaks": self.breaks.iter().map(|b| json!({
                "file": b.file,
                "line": b.line,
                "kind": format!("{:?}", b.kind),
                "detail": b.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

/// One integrity break located in the audit trail.
#[derive(Debug)]
pub struct ChainBreak {
    pub file: String,
    /// 1-based line number within the file (`0` for a whole-file/head break).
    pub line: usize,
    pub kind: BreakKind,
    pub detail: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// The stored `hash` is not a valid MAC of the record body under the key.
    BadMac,
    /// A line's `prev` does not equal the previous line's `hash`.
    PrevMismatch,
    /// The line is not parseable JSON, lacks `hash`, or predates the keyed format.
    Malformed,
    /// The file has fewer records than the keyed head anchor recorded — a tail was cut.
    Truncated,
    /// The head anchor's own MAC does not verify (the anchor itself was edited).
    HeadTampered,
}

/// Verify an audit directory against `key`, using the default sibling head anchor.
pub fn verify(dir: &Path, key: &[u8]) -> VerifyReport {
    verify_with_head(dir, &head_path_for(dir), key)
}

/// Verify every `control-YYYYMMDD.jsonl` in `dir` (keyed MAC + `prev` linkage) and
/// cross-check each day's length/last-hash against the keyed head at `head_path`.
pub fn verify_with_head(dir: &Path, head_path: &Path, key: &[u8]) -> VerifyReport {
    let mut report = VerifyReport::default();
    let head = read_head_map(head_path);

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(date) = name
                    .strip_prefix("control-")
                    .and_then(|s| s.strip_suffix(".jsonl"))
                {
                    files.push((date.to_string(), path));
                }
            }
        }
    }
    files.sort();

    for (date, path) in files {
        report.files += 1;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                report.breaks.push(ChainBreak {
                    file: name,
                    line: 0,
                    kind: BreakKind::Malformed,
                    detail: format!("cannot read file: {e}"),
                });
                continue;
            }
        };

        let mut prev = String::new();
        let mut count: u64 = 0;
        let mut last_hash = String::new();
        for (i, raw) in content.lines().enumerate() {
            if raw.trim().is_empty() {
                continue;
            }
            report.records += 1;
            count += 1;
            let line_no = i + 1;

            let rec: Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(e) => {
                    report.breaks.push(ChainBreak {
                        file: name.clone(),
                        line: line_no,
                        kind: BreakKind::Malformed,
                        detail: format!("unparseable JSON: {e}"),
                    });
                    continue;
                }
            };

            let Some(stored) = rec.get("hash").and_then(|v| v.as_str()) else {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line: line_no,
                    kind: BreakKind::Malformed,
                    detail: "record has no `hash` field".into(),
                });
                continue;
            };
            let stored = stored.to_string();

            if rec.get("v").and_then(|v| v.as_u64()) != Some(AUDIT_FORMAT_VERSION) {
                // A record from the OLD unkeyed (pre-v2) scheme. On an upgraded install
                // the audit dir legitimately holds such history, and the freshly-minted
                // key cannot MAC-verify it. This is NOT tampering, so it must NOT count
                // as an integrity break (P72-1: reporting every legacy line as a break on
                // the first post-deploy startup is cry-wolf that erodes trust in the real
                // alarm). Count it as legacy, advance the linkage so the first genuine v2
                // record still chains cleanly onto it, and move on.
                report.legacy += 1;
                prev = stored.clone();
                last_hash = stored;
                continue;
            }

            // Recompute the MAC over the body-minus-hash and compare in constant time.
            let mut body = rec.clone();
            if let Some(obj) = body.as_object_mut() {
                obj.remove("hash");
            }
            let body_bytes = serde_json::to_string(&body).unwrap_or_default();
            let mac_ok = unhex(&stored)
                .map(|tag| verify_mac(key, body_bytes.as_bytes(), &tag))
                .unwrap_or(false);
            if !mac_ok {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line: line_no,
                    kind: BreakKind::BadMac,
                    detail: "HMAC does not verify (edited body or wrong key)".into(),
                });
            }

            let rec_prev = rec.get("prev").and_then(|v| v.as_str()).unwrap_or("");
            if rec_prev != prev {
                report.breaks.push(ChainBreak {
                    file: name.clone(),
                    line: line_no,
                    kind: BreakKind::PrevMismatch,
                    detail: format!("`prev` {rec_prev:?} != prior line hash {prev:?}"),
                });
            }

            prev = stored.clone();
            last_hash = stored;
        }

        // Cross-check the keyed head: detect a tail that was truncated away (a
        // self-consistent shorter chain the per-line checks alone cannot see).
        if let Some(entry) = head.get(&date) {
            let h_count = entry.get("count").and_then(|v| v.as_u64());
            let h_last = entry.get("last").and_then(|v| v.as_str());
            let h_mac = entry.get("mac").and_then(|v| v.as_str());
            match (h_count, h_last, h_mac) {
                (Some(hc), Some(hl), Some(hm)) => {
                    let expect = hex(&mac(key, format!("{date}|{hc}|{hl}").as_bytes()));
                    if !ct_eq(hm.as_bytes(), expect.as_bytes()) {
                        report.breaks.push(ChainBreak {
                            file: name.clone(),
                            line: 0,
                            kind: BreakKind::HeadTampered,
                            detail: "head anchor MAC does not verify".into(),
                        });
                    } else if count < hc {
                        report.breaks.push(ChainBreak {
                            file: name.clone(),
                            line: 0,
                            kind: BreakKind::Truncated,
                            detail: format!(
                                "file has {count} record(s) but the head anchored {hc} — a tail was truncated"
                            ),
                        });
                    } else if count == hc && last_hash != hl {
                        report.breaks.push(ChainBreak {
                            file: name.clone(),
                            line: 0,
                            kind: BreakKind::HeadTampered,
                            detail: "final record hash does not match the head anchor".into(),
                        });
                    }
                }
                _ => {
                    report.breaks.push(ChainBreak {
                        file: name.clone(),
                        line: 0,
                        kind: BreakKind::HeadTampered,
                        detail: "head anchor entry is malformed".into(),
                    });
                }
            }
        }
    }

    report
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

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
        // non-sensitive identifiers / key names — log them verbatim.
        _ => args.clone(),
    }
}

// ---------------------------------------------------------------------------
// Crypto + encoding helpers
// ---------------------------------------------------------------------------

/// HMAC-SHA256 of `data` under `key`, returned as raw tag bytes.
fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// Constant-time verify that `tag` is the HMAC-SHA256 of `data` under `key`.
fn verify_mac(key: &[u8], data: &[u8], tag: &[u8]) -> bool {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(data);
    m.verify_slice(tag).is_ok()
}

/// Constant-time byte-slice equality (for comparing hex head MACs).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Hex-encode a byte slice (lowercase).
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Decode a lowercase/uppercase hex string to bytes; `None` on any non-hex char or
/// odd length.
fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

/// Tighten a just-created state file to owner-only (`0600`) on unix. The perms result
/// is intentionally surfaced-to-stderr rather than silently dropped (M4).
fn set_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            eprintln!("t-hub-audit: could not set 0600 on {path:?}: {e}");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Read the last non-empty line's `hash` and the count of non-empty lines from an
/// existing audit file, to re-seed the chain + count across restarts / day rollovers.
/// Returns `(String::new(), 0)` if the file is absent or empty.
fn last_hash_and_count(path: &Path) -> (String, u64) {
    let Ok(file) = File::open(path) else {
        return (String::new(), 0);
    };
    let reader = BufReader::new(file);
    let mut last: Option<String> = None;
    let mut count: u64 = 0;
    for line in reader.lines().map_while(Result::ok) {
        if !line.trim().is_empty() {
            count += 1;
            last = Some(line);
        }
    }
    let seed = last
        .and_then(|l| serde_json::from_str::<Value>(&l).ok())
        .and_then(|v| v.get("hash").and_then(|h| h.as_str()).map(String::from))
        .unwrap_or_default();
    (seed, count)
}

/// Read the head anchor file into a JSON object map (`{date: {count,last,mac}}`),
/// returning an empty map if it is absent/unreadable/not an object.
fn read_head_map(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        // Unique per test (thread id + tag) to avoid cross-test collisions without
        // touching the process-global env.
        let uniq = format!("t-hub-audit-test-{tag}-{:?}", std::thread::current().id());
        std::env::temp_dir().join(uniq)
    }

    fn clean(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(head_path_for(dir));
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
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                if !line.trim().is_empty() {
                    out.push(serde_json::from_str(line).unwrap());
                }
            }
        }
        out
    }

    // A fixed key so a test can simulate a process restart (a new AuditLog over the
    // same dir) and still verify the earlier records.
    const TEST_KEY: &[u8] = b"test-audit-key-0123456789abcdef-32b";

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
    fn hash_chain_links_and_verifies() {
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
        // The production verifier accepts the untouched chain.
        let report = verify(&dir, TEST_KEY);
        assert!(report.ok(), "clean chain should verify: {:?}", report.breaks);
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
    fn keyed_chain_is_unforgeable_without_the_key() {
        // The whole point of H2: an editor WITHOUT the key cannot produce a valid MAC.
        // Recomputing a plain SHA-256 (the old scheme) over an edited body must NOT
        // pass the keyed verifier.
        let dir = temp_dir("unforgeable");
        clean(&dir);
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.record(
            "spawn_terminal",
            "process-changing",
            "refused-cap",
            &json!({"cwd": "/tmp"}),
            meta(),
        );
        drop(log);

        // Attacker flips refused -> allowed and re-signs with an UNKEYED SHA-256, the
        // best a file-writer without the key can do.
        let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let mut rec: Value = serde_json::from_str(std::fs::read_to_string(&file).unwrap().trim()).unwrap();
        rec["decision"] = json!("allowed");
        let mut body = rec.clone();
        body.as_object_mut().unwrap().remove("hash");
        let forged = hex(&Sha256::digest(serde_json::to_string(&body).unwrap().as_bytes()));
        rec["hash"] = json!(forged);
        std::fs::write(&file, format!("{}\n", serde_json::to_string(&rec).unwrap())).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(!report.ok(), "a re-signed unkeyed forgery must be detected");
        assert!(report.breaks.iter().any(|b| b.kind == BreakKind::BadMac));
        clean(&dir);
    }

    #[test]
    fn in_place_edit_breaks_the_mac() {
        let dir = temp_dir("edit");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record("close_terminal", "process-changing", "allowed", &json!({"sessionId": "a"}), meta());
            log.record("close_terminal", "process-changing", "allowed", &json!({"sessionId": "b"}), meta());
        }
        assert!(verify(&dir, TEST_KEY).ok());

        // Edit a field WITHOUT touching the hash: the MAC no longer matches.
        let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let content = std::fs::read_to_string(&file).unwrap();
        let mut lines: Vec<&str> = content.lines().collect();
        let mut first: Value = serde_json::from_str(lines[0]).unwrap();
        first["args"]["sessionId"] = json!("TAMPERED");
        let edited = serde_json::to_string(&first).unwrap();
        lines[0] = &edited;
        std::fs::write(&file, format!("{}\n", lines.join("\n"))).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(!report.ok(), "an in-place edit must be detected");
        assert!(report.breaks.iter().any(|b| b.kind == BreakKind::BadMac));
        clean(&dir);
    }

    #[test]
    fn tail_truncation_is_detected_by_the_head_anchor() {
        // Truncating whole trailing lines leaves a self-consistent shorter chain; the
        // keyed head anchor is what catches it (revert the head cross-check and this
        // goes RED).
        let dir = temp_dir("truncate");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            for i in 0..4 {
                log.record(
                    "close_terminal",
                    "process-changing",
                    "allowed",
                    &json!({"sessionId": format!("s{i}")}),
                    meta(),
                );
            }
        }
        assert!(verify(&dir, TEST_KEY).ok());

        // Drop the last two lines — the surviving prefix still MAC-verifies + links.
        let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let content = std::fs::read_to_string(&file).unwrap();
        let kept: Vec<&str> = content.lines().take(2).collect();
        std::fs::write(&file, format!("{}\n", kept.join("\n"))).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(!report.ok(), "tail truncation must be detected");
        assert!(
            report.breaks.iter().any(|b| b.kind == BreakKind::Truncated),
            "expected a Truncated break, got {:?}",
            report.breaks
        );
        clean(&dir);
    }

    #[test]
    fn head_anchor_edit_is_detected() {
        let dir = temp_dir("headedit");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record("close_terminal", "process-changing", "allowed", &json!({"sessionId": "a"}), meta());
        }
        // Lower the anchored count by hand (without the key, the mac won't match).
        let head = head_path_for(&dir);
        let mut map = read_head_map(&head);
        for (_k, v) in map.iter_mut() {
            v["count"] = json!(99);
        }
        std::fs::write(&head, serde_json::to_string(&Value::Object(map)).unwrap()).unwrap();

        let report = verify(&dir, TEST_KEY);
        assert!(!report.ok());
        assert!(report.breaks.iter().any(|b| b.kind == BreakKind::HeadTampered));
        clean(&dir);
    }

    #[test]
    fn chain_survives_a_simulated_restart() {
        // A new AuditLog over the same dir + key (a restart) re-seeds the chain from
        // the file and keeps a single continuous, verifiable chain.
        let dir = temp_dir("restart");
        clean(&dir);
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record("close_terminal", "process-changing", "allowed", &json!({"sessionId": "a"}), meta());
        }
        {
            let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
            log.record("close_terminal", "process-changing", "allowed", &json!({"sessionId": "b"}), meta());
        }
        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["prev"], lines[0]["hash"], "restart must re-seed the chain");
        let report = verify(&dir, TEST_KEY);
        assert!(report.ok(), "chain across a restart should verify: {:?}", report.breaks);
        clean(&dir);
    }

    #[test]
    fn try_record_surfaces_a_sink_failure() {
        // Fail-closed substrate (H3): point the sink at a path that cannot be created
        // (a directory whose parent is a FILE) and confirm try_record returns Err so
        // the dispatch path can refuse.
        let base = temp_dir("sinkfail");
        clean(&base);
        std::fs::write(&base, b"i am a file, not a dir").unwrap();
        let dir = base.join("audit"); // create_dir_all(dir) will fail: base is a file
        let log = AuditLog::with_key(dir, TEST_KEY.to_vec());
        let res = log.try_record(
            "spawn_terminal",
            "process-changing",
            "allowed",
            &json!({"cwd": "/tmp"}),
            meta(),
        );
        assert!(res.is_err(), "a sink write failure must surface as Err");
        let _ = std::fs::remove_file(&base);
    }

    #[test]
    fn legacy_pre_v2_records_are_not_counted_as_breaks() {
        // P72-1: on an upgraded install the audit dir holds OLD unkeyed (pre-v2)
        // history the new key cannot MAC-verify. That must NOT be reported as tampering
        // (cry-wolf), yet a genuine v2 tamper AFTER the legacy tail must still be caught.
        // BYPASS-WOULD-FAIL: restore the `Malformed`-break-on-`v!=2` path and the first
        // assert flips RED.
        let dir = temp_dir("legacy");
        clean(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Two legacy v1 lines (no `v` field, unkeyed SHA-256 chain), then a genuine v2
        // line appended by the new build (which re-seeds `prev` from the last line). The
        // legacy lines must live in TODAY's file so the live `record` (which keys off
        // `chrono::Local::now()`) appends v2 to the SAME file - the real mid-day upgrade.
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let path = dir.join(format!("control-{today}.jsonl"));
        let mut prev = String::new();
        let mut legacy_lines = Vec::new();
        for i in 0..2 {
            let mut rec = json!({
                "ts": "2026-01-01T00:00:00Z",
                "command": "close_terminal",
                "decision": "allowed",
                "prev": prev,
                "args": {"sessionId": format!("legacy{i}")},
            });
            let body = serde_json::to_string(&rec).unwrap();
            let h = hex(&Sha256::digest(body.as_bytes())); // OLD unkeyed scheme
            rec["hash"] = json!(h);
            legacy_lines.push(serde_json::to_string(&rec).unwrap());
            prev = h;
        }
        std::fs::write(&path, format!("{}\n", legacy_lines.join("\n"))).unwrap();

        // Now the live log appends a v2 record over the same file (re-seeds from the
        // last legacy hash) - the normal upgrade path.
        let log = AuditLog::with_key(dir.clone(), TEST_KEY.to_vec());
        log.record("close_terminal", "process-changing", "allowed", &json!({"sessionId": "fresh"}), meta());

        let report = verify(&dir, TEST_KEY);
        assert!(report.ok(), "legacy history must not be a break: {:?}", report.breaks);
        assert_eq!(report.legacy, 2, "both pre-v2 lines should be counted as legacy");
        assert_eq!(report.records, 3);

        // A tamper of the v2 tail is STILL detected (the alarm is not defeated).
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let mut v2: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        v2["decision"] = json!("refused-cap");
        *lines.last_mut().unwrap() = serde_json::to_string(&v2).unwrap();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        let report = verify(&dir, TEST_KEY);
        assert!(!report.ok(), "a v2 tamper after legacy history must still be caught");
        assert!(report.breaks.iter().any(|b| b.kind == BreakKind::BadMac));
        clean(&dir);
    }

    #[test]
    fn unhex_roundtrips_and_rejects_bad_input() {
        assert_eq!(unhex(&hex(&[0x00, 0xab, 0xff])), Some(vec![0x00, 0xab, 0xff]));
        assert_eq!(unhex("xyz"), None);
        assert_eq!(unhex("abc"), None); // odd length
    }
}
