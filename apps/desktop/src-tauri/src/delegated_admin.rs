//! Durable delegated-administration roles and authorization.
//!
//! A control capability only admits a caller to the control surface.
//! It does not confer authority over a target.
//! This module is the separate source of truth for durable Ship Admin and Fleet
//! Admin grants, their effective scope, revocation, supervisor-generation
//! invalidation, destructive-operation evidence, and audit attribution.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA_VERSION: u32 = 2;
const LEGACY_SCHEMA_VERSION: u32 = 1;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The durable delegated role held by a Crew identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DelegatedAdminRole {
    /// Operational administration inside one exact Captain-owned ship.
    ShipAdmin,
    /// Fleet-wide Captain administration delegated by Cortana.
    FleetAdmin,
}

impl DelegatedAdminRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::ShipAdmin => "shipAdmin",
            Self::FleetAdmin => "fleetAdmin",
        }
    }
}

/// The only supervisor roles that may originate delegated-administration grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DelegatingSupervisorRole {
    Cortana,
    Captain,
}

impl DelegatingSupervisorRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cortana => "cortana",
            Self::Captain => "captain",
        }
    }
}

/// The authoritative supervisor state supplied by the fleet registry at the time
/// of appointment, revocation, or authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorAuthority {
    pub identity_id: String,
    pub role: DelegatingSupervisorRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_slug: Option<String>,
    pub authority_generation: u64,
    pub active: bool,
}

/// The durable boundary of a delegated grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AdminScope {
    Ship {
        #[serde(rename = "shipSlug", alias = "ship_slug")]
        ship_slug: String,
    },
    Fleet,
}

/// Operations that may appear in an appointment request or an authorization
/// decision.
///
/// The final four variants are deliberately representable but never grantable.
/// Keeping them in the typed operation space makes overgrant attempts explicit and
/// testable instead of depending on string filtering at an outer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdminOperation {
    InspectStatus,
    MaintainSession,
    CleanupSession,
    RecoverResource,
    MaintainWorktree,
    CleanupWorktree,
    PrepareRetirement,
    BuildCrossCaptainReport,
    MaintainFleetResource,
    DirectImplementation,
    GrantAdministrativeRole,
    AssumeCaptainAuthority,
    ApproveGeneralReservedAction,
}

impl AdminOperation {
    pub fn is_destructive(self) -> bool {
        matches!(self, Self::CleanupSession | Self::CleanupWorktree)
    }

    /// Minimum outer control capability needed before delegated authorization is
    /// evaluated.
    pub fn control_access(self) -> ControlAccess {
        match self {
            Self::InspectStatus | Self::BuildCrossCaptainReport => ControlAccess::Read,
            _ => ControlAccess::Mutation,
        }
    }
}

/// The target of an administrative operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AdminTarget {
    Fleet,
    Ship {
        #[serde(rename = "shipSlug", alias = "ship_slug")]
        ship_slug: String,
    },
    Captain {
        #[serde(rename = "shipSlug", alias = "ship_slug")]
        ship_slug: String,
        #[serde(rename = "captainIdentityId", alias = "captain_identity_id")]
        captain_identity_id: String,
    },
    CrewSession {
        #[serde(rename = "shipSlug", alias = "ship_slug")]
        ship_slug: String,
        #[serde(rename = "sessionId", alias = "session_id")]
        session_id: String,
    },
    Worktree {
        #[serde(rename = "shipSlug", alias = "ship_slug")]
        ship_slug: String,
        #[serde(rename = "worktreeId", alias = "worktree_id")]
        worktree_id: String,
    },
    GeneralReserved {
        action: String,
    },
    Implementation {
        #[serde(rename = "shipSlug", alias = "ship_slug")]
        ship_slug: String,
        #[serde(rename = "assignmentId", alias = "assignment_id")]
        assignment_id: String,
    },
}

impl AdminTarget {
    fn ship_slug(&self) -> Option<&str> {
        match self {
            Self::Ship { ship_slug }
            | Self::Captain { ship_slug, .. }
            | Self::CrewSession { ship_slug, .. }
            | Self::Worktree { ship_slug, .. }
            | Self::Implementation { ship_slug, .. } => Some(ship_slug),
            Self::Fleet | Self::GeneralReserved { .. } => None,
        }
    }

    /// Stable exact-target key used to bind approval and safety evidence.
    pub fn fingerprint(&self) -> String {
        match self {
            Self::Fleet => "fleet".into(),
            Self::Ship { ship_slug } => format!("ship:{ship_slug}"),
            Self::Captain {
                ship_slug,
                captain_identity_id,
            } => format!("captain:{ship_slug}:{captain_identity_id}"),
            Self::CrewSession {
                ship_slug,
                session_id,
            } => format!("crewSession:{ship_slug}:{session_id}"),
            Self::Worktree {
                ship_slug,
                worktree_id,
            } => format!("worktree:{ship_slug}:{worktree_id}"),
            Self::GeneralReserved { action } => format!("generalReserved:{action}"),
            Self::Implementation {
                ship_slug,
                assignment_id,
            } => format!("implementation:{ship_slug}:{assignment_id}"),
        }
    }
}

/// A durable reference to the supervisor that created a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatingSupervisor {
    pub identity_id: String,
    pub role: DelegatingSupervisorRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_slug: Option<String>,
    pub authority_generation: u64,
}

impl From<&SupervisorAuthority> for DelegatingSupervisor {
    fn from(authority: &SupervisorAuthority) -> Self {
        Self {
            identity_id: authority.identity_id.clone(),
            role: authority.role,
            ship_slug: authority.ship_slug.clone(),
            authority_generation: authority.authority_generation,
        }
    }
}

/// Active state or a durable revocation tombstone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum GrantState {
    Active,
    Revoked {
        revoked_at: u64,
        revoked_by_identity_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl GrantState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// One persisted appointment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedAdminGrant {
    pub grant_id: String,
    pub grant_generation: u64,
    pub actor_identity_id: String,
    pub role: DelegatedAdminRole,
    pub delegator: DelegatingSupervisor,
    pub scope: AdminScope,
    pub permitted_operations: BTreeSet<AdminOperation>,
    pub granted_at: u64,
    pub state: GrantState,
}

/// Input to an appointment made by a registry-authoritative supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppointmentRequest {
    pub actor_identity_id: String,
    pub role: DelegatedAdminRole,
    pub delegator: SupervisorAuthority,
    pub scope: AdminScope,
    pub permitted_operations: BTreeSet<AdminOperation>,
}

/// The resolved Crew identity that is attempting an administrative operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminActor {
    pub identity_id: String,
    pub session_tile: Option<String>,
}

/// Exact approval required for a destructive delegated operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactApproval {
    pub approval_id: String,
    pub operation: AdminOperation,
    pub target_fingerprint: String,
    pub approved_by_identity_id: String,
    pub supervisor_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ApprovalState {
    Active,
    Consumed {
        consumed_at: u64,
        consumed_by_identity_id: String,
    },
    Invalidated {
        invalidated_at: u64,
        reason: String,
    },
}

