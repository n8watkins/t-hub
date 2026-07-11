//! Comms plane - PHASE 1: Single Write Authority as the PRIMARY write path.
//!
//! This module is the single, named seam every AGENT/AUTOMATION byte destined for
//! a terminal's input is meant to funnel through. It exists so that, from Phase 1
//! on, there is exactly ONE primary door for automation input and every write that
//! goes around it is a loudly-marked, audited BREAK-GLASS deviation rather than a
//! silent second writer.
//!
//! HONEST SCOPE - what Phase 1 is and is NOT (see the ratified design, §0.1 H2 and
//! §3.2 Phase 1). Phase 1 establishes the invariant as PRIMARY-now, NOT LAW-now:
//!
//! - It is NOT durable. There is no queue, no seq, no receipt state machine, no
//!   redelivery. Those are Phase 2. `deliver_*` here writes straight to the
//!   substrate exactly as the pre-plane code did - the win is that the write is now
//!   funnelled + attributed, not that it survives a crash.
//! - It does NOT enforce ACLs. Who may write to whom is Phase 3. This module does
//!   not consult any capability/role table.
//! - It does NOT gate on a human typing. The `NOT human_busy` predicate is Phase 4.
//! - Break-glass is DEMOTED, not DENIED. The MCP `send_text`/`send_keys` handlers
//!   still work; they just emit a loud marker on every use (control::mark_break_glass).
//!   They remain callable by any Full-token session - i.e. every crew today - until
//!   program item 3 tiers the control token away from crew. So Single Write
//!   Authority is the PRIMARY path here, and only becomes an absolute wall after
//!   item 3. This module claims nothing stronger.
//!
//! Attribution is likewise coarse in Phase 1: `WriteSource` names WHICH internal
//! automation writer enqueued (fleet wake / auto-continue / rules engine), not a
//! per-session cryptographic identity - that is the Phase 2 identity slice. The
//! marker is a `t-hub-plane:` log line (greppable) plus, for break-glass, a live
//! `control://break-glass` fanout event.

use crate::tmux;

/// The internal automation writers that Phase 1 migrates onto the plane's primary
/// path. This is coarse provenance (which subsystem), NOT the per-session identity
/// the Phase 2 slice adds; it is enough to attribute a primary write in telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteSource {
    /// `fleet.rs` orchestrator wake (control/tmux substrate, path a).
    FleetWake,
    /// Auto-continue usage-limit recovery (#47, in-app write substrate, path b).
    AutoContinue,
    /// The rules engine `sendText` / `run` actions (in-app write substrate, path b).
    RulesEngine,
}

impl WriteSource {
    /// Stable, kebab-case label for logs, telemetry, and the IPC contract.
    pub fn label(self) -> &'static str {
        match self {
            WriteSource::FleetWake => "fleet-wake",
            WriteSource::AutoContinue => "auto-continue",
            WriteSource::RulesEngine => "rules-engine",
        }
    }

    /// Resolve the label the frontend `deliver_agent_input` IPC carries back to a
    /// known automation source. Unknown labels are REFUSED rather than silently
    /// accepted, so the plane's automation door cannot be repurposed as a generic
    /// bypass by passing an arbitrary source string.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "fleet-wake" => Ok(WriteSource::FleetWake),
            "auto-continue" => Ok(WriteSource::AutoContinue),
            "rules-engine" => Ok(WriteSource::RulesEngine),
            other => Err(format!(
                "unknown plane write source '{other}' (expected one of: \
                 fleet-wake, auto-continue, rules-engine)"
            )),
        }
    }
}

/// Record a PRIMARY plane write - the Single-Write-Authority path. Phase 1 does not
/// persist or gate this; the record is the attribution/telemetry marker that lets
/// an operator see automation input flowing through the one primary door. `bytes`
/// is the payload length only (never the content - the content can carry secrets,
/// mirroring the audit log's redaction discipline).
pub fn note_primary(source: WriteSource, target: &str, bytes: usize) {
    eprintln!(
        "t-hub-plane: write source={} target={} bytes={}",
        source.label(),
        target,
        bytes
    );
}

/// Record a BREAK-GLASS deviation - a write that went around the primary path (the
/// demoted MCP `send_text`/`send_keys`, `th send`, or raw tmux). Phase 1 does NOT
/// block these (H2); it marks them LOUDLY so a break-glass path can never quietly
/// become the primary path again (design D11 recommendation a). Attribution is
/// coarse in Phase 1: the shared control token cannot yet name the calling session
/// (per-session identity is the Phase 2 slice), so we name the COMMAND that
/// deviated, not who called it.
pub fn note_break_glass(command: &str, target: &str, bytes: usize) {
    eprintln!("t-hub-plane: BREAK-GLASS command={command} target={target} bytes={bytes}");
}

/// Primary-path delivery over the control/tmux substrate (path a). The fleet wake
/// injector routes through here instead of calling `tmux::send_text` directly, so
/// the wake is the plane's first primary writer. Phase 1: the underlying write is
/// still an immediate `tmux::send_text` (no durability yet); the readiness gate
/// (`is_ready_for_wake`) stays with the caller in `fleet.rs`, unchanged.
pub fn deliver_tmux(
    target: &str,
    text: &str,
    enter: bool,
    source: WriteSource,
) -> Result<(), String> {
    note_primary(source, target, text.len());
    tmux::send_text(target, text, enter).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_source_labels_are_stable_kebab_case() {
        assert_eq!(WriteSource::FleetWake.label(), "fleet-wake");
        assert_eq!(WriteSource::AutoContinue.label(), "auto-continue");
        assert_eq!(WriteSource::RulesEngine.label(), "rules-engine");
    }

    #[test]
    fn parse_round_trips_every_known_source() {
        for src in [
            WriteSource::FleetWake,
            WriteSource::AutoContinue,
            WriteSource::RulesEngine,
        ] {
            assert_eq!(WriteSource::parse(src.label()), Ok(src));
        }
    }

    #[test]
    fn parse_refuses_an_unknown_source() {
        // The automation door must not accept an arbitrary source string, or it
        // becomes a generic bypass of the primary-path attribution.
        let err = WriteSource::parse("send_text").unwrap_err();
        assert!(err.contains("unknown plane write source"), "got: {err}");
        assert!(WriteSource::parse("").is_err());
    }
}
