//! Comms plane - PHASE 3: the delegation-gate CARRIER (§2.6 M1).
//!
//! The settled matrix's money/public GATE is satisfied ONLY by a provenance-verified
//! GENERAL-AUTHORIZATION present in the plane. This module is that carrier: a durable,
//! app-stamped, REFERENCEABLE authorization artifact plus the resolve-and-verify gate a
//! captain's gate consults. It is built to the STATE-2 shape (design §2.6, N1):
//!
//! - STATE 1 (pre-plane): no artifact, no carrier - human verify. (Superseded here.)
//! - STATE 2 (plane-built, relay DISABLED - the default this ships): the carrier + the
//!   durable app-stamped artifact + the resolve-and-verify gate EXIST. The gate accepts
//!   a GENERAL (direct) authorization by reference; a general->Cortana RELAY reference
//!   is REFUSED (`accept_relayed_authorization = false`).
//! - STATE 3 (relay ENABLED): flip the single bit (`T_HUB_ACCEPT_RELAYED_AUTHORIZATION`).
//!   Nothing else changes - not the artifact, not the gate, not the ACL cell.
//!
//! A "relay" is a REFERENCE, never Cortana authoring an assertion: the general's
//! authorization is the first-class durable artifact with an id; Cortana relaying points
//! a captain at THAT id. The gate resolves the referenced artifact and checks ITS
//! app-stamped origin == general (unforgeable across sessions via the per-session
//! identity, `identity.rs`). WHO may ORIGINATE is enforced UPSTREAM by
//! `acl::can_originate_authorization` (general only); this module records what it is
//! handed and verifies provenance on resolve - it never authorizes the write itself.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The app-stamped origin role recorded on an artifact. Only `"general"` satisfies the
/// gate; the field is stamped by `control.rs` from the resolved per-session identity,
/// never from a sender-supplied value.
pub const ORIGIN_GENERAL: &str = "general";

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One durable general-authorization artifact. `origin_role`/`origin_session` are the
/// app-stamped provenance (the attributed chain's root); `relayed_by`, when present, is
/// the Cortana session that relayed the reference (STATE 3 only). The artifact is the
/// thing a captain's gate REFERENCES by `id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Authorization {
    /// The stable reference id a captain's gate consults.
    pub id: String,
    /// App-stamped origin role (must be [`ORIGIN_GENERAL`] to satisfy the gate).
    pub origin_role: String,
    /// The minted per-session id that ORIGINATED it (the unforgeable attribution root).
    pub origin_session: String,
    /// The authorized action/scope (free-form: e.g. `spend`, `publish`, a description).
    pub action: String,
    /// The ship the authorization is scoped to, when scoped (`None` = fleet-wide).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ship: Option<String>,
    /// The Cortana session that relayed this reference down, when relayed (STATE 3).
    /// `None` for a general-direct authorization. A relayed reference is REFUSED by the
    /// gate under STATE 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
    /// Epoch-ms created.
    pub created_at: u64,
    /// Whether the authorization has been revoked (a durable tombstone; a revoked
    /// artifact never satisfies the gate again).
    #[serde(default)]
    pub revoked: bool,
}

/// The outcome of a resolve-and-verify gate consult (`general_authorization_present`).
/// Distinguishes the reasons so a captain's gate can log WHY it refused, not just that
/// it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// A provenance-verified general authorization is present: the gate is satisfied.
    Present,
    /// No artifact with that id (never authorized, or compacted away).
    Absent,
    /// The artifact exists but its origin is not the general (would be a forgery / a
    /// Cortana-originated assertion the matrix forbids).
    NotGeneral,
    /// The artifact was revoked (durable tombstone).
    Revoked,
    /// The artifact is a RELAYED reference and relay is disabled (STATE 2). Flip
    /// `T_HUB_ACCEPT_RELAYED_AUTHORIZATION` to accept it (STATE 3).
    RelayDisabled,
}

