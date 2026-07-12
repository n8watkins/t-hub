//! Comms plane - PHASE 3: the ACL policy (the settled capability matrix as PURE
//! predicates). This module is the single, testable source of truth for "who may do
//! what to whom" on the plane; `control.rs` WIRES these predicates at the concrete
//! enforcement points (the read/write session handlers, the plane send, the abort
//! primitive, the inbox ack, and the fleet-infra admin surface).
//!
//! It is deliberately kept PURE - no I/O, no tmux, no locks, no `ControlContext`. It
//! answers a decision over plain inputs (`AclActor` + a `ShipRef`/`MessageTarget`) so
//! every cell of the matrix is unit-testable to the "bypass-would-fail" bar without a
//! socket or a live session. The matrix rows are ratified
//! (`capability-matrix-draft.md`, the general 2026-07-10); this encodes them verbatim.
//!
//! HONEST SCOPE - what Phase 3 is and is NOT (ratified design §3.2 Phase 3):
//!
//! - It IS the enqueue-time / access-time authorization: the message DOWN/UP rows, the
//!   sibling no-daisy-chain denial as an explicit cell, cross-ship read/message
//!   ISOLATION (the one mechanization add, H3), the EMERGENCY-flag authority
//!   (GENERAL + CAPTAINS ONLY), the abort/interrupt authority, inbox-ack self-scope,
//!   the operate-fleet-infra owner, and who may ORIGINATE a general-authorization.
//! - It does NOT build the Phase-4 typing-guard. `NOT human_busy(T)` stays UNBUILT;
//!   the drain predicate is untouched. Emergency here is purely WHO-may-flag, never the
//!   fast-lane drain relaxation (that is a Phase-4 drain concern on the same queue).
//! - FAIL-OPEN vs the trusted host is a WIRING decision, not a policy one: these
//!   predicates decide for an IDENTIFIED actor. `control.rs` fails open for a caller
//!   that presents NO per-session identity (the app's own webview / MCP / fleet wake
//!   over the control token) - the NORM-now / LAW-target staging the design mandates
//!   (§2.6: isolation is enforced for identified sessions once item-2's ship key
//!   exists, best-effort/NORM for the trusted control-token host).

/// The effective org role an ACL decision keys on. Derived from a resolved identity by
/// [`AclRole::resolve`]: the registry-authoritative FLEET role (Cortana/Captain) when
/// the session is a supervisor terminal, else the mint-time role (Crew/General/Unknown).
/// Kept independent of `control::FleetRole` / `identity::Role` so this module has no
/// upward dependency (the caller maps its role kinds into this at the boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclRole {
    /// The human apex. Not spawned as a session today, but the matrix's top row.
    General,
    /// The apex orchestrator (owns captains fleet-wide).
    Cortana,
    /// A ship's captain (owns its own crew).
    Captain,
    /// A worker under a captain (the least-privilege default).
    Crew,
    /// Role not resolvable (a session that never minted a role, or an unknown mint).
    Unknown,
}

impl AclRole {
    /// Stable label for a denial reason / audit stamp.
    pub fn label(self) -> &'static str {
        match self {
            AclRole::General => "general",
            AclRole::Cortana => "cortana",
            AclRole::Captain => "captain",
            AclRole::Crew => "crew",
            AclRole::Unknown => "unknown",
        }
    }
}

/// The caller an ACL decision is made for: its effective role, its durable ship, and
/// its tile (the mutable session pointer, used for the inbox-ack self-scope). Built by
/// `control.rs` from a `ResolvedIdentity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclActor {
    pub role: AclRole,
    /// The durable ship this session belongs to (`None` when not yet resolvable).
    pub ship: Option<String>,
    /// The tile this session is bound to (`None` before the bind lands).
    pub tile: Option<String>,
    /// The minted per-session id (attribution handle for the denial stamp).
    pub session_id: String,
}

