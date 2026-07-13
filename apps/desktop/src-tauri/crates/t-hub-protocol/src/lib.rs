//! `t-hub-protocol` — the versioned NDJSON-over-stdio contract between the
//! T-Hub **core** (the Tauri app, Windows side) and the **`t-hub-agent`**
//! (the WSL-side control binary).
//!
//! This crate is the *single source of truth* for the agent⇄core wire format.
//! It is mirrored field-for-field in TypeScript at `src/ipc/protocol.ts` (which
//! exists only so the frontend can render journal/agent payloads the core
//! forwards to it — the frontend never speaks this protocol directly).
//!
//! ## Why this shape (carry-forward truths from REVIEW.md / PLAN.md)
//!
//! 1. **Process durability is app-close-only; conversation durability is
//!    reboot-survivable.** The agent therefore owns a *durable, append-only*
//!    [`EventJournalEntry`] log on the WSL VHDX. The journal — not live process
//!    state — is the authority for reconstruction intent, and it is replayed to
//!    the core on every (re)connect.
//!
//! 2. **The event spine is: Claude hook → WSL journal → agent → core → UI.**
//!    Hook handler scripts append to the journal and ping the agent; the agent
//!    forwards new journal entries to the core as [`AgentToCore::Journal`]
//!    frames; the core fans them out to the UI.
//!
//! 3. **NDJSON head-of-line blocking is real** (REVIEW): a bulk read on the same
//!    pipe stalls a metrics ping. We mitigate this *in the protocol itself* with
//!    an explicit [`Channel`] tag on every frame and a [`Priority`] on every
//!    request, so the transport can interleave/serve control + metrics ahead of
//!    bulk payloads without a second OS pipe.
//!
//! ## Framing
//!
//! Every line on stdio is exactly one JSON object — a [`Frame`] — terminated by
//! `\n`. There is no length prefix; readers split on newlines and parse each
//! line independently. A malformed line is reported via [`AgentToCore::Error`]
//! / [`CoreToAgent`]-side logging and skipped; it never desynchronizes the
//! stream (the next `\n` is a clean frame boundary).
//!
//! ## Versioning
//!
//! [`PROTOCOL_VERSION`] is sent in the [`Hello`]/[`Ready`] handshake. The core
//! and agent record each other's version; unknown enum variants deserialize via
//! `#[serde(other)]` catch-alls where present so a newer peer does not crash an
//! older one. Bump [`PROTOCOL_VERSION`] on any breaking change to a message
//! shape; additive (new optional field / new variant with a catch-all) changes
//! keep the same major and only require a peer that ignores what it doesn't
//! understand.

use serde::{Deserialize, Serialize};

/// The wire-format version advertised in the handshake. Bump on breaking change.
///
/// History:
///   - `1` — initial 0.5 contract: registry/metrics/git/journal/hook + status.
pub const PROTOCOL_VERSION: u32 = 1;

/// Logical channel a [`Frame`] travels on. The single stdio pipe is multiplexed
/// by this tag so the transport can prioritize control/metrics over bulk reads
/// (NDJSON head-of-line-blocking mitigation, REVIEW). It is advisory metadata —
/// every frame still arrives in stream order — but lets a scheduler on either
/// side reorder *its own* outbound work and account for in-flight bulk traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Handshake, lifecycle, registry mutations, small control RPCs. Highest
    /// priority; never blocked behind bulk traffic.
    Control,
    /// Periodic host metrics (RAM/CPU/load/...). Time-sensitive; must not sit
    /// behind a large read.
    Metrics,
    /// The hook → journal → core event spine (journal entries, hook/status
    /// ingestion notifications). Ordered and durable but not latency-critical.
    Events,
    /// Potentially large or slow payloads (scrollback dumps, future file reads).
    /// Lowest priority; designed to be interleavable so it can't starve the
    /// others.
    Bulk,
}

impl Channel {
    /// A small integer where **lower = more urgent**, for use as a scheduling
    /// key. Stable across versions.
    pub fn rank(self) -> u8 {
        match self {
            Channel::Control => 0,
            Channel::Metrics => 1,
            Channel::Events => 2,
            Channel::Bulk => 3,
        }
    }
}

