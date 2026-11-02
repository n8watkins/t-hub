//! Comms plane - PHASE 2: the durable inbox (transport + seq + at-least-once + the
//! receipt state machine). This is the store the ratified design's §2.2/§2.4 call
//! for; Phase 1 (`plane.rs`) deferred exactly this.
//!
//! HONEST SCOPE - what Phase 2 is and is NOT (ratified design §3.2 Phase 2):
//!
//! - It IS durable: records persist on enqueue BEFORE the sender's ACK, survive an
//!   app restart (queues rebuild from the on-disk segments), and survive a crash
//!   mid-drain via at-least-once-write with APP-SIDE dedup (M2 - a `delivered`
//!   record is never re-written; effectively-once does not depend on the recipient
//!   agent cooperating).
//! - It carries a per-recipient monotonic `seq`, per-recipient FIFO within a
//!   priority class, and EMERGENCY ordered ahead of STATUS/DECISION but still seq'd.
//! - It implements the receipt machine: optional transaction-local `held`, then
//!   `enqueued -> delivered -> processed`. Only `processed` (a cooperative ack on
//!   DRAIN, never on enqueue) retires a record.
//!
//! - It does NOT enforce ACLs. Who may enqueue to whom is Phase 3 - this module
//!   stamps the `sender` it is handed and never authorizes it.
//! - It does NOT gate on a human typing. The `NOT human_busy` predicate is Phase 4.
//!   Phase 2 drains on the EXISTING `Completed` turn-boundary predicate ONLY, which
//!   the CALLER supplies (the fleet notifier for a wake); this module only writes
//!   `at_boundary` records the caller vouches are drainable.
//! - It is NOT the voice/visual decision surface and adds no marked lane - Phase 4.
//!
//! Effectively-once, precisely (M2): a record is written to a PTY AT-MOST-ONCE
//! relative to its own durable `Delivered` state - once `state == Delivered` it is
//! never re-picked (the re-pick guard is the per-record state itself, checked in
//! `head_enqueued`; `last_drained_seq` is observability, not a correctness guard).
//! The only re-write is a record still `Enqueued` because the write FAILED or the app
//! crashed after the write but before the `Delivered` state persisted (the narrow
//! at-least-once seam the design chooses over silent loss: §2.2 "At-least-once-write
//! + at-most-once-delivered is chosen over at-most-once because silent loss is the
//! cardinal sin"). Dedup lives in this trusted app, not on the untrusted recipient.
//!
//! DURABILITY IS PROCESS-CRASH-DURABLE, not power-loss-durable (review L1). Segments
//! are published with temp-write + atomic `rename` but WITHOUT an `fsync` of the file
//! or its directory - matching the established crate discipline (`write_handshake`,
//! the captains registry persist, `voice.rs`). For the dominant crash model (app
//! restart / process death) the page cache survives and an ACK'd message is never
//! lost. A power-loss / kernel-panic in the write window is an undisclosed-elsewhere
//! gap that closing would need fsync (a crate-wide change, deliberately not made here).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Epoch-ms, matching the rest of the crate's timestamp convention (`model.rs`
/// records, `control.rs` snapshots). Monotonicity is not required - these are audit
/// timestamps, never ordering keys (the per-recipient `seq` is the ordering key).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Priority class. Phase 2 carries the field and orders EMERGENCY ahead of STANDARD
/// in the drain pick, but does NOT build the fast-lane predicate relaxation
/// (drain-on-arrival) or the WHO-may-flag authority - those are §2.7 / Phase 3. Here
/// it is purely a store ordering property: "EMERGENCY orders ahead of STATUS/DECISION
/// but is still seq'd" (§2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    /// Ordinary STATUS / DECISION traffic. Lower than `Emergency` so it drains after.
    Standard,
    /// EMERGENCY / fast-lane. Drains ahead of `Standard`, but still FIFO-by-seq
    /// within its own class. (Enum order matters: `Emergency` must sort greater.)
    Emergency,
}

/// The receipt states (§2.4). Normal delivery transitions are strictly forward:
/// `Held -> Enqueued -> Delivered -> Processed`. A transient in-flight marker lives
/// on the queue index, NOT here, so this enum stays a clean persisted lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReceiptState {
    /// Persisted as part of a multi-store operation but not yet eligible for
    /// delivery. Activation moves it to `Enqueued` only after every required
    /// durable mutation has committed.
    Held,
    /// Durable; the sender ACK means "persisted", NOT "received" (§2.4).
    Enqueued,
    /// Bytes written to the PTY at-most-once; the durable `delivered` marker guards
    /// against re-write (M2). Redelivery-eligible ONLY on a failed write.
    Delivered,
    /// The recipient confirmed intake at its turn boundary (`inbox_ack`, §2.4). Means
    /// "the agent confirmed intake", NOT "the agent acted on it". Only `processed`
    /// retires a record.
    Processed,
}

/// One durable message in a recipient's queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxRecord {
    /// Per-recipient monotonic sequence (the FIFO ordering key).
    pub seq: u64,
    /// Stable producer request identity. Present for typed control-plane sends so
    /// retries after an app restart replay the original enqueue instead of
    /// creating a second instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// App-stamped provenance. Phase 2: the coarse subsystem label for app-originated
    /// messages (e.g. the fleet wake), OR a per-session identity id for a
    /// session-originated message. This module NEVER authorizes it (that is Phase 3);
    /// it records exactly what the caller stamped.
    pub sender: String,
    /// Priority class (drain-ordering only in Phase 2).
    pub priority: Priority,
    /// The payload written to the recipient's PTY on drain. Content is opaque here.
    pub body: String,
    /// Whether the drain submits the payload with an Enter (matches the pre-plane
    /// wake behavior, which always submits).
    pub enter: bool,
    /// The receipt lifecycle state.
    pub state: ReceiptState,
    /// Epoch-ms the record was persisted (enqueue).
    pub enqueued_at: u64,
    /// Epoch-ms of the at-most-once write, once delivered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<u64>,
    /// Epoch-ms the recipient acked intake, once processed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed_at: Option<u64>,
    /// How many times a write was ATTEMPTED and FAILED before delivery (observability;
    /// a successful write does not increment it). A crash-recovered in-flight record
    /// also counts as a re-attempt on its next drain.
    #[serde(default)]
    pub write_attempts: u32,
}

impl InboxRecord {
    fn is_open(&self) -> bool {
        !matches!(self.state, ReceiptState::Processed)
    }
}