impl GateVerdict {
    pub fn is_present(&self) -> bool {
        matches!(self, GateVerdict::Present)
    }
    pub fn label(&self) -> &'static str {
        match self {
            GateVerdict::Present => "present",
            GateVerdict::Absent => "absent",
            GateVerdict::NotGeneral => "not-general",
            GateVerdict::Revoked => "revoked",
            GateVerdict::RelayDisabled => "relay-disabled",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthzSnapshot {
    /// Keyed by artifact id.
    authorizations: HashMap<String, Authorization>,
}

/// The durable general-authorization store: an in-memory map behind a mutex, persisted
/// atomically to `authorizations.json` with the registry's temp+rename+0600 discipline.
/// Shared (`Arc`) between the record path (`authorize`) and the gate-consult path
/// (`check_authorization` / a captain's money/publish gate).
pub struct AuthzStore {
    path: Option<PathBuf>,
    inner: Mutex<AuthzSnapshot>,
}

impl AuthzStore {
    /// Load (or start empty) from `path`. A missing/corrupt file starts empty - never a
    /// startup failure, matching the other plane stores.
    pub fn load(path: PathBuf) -> Self {
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<AuthzSnapshot>(&body).ok())
            .unwrap_or_default();
        AuthzStore {
            path: Some(path),
            inner: Mutex::new(inner),
        }
    }

    /// Load from the default `~/.t-hub/authorizations.json` (override
    /// `T_HUB_AUTHORIZATIONS_FILE`).
    pub fn load_default() -> Self {
        Self::load(default_authz_path())
    }

    /// An in-memory-only store (tests / a headless run with no addr).
    pub fn ephemeral() -> Self {
        AuthzStore {
            path: None,
            inner: Mutex::new(AuthzSnapshot::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AuthzSnapshot> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, snap: &AuthzSnapshot) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomic(path, snap) {
            eprintln!("t-hub-authz: persist failed: {e}");
        }
    }

    /// Record a durable general-authorization artifact. The caller (`control.rs`) has
    /// ALREADY enforced `acl::can_originate_authorization` (general only) and passes the
    /// APP-STAMPED `origin_role` + `origin_session` from the resolved identity - never a
    /// sender-supplied value. Returns the minted artifact (its `id` is the reference a
    /// captain's gate later consults).
    pub fn record(
        &self,
        origin_role: &str,
        origin_session: &str,
        action: &str,
        target_ship: Option<String>,
        relayed_by: Option<String>,
    ) -> Authorization {
        let auth = Authorization {
            id: uuid::Uuid::new_v4().simple().to_string(),
            origin_role: origin_role.to_string(),
            origin_session: origin_session.to_string(),
            action: action.to_string(),
            target_ship,
            relayed_by,
            created_at: now_ms(),
            revoked: false,
        };
        let mut snap = self.lock();
        snap.authorizations.insert(auth.id.clone(), auth.clone());
        self.persist(&snap);
        auth
    }

    /// Look up an artifact by its reference id.
    pub fn get(&self, id: &str) -> Option<Authorization> {
        self.lock().authorizations.get(id).cloned()
    }

    /// Revoke an artifact (a durable tombstone): the referenced authorization stops
    /// satisfying the gate immediately and forever. Returns true if it changed.
    pub fn revoke(&self, id: &str) -> bool {
        let mut snap = self.lock();
        if let Some(a) = snap.authorizations.get_mut(id) {
            if !a.revoked {
                a.revoked = true;
                self.persist(&snap);
                return true;
            }
        }
        false
    }

    /// The resolve-and-verify GATE (`general_authorization_present`): resolve the
    /// referenced artifact and verify its app-stamped origin == general, it is not
    /// revoked, and - under STATE 2 - it is not a relayed reference. `accept_relayed`
    /// is the single policy bit (from `accept_relayed_authorization`) that distinguishes
    /// STATE 2 (false) from STATE 3 (true).
    pub fn present(&self, id: &str, accept_relayed: bool) -> GateVerdict {
        let snap = self.lock();
        let Some(a) = snap.authorizations.get(id) else {
            return GateVerdict::Absent;
        };
        if a.revoked {
            return GateVerdict::Revoked;
        }
        if a.origin_role != ORIGIN_GENERAL {
            return GateVerdict::NotGeneral;
        }
        if a.relayed_by.is_some() && !accept_relayed {
            return GateVerdict::RelayDisabled;
        }
        GateVerdict::Present
    }

    /// Count of recorded artifacts (observability / tests).
    pub fn len(&self) -> usize {
        self.lock().authorizations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Whether general-authorization RELAY is accepted (the single STATE-2 -> STATE-3 bit).
/// Default FALSE (STATE 2: relay disabled - a general-direct authorization by reference
/// is accepted, a Cortana-relayed reference is refused). `T_HUB_ACCEPT_RELAYED_AUTHORIZATION=1`
/// flips it (STATE 3), safe ONLY because per-session identity makes a relayed reference
/// unforgeable.
pub fn accept_relayed_authorization() -> bool {
    std::env::var("T_HUB_ACCEPT_RELAYED_AUTHORIZATION")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn default_authz_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_AUTHORIZATIONS_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("authorizations.json")
}

/// Atomic write (temp + 0600 + rename), the registry discipline. The artifacts are
/// governance-sensitive (they gate money/publish), so 0600 + atomicity matter.
fn write_atomic(path: &PathBuf, snap: &AuthzSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(snap)?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static C: AtomicU64 = AtomicU64::new(0);

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "t-hub-authz-test-{}-{}.json",
            std::process::id(),
            C.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn a_general_direct_authorization_is_present_under_state_2() {
        // STATE 2: a general-direct (non-relayed) authorization resolves Present with
        // relay disabled. This is the artifact + resolve-and-verify gate the captain's
        // money/publish gate consults.
        let store = AuthzStore::ephemeral();
        let a = store.record(
            ORIGIN_GENERAL,
            "sess-general",
            "spend",
            Some("ship-a".into()),
            None,
        );
        assert_eq!(store.present(&a.id, false), GateVerdict::Present);
        // An unknown reference is Absent (the gate FIRES = escalate).
        assert_eq!(store.present("no-such-id", false), GateVerdict::Absent);
    }

    #[test]
    fn a_relayed_reference_is_refused_under_state_2_accepted_under_state_3() {
        // The one-bit flip. BYPASS-WOULD-FAIL: if `present` ignored `accept_relayed`,
        // the STATE-2 assert (RelayDisabled) would go RED.
        let store = AuthzStore::ephemeral();
        let a = store.record(
            ORIGIN_GENERAL,
            "sess-general",
            "publish",
            None,
            Some("sess-cortana".into()),
        );
        // STATE 2 (relay disabled): refused.
        assert_eq!(store.present(&a.id, false), GateVerdict::RelayDisabled);
        // STATE 3 (relay enabled): the SAME artifact is now Present. Nothing else changed.
        assert_eq!(store.present(&a.id, true), GateVerdict::Present);
    }

    #[test]
    fn a_non_general_origin_never_satisfies_the_gate() {
        // A cortana-ORIGINATED artifact (the matrix forbids this; upstream ACL blocks
        // it, but defense-in-depth: even if one is recorded it does NOT satisfy the gate).
        let store = AuthzStore::ephemeral();
        let a = store.record("cortana", "sess-cortana", "spend", None, None);
        assert_eq!(store.present(&a.id, false), GateVerdict::NotGeneral);
        assert_eq!(store.present(&a.id, true), GateVerdict::NotGeneral);
    }

    #[test]
    fn revoke_tombstones_the_authorization() {
        let store = AuthzStore::ephemeral();
        let a = store.record(ORIGIN_GENERAL, "sess-general", "spend", None, None);
        assert!(store.present(&a.id, false).is_present());
        assert!(store.revoke(&a.id));
        assert_eq!(store.present(&a.id, false), GateVerdict::Revoked);
        // A second revoke is a no-op.
        assert!(!store.revoke(&a.id));
    }

    #[test]
    fn artifacts_persist_across_a_reload() {
        let path = temp_path();
        let id = {
            let store = AuthzStore::load(path.clone());
            let a = store.record(
                ORIGIN_GENERAL,
                "sess-general",
                "spend",
                Some("ship-a".into()),
                None,
            );
            a.id
        };
        let reloaded = AuthzStore::load(path.clone());
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.present(&id, false), GateVerdict::Present);
        let got = reloaded.get(&id).unwrap();
        assert_eq!(got.action, "spend");
        assert_eq!(got.target_ship.as_deref(), Some("ship-a"));
        let _ = std::fs::remove_file(&path);
    }
}
