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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The env var carrying the per-session token into a spawned session, ALONGSIDE the
/// existing tier token (`T_HUB_CONTROL_TOKEN`). The in-session client presents it so
/// the app can resolve the calling session's identity.
pub const SESSION_TOKEN_ENV: &str = "T_HUB_SESSION_TOKEN";
const IDENTITIES_SCHEMA_VERSION: u32 = 1;
const IDENTITY_GENERATION_WATERMARK_SCHEMA_VERSION: u32 = 1;
const IDENTITY_GENERATION_WATERMARK_FORMAT: &str = "t-hub:identity-generation-watermark:v1";

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
///
/// `Debug` is HAND-WRITTEN to REDACT `secret` (LOW-3): item-3 seals this secret at
/// rest, so a stray `{:?}` in a log must not undo that by printing it in cleartext.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentity {
    /// Stable, non-secret identity handle (the attribution stamp).
    pub id: String,
    /// Durable monotonic generation allocated by the IdentityStore.
    /// Legacy identities deserialize as zero and cannot attest a new runtime.
    #[serde(default)]
    pub generation: u64,
    /// The per-session bearer secret (env-injected; unforgeable across sessions).
    pub secret: String,
    /// Best-effort role at mint (see the module note re: item-2 durable re-key).
    pub role: Role,
    /// The tile id this identity was bound to, once the session exists. A MUTABLE
    /// pointer (id-namespace L1 / item-2 re-key hazard) - the durable key is the
    /// minted `id`, not the tile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_tile: Option<String>,
    /// The durable SHIP this session belongs to, resolved at mint from the spawner's
    /// identity (item-2 §2.6 widened binding, D5). A crew inherits its spawner
    /// captain's ship; `None` when the ship is not yet resolvable at mint (the
    /// spawner had no claim yet) - the durable ship key lives in the captains
    /// registry regardless, this is the fast-path attribution copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_slug: Option<String>,
    /// Epoch-ms minted.
    pub minted_at: u64,
}

impl std::fmt::Debug for SessionIdentity {
    /// Redacts `secret` (LOW-3) so no `{:?}` ever leaks the bearer token in cleartext,
    /// undermining the at-rest sealing. All non-secret fields print normally.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionIdentity")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("secret", &"<redacted>")
            .field("role", &self.role)
            .field("session_tile", &self.session_tile)
            .field("ship_slug", &self.ship_slug)
            .field("minted_at", &self.minted_at)
            .finish()
    }
}

/// A non-secret view of an identity - what an attribution stamp / observability may
/// safely expose. NEVER carries the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityStamp {
    pub id: String,
    pub generation: u64,
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_tile: Option<String>,
    /// The durable ship this session belongs to, when resolved (item-2 §2.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_slug: Option<String>,
}