/// The on-disk content of one recipient's segment file - the records PLUS the tiny
/// authoritative index the design's §2.2 describes (next/last-drained seq + the
/// transient in-flight marker). Folded into one file so every mutation is a single
/// atomic rename (crash-consistent), reusing the registry's write discipline.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecipientQueue {
    /// The recipient key (a tile id in Phase 2 - see the id-namespace note in the
    /// design §2.1 L1; item 2 re-keys this to a durable ship/role slug).
    recipient: String,
    /// Next seq to assign (head/tail derivable from the records; this is the mint).
    next_seq: u64,
    /// Highest seq that has reached `Delivered` (the resume cursor after a restart).
    last_drained_seq: u64,
    /// The seq currently being written (bytes handed to the PTY, awaiting the
    /// commit of its `Delivered` marker). Persisted so a crash mid-write is
    /// detectable: on load it is CLEARED and its still-`Enqueued` record becomes
    /// redelivery-eligible (the at-least-once seam). At most one at a time.
    #[serde(skip_serializing_if = "Option::is_none")]
    inflight: Option<u64>,
    /// The messages, in enqueue order.
    records: Vec<InboxRecord>,
    /// Permanent, body-free idempotency receipts. Processed message bodies may be
    /// compacted, but a producer request id must never become reusable.
    #[serde(default)]
    idempotency: HashMap<String, IdempotencyReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdempotencyReceipt {
    seq: u64,
    signature: String,
}

impl RecipientQueue {
    fn open_records(&self) -> usize {
        self.records.iter().filter(|r| r.is_open()).count()
    }

    /// Index of the next record to drain: the highest-priority, lowest-seq record
    /// still `Enqueued`. Returns `None` when nothing is drainable.
    fn head_enqueued(&self) -> Option<usize> {
        self.records
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.state, ReceiptState::Enqueued))
            // Higher priority first (Emergency > Standard), then lower seq (FIFO).
            .min_by(|(_, a), (_, b)| b.priority.cmp(&a.priority).then(a.seq.cmp(&b.seq)))
            .map(|(i, _)| i)
    }

    fn find_mut(&mut self, seq: u64) -> Option<&mut InboxRecord> {
        self.records.iter_mut().find(|r| r.seq == seq)
    }
}

/// Outcome of an enqueue (the honest ACK, design D6: keep `accepted`, add a distinct
/// `enqueued`/seq - persistence, NOT receipt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueOutcome {
    pub seq: u64,
    pub duplicate: bool,
}

/// Why an enqueue or activation was refused. Authorization remains outside this
/// transport store; it reports capacity, request conflicts, and persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueError {
    /// The recipient's queue is at its bounded depth. Surfaced as an attributed
    /// backpressure error, never a silent drop (D5 option a). Emergency traffic is
    /// exempt from the bound so a decision is never lost to backpressure.
    Overflow {
        recipient: String,
        depth: usize,
        max: usize,
    },
    IdempotencyConflict {
        request_id: String,
    },
    Persistence {
        recipient: String,
        message: String,
    },
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnqueueError::Overflow {
                recipient,
                depth,
                max,
            } => write!(
                f,
                "inbox overflow for recipient '{recipient}': {depth} open messages at bound {max}"
            ),
            EnqueueError::IdempotencyConflict { request_id } => write!(
                f,
                "inbox requestId '{request_id}' was already used with a different message"
            ),
            EnqueueError::Persistence { recipient, message } => write!(
                f,
                "inbox persistence failed for recipient '{recipient}': {message}"
            ),
        }
    }
}

/// Outcome of a single drain attempt at a turn boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    /// A record was written at-most-once and marked `Delivered`.
    Delivered { seq: u64 },
    /// Nothing was `Enqueued` for this recipient (queue empty or all delivered).
    Empty,
    /// A record is already in flight (its write has not committed); the caller must
    /// not start a second concurrent write. FIFO/at-most-once are preserved.
    Busy,
    /// The write itself failed; the record stays `Enqueued` and is redelivery-eligible
    /// on the next boundary (the only redelivery trigger, M2).
    WriteFailed { seq: u64, error: String },
}

/// Outcome of an ack (the `delivered -> processed` intake confirmation, M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckOutcome {
    /// The record moved `Delivered -> Processed` and is now retired.
    Processed { seq: u64 },
    /// The record was already `Processed` (a duplicate/lost-then-retried ack; safe -
    /// a lost ack never triggers a re-write, M2).
    AlreadyProcessed { seq: u64 },
    /// The seq is not known for this recipient (never enqueued, or already compacted).
    Unknown { seq: u64 },
    /// The record exists but has not been delivered yet - an ack cannot precede
    /// delivery. Rejected rather than silently advancing state.
    NotDelivered { seq: u64 },
}

/// Per-recipient observability snapshot (§2.8: queue depth + oldest-un-drained age +
/// the cursors). Content is never included - counts and ages only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueDepth {
    pub recipient: String,
    pub enqueued: usize,
    pub delivered: usize,
    pub processed: usize,
    pub next_seq: u64,
    pub last_drained_seq: u64,
    pub inflight: Option<u64>,
    /// Age in ms of the oldest still-`Enqueued` record (the drain-starvation signal,
    /// §2.1 M6 - the max-hold deadline surfacing consumes this in a later phase).
    pub oldest_enqueued_age_ms: Option<u64>,
}

/// Default per-recipient depth bound (D5). Generous - the fleet wake coalesces, so a
/// healthy recipient sits near zero; the bound only guards a wedged never-draining
/// recipient from unbounded growth. Overridable via `T_HUB_INBOX_MAX_DEPTH`.
const DEFAULT_MAX_DEPTH: usize = 256;

/// How many `Processed` records to retain per recipient as a rolling audit tail
/// before compaction drops them (§2.2 retention). Overridable via
/// `T_HUB_INBOX_AUDIT_TAIL`.
const DEFAULT_AUDIT_TAIL: usize = 64;

/// The durable inbox: one segment file per recipient under `dir`, an in-memory cache
/// of the parsed queues behind a single mutex. Low-volume (fleet wakes coalesce), so
/// a coarse lock is simplest-correct; the only work done OUTSIDE the lock is the PTY
/// write in `drain_one`, guarded by the persisted in-flight marker.
/// A per-message lifecycle telemetry event (§2.8 delivery telemetry). Carries the
/// routing metadata + state transition, NEVER the body (content can carry secrets,
/// mirroring the audit log's redaction). `lib.rs` fans these out on `control://inbox`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxEvent {
    pub recipient: String,
    pub seq: u64,
    pub sender: String,
    pub priority: Priority,
    /// The lifecycle transition this event marks: `enqueued` | `delivered` |
    /// `processed` | `writeFailed`. Distinct from the enqueue ACK's `accepted`
    /// (which means "persisted") - D6's "add a distinct processed receipt event".
    pub event: &'static str,
    /// Payload length only (never the content).
    pub bytes: usize,
    pub at_ms: u64,
}

