//! Claude adapter (PLAN.md Workstream B): hook-handler installation, the status
//! bridge, and exact session-ID capture.
//!
//! ## Pieces
//!   - [`hooks`]: the exact verified hook set, the handler-script content, and
//!     the **non-destructive** `~/.claude/settings.json` installer/uninstaller
//!     (explicit consent, merges with the user's existing hooks, survives
//!     hand-edits — REVIEW risk).
//!   - [`status`]: the status bridge that ingests Claude's statusline JSON and
//!     keeps the latest snapshot per exact session id (context %, usage,
//!     rate-limit block when present, with the Pro/Max + after-first-response
//!     caveat).
//!   - Exact session-ID capture at `SessionStart` (from the `session_id` base
//!     field) feeds [`crate::model::AgentSessionRecord`] via the journal spine.
//!
//! ## Status
//! Module boundaries + the hook list + the status-snapshot type are contract.
//! The installer's file-merge logic and the SDK-backed
//! discover/resume/fork/verify ops are SUBAGENT(claude-adapter)'s to implement.
//!
//! Boundary: SUBAGENT(claude-adapter) owns this directory (`claude/`). It must
//! not change `t-hub-protocol`, `model.rs`, or `supervision.rs`.

pub mod hooks;
pub mod install;
pub mod status;

// Public contract surface (mirrored in src/ipc/model.ts). `RateLimitWindow` is
// re-exported for consumers/tests even though nothing in this crate names it
// directly yet, so the adapter's status types are reachable as `claude::*`.
#[allow(unused_imports)]
pub use status::{RateLimitWindow, StatusBridge, StatusSnapshot};
#[allow(unused_imports)]
pub use install::InstallReport;