impl SessionIdentity {
    /// The attribution stamp (id + role + tile + ship), never the secret.
    pub fn stamp(&self) -> IdentityStamp {
        IdentityStamp {
            id: self.id.clone(),
            generation: self.generation,
            role: self.role,
            session_tile: self.session_tile.clone(),
            ship_slug: self.ship_slug.clone(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentitiesSnapshot {
    schema_version: u32,
    /// Last generation allocated to a SessionIdentity.
    #[serde(default)]
    generation: u64,
    /// Keyed by the minted identity id.
    identities: HashMap<String, SessionIdentity>,
    /// item-3 Pillar B: the explicit REVOCATION set (minted ids). An id here never
    /// resolves again even if its record somehow survives, so a compromised secret is
    /// killed the instant it is revoked. Persisted (a durable tombstone), `#[serde(
    /// default)]` so pre-item-3 stores load. Distinct from retire/close-GC (which is
    /// lifecycle cleanup); this is a deliberate "this credential is burned" act.
    #[serde(default)]
    revoked: HashSet<String>,
}

impl Default for IdentitiesSnapshot {
    fn default() -> Self {
        Self {
            schema_version: IDENTITIES_SCHEMA_VERSION,
            generation: 0,
            identities: HashMap::new(),
            revoked: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityGenerationWatermark {
    schema_version: u32,
    format_identity: String,
    identities_schema_version: u32,
    generation_high_water: u64,
}

impl IdentityGenerationWatermark {
    fn current(generation_high_water: u64) -> Self {
        Self {
            schema_version: IDENTITY_GENERATION_WATERMARK_SCHEMA_VERSION,
            format_identity: IDENTITY_GENERATION_WATERMARK_FORMAT.to_string(),
            identities_schema_version: IDENTITIES_SCHEMA_VERSION,
            generation_high_water,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != IDENTITY_GENERATION_WATERMARK_SCHEMA_VERSION
            || self.format_identity != IDENTITY_GENERATION_WATERMARK_FORMAT
            || self.identities_schema_version != IDENTITIES_SCHEMA_VERSION
        {
            return Err("identity generation watermark is malformed or unsupported".into());
        }
        Ok(())
    }
}

fn generation_watermark_path(identity_path: &Path) -> PathBuf {
    identity_path.with_extension("generations.json")
}

fn read_generation_watermark(path: &PathBuf) -> Result<IdentityGenerationWatermark, String> {
    let source = std::fs::read(path).map_err(|error| {
        format!(
            "identity generation watermark '{}' could not be read: {error}",
            path.display()
        )
    })?;
    let watermark: IdentityGenerationWatermark =
        serde_json::from_slice(&source).map_err(|error| {
            format!(
                "identity generation watermark '{}' is malformed: {error}",
                path.display()
            )
        })?;
    watermark.validate()?;
    Ok(watermark)
}

fn write_generation_watermark(
    path: &PathBuf,
    watermark: &IdentityGenerationWatermark,
) -> std::io::Result<()> {
    watermark.validate().map_err(std::io::Error::other)?;
    write_private_atomic(path, &serde_json::to_vec_pretty(watermark)?)
}

fn validate_identities_snapshot(snapshot: &IdentitiesSnapshot) -> Result<(), String> {
    if snapshot.schema_version != IDENTITIES_SCHEMA_VERSION {
        return Err(format!(
            "identity store uses unsupported schemaVersion {}",
            snapshot.schema_version
        ));
    }
    let mut generations = HashSet::new();
    let mut maximum = 0;
    for (key, identity) in &snapshot.identities {
        if key != &identity.id {
            return Err("identity store key does not match the persisted identity id".into());
        }
        if identity.generation == 0
            || identity.generation > snapshot.generation
            || !generations.insert(identity.generation)
        {
            return Err(
                "identity store contains a zero, duplicate, or out-of-range generation".into(),
            );
        }
        maximum = maximum.max(identity.generation);
    }
    if maximum > snapshot.generation
        || (snapshot.generation == 0 && !snapshot.identities.is_empty())
    {
        return Err("identity store generation high-water mark is inconsistent".into());
    }
    Ok(())
}

fn read_identities_snapshot(path: &PathBuf) -> Result<(IdentitiesSnapshot, bool), String> {
    let source = std::fs::read(path).map_err(|error| {
        format!(
            "identity store '{}' could not be read: {error}",
            path.display()
        )
    })?;
    let mut document: serde_json::Value = serde_json::from_slice(&source).map_err(|error| {
        format!(
            "identity store '{}' is invalid JSON: {error}",
            path.display()
        )
    })?;
    let object = document.as_object_mut().ok_or_else(|| {
        format!(
            "identity store '{}' is not a versioned object",
            path.display()
        )
    })?;
    if let Some(version) = object.get("schemaVersion") {
        let version = version.as_u64().ok_or_else(|| {
            format!(
                "identity store '{}' has a malformed schemaVersion",
                path.display()
            )
        })?;
        if version != IDENTITIES_SCHEMA_VERSION as u64 {
            return Err(format!(
                "identity store '{}' uses unsupported schemaVersion {version}",
                path.display()
            ));
        }
        let snapshot: IdentitiesSnapshot = serde_json::from_value(document).map_err(|error| {
            format!(
                "identity store '{}' has an invalid current-schema shape: {error}",
                path.display()
            )
        })?;
        validate_identities_snapshot(&snapshot)?;
        return Ok((snapshot, false));
    }

    let generation_counter_present = object.contains_key("generation");
    let identities = object
        .get("identities")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            format!(
                "legacy identity store '{}' has no identities object",
                path.display()
            )
        })?;
    let generated_identity_count = identities
        .values()
        .filter(|identity| identity.get("generation").is_some())
        .count();
    if (generation_counter_present && generated_identity_count != identities.len())
        || (!generation_counter_present && generated_identity_count != 0)
    {
        return Err(format!(
            "legacy identity store '{}' has a partial durable generation shape",
            path.display()
        ));
    }
    object.insert(
        "schemaVersion".to_string(),
        serde_json::Value::from(IDENTITIES_SCHEMA_VERSION),
    );
    let mut snapshot: IdentitiesSnapshot = serde_json::from_value(document).map_err(|error| {
        format!(
            "legacy identity store '{}' could not be migrated: {error}",
            path.display()
        )
    })?;
    if !generation_counter_present {
        let mut ids = snapshot.identities.keys().cloned().collect::<Vec<_>>();
        ids.sort_by(|left, right| {
            let left_identity = &snapshot.identities[left];
            let right_identity = &snapshot.identities[right];
            (left_identity.minted_at, left).cmp(&(right_identity.minted_at, right))
        });
        for (index, id) in ids.into_iter().enumerate() {
            let generation = u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .ok_or("legacy identity store has too many identities to migrate")?;
            snapshot.identities.get_mut(&id).unwrap().generation = generation;
            snapshot.generation = generation;
        }
    }
    validate_identities_snapshot(&snapshot)?;
    Ok((snapshot, true))
}

/// Identity-secret TTL in ms, from `T_HUB_IDENTITY_TTL_SECS` (seconds). `0` (the
/// default) DISABLES expiry - the mechanism ships and is enforced when configured, but
/// off by default so a long-lived session (a captain running for days) is never cut
/// off mid-life; the cadence is an operator knob (N4-class). Revocation is always-on.
fn identity_ttl_ms() -> u64 {
    std::env::var("T_HUB_IDENTITY_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|secs| secs.saturating_mul(1000))
        .unwrap_or(0)
}

/// The per-session identity store: an in-memory map behind a mutex, persisted
/// atomically to `identities.json`. Shared (`Arc`) between the spawn path (mint/bind)
/// and the enqueue/ack path (resolve).
pub struct IdentityStore {
    path: Option<PathBuf>,
    watermark_path: Option<PathBuf>,
    inner: Mutex<IdentitiesSnapshot>,
    generation_watermark: Mutex<IdentityGenerationWatermark>,
    write_blocked: Option<String>,
    #[cfg(test)]
    persist_fail_after: Mutex<Option<usize>>,
    #[cfg(test)]
    fail_identity_write_after_watermark: Mutex<bool>,
}

impl IdentityStore {
    /// Load (or start empty) from `path`. A missing file starts empty. Malformed or
    /// unsupported persisted state remains byte-preserved and blocks every write.
    /// item-3 Pillar B: each
    /// secret is UNSEALED on load (DPAPI on Windows; a legacy/plaintext secret reads
    /// verbatim), so the in-memory secret is always the plaintext `resolve` compares.
    pub fn load(path: PathBuf) -> Self {
        let watermark_path = generation_watermark_path(&path);
        let identity_source_exists = path.exists();
        let watermark_source_exists = watermark_path.exists();
        let identity_read = if identity_source_exists {
            match read_identities_snapshot(&path) {
                Ok(result) => Ok(Some(result)),
                Err(error) => Err(error),
            }
        } else {
            Ok(None)
        };
        let watermark_read = if watermark_source_exists {
            read_generation_watermark(&watermark_path).map(Some)
        } else {
            Ok(None)
        };
        let (mut inner, schema_upgraded, write_blocked, watermark) =
            match (identity_read, watermark_read) {
                (Err(error), _) | (_, Err(error)) => (
                    IdentitiesSnapshot::default(),
                    false,
                    Some(error),
                    IdentityGenerationWatermark::current(0),
                ),
                (Ok(None), Ok(Some(watermark))) => (
                    IdentitiesSnapshot::default(),
                    false,
                    Some(
                        "identity generation watermark exists but identities.json is missing"
                            .to_string(),
                    ),
                    watermark,
                ),
                (Ok(Some((_snapshot, true))), Ok(Some(watermark))) => (
                    IdentitiesSnapshot::default(),
                    false,
                    Some(
                        "identity generation watermark proves identities.json was downgraded to an unversioned shape"
                            .to_string(),
                    ),
                    watermark,
                ),
                (Ok(Some((snapshot, false))), Ok(Some(watermark)))
                    if snapshot.generation != watermark.generation_high_water =>
                {
                    (
                        IdentitiesSnapshot::default(),
                        false,
                        Some(
                            "identity generation watermark does not match identities.json"
                                .to_string(),
                        ),
                        watermark,
                    )
                }
                (Ok(Some((snapshot, upgraded))), Ok(Some(watermark))) => {
                    (snapshot, upgraded, None, watermark)
                }
                (Ok(Some((snapshot, upgraded))), Ok(None)) => {
                    let generation = snapshot.generation;
                    (
                        snapshot,
                        upgraded,
                        None,
                        IdentityGenerationWatermark::current(generation),
                    )
                }
                (Ok(None), Ok(None)) => (
                    IdentitiesSnapshot::default(),
                    false,
                    None,
                    IdentityGenerationWatermark::current(0),
                ),
            };
        if let Some(error) = write_blocked.as_deref() {
            eprintln!("t-hub-identity: writes blocked: {error}");
        }
        for ident in inner.identities.values_mut() {
            if let Some(plain) = crate::secret_seal::unseal_str(&ident.secret) {
                ident.secret = plain;
            }
            // If a sealed secret cannot be unsealed here (wrong host), it is left as-is
            // and simply never resolves - the store belongs to the Windows-hosted app.
        }
        let mut store = IdentityStore {
            path: Some(path),
            watermark_path: Some(watermark_path),
            inner: Mutex::new(inner),
            generation_watermark: Mutex::new(watermark),
            write_blocked,
            #[cfg(test)]
            persist_fail_after: Mutex::new(None),
            #[cfg(test)]
            fail_identity_write_after_watermark: Mutex::new(false),
        };
        let watermark_missing = !watermark_source_exists;
        if (schema_upgraded || (identity_source_exists && watermark_missing))
            && store.write_blocked.is_none()
        {
            let persist_result = {
                let snapshot = store.lock();
                store.persist(&snapshot)
            };
            if let Err(error) = persist_result {
                store.write_blocked = Some(format!(
                    "identity store migration could not be persisted: {error}"
                ));
            }
        }
        store
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
            watermark_path: None,
            inner: Mutex::new(IdentitiesSnapshot::default()),
            generation_watermark: Mutex::new(IdentityGenerationWatermark::current(0)),
            write_blocked: None,
            #[cfg(test)]
            persist_fail_after: Mutex::new(None),
            #[cfg(test)]
            fail_identity_write_after_watermark: Mutex::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, IdentitiesSnapshot> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, snap: &IdentitiesSnapshot) -> Result<(), String> {
        if let Some(reason) = self.write_blocked.as_deref() {
            return Err(format!("identity store writes are blocked: {reason}"));
        }
        validate_identities_snapshot(snap)?;
        #[cfg(test)]
        {
            let mut fail_after = self
                .persist_fail_after
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(remaining) = fail_after.as_mut() {
                if *remaining == 0 {
                    *fail_after = None;
                    return Err("identity store persist failure injected by test".into());
                }
                *remaining -= 1;
            }
        }
        let Some(path) = &self.path else {
            self.generation_watermark
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .generation_high_water = snap.generation;
            return Ok(());
        };
        let watermark_path = self
            .watermark_path
            .as_ref()
            .ok_or("identity generation watermark path is unavailable")?;
        {
            let mut watermark = self
                .generation_watermark
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if snap.generation < watermark.generation_high_water {
                return Err(
                    "identity store generation would regress below its durable watermark".into(),
                );
            }
            if snap.generation > watermark.generation_high_water || !watermark_path.exists() {
                let advanced = IdentityGenerationWatermark::current(snap.generation);
                write_generation_watermark(watermark_path, &advanced).map_err(|error| {
                    format!(
                        "identity generation watermark persist to '{}' failed: {error}",
                        watermark_path.display()
                    )
                })?;
                *watermark = advanced;
            }
        }
        #[cfg(test)]
        {
            let mut fail = self
                .fail_identity_write_after_watermark
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *fail {
                *fail = false;
                return Err(
                    "identity store failure after watermark persistence injected by test".into(),
                );
            }
        }
        write_atomic(path, snap).map_err(|error| {
            format!(
                "identity store persist to '{}' failed: {error}",
                path.display()
            )
        })
    }

    /// Mint a fresh per-session identity: a distinct id + secret, recorded in the
    /// store. The secret is what gets env-injected; the returned identity carries it
    /// so the caller can inject it, but the STAMP (id/role) is what attribution uses.
    pub fn mint(&self, role: Role) -> Result<SessionIdentity, String> {
        self.mint_for(role, None)
    }

    /// Mint a fresh per-session identity carrying its durable SHIP (item-2 §2.6,
    /// D5: the widened binding). `ship` is the spawner captain's ship_slug when it
    /// could be resolved at spawn (a crew inherits its spawner's ship), else `None`.
    /// Everything else matches [`mint`](Self::mint) - one seam, wider payload.
    pub fn mint_for(&self, role: Role, ship: Option<String>) -> Result<SessionIdentity, String> {
        let mut snap = self.lock();
        let durable_high_water = self
            .generation_watermark
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation_high_water;
        let generation = snap
            .generation
            .max(durable_high_water)
            .checked_add(1)
            .ok_or("identity store generation exhausted")?;
        let identity = SessionIdentity {
            id: uuid::Uuid::new_v4().simple().to_string(),
            generation,
            // Two v4 uuids => ~244 bits of entropy; a bearer secret, not a display id.
            secret: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            role,
            session_tile: None,
            ship_slug: ship,
            minted_at: now_ms(),
        };
        let previous = snap.clone();
        snap.generation = generation;
        snap.identities
            .insert(identity.id.clone(), identity.clone());
        if let Err(error) = self.persist(&snap) {
            *snap = previous;
            return Err(error);
        }
        Ok(identity)
    }

    /// Bind a minted identity to the tile id its session landed on (after spawn). The
    /// tile is a mutable pointer; the durable key stays the minted id.
    pub fn bind_tile(&self, id: &str, tile: &str) -> Result<(), String> {
        let mut snap = self.lock();
        let previous = snap.clone();
        let ident = snap
            .identities
            .get_mut(id)
            .ok_or_else(|| format!("cannot bind unknown identity '{id}'"))?;
        ident.session_tile = Some(tile.to_string());
        if let Err(error) = self.persist(&snap) {
            *snap = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Retire (forget) an identity by its minted id, dropping its persisted secret.
    /// Used to clean up an orphan from a FAILED spawn (the mint persisted before the
    /// spawn's `?` returned - review L2) and, via [`retire_tile`](Self::retire_tile),
    /// when a session closes (review M3 - bound the secret-bearing store to live +
    /// not-yet-closed sessions rather than letting it grow forever). A no-op for an
    /// unknown id. After this, the retired secret no longer `resolve`s.
    pub fn retire(&self, id: &str) -> Result<bool, String> {
        let mut snap = self.lock();
        let previous = snap.clone();
        let removed = snap.identities.remove(id).is_some();
        if removed {
            if let Err(error) = self.persist(&snap) {
                *snap = previous;
                return Err(error);
            }
        }
        Ok(removed)
    }

    /// Retire the identity bound to a tile id (the session-close GC hook, M3). Removes
    /// the identity whose `session_tile` matches, so a dead session's secret stops
    /// resolving and the store does not accrete dead sessions. A no-op if no identity
    /// is bound to that tile.
    pub fn retire_tile(&self, tile: &str) -> Result<bool, String> {
        let id = {
            let snap = self.lock();
            snap.identities
                .values()
                .find(|i| i.session_tile.as_deref() == Some(tile))
                .map(|i| i.id.clone())
        };
        match id {
            Some(id) => self.retire(&id),
            None => Ok(false),
        }
    }

    /// Load-time prune / reconciliation (PR-56 review LOW residual, closed by item-2
    /// which now owns the identity store). Session-close GC ([`retire_tile`]) is
    /// close-path-driven ONLY: an identity whose session died WITHOUT a clean
    /// `close_terminal` (SIGKILL, crash, a process that exited before its close
    /// reached us) is never retired, so its secret lingers in the store forever and
    /// the store accretes across restarts. This reconciles at load, retiring an
    /// identity when its session is UNAMBIGUOUSLY gone:
    ///
    /// - `session_tile: None`: minted by a PRIOR process that died before it bound the
    ///   tile (a failed spawn whose L2 retire never ran); a fresh process has no live
    ///   in-flight mints, so an unbound identity at load is a leak.
    /// - `session_tile: Some(tile)` where `is_live(tile)` is false: the tile's tmux
    ///   session is gone (the SOLE unambiguous-death signal, mirroring the registry's
    ///   transfer-grade liveness; `is_live` is `tmux::has_session` in production).
    ///
    /// A still-live tile is KEPT. Returns the number retired. Call ONCE at load, from
    /// the construction site that can supply the liveness predicate (`lib.rs`); the
    /// store itself stays tmux-free and unit-testable via an injected predicate.
    pub fn prune_dead(&self, is_live: impl Fn(&str) -> bool) -> Result<usize, String> {
        let mut snap = self.lock();
        let previous = snap.clone();
        let before = snap.identities.len();
        snap.identities
            .retain(|_, ident| match ident.session_tile.as_deref() {
                Some(tile) => is_live(tile),
                None => false,
            });
        let removed = before - snap.identities.len();
        if removed > 0 {
            if let Err(error) = self.persist(&snap) {
                *snap = previous;
                return Err(error);
            }
        }
        Ok(removed)
    }

    /// Explicitly REVOKE an identity by its minted id (item-3 Pillar B): record it in
    /// the durable revocation set AND drop its secret from the store, so a compromised
    /// or burned credential stops resolving immediately and forever. Distinct from
    /// [`retire`](Self::retire) (lifecycle GC): revocation is a deliberate "this
    /// credential is burned" act and leaves a persistent tombstone so the id can never
    /// resolve again. Returns true if anything changed (newly revoked or removed).
    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        let mut snap = self.lock();
        let previous = snap.clone();
        let newly_revoked = snap.revoked.insert(id.to_string());
        let removed = snap.identities.remove(id).is_some();
        if newly_revoked || removed {
            if let Err(error) = self.persist(&snap) {
                *snap = previous;
                return Err(error);
            }
        }
        Ok(newly_revoked || removed)
    }

    /// Whether an identity id has been revoked.
    pub fn is_revoked(&self, id: &str) -> bool {
        self.lock().revoked.contains(id)
    }

    /// Resolve a presented per-session token to its identity, constant-time. `None`
    /// for an empty/unknown token, a REVOKED identity, or one past its TTL (item-3
    /// Pillar B). This is IDENTIFICATION, never authorization.
    pub fn resolve(&self, presented: &str) -> Option<SessionIdentity> {
        self.resolve_with(presented, now_ms(), identity_ttl_ms())
    }

    /// [`resolve`](Self::resolve) with the clock + TTL passed in, so the expiry/
    /// revocation logic is pure and race-free to test (no env / no wall clock).
    fn resolve_with(&self, presented: &str, now: u64, ttl_ms: u64) -> Option<SessionIdentity> {
        if presented.is_empty() {
            return None;
        }
        let snap = self.lock();
        snap.identities
            .values()
            .find(|ident| ct_eq(&ident.secret, presented))
            // Revoked ids never resolve, even if the record somehow lingers.
            .filter(|ident| !snap.revoked.contains(&ident.id))
            // TTL expiry (0 => disabled): a leaked dead-session secret stops resolving.
            .filter(|ident| ttl_ms == 0 || now.saturating_sub(ident.minted_at) <= ttl_ms)
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

    #[cfg(test)]
    pub fn fail_persist_after(&self, successful_writes: usize) {
        *self
            .persist_fail_after
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(successful_writes);
    }

    #[cfg(test)]
    pub fn fail_identity_write_after_watermark(&self) {
        *self
            .fail_identity_write_after_watermark
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
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
/// item-3 Pillar B: each secret is SEALED at rest (DPAPI on Windows; plaintext behind
/// 0600 on the pure-WSL/ext4 and dev/CI hosts) so the shared-readable state file does
/// not expose the bearer tokens in cleartext on the real Windows deployment.
fn write_atomic(path: &PathBuf, snap: &IdentitiesSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Seal secrets only at the disk boundary; the in-memory secret stays plaintext so
    // `resolve` compares directly.
    let mut sealed = snap.clone();
    for ident in sealed.identities.values_mut() {
        ident.secret = crate::secret_seal::seal_str(&ident.secret);
    }
    let body = serde_json::to_vec_pretty(&sealed)?;
    write_private_atomic(path, &body)
}

fn write_private_atomic(path: &PathBuf, body: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, body)?;
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
        let a = store.mint(Role::Crew).unwrap();
        let b = store.mint(Role::Crew).unwrap();
        assert_ne!(a.id, b.id, "ids are distinct");
        assert_ne!(a.secret, b.secret, "secrets are distinct");
        assert_eq!(a.generation, 1);
        assert_eq!(b.generation, 2);
        assert!(!a.secret.is_empty());
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn debug_redacts_the_secret() {
        // LOW-3: a stray {:?} must never print the bearer secret in cleartext (it would
        // undo the at-rest sealing). Non-secret fields still print.
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        let dbg = format!("{a:?}");
        assert!(
            !dbg.contains(&a.secret),
            "Debug must NOT contain the secret: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Debug must show the redaction marker"
        );
        assert!(dbg.contains(&a.id), "Debug still shows the non-secret id");
    }

    #[test]
    fn revoke_kills_resolve_and_persists_a_tombstone() {
        // item-3 Pillar B: an explicit revoke stops the secret resolving immediately,
        // AND survives a reload as a durable tombstone. BYPASS-WOULD-FAIL: drop the
        // revoked-set check in resolve and the post-revoke resolve returns the identity.
        let path = temp_path();
        let a = {
            let store = IdentityStore::load(path.clone());
            let a = store.mint(Role::Crew).unwrap();
            assert!(
                store.resolve(&a.secret).is_some(),
                "mints resolve before revoke"
            );
            assert!(store.revoke(&a.id).unwrap(), "revoke reports a change");
            assert!(
                store.resolve(&a.secret).is_none(),
                "a revoked secret must not resolve"
            );
            assert!(store.is_revoked(&a.id));
            a
        };
        // Reload: the tombstone persisted, so even a resurrected record would not resolve.
        let reloaded = IdentityStore::load(path.clone());
        assert!(
            reloaded.is_revoked(&a.id),
            "the revocation tombstone must persist"
        );
        assert!(reloaded.resolve(&a.secret).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ttl_expires_a_secret_but_zero_disables_expiry() {
        // item-3 Pillar B TTL mechanism (pure, race-free via resolve_with). ttl=0
        // disables expiry (the default); a positive ttl rejects a secret older than it.
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        let minted = a.minted_at;
        // ttl disabled: resolves at any clock.
        assert!(store
            .resolve_with(&a.secret, minted + 10_000_000, 0)
            .is_some());
        // Within ttl: resolves.
        assert!(store.resolve_with(&a.secret, minted + 500, 1000).is_some());
        // Past ttl: expired, no resolve.
        assert!(
            store.resolve_with(&a.secret, minted + 2000, 1000).is_none(),
            "a secret older than the TTL must not resolve"
        );
    }

    #[test]
    fn identity_secrets_are_sealed_at_rest_and_unsealed_on_load() {
        // item-3 Pillar B at-rest sealing: the on-disk secret is the sealed form
        // (unseal_str recovers it), and load() restores the plaintext so resolve works.
        // On the Linux dev/CI host the seal is the 0600-fallback identity, so the disk
        // form is plaintext; the round-trip and the unseal_str invariant hold regardless.
        let path = temp_path();
        let secret = {
            let store = IdentityStore::load(path.clone());
            store.mint(Role::Crew).unwrap().secret
        };
        // The raw file's stored secret unseals back to the plaintext secret.
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let on_disk = parsed["identities"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["secret"]
            .as_str()
            .unwrap();
        assert_eq!(
            crate::secret_seal::unseal_str(on_disk).as_deref(),
            Some(secret.as_str()),
            "the on-disk secret must unseal back to the plaintext"
        );
        // And a fresh load restores the plaintext so resolve still works.
        let reloaded = IdentityStore::load(path.clone());
        assert!(
            reloaded.resolve(&secret).is_some(),
            "reload must restore a resolvable secret"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolve_returns_the_minting_identity_only_for_its_own_secret() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Captain).unwrap();
        let b = store.mint(Role::Crew).unwrap();
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
        let a = store.mint(Role::Crew).unwrap();
        let b = store.mint(Role::Crew).unwrap();
        // Present A's secret => you are A, full stop; you cannot become B.
        let resolved = store.resolve(&a.secret).unwrap();
        assert_ne!(resolved.id, b.id);
    }

    #[test]
    fn bind_tile_records_the_mutable_pointer() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        assert!(a.session_tile.is_none());
        store.bind_tile(&a.id, "abc12345").unwrap();
        assert_eq!(
            store.get(&a.id).unwrap().session_tile.as_deref(),
            Some("abc12345")
        );
        assert_eq!(store.for_tile("abc12345").unwrap().id, a.id);
    }

    #[test]
    fn persistence_round_trips_including_the_binding() {
        let path = temp_path();
        let _ = std::fs::remove_file(&path);
        let id;
        let secret;
        let generation;
        {
            let store = IdentityStore::load(path.clone());
            let a = store.mint(Role::Captain).unwrap();
            store.bind_tile(&a.id, "tile-1").unwrap();
            id = a.id.clone();
            secret = a.secret.clone();
            generation = a.generation;
        }
        // Reopen from disk: the identity + binding survive, and the secret still
        // resolves.
        let store2 = IdentityStore::load(path.clone());
        assert_eq!(store2.len(), 1);
        let resolved = store2.resolve(&secret).unwrap();
        assert_eq!(resolved.id, id);
        assert_eq!(resolved.generation, generation);
        assert_eq!(resolved.role, Role::Captain);
        assert_eq!(resolved.session_tile.as_deref(), Some("tile-1"));
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            persisted["schemaVersion"],
            serde_json::Value::from(IDENTITIES_SCHEMA_VERSION)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn identity_generation_is_durable_monotonic_and_never_reuses_retired_values() {
        let path = temp_path();
        let first_generation = {
            let store = IdentityStore::load(path.clone());
            let first = store.mint(Role::Cortana).unwrap();
            assert!(store.retire(&first.id).unwrap());
            first.generation
        };
        let reloaded = IdentityStore::load(path.clone());
        let replacement = reloaded.mint(Role::Cortana).unwrap();
        assert_eq!(replacement.generation, first_generation + 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sender_label_is_role_scoped_and_carries_no_secret() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        let label = a.sender_label();
        assert!(label.starts_with("crew:"));
        assert!(
            !label.contains(&a.secret),
            "the sender label never leaks the secret"
        );
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
        assert!(store.write_blocked.is_none());
    }

    #[test]
    fn legacy_identity_store_migration_is_explicit_deterministic_and_persisted() {
        let path = temp_path();
        let legacy = serde_json::json!({
            "identities": {
                "identity-b": {
                    "id": "identity-b",
                    "secret": "secret-b",
                    "role": "crew",
                    "mintedAt": 20
                },
                "identity-a": {
                    "id": "identity-a",
                    "secret": "secret-a",
                    "role": "cortana",
                    "mintedAt": 10
                }
            },
            "revoked": []
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let store = IdentityStore::load(path.clone());
        assert!(store.write_blocked.is_none());
        assert_eq!(store.get("identity-a").unwrap().generation, 1);
        assert_eq!(store.get("identity-b").unwrap().generation, 2);
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            persisted["schemaVersion"],
            serde_json::Value::from(IDENTITIES_SCHEMA_VERSION)
        );
        assert_eq!(persisted["generation"], 2);
        assert_eq!(persisted["identities"]["identity-a"]["generation"], 1);
        assert_eq!(persisted["identities"]["identity-b"]["generation"], 2);

        let reloaded = IdentityStore::load(path.clone());
        assert_eq!(reloaded.get("identity-a").unwrap().generation, 1);
        assert_eq!(reloaded.get("identity-b").unwrap().generation, 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unversioned_generation_store_preserves_its_durable_high_water_mark() {
        let path = temp_path();
        let e6_shape = serde_json::json!({
            "generation": 9,
            "identities": {
                "identity-seven": {
                    "id": "identity-seven",
                    "generation": 7,
                    "secret": "secret-seven",
                    "role": "cortana",
                    "mintedAt": 10
                }
            },
            "revoked": []
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&e6_shape).unwrap()).unwrap();

        let store = IdentityStore::load(path.clone());
        assert!(store.write_blocked.is_none());
        assert_eq!(store.get("identity-seven").unwrap().generation, 7);
        assert_eq!(store.mint(Role::Crew).unwrap().generation, 10);
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["generation"], 10);
        assert_eq!(
            persisted["schemaVersion"],
            serde_json::Value::from(IDENTITIES_SCHEMA_VERSION)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn old_writer_round_trip_after_v1_is_detected_and_never_reuses_retired_generation() {
        let path = temp_path();
        let watermark_path = generation_watermark_path(&path);
        let (survivor, retired_high_water) = {
            let store = IdentityStore::load(path.clone());
            let survivor = store.mint(Role::Cortana).unwrap();
            let retired = store.mint(Role::Crew).unwrap();
            assert!(store.retire(&retired.id).unwrap());
            (survivor, retired.generation)
        };
        assert_eq!(
            read_generation_watermark(&watermark_path)
                .unwrap()
                .generation_high_water,
            retired_high_water
        );
        assert!(
            !String::from_utf8_lossy(&std::fs::read(&watermark_path).unwrap())
                .contains(&survivor.secret)
        );

        let mut old_writer_document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let object = old_writer_document.as_object_mut().unwrap();
        object.remove("schemaVersion");
        object.remove("generation");
        for identity in object["identities"].as_object_mut().unwrap().values_mut() {
            identity.as_object_mut().unwrap().remove("generation");
        }
        let downgraded_bytes = serde_json::to_vec_pretty(&old_writer_document).unwrap();
        std::fs::write(&path, &downgraded_bytes).unwrap();

        let downgraded = IdentityStore::load(path.clone());
        assert!(downgraded
            .write_blocked
            .as_deref()
            .is_some_and(|reason| { reason.contains("downgraded to an unversioned shape") }));
        assert!(downgraded
            .mint(Role::Crew)
            .unwrap_err()
            .contains("writes are blocked"));
        assert!(downgraded
            .revoke(&survivor.id)
            .unwrap_err()
            .contains("writes are blocked"));
        assert_eq!(std::fs::read(&path).unwrap(), downgraded_bytes);
        assert_eq!(
            read_generation_watermark(&watermark_path)
                .unwrap()
                .generation_high_water,
            retired_high_water
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(watermark_path);
    }

    #[test]
    fn watermark_future_malformed_and_mismatch_states_fail_closed_byte_preserving() {
        let cases = [
            (
                "future",
                serde_json::json!({
                    "schemaVersion": IDENTITY_GENERATION_WATERMARK_SCHEMA_VERSION + 1,
                    "formatIdentity": IDENTITY_GENERATION_WATERMARK_FORMAT,
                    "identitiesSchemaVersion": IDENTITIES_SCHEMA_VERSION,
                    "generationHighWater": 1
                }),
            ),
            (
                "malformed",
                serde_json::json!({
                    "schemaVersion": IDENTITY_GENERATION_WATERMARK_SCHEMA_VERSION,
                    "formatIdentity": "wrong-format",
                    "identitiesSchemaVersion": IDENTITIES_SCHEMA_VERSION,
                    "generationHighWater": 1
                }),
            ),
            (
                "mismatch",
                serde_json::json!({
                    "schemaVersion": IDENTITY_GENERATION_WATERMARK_SCHEMA_VERSION,
                    "formatIdentity": IDENTITY_GENERATION_WATERMARK_FORMAT,
                    "identitiesSchemaVersion": IDENTITIES_SCHEMA_VERSION,
                    "generationHighWater": 9
                }),
            ),
        ];

        for (name, watermark_document) in cases {
            let path = temp_path();
            let watermark_path = generation_watermark_path(&path);
            let secret = {
                let store = IdentityStore::load(path.clone());
                store.mint(Role::Crew).unwrap().secret
            };
            let identity_bytes = std::fs::read(&path).unwrap();
            let watermark_bytes = serde_json::to_vec_pretty(&watermark_document).unwrap();
            assert!(!String::from_utf8_lossy(&watermark_bytes).contains(&secret));
            std::fs::write(&watermark_path, &watermark_bytes).unwrap();

            let blocked = IdentityStore::load(path.clone());
            assert!(blocked.write_blocked.is_some(), "case {name}");
            assert!(
                blocked
                    .mint(Role::Crew)
                    .unwrap_err()
                    .contains("writes are blocked"),
                "case {name}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), identity_bytes, "case {name}");
            assert_eq!(
                std::fs::read(&watermark_path).unwrap(),
                watermark_bytes,
                "case {name}"
            );
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(watermark_path);
        }
    }

    #[test]
    fn watermark_first_crash_ordering_blocks_restart_without_identity_bytes() {
        let path = temp_path();
        let watermark_path = generation_watermark_path(&path);
        {
            let store = IdentityStore::load(path.clone());
            store.fail_identity_write_after_watermark();
            assert!(store.mint(Role::Crew).is_err());
            assert!(store.is_empty());
        }
        assert!(!path.exists());
        let watermark_bytes = std::fs::read(&watermark_path).unwrap();
        assert_eq!(
            read_generation_watermark(&watermark_path)
                .unwrap()
                .generation_high_water,
            1
        );

        let restarted = IdentityStore::load(path.clone());
        assert!(restarted.write_blocked.is_some());
        assert!(restarted.mint(Role::Crew).is_err());
        assert_eq!(std::fs::read(&watermark_path).unwrap(), watermark_bytes);
        let _ = std::fs::remove_file(watermark_path);
    }

    #[test]
    fn watermark_rollback_retires_failed_generation_before_same_process_retry() {
        let path = temp_path();
        let watermark_path = generation_watermark_path(&path);
        let store = IdentityStore::load(path.clone());
        store.fail_identity_write_after_watermark();
        assert!(store.mint(Role::Crew).is_err());
        assert!(store.is_empty());

        let replacement = store.mint(Role::Crew).unwrap();
        assert_eq!(replacement.generation, 2);
        assert_eq!(
            read_generation_watermark(&watermark_path)
                .unwrap()
                .generation_high_water,
            2
        );
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["generation"], 2);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(watermark_path);
    }

    #[cfg(unix)]
    #[test]
    fn identity_and_generation_watermark_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_path();
        let watermark_path = generation_watermark_path(&path);
        let store = IdentityStore::load(path.clone());
        store.mint(Role::Crew).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&watermark_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(watermark_path);
    }

    #[test]
    fn future_identity_schema_blocks_downgrade_writes_and_preserves_bytes() {
        let path = temp_path();
        let future = serde_json::json!({
            "schemaVersion": IDENTITIES_SCHEMA_VERSION + 1,
            "generation": 41,
            "identities": {},
            "revoked": []
        });
        let source = serde_json::to_vec_pretty(&future).unwrap();
        std::fs::write(&path, &source).unwrap();

        let store = IdentityStore::load(path.clone());
        assert!(store
            .write_blocked
            .as_deref()
            .is_some_and(|reason| { reason.contains("unsupported schemaVersion") }));
        assert!(store
            .mint(Role::Crew)
            .unwrap_err()
            .contains("writes are blocked"));
        assert_eq!(std::fs::read(&path).unwrap(), source);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn malformed_identity_generations_fail_closed_and_preserve_source_bytes() {
        let cases = [
            (
                "duplicate",
                serde_json::json!({
                    "schemaVersion": IDENTITIES_SCHEMA_VERSION,
                    "generation": 2,
                    "identities": {
                        "identity-a": {
                            "id": "identity-a", "generation": 1, "secret": "a",
                            "role": "crew", "mintedAt": 1
                        },
                        "identity-b": {
                            "id": "identity-b", "generation": 1, "secret": "b",
                            "role": "crew", "mintedAt": 2
                        }
                    },
                    "revoked": []
                }),
            ),
            (
                "above-high-water",
                serde_json::json!({
                    "schemaVersion": IDENTITIES_SCHEMA_VERSION,
                    "generation": 1,
                    "identities": {
                        "identity-a": {
                            "id": "identity-a", "generation": 2, "secret": "a",
                            "role": "crew", "mintedAt": 1
                        }
                    },
                    "revoked": []
                }),
            ),
            (
                "partial-legacy",
                serde_json::json!({
                    "generation": 2,
                    "identities": {
                        "identity-a": {
                            "id": "identity-a", "generation": 1, "secret": "a",
                            "role": "crew", "mintedAt": 1
                        },
                        "identity-b": {
                            "id": "identity-b", "secret": "b",
                            "role": "crew", "mintedAt": 2
                        }
                    },
                    "revoked": []
                }),
            ),
        ];

        for (name, malformed) in cases {
            let path = temp_path();
            let source = serde_json::to_vec_pretty(&malformed).unwrap();
            std::fs::write(&path, &source).unwrap();
            let store = IdentityStore::load(path.clone());
            assert!(store.write_blocked.is_some(), "case {name}");
            assert!(
                store
                    .mint(Role::Crew)
                    .unwrap_err()
                    .contains("writes are blocked"),
                "case {name}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), source, "case {name}");
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn mint_fails_without_publishing_an_in_memory_identity_when_persistence_fails() {
        let blocker = temp_path();
        std::fs::write(&blocker, b"not a directory").unwrap();
        let store = IdentityStore::load(blocker.join("identities.json"));

        let error = store.mint(Role::Crew).unwrap_err();

        assert!(error.contains("persist"), "got: {error}");
        assert!(
            store.is_empty(),
            "a failed durable mint must roll back memory"
        );
        std::fs::remove_file(blocker).unwrap();
    }

    #[test]
    fn bind_failure_restores_the_prior_unbound_identity() {
        let path = temp_path();
        let store = IdentityStore::load(path.clone());
        let identity = store.mint(Role::Crew).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        let error = store.bind_tile(&identity.id, "tile-a").unwrap_err();

        assert!(error.contains("persist"), "got: {error}");
        assert!(
            store.get(&identity.id).unwrap().session_tile.is_none(),
            "a failed durable bind must not remain published in memory"
        );
        std::fs::remove_dir(path).unwrap();
    }

    #[test]
    fn retire_failure_keeps_memory_and_disk_consistent() {
        let path = temp_path();
        let store = IdentityStore::load(path.clone());
        let identity = store.mint(Role::Crew).unwrap();
        store.fail_persist_after(0);

        assert!(store.retire(&identity.id).is_err());
        assert!(store.resolve(&identity.secret).is_some());
        let reloaded = IdentityStore::load(path.clone());
        assert!(reloaded.resolve(&identity.secret).is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prune_failure_restores_every_removed_identity() {
        let path = temp_path();
        let store = IdentityStore::load(path.clone());
        let a = store.mint(Role::Crew).unwrap();
        store.bind_tile(&a.id, "dead-a").unwrap();
        let b = store.mint(Role::Crew).unwrap();
        store.bind_tile(&b.id, "dead-b").unwrap();
        store.fail_persist_after(0);

        assert!(store.prune_dead(|_| false).is_err());
        assert_eq!(store.len(), 2);
        assert!(store.resolve(&a.secret).is_some());
        assert!(store.resolve(&b.secret).is_some());
        assert_eq!(IdentityStore::load(path.clone()).len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn revoke_failure_never_temporarily_revokes_then_resurrects_on_reload() {
        let path = temp_path();
        let store = IdentityStore::load(path.clone());
        let identity = store.mint(Role::Crew).unwrap();
        store.fail_persist_after(0);

        assert!(store.revoke(&identity.id).is_err());
        assert!(!store.is_revoked(&identity.id));
        assert!(
            store.resolve(&identity.secret).is_some(),
            "failed revoke must roll memory back instead of denying until restart"
        );
        let reloaded = IdentityStore::load(path.clone());
        assert!(!reloaded.is_revoked(&identity.id));
        assert!(reloaded.resolve(&identity.secret).is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn retire_drops_the_identity_and_stops_resolving() {
        // Review L2/M3: a retired identity is forgotten - its secret no longer
        // resolves and the store shrinks (no unbounded secret-bearing growth).
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        let b = store.mint(Role::Crew).unwrap();
        assert!(store.resolve(&a.secret).is_some());
        assert!(
            store.retire(&a.id).unwrap(),
            "retire reports it removed something"
        );
        assert!(
            store.resolve(&a.secret).is_none(),
            "retired secret no longer resolves"
        );
        assert_eq!(store.len(), 1, "only the retired identity is gone");
        assert!(
            store.resolve(&b.secret).is_some(),
            "the other identity is untouched"
        );
        // Retiring an unknown id is a no-op.
        assert!(!store.retire("no-such-id").unwrap());
    }

    #[test]
    fn mint_for_carries_the_durable_ship() {
        // Item-2 §2.6 D5: the widened binding carries the spawner's ship, resolved at
        // mint. A plain mint leaves it None (ship not resolvable / a captain claim).
        let store = IdentityStore::ephemeral();
        let crew = store
            .mint_for(Role::Crew, Some("t-hub-app".to_string()))
            .unwrap();
        assert_eq!(crew.ship_slug.as_deref(), Some("t-hub-app"));
        assert_eq!(crew.stamp().ship_slug.as_deref(), Some("t-hub-app"));
        let plain = store.mint(Role::Crew).unwrap();
        assert!(plain.ship_slug.is_none());
    }

    #[test]
    fn prune_dead_retires_gone_and_unbound_but_keeps_live() {
        // PR-56 residual: load-time reconciliation retires identities whose session is
        // unambiguously gone (dead tile OR never-bound prior-run leak) and KEEPS live
        // ones. A BYPASS - keeping a dead session's secret resolvable - fails here.
        let store = IdentityStore::ephemeral();
        let live = store.mint(Role::Crew).unwrap();
        store.bind_tile(&live.id, "live-tile").unwrap();
        let dead = store.mint(Role::Crew).unwrap();
        store.bind_tile(&dead.id, "dead-tile").unwrap();
        let unbound = store.mint(Role::Crew).unwrap(); // never bound (prior-run failed spawn)
        assert_eq!(store.len(), 3);

        let removed = store.prune_dead(|tile| tile == "live-tile").unwrap();
        assert_eq!(
            removed, 2,
            "the dead tile AND the unbound identity are pruned"
        );
        assert!(
            store.resolve(&live.secret).is_some(),
            "the live session survives"
        );
        assert!(
            store.resolve(&dead.secret).is_none(),
            "the dead session's secret is gone"
        );
        assert!(
            store.resolve(&unbound.secret).is_none(),
            "the unbound leak is gone"
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn prune_dead_is_a_noop_when_all_live() {
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        store.bind_tile(&a.id, "t").unwrap();
        assert_eq!(store.prune_dead(|_| true).unwrap(), 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn retire_tile_removes_the_identity_bound_to_a_closed_session() {
        // Review M3: the session-close GC hook retires by tile binding.
        let store = IdentityStore::ephemeral();
        let a = store.mint(Role::Crew).unwrap();
        store.bind_tile(&a.id, "deadtile").unwrap();
        assert!(store.retire_tile("deadtile").unwrap());
        assert!(store.resolve(&a.secret).is_none());
        assert!(store.is_empty());
        // No binding for that tile => no-op.
        assert!(!store.retire_tile("never-bound").unwrap());
    }
}