/// A telemetry sink the inbox calls on each lifecycle transition. Wired to the
/// control event fanout in production; a recording closure in tests.
pub type TelemetrySink = std::sync::Arc<dyn Fn(&InboxEvent) + Send + Sync>;

pub struct Inbox {
    /// The segment directory, or `None` for an ephemeral (in-memory-only) inbox - the
    /// headless-test / no-addr default, mirroring `IdentityStore::ephemeral`. A `None`
    /// inbox never touches disk (so unrelated tests do not write to `~/.t-hub/inbox`).
    dir: Option<PathBuf>,
    queues: Mutex<HashMap<String, RecipientQueue>>,
    max_depth: usize,
    audit_tail: usize,
    /// Optional per-message lifecycle telemetry sink (§2.8). Emitted OUTSIDE the queue
    /// lock so the sink can never deadlock the inbox.
    telemetry: Option<TelemetrySink>,
}

impl Inbox {
    /// Open (or create) the inbox rooted at `dir`, loading every existing segment.
    /// Any in-flight marker found on disk is a crash mid-write: it is CLEARED so its
    /// still-`Enqueued` record redelivers on the next boundary (at-least-once).
    pub fn open(dir: PathBuf) -> Self {
        let queues = load_segments(&dir);
        Inbox {
            dir: Some(dir),
            queues: Mutex::new(queues),
            max_depth: env_max_depth(),
            audit_tail: env_audit_tail(),
            telemetry: None,
        }
    }

    /// Open at the default `~/.t-hub/inbox/` (override `T_HUB_INBOX_DIR`), mirroring
    /// `captains_path` / `handshake_path`'s resolution.
    pub fn open_default() -> Self {
        Self::open(default_inbox_dir())
    }

    /// An in-memory-only inbox that never persists (headless / no-addr default).
    pub fn ephemeral() -> Self {
        Inbox {
            dir: None,
            queues: Mutex::new(HashMap::new()),
            max_depth: env_max_depth(),
            audit_tail: env_audit_tail(),
            telemetry: None,
        }
    }