impl ApprovalState {
    fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedAdminApproval {
    pub approval: ExactApproval,
    pub grant_id: String,
    pub grant_generation: u64,
    pub actor_identity_id: String,
    pub target: AdminTarget,
    pub issued_at: u64,
    pub state: ApprovalState,
}

/// Authoritative worktree-safety verdict for the exact cleanup target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSafetyEvidence {
    pub evidence_id: String,
    pub target_fingerprint: String,
    pub removable: bool,
}

/// Evidence that must be supplied by the control wiring for destructive actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminSafeguards {
    pub authoritative_ownership_verified: bool,
    pub exact_approval: Option<ExactApproval>,
    pub worktree_safety: Option<WorktreeSafetyEvidence>,
}

/// Audit context returned only after every authorization gate succeeds.
///
/// The acting Crew identity and delegating supervisor are both present so the audit
/// record never collapses delegated execution into an unattributed supervisor action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditContext {
    pub actor_identity_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_session_tile: Option<String>,
    pub delegated_role: DelegatedAdminRole,
    pub delegating_supervisor_identity_id: String,
    pub delegating_supervisor_role: DelegatingSupervisorRole,
    pub grant_id: String,
    pub grant_generation: u64,
    pub scope: AdminScope,
    pub operation: AdminOperation,
    pub target: AdminTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_evidence_id: Option<String>,
}

/// Capability tier on the outer control transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCapability {
    Missing,
    ReadOnly,
    Full,
}

/// Whether the requested control operation only reads state or mutates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAccess {
    Read,
    Mutation,
}

/// Check only whether a control call may enter the surface.
///
/// Successful admission does not authorize any delegated role, operation, or target.
/// Callers must separately invoke [`DelegatedAdminStore::authorize`].
pub fn require_control_capability(
    capability: ControlCapability,
    access: ControlAccess,
) -> Result<(), DelegatedAdminError> {
    match (capability, access) {
        (ControlCapability::Full, _) | (ControlCapability::ReadOnly, ControlAccess::Read) => Ok(()),
        (ControlCapability::ReadOnly, ControlAccess::Mutation) => {
            Err(DelegatedAdminError::InsufficientControlCapability)
        }
        (ControlCapability::Missing, _) => Err(DelegatedAdminError::MissingControlCapability),
    }
}

/// Structured delegated-administration denial or persistence failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegatedAdminError {
    MissingControlCapability,
    InsufficientControlCapability,
    InvalidAppointment(String),
    ActiveGrantExists(String),
    GrantNotFound(String),
    GrantRevoked(String),
    ApprovalNotFound(String),
    ApprovalUsed(String),
    ApprovalMismatch(String),
    NoActiveGrant(String),
    SupervisorInactive(String),
    SupervisorMismatch(String),
    AuthorityGenerationMismatch { expected: u64, actual: u64 },
    OperationForbidden(AdminOperation),
    OperationNotGranted(AdminOperation),
    TargetOutOfScope(String),
    MissingAuthoritativeOwnership,
    MissingExactApproval,
    InvalidExactApproval(String),
    MissingWorktreeSafetyEvidence,
    UnsafeWorktree(String),
    CorruptState(String),
    UnsupportedSchema(u32),
    Io(String),
    Persistence(String),
}

impl DelegatedAdminError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingControlCapability => "missingControlCapability",
            Self::InsufficientControlCapability => "insufficientControlCapability",
            Self::InvalidAppointment(_) => "invalidAppointment",
            Self::ActiveGrantExists(_) => "activeGrantExists",
            Self::GrantNotFound(_) => "grantNotFound",
            Self::GrantRevoked(_) => "grantRevoked",
            Self::ApprovalNotFound(_) => "approvalNotFound",
            Self::ApprovalUsed(_) => "approvalUsed",
            Self::ApprovalMismatch(_) => "approvalMismatch",
            Self::NoActiveGrant(_) => "noActiveGrant",
            Self::SupervisorInactive(_) => "supervisorInactive",
            Self::SupervisorMismatch(_) => "supervisorMismatch",
            Self::AuthorityGenerationMismatch { .. } => "authorityGenerationMismatch",
            Self::OperationForbidden(_) => "operationForbidden",
            Self::OperationNotGranted(_) => "operationNotGranted",
            Self::TargetOutOfScope(_) => "targetOutOfScope",
            Self::MissingAuthoritativeOwnership => "missingAuthoritativeOwnership",
            Self::MissingExactApproval => "missingExactApproval",
            Self::InvalidExactApproval(_) => "invalidExactApproval",
            Self::MissingWorktreeSafetyEvidence => "missingWorktreeSafetyEvidence",
            Self::UnsafeWorktree(_) => "unsafeWorktree",
            Self::CorruptState(_) => "corruptState",
            Self::UnsupportedSchema(_) => "unsupportedSchema",
            Self::Io(_) => "io",
            Self::Persistence(_) => "persistence",
        }
    }
}

impl std::fmt::Display for DelegatedAdminError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingControlCapability => write!(formatter, "control capability is missing"),
            Self::InsufficientControlCapability => {
                write!(
                    formatter,
                    "read-only control capability cannot mutate state"
                )
            }
            Self::InvalidAppointment(reason) => write!(formatter, "invalid appointment: {reason}"),
            Self::ActiveGrantExists(actor) => {
                write!(
                    formatter,
                    "actor '{actor}' already has an active delegated grant"
                )
            }
            Self::GrantNotFound(grant) => write!(formatter, "grant '{grant}' was not found"),
            Self::GrantRevoked(grant) => write!(formatter, "grant '{grant}' is revoked"),
            Self::ApprovalNotFound(approval) => {
                write!(formatter, "approval '{approval}' was not found")
            }
            Self::ApprovalUsed(approval) => {
                write!(formatter, "approval '{approval}' is no longer active")
            }
            Self::ApprovalMismatch(reason) => {
                write!(formatter, "approval does not match: {reason}")
            }
            Self::NoActiveGrant(actor) => {
                write!(formatter, "actor '{actor}' has no active delegated grant")
            }
            Self::SupervisorInactive(identity) => {
                write!(
                    formatter,
                    "delegating supervisor '{identity}' is not active"
                )
            }
            Self::SupervisorMismatch(reason) => write!(formatter, "supervisor mismatch: {reason}"),
            Self::AuthorityGenerationMismatch { expected, actual } => write!(
                formatter,
                "supervisor authority generation mismatch: expected {expected}, got {actual}"
            ),
            Self::OperationForbidden(operation) => {
                write!(formatter, "operation {operation:?} cannot be delegated")
            }
            Self::OperationNotGranted(operation) => {
                write!(
                    formatter,
                    "operation {operation:?} is not present in the grant"
                )
            }
            Self::TargetOutOfScope(reason) => write!(formatter, "target is out of scope: {reason}"),
            Self::MissingAuthoritativeOwnership => {
                write!(formatter, "authoritative ownership evidence is required")
            }
            Self::MissingExactApproval => write!(formatter, "exact approval is required"),
            Self::InvalidExactApproval(reason) => {
                write!(formatter, "invalid exact approval: {reason}")
            }
            Self::MissingWorktreeSafetyEvidence => {
                write!(
                    formatter,
                    "authoritative worktree safety evidence is required"
                )
            }
            Self::UnsafeWorktree(target) => {
                write!(
                    formatter,
                    "worktree safety service did not approve '{target}' for removal"
                )
            }
            Self::CorruptState(reason) => {
                write!(formatter, "delegated-admin state is corrupt: {reason}")
            }
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported delegated-admin schema version {version}"
                )
            }
            Self::Io(reason) => write!(formatter, "delegated-admin I/O failed: {reason}"),
            Self::Persistence(reason) => {
                write!(formatter, "delegated-admin persistence failed: {reason}")
            }
        }
    }
}