/// Per-request urgency hint. Distinct from [`Channel`] (which classifies the
/// *kind* of traffic): two `Control` requests can still carry different
/// priorities. Lower numeric rank = served first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// User-blocking control (spawn/kill/attach intent, resume guards).
    High,
    /// Default for routine RPCs.
    #[default]
    Normal,
    /// Background/best-effort (bulk dumps, opportunistic refreshes).
    Low,
}

impl Priority {
    pub fn rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Normal => 1,
            Priority::Low => 2,
        }
    }
}

/// Correlates a [`CoreToAgent::Request`] with its [`AgentToCore::Response`].
/// Monotonic per core process; the agent echoes it verbatim. Notifications and
/// unsolicited events carry no id.
pub type RequestId = u64;

/// The top-level NDJSON frame. Exactly one is encoded per line. The `dir` is
/// implicit in *who writes it* (core writes [`CoreToAgent`]; agent writes
/// [`AgentToCore`]); we keep them as separate enums rather than one tagged union
/// so each side's match is exhaustive over only the messages it can receive.
///
/// We serialize a thin wrapper carrying the [`Channel`] + payload so the
/// transport scheduler can read the channel without fully parsing the payload
/// body on the hot path (it still must parse the line, but `channel` is a
/// top-level key it can branch on cheaply).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreFrame {
    pub channel: Channel,
    #[serde(flatten)]
    pub msg: CoreToAgent,
}

/// The agent-authored counterpart to [`CoreFrame`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFrame {
    pub channel: Channel,
    #[serde(flatten)]
    pub msg: AgentToCore,
}

// ---------------------------------------------------------------------------
// Core → Agent
// ---------------------------------------------------------------------------

/// Messages the **core** sends to the **agent**. Tagged by `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreToAgent {
    /// First frame the core sends. Advertises protocol version + core build.
    Hello(Hello),
    /// A correlated request expecting exactly one [`AgentToCore::Response`].
    Request {
        id: RequestId,
        #[serde(default)]
        priority: Priority,
        #[serde(flatten)]
        body: AgentRequest,
    },
    /// Liveness ping; the agent replies with [`AgentToCore::Pong`]. Carries a
    /// nonce so RTT can be measured and stale pongs ignored.
    Ping { nonce: u64 },
    /// Ask the agent to replay journal entries with sequence `> after_seq`
    /// (0 = from the beginning). Replayed entries arrive as
    /// [`AgentToCore::Journal`] frames, then a [`AgentToCore::ReplayComplete`].
    ReplayJournal { after_seq: u64 },
    /// Graceful shutdown request; the agent flushes the journal and exits.
    Shutdown,
    /// An unknown/future message kind — ignored by older agents.
    #[serde(other)]
    Unknown,
}

/// The handshake the core opens with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    /// Human-readable core build string (e.g. `"t-hub 0.5.0"`).
    pub core_version: String,
}

/// The body of a [`CoreToAgent::Request`]. These are the agent's RPC surface for
/// 0.5: tmux/session registry, host metrics, git/worktree queries, and journal
/// control. Tagged by `op`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentRequest {
    // --- tmux / session registry (Channel::Control) ---
    /// List tmux sessions on the isolated `t-hub` socket.
    ListSessions,
    /// Create a detached tmux session.
    NewSession {
        name: String,
        cwd: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    /// Whether a tmux session exists.
    HasSession { name: String },
    /// Kill a tmux session (terminate its process tree).
    KillSession { name: String },

    // --- host metrics (Channel::Metrics) ---
    /// One-shot snapshot of WSL host metrics (RAM/swap/CPU/load/...).
    Metrics,

    // --- git / worktree (Channel::Control) ---
    /// `git branch --show-current` in `cwd` (the statusline does NOT carry the
    /// non-worktree branch; we derive it here — PLAN.md §H / REVIEW).
    GitBranch { cwd: String },
    /// `git worktree list --porcelain` for the repo containing `cwd`.
    GitWorktrees { cwd: String },

    // --- bulk (Channel::Bulk) ---
    /// Capture pane scrollback (potentially large → routed on the bulk channel).
    CapturePane { name: String },

    /// Unknown/future op — agent responds with an `unsupported` error.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Agent → Core
// ---------------------------------------------------------------------------

/// Messages the **agent** sends to the **core**. Tagged by `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToCore {
    /// First frame the agent sends, in reply to [`Hello`]. Confirms the agreed
    /// protocol version and reports the agent build + host facts.
    Ready(Ready),
    /// The correlated reply to a [`CoreToAgent::Request`].
    Response {
        id: RequestId,
        #[serde(flatten)]
        body: AgentResponse,
    },
    /// Reply to [`CoreToAgent::Ping`].
    Pong { nonce: u64 },
    /// A durable journal entry, either streamed live (hook spine) or replayed.
    /// Carries the monotonic `seq` so the core can de-dupe and resume replay.
    Journal { seq: u64, entry: EventJournalEntry },
    /// Marks the end of a [`CoreToAgent::ReplayJournal`] batch; `last_seq` is the
    /// highest sequence replayed (so the core can advance its cursor).
    ReplayComplete { last_seq: u64 },
    /// Out-of-band agent-level error not tied to a specific request.
    Error { message: String },
    /// An unknown/future message kind — ignored by older cores.
    #[serde(other)]
    Unknown,
}

