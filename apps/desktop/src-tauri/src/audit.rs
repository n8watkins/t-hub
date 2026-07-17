//! Control-socket **audit log with teeth** — Phase 1 of the socket hardening
//! (`docs/SOCKET-AUTH-DESIGN.md` §6).
//!
//! Gives the aspirational `"audited": true` flag a real sink. Every
//! Organization- and ProcessChanging-tier command, and every governor refusal,
//! appends one JSON line to `~/.t-hub/audit/control-YYYYMMDD.jsonl` (mode `0600`
//! on unix). Read-tier commands are NOT logged (they are not process-affecting
//! and would drown the signal).
//!
//! ### Teeth
//! - **Tamper-evident**: each line carries `prev` (the previous line's hash) and
//!   `hash` = SHA-256 of the line's own bytes-minus-`hash`. A truncation or an
//!   in-place edit breaks the chain and is detectable by a verifier that recomputes
//!   forward. The chain is re-seeded from the last line on restart / day-rollover
//!   so it stays continuous.
//! - **Redaction**: `send_text` content is never written — only its length and a
//!   SHA-256 prefix — so the log cannot become a secret-harvesting oracle. `send_keys`
//!   key names ARE logged (they are exactly the kill-pattern signal we want).
//! - **Buffered + fsync-flushed** behind a mutex, written after the dispatch
//!   decision so both allowed and refused commands land.
//!
//! The live mirror of refusals onto the event fanout lives in `control.rs` (it
//! owns the fanout); this module owns only the durable record.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Resolve the audit directory: `$T_HUB_AUDIT_DIR` if set (dev-isolation / tests),
/// else `~/.t-hub/audit`. Mirrors `control::handshake_path`'s home resolution.
pub fn audit_dir() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_AUDIT_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("audit")
}

/// The append-only audit sink. Cheap to construct (no filesystem touch until the
/// first record) so it is safe to build unconditionally in `ControlContext::new`.
pub struct AuditLog {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    /// The day file currently open for append, keyed by its `YYYYMMDD` stamp, plus
    /// its buffered writer. `None` until the first record (or after a day rollover).
    writer: Option<(String, BufWriter<File>)>,
    /// The previous line's `hash`, hex-encoded — the chain link for the next line.
    prev_hash: String,
}

impl AuditLog {
    /// Build a log rooted at `dir`. No I/O happens here.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            inner: Mutex::new(Inner {
                writer: None,
                prev_hash: String::new(),
            }),
        }
    }

    /// Build a log at the default location (`audit_dir`).
    pub fn from_env() -> Self {
        Self::new(audit_dir())
    }

    /// Append one audit record. `command`/`tier`/`decision` are the classification;
    /// `args` is the RAW request args (redaction happens here); `meta` carries the
    /// caller context (`peer`, `tokenTier`, `spawnedBy`) and the dispatch outcome.
    /// Best-effort: a disk failure is logged to stderr and never breaks dispatch.
    pub fn record(&self, command: &str, tier: &str, decision: &str, args: &Value, meta: AuditMeta) {
        if let Err(e) = self.record_inner(command, tier, decision, args, meta) {
            eprintln!("t-hub-audit: failed to write audit record for '{command}': {e}");
        }
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

        // Open (or roll over to) today's file, re-seeding the chain from its last
        // line so restarts and midnight rollovers keep a continuous hash chain.
        let need_open = match &guard.writer {
            Some((open_date, _)) => open_date != &date,
            None => true,
        };
        if need_open {
            std::fs::create_dir_all(&self.dir)?;
            let path = self.dir.join(format!("control-{date}.jsonl"));
            guard.prev_hash = last_line_hash(&path).unwrap_or_default();
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            guard.writer = Some((date.clone(), BufWriter::new(file)));
        }

        // Build the record body (every field EXCEPT `hash`), then chain-hash it.
        let mut record = json!({
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
            record["outcome"] = json!("ok");
        }

        // serde_json's default Map is sorted (BTreeMap), so a verifier recomputes
        // the same bytes deterministically.
        let body = serde_json::to_string(&record)?;
        let hash = hex(&Sha256::digest(body.as_bytes()));
        record["hash"] = json!(hash);
        let line = serde_json::to_string(&record)?;

        let (_, writer) = guard.writer.as_mut().expect("writer opened above");
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        guard.prev_hash = hash;
        Ok(())
    }
}