    /// Attach a per-message lifecycle telemetry sink (§2.8). `lib.rs` wires it to the
    /// `control://inbox` fanout; tests pass a recording closure.
    pub fn with_telemetry(mut self, sink: TelemetrySink) -> Self {
        self.telemetry = Some(sink);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, RecipientQueue>> {
        self.queues.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Emit a lifecycle telemetry event, if a sink is attached. Called only OUTSIDE
    /// the queue lock.
    fn emit(&self, recipient: &str, rec: &InboxRecord, event: &'static str) {
        if let Some(sink) = &self.telemetry {
            sink(&InboxEvent {
                recipient: recipient.to_string(),
                seq: rec.seq,
                sender: rec.sender.clone(),
                priority: rec.priority,
                event,
                bytes: rec.body.len(),
                at_ms: now_ms(),
            });
        }
    }

    /// Persist one recipient's segment atomically (temp + rename + 0600), the exact
    /// discipline the captains registry / control.json use. The in-memory cache is
    /// the source of truth; a failed write logs and leaves the cache ahead of disk
    /// (the next successful mutation re-persists the whole segment). A no-op for an
    /// ephemeral inbox.
    fn persist(&self, q: &RecipientQueue) {
        let Some(dir) = &self.dir else { return };
        if let Err(e) = write_segment(dir, q) {
            eprintln!(
                "t-hub-inbox: segment persist for '{}' failed: {e}",
                q.recipient
            );
        }
    }

    /// Enqueue a durable message. Persists BEFORE returning (so the ACK genuinely
    /// means "persisted"). Assigns the next per-recipient seq. Refuses only on
    /// overflow (D5); Emergency is exempt from the bound so a decision is never lost.
    pub fn enqueue(
        &self,
        recipient: &str,
        sender: &str,
        priority: Priority,
        body: &str,
        enter: bool,
    ) -> Result<EnqueueOutcome, EnqueueError> {
        self.enqueue_once(
            recipient,
            sender,
            priority,
            body,
            enter,
            None,
            None,
            ReceiptState::Enqueued,
        )
    }

    /// Enqueue exactly once for a stable producer request id. The idempotency
    /// receipt is stored in the same atomic segment as the message, so replay
    /// remains safe across process restart and after body compaction.
    pub fn enqueue_idempotent(
        &self,
        recipient: &str,
        sender: &str,
        priority: Priority,
        body: &str,
        enter: bool,
        request_id: &str,
    ) -> Result<EnqueueOutcome, EnqueueError> {
        self.enqueue_once(
            recipient,
            sender,
            priority,
            body,
            enter,
            Some(request_id),
            None,
            ReceiptState::Enqueued,
        )
    }

    /// Persist an idempotent message in a non-deliverable state. The semantic
    /// signature is supplied by the typed operation and must cover every field
    /// whose meaning is immutable for the request id.
    pub fn prepare_idempotent(
        &self,
        recipient: &str,
        sender: &str,
        priority: Priority,
        body: &str,
        enter: bool,
        request_id: &str,
        semantic_signature: &str,
    ) -> Result<EnqueueOutcome, EnqueueError> {
        self.enqueue_once(
            recipient,
            sender,
            priority,
            body,
            enter,
            Some(request_id),
            Some(semantic_signature),
            ReceiptState::Held,
        )
    }

    fn enqueue_once(
        &self,
        recipient: &str,
        sender: &str,
        priority: Priority,
        body: &str,
        enter: bool,
        request_id: Option<&str>,
        semantic_signature: Option<&str>,
        initial_state: ReceiptState,
    ) -> Result<EnqueueOutcome, EnqueueError> {
        let mut queues = self.lock();
        let signature = request_id.map(|_| {
            if let Some(signature) = semantic_signature {
                return signature.to_string();
            }
            let value = serde_json::json!({
                "sender": sender,
                "priority": priority,
                "body": body,
                "enter": enter,
            });
            format!("{:x}", Sha256::digest(value.to_string()))
        });
        if let (Some(request_id), Some(signature)) = (request_id, signature.as_deref()) {
            if let Some((existing_recipient, receipt)) = queues.iter().find_map(|(key, queue)| {
                queue
                    .idempotency
                    .get(request_id)
                    .map(|receipt| (key, receipt))
            }) {
                return if existing_recipient == recipient && receipt.signature == signature {
                    Ok(EnqueueOutcome {
                        seq: receipt.seq,
                        duplicate: true,
                    })
                } else {
                    Err(EnqueueError::IdempotencyConflict {
                        request_id: request_id.to_string(),
                    })
                };
            }
        }
        let q = queues
            .entry(recipient.to_string())
            .or_insert_with(|| RecipientQueue {
                recipient: recipient.to_string(),
                ..Default::default()
            });
        if priority != Priority::Emergency && q.open_records() >= self.max_depth {
            return Err(EnqueueError::Overflow {
                recipient: recipient.to_string(),
                depth: q.open_records(),
                max: self.max_depth,
            });
        }
        let seq = q.next_seq;
        q.next_seq += 1;
        let record = InboxRecord {
            seq,
            request_id: request_id.map(str::to_string),
            sender: sender.to_string(),
            priority,
            body: body.to_string(),
            enter,
            state: initial_state,
            enqueued_at: now_ms(),
            delivered_at: None,
            processed_at: None,
            write_attempts: 0,
        };
        q.records.push(record.clone());
        if let (Some(request_id), Some(signature)) = (request_id, signature) {
            q.idempotency.insert(
                request_id.to_string(),
                IdempotencyReceipt { seq, signature },
            );
        }
        if let Some(dir) = &self.dir {
            if let Err(error) = write_segment(dir, q) {
                q.records.pop();
                q.next_seq = seq;
                if let Some(request_id) = request_id {
                    q.idempotency.remove(request_id);
                }
                return Err(EnqueueError::Persistence {
                    recipient: recipient.to_string(),
                    message: error.to_string(),
                });
            }
        }
        drop(queues);
        if initial_state == ReceiptState::Enqueued {
            self.emit(recipient, &record, "enqueued");
        }
        Ok(EnqueueOutcome {
            seq,
            duplicate: false,
        })
    }

    /// Make a prepared message deliverable after the enclosing durable operation
    /// has committed. A persistence failure restores `Held` in memory and on the
    /// next retry the same request can safely attempt activation again.
    pub fn activate_prepared(
        &self,
        recipient: &str,
        request_id: &str,
    ) -> Result<EnqueueOutcome, EnqueueError> {
        let mut queues = self.lock();
        let q = queues
            .get_mut(recipient)
            .ok_or_else(|| EnqueueError::IdempotencyConflict {
                request_id: request_id.to_string(),
            })?;
        let receipt = q.idempotency.get(request_id).cloned().ok_or_else(|| {
            EnqueueError::IdempotencyConflict {
                request_id: request_id.to_string(),
            }
        })?;
        let record = q
            .find_mut(receipt.seq)
            .ok_or_else(|| EnqueueError::IdempotencyConflict {
                request_id: request_id.to_string(),
            })?;
        if record.state != ReceiptState::Held {
            return Ok(EnqueueOutcome {
                seq: receipt.seq,
                duplicate: true,
            });
        }
        record.state = ReceiptState::Enqueued;
        let activated = record.clone();
        if let Some(dir) = &self.dir {
            if let Err(error) = write_segment(dir, q) {
                if let Some(record) = q.find_mut(receipt.seq) {
                    record.state = ReceiptState::Held;
                }
                return Err(EnqueueError::Persistence {
                    recipient: recipient.to_string(),
                    message: error.to_string(),
                });
            }
        }
        drop(queues);
        self.emit(recipient, &activated, "enqueued");
        Ok(EnqueueOutcome {
            seq: receipt.seq,
            duplicate: false,
        })
    }

    /// Drain AT MOST ONE record for `recipient` at a turn boundary the CALLER has
    /// established (Phase 2 drains on the existing `Completed` predicate only; this
    /// module does not evaluate it). One record per boundary preserves the
    /// "inject-N cannot overtake inject-(N-1)'s turn" serialization (§2.1).
    ///
    /// At-most-once (M2): the head record is marked in-flight and its marker persisted
    /// UNDER the lock; the `writer` runs OUTSIDE the lock (a PTY write may block);
    /// then the result is committed under the lock. A `Delivered` record is never
    /// re-picked. A crash between the write and the commit leaves the record
    /// `Enqueued` (in-flight cleared on the next `open`), so it redelivers - the only
    /// redelivery trigger.
    pub fn drain_one<F>(&self, recipient: &str, writer: F) -> DrainOutcome
    where
        F: FnOnce(&InboxRecord) -> Result<(), String>,
    {
        // Phase 1: pick the head + claim in-flight, persist the marker, release.
        let record = {
            let mut queues = self.lock();
            let Some(q) = queues.get_mut(recipient) else {
                return DrainOutcome::Empty;
            };
            if q.inflight.is_some() {
                return DrainOutcome::Busy;
            }
            let Some(idx) = q.head_enqueued() else {
                return DrainOutcome::Empty;
            };
            let seq = q.records[idx].seq;
            q.inflight = Some(seq);
            self.persist(q);
            q.records[idx].clone()
        };

        // Phase 2: the actual PTY write, no lock held.
        let result = writer(&record);

        // Phase 3: commit the outcome.
        let mut queues = self.lock();
        let Some(q) = queues.get_mut(recipient) else {
            // Recipient vanished mid-write (should not happen for a live tile); nothing
            // to commit against.
            return DrainOutcome::Empty;
        };
        q.inflight = None;
        match result {
            Ok(()) => {
                if let Some(rec) = q.find_mut(record.seq) {
                    rec.state = ReceiptState::Delivered;
                    rec.delivered_at = Some(now_ms());
                }
                if record.seq >= q.last_drained_seq {
                    q.last_drained_seq = record.seq;
                }
                self.persist(q);
                drop(queues);
                self.emit(recipient, &record, "delivered");
                DrainOutcome::Delivered { seq: record.seq }
            }
            Err(error) => {
                if let Some(rec) = q.find_mut(record.seq) {
                    rec.write_attempts = rec.write_attempts.saturating_add(1);
                }
                self.persist(q);
                drop(queues);
                self.emit(recipient, &record, "writeFailed");
                DrainOutcome::WriteFailed {
                    seq: record.seq,
                    error,
                }
            }
        }
    }

    /// Confirm intake of a delivered record (`delivered -> processed`, the `inbox_ack`
    /// channel, M2). A lost ack is safe: it never triggers a re-write. Acking a record
    /// that is already processed is a benign duplicate; acking before delivery, or an
    /// unknown seq, is rejected rather than silently advancing state. Retires + compacts.
    pub fn ack(&self, recipient: &str, seq: u64) -> AckOutcome {
        let mut queues = self.lock();
        let Some(q) = queues.get_mut(recipient) else {
            return AckOutcome::Unknown { seq };
        };
        let outcome = match q.find_mut(seq).map(|r| r.state) {
            None => AckOutcome::Unknown { seq },
            Some(ReceiptState::Processed) => AckOutcome::AlreadyProcessed { seq },
            Some(ReceiptState::Held | ReceiptState::Enqueued) => AckOutcome::NotDelivered { seq },
            Some(ReceiptState::Delivered) => {
                if let Some(rec) = q.find_mut(seq) {
                    rec.state = ReceiptState::Processed;
                    rec.processed_at = Some(now_ms());
                }
                AckOutcome::Processed { seq }
            }
        };
        let mut processed_record = None;
        if matches!(outcome, AckOutcome::Processed { .. }) {
            processed_record = q.find_mut(seq).map(|r| r.clone());
            compact(q, self.audit_tail);
            self.persist(q);
        }
        drop(queues);
        if let Some(rec) = &processed_record {
            self.emit(recipient, rec, "processed");
        }
        outcome
    }

    /// Per-recipient observability snapshot (counts + oldest-un-drained age + cursors).
    pub fn depth(&self, recipient: &str) -> QueueDepth {
        let queues = self.lock();
        let Some(q) = queues.get(recipient) else {
            return QueueDepth {
                recipient: recipient.to_string(),
                ..Default::default()
            };
        };
        depth_of(q)
    }

    /// Comms-plane Phase 3 (operate-fleet-infra, §2.7 R-L2): PURGE a recipient's queue -
    /// drop ALL its records (a wedged-queue reset / drain-flush admin op). Returns the
    /// number of records removed. This is a DESTRUCTIVE administrative operation (durable
    /// messages are lost), which is why `control.rs` gates it to the apex fleet-infra
    /// owner via `acl::can_operate_fleet_infra`. A no-op for an unknown recipient.
    pub fn purge_recipient(&self, recipient: &str) -> usize {
        let mut queues = self.lock();
        let Some(q) = queues.get_mut(recipient) else {
            return 0;
        };
        let removed = q.records.len();
        q.records.clear();
        q.inflight = None;
        self.persist(q);
        removed
    }

    /// Observability across all recipients (§2.8 health panel feed).
    pub fn depth_all(&self) -> Vec<QueueDepth> {
        let queues = self.lock();
        let mut out: Vec<QueueDepth> = queues.values().map(depth_of).collect();
        out.sort_by(|a, b| a.recipient.cmp(&b.recipient));
        out
    }
}

fn depth_of(q: &RecipientQueue) -> QueueDepth {
    let now = now_ms();
    let mut enqueued = 0;
    let mut delivered = 0;
    let mut processed = 0;
    let mut oldest_enqueued: Option<u64> = None;
    for r in &q.records {
        match r.state {
            ReceiptState::Held | ReceiptState::Enqueued => {
                enqueued += 1;
                oldest_enqueued = Some(match oldest_enqueued {
                    Some(t) => t.min(r.enqueued_at),
                    None => r.enqueued_at,
                });
            }
            ReceiptState::Delivered => delivered += 1,
            ReceiptState::Processed => processed += 1,
        }
    }
    QueueDepth {
        recipient: q.recipient.clone(),
        enqueued,
        delivered,
        processed,
        next_seq: q.next_seq,
        last_drained_seq: q.last_drained_seq,
        inflight: q.inflight,
        oldest_enqueued_age_ms: oldest_enqueued.map(|t| now.saturating_sub(t)),
    }
}

/// Drop `Processed` records beyond a rolling audit tail (§2.2 retention). Never
/// touches `Enqueued`/`Delivered` records. Keeps the newest `audit_tail` processed
/// (by seq) for observability.
fn compact(q: &mut RecipientQueue, audit_tail: usize) {
    // Defense in depth vs review L4: even if a caller hands `audit_tail == 0`
    // (env is already clamped in `env_audit_tail`), never index out of bounds -
    // treat 0 as "retain 1" so the cutoff arithmetic below is always valid.
    let audit_tail = audit_tail.max(1);
    let processed: Vec<u64> = q
        .records
        .iter()
        .filter(|r| matches!(r.state, ReceiptState::Processed))
        .map(|r| r.seq)
        .collect();
    if processed.len() <= audit_tail {
        return;
    }
    // Keep the newest `audit_tail` by seq; drop the rest.
    let mut keep = processed;
    keep.sort_unstable();
    let cutoff = keep[keep.len() - audit_tail];
    q.records
        .retain(|r| !matches!(r.state, ReceiptState::Processed) || r.seq >= cutoff);
}

// ---- on-disk segment I/O -------------------------------------------------------

fn env_max_depth() -> usize {
    std::env::var("T_HUB_INBOX_MAX_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_DEPTH)
}

fn env_audit_tail() -> usize {
    // Clamp to >= 1 (review L4): a `0` would make `compact()` index `keep[len]` out of
    // bounds and panic under the queue lock. There must always be room for at least
    // one retained processed record.
    std::env::var("T_HUB_INBOX_AUDIT_TAIL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_AUDIT_TAIL)
        .max(1)
}

/// Default inbox directory, mirroring `captains_path`'s HOME resolution. Override
/// with `T_HUB_INBOX_DIR` (tests point this at a temp dir).
fn default_inbox_dir() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_INBOX_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("inbox")
}

/// Map a recipient key to a safe segment filename. Tile ids are short and
/// filesystem-safe, but a key could in principle contain a separator; anything
/// outside `[A-Za-z0-9_-]` is hex-escaped so the key can never traverse the dir.
fn segment_name(recipient: &str) -> String {
    let mut out = String::with_capacity(recipient.len() + 5);
    for b in recipient.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out.push_str(".json");
    out
}

fn segment_path(dir: &Path, recipient: &str) -> PathBuf {
    dir.join(segment_name(recipient))
}

/// Load every segment file in `dir` into an in-memory map, clearing any in-flight
/// marker (a crash mid-write; the record stays `Enqueued` and redelivers).
fn load_segments(dir: &Path) -> HashMap<String, RecipientQueue> {
    let mut map = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return map; // dir absent => empty inbox; created lazily on first persist.
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<RecipientQueue>(&body) {
            Ok(mut q) => {
                // A persisted in-flight marker means the app died mid-write; clear it
                // so the still-`Enqueued` record is redelivery-eligible.
                q.inflight = None;
                map.insert(q.recipient.clone(), q);
            }
            Err(e) => {
                eprintln!(
                    "t-hub-inbox: skipping unreadable segment {}: {e}",
                    path.display()
                );
            }
        }
    }
    map
}

/// Atomic segment write: temp sibling + set 0600 + rename over the target, cleaning
/// up the temp on failure. Identical discipline to `write_handshake` / the captains
/// `persist`, so a reader never sees a half-written segment.
fn write_segment(dir: &Path, q: &RecipientQueue) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = segment_path(dir, &q.recipient);
    let body = serde_json::to_vec_pretty(q)?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh inbox in a unique temp dir, isolated per test (no env globals) - the
    /// crate's `temp_db()` pattern.
    fn temp_inbox() -> (Inbox, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "t-hub-inbox-test-{}-{}",
            std::process::id(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Inbox::open(dir.clone()), dir)
    }

    /// A writer that always succeeds and records what it was asked to write.
    fn ok_writer(
        sink: &std::sync::Mutex<Vec<(u64, String, bool)>>,
    ) -> impl Fn(&InboxRecord) -> Result<(), String> + '_ {
        move |rec: &InboxRecord| {
            sink.lock()
                .unwrap()
                .push((rec.seq, rec.body.clone(), rec.enter));
            Ok(())
        }
    }