/// The agent's handshake reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ready {
    pub protocol_version: u32,
    /// Human-readable agent build string (e.g. `"t-hub-agent 0.5.0"`).
    pub agent_version: String,
    /// The distro name the agent is running in, when derivable (`/etc/os-release`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Highest journal sequence the agent currently holds (so the core can
    /// decide whether it needs a replay before going live).
    pub journal_head_seq: u64,
}

/// The body of an [`AgentToCore::Response`], tagged by `result`. Each variant
/// corresponds to one [`AgentRequest`] (or the shared `error`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum AgentResponse {
    /// `list_sessions` → tmux session names on the `t-hub` socket.
    Sessions { names: Vec<String> },
    /// `new_session` succeeded.
    SessionCreated,
    /// `has_session` → existence.
    SessionExists { exists: bool },
    /// `kill_session` succeeded (idempotent — also Ok if already gone).
    SessionKilled,
    /// `metrics` → a host snapshot.
    Metrics(HostMetrics),
    /// `git_branch` → the current branch (None when detached/not a repo).
    GitBranch {
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// `git_worktrees` → parsed worktree entries.
    GitWorktrees { worktrees: Vec<WorktreeInfo> },
    /// `capture_pane` → base64-encoded scrollback bytes (ANSI preserved).
    Pane { base64: String },
    /// Any request that failed. `kind` is a stable machine-readable code; see
    /// [`ResponseErrorKind`].
    Error {
        kind: ResponseErrorKind,
        message: String,
    },
}

/// Stable, machine-readable error codes for an [`AgentResponse::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseErrorKind {
    /// The op isn't supported by this agent version.
    Unsupported,
    /// The target (session/pane/path) doesn't exist.
    NotFound,
    /// An underlying command (tmux/git) failed.
    CommandFailed,
    /// Malformed request arguments.
    BadRequest,
    /// Catch-all for anything else.
    Internal,
}

// ---------------------------------------------------------------------------
// Shared payload types
// ---------------------------------------------------------------------------

/// A snapshot of WSL host health, surfaced in the utility area (PLAN.md §H).
/// All memory values are **kibibytes** (as reported by `/proc/meminfo`); load
/// averages are the raw 1/5/15-minute figures from `/proc/loadavg`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostMetrics {
    pub mem_total_kib: u64,
    pub mem_available_kib: u64,
    pub swap_total_kib: u64,
    pub swap_free_kib: u64,
    /// Number of logical CPUs.
    pub cpu_count: u32,
    /// 1/5/15-minute load averages.
    pub load_avg: [f32; 3],
    /// Total process count (entries under `/proc`).
    pub process_count: u32,
    /// Distro name from `/etc/os-release` `PRETTY_NAME`, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Unix-epoch milliseconds the snapshot was taken (agent clock).
    pub captured_at_ms: u64,
}

/// One entry from `git worktree list --porcelain`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// True for the `bare` repo entry; false for normal worktrees.
    #[serde(default)]
    pub bare: bool,
    /// True when porcelain reports this worktree `detached`.
    #[serde(default)]
    pub detached: bool,
}