impl AclActor {
    fn same_ship(&self, other: Option<&str>) -> bool {
        match (self.ship.as_deref(), other) {
            (Some(a), Some(b)) => a == b,
            // A shipless caller / a shipless target never matches by ship (fail-safe:
            // an identified-but-shipless session cannot reach an owned session).
            _ => false,
        }
    }
}

/// A target session's ship membership (the access/abort target). Mapped by `control.rs`
/// from `CaptainsRegistry::ship_of`. `Unowned` = the tile belongs to no ship (an
/// unregistered pane) - nothing to isolate, so cross-ship isolation does not fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShipRef {
    /// A supervisor terminal (a captain or Cortana) and its ship.
    Supervisor { ship: String },
    /// A crew tile and its ship.
    Crew { ship: String },
    /// The tile belongs to no ship (unregistered).
    Unowned,
}

impl ShipRef {
    fn ship(&self) -> Option<&str> {
        match self {
            ShipRef::Supervisor { ship } | ShipRef::Crew { ship } => Some(ship),
            ShipRef::Unowned => None,
        }
    }
    fn is_supervisor(&self) -> bool {
        matches!(self, ShipRef::Supervisor { .. })
    }
}

/// A message recipient for the send ACL: its effective role + its ship. `control.rs`
/// resolves the recipient tile's membership into this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageTarget {
    pub role: AclRole,
    pub ship: Option<String>,
}

/// An ACL denial: an attributed, human-legible reason (never a silent drop). The
/// design mandates denials are refused AND attributed (§3.2 Phase 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub reason: String,
}