    #[test]
    fn enqueue_assigns_monotonic_per_recipient_seq_and_persists() {
        let (inbox, _dir) = temp_inbox();
        assert_eq!(
            inbox
                .enqueue("t1", "s", Priority::Standard, "a", true)
                .unwrap()
                .seq,
            0
        );
        assert_eq!(
            inbox
                .enqueue("t1", "s", Priority::Standard, "b", true)
                .unwrap()
                .seq,
            1
        );
        // A different recipient has its OWN seq space starting at 0.
        assert_eq!(
            inbox
                .enqueue("t2", "s", Priority::Standard, "c", true)
                .unwrap()
                .seq,
            0
        );
        let d = inbox.depth("t1");
        assert_eq!(d.enqueued, 2);
        assert_eq!(d.next_seq, 2);
    }

    #[test]
    fn idempotent_enqueue_replays_after_restart_and_rejects_payload_reuse() {
        let (inbox, dir) = temp_inbox();
        let first = inbox
            .enqueue_idempotent(
                "agent-1",
                "captain:captain-1",
                Priority::Standard,
                "continue",
                true,
                "followup-1",
            )
            .unwrap();
        assert_eq!(first.seq, 0);
        assert!(!first.duplicate);
        drop(inbox);

        let restored = Inbox::open(dir);
        let replay = restored
            .enqueue_idempotent(
                "agent-1",
                "captain:captain-1",
                Priority::Standard,
                "continue",
                true,
                "followup-1",
            )
            .unwrap();
        assert_eq!(replay.seq, 0);
        assert!(replay.duplicate);
        assert_eq!(restored.depth("agent-1").enqueued, 1);

        assert!(matches!(
            restored.enqueue_idempotent(
                "agent-1",
                "captain:captain-1",
                Priority::Standard,
                "different",
                true,
                "followup-1",
            ),
            Err(EnqueueError::IdempotencyConflict { .. })
        ));
        assert!(matches!(
            restored.enqueue_idempotent(
                "agent-2",
                "captain:captain-1",
                Priority::Standard,
                "continue",
                true,
                "followup-1",
            ),
            Err(EnqueueError::IdempotencyConflict { .. })
        ));
    }

