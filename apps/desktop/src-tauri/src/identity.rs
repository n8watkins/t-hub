//! Comms plane - PHASE 2: the per-session identity slice (mint / bind / resolve).
//!
//! The design (§2.3, D9) pulls the MINIMUM identity slice forward from program items
//! 2/3 so the plane's attribution is per-session "from day one", not merely the
//! coarse capability tier every Full-token session shares. This module is exactly
//! that slice and nothing more:
//!
//! - MINT: a distinct per-session secret at spawn, bound to the session.
//! - BIND: record a session -> identity binding in an app store (`identities.json`),
//!   written with the same temp + atomic-rename + 0600 discipline as the registry.
//! - RESOLVE: map a presented per-session token back to its identity (+ role) so an
//!   enqueue can be stamped with WHICH session originated it.
//!
//! HONEST LIMITS (design §2.3, verbatim intent):
//! - This makes identity unforgeable ACROSS sessions (session A cannot stamp as
//!   session B - it never learns B's secret) and thus attributable. It does NOT
//!   defend against a session leaking its OWN token (the env-injected token is
//!   readable within that session's own process tree - the H3/H4 class). Item 3's
//!   credential hardening is what closes that; this slice only CONSUMES provenance.
//! - `role` here is best-effort, captured at mint from spawn context. Durable
//!   role-PINNING / role-UNIQUENESS need item 2's ship/role re-key (role-pinning
//!   deadlocks on migration today, R-H2). This module keys identities by their own
//!   minted id and records the session's tile id as a MUTABLE pointer; item 2 re-keys
//!   to a durable ship/role slug. The seam is flagged, not solved here.
//! - This module does NOT authorize anything (that is Phase 3's ACL). It answers
//!   "which session, in what role" - never "may it do X".

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The env var carrying the per-session token into a spawned session, ALONGSIDE the
/// existing tier token (`T_HUB_CONTROL_TOKEN`). The in-session client presents it so
/// the app can resolve the calling session's identity.
pub const SESSION_TOKEN_ENV: &str = "T_HUB_SESSION_TOKEN";

/// Best-effort org role captured at mint. Durable role-keying is item 2 (see the
/// module note); this is enough for the plane to record "in what role" alongside the
/// per-session id for attribution, and for Phase 3 to build its ACL on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The general (human apex). Not spawned through this path today; present for
    /// completeness so the enum matches the org model.
    General,
    /// The apex orchestrator.
    Cortana,
    /// A ship's captain.
    Captain,
    /// A worker under a captain (the default for a `spawn_terminal`-spawned session).
    Crew,
    /// Role not yet determined at mint (item-2 re-key will resolve it durably).
    Unknown,
}

impl Role {
    /// Stable label for logs / telemetry / the attribution stamp.
    pub fn label(self) -> &'static str {
        match self {
            Role::General => "general",
            Role::Cortana => "cortana",
            Role::Captain => "captain",
            Role::Crew => "crew",
            Role::Unknown => "unknown",
        }
    }
}

/// One minted per-session identity. `secret` is the bearer token injected into the
/// session's env; `id` is the stable, non-secret handle used to STAMP attribution
/// (an enqueue records the id/role, never the secret).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    /// Stable, non-secret identity handle (the attribution stamp).
    pub id: String,
    /// The per-session bearer secret (env-injected; unforgeable across sessions).
    pub secret: String,
    /// Best-effort role at mint (see the module note re: item-2 durable re-key).
    pub role: Role,
    /// The tile id this identity was bound to, once the session exists. A MUTABLE
    /// pointer (id-namespace L1 / item-2 re-key hazard) - the durable key is the
    /// minted `id`, not the tile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_tile: Option<String>,
    /// Epoch-ms minted.
    pub minted_at: u64,
}

/// A non-secret view of an identity - what an attribution stamp / observability may
/// safely expose. NEVER carries the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityStamp {
    pub id: String,
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_tile: Option<String>,
}

impl SessionIdentity {
    /// The attribution stamp (id + role + tile), never the secret.
    pub fn stamp(&self) -> IdentityStamp {
        IdentityStamp {
            id: self.id.clone(),
            role: self.role,
            session_tile: self.session_tile.clone(),
        }
    }