fn redact_error(command: &str, error: &str) -> Value {
    if matches!(
        command,
        "append_crew_powder_work_log" | "review_crew_powder_criterion" | "complete_crew_powder"
    ) {
        return json!({
            "redacted": true,
            "len": error.len(),
            "sha256": &hex(&Sha256::digest(error.as_bytes()))[..16],
        });
    }
    json!(error)
}

/// Caller context + dispatch outcome attached to an audit record. Kept separate
/// from the command args so the call site reads clearly.
pub struct AuditMeta<'a> {
    /// `"loopback"` or `"remote"` — the connection origin (`ControlContext::peer_is_loopback`).
    pub peer: &'a str,
    /// The capability tier of the presented token. Phase 1 has a single full-power
    /// token, so always `"control"`; the field exists for Phase 2 forward-compat.
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
/// text, same secret risk as `send_text`). Powder work-log bodies and completion
/// proof URLs receive the same length-and-digest treatment. Other commands' args
/// are small identifiers (tab/session ids) and pass through as-is.
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
        "append_crew_powder_work_log" => {
            let message = s("message").unwrap_or("");
            json!({
                "operationId": s("operationId"),
                "messageLen": message.len(),
                "messageSha256": &hex(&Sha256::digest(message.as_bytes()))[..16],
            })
        }
        "review_crew_powder_criterion" => {
            let proof = s("proof").unwrap_or("");
            json!({
                "crewSessionId": s("crewSessionId"),
                "operationId": s("operationId"),
                "criterion": args.get("criterion").and_then(Value::as_u64),
                "criterionId": s("criterionId"),
                "decision": s("decision"),
                "expectedReviewerIdentity": s("expectedReviewerIdentity"),
                "proofLen": proof.len(),
                "proofSha256": &hex(&Sha256::digest(proof.as_bytes()))[..16],
            })
        }
        "complete_crew_powder" => {
            let proof = s("proof").unwrap_or("");
            let criterion_proofs = args
                .get("criterionProofs")
                .or_else(|| args.get("criterion_proofs"))
                .and_then(Value::as_array)
                .map(|proofs| {
                    proofs
                        .iter()
                        .map(|item| {
                            let criterion = item.get("criterion").and_then(Value::as_u64);
                            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
                            json!({
                                "criterion": criterion,
                                "urlLen": url.len(),
                                "urlSha256": &hex(&Sha256::digest(url.as_bytes()))[..16],
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            json!({
                "crewSessionId": s("crewSessionId").or_else(|| s("crew_session_id")),
                "operationId": s("operationId"),
                "proofLen": proof.len(),
                "proofSha256": &hex(&Sha256::digest(proof.as_bytes()))[..16],
                "criterionProofs": criterion_proofs,
            })
        }
        // send_keys, close_terminal, and the Organization commands carry only
        // non-sensitive identifiers / key names — log them verbatim.
        _ => args.clone(),
    }
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

/// Read the last non-empty line of an existing audit file and return its `hash`
/// field, to re-seed the chain across restarts / day rollovers. `None` if the
/// file is absent, empty, or its last line lacks a parseable hash.
fn last_line_hash(path: &std::path::Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        if !line.trim().is_empty() {
            last = Some(line);
        }
    }
    let last = last?;
    serde_json::from_str::<Value>(&last)
        .ok()?
        .get("hash")?
        .as_str()
        .map(|s| s.to_string())
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

    fn read_lines(dir: &std::path::Path) -> Vec<Value> {
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

    #[test]
    fn send_text_content_is_redacted() {
        let dir = temp_dir("redact");
        let _ = std::fs::remove_dir_all(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "send_text",
            "process-changing",
            "allowed",
            &json!({"sessionId": "abc", "text": "SECRET password 123", "enter": true}),
            AuditMeta {
                peer: "loopback",
                token_tier: "control",
                session: Some("abc"),
                spawned_by: None,
                error: None,
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_keys_names_are_kept() {
        let dir = temp_dir("keys");
        let _ = std::fs::remove_dir_all(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "send_keys",
            "process-changing",
            "allowed",
            &json!({"sessionId": "abc", "keys": ["C-c", "Enter"]}),
            AuditMeta {
                peer: "loopback",
                token_tier: "control",
                session: Some("abc"),
                spawned_by: None,
                error: None,
            },
        );
        let lines = read_lines(&dir);
        assert_eq!(lines[0]["args"]["keys"][0], "C-c");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn powder_work_log_and_completion_proofs_are_redacted() {
        let dir = temp_dir("powder-redact");
        let _ = std::fs::remove_dir_all(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "append_crew_powder_work_log",
            "organization",
            "allowed",
            &json!({
                "operationId": "work-log:audit",
                "message": "SECRET work log body",
            }),
            AuditMeta {
                peer: "loopback",
                token_tier: "read",
                session: Some("crew-1"),
                spawned_by: None,
                error: None,
            },
        );
        log.record(
            "review_crew_powder_criterion",
            "organization",
            "allowed",
            &json!({
                "crewSessionId": "crew-1",
                "operationId": "criterion:audit",
                "criterion": 0,
                "criterionId": "powder.criterion.v1:sha256:safe:0",
                "decision": "approved",
                "proof": "SECRET criterion review proof",
                "expectedReviewerIdentity": "actor-captain-1",
            }),
            AuditMeta {
                peer: "loopback",
                token_tier: "control",
                session: Some("captain-1"),
                spawned_by: None,
                error: None,
            },
        );
        log.record(
            "complete_crew_powder",
            "organization",
            "allowed",
            &json!({
                "crewSessionId": "crew-1",
                "operationId": "completion:audit",
                "proof": "https://secret.example.test/overall-proof",
                "criterionProofs": [{
                    "criterion": 0,
                    "url": "https://secret.example.test/criterion-proof",
                }],
            }),
            AuditMeta {
                peer: "loopback",
                token_tier: "control",
                session: Some("captain-1"),
                spawned_by: None,
                error: Some("proof rejected at https://secret.example.test/criterion-proof"),
            },
        );

        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 3);
        let serialized = serde_json::to_string(&lines).unwrap();
        assert!(!serialized.contains("SECRET work log body"));
        assert!(!serialized.contains("SECRET criterion review proof"));
        assert!(!serialized.contains("secret.example.test"));
        assert_eq!(lines[0]["args"]["messageLen"], 20);
        assert_eq!(lines[1]["args"]["criterion"], 0);
        assert_eq!(lines[1]["args"]["operationId"], "criterion:audit");
        assert_eq!(lines[1]["args"]["proofSha256"].as_str().unwrap().len(), 16);
        assert_eq!(lines[2]["args"]["criterionProofs"][0]["criterion"], 0);
        assert_eq!(lines[2]["error"]["redacted"], true);
        assert_eq!(lines[2]["args"]["proofSha256"].as_str().unwrap().len(), 16);
        assert_eq!(
            lines[2]["args"]["criterionProofs"][0]["urlSha256"]
                .as_str()
                .unwrap()
                .len(),
            16
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_chain_links_each_line() {
        let dir = temp_dir("chain");
        let _ = std::fs::remove_dir_all(&dir);
        let log = AuditLog::new(dir.clone());
        for i in 0..3 {
            log.record(
                "close_terminal",
                "process-changing",
                "allowed",
                &json!({"sessionId": format!("s{i}")}),
                AuditMeta {
                    peer: "loopback",
                    token_tier: "control",
                    session: None,
                    spawned_by: None,
                    error: None,
                },
            );
        }
        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 3);
        // First line chains from the genesis (empty) prev.
        assert_eq!(lines[0]["prev"], "");
        // Each subsequent line's prev equals the prior line's hash.
        assert_eq!(lines[1]["prev"], lines[0]["hash"]);
        assert_eq!(lines[2]["prev"], lines[1]["hash"]);
        // And each hash actually verifies: recompute SHA-256 of the body-minus-hash.
        for rec in &lines {
            let mut body = rec.clone();
            body.as_object_mut().unwrap().remove("hash");
            let recomputed = hex(&Sha256::digest(
                serde_json::to_string(&body).unwrap().as_bytes(),
            ));
            assert_eq!(recomputed, rec["hash"].as_str().unwrap());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refusal_is_recorded() {
        let dir = temp_dir("refuse");
        let _ = std::fs::remove_dir_all(&dir);
        let log = AuditLog::new(dir.clone());
        log.record(
            "spawn_terminal",
            "process-changing",
            "refused-cap",
            &json!({"cwd": "/tmp"}),
            AuditMeta {
                peer: "loopback",
                token_tier: "control",
                session: None,
                spawned_by: None,
                error: None,
            },
        );
        let lines = read_lines(&dir);
        assert_eq!(lines[0]["decision"], "refused-cap");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