// ---------------------------------------------------------------------------
// Event journal (the durable spine)
// ---------------------------------------------------------------------------

/// A single durable entry in the WSL-side append-only event journal
/// (PRD §8 / PLAN.md data-model). The journal survives the Windows app being
/// closed and is replayed on reconnect; it is the authority for reconstruction
/// intent, not live process state.
///
/// Mirrors `EventJournalEntry` in `src/ipc/protocol.ts`. Field set deliberately
/// matches the PRD's `(timestamp, source, entity_id, event_type, payload,
/// result)` tuple, plus a stable `seq` the agent assigns on append.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventJournalEntry {
    /// Monotonic sequence assigned by the agent on append (1-based). Used for
    /// de-dupe + replay cursors. Not serialized into the on-disk *payload* line
    /// (the line's position is the seq) but echoed in [`AgentToCore::Journal`].
    #[serde(default)]
    pub seq: u64,
    /// Unix-epoch milliseconds the event was recorded (agent clock).
    pub timestamp_ms: u64,
    /// Who/what produced the entry.
    pub source: JournalSource,
    /// The primary entity the entry concerns — usually a Claude `session_id`,
    /// a subagent `agent_id`, or a tmux session name. Free-form by design so the
    /// journal can carry events about entities the core models loosely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    /// The event kind (see [`JournalEventType`]).
    pub event_type: JournalEventType,
    /// Arbitrary structured payload (e.g. the raw hook stdin object, status
    /// JSON, or a command's parameters). Kept as untyped JSON so the journal
    /// schema is stable even as hook payloads evolve.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Optional outcome of a recorded *action* (command success/failure text);
    /// `None` for pure observations like hook firings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// The origin of a [`EventJournalEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalSource {
    /// A Claude Code hook handler appended this (the event spine).
    Hook,
    /// Claude's statusline status bridge appended this.
    Status,
    /// The agent itself recorded an action/observation (tmux/git/metrics).
    Agent,
    /// The Windows core recorded a recovery/lifecycle action via the agent.
    Core,
    /// Unknown/future source.
    #[serde(other)]
    Unknown,
}

/// The kind of event a journal entry records. The hook-derived variants use the
/// **exact Claude Code hook names** verified in REVIEW.md so the mapping from a
/// hook firing to a journal entry is mechanical. Non-hook variants cover the
/// agent/core/status sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEventType {
    // --- Claude Code hooks (verified names, REVIEW.md §9.6) ---
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    Stop,
    StopFailure,
    PermissionRequest,
    Notification,
    Elicitation,
    SubagentStart,
    SubagentStop,
    TaskCreated,
    TaskCompleted,
    CwdChanged,
    WorktreeCreate,
    WorktreeRemove,

    // --- status bridge ---
    /// A statusline JSON snapshot was ingested for a session.
    StatusSnapshot,

    // --- agent / core lifecycle + actions ---
    /// The agent connected (handshake completed).
    AgentConnected,
    /// A tmux/git/metrics command the agent ran (result in `result`).
    AgentCommand,
    /// A recovery/lifecycle action the core drove (result in `result`).
    CoreAction,

    /// Unknown/future event type.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// (De)serialization helpers — one NDJSON line ⇄ one frame.
// ---------------------------------------------------------------------------

/// Serialize a core→agent frame as a single NDJSON line (no trailing newline;
/// the caller appends `\n`). Returns `serde_json::Error` only if the value
/// somehow can't be serialized (it always can for these types).
pub fn encode_core(frame: &CoreFrame) -> Result<String, serde_json::Error> {
    serde_json::to_string(frame)
}

/// Serialize an agent→core frame as a single NDJSON line.
pub fn encode_agent(frame: &AgentFrame) -> Result<String, serde_json::Error> {
    serde_json::to_string(frame)
}

/// Parse one NDJSON line into a core→agent frame (the agent's reader side).
pub fn decode_core(line: &str) -> Result<CoreFrame, serde_json::Error> {
    serde_json::from_str(line)
}