    /// A compact `role:id` sender label for the inbox `sender` field.
    pub fn sender_label(&self) -> String {
        format!("{}:{}", self.role.label(), self.id)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Constant-time secret comparison, mirroring `control::ct_token_eq`, so resolving a
/// presented token is not a timing oracle.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// On-disk shape of the identity store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentitiesSnapshot {
    /// Keyed by the minted identity id.
    identities: HashMap<String, SessionIdentity>,
}

/// The per-session identity store: an in-memory map behind a mutex, persisted
/// atomically to `identities.json`. Shared (`Arc`) between the spawn path (mint/bind)
/// and the enqueue/ack path (resolve).
pub struct IdentityStore {
    path: Option<PathBuf>,
    inner: Mutex<IdentitiesSnapshot>,
}

impl IdentityStore {
    /// Load (or start empty) from `path`. A missing/corrupt file starts empty - never
    /// a startup failure, matching `CaptainsRegistry::load`.
    pub fn load(path: PathBuf) -> Self {
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<IdentitiesSnapshot>(&body).ok())
            .unwrap_or_default();
        IdentityStore {
            path: Some(path),
            inner: Mutex::new(inner),
        }
    }

    /// Load from the default `~/.t-hub/identities.json` (override
    /// `T_HUB_IDENTITIES_FILE`), mirroring `captains_path`.
    pub fn load_default() -> Self {
        Self::load(default_identities_path())
    }