impl std::error::Error for DelegatedAdminError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DelegatedAdminSnapshot {
    schema_version: u32,
    next_generation: u64,
    grants: BTreeMap<String, DelegatedAdminGrant>,
    #[serde(default)]
    approvals: BTreeMap<String, DelegatedAdminApproval>,
}

impl Default for DelegatedAdminSnapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            next_generation: 1,
            grants: BTreeMap::new(),
            approvals: BTreeMap::new(),
        }
    }
}

/// Durable delegated-administration registry.
#[derive(Debug)]
pub struct DelegatedAdminStore {
    path: Option<PathBuf>,
    inner: Mutex<DelegatedAdminSnapshot>,
}

impl DelegatedAdminStore {
    /// Load a store and fail closed if durable state exists but cannot be read,
    /// decoded, or validated.
    pub fn load(path: PathBuf) -> Result<Self, DelegatedAdminError> {
        let mut snapshot = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<DelegatedAdminSnapshot>(&bytes).map_err(|error| {
                    DelegatedAdminError::CorruptState(format!("{}: {error}", path.display()))
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DelegatedAdminSnapshot::default()
            }
            Err(error) => {
                return Err(DelegatedAdminError::Io(format!(
                    "{}: {error}",
                    path.display()
                )))
            }
        };
        if snapshot.schema_version == LEGACY_SCHEMA_VERSION {
            snapshot.schema_version = SCHEMA_VERSION;
        }
        validate_snapshot(&snapshot)?;
        Ok(Self {
            path: Some(path),
            inner: Mutex::new(snapshot),
        })
    }

    pub fn load_default() -> Result<Self, DelegatedAdminError> {
        Self::load(default_store_path())
    }