impl Denied {
    fn new(reason: impl Into<String>) -> Self {
        Denied {
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

/// The result of an ACL check: `Ok(())` = permitted; `Err(Denied)` = refused+attributed.
pub type AclResult = Result<(), Denied>;

// ---------------------------------------------------------------------------
// Cross-ship read / message ISOLATION (H3 - the one mechanization add, §2.6)
// ---------------------------------------------------------------------------

/// May `caller` READ or WRITE the session identified by `target`? This is the
/// handler-level cross-ship ownership ACL that closes the read/write hole the queue
/// model does NOT cover (`read_terminal` bypasses the plane entirely; break-glass
/// `send_text`/`send_keys` reach any pane). Keyed on the per-session identity's ship.
///
/// Rules (§2.6, corrected to clause 1):
/// - GENERAL may access anyone.
/// - CORTANA (apex, owns captains fleet-wide) may access a SUPERVISOR session on any
///   ship (her subordinate captains) and anything on her own ship; she may NOT reach
///   into another ship's CREW directly (no skip-level).
/// - CAPTAIN / CREW / UNKNOWN may access ONLY a session on their OWN ship.
/// - An `Unowned` target is not "another ship's secret", so isolation does not fire.
///
/// The FAIL-OPEN for a tokenless control-host caller is applied by the wiring, not here
/// (this predicate decides for an IDENTIFIED actor).
pub fn can_access_session(caller: &AclActor, target: &ShipRef) -> AclResult {
    if caller.role == AclRole::General {
        return Ok(());
    }
    // Nothing to isolate: a tile owned by no ship is not a cross-ship secret.
    if matches!(target, ShipRef::Unowned) {
        return Ok(());
    }
    match caller.role {
        AclRole::Cortana => {
            if target.is_supervisor() || caller.same_ship(target.ship()) {
                Ok(())
            } else {
                Err(Denied::new(format!(
                    "cross-ship isolation: cortana may not reach into ship '{}' crew directly (no skip-level)",
                    target.ship().unwrap_or("?")
                )))
            }
        }
        AclRole::Captain | AclRole::Crew | AclRole::Unknown => {
            if caller.same_ship(target.ship()) {
                Ok(())
            } else {
                Err(Denied::new(format!(
                    "cross-ship isolation: {} on ship '{}' may not access a session on ship '{}'",
                    caller.role.label(),
                    caller.ship.as_deref().unwrap_or("<none>"),
                    target.ship().unwrap_or("?"),
                )))
            }
        }
        AclRole::General => unreachable!("handled above"),
    }
}

// ---------------------------------------------------------------------------
// Abort / interrupt a subordinate (the missing primitive, §2.7 R-H3)
// ---------------------------------------------------------------------------

/// May `caller` ABORT (interrupt) the running session `target`? Abort is a preempt
/// control signal, not a queued message, but it obeys the SAME per-session
/// authorization, and cross-ship/sibling abort is DENIED (§2.7):
/// - GENERAL may abort anyone.
/// - CORTANA may abort a CAPTAIN (a supervisor session, any ship) - her subordinate;
///   she may NOT reach a ship's crew directly (no skip-level).
/// - A CAPTAIN may abort its OWN crew (same-ship crew tile) - never a sibling captain,
///   never another ship's crew.
/// - CREW / UNKNOWN have no subordinates: DENIED (the never-seized guard).
pub fn can_abort(caller: &AclActor, target: &ShipRef) -> AclResult {
    match caller.role {
        AclRole::General => Ok(()),
        AclRole::Cortana => {
            if target.is_supervisor() {
                Ok(())
            } else {
                Err(Denied::new(
                    "abort denied: cortana aborts a captain, not a ship's crew directly (no skip-level)",
                ))
            }
        }
        AclRole::Captain => match target {
            ShipRef::Crew { ship } if caller.same_ship(Some(ship)) => Ok(()),
            ShipRef::Crew { ship } => Err(Denied::new(format!(
                "abort denied: a captain aborts its OWN crew only, not ship '{ship}' crew (cross-ship)"
            ))),
            ShipRef::Supervisor { .. } => Err(Denied::new(
                "abort denied: a captain may not abort a sibling supervisor (no sibling seize)",
            )),
            ShipRef::Unowned => Err(Denied::new(
                "abort denied: target belongs to no ship the captain owns",
            )),
        },
        AclRole::Crew | AclRole::Unknown => Err(Denied::new(format!(
            "abort denied: {} has no subordinate to abort (crew escalate through their captain)",
            caller.role.label()
        ))),
    }
}

// ---------------------------------------------------------------------------
// EMERGENCY-flag authority (§2.7 M4 - RATIFIED GENERAL + CAPTAINS ONLY)
// ---------------------------------------------------------------------------

/// May `caller` raise the EMERGENCY priority on a plane message? RATIFIED (general,
/// 2026-07-10): EMERGENCY-flag authority = GENERAL + CAPTAINS ONLY. Crew are EXCLUDED
/// (they escalate through their captain; there is no crew soft-flag). Cortana is NOT in
/// the ratified flag set either - the parameter reads "GENERAL + CAPTAINS ONLY" - so she
/// sends at Standard priority like anyone outside the set. Setting the priority is a
/// CAPABILITY app-stamped from the resolved role, never a free field the sender fills.
pub fn can_flag_emergency(caller: &AclActor) -> AclResult {
    match caller.role {
        AclRole::General | AclRole::Captain => Ok(()),
        _ => Err(Denied::new(format!(
            "emergency-flag authority is GENERAL + CAPTAINS ONLY; {} may not raise EMERGENCY",
            caller.role.label()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Message-send ACL: the settled matrix DOWN/UP rows + the no-daisy-chain cell
// ---------------------------------------------------------------------------

/// May `caller` SEND a plane message to `target`? Encodes the settled matrix message
/// rows AND the no-daisy-chain sibling denial as an explicit cell (§2.6, corrected to
/// clause 1: Cortana owns captains only, a crew's only up-target is its captain):
///
/// DOWN:  General -> anyone;  Cortana -> Captains;  Captain -> own Crew.
/// UP:    Captain -> Cortana (status) + General (own-ship decisions);  Crew -> its Captain.
/// DENY:  sibling Captain -> Captain, sibling Crew -> Crew, any skip-level, any cross-ship.
///
/// The crew -> General "declared-up-front narrow lane" (matrix row 25) is a NORM the
/// substrate cannot verify mechanically ("declared up front"); the mechanical default
/// is DENY, with that narrow lane handled out-of-band (documented, not silently opened).
pub fn can_message(caller: &AclActor, target: &MessageTarget) -> AclResult {
    let deny = |why: &str| Err(Denied::new(format!("message denied: {why}")));
    match caller.role {
        AclRole::General => Ok(()), // anyone, instant (fast lane)
        AclRole::Cortana => match target.role {
            AclRole::Captain => Ok(()),       // down to a captain
            AclRole::General => Ok(()),        // up to the general (filtered)
            _ => deny("cortana messages captains (down) or the general (up) - never crew directly (no skip-level)"),
        },
        AclRole::Captain => match target.role {
            AclRole::Cortana => Ok(()), // up: status
            AclRole::General => Ok(()),  // up: own-ship decisions
            AclRole::Crew => {
                if caller.same_ship(target.ship.as_deref()) {
                    Ok(()) // down to OWN crew
                } else {
                    deny("a captain messages its OWN crew only (cross-ship isolation)")
                }
            }
            AclRole::Captain => deny("a captain may not message a sibling captain directly (no daisy-chain)"),
            AclRole::Unknown => deny("a captain may not message an unroled session"),
        },
        AclRole::Crew => match target.role {
            AclRole::Captain if caller.same_ship(target.ship.as_deref()) => Ok(()), // up to its captain
            AclRole::Captain => deny("crew message up to their OWN captain only (cross-ship)"),
            AclRole::General => deny(
                "crew -> general is a declared-up-front narrow lane (NORM), not a mechanically-open cell",
            ),
            AclRole::Crew => deny("crew may not message a sibling crew (no daisy-chain)"),
            AclRole::Cortana => deny("crew may not skip-level to cortana (escalate through their captain)"),
            AclRole::Unknown => deny("crew may not message an unroled session"),
        },
        AclRole::Unknown => deny("an unroled session has no send capability"),
    }
}

// ---------------------------------------------------------------------------
// inbox-ack self-scope (§2.4.1 - retire the interim "ack stays Organization")
// ---------------------------------------------------------------------------

/// May `caller` ACK the inbox keyed on `recipient_tile`? Self-scope: a session may ack
/// ONLY its OWN inbox (its tile == the recipient key). This retires the interim price
/// (PR-56/#59: "ack stays Organization; a control-capable relay carries it") now that
/// the per-session token substrate lands the caller's identity on the wire: a crew
/// self-acks with its own token, cross-session acks are refused. A control-capable
/// host/relay ack-on-behalf is granted by the WIRING, not this self-scope predicate.
pub fn can_ack(caller: &AclActor, recipient_tile: &str) -> AclResult {
    if caller.tile.as_deref() == Some(recipient_tile) {
        Ok(())
    } else {
        Err(Denied::new(format!(
            "inbox-ack self-scope: {} (tile {:?}) may only ack its OWN inbox, not '{}'",
            caller.role.label(),
            caller.tile.as_deref().unwrap_or("<unbound>"),
            recipient_tile,
        )))
    }
}

// ---------------------------------------------------------------------------
// operate-fleet-infra (§2.7 R-L2 - the named capability + a safe default owner)
// ---------------------------------------------------------------------------

/// May `caller` operate SHARED fleet infra (the plane's own administrative ops:
/// drain-flush, queue purge, re-parent)? The design's deliverable is NAMING the
/// capability and providing the gated surface; WHO holds it is a matrix policy call
/// (item 4). Safe default: the apex only (GENERAL or CORTANA) - never a captain/crew by
/// omission (the M1 shadow-SRE gap). A future fleet-infra captain is granted by widening
/// this one predicate.
pub fn can_operate_fleet_infra(caller: &AclActor) -> AclResult {
    match caller.role {
        AclRole::General | AclRole::Cortana => Ok(()),
        _ => Err(Denied::new(format!(
            "operate-fleet-infra is apex-owned (general/cortana); {} may not administer the plane",
            caller.role.label()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Delegation-gate: who may ORIGINATE a general-authorization (M1, STATE 2)
// ---------------------------------------------------------------------------

/// May `caller` ORIGINATE a general-authorization artifact (the delegation-gate
/// carrier, §2.6 M1)? Only the GENERAL originates. Cortana may RELAY the general's
/// authorization by REFERENCE (pointing a captain at the general's durable artifact id)
/// but may never ORIGINATE one (that would be "cortana (origin)", which the matrix
/// forbids for authorization). This is the STATE-2 rule (carrier built, relay policy is
/// a separate one-bit flip resolved in `authz.rs`).
pub fn can_originate_authorization(caller: &AclActor) -> AclResult {
    match caller.role {
        AclRole::General => Ok(()),
        _ => Err(Denied::new(format!(
            "only the general may ORIGINATE an authorization; {} may at most relay one by reference",
            caller.role.label()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(role: AclRole, ship: Option<&str>, tile: Option<&str>) -> AclActor {
        AclActor {
            role,
            ship: ship.map(String::from),
            tile: tile.map(String::from),
            session_id: format!("sid-{}", role.label()),
        }
    }

    // ---- cross-ship isolation -------------------------------------------------

    #[test]
    fn cross_ship_read_write_is_isolated_for_identified_sessions() {
        // The mandated cross-ship-isolation guard. A crew on ship A may read/write only
        // its own ship's sessions; reaching ship B is REFUSED. BYPASS-WOULD-FAIL: drop
        // the `same_ship` check in `can_access_session` and the cross-ship assert flips
        // to Ok and this test goes RED.
        let crew_a = actor(AclRole::Crew, Some("ship-a"), Some("crew-a"));
        // Same ship: allowed.
        assert!(can_access_session(&crew_a, &ShipRef::Crew { ship: "ship-a".into() }).is_ok());
        assert!(can_access_session(&crew_a, &ShipRef::Supervisor { ship: "ship-a".into() }).is_ok());
        // Cross ship: REFUSED, attributed.
        let denied = can_access_session(&crew_a, &ShipRef::Crew { ship: "ship-b".into() }).unwrap_err();
        assert!(denied.reason.contains("cross-ship isolation"), "got: {}", denied.reason);
        // A captain is isolated the same way.
        let cap_a = actor(AclRole::Captain, Some("ship-a"), Some("cap-a"));
        assert!(can_access_session(&cap_a, &ShipRef::Crew { ship: "ship-b".into() }).is_err());
        assert!(can_access_session(&cap_a, &ShipRef::Crew { ship: "ship-a".into() }).is_ok());
    }

    #[test]
    fn general_reads_anyone_cortana_reaches_captains_not_foreign_crew() {
        let general = actor(AclRole::General, None, None);
        assert!(can_access_session(&general, &ShipRef::Crew { ship: "ship-x".into() }).is_ok());
        let cortana = actor(AclRole::Cortana, Some("cortana"), Some("cor"));
        // A captain (supervisor) on any ship: her subordinate, allowed.
        assert!(can_access_session(&cortana, &ShipRef::Supervisor { ship: "ship-x".into() }).is_ok());
        // Another ship's CREW directly: no skip-level, REFUSED.
        assert!(can_access_session(&cortana, &ShipRef::Crew { ship: "ship-x".into() }).is_err());
    }

    #[test]
    fn unowned_target_is_not_isolated() {
        // A tile that belongs to no ship is not "another ship's secret" - isolation
        // does not fire (fail-open on the non-secret).
        let crew = actor(AclRole::Crew, Some("ship-a"), Some("c"));
        assert!(can_access_session(&crew, &ShipRef::Unowned).is_ok());
    }

    // ---- abort / never-seized ------------------------------------------------

    #[test]
    fn crew_can_never_abort_and_captain_aborts_only_own_crew() {
        // The mandated never-seized guard. Crew has no subordinate: abort is REFUSED.
        // BYPASS-WOULD-FAIL: make `can_abort` return Ok for crew and this goes RED.
        let crew = actor(AclRole::Crew, Some("ship-a"), Some("crew-a"));
        assert!(can_abort(&crew, &ShipRef::Crew { ship: "ship-a".into() }).is_err());
        assert!(can_abort(&crew, &ShipRef::Supervisor { ship: "ship-a".into() }).is_err());

        let cap_a = actor(AclRole::Captain, Some("ship-a"), Some("cap-a"));
        // Own crew: allowed.
        assert!(can_abort(&cap_a, &ShipRef::Crew { ship: "ship-a".into() }).is_ok());
        // Sibling captain: never (no sibling seize).
        assert!(can_abort(&cap_a, &ShipRef::Supervisor { ship: "ship-b".into() }).is_err());
        // Another ship's crew: cross-ship, refused.
        assert!(can_abort(&cap_a, &ShipRef::Crew { ship: "ship-b".into() }).is_err());
    }

    #[test]
    fn cortana_aborts_a_captain_general_aborts_anyone() {
        let cortana = actor(AclRole::Cortana, Some("cortana"), Some("cor"));
        assert!(can_abort(&cortana, &ShipRef::Supervisor { ship: "ship-x".into() }).is_ok());
        // Not a ship's crew directly (no skip-level).
        assert!(can_abort(&cortana, &ShipRef::Crew { ship: "ship-x".into() }).is_err());
        let general = actor(AclRole::General, None, None);
        assert!(can_abort(&general, &ShipRef::Crew { ship: "ship-x".into() }).is_ok());
        assert!(can_abort(&general, &ShipRef::Supervisor { ship: "ship-x".into() }).is_ok());
    }

    // ---- emergency authority / never-crew-emergency --------------------------

    #[test]
    fn emergency_flag_is_general_and_captains_only_never_crew() {
        // The mandated never-crew-emergency guard. BYPASS-WOULD-FAIL: widen
        // `can_flag_emergency` to admit crew and this goes RED.
        assert!(can_flag_emergency(&actor(AclRole::General, None, None)).is_ok());
        assert!(can_flag_emergency(&actor(AclRole::Captain, Some("s"), Some("c"))).is_ok());
        // Crew EXCLUDED.
        assert!(can_flag_emergency(&actor(AclRole::Crew, Some("s"), Some("c"))).is_err());
        // Cortana is NOT in the ratified GENERAL + CAPTAINS ONLY set.
        assert!(can_flag_emergency(&actor(AclRole::Cortana, Some("cortana"), Some("cor"))).is_err());
        assert!(can_flag_emergency(&actor(AclRole::Unknown, None, None)).is_err());
    }

    // ---- message rows + no-daisy-chain ---------------------------------------

    fn mt(role: AclRole, ship: Option<&str>) -> MessageTarget {
        MessageTarget { role, ship: ship.map(String::from) }
    }

    #[test]
    fn message_down_rows() {
        // General -> anyone.
        let g = actor(AclRole::General, None, None);
        assert!(can_message(&g, &mt(AclRole::Crew, Some("ship-x"))).is_ok());
        // Cortana -> captains only (down); never crew directly.
        let cor = actor(AclRole::Cortana, Some("cortana"), Some("cor"));
        assert!(can_message(&cor, &mt(AclRole::Captain, Some("ship-x"))).is_ok());
        assert!(can_message(&cor, &mt(AclRole::Crew, Some("ship-x"))).is_err());
        // Captain -> own crew (down); not another ship's crew.
        let cap = actor(AclRole::Captain, Some("ship-a"), Some("cap-a"));
        assert!(can_message(&cap, &mt(AclRole::Crew, Some("ship-a"))).is_ok());
        assert!(can_message(&cap, &mt(AclRole::Crew, Some("ship-b"))).is_err());
    }

    #[test]
    fn message_up_rows() {
        // Captain -> Cortana (status) + General (own-ship decisions).
        let cap = actor(AclRole::Captain, Some("ship-a"), Some("cap-a"));
        assert!(can_message(&cap, &mt(AclRole::Cortana, Some("cortana"))).is_ok());
        assert!(can_message(&cap, &mt(AclRole::General, None)).is_ok());
        // Crew -> its own captain.
        let crew = actor(AclRole::Crew, Some("ship-a"), Some("crew-a"));
        assert!(can_message(&crew, &mt(AclRole::Captain, Some("ship-a"))).is_ok());
        // Crew -> another ship's captain: cross-ship, refused.
        assert!(can_message(&crew, &mt(AclRole::Captain, Some("ship-b"))).is_err());
    }

    #[test]
    fn no_daisy_chain_sibling_and_skip_level_denied() {
        // Sibling captain -> captain: DENIED. BYPASS-WOULD-FAIL: return Ok for the
        // captain->captain arm and this goes RED.
        let cap_a = actor(AclRole::Captain, Some("ship-a"), Some("cap-a"));
        let d = can_message(&cap_a, &mt(AclRole::Captain, Some("ship-b"))).unwrap_err();
        assert!(d.reason.contains("daisy-chain"), "got: {}", d.reason);
        // Sibling crew -> crew: DENIED.
        let crew_a = actor(AclRole::Crew, Some("ship-a"), Some("crew-a"));
        assert!(can_message(&crew_a, &mt(AclRole::Crew, Some("ship-a"))).is_err());
        // Crew -> Cortana skip-level: DENIED.
        assert!(can_message(&crew_a, &mt(AclRole::Cortana, Some("cortana"))).is_err());
        // Crew -> General (declared narrow lane) is NOT mechanically open: DENIED here.
        assert!(can_message(&crew_a, &mt(AclRole::General, None)).is_err());
    }

    // ---- inbox-ack self-scope -------------------------------------------------

    #[test]
    fn ack_is_self_scoped() {
        // A crew acks ONLY its own inbox (its tile). BYPASS-WOULD-FAIL: drop the
        // tile-equality check and a cross-session ack passes, going RED.
        let crew = actor(AclRole::Crew, Some("ship-a"), Some("crew-a"));
        assert!(can_ack(&crew, "crew-a").is_ok());
        let d = can_ack(&crew, "someone-else").unwrap_err();
        assert!(d.reason.contains("self-scope"), "got: {}", d.reason);
        // An unbound session cannot ack anyone.
        let unbound = actor(AclRole::Crew, Some("ship-a"), None);
        assert!(can_ack(&unbound, "crew-a").is_err());
    }

    // ---- fleet-infra + authorization originate --------------------------------

    #[test]
    fn fleet_infra_is_apex_only() {
        assert!(can_operate_fleet_infra(&actor(AclRole::General, None, None)).is_ok());
        assert!(can_operate_fleet_infra(&actor(AclRole::Cortana, Some("cortana"), Some("c"))).is_ok());
        assert!(can_operate_fleet_infra(&actor(AclRole::Captain, Some("s"), Some("c"))).is_err());
        assert!(can_operate_fleet_infra(&actor(AclRole::Crew, Some("s"), Some("c"))).is_err());
    }

    #[test]
    fn only_general_originates_authorization() {
        assert!(can_originate_authorization(&actor(AclRole::General, None, None)).is_ok());
        // Cortana may relay-by-reference but never originate.
        let d = can_originate_authorization(&actor(AclRole::Cortana, Some("cortana"), Some("c"))).unwrap_err();
        assert!(d.reason.contains("relay"), "got: {}", d.reason);
        assert!(can_originate_authorization(&actor(AclRole::Captain, Some("s"), Some("c"))).is_err());
        assert!(can_originate_authorization(&actor(AclRole::Crew, Some("s"), Some("c"))).is_err());
    }
}