    /// An in-memory-only store (tests / a headless run with no addr).
    pub fn ephemeral() -> Self {
        IdentityStore {
            path: None,
            inner: Mutex::new(IdentitiesSnapshot::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, IdentitiesSnapshot> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, snap: &IdentitiesSnapshot) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomic(path, snap) {
            eprintln!("t-hub-identity: persist failed: {e}");
        }
    }

    /// Mint a fresh per-session identity: a distinct id + secret, recorded in the
    /// store. The secret is what gets env-injected; the returned identity carries it
    /// so the caller can inject it, but the STAMP (id/role) is what attribution uses.
    pub fn mint(&self, role: Role) -> SessionIdentity {
        let identity = SessionIdentity {
            id: uuid::Uuid::new_v4().simple().to_string(),
            // Two v4 uuids => ~244 bits of entropy; a bearer secret, not a display id.
            secret: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            role,
            session_tile: None,
            minted_at: now_ms(),
        };
        let mut snap = self.lock();
        snap.identities.insert(identity.id.clone(), identity.clone());
        self.persist(&snap);
        identity
    }

    /// Bind a minted identity to the tile id its session landed on (after spawn). The
    /// tile is a mutable pointer; the durable key stays the minted id.
    pub fn bind_tile(&self, id: &str, tile: &str) {
        let mut snap = self.lock();
        if let Some(ident) = snap.identities.get_mut(id) {
            ident.session_tile = Some(tile.to_string());
        }
        self.persist(&snap);
    }

    /// Retire (forget) an identity by its minted id, dropping its persisted secret.
    /// Used to clean up an orphan from a FAILED spawn (the mint persisted before the
    /// spawn's `?` returned - review L2) and, via [`retire_tile`](Self::retire_tile),
    /// when a session closes (review M3 - bound the secret-bearing store to live +
    /// not-yet-closed sessions rather than letting it grow forever). A no-op for an
    /// unknown id. After this, the retired secret no longer `resolve`s.
    pub fn retire(&self, id: &str) -> bool {
        let mut snap = self.lock();
        let removed = snap.identities.remove(id).is_some();
        if removed {
            self.persist(&snap);
        }
        removed
    }

    /// Retire the identity bound to a tile id (the session-close GC hook, M3). Removes
    /// the identity whose `session_tile` matches, so a dead session's secret stops
    /// resolving and the store does not accrete dead sessions. A no-op if no identity
    /// is bound to that tile.
    pub fn retire_tile(&self, tile: &str) -> bool {
        let id = {
            let snap = self.lock();
            snap.identities
                .values()
                .find(|i| i.session_tile.as_deref() == Some(tile))
                .map(|i| i.id.clone())
        };
        match id {
            Some(id) => self.retire(&id),
            None => false,
        }
    }

    /// Resolve a presented per-session token to its identity, constant-time. `None`
    /// for an empty or unknown token. This is IDENTIFICATION, never authorization.
    pub fn resolve(&self, presented: &str) -> Option<SessionIdentity> {
        if presented.is_empty() {
            return None;
        }
        let snap = self.lock();
        snap.identities
            .values()
            .find(|ident| ct_eq(&ident.secret, presented))
            .cloned()
    }

    /// Look up an identity by its (non-secret) minted id.
    pub fn get(&self, id: &str) -> Option<SessionIdentity> {
        self.lock().identities.get(id).cloned()
    }

    /// The identity currently bound to a tile id, if any (for attribution of an
    /// app-side action keyed by tile).
    pub fn for_tile(&self, tile: &str) -> Option<SessionIdentity> {
        self.lock()
            .identities
            .values()
            .find(|i| i.session_tile.as_deref() == Some(tile))
            .cloned()
    }

    /// Count of minted identities (observability / tests).
    pub fn len(&self) -> usize {
        self.lock().identities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn default_identities_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_IDENTITIES_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("identities.json")
}

/// Atomic write (temp + 0600 + rename), the registry discipline. The store holds
/// secrets, so 0600 matters - it is the same sensitivity class as the server-key file.
fn write_atomic(path: &PathBuf, snap: &IdentitiesSnapshot) -> std::io::Result<()> {
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
            "t-hub-identity-test-{}-{}.json",
            std::process::id(),
            C.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn mint_produces_distinct_ids_and_secrets() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew);
        let b = store.mint(Role::Crew);
        assert_ne!(a.id, b.id, "ids are distinct");
        assert_ne!(a.secret, b.secret, "secrets are distinct");
        assert!(!a.secret.is_empty());
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn resolve_returns_the_minting_identity_only_for_its_own_secret() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Captain);
        let b = store.mint(Role::Crew);
        // A's secret resolves to A, never B (unforgeable across sessions).
        assert_eq!(store.resolve(&a.secret).unwrap().id, a.id);
        assert_eq!(store.resolve(&b.secret).unwrap().id, b.id);
        assert_eq!(store.resolve(&a.secret).unwrap().role, Role::Captain);
        // An unknown or empty token resolves to nothing.
        assert!(store.resolve("not-a-real-secret").is_none());
        assert!(store.resolve("").is_none());
    }

    #[test]
    fn a_session_cannot_forge_anothers_stamp() {
        // The core cross-session unforgeability property: knowing only your OWN secret,
        // you can never resolve as another identity. There is no secret that maps to B
        // except B's own, which A never learns.
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew);
        let b = store.mint(Role::Crew);
        // Present A's secret => you are A, full stop; you cannot become B.
        let resolved = store.resolve(&a.secret).unwrap();
        assert_ne!(resolved.id, b.id);
    }

    #[test]
    fn bind_tile_records_the_mutable_pointer() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew);
        assert!(a.session_tile.is_none());
        store.bind_tile(&a.id, "abc12345");
        assert_eq!(store.get(&a.id).unwrap().session_tile.as_deref(), Some("abc12345"));
        assert_eq!(store.for_tile("abc12345").unwrap().id, a.id);
    }

    #[test]
    fn persistence_round_trips_including_the_binding() {
        let path = temp_path();
        let _ = std::fs::remove_file(&path);
        let id;
        let secret;
        {
            let store = IdentityStore::load(path.clone());
            let a = store.mint(Role::Captain);
            store.bind_tile(&a.id, "tile-1");
            id = a.id.clone();
            secret = a.secret.clone();
        }
        // Reopen from disk: the identity + binding survive, and the secret still
        // resolves.
        let store2 = IdentityStore::load(path.clone());
        assert_eq!(store2.len(), 1);
        let resolved = store2.resolve(&secret).unwrap();
        assert_eq!(resolved.id, id);
        assert_eq!(resolved.role, Role::Captain);
        assert_eq!(resolved.session_tile.as_deref(), Some("tile-1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sender_label_is_role_scoped_and_carries_no_secret() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew);
        let label = a.sender_label();
        assert!(label.starts_with("crew:"));
        assert!(!label.contains(&a.secret), "the sender label never leaks the secret");
        let stamp = a.stamp();
        assert_eq!(stamp.id, a.id);
        // The stamp serializes without the secret field.
        let json = serde_json::to_string(&stamp).unwrap();
        assert!(!json.contains(&a.secret));
    }

    #[test]
    fn missing_file_starts_empty() {
        let store = IdentityStore::load(temp_path());
        assert!(store.is_empty());
    }

    #[test]
    fn retire_drops_the_identity_and_stops_resolving() {
        // Review L2/M3: a retired identity is forgotten - its secret no longer
        // resolves and the store shrinks (no unbounded secret-bearing growth).
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew);
        let b = store.mint(Role::Crew);
        assert!(store.resolve(&a.secret).is_some());
        assert!(store.retire(&a.id), "retire reports it removed something");
        assert!(store.resolve(&a.secret).is_none(), "retired secret no longer resolves");
        assert_eq!(store.len(), 1, "only the retired identity is gone");
        assert!(store.resolve(&b.secret).is_some(), "the other identity is untouched");
        // Retiring an unknown id is a no-op.
        assert!(!store.retire("no-such-id"));
    }

    #[test]
    fn retire_tile_removes_the_identity_bound_to_a_closed_session() {
        // Review M3: the session-close GC hook retires by tile binding.
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew);
        store.bind_tile(&a.id, "deadtile");
        assert!(store.retire_tile("deadtile"));
        assert!(store.resolve(&a.secret).is_none());
        assert!(store.is_empty());
        // No binding for that tile => no-op.
        assert!(!store.retire_tile("never-bound"));
    }
}