    pub fn ephemeral() -> Self {
        Self {
            path: None,
            inner: Mutex::new(DelegatedAdminSnapshot::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DelegatedAdminSnapshot> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn persist(&self, snapshot: &DelegatedAdminSnapshot) -> Result<(), DelegatedAdminError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_atomic(path, snapshot).map_err(|error| {
            DelegatedAdminError::Persistence(format!("{}: {error}", path.display()))
        })
    }

    /// Appoint one Crew identity to a durable administrative role.
    ///
    /// The caller must supply supervisor identity and generation from the
    /// authoritative fleet registry, never from request JSON.
    pub fn appoint(
        &self,
        request: AppointmentRequest,
    ) -> Result<DelegatedAdminGrant, DelegatedAdminError> {
        validate_appointment(&request)?;
        let mut snapshot = self.lock();
        if snapshot.grants.values().any(|grant| {
            grant.actor_identity_id == request.actor_identity_id && grant.state.is_active()
        }) {
            return Err(DelegatedAdminError::ActiveGrantExists(
                request.actor_identity_id,
            ));
        }

        let previous = snapshot.clone();
        let grant = DelegatedAdminGrant {
            grant_id: uuid::Uuid::new_v4().simple().to_string(),
            grant_generation: snapshot.next_generation,
            actor_identity_id: request.actor_identity_id,
            role: request.role,
            delegator: DelegatingSupervisor::from(&request.delegator),
            scope: request.scope,
            permitted_operations: request.permitted_operations,
            granted_at: now_ms(),
            state: GrantState::Active,
        };
        snapshot.next_generation = snapshot.next_generation.saturating_add(1);
        if snapshot.next_generation == grant.grant_generation {
            return Err(DelegatedAdminError::CorruptState(
                "grant generation exhausted".into(),
            ));
        }
        snapshot
            .grants
            .insert(grant.grant_id.clone(), grant.clone());
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        Ok(grant)
    }

    /// Revoke a grant while retaining a durable tombstone.
    pub fn revoke(
        &self,
        grant_id: &str,
        supervisor: &SupervisorAuthority,
        reason: Option<String>,
    ) -> Result<DelegatedAdminGrant, DelegatedAdminError> {
        let mut snapshot = self.lock();
        let existing = snapshot
            .grants
            .get(grant_id)
            .cloned()
            .ok_or_else(|| DelegatedAdminError::GrantNotFound(grant_id.into()))?;
        validate_current_supervisor(&existing, supervisor)?;
        if !existing.state.is_active() {
            return Ok(existing);
        }

        let previous = snapshot.clone();
        let result = {
            let grant = snapshot
                .grants
                .get_mut(grant_id)
                .expect("grant was resolved under the same lock");
            grant.state = GrantState::Revoked {
                revoked_at: now_ms(),
                revoked_by_identity_id: supervisor.identity_id.clone(),
                reason,
            };
            grant.clone()
        };
        for approval in snapshot
            .approvals
            .values_mut()
            .filter(|approval| approval.grant_id == grant_id && approval.state.is_active())
        {
            approval.state = ApprovalState::Invalidated {
                invalidated_at: now_ms(),
                reason: "delegated grant revoked".into(),
            };
        }
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        Ok(result)
    }

    pub fn get(&self, grant_id: &str) -> Option<DelegatedAdminGrant> {
        self.lock().grants.get(grant_id).cloned()
    }

    pub fn grants_for_actor(&self, actor_identity_id: &str) -> Vec<DelegatedAdminGrant> {
        self.lock()
            .grants
            .values()
            .filter(|grant| grant.actor_identity_id == actor_identity_id)
            .cloned()
            .collect()
    }

    pub fn grants_delegated_by(&self, supervisor_identity_id: &str) -> Vec<DelegatedAdminGrant> {
        self.lock()
            .grants
            .values()
            .filter(|grant| grant.delegator.identity_id == supervisor_identity_id)
            .cloned()
            .collect()
    }

    pub fn active_grants(&self) -> Vec<DelegatedAdminGrant> {
        self.lock()
            .grants
            .values()
            .filter(|grant| grant.state.is_active())
            .cloned()
            .collect()
    }

    pub fn get_approval(&self, approval_id: &str) -> Option<DelegatedAdminApproval> {
        self.lock().approvals.get(approval_id).cloned()
    }

    pub fn issue_exact_approval(
        &self,
        grant_id: &str,
        supervisor: &SupervisorAuthority,
        operation: AdminOperation,
        target: &AdminTarget,
    ) -> Result<DelegatedAdminApproval, DelegatedAdminError> {
        if !operation.is_destructive() {
            return Err(DelegatedAdminError::InvalidAppointment(
                "exact approvals are only valid for destructive delegated operations".into(),
            ));
        }
        let mut snapshot = self.lock();
        let grant = snapshot
            .grants
            .get(grant_id)
            .cloned()
            .ok_or_else(|| DelegatedAdminError::GrantNotFound(grant_id.into()))?;
        if !grant.state.is_active() {
            return Err(DelegatedAdminError::GrantRevoked(grant_id.into()));
        }
        validate_current_supervisor(&grant, supervisor)?;
        if !operation_allowed_for_role(grant.role, operation) {
            return Err(DelegatedAdminError::OperationForbidden(operation));
        }
        if !grant.permitted_operations.contains(&operation) {
            return Err(DelegatedAdminError::OperationNotGranted(operation));
        }
        validate_target(grant.role, &grant.scope, operation, target)?;

        let previous = snapshot.clone();
        let approval = ExactApproval {
            approval_id: uuid::Uuid::new_v4().simple().to_string(),
            operation,
            target_fingerprint: target.fingerprint(),
            approved_by_identity_id: supervisor.identity_id.clone(),
            supervisor_generation: supervisor.authority_generation,
        };
        let record = DelegatedAdminApproval {
            approval: approval.clone(),
            grant_id: grant.grant_id,
            grant_generation: grant.grant_generation,
            actor_identity_id: grant.actor_identity_id,
            target: target.clone(),
            issued_at: now_ms(),
            state: ApprovalState::Active,
        };
        snapshot
            .approvals
            .insert(approval.approval_id.clone(), record.clone());
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        Ok(record)
    }

    pub fn consume_exact_approval(
        &self,
        approval_id: &str,
        actor_identity_id: &str,
        current_supervisor: &SupervisorAuthority,
        operation: AdminOperation,
        target: &AdminTarget,
    ) -> Result<ExactApproval, DelegatedAdminError> {
        let mut snapshot = self.lock();
        let record = snapshot
            .approvals
            .get(approval_id)
            .cloned()
            .ok_or_else(|| DelegatedAdminError::ApprovalNotFound(approval_id.into()))?;
        if !record.state.is_active() {
            return Err(DelegatedAdminError::ApprovalUsed(approval_id.into()));
        }
        let grant = snapshot
            .grants
            .get(&record.grant_id)
            .cloned()
            .ok_or_else(|| DelegatedAdminError::GrantNotFound(record.grant_id.clone()))?;
        if !grant.state.is_active() {
            return Err(DelegatedAdminError::GrantRevoked(grant.grant_id));
        }
        validate_current_supervisor(&grant, current_supervisor)?;
        if record.grant_generation != grant.grant_generation
            || record.actor_identity_id != actor_identity_id
            || record.target != *target
            || record.approval.operation != operation
            || record.approval.target_fingerprint != target.fingerprint()
            || record.approval.approved_by_identity_id != current_supervisor.identity_id
            || record.approval.supervisor_generation != current_supervisor.authority_generation
        {
            return Err(DelegatedAdminError::ApprovalMismatch(
                "grant, actor, operation, target, supervisor, and generation must all match".into(),
            ));
        }
        if !operation_allowed_for_role(grant.role, operation) {
            return Err(DelegatedAdminError::OperationForbidden(operation));
        }
        if !grant.permitted_operations.contains(&operation) {
            return Err(DelegatedAdminError::OperationNotGranted(operation));
        }
        validate_target(grant.role, &grant.scope, operation, target)?;

        let previous = snapshot.clone();
        snapshot
            .approvals
            .get_mut(approval_id)
            .expect("approval was resolved under the same lock")
            .state = ApprovalState::Consumed {
            consumed_at: now_ms(),
            consumed_by_identity_id: actor_identity_id.into(),
        };
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        Ok(record.approval)
    }

    /// Resolve and authorize an effective delegated grant.
    ///
    /// This method intentionally accepts no control token or capability tier.
    /// Transport admission is a separate gate through [`require_control_capability`].
    pub fn authorize(
        &self,
        actor: &AdminActor,
        current_supervisor: &SupervisorAuthority,
        operation: AdminOperation,
        target: &AdminTarget,
        safeguards: &AdminSafeguards,
    ) -> Result<AdminAuditContext, DelegatedAdminError> {
        let snapshot = self.lock();
        let grant = snapshot
            .grants
            .values()
            .find(|grant| grant.actor_identity_id == actor.identity_id && grant.state.is_active())
            .cloned();
        let Some(grant) = grant else {
            let revoked = snapshot
                .grants
                .values()
                .filter(|grant| grant.actor_identity_id == actor.identity_id)
                .max_by_key(|grant| grant.grant_generation);
            return match revoked {
                Some(grant) => Err(DelegatedAdminError::GrantRevoked(grant.grant_id.clone())),
                None => Err(DelegatedAdminError::NoActiveGrant(
                    actor.identity_id.clone(),
                )),
            };
        };
        drop(snapshot);

        validate_current_supervisor(&grant, current_supervisor)?;
        if !operation_allowed_for_role(grant.role, operation) {
            return Err(DelegatedAdminError::OperationForbidden(operation));
        }
        if !grant.permitted_operations.contains(&operation) {
            return Err(DelegatedAdminError::OperationNotGranted(operation));
        }
        validate_target(grant.role, &grant.scope, operation, target)?;
        validate_safeguards(operation, target, current_supervisor, safeguards)?;

        Ok(AdminAuditContext {
            actor_identity_id: actor.identity_id.clone(),
            actor_session_tile: actor.session_tile.clone(),
            delegated_role: grant.role,
            delegating_supervisor_identity_id: grant.delegator.identity_id.clone(),
            delegating_supervisor_role: grant.delegator.role,
            grant_id: grant.grant_id,
            grant_generation: grant.grant_generation,
            scope: grant.scope,
            operation,
            target: target.clone(),
            exact_approval_id: safeguards
                .exact_approval
                .as_ref()
                .map(|approval| approval.approval_id.clone()),
            safety_evidence_id: safeguards
                .worktree_safety
                .as_ref()
                .map(|evidence| evidence.evidence_id.clone()),
        })
    }
}

fn validate_appointment(request: &AppointmentRequest) -> Result<(), DelegatedAdminError> {
    if request.actor_identity_id.trim().is_empty() {
        return Err(DelegatedAdminError::InvalidAppointment(
            "actor identity is required".into(),
        ));
    }
    if !request.delegator.active {
        return Err(DelegatedAdminError::SupervisorInactive(
            request.delegator.identity_id.clone(),
        ));
    }
    if request.delegator.identity_id.trim().is_empty() {
        return Err(DelegatedAdminError::InvalidAppointment(
            "delegating supervisor identity is required".into(),
        ));
    }
    if request.actor_identity_id == request.delegator.identity_id {
        return Err(DelegatedAdminError::InvalidAppointment(
            "a supervisor cannot appoint itself as delegated Crew".into(),
        ));
    }
    match (&request.role, &request.delegator.role, &request.scope) {
        (
            DelegatedAdminRole::ShipAdmin,
            DelegatingSupervisorRole::Captain,
            AdminScope::Ship { ship_slug },
        ) if request.delegator.ship_slug.as_deref() == Some(ship_slug.as_str())
            && !ship_slug.trim().is_empty() => {}
        (DelegatedAdminRole::FleetAdmin, DelegatingSupervisorRole::Cortana, AdminScope::Fleet) => {}
        (DelegatedAdminRole::ShipAdmin, _, _) => {
            return Err(DelegatedAdminError::InvalidAppointment(
                "Ship Admin requires its owning Captain and that exact ship scope".into(),
            ))
        }
        (DelegatedAdminRole::FleetAdmin, _, _) => {
            return Err(DelegatedAdminError::InvalidAppointment(
                "Fleet Admin requires Cortana and fleet scope".into(),
            ))
        }
    }
    if request.permitted_operations.is_empty() {
        return Err(DelegatedAdminError::InvalidAppointment(
            "at least one permitted operation is required".into(),
        ));
    }
    for operation in &request.permitted_operations {
        if !operation_allowed_for_role(request.role, *operation) {
            return Err(DelegatedAdminError::OperationForbidden(*operation));
        }
    }
    Ok(())
}

fn validate_current_supervisor(
    grant: &DelegatedAdminGrant,
    current: &SupervisorAuthority,
) -> Result<(), DelegatedAdminError> {
    if !current.active {
        return Err(DelegatedAdminError::SupervisorInactive(
            current.identity_id.clone(),
        ));
    }
    if current.identity_id != grant.delegator.identity_id {
        return Err(DelegatedAdminError::SupervisorMismatch(
            "identity changed or ownership transferred".into(),
        ));
    }
    if current.role != grant.delegator.role || current.ship_slug != grant.delegator.ship_slug {
        return Err(DelegatedAdminError::SupervisorMismatch(
            "role or ship ownership changed".into(),
        ));
    }
    if current.authority_generation != grant.delegator.authority_generation {
        return Err(DelegatedAdminError::AuthorityGenerationMismatch {
            expected: grant.delegator.authority_generation,
            actual: current.authority_generation,
        });
    }
    Ok(())
}

fn operation_allowed_for_role(role: DelegatedAdminRole, operation: AdminOperation) -> bool {
    match role {
        DelegatedAdminRole::ShipAdmin => matches!(
            operation,
            AdminOperation::InspectStatus
                | AdminOperation::MaintainSession
                | AdminOperation::CleanupSession
                | AdminOperation::RecoverResource
                | AdminOperation::MaintainWorktree
                | AdminOperation::CleanupWorktree
                | AdminOperation::PrepareRetirement
        ),
        DelegatedAdminRole::FleetAdmin => matches!(
            operation,
            AdminOperation::InspectStatus
                | AdminOperation::MaintainSession
                | AdminOperation::CleanupSession
                | AdminOperation::RecoverResource
                | AdminOperation::PrepareRetirement
                | AdminOperation::BuildCrossCaptainReport
                | AdminOperation::MaintainFleetResource
        ),
    }
}

fn validate_target(
    role: DelegatedAdminRole,
    scope: &AdminScope,
    operation: AdminOperation,
    target: &AdminTarget,
) -> Result<(), DelegatedAdminError> {
    if matches!(target, AdminTarget::GeneralReserved { .. }) {
        return Err(DelegatedAdminError::TargetOutOfScope(
            "General-reserved actions cannot be delegated".into(),
        ));
    }
    if matches!(target, AdminTarget::Implementation { .. }) {
        return Err(DelegatedAdminError::TargetOutOfScope(
            "implementation direction remains with supervisors".into(),
        ));
    }

    match (role, scope) {
        (DelegatedAdminRole::ShipAdmin, AdminScope::Ship { ship_slug }) => {
            if target.ship_slug() != Some(ship_slug.as_str()) {
                return Err(DelegatedAdminError::TargetOutOfScope(format!(
                    "Ship Admin is limited to ship '{ship_slug}'"
                )));
            }
            let compatible = match operation {
                AdminOperation::InspectStatus | AdminOperation::RecoverResource => matches!(
                    target,
                    AdminTarget::Ship { .. }
                        | AdminTarget::Captain { .. }
                        | AdminTarget::CrewSession { .. }
                        | AdminTarget::Worktree { .. }
                ),
                AdminOperation::MaintainSession | AdminOperation::CleanupSession => matches!(
                    target,
                    AdminTarget::Captain { .. } | AdminTarget::CrewSession { .. }
                ),
                AdminOperation::MaintainWorktree | AdminOperation::CleanupWorktree => {
                    matches!(target, AdminTarget::Worktree { .. })
                }
                AdminOperation::PrepareRetirement => matches!(
                    target,
                    AdminTarget::Ship { .. }
                        | AdminTarget::Captain { .. }
                        | AdminTarget::CrewSession { .. }
                        | AdminTarget::Worktree { .. }
                ),
                _ => false,
            };
            if compatible {
                Ok(())
            } else {
                Err(DelegatedAdminError::TargetOutOfScope(format!(
                    "operation {operation:?} is not valid for target {}",
                    target.fingerprint()
                )))
            }
        }
        (DelegatedAdminRole::FleetAdmin, AdminScope::Fleet) => {
            let compatible = match operation {
                AdminOperation::InspectStatus | AdminOperation::RecoverResource => matches!(
                    target,
                    AdminTarget::Fleet | AdminTarget::Ship { .. } | AdminTarget::Captain { .. }
                ),
                AdminOperation::MaintainSession | AdminOperation::CleanupSession => {
                    matches!(target, AdminTarget::Captain { .. })
                }
                AdminOperation::PrepareRetirement => {
                    matches!(
                        target,
                        AdminTarget::Ship { .. } | AdminTarget::Captain { .. }
                    )
                }
                AdminOperation::BuildCrossCaptainReport => matches!(target, AdminTarget::Fleet),
                AdminOperation::MaintainFleetResource => matches!(
                    target,
                    AdminTarget::Fleet | AdminTarget::Ship { .. } | AdminTarget::Captain { .. }
                ),
                _ => false,
            };
            if compatible {
                Ok(())
            } else {
                Err(DelegatedAdminError::TargetOutOfScope(format!(
                    "Fleet Admin may administer Captains but not target {}",
                    target.fingerprint()
                )))
            }
        }
        _ => Err(DelegatedAdminError::CorruptState(
            "grant role and scope are inconsistent".into(),
        )),
    }
}

fn validate_safeguards(
    operation: AdminOperation,
    target: &AdminTarget,
    supervisor: &SupervisorAuthority,
    safeguards: &AdminSafeguards,
) -> Result<(), DelegatedAdminError> {
    if !operation.is_destructive() {
        return Ok(());
    }
    if !safeguards.authoritative_ownership_verified {
        return Err(DelegatedAdminError::MissingAuthoritativeOwnership);
    }
    let approval = safeguards
        .exact_approval
        .as_ref()
        .ok_or(DelegatedAdminError::MissingExactApproval)?;
    if approval.approval_id.trim().is_empty()
        || approval.operation != operation
        || approval.target_fingerprint != target.fingerprint()
        || approval.approved_by_identity_id != supervisor.identity_id
        || approval.supervisor_generation != supervisor.authority_generation
    {
        return Err(DelegatedAdminError::InvalidExactApproval(
            "approval must bind the operation, exact target, supervisor, and generation".into(),
        ));
    }
    if operation == AdminOperation::CleanupWorktree {
        let evidence = safeguards
            .worktree_safety
            .as_ref()
            .ok_or(DelegatedAdminError::MissingWorktreeSafetyEvidence)?;
        if evidence.evidence_id.trim().is_empty()
            || evidence.target_fingerprint != target.fingerprint()
            || !evidence.removable
        {
            return Err(DelegatedAdminError::UnsafeWorktree(target.fingerprint()));
        }
    }
    Ok(())
}

fn validate_snapshot(snapshot: &DelegatedAdminSnapshot) -> Result<(), DelegatedAdminError> {
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(DelegatedAdminError::UnsupportedSchema(
            snapshot.schema_version,
        ));
    }
    let mut generations = BTreeSet::new();
    let mut active_actors = BTreeSet::new();
    let mut max_generation = 0;
    for (key, grant) in &snapshot.grants {
        if key != &grant.grant_id || grant.grant_id.trim().is_empty() {
            return Err(DelegatedAdminError::CorruptState(
                "grant map key does not match grant id".into(),
            ));
        }
        if grant.actor_identity_id.trim().is_empty()
            || grant.delegator.identity_id.trim().is_empty()
            || grant.permitted_operations.is_empty()
        {
            return Err(DelegatedAdminError::CorruptState(format!(
                "grant '{}' has an empty required field",
                grant.grant_id
            )));
        }
        if !generations.insert(grant.grant_generation) {
            return Err(DelegatedAdminError::CorruptState(
                "grant generations are not unique".into(),
            ));
        }
        max_generation = max_generation.max(grant.grant_generation);
        if grant.state.is_active() && !active_actors.insert(grant.actor_identity_id.clone()) {
            return Err(DelegatedAdminError::CorruptState(format!(
                "actor '{}' has multiple active grants",
                grant.actor_identity_id
            )));
        }
        let request = AppointmentRequest {
            actor_identity_id: grant.actor_identity_id.clone(),
            role: grant.role,
            delegator: SupervisorAuthority {
                identity_id: grant.delegator.identity_id.clone(),
                role: grant.delegator.role,
                ship_slug: grant.delegator.ship_slug.clone(),
                authority_generation: grant.delegator.authority_generation,
                active: true,
            },
            scope: grant.scope.clone(),
            permitted_operations: grant.permitted_operations.clone(),
        };
        validate_appointment(&request).map_err(|error| {
            DelegatedAdminError::CorruptState(format!(
                "grant '{}' failed validation: {error}",
                grant.grant_id
            ))
        })?;
    }
    for (key, approval) in &snapshot.approvals {
        if key != &approval.approval.approval_id || key.trim().is_empty() {
            return Err(DelegatedAdminError::CorruptState(
                "approval map key does not match approval id".into(),
            ));
        }
        let grant = snapshot.grants.get(&approval.grant_id).ok_or_else(|| {
            DelegatedAdminError::CorruptState(format!(
                "approval '{}' references an unknown grant",
                approval.approval.approval_id
            ))
        })?;
        if approval.grant_generation != grant.grant_generation
            || approval.actor_identity_id != grant.actor_identity_id
            || !approval.approval.operation.is_destructive()
            || approval.approval.target_fingerprint != approval.target.fingerprint()
            || approval.approval.approved_by_identity_id != grant.delegator.identity_id
            || approval.approval.supervisor_generation != grant.delegator.authority_generation
        {
            return Err(DelegatedAdminError::CorruptState(format!(
                "approval '{}' does not match its grant and exact target",
                approval.approval.approval_id
            )));
        }
        if !operation_allowed_for_role(grant.role, approval.approval.operation)
            || !grant
                .permitted_operations
                .contains(&approval.approval.operation)
        {
            return Err(DelegatedAdminError::CorruptState(format!(
                "approval '{}' names an operation outside its grant",
                approval.approval.approval_id
            )));
        }
        validate_target(
            grant.role,
            &grant.scope,
            approval.approval.operation,
            &approval.target,
        )
        .map_err(|error| {
            DelegatedAdminError::CorruptState(format!(
                "approval '{}' has an invalid target: {error}",
                approval.approval.approval_id
            ))
        })?;
    }
    if snapshot.next_generation <= max_generation {
        return Err(DelegatedAdminError::CorruptState(
            "next grant generation does not advance past persisted grants".into(),
        ));
    }
    Ok(())
}

fn default_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("T_HUB_DELEGATED_ADMIN_FILE") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("delegated-admin.json")
}