/// Parse one NDJSON line into an agent→core frame (the core's reader side).
pub fn decode_agent(line: &str) -> Result<AgentFrame, serde_json::Error> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    #[test]
    fn channel_and_priority_ranks_are_ordered() {
        assert!(Channel::Control.rank() < Channel::Metrics.rank());
        assert!(Channel::Metrics.rank() < Channel::Events.rank());
        assert!(Channel::Events.rank() < Channel::Bulk.rank());
        assert!(Priority::High.rank() < Priority::Normal.rank());
        assert!(Priority::Normal.rank() < Priority::Low.rank());
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn hello_ready_handshake_roundtrips() {
        let hello = CoreFrame {
            channel: Channel::Control,
            msg: CoreToAgent::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                core_version: "t-hub 0.5.0".into(),
            }),
        };
        let line = encode_core(&hello).unwrap();
        assert!(!line.contains('\n'), "a frame must be a single line");
        let back = decode_core(&line).unwrap();
        match back.msg {
            CoreToAgent::Hello(h) => assert_eq!(h.protocol_version, PROTOCOL_VERSION),
            other => panic!("expected Hello, got {other:?}"),
        }
        assert_eq!(back.channel, Channel::Control);
    }

    #[test]
    fn request_response_roundtrip_with_priority_and_id() {
        let req = CoreFrame {
            channel: Channel::Control,
            msg: CoreToAgent::Request {
                id: 42,
                priority: Priority::High,
                body: AgentRequest::NewSession {
                    name: "th_abc".into(),
                    cwd: "/home/u".into(),
                    command: None,
                },
            },
        };
        let line = encode_core(&req).unwrap();
        let back = decode_core(&line).unwrap();
        match back.msg {
            CoreToAgent::Request { id, priority, body } => {
                assert_eq!(id, 42);
                assert_eq!(priority, Priority::High);
                match body {
                    AgentRequest::NewSession { name, .. } => assert_eq!(name, "th_abc"),
                    other => panic!("expected NewSession, got {other:?}"),
                }
            }
            other => panic!("expected Request, got {other:?}"),
        }

        let resp = AgentFrame {
            channel: Channel::Control,
            msg: AgentToCore::Response {
                id: 42,
                body: AgentResponse::SessionCreated,
            },
        };
        let line = encode_agent(&resp).unwrap();
        let back = decode_agent(&line).unwrap();
        match back.msg {
            AgentToCore::Response { id, body } => {
                assert_eq!(id, 42);
                assert!(matches!(body, AgentResponse::SessionCreated));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn journal_entry_roundtrips_with_hook_payload() {
        let entry = EventJournalEntry {
            seq: 7,
            timestamp_ms: now_ms(),
            source: JournalSource::Hook,
            entity_id: Some("sess-123".into()),
            event_type: JournalEventType::SessionStart,
            payload: serde_json::json!({
                "session_id": "sess-123",
                "cwd": "/home/u/proj",
                "transcript_path": "/home/u/.claude/projects/proj/sess-123.jsonl"
            }),
            result: None,
        };
        let frame = AgentFrame {
            channel: Channel::Events,
            msg: AgentToCore::Journal { seq: 7, entry },
        };
        let line = encode_agent(&frame).unwrap();
        let back = decode_agent(&line).unwrap();
        match back.msg {
            AgentToCore::Journal { seq, entry } => {
                assert_eq!(seq, 7);
                assert_eq!(entry.event_type, JournalEventType::SessionStart);
                assert_eq!(entry.entity_id.as_deref(), Some("sess-123"));
                assert_eq!(entry.payload["cwd"], "/home/u/proj");
            }
            other => panic!("expected Journal, got {other:?}"),
        }
    }

    #[test]
    fn unknown_request_op_decodes_to_unknown_not_error() {
        // Forward-compat: a newer core op an older agent doesn't know must
        // decode to the catch-all, not fail the whole line.
        let line = r#"{"channel":"control","type":"request","id":1,"op":"some_future_op","x":1}"#;
        let back = decode_core(line).unwrap();
        match back.msg {
            CoreToAgent::Request { body, .. } => assert!(matches!(body, AgentRequest::Unknown)),
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_type_decodes_to_unknown() {
        let line = r#"{"channel":"control","type":"some_future_message"}"#;
        let back = decode_core(line).unwrap();
        assert!(matches!(back.msg, CoreToAgent::Unknown));
    }
}