    #[test]
    fn prepared_message_stays_held_across_restart_until_activation() {
        let (inbox, dir) = temp_inbox();
        let prepared = inbox
            .prepare_idempotent(
                "agent-1",
                "captain:captain-1",
                Priority::Standard,
                "new scope",
                true,
                "followup-held",
                "semantic-digest-one",
            )
            .unwrap();
        assert_eq!(prepared.seq, 0);
        assert_eq!(inbox.drain_one("agent-1", |_| Ok(())), DrainOutcome::Empty);
        drop(inbox);

        let restored = Inbox::open(dir);
        assert_eq!(
            restored.drain_one("agent-1", |_| Ok(())),
            DrainOutcome::Empty
        );
        assert!(matches!(
            restored.prepare_idempotent(
                "agent-1",
                "captain:captain-1",
                Priority::Standard,
                "new scope",
                true,
                "followup-held",
                "semantic-digest-two",
            ),
            Err(EnqueueError::IdempotencyConflict { .. })
        ));
        restored
            .activate_prepared("agent-1", "followup-held")
            .unwrap();
        assert_eq!(
            restored.drain_one("agent-1", |_| Ok(())),
            DrainOutcome::Delivered { seq: 0 }
        );
    }

    #[test]
    fn drain_writes_head_then_marks_delivered_at_most_once() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "hello", true)
            .unwrap();
        let sink = std::sync::Mutex::new(Vec::new());
        // First drain writes + delivers seq 0.
        assert_eq!(
            inbox.drain_one("t1", ok_writer(&sink)),
            DrainOutcome::Delivered { seq: 0 }
        );
        // Second drain finds nothing enqueued - a DELIVERED record is never re-picked
        // (at-most-once), so the writer is NOT called again.
        assert_eq!(inbox.drain_one("t1", ok_writer(&sink)), DrainOutcome::Empty);
        let writes = sink.lock().unwrap();
        assert_eq!(
            &*writes,
            &[(0, "hello".to_string(), true)],
            "exactly one write for seq 0"
        );
    }

    #[test]
    fn drains_one_per_boundary_in_fifo_order() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "first", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Standard, "second", true)
            .unwrap();
        let sink = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            inbox.drain_one("t1", ok_writer(&sink)),
            DrainOutcome::Delivered { seq: 0 }
        );
        assert_eq!(
            inbox.drain_one("t1", ok_writer(&sink)),
            DrainOutcome::Delivered { seq: 1 }
        );
        let writes = sink.lock().unwrap();
        assert_eq!(
            writes.iter().map(|w| w.0).collect::<Vec<_>>(),
            vec![0, 1],
            "FIFO by seq"
        );
    }

    #[test]
    fn emergency_drains_ahead_of_standard_but_is_still_seqd() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "std-0", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Emergency, "emg-1", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Emergency, "emg-2", true)
            .unwrap();
        let sink = std::sync::Mutex::new(Vec::new());
        // Emergency records (seq 1,2) drain before the earlier Standard (seq 0), but
        // FIFO within the Emergency class (1 before 2).
        inbox.drain_one("t1", ok_writer(&sink));
        inbox.drain_one("t1", ok_writer(&sink));
        inbox.drain_one("t1", ok_writer(&sink));
        let order: Vec<u64> = sink.lock().unwrap().iter().map(|w| w.0).collect();
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn failed_write_leaves_record_enqueued_and_redelivers() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "retry-me", true)
            .unwrap();
        // A write that fails: the record must NOT advance to delivered.
        let out = inbox.drain_one("t1", |_rec| Err("pty gone".to_string()));
        assert_eq!(
            out,
            DrainOutcome::WriteFailed {
                seq: 0,
                error: "pty gone".to_string()
            }
        );
        assert_eq!(
            inbox.depth("t1").enqueued,
            1,
            "still enqueued after a failed write"
        );
        assert!(
            inbox.depth("t1").inflight.is_none(),
            "in-flight cleared after commit"
        );
        // Next boundary redelivers the same seq; this time it lands.
        let sink = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            inbox.drain_one("t1", ok_writer(&sink)),
            DrainOutcome::Delivered { seq: 0 }
        );
        assert_eq!(inbox.depth("t1").delivered, 1);
    }

    #[test]
    fn survives_restart_and_resumes_from_disk() {
        let (inbox, dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "durable-0", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Standard, "durable-1", true)
            .unwrap();
        // Deliver seq 0 only.
        let sink = std::sync::Mutex::new(Vec::new());
        inbox.drain_one("t1", ok_writer(&sink));
        drop(inbox);
        // Reopen from disk (app restart): seq 0 stays delivered (not re-written),
        // seq 1 is still enqueued and drains.
        let inbox2 = Inbox::open(dir);
        let d = inbox2.depth("t1");
        assert_eq!(d.delivered, 1);
        assert_eq!(d.enqueued, 1);
        assert_eq!(d.next_seq, 2, "seq mint survives restart - no reuse");
        let sink2 = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            inbox2.drain_one("t1", ok_writer(&sink2)),
            DrainOutcome::Delivered { seq: 1 }
        );
        assert_eq!(
            sink2
                .lock()
                .unwrap()
                .iter()
                .map(|w| w.0)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn crash_mid_write_inflight_marker_redelivers_on_reopen() {
        let (inbox, dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "crashy", true)
            .unwrap();
        // Simulate a crash DURING the write: the writer panics-equivalent is a hard
        // process death, which we model by persisting the in-flight marker and NOT
        // committing. We reach into a drain that "hangs" by writing the marker then
        // dropping without commit - emulate by a writer that sets the marker via a
        // real drain whose commit we skip is not possible through the public API, so
        // instead assert the on-disk contract: after a delivered-less enqueue, an
        // in-flight marker on disk is cleared on reopen and the record redelivers.
        // Force an in-flight marker onto disk by starting a drain whose writer stalls
        // is also not expressible; instead we hand-write the marker to mirror a crash.
        let seg = super::segment_path(&dir, "t1");
        let mut q: super::RecipientQueue =
            serde_json::from_str(&std::fs::read_to_string(&seg).unwrap()).unwrap();
        q.inflight = Some(0);
        std::fs::write(&seg, serde_json::to_vec_pretty(&q).unwrap()).unwrap();
        drop(inbox);
        // Reopen: the in-flight marker is a detected crash; it is cleared and the
        // still-enqueued record redelivers (at-least-once).
        let inbox2 = Inbox::open(dir);
        assert!(
            inbox2.depth("t1").inflight.is_none(),
            "crash in-flight marker cleared on load"
        );
        let sink = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            inbox2.drain_one("t1", ok_writer(&sink)),
            DrainOutcome::Delivered { seq: 0 }
        );
    }

    #[test]
    fn ack_moves_delivered_to_processed_and_is_idempotent() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "x", true)
            .unwrap();
        // Ack before delivery is rejected.
        assert_eq!(inbox.ack("t1", 0), AckOutcome::NotDelivered { seq: 0 });
        let sink = std::sync::Mutex::new(Vec::new());
        inbox.drain_one("t1", ok_writer(&sink));
        assert_eq!(inbox.ack("t1", 0), AckOutcome::Processed { seq: 0 });
        // A re-ack (lost-then-retried) is a benign duplicate - never a re-write.
        assert_eq!(inbox.ack("t1", 0), AckOutcome::AlreadyProcessed { seq: 0 });
        assert_eq!(inbox.ack("t1", 99), AckOutcome::Unknown { seq: 99 });
        assert_eq!(inbox.depth("t1").processed, 1);
    }

    #[test]
    fn only_processed_retires_a_record_delivered_is_not_enough() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "x", true)
            .unwrap();
        let sink = std::sync::Mutex::new(Vec::new());
        inbox.drain_one("t1", ok_writer(&sink));
        // Delivered-but-not-acked stays on the books (redelivery is off - it was
        // delivered - but it is NOT retired until processed).
        let d = inbox.depth("t1");
        assert_eq!((d.enqueued, d.delivered, d.processed), (0, 1, 0));
    }

    #[test]
    fn overflow_rejects_standard_but_never_emergency() {
        let dir = std::env::temp_dir().join(format!(
            "t-hub-inbox-of-{}-{}",
            std::process::id(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let inbox = Inbox {
            dir: Some(dir),
            queues: Mutex::new(HashMap::new()),
            max_depth: 2,
            audit_tail: 8,
            telemetry: None,
        };
        inbox
            .enqueue("t1", "s", Priority::Standard, "a", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Standard, "b", true)
            .unwrap();
        // Third standard enqueue overflows the depth-2 bound.
        let err = inbox
            .enqueue("t1", "s", Priority::Standard, "c", true)
            .unwrap_err();
        assert!(matches!(err, EnqueueError::Overflow { max: 2, .. }));
        // Emergency is exempt - a decision is never lost to backpressure.
        assert!(inbox
            .enqueue("t1", "s", Priority::Emergency, "urgent", true)
            .is_ok());
    }

    #[test]
    fn compaction_keeps_a_bounded_processed_audit_tail() {
        let dir = std::env::temp_dir().join(format!(
            "t-hub-inbox-cmp-{}-{}",
            std::process::id(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let inbox = Inbox {
            dir: Some(dir),
            queues: Mutex::new(HashMap::new()),
            max_depth: 256,
            audit_tail: 2,
            telemetry: None,
        };
        // Enqueue, deliver, and ack 5 records; only the newest 2 processed survive.
        for _ in 0..5 {
            inbox
                .enqueue("t1", "s", Priority::Standard, "x", true)
                .unwrap();
        }
        let sink = std::sync::Mutex::new(Vec::new());
        for _ in 0..5 {
            inbox.drain_one("t1", ok_writer(&sink));
        }
        for seq in 0..5 {
            inbox.ack("t1", seq);
        }
        assert_eq!(
            inbox.depth("t1").processed,
            2,
            "audit tail bounds processed retention"
        );
    }

    #[test]
    fn compact_with_zero_audit_tail_does_not_panic() {
        // Review L4: a 0 audit tail must not panic `compact()` (out-of-bounds index)
        // under the queue lock. The guard treats 0 as "retain >= 1".
        let dir = std::env::temp_dir().join(format!(
            "t-hub-inbox-l4-{}-{}",
            std::process::id(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let inbox = Inbox {
            dir: Some(dir),
            queues: Mutex::new(HashMap::new()),
            max_depth: 256,
            audit_tail: 0,
            telemetry: None,
        };
        for _ in 0..3 {
            inbox
                .enqueue("t1", "s", Priority::Standard, "x", true)
                .unwrap();
        }
        let sink = std::sync::Mutex::new(Vec::new());
        for _ in 0..3 {
            inbox.drain_one("t1", ok_writer(&sink));
        }
        // Acking triggers compact with audit_tail 0 - must not panic; at least one
        // processed record is retained.
        for seq in 0..3 {
            inbox.ack("t1", seq);
        }
        assert!(
            inbox.depth("t1").processed >= 1,
            "compact retains >= 1 despite a 0 tail"
        );
    }

    #[test]
    fn purge_recipient_flushes_the_queue() {
        // operate-fleet-infra (Phase 3): purge drops ALL of a recipient's records (a
        // wedged-queue reset) and reports the count; an unknown recipient is a no-op.
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "a", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Emergency, "b", true)
            .unwrap();
        assert_eq!(inbox.purge_recipient("t1"), 2, "both records removed");
        assert_eq!(inbox.depth("t1").enqueued, 0);
        assert!(inbox.depth("t1").inflight.is_none());
        assert_eq!(
            inbox.purge_recipient("no-such"),
            0,
            "unknown recipient is a no-op"
        );
    }

    #[test]
    fn depth_reports_oldest_enqueued_age() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "x", true)
            .unwrap();
        let d = inbox.depth("t1");
        assert!(d.oldest_enqueued_age_ms.is_some());
        assert_eq!(
            inbox.depth("no-such").enqueued,
            0,
            "unknown recipient => empty snapshot"
        );
    }

    #[test]
    fn segment_name_escapes_unsafe_keys() {
        assert_eq!(segment_name("abc-123_x"), "abc-123_x.json");
        // A separator cannot traverse the directory.
        assert!(!segment_name("../etc/passwd").contains('/'));
        assert!(segment_name("../etc").contains('%'));
    }

    #[test]
    fn telemetry_reports_the_full_lifecycle_without_the_body() {
        let (inbox, _dir) = temp_inbox();
        let events: std::sync::Arc<std::sync::Mutex<Vec<(String, u64, usize)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let inbox = inbox.with_telemetry(std::sync::Arc::new(move |ev: &InboxEvent| {
            sink_events
                .lock()
                .unwrap()
                .push((ev.event.to_string(), ev.seq, ev.bytes));
        }));
        inbox
            .enqueue("t1", "crew:abc", Priority::Standard, "secret-body", true)
            .unwrap();
        let sink = std::sync::Mutex::new(Vec::new());
        inbox.drain_one("t1", ok_writer(&sink));
        inbox.ack("t1", 0);
        let got = events.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                ("enqueued".to_string(), 0, "secret-body".len()),
                ("delivered".to_string(), 0, "secret-body".len()),
                ("processed".to_string(), 0, "secret-body".len()),
            ],
            "the full lifecycle is reported as distinct events (D6), by length not content"
        );
    }

    #[test]
    fn telemetry_reports_a_failed_write() {
        let (inbox, _dir) = temp_inbox();
        let events: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let inbox = inbox.with_telemetry(std::sync::Arc::new(move |ev: &InboxEvent| {
            sink_events.lock().unwrap().push(ev.event.to_string());
        }));
        inbox
            .enqueue("t1", "s", Priority::Standard, "x", true)
            .unwrap();
        inbox.drain_one("t1", |_rec| Err("boom".to_string()));
        assert_eq!(
            events.lock().unwrap().clone(),
            vec!["enqueued".to_string(), "writeFailed".to_string()]
        );
    }

    #[test]
    fn concurrent_drain_sees_busy_while_a_write_is_in_flight() {
        let (inbox, _dir) = temp_inbox();
        inbox
            .enqueue("t1", "s", Priority::Standard, "a", true)
            .unwrap();
        inbox
            .enqueue("t1", "s", Priority::Standard, "b", true)
            .unwrap();
        // Thread A holds a record in-flight (its writer blocks on a channel); while it
        // is mid-write, thread B's drain must see Busy, never start a second write.
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let inbox_ref = &inbox; // shared &Inbox (Inbox: Sync) across both threads
        std::thread::scope(|scope| {
            let a = scope.spawn(move || {
                // Own the channel ends inside the thread (Receiver is not Sync); the
                // inbox is shared by reference.
                inbox_ref.drain_one("t1", move |_rec| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap(); // block inside the write, in-flight held
                    Ok(())
                })
            });
            started_rx.recv().unwrap(); // A is now mid-write with seq 0 in-flight
                                        // B tries to drain concurrently and must be told Busy (at-most-once + FIFO).
            assert_eq!(inbox.drain_one("t1", |_rec| Ok(())), DrainOutcome::Busy);
            release_tx.send(()).unwrap();
            assert_eq!(a.join().unwrap(), DrainOutcome::Delivered { seq: 0 });
        });
        // After A commits, B can drain seq 1.
        let sink = std::sync::Mutex::new(Vec::new());
        assert_eq!(
            inbox.drain_one("t1", ok_writer(&sink)),
            DrainOutcome::Delivered { seq: 1 }
        );
    }
}