fn write_atomic(path: &Path, snapshot: &DelegatedAdminSnapshot) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(snapshot)?;
    let temp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)?;
        #[cfg(unix)]
        {
            // The rename is already committed at this point. Directory sync improves
            // crash durability, but a sync failure must not make the caller roll back
            // memory while the new snapshot is already visible on disk.
            let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operations(items: &[AdminOperation]) -> BTreeSet<AdminOperation> {
        items.iter().copied().collect()
    }

    fn captain(ship: &str, generation: u64) -> SupervisorAuthority {
        SupervisorAuthority {
            identity_id: format!("captain-{ship}"),
            role: DelegatingSupervisorRole::Captain,
            ship_slug: Some(ship.into()),
            authority_generation: generation,
            active: true,
        }
    }

    fn cortana(generation: u64) -> SupervisorAuthority {
        SupervisorAuthority {
            identity_id: "cortana".into(),
            role: DelegatingSupervisorRole::Cortana,
            ship_slug: Some("cortana".into()),
            authority_generation: generation,
            active: true,
        }
    }

    fn actor(identity: &str) -> AdminActor {
        AdminActor {
            identity_id: identity.into(),
            session_tile: Some(format!("tile-{identity}")),
        }
    }

    fn ship_appointment(
        actor_identity_id: &str,
        ship: &str,
        permitted_operations: BTreeSet<AdminOperation>,
    ) -> AppointmentRequest {
        AppointmentRequest {
            actor_identity_id: actor_identity_id.into(),
            role: DelegatedAdminRole::ShipAdmin,
            delegator: captain(ship, 7),
            scope: AdminScope::Ship {
                ship_slug: ship.into(),
            },
            permitted_operations,
        }
    }

    fn fleet_appointment(
        actor_identity_id: &str,
        permitted_operations: BTreeSet<AdminOperation>,
    ) -> AppointmentRequest {
        AppointmentRequest {
            actor_identity_id: actor_identity_id.into(),
            role: DelegatedAdminRole::FleetAdmin,
            delegator: cortana(11),
            scope: AdminScope::Fleet,
            permitted_operations,
        }
    }

    #[test]
    fn control_capability_admits_calls_but_does_not_create_role_authority() {
        assert!(
            require_control_capability(ControlCapability::Full, ControlAccess::Mutation).is_ok()
        );
        assert_eq!(
            require_control_capability(ControlCapability::ReadOnly, ControlAccess::Mutation)
                .unwrap_err()
                .code(),
            "insufficientControlCapability"
        );
        let store = DelegatedAdminStore::ephemeral();
        let denied = store
            .authorize(
                &actor("crew-a"),
                &captain("alpha", 7),
                AdminOperation::InspectStatus,
                &AdminTarget::Ship {
                    ship_slug: "alpha".into(),
                },
                &AdminSafeguards::default(),
            )
            .unwrap_err();
        assert_eq!(denied.code(), "noActiveGrant");
    }

    #[test]
    fn ship_admin_is_limited_to_its_exact_ship() {
        let store = DelegatedAdminStore::ephemeral();
        store
            .appoint(ship_appointment(
                "admin-a",
                "alpha",
                operations(&[AdminOperation::InspectStatus]),
            ))
            .unwrap();
        let admin = actor("admin-a");
        let authority = captain("alpha", 7);

        for target in [
            AdminTarget::Ship {
                ship_slug: "alpha".into(),
            },
            AdminTarget::Captain {
                ship_slug: "alpha".into(),
                captain_identity_id: "captain-alpha".into(),
            },
            AdminTarget::CrewSession {
                ship_slug: "alpha".into(),
                session_id: "crew-alpha".into(),
            },
        ] {
            assert!(store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::InspectStatus,
                    &target,
                    &AdminSafeguards::default(),
                )
                .is_ok());
        }

        for target in [
            AdminTarget::CrewSession {
                ship_slug: "beta".into(),
                session_id: "sibling-ship-crew".into(),
            },
            AdminTarget::CrewSession {
                ship_slug: "foreign".into(),
                session_id: "foreign-ship-crew".into(),
            },
        ] {
            assert_eq!(
                store
                    .authorize(
                        &admin,
                        &authority,
                        AdminOperation::InspectStatus,
                        &target,
                        &AdminSafeguards::default(),
                    )
                    .unwrap_err()
                    .code(),
                "targetOutOfScope"
            );
        }
    }

    #[test]
    fn fleet_admin_can_administer_captains_but_not_crew_or_implementation() {
        let store = DelegatedAdminStore::ephemeral();
        store
            .appoint(fleet_appointment(
                "fleet-a",
                operations(&[
                    AdminOperation::InspectStatus,
                    AdminOperation::MaintainSession,
                    AdminOperation::BuildCrossCaptainReport,
                ]),
            ))
            .unwrap();
        let admin = actor("fleet-a");
        let authority = cortana(11);
        assert!(store
            .authorize(
                &admin,
                &authority,
                AdminOperation::MaintainSession,
                &AdminTarget::Captain {
                    ship_slug: "alpha".into(),
                    captain_identity_id: "captain-alpha".into(),
                },
                &AdminSafeguards::default(),
            )
            .is_ok());
        assert!(store
            .authorize(
                &admin,
                &authority,
                AdminOperation::BuildCrossCaptainReport,
                &AdminTarget::Fleet,
                &AdminSafeguards::default(),
            )
            .is_ok());
        assert_eq!(
            store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::MaintainSession,
                    &AdminTarget::CrewSession {
                        ship_slug: "alpha".into(),
                        session_id: "crew-alpha".into(),
                    },
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "targetOutOfScope"
        );
        assert_eq!(
            store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::MaintainFleetResource,
                    &AdminTarget::Fleet,
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "operationNotGranted"
        );
        assert_eq!(
            store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::InspectStatus,
                    &AdminTarget::Implementation {
                        ship_slug: "alpha".into(),
                        assignment_id: "feature".into(),
                    },
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "targetOutOfScope"
        );
    }

    #[test]
    fn appointment_rejects_wrong_delegator_scope_and_overgrant() {
        let store = DelegatedAdminStore::ephemeral();
        let mut wrong_delegator = ship_appointment(
            "admin-a",
            "alpha",
            operations(&[AdminOperation::InspectStatus]),
        );
        wrong_delegator.delegator = cortana(7);
        assert_eq!(
            store.appoint(wrong_delegator).unwrap_err().code(),
            "invalidAppointment"
        );

        let mut cross_ship = ship_appointment(
            "admin-b",
            "alpha",
            operations(&[AdminOperation::InspectStatus]),
        );
        cross_ship.scope = AdminScope::Ship {
            ship_slug: "beta".into(),
        };
        assert_eq!(
            store.appoint(cross_ship).unwrap_err().code(),
            "invalidAppointment"
        );

        for forbidden in [
            AdminOperation::DirectImplementation,
            AdminOperation::GrantAdministrativeRole,
            AdminOperation::AssumeCaptainAuthority,
            AdminOperation::ApproveGeneralReservedAction,
        ] {
            assert_eq!(
                store
                    .appoint(ship_appointment(
                        &format!("admin-{forbidden:?}"),
                        "alpha",
                        operations(&[forbidden]),
                    ))
                    .unwrap_err()
                    .code(),
                "operationForbidden"
            );
            assert_eq!(
                store
                    .appoint(fleet_appointment(
                        &format!("fleet-{forbidden:?}"),
                        operations(&[forbidden]),
                    ))
                    .unwrap_err()
                    .code(),
                "operationForbidden"
            );
        }
    }

    #[test]
    fn revocation_tombstone_survives_reload_and_generation_advances() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delegated-admin.json");
        let first = {
            let store = DelegatedAdminStore::load(path.clone()).unwrap();
            let grant = store
                .appoint(ship_appointment(
                    "admin-a",
                    "alpha",
                    operations(&[AdminOperation::InspectStatus]),
                ))
                .unwrap();
            let revoked = store
                .revoke(
                    &grant.grant_id,
                    &captain("alpha", 7),
                    Some("rotation".into()),
                )
                .unwrap();
            assert!(!revoked.state.is_active());
            grant
        };

        let reloaded = DelegatedAdminStore::load(path.clone()).unwrap();
        assert!(!reloaded.get(&first.grant_id).unwrap().state.is_active());
        assert_eq!(
            reloaded
                .authorize(
                    &actor("admin-a"),
                    &captain("alpha", 7),
                    AdminOperation::InspectStatus,
                    &AdminTarget::Ship {
                        ship_slug: "alpha".into(),
                    },
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "grantRevoked"
        );
        let replacement = reloaded
            .appoint(ship_appointment(
                "admin-a",
                "alpha",
                operations(&[AdminOperation::InspectStatus]),
            ))
            .unwrap();
        assert!(replacement.grant_generation > first.grant_generation);
    }

    #[test]
    fn supervisor_retirement_ownership_change_and_generation_change_invalidate_grant() {
        let store = DelegatedAdminStore::ephemeral();
        store
            .appoint(ship_appointment(
                "admin-a",
                "alpha",
                operations(&[AdminOperation::InspectStatus]),
            ))
            .unwrap();
        let target = AdminTarget::Ship {
            ship_slug: "alpha".into(),
        };

        let mut retired = captain("alpha", 7);
        retired.active = false;
        assert_eq!(
            store
                .authorize(
                    &actor("admin-a"),
                    &retired,
                    AdminOperation::InspectStatus,
                    &target,
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "supervisorInactive"
        );

        let mut replacement = captain("alpha", 7);
        replacement.identity_id = "replacement-captain".into();
        assert_eq!(
            store
                .authorize(
                    &actor("admin-a"),
                    &replacement,
                    AdminOperation::InspectStatus,
                    &target,
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "supervisorMismatch"
        );

        assert_eq!(
            store
                .authorize(
                    &actor("admin-a"),
                    &captain("alpha", 8),
                    AdminOperation::InspectStatus,
                    &target,
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "authorityGenerationMismatch"
        );
    }

    #[test]
    fn worktree_cleanup_requires_owner_approval_and_safe_exact_target() {
        let store = DelegatedAdminStore::ephemeral();
        store
            .appoint(ship_appointment(
                "admin-a",
                "alpha",
                operations(&[AdminOperation::CleanupWorktree]),
            ))
            .unwrap();
        let admin = actor("admin-a");
        let authority = captain("alpha", 7);
        let target = AdminTarget::Worktree {
            ship_slug: "alpha".into(),
            worktree_id: "wt-42".into(),
        };

        assert_eq!(
            store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::CleanupWorktree,
                    &target,
                    &AdminSafeguards::default(),
                )
                .unwrap_err()
                .code(),
            "missingAuthoritativeOwnership"
        );

        let approval = ExactApproval {
            approval_id: "approval-1".into(),
            operation: AdminOperation::CleanupWorktree,
            target_fingerprint: target.fingerprint(),
            approved_by_identity_id: authority.identity_id.clone(),
            supervisor_generation: authority.authority_generation,
        };
        let wrong_target_approval = ExactApproval {
            target_fingerprint: "worktree:alpha:different".into(),
            ..approval.clone()
        };
        assert_eq!(
            store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::CleanupWorktree,
                    &target,
                    &AdminSafeguards {
                        authoritative_ownership_verified: true,
                        exact_approval: Some(wrong_target_approval),
                        worktree_safety: None,
                    },
                )
                .unwrap_err()
                .code(),
            "invalidExactApproval"
        );
        let unsafe_evidence = WorktreeSafetyEvidence {
            evidence_id: "safety-1".into(),
            target_fingerprint: target.fingerprint(),
            removable: false,
        };
        let safeguards = AdminSafeguards {
            authoritative_ownership_verified: true,
            exact_approval: Some(approval.clone()),
            worktree_safety: Some(unsafe_evidence),
        };
        assert_eq!(
            store
                .authorize(
                    &admin,
                    &authority,
                    AdminOperation::CleanupWorktree,
                    &target,
                    &safeguards,
                )
                .unwrap_err()
                .code(),
            "unsafeWorktree"
        );

        let safeguards = AdminSafeguards {
            authoritative_ownership_verified: true,
            exact_approval: Some(approval),
            worktree_safety: Some(WorktreeSafetyEvidence {
                evidence_id: "safety-2".into(),
                target_fingerprint: target.fingerprint(),
                removable: true,
            }),
        };
        let audit = store
            .authorize(
                &admin,
                &authority,
                AdminOperation::CleanupWorktree,
                &target,
                &safeguards,
            )
            .unwrap();
        assert_eq!(audit.actor_identity_id, "admin-a");
        assert_eq!(audit.delegating_supervisor_identity_id, "captain-alpha");
        assert_eq!(audit.exact_approval_id.as_deref(), Some("approval-1"));
        assert_eq!(audit.safety_evidence_id.as_deref(), Some("safety-2"));
    }

    #[test]
    fn exact_session_cleanup_approval_is_durable_actor_bound_and_one_time() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delegated-admin.json");
        let target = AdminTarget::CrewSession {
            ship_slug: "alpha".into(),
            session_id: "crew-alpha".into(),
        };
        let approval_id = {
            let store = DelegatedAdminStore::load(path.clone()).unwrap();
            let grant = store
                .appoint(ship_appointment(
                    "admin-a",
                    "alpha",
                    operations(&[AdminOperation::CleanupSession]),
                ))
                .unwrap();
            store
                .issue_exact_approval(
                    &grant.grant_id,
                    &captain("alpha", 7),
                    AdminOperation::CleanupSession,
                    &target,
                )
                .unwrap()
                .approval
                .approval_id
        };

        let reloaded = DelegatedAdminStore::load(path.clone()).unwrap();
        assert!(reloaded
            .get_approval(&approval_id)
            .is_some_and(|approval| approval.state.is_active()));
        let wrong_actor = reloaded
            .consume_exact_approval(
                &approval_id,
                "admin-b",
                &captain("alpha", 7),
                AdminOperation::CleanupSession,
                &target,
            )
            .unwrap_err();
        assert_eq!(wrong_actor.code(), "approvalMismatch");
        reloaded
            .consume_exact_approval(
                &approval_id,
                "admin-a",
                &captain("alpha", 7),
                AdminOperation::CleanupSession,
                &target,
            )
            .unwrap();
        assert_eq!(
            reloaded
                .consume_exact_approval(
                    &approval_id,
                    "admin-a",
                    &captain("alpha", 7),
                    AdminOperation::CleanupSession,
                    &target,
                )
                .unwrap_err()
                .code(),
            "approvalUsed"
        );
        assert!(DelegatedAdminStore::load(path)
            .unwrap()
            .get_approval(&approval_id)
            .is_some_and(|approval| matches!(approval.state, ApprovalState::Consumed { .. })));
    }

    #[test]
    fn corrupt_persisted_state_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delegated-admin.json");
        std::fs::write(&path, b"{ not valid json").unwrap();
        assert_eq!(
            DelegatedAdminStore::load(path).unwrap_err().code(),
            "corruptState"
        );
    }
}
