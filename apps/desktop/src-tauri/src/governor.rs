//! Fleet **spawn governor** - Phase 1 of the control-socket hardening
//! (`docs/SOCKET-AUTH-DESIGN.md` §4).
//!
//! Bounds the blast radius of process-changing control commands *regardless of
//! caller identity*: an injection-hijacked but fully-authenticated token holder
//! still cannot spawn or kill the fleet without limit. This layer changes NO
//! tokens - the only new behavior is refuse-past-ceiling. It is consulted from
//! `control::dispatch_authenticated` for the ProcessChanging tier only; the Read
//! and Organization tiers never touch it.
//!
//! Four controls:
//!   1. **Concurrent-session cap** - a soft ceiling on live `th_*` sessions,
//!      DERIVED from the authoritative tmux registry and reconciled on every
//!      spawn (never a free-running counter that drifts when a session dies
//!      without a `close_terminal`). Default 64, env `T_HUB_MAX_SESSIONS`.
//!   2. **Spawn rate** - a token-bucket: sustained 20/min, burst 8 (env
//!      `T_HUB_SPAWN_RATE` / `T_HUB_SPAWN_BURST`). The burst covers short
//!      adaptive fan-out plus supervisor recovery slack; the sustained rate
//!      lets independent near-simultaneous lanes through while starving a
//!      runaway loop.
//!   3. **Hard ceiling** - an absolute concurrent stop (128) that no env
//!      override can exceed (defense against a mis-set `T_HUB_MAX_SESSIONS`).
//!   4. **Destructive rate** - a separate token-bucket throttling `close_terminal`
//!      and kill-style `send_keys` (`C-c` and friends) at 15/min burst 10, so an
//!      injection cannot wipe the fleet in one tight loop while a crew closing
//!      its own handful of tiles stays well under.
//!
//! The governor holds no filesystem / tmux handles: the concurrent count is
//! passed in by the caller (which reads it from tmux) and the clock is passed in
//! as an `Instant`, so every path is deterministically unit-testable.
//!
//! The adaptive dispatch preflight complements those per-process controls.
//! It admits any number of genuinely independent lanes up to measured capacity,
//! while preserving room for Cortana, standing administrators, and recovery.
//! It also rejects ambiguous ownership, dependencies, and mutable-resource
//! collisions before callers allocate worktrees or start provider sessions.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

/// Absolute concurrent-session stop. No `T_HUB_MAX_SESSIONS` override can raise
/// the effective cap above this - it is the backstop against a fat-fingered env.
pub const HARD_SESSION_CEILING: usize = 128;
/// Default soft concurrent-session cap (env `T_HUB_MAX_SESSIONS`).
pub const DEFAULT_MAX_SESSIONS: usize = 64;
/// Default sustained spawn rate, spawns per minute (env `T_HUB_SPAWN_RATE`).
pub const DEFAULT_SPAWN_RATE_PER_MIN: f64 = 20.0;
/// Default spawn burst, the token-bucket capacity (env `T_HUB_SPAWN_BURST`).
pub const DEFAULT_SPAWN_BURST: f64 = 8.0;
/// Destructive-command sustained rate, per minute (not env-overridable).
pub const DESTRUCTIVE_RATE_PER_MIN: f64 = 15.0;
/// Destructive-command burst, the token-bucket capacity (not env-overridable).
pub const DESTRUCTIVE_BURST: f64 = 10.0;

/// Why a process-changing command was refused. Carries the machine-readable
/// error string (`docs/SOCKET-AUTH-DESIGN.md` §5) and a short decision code the
/// audit log records verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The audit decision code (`refused-cap` / `refused-rate` / `refused-ceiling`).
    pub code: &'static str,
    /// The client-facing error message (exact wording from the design's §5).
    pub message: String,
}

/// Stable structured reasons returned by adaptive dispatch preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchReasonCode {
    MachineUnhealthy,
    HardCeiling,
    ConfiguredCapacity,
    MachineCapacity,
    ProviderCapacity,
    WorktreeCapacity,
    ReservedCapacity,
    MissingLaneIdentity,
    MissingOwnership,
    MissingDependencies,
    UnmetDependency,
    DuplicateLane,
    DuplicateOwner,
    UnknownDependency,
    DependencyCycle,
    InvalidResourceClaim,
    InvalidOrderingContract,
    MutableFileCollision,
    SchemaCollision,
    InterfaceCollision,
}

impl DispatchReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MachineUnhealthy => "machine-unhealthy",
            Self::HardCeiling => "hard-ceiling",
            Self::ConfiguredCapacity => "configured-capacity",
            Self::MachineCapacity => "machine-capacity",
            Self::ProviderCapacity => "provider-capacity",
            Self::WorktreeCapacity => "worktree-capacity",
            Self::ReservedCapacity => "reserved-capacity",
            Self::MissingLaneIdentity => "missing-lane-identity",
            Self::MissingOwnership => "missing-ownership",
            Self::MissingDependencies => "missing-dependencies",
            Self::UnmetDependency => "unmet-dependency",
            Self::DuplicateLane => "duplicate-lane",
            Self::DuplicateOwner => "duplicate-owner",
            Self::UnknownDependency => "unknown-dependency",
            Self::DependencyCycle => "dependency-cycle",
            Self::InvalidResourceClaim => "invalid-resource-claim",
            Self::InvalidOrderingContract => "invalid-ordering-contract",
            Self::MutableFileCollision => "mutable-file-collision",
            Self::SchemaCollision => "schema-collision",
            Self::InterfaceCollision => "interface-collision",
        }
    }
}

/// Fleet capacity that is reserved before ordinary implementation lanes are
/// admitted. Values are minimum live-or-recoverable slots, not fixed Crew caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservationPolicy {
    pub cortana: usize,
    pub fleet_admins: usize,
    pub ship_admins_per_active_captain: usize,
    pub recovery: usize,
}

/// The durable intent attached to an admitted agent runtime.
///
/// A privileged purpose may fill only its matching reserved slot. It does not
/// itself grant the administrative role, which still requires an explicit
/// supervisor appointment and durable grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdmissionPurpose {
    #[default]
    Ordinary,
    Cortana,
    FleetAdmin,
    ShipAdmin,
    Recovery,
}

impl Default for ReservationPolicy {
    fn default() -> Self {
        Self {
            cortana: 1,
            fleet_admins: 1,
            ship_admins_per_active_captain: 1,
            recovery: 1,
        }
    }
}

/// One reservation class in a capacity report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservationClassReport {
    pub required: usize,
    pub live: usize,
    pub deficit: usize,
}

impl ReservationClassReport {
    fn new(required: usize, live: usize) -> Self {
        Self {
            required,
            live,
            deficit: required.saturating_sub(live),
        }
    }
}

/// Capacity held back for durable supervisors, their standing aides, and
/// recovery. Only the current deficit consumes otherwise available headroom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservationReport {
    pub cortana: ReservationClassReport,
    pub fleet_admins: ReservationClassReport,
    pub ship_admins: ReservationClassReport,
    pub recovery: ReservationClassReport,
    pub total_deficit: usize,
}

/// Runtime observations supplied by the authoritative control layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapacity {
    pub live_sessions: usize,
    pub machine_healthy: bool,
    pub machine_session_capacity: usize,
    pub provider_session_capacity: usize,
    pub provider_live_sessions: usize,
    #[serde(default)]
    pub provider_capacity_status: ProviderCapacityStatus,
    pub available_worktrees: usize,
    pub active_captains: usize,
    /// Exact active ship scopes used to reserve one Ship Admin per ship.
    /// Empty means the caller supplied only the legacy aggregate count.
    #[serde(default)]
    pub active_captain_ships: BTreeSet<String>,
    pub live_cortana: usize,
    pub live_fleet_admins: usize,
    pub live_ship_admins: usize,
    /// Live Ship Admins by exact ship scope.
    /// Empty means the caller supplied only the legacy aggregate count.
    #[serde(default)]
    pub live_ship_admin_scopes: BTreeMap<String, usize>,
    pub live_recovery_sessions: usize,
}

/// Provenance and health of the provider-capacity ceiling used for admission.
///
/// A packaged policy is usable but degraded because it is a conservative local
/// safety ceiling rather than live account telemetry. Explicit configured
/// telemetry is healthy only after its value has been validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapacityStatus {
    pub source: String,
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Default for ProviderCapacityStatus {
    fn default() -> Self {
        Self {
            source: "legacy-unspecified".into(),
            degraded: true,
            detail: Some("provider capacity provenance was not recorded".into()),
        }
    }
}

/// An implementation lane's explicit ownership, dependency, and mutable
/// resource claims. `dependencies: Some(empty)` means explicitly independent;
/// `None` means the assignment omitted dependency analysis and is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneClaim {
    pub lane_id: String,
    pub owner_id: String,
    pub dependencies: Option<BTreeSet<String>>,
    #[serde(default)]
    pub mutable_files: BTreeSet<String>,
    #[serde(default)]
    pub mutable_schemas: BTreeSet<String>,
    #[serde(default)]
    pub mutable_interfaces: BTreeSet<String>,
}

/// An explicit exception to mutable-resource isolation.
///
/// The ordered lane list establishes a single integration sequence, and
/// `integration_owner` names the one actor responsible for applying it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationContract {
    pub contract_id: String,
    pub integration_owner: String,
    pub ordered_lane_ids: Vec<String>,
}

/// Complete input to an adaptive dispatch preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchPreflight {
    pub requested_lanes: Vec<LaneClaim>,
    /// Number of requested lanes that will consume a provider Harness slot.
    /// Generic shells and plain worktrees explicitly request zero.
    #[serde(default)]
    pub requested_provider_lanes: usize,
    /// Reservation class requested by a single runtime admission.
    #[serde(default)]
    pub admission_purpose: AdmissionPurpose,
    /// Required only for a Ship Admin reservation request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_admin_scope: Option<String>,
    #[serde(default)]
    pub active_lanes: Vec<LaneClaim>,
    #[serde(default)]
    pub satisfied_dependencies: BTreeSet<String>,
    #[serde(default)]
    pub integration_contracts: Vec<IntegrationContract>,
    pub capacity: RuntimeCapacity,
}

/// Machine-readable capacity and reservation evidence for an admission result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityReport {
    pub requested_lanes: usize,
    pub requested_provider_lanes: usize,
    pub configured_session_limit: usize,
    pub hard_session_limit: usize,
    pub machine_session_limit: usize,
    pub effective_session_limit: usize,
    pub live_sessions: usize,
    pub session_headroom_before_reservations: usize,
    pub session_headroom_after_reservations: usize,
    #[serde(default)]
    pub provider_session_limit: usize,
    #[serde(default)]
    pub provider_live_sessions: usize,
    pub provider_headroom: usize,
    /// Provider headroom left after protecting all unfilled role reservations.
    pub provider_headroom_after_reservations: usize,
    #[serde(default)]
    pub provider_capacity_status: ProviderCapacityStatus,
    pub worktree_headroom: usize,
    pub effective_lane_headroom: usize,
    pub reservations: ReservationReport,
    pub limiting_factors: Vec<DispatchReasonCode>,
}

/// Compatibility wire shape for reports persisted before provider reservations
/// and explicit provider-lane requests were introduced.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapacityReportWire {
    requested_lanes: usize,
    #[serde(default)]
    requested_provider_lanes: Option<usize>,
    configured_session_limit: usize,
    hard_session_limit: usize,
    machine_session_limit: usize,
    effective_session_limit: usize,
    live_sessions: usize,
    session_headroom_before_reservations: usize,
    session_headroom_after_reservations: usize,
    #[serde(default)]
    provider_session_limit: usize,
    #[serde(default)]
    provider_live_sessions: usize,
    provider_headroom: usize,
    #[serde(default)]
    provider_headroom_after_reservations: Option<usize>,
    #[serde(default)]
    provider_capacity_status: ProviderCapacityStatus,
    worktree_headroom: usize,
    effective_lane_headroom: usize,
    reservations: ReservationReport,
    limiting_factors: Vec<DispatchReasonCode>,
}

impl<'de> Deserialize<'de> for CapacityReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CapacityReportWire::deserialize(deserializer)?;
        let provider_headroom_after_reservations = wire
            .provider_headroom_after_reservations
            .unwrap_or_else(|| {
                wire.provider_headroom
                    .saturating_sub(wire.reservations.total_deficit)
            });
        Ok(Self {
            requested_lanes: wire.requested_lanes,
            requested_provider_lanes: wire
                .requested_provider_lanes
                .unwrap_or(wire.requested_lanes),
            configured_session_limit: wire.configured_session_limit,
            hard_session_limit: wire.hard_session_limit,
            machine_session_limit: wire.machine_session_limit,
            effective_session_limit: wire.effective_session_limit,
            live_sessions: wire.live_sessions,
            session_headroom_before_reservations: wire.session_headroom_before_reservations,
            session_headroom_after_reservations: wire.session_headroom_after_reservations,
            provider_session_limit: wire.provider_session_limit,
            provider_live_sessions: wire.provider_live_sessions,
            provider_headroom: wire.provider_headroom,
            provider_headroom_after_reservations,
            provider_capacity_status: wire.provider_capacity_status,
            worktree_headroom: wire.worktree_headroom,
            effective_lane_headroom: wire.effective_lane_headroom,
            reservations: wire.reservations,
            limiting_factors: wire.limiting_factors,
        })
    }
}

/// A rejected dispatch with a stable reason, relevant lane/resource evidence,
/// and the same capacity report returned for an admitted request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchRefusal {
    pub code: DispatchReasonCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lane_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    pub capacity: Box<CapacityReport>,
}

/// A classic token bucket: `capacity` tokens max, refilled at `refill_per_sec`,
/// one token spent per admitted event. `try_take` refills lazily from the elapsed
/// wall-clock since the last call, so it needs no background timer. The clock is
/// injected (`now`) so tests are deterministic.
#[derive(Debug)]
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last: Option<Instant>,
}

impl TokenBucket {
    fn new(capacity: f64, rate_per_min: f64) -> Self {
        Self {
            capacity: capacity.max(1.0),
            // Start full so a fresh listener admits an immediate legitimate burst.
            tokens: capacity.max(1.0),
            refill_per_sec: (rate_per_min / 60.0).max(0.0),
            last: None,
        }
    }

    /// Refill for the elapsed time, then spend one token if available. Returns
    /// `true` when the event is admitted.
    fn try_take(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last {
            let elapsed = now.saturating_duration_since(last).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        }
        self.last = Some(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The fleet spawn governor. Cloneable-by-`Arc` and shared across every
/// connection handler thread (like the existing `ACTIVE_CONNS` atomic), so all
/// callers draw from one fleet-wide budget.
pub struct SpawnGovernor {
    max_sessions: usize,
    reservations: ReservationPolicy,
    spawn: Mutex<TokenBucket>,
    destructive: Mutex<TokenBucket>,
    spawn_rate_per_min: f64,
    destructive_rate_per_min: f64,
}

impl SpawnGovernor {
    /// Build a governor with explicit limits (tests / callers that don't read the
    /// environment). `max_sessions` is clamped to [`HARD_SESSION_CEILING`].
    pub fn new(max_sessions: usize, spawn_rate_per_min: f64, spawn_burst: f64) -> Self {
        Self {
            max_sessions: max_sessions.min(HARD_SESSION_CEILING),
            reservations: ReservationPolicy::default(),
            spawn: Mutex::new(TokenBucket::new(spawn_burst, spawn_rate_per_min)),
            destructive: Mutex::new(TokenBucket::new(
                DESTRUCTIVE_BURST,
                DESTRUCTIVE_RATE_PER_MIN,
            )),
            spawn_rate_per_min,
            destructive_rate_per_min: DESTRUCTIVE_RATE_PER_MIN,
        }
    }

    /// Build a governor from the environment, falling back to the Phase 1
    /// defaults. `T_HUB_MAX_SESSIONS` is clamped to [`HARD_SESSION_CEILING`]; the
    /// destructive throttle is fixed (not operator-tunable).
    pub fn from_env() -> Self {
        let max = env_usize("T_HUB_MAX_SESSIONS", DEFAULT_MAX_SESSIONS);
        let rate = env_f64("T_HUB_SPAWN_RATE", DEFAULT_SPAWN_RATE_PER_MIN);
        let burst = env_f64("T_HUB_SPAWN_BURST", DEFAULT_SPAWN_BURST);
        Self::new(max, rate, burst)
    }

    /// The effective concurrent-session cap after clamping (diagnostics / tests).
    pub fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    /// Replace the ordinary-lane reservation policy.
    ///
    /// This keeps [`SpawnGovernor::new`] backward compatible while allowing the
    /// control layer to tune durable-role reservations from authoritative fleet
    /// configuration.
    #[allow(dead_code)] // stable control/CLI seam; wiring lands after the governor foundation
    pub fn with_reservation_policy(mut self, reservations: ReservationPolicy) -> Self {
        self.reservations = reservations;
        self
    }

    /// Evaluate a multi-lane dispatch without consuming a spawn-rate token.
    ///
    /// Callers should run this before allocating worktrees or provider sessions,
    /// then retain the report as dispatch evidence. Actual process creation must
    /// still pass [`Self::check_spawn`] immediately before each spawn.
    pub fn preflight_dispatch(
        &self,
        request: &DispatchPreflight,
    ) -> Result<CapacityReport, DispatchRefusal> {
        let report = self.capacity_report(request);
        self.validate_lane_contract(request, &report)?;
        self.validate_capacity(request, &report)?;
        Ok(report)
    }

    /// Produce the capacity evidence used by [`Self::preflight_dispatch`].
    /// This is exposed separately so status surfaces can report headroom even
    /// when no dispatch is being attempted.
    pub fn capacity_report(&self, request: &DispatchPreflight) -> CapacityReport {
        let runtime = &request.capacity;
        let ship_admins_required = if runtime.active_captain_ships.is_empty() {
            runtime
                .active_captains
                .saturating_mul(self.reservations.ship_admins_per_active_captain)
        } else {
            runtime
                .active_captain_ships
                .len()
                .saturating_mul(self.reservations.ship_admins_per_active_captain)
        };
        // Do not let duplicate administrators in one ship satisfy another
        // ship's standing slot.  Capacity is scoped, so each active ship can
        // contribute at most its required number of live administrators.
        let live_ship_admins = if runtime.live_ship_admin_scopes.is_empty() {
            runtime.live_ship_admins
        } else {
            runtime
                .active_captain_ships
                .iter()
                .map(|ship| {
                    runtime
                        .live_ship_admin_scopes
                        .get(ship)
                        .copied()
                        .unwrap_or(0)
                        .min(self.reservations.ship_admins_per_active_captain)
                })
                .sum()
        };
        let cortana = ReservationClassReport::new(self.reservations.cortana, runtime.live_cortana);
        let fleet_admins =
            ReservationClassReport::new(self.reservations.fleet_admins, runtime.live_fleet_admins);
        let ship_admins = ReservationClassReport::new(ship_admins_required, live_ship_admins);
        let recovery =
            ReservationClassReport::new(self.reservations.recovery, runtime.live_recovery_sessions);
        let total_deficit = cortana
            .deficit
            .saturating_add(fleet_admins.deficit)
            .saturating_add(ship_admins.deficit)
            .saturating_add(recovery.deficit);
        let reservations = ReservationReport {
            cortana,
            fleet_admins,
            ship_admins,
            recovery,
            total_deficit,
        };

        let effective_session_limit = self
            .max_sessions
            .min(HARD_SESSION_CEILING)
            .min(runtime.machine_session_capacity);
        let session_headroom_before_reservations =
            effective_session_limit.saturating_sub(runtime.live_sessions);
        let session_headroom_after_reservations =
            session_headroom_before_reservations.saturating_sub(total_deficit);
        let provider_headroom = runtime
            .provider_session_capacity
            .saturating_sub(runtime.provider_live_sessions);
        let provider_headroom_after_reservations = provider_headroom.saturating_sub(total_deficit);
        let protected_for_request = self.reservation_deficit_protected_from(request, &reservations);
        let session_headroom_for_request =
            session_headroom_before_reservations.saturating_sub(protected_for_request);
        let provider_headroom_for_request = provider_headroom.saturating_sub(protected_for_request);
        let provider_lane_headroom = if request.requested_provider_lanes == 0 {
            usize::MAX
        } else {
            provider_headroom_for_request
        };
        let effective_lane_headroom = if runtime.machine_healthy {
            session_headroom_for_request
                .min(provider_lane_headroom)
                .min(runtime.available_worktrees)
        } else {
            0
        };

        let mut limiting_factors = BTreeSet::new();
        if !runtime.machine_healthy {
            limiting_factors.insert(DispatchReasonCode::MachineUnhealthy);
        }
        if runtime.live_sessions >= HARD_SESSION_CEILING {
            limiting_factors.insert(DispatchReasonCode::HardCeiling);
        }
        if self.max_sessions <= runtime.machine_session_capacity
            && session_headroom_before_reservations <= provider_lane_headroom
            && session_headroom_before_reservations <= runtime.available_worktrees
        {
            limiting_factors.insert(DispatchReasonCode::ConfiguredCapacity);
        }
        if runtime.machine_session_capacity <= self.max_sessions
            && session_headroom_before_reservations <= provider_lane_headroom
            && session_headroom_before_reservations <= runtime.available_worktrees
        {
            limiting_factors.insert(DispatchReasonCode::MachineCapacity);
        }
        if request.requested_lanes.len() > session_headroom_for_request
            || (request.requested_provider_lanes > 0
                && request.requested_provider_lanes > provider_headroom_for_request
                && request.requested_provider_lanes <= provider_headroom)
        {
            limiting_factors.insert(DispatchReasonCode::ReservedCapacity);
        }
        if request.requested_provider_lanes > 0
            && request.requested_provider_lanes > provider_headroom
        {
            limiting_factors.insert(DispatchReasonCode::ProviderCapacity);
        }
        if runtime.available_worktrees <= session_headroom_for_request
            && runtime.available_worktrees <= provider_lane_headroom
        {
            limiting_factors.insert(DispatchReasonCode::WorktreeCapacity);
        }

        CapacityReport {
            requested_lanes: request.requested_lanes.len(),
            requested_provider_lanes: request.requested_provider_lanes,
            configured_session_limit: self.max_sessions,
            hard_session_limit: HARD_SESSION_CEILING,
            machine_session_limit: runtime.machine_session_capacity,
            effective_session_limit,
            live_sessions: runtime.live_sessions,
            session_headroom_before_reservations,
            session_headroom_after_reservations,
            provider_session_limit: runtime.provider_session_capacity,
            provider_live_sessions: runtime.provider_live_sessions,
            provider_headroom,
            provider_headroom_after_reservations,
            provider_capacity_status: runtime.provider_capacity_status.clone(),
            worktree_headroom: runtime.available_worktrees,
            effective_lane_headroom,
            reservations,
            limiting_factors: limiting_factors.into_iter().collect(),
        }
    }

    /// Return the reservation deficit that must remain protected ahead of this
    /// request. A request that fills its own missing role slot may consume that
    /// slot, but it can never consume a higher-priority deficit.
    ///
    /// Priority is stable and fail-closed: Cortana, Recovery, Fleet Admin, then
    /// Ship Admin. Missing Ship Admin scopes are ordered lexically so concurrent
    /// requests cannot choose whichever ship happens to win a race.
    fn reservation_deficit_protected_from(
        &self,
        request: &DispatchPreflight,
        report: &ReservationReport,
    ) -> usize {
        let ordinary = || report.total_deficit;
        match request.admission_purpose {
            AdmissionPurpose::Ordinary => ordinary(),
            AdmissionPurpose::Cortana if report.cortana.deficit > 0 => 0,
            AdmissionPurpose::Recovery if report.recovery.deficit > 0 => report.cortana.deficit,
            AdmissionPurpose::FleetAdmin if report.fleet_admins.deficit > 0 => report
                .cortana
                .deficit
                .saturating_add(report.recovery.deficit),
            AdmissionPurpose::ShipAdmin => {
                let Some(scope) = request.ship_admin_scope.as_deref() else {
                    return ordinary();
                };
                let runtime = &request.capacity;
                if runtime.active_captain_ships.is_empty() {
                    return if report.ship_admins.deficit > 0 {
                        report
                            .cortana
                            .deficit
                            .saturating_add(report.recovery.deficit)
                            .saturating_add(report.fleet_admins.deficit)
                    } else {
                        ordinary()
                    };
                }
                if !runtime.active_captain_ships.contains(scope) {
                    return ordinary();
                }
                let live_for_scope = runtime
                    .live_ship_admin_scopes
                    .get(scope)
                    .copied()
                    .unwrap_or(0);
                if live_for_scope >= self.reservations.ship_admins_per_active_captain {
                    return ordinary();
                }
                let preceding_ship_deficits = runtime
                    .active_captain_ships
                    .iter()
                    .take_while(|candidate| candidate.as_str() < scope)
                    .map(|candidate| {
                        self.reservations
                            .ship_admins_per_active_captain
                            .saturating_sub(
                                runtime
                                    .live_ship_admin_scopes
                                    .get(candidate)
                                    .copied()
                                    .unwrap_or(0),
                            )
                    })
                    .sum::<usize>();
                report
                    .cortana
                    .deficit
                    .saturating_add(report.recovery.deficit)
                    .saturating_add(report.fleet_admins.deficit)
                    .saturating_add(preceding_ship_deficits)
            }
            _ => ordinary(),
        }
    }

    fn validate_capacity(
        &self,
        request: &DispatchPreflight,
        report: &CapacityReport,
    ) -> Result<(), DispatchRefusal> {
        let requested = request.requested_lanes.len();
        let requested_provider = request.requested_provider_lanes;
        let runtime = &request.capacity;
        if requested == 0 {
            return Ok(());
        }
        if requested_provider > requested {
            return Err(dispatch_refusal(
                DispatchReasonCode::ProviderCapacity,
                format!(
                    "dispatch refused: requested provider lanes {requested_provider} exceed total requested lanes {requested}"
                ),
                report,
            ));
        }
        if request.admission_purpose != AdmissionPurpose::Ordinary && requested != 1 {
            return Err(dispatch_refusal(
                DispatchReasonCode::ReservedCapacity,
                "dispatch refused: a privileged reservation admission must name exactly one lane",
                report,
            ));
        }
        if !runtime.machine_healthy {
            return Err(dispatch_refusal(
                DispatchReasonCode::MachineUnhealthy,
                "dispatch refused: machine health is degraded",
                report,
            ));
        }
        if runtime.live_sessions.saturating_add(requested) > HARD_SESSION_CEILING {
            return Err(dispatch_refusal(
                DispatchReasonCode::HardCeiling,
                format!(
                    "dispatch refused: {requested} lanes would exceed hard session ceiling {HARD_SESSION_CEILING}"
                ),
                report,
            ));
        }
        let configured_headroom = self.max_sessions.saturating_sub(runtime.live_sessions);
        if requested > configured_headroom {
            return Err(dispatch_refusal(
                DispatchReasonCode::ConfiguredCapacity,
                format!(
                    "dispatch refused: {requested} lanes exceed configured headroom {configured_headroom}"
                ),
                report,
            ));
        }
        let machine_headroom = runtime
            .machine_session_capacity
            .saturating_sub(runtime.live_sessions);
        if requested > machine_headroom {
            return Err(dispatch_refusal(
                DispatchReasonCode::MachineCapacity,
                format!(
                    "dispatch refused: {requested} lanes exceed healthy machine headroom {machine_headroom}"
                ),
                report,
            ));
        }
        if requested_provider > report.provider_headroom {
            return Err(dispatch_refusal(
                DispatchReasonCode::ProviderCapacity,
                format!(
                    "dispatch refused: {requested_provider} provider lanes exceed raw provider headroom {}",
                    report.provider_headroom
                ),
                report,
            ));
        }
        let protected_for_request =
            self.reservation_deficit_protected_from(request, &report.reservations);
        let session_headroom_for_request = report
            .session_headroom_before_reservations
            .saturating_sub(protected_for_request);
        if requested > session_headroom_for_request {
            return Err(dispatch_refusal(
                DispatchReasonCode::ReservedCapacity,
                format!(
                    "dispatch refused: {requested} lanes would consume {protected_for_request} higher-priority or unclaimed supervisor, administrator, or recovery slots"
                ),
                report,
            ));
        }
        let provider_headroom_for_request = report
            .provider_headroom
            .saturating_sub(protected_for_request);
        if requested_provider > provider_headroom_for_request {
            return Err(dispatch_refusal(
                DispatchReasonCode::ReservedCapacity,
                format!(
                    "dispatch refused: {requested_provider} provider lanes would consume {protected_for_request} higher-priority or unclaimed provider reservations"
                ),
                report,
            ));
        }
        if requested > report.worktree_headroom {
            return Err(dispatch_refusal(
                DispatchReasonCode::WorktreeCapacity,
                format!(
                    "dispatch refused: {requested} lanes exceed available worktrees {}",
                    report.worktree_headroom
                ),
                report,
            ));
        }
        Ok(())
    }

    fn validate_lane_contract(
        &self,
        request: &DispatchPreflight,
        report: &CapacityReport,
    ) -> Result<(), DispatchRefusal> {
        let all_lanes: Vec<&LaneClaim> = request
            .active_lanes
            .iter()
            .chain(request.requested_lanes.iter())
            .collect();
        let mut lanes_by_id = BTreeMap::new();
        let mut lanes_by_owner = BTreeMap::new();
        for lane in &all_lanes {
            if lane.lane_id.trim().is_empty() {
                return Err(dispatch_refusal(
                    DispatchReasonCode::MissingLaneIdentity,
                    "dispatch refused: every lane must have a stable lane identity",
                    report,
                ));
            }
            if lane.owner_id.trim().is_empty() {
                return Err(dispatch_refusal_with_lanes(
                    DispatchReasonCode::MissingOwnership,
                    format!(
                        "dispatch refused: lane '{}' has no explicit owner",
                        lane.lane_id
                    ),
                    vec![lane.lane_id.clone()],
                    None,
                    report,
                ));
            }
            if lane.dependencies.is_none() {
                return Err(dispatch_refusal_with_lanes(
                    DispatchReasonCode::MissingDependencies,
                    format!(
                        "dispatch refused: lane '{}' has no explicit dependency declaration",
                        lane.lane_id
                    ),
                    vec![lane.lane_id.clone()],
                    None,
                    report,
                ));
            }
            if let Some(other) = lanes_by_id.insert(lane.lane_id.clone(), *lane) {
                return Err(dispatch_refusal_with_lanes(
                    DispatchReasonCode::DuplicateLane,
                    format!(
                        "dispatch refused: duplicate lane identity '{}'",
                        lane.lane_id
                    ),
                    vec![other.lane_id.clone(), lane.lane_id.clone()],
                    None,
                    report,
                ));
            }
            if let Some(other_lane_id) =
                lanes_by_owner.insert(lane.owner_id.clone(), lane.lane_id.clone())
            {
                return Err(dispatch_refusal_with_lanes(
                    DispatchReasonCode::DuplicateOwner,
                    format!(
                        "dispatch refused: owner '{}' is assigned to concurrent lanes",
                        lane.owner_id
                    ),
                    vec![other_lane_id, lane.lane_id.clone()],
                    None,
                    report,
                ));
            }
            validate_resource_claims(lane, report)?;
        }

        validate_dependencies(request, &lanes_by_id, report)?;
        let contracts = validate_integration_contracts(request, &lanes_by_id, report)?;
        validate_collisions(&all_lanes, &contracts, report)
    }

    /// Gate a `spawn_terminal`. `live_sessions` is the authoritative live-session
    /// count (from tmux), reconciled by the caller immediately before this call.
    /// Order matters: the hard ceiling and the soft cap are checked BEFORE a rate
    /// token is spent, so a capacity rejection never burns spawn-rate budget.
    pub fn check_spawn(&self, live_sessions: usize, now: Instant) -> Result<(), Refusal> {
        if live_sessions >= HARD_SESSION_CEILING {
            return Err(Refusal {
                code: "refused-ceiling",
                message: format!("refused: hard session ceiling ({HARD_SESSION_CEILING}) reached"),
            });
        }
        if live_sessions >= self.max_sessions {
            return Err(Refusal {
                code: "refused-cap",
                message: format!(
                    "refused: fleet at concurrent-session cap ({live_sessions}/{}); \
                     close sessions or raise T_HUB_MAX_SESSIONS",
                    self.max_sessions
                ),
            });
        }
        if !self.spawn.lock().unwrap().try_take(now) {
            return Err(Refusal {
                code: "refused-rate",
                message: format!(
                    "refused: spawn rate limit ({}/min); retry shortly",
                    fmt_rate(self.spawn_rate_per_min)
                ),
            });
        }
        Ok(())
    }

    /// Gate a destructive command (`close_terminal`, kill-style `send_keys`).
    /// Rate-limited only - there is no concurrent notion for a teardown.
    pub fn check_destructive(&self, now: Instant) -> Result<(), Refusal> {
        if !self.destructive.lock().unwrap().try_take(now) {
            return Err(Refusal {
                code: "refused-rate",
                message: format!(
                    "refused: destructive-command rate limit ({}/min); retry shortly",
                    fmt_rate(self.destructive_rate_per_min)
                ),
            });
        }
        Ok(())
    }
}

fn dispatch_refusal(
    code: DispatchReasonCode,
    message: impl Into<String>,
    report: &CapacityReport,
) -> DispatchRefusal {
    dispatch_refusal_with_lanes(code, message, Vec::new(), None, report)
}

fn dispatch_refusal_with_lanes(
    code: DispatchReasonCode,
    message: impl Into<String>,
    lane_ids: Vec<String>,
    resource: Option<String>,
    report: &CapacityReport,
) -> DispatchRefusal {
    DispatchRefusal {
        code,
        message: message.into(),
        lane_ids,
        resource,
        capacity: Box::new(report.clone()),
    }
}

fn validate_resource_claims(
    lane: &LaneClaim,
    report: &CapacityReport,
) -> Result<(), DispatchRefusal> {
    for file in &lane.mutable_files {
        if normalize_file_claim(file).is_none() {
            return Err(dispatch_refusal_with_lanes(
                DispatchReasonCode::InvalidResourceClaim,
                format!(
                    "dispatch refused: lane '{}' contains invalid mutable file claim '{file}'",
                    lane.lane_id
                ),
                vec![lane.lane_id.clone()],
                Some(file.clone()),
                report,
            ));
        }
    }
    for resource in lane
        .mutable_schemas
        .iter()
        .chain(lane.mutable_interfaces.iter())
    {
        if resource.trim().is_empty() {
            return Err(dispatch_refusal_with_lanes(
                DispatchReasonCode::InvalidResourceClaim,
                format!(
                    "dispatch refused: lane '{}' contains an empty mutable resource claim",
                    lane.lane_id
                ),
                vec![lane.lane_id.clone()],
                Some(resource.clone()),
                report,
            ));
        }
    }
    Ok(())
}

fn validate_dependencies(
    request: &DispatchPreflight,
    lanes_by_id: &BTreeMap<String, &LaneClaim>,
    report: &CapacityReport,
) -> Result<(), DispatchRefusal> {
    if let Some(lane) = request
        .requested_lanes
        .iter()
        .find(|lane| request.satisfied_dependencies.contains(&lane.lane_id))
    {
        return Err(dispatch_refusal_with_lanes(
            DispatchReasonCode::DuplicateLane,
            format!(
                "dispatch refused: lane '{}' already has complete delivery evidence",
                lane.lane_id
            ),
            vec![lane.lane_id.clone()],
            None,
            report,
        ));
    }
    let mut indegree: BTreeMap<String, usize> = lanes_by_id
        .keys()
        .map(|lane_id| (lane_id.clone(), 0))
        .collect();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for lane in lanes_by_id.values() {
        for dependency in lane.dependencies.as_ref().expect("validated above") {
            if request.satisfied_dependencies.contains(dependency) {
                continue;
            }
            if !lanes_by_id.contains_key(dependency) {
                return Err(dispatch_refusal_with_lanes(
                    DispatchReasonCode::UnknownDependency,
                    format!(
                        "dispatch refused: lane '{}' names unknown dependency '{dependency}'",
                        lane.lane_id
                    ),
                    vec![lane.lane_id.clone()],
                    Some(dependency.clone()),
                    report,
                ));
            }
            *indegree.get_mut(&lane.lane_id).expect("lane is indexed") += 1;
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(lane.lane_id.clone());
        }
    }

    let mut ready: VecDeque<String> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(lane_id, _)| lane_id.clone())
        .collect();
    let mut visited = 0;
    while let Some(lane_id) = ready.pop_front() {
        visited += 1;
        if let Some(children) = dependents.get(&lane_id) {
            for child in children {
                let degree = indegree.get_mut(child).expect("dependent is indexed");
                *degree -= 1;
                if *degree == 0 {
                    ready.push_back(child.clone());
                }
            }
        }
    }
    if visited != lanes_by_id.len() {
        let cyclic_lanes = indegree
            .into_iter()
            .filter(|(_, degree)| *degree > 0)
            .map(|(lane_id, _)| lane_id)
            .collect::<Vec<_>>();
        return Err(dispatch_refusal_with_lanes(
            DispatchReasonCode::DependencyCycle,
            "dispatch refused: lane dependencies contain a cycle",
            cyclic_lanes,
            None,
            report,
        ));
    }
    for lane in &request.requested_lanes {
        if let Some(dependency) = lane.dependencies.as_ref().and_then(|dependencies| {
            dependencies
                .iter()
                .find(|dependency| !request.satisfied_dependencies.contains(*dependency))
        }) {
            return Err(dispatch_refusal_with_lanes(
                DispatchReasonCode::UnmetDependency,
                format!(
                    "dispatch refused: lane '{}' is waiting for dependency '{dependency}'",
                    lane.lane_id
                ),
                vec![lane.lane_id.clone()],
                Some(dependency.clone()),
                report,
            ));
        }
    }
    Ok(())
}

fn validate_integration_contracts<'a>(
    request: &'a DispatchPreflight,
    lanes_by_id: &BTreeMap<String, &LaneClaim>,
    report: &CapacityReport,
) -> Result<Vec<&'a IntegrationContract>, DispatchRefusal> {
    let mut contract_ids = BTreeSet::new();
    for contract in &request.integration_contracts {
        if contract.contract_id.trim().is_empty()
            || contract.integration_owner.trim().is_empty()
            || contract.ordered_lane_ids.len() < 2
            || !contract_ids.insert(contract.contract_id.clone())
        {
            return Err(dispatch_refusal(
                DispatchReasonCode::InvalidOrderingContract,
                "dispatch refused: integration contracts require a unique identity, one owner, and at least two ordered lanes",
                report,
            ));
        }
        let mut ordered_lanes = BTreeSet::new();
        for lane_id in &contract.ordered_lane_ids {
            if !lanes_by_id.contains_key(lane_id) || !ordered_lanes.insert(lane_id) {
                return Err(dispatch_refusal_with_lanes(
                    DispatchReasonCode::InvalidOrderingContract,
                    format!(
                        "dispatch refused: integration contract '{}' contains an unknown or duplicate lane '{lane_id}'",
                        contract.contract_id
                    ),
                    vec![lane_id.clone()],
                    None,
                    report,
                ));
            }
        }
    }
    Ok(request.integration_contracts.iter().collect())
}

#[derive(Debug, Clone, Copy)]
enum MutableResourceKind {
    File,
    Schema,
    Interface,
}

impl MutableResourceKind {
    fn refusal_code(self) -> DispatchReasonCode {
        match self {
            Self::File => DispatchReasonCode::MutableFileCollision,
            Self::Schema => DispatchReasonCode::SchemaCollision,
            Self::Interface => DispatchReasonCode::InterfaceCollision,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::File => "mutable file",
            Self::Schema => "schema",
            Self::Interface => "interface",
        }
    }
}

fn validate_collisions(
    lanes: &[&LaneClaim],
    contracts: &[&IntegrationContract],
    report: &CapacityReport,
) -> Result<(), DispatchRefusal> {
    for left_index in 0..lanes.len() {
        for right_index in (left_index + 1)..lanes.len() {
            let left = lanes[left_index];
            let right = lanes[right_index];
            let collisions = lane_collisions(left, right);
            for (kind, resource) in collisions {
                let matching_contracts = contracts
                    .iter()
                    .filter(|contract| {
                        contract.ordered_lane_ids.contains(&left.lane_id)
                            && contract.ordered_lane_ids.contains(&right.lane_id)
                    })
                    .count();
                if matching_contracts == 1 {
                    continue;
                }
                if matching_contracts > 1 {
                    return Err(dispatch_refusal_with_lanes(
                        DispatchReasonCode::InvalidOrderingContract,
                        format!(
                            "dispatch refused: lanes '{}' and '{}' have multiple integration ordering contracts",
                            left.lane_id, right.lane_id
                        ),
                        vec![left.lane_id.clone(), right.lane_id.clone()],
                        Some(resource),
                        report,
                    ));
                }
                return Err(dispatch_refusal_with_lanes(
                    kind.refusal_code(),
                    format!(
                        "dispatch refused: lanes '{}' and '{}' both claim {} '{}' without one integration owner and ordering contract",
                        left.lane_id,
                        right.lane_id,
                        kind.label(),
                        resource
                    ),
                    vec![left.lane_id.clone(), right.lane_id.clone()],
                    Some(resource),
                    report,
                ));
            }
        }
    }
    Ok(())
}

fn lane_collisions(left: &LaneClaim, right: &LaneClaim) -> Vec<(MutableResourceKind, String)> {
    let mut collisions = Vec::new();
    let left_files = left
        .mutable_files
        .iter()
        .filter_map(|file| normalize_file_claim(file))
        .collect::<Vec<_>>();
    let right_files = right
        .mutable_files
        .iter()
        .filter_map(|file| normalize_file_claim(file))
        .collect::<Vec<_>>();
    for left_file in &left_files {
        for right_file in &right_files {
            if file_claims_overlap(left_file, right_file) {
                let resource = if left_file == right_file {
                    left_file.clone()
                } else {
                    format!("{left_file} <-> {right_file}")
                };
                collisions.push((MutableResourceKind::File, resource));
            }
        }
    }
    for schema in left.mutable_schemas.intersection(&right.mutable_schemas) {
        collisions.push((MutableResourceKind::Schema, schema.clone()));
    }
    for interface in left
        .mutable_interfaces
        .intersection(&right.mutable_interfaces)
    {
        collisions.push((MutableResourceKind::Interface, interface.clone()));
    }
    collisions
}

fn normalize_file_claim(claim: &str) -> Option<String> {
    if claim.is_empty()
        || claim != claim.trim()
        || claim.starts_with('/')
        || claim.contains('\\')
        || claim.chars().any(char::is_control)
        || claim
            .bytes()
            .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b']' | b'{' | b'}'))
    {
        return None;
    }
    let bytes = claim.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    let components = claim.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return None;
    }
    Some(components.join("/"))
}

fn file_claims_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

impl Default for SpawnGovernor {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_SESSIONS,
            DEFAULT_SPAWN_RATE_PER_MIN,
            DEFAULT_SPAWN_BURST,
        )
    }
}

/// Format a per-minute rate without a trailing `.0` for whole numbers, so the
/// error strings read `20/min`, not `20.0/min`.
fn fmt_rate(rate: f64) -> String {
    if (rate.fract()).abs() < f64::EPSILON {
        format!("{}", rate as i64)
    } else {
        format!("{rate}")
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| *n > 0.0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn strings(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn lane(lane_id: &str) -> LaneClaim {
        LaneClaim {
            lane_id: lane_id.to_owned(),
            owner_id: format!("owner-{lane_id}"),
            dependencies: Some(BTreeSet::new()),
            mutable_files: BTreeSet::new(),
            mutable_schemas: BTreeSet::new(),
            mutable_interfaces: BTreeSet::new(),
        }
    }

    fn healthy_capacity() -> RuntimeCapacity {
        RuntimeCapacity {
            live_sessions: 4,
            machine_healthy: true,
            machine_session_capacity: 32,
            provider_session_capacity: 32,
            provider_live_sessions: 4,
            provider_capacity_status: ProviderCapacityStatus {
                source: "test-telemetry".into(),
                degraded: false,
                detail: None,
            },
            available_worktrees: 16,
            active_captains: 2,
            active_captain_ships: strings(&["ship-a", "ship-b"]),
            live_cortana: 1,
            live_fleet_admins: 1,
            live_ship_admins: 2,
            live_ship_admin_scopes: [
                ("ship-a".to_string(), 1usize),
                ("ship-b".to_string(), 1usize),
            ]
            .into_iter()
            .collect(),
            live_recovery_sessions: 0,
        }
    }

    fn preflight(lanes: Vec<LaneClaim>) -> DispatchPreflight {
        let requested_provider_lanes = lanes.len();
        DispatchPreflight {
            requested_lanes: lanes,
            requested_provider_lanes,
            admission_purpose: AdmissionPurpose::Ordinary,
            ship_admin_scope: None,
            active_lanes: Vec::new(),
            satisfied_dependencies: BTreeSet::new(),
            integration_contracts: Vec::new(),
            capacity: healthy_capacity(),
        }
    }

    #[test]
    fn adaptive_preflight_allows_more_than_four_independent_lanes() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let request = preflight(
            (1..=6)
                .map(|index| lane(&format!("lane-{index}")))
                .collect(),
        );

        let report = governor.preflight_dispatch(&request).unwrap();

        assert_eq!(report.requested_lanes, 6);
        assert_eq!(report.reservations.recovery.deficit, 1);
        assert!(report.effective_lane_headroom >= 6);
    }

    #[test]
    fn legacy_capacity_report_without_provider_provenance_remains_readable() {
        let report = SpawnGovernor::new(32, 20.0, 8.0)
            .preflight_dispatch(&preflight(vec![lane("lane-legacy")]))
            .unwrap();
        let mut value = serde_json::to_value(report).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("providerSessionLimit");
        object.remove("providerLiveSessions");
        object.remove("providerCapacityStatus");
        object.remove("requestedProviderLanes");
        object.remove("providerHeadroomAfterReservations");

        let legacy: CapacityReport = serde_json::from_value(value).unwrap();

        assert_eq!(legacy.provider_session_limit, 0);
        assert_eq!(legacy.provider_live_sessions, 0);
        assert_eq!(legacy.requested_provider_lanes, legacy.requested_lanes);
        assert_eq!(
            legacy.provider_headroom_after_reservations,
            legacy
                .provider_headroom
                .saturating_sub(legacy.reservations.total_deficit)
        );
        assert_eq!(legacy.provider_capacity_status.source, "legacy-unspecified");
        assert!(legacy.provider_capacity_status.degraded);
    }

    #[test]
    fn provider_reservations_and_raw_quota_have_distinct_refusal_codes() {
        let governor = SpawnGovernor::new(16, 20.0, 8.0);
        let mut reserved = preflight(vec![lane("lane-1"), lane("lane-2")]);
        reserved.capacity.live_sessions = 3;
        reserved.capacity.machine_session_capacity = 16;
        reserved.capacity.provider_session_capacity = 2;
        reserved.capacity.provider_live_sessions = 0;
        reserved.capacity.active_captains = 0;
        reserved.capacity.active_captain_ships.clear();
        reserved.capacity.live_cortana = 0;
        reserved.capacity.live_fleet_admins = 1;
        reserved.capacity.live_recovery_sessions = 1;
        reserved.capacity.live_ship_admins = 0;
        reserved.capacity.live_ship_admin_scopes.clear();

        let reserved_refusal = governor.preflight_dispatch(&reserved).unwrap_err();
        assert_eq!(reserved_refusal.code, DispatchReasonCode::ReservedCapacity);
        assert_eq!(reserved_refusal.capacity.provider_headroom, 2);
        assert_eq!(
            reserved_refusal
                .capacity
                .provider_headroom_after_reservations,
            1
        );
        assert!(reserved_refusal
            .capacity
            .limiting_factors
            .contains(&DispatchReasonCode::ReservedCapacity));
        assert!(!reserved_refusal
            .capacity
            .limiting_factors
            .contains(&DispatchReasonCode::ProviderCapacity));

        let mut raw = reserved;
        raw.capacity.provider_session_capacity = 1;
        let raw_refusal = governor.preflight_dispatch(&raw).unwrap_err();
        assert_eq!(raw_refusal.code, DispatchReasonCode::ProviderCapacity);
        assert!(raw_refusal
            .capacity
            .limiting_factors
            .contains(&DispatchReasonCode::ProviderCapacity));
    }

    #[test]
    fn generic_lane_ignores_full_provider_quota_but_still_consumes_session_capacity() {
        let governor = SpawnGovernor::new(8, 20.0, 8.0);
        let mut generic = preflight(vec![lane("shell")]);
        generic.requested_provider_lanes = 0;
        generic.capacity.live_sessions = 4;
        generic.capacity.provider_session_capacity = 1;
        generic.capacity.provider_live_sessions = 1;
        generic.capacity.live_cortana = 1;
        generic.capacity.live_fleet_admins = 1;
        generic.capacity.live_recovery_sessions = 1;

        assert!(governor.preflight_dispatch(&generic).is_ok());

        generic.capacity.live_sessions = 8;
        assert_eq!(
            governor.preflight_dispatch(&generic).unwrap_err().code,
            DispatchReasonCode::ConfiguredCapacity
        );
    }

    #[test]
    fn privileged_reservations_follow_stable_purpose_priority() {
        let case = |limit: usize,
                    live_sessions: usize,
                    live_cortana: usize,
                    live_recovery_sessions: usize,
                    live_fleet_admins: usize,
                    purpose: AdmissionPurpose| {
            let governor = SpawnGovernor::new(limit, 20.0, 8.0);
            let mut request = preflight(vec![lane("privileged")]);
            request.admission_purpose = purpose;
            request.capacity.live_sessions = live_sessions;
            request.capacity.machine_session_capacity = limit;
            request.capacity.provider_session_capacity = limit;
            request.capacity.provider_live_sessions = live_sessions;
            request.capacity.active_captains = 0;
            request.capacity.active_captain_ships.clear();
            request.capacity.live_cortana = live_cortana;
            request.capacity.live_recovery_sessions = live_recovery_sessions;
            request.capacity.live_fleet_admins = live_fleet_admins;
            request.capacity.live_ship_admins = 0;
            request.capacity.live_ship_admin_scopes.clear();
            governor.preflight_dispatch(&request)
        };

        assert!(case(1, 0, 0, 0, 0, AdmissionPurpose::Cortana).is_ok());
        assert_eq!(
            case(1, 0, 0, 0, 0, AdmissionPurpose::Recovery)
                .unwrap_err()
                .code,
            DispatchReasonCode::ReservedCapacity
        );
        assert!(case(2, 1, 1, 0, 0, AdmissionPurpose::Recovery).is_ok());
        assert!(case(3, 2, 1, 1, 0, AdmissionPurpose::FleetAdmin).is_ok());

        let mut ship = preflight(vec![lane("ship-admin")]);
        ship.admission_purpose = AdmissionPurpose::ShipAdmin;
        ship.ship_admin_scope = Some("ship-a".into());
        ship.capacity.live_sessions = 3;
        ship.capacity.machine_session_capacity = 4;
        ship.capacity.provider_session_capacity = 4;
        ship.capacity.provider_live_sessions = 3;
        ship.capacity.active_captains = 1;
        ship.capacity.active_captain_ships = strings(&["ship-a"]);
        ship.capacity.live_cortana = 1;
        ship.capacity.live_recovery_sessions = 1;
        ship.capacity.live_fleet_admins = 1;
        ship.capacity.live_ship_admins = 0;
        ship.capacity.live_ship_admin_scopes.clear();
        assert!(SpawnGovernor::new(4, 20.0, 8.0)
            .preflight_dispatch(&ship)
            .is_ok());

        ship.admission_purpose = AdmissionPurpose::FleetAdmin;
        ship.ship_admin_scope = None;
        ship.capacity.live_fleet_admins = 1;
        assert_eq!(
            SpawnGovernor::new(4, 20.0, 8.0)
                .preflight_dispatch(&ship)
                .unwrap_err()
                .code,
            DispatchReasonCode::ReservedCapacity,
            "an extra Fleet Admin uses ordinary headroom and cannot consume the missing Ship Admin slot"
        );
    }

    #[test]
    fn ship_admin_reservation_is_scope_aware_and_lexically_stable() {
        let governor = SpawnGovernor::new(5, 20.0, 8.0);
        let mut request = preflight(vec![lane("ship-admin")]);
        request.admission_purpose = AdmissionPurpose::ShipAdmin;
        request.capacity.live_sessions = 4;
        request.capacity.machine_session_capacity = 5;
        request.capacity.provider_session_capacity = 5;
        request.capacity.provider_live_sessions = 4;
        request.capacity.active_captains = 2;
        request.capacity.active_captain_ships = strings(&["ship-a", "ship-b"]);
        request.capacity.live_cortana = 1;
        request.capacity.live_recovery_sessions = 1;
        request.capacity.live_fleet_admins = 1;
        request.capacity.live_ship_admins = 1;
        request.capacity.live_ship_admin_scopes =
            [("ship-a".to_string(), 1usize)].into_iter().collect();

        request.ship_admin_scope = Some("ship-a".into());
        assert_eq!(
            governor.preflight_dispatch(&request).unwrap_err().code,
            DispatchReasonCode::ReservedCapacity
        );

        request.ship_admin_scope = Some("ship-b".into());
        assert!(governor.preflight_dispatch(&request).is_ok());

        request.capacity.live_ship_admin_scopes.clear();
        request.capacity.live_ship_admins = 0;
        request.ship_admin_scope = Some("ship-b".into());
        assert_eq!(
            governor.preflight_dispatch(&request).unwrap_err().code,
            DispatchReasonCode::ReservedCapacity,
            "ship-b cannot consume ship-a's lexically earlier missing reservation"
        );
        request.ship_admin_scope = Some("ship-a".into());
        assert!(governor.preflight_dispatch(&request).is_ok());
    }

    #[test]
    fn preflight_preserves_supervisor_admin_and_recovery_reservations() {
        let governor = SpawnGovernor::new(10, 20.0, 8.0);
        let mut request = preflight(
            (1..=4)
                .map(|index| lane(&format!("lane-{index}")))
                .collect(),
        );
        request.capacity = RuntimeCapacity {
            live_sessions: 2,
            machine_healthy: true,
            machine_session_capacity: 10,
            provider_session_capacity: 20,
            provider_live_sessions: 2,
            provider_capacity_status: ProviderCapacityStatus {
                source: "test-telemetry".into(),
                degraded: false,
                detail: None,
            },
            available_worktrees: 10,
            active_captains: 2,
            active_captain_ships: strings(&["ship-a", "ship-b"]),
            live_cortana: 0,
            live_fleet_admins: 0,
            live_ship_admins: 0,
            live_ship_admin_scopes: BTreeMap::new(),
            live_recovery_sessions: 0,
        };

        let refusal = governor.preflight_dispatch(&request).unwrap_err();

        assert_eq!(refusal.code, DispatchReasonCode::ReservedCapacity);
        assert_eq!(refusal.capacity.reservations.total_deficit, 5);
        assert_eq!(refusal.capacity.session_headroom_before_reservations, 8);
        assert_eq!(refusal.capacity.session_headroom_after_reservations, 3);
    }

    #[test]
    fn provider_machine_health_and_machine_capacity_limit_dispatch() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut unhealthy = preflight(vec![lane("lane-1")]);
        unhealthy.capacity.machine_healthy = false;
        assert_eq!(
            governor.preflight_dispatch(&unhealthy).unwrap_err().code,
            DispatchReasonCode::MachineUnhealthy
        );

        let mut provider_limited = preflight(vec![lane("lane-1"), lane("lane-2")]);
        provider_limited.capacity.live_recovery_sessions = 1;
        provider_limited.capacity.provider_session_capacity = 5;
        provider_limited.capacity.provider_live_sessions = 4;
        assert_eq!(
            governor
                .preflight_dispatch(&provider_limited)
                .unwrap_err()
                .code,
            DispatchReasonCode::ProviderCapacity
        );

        let mut machine_limited = preflight(vec![lane("lane-1"), lane("lane-2")]);
        machine_limited.capacity.live_recovery_sessions = 1;
        machine_limited.capacity.machine_session_capacity = 5;
        assert_eq!(
            governor
                .preflight_dispatch(&machine_limited)
                .unwrap_err()
                .code,
            DispatchReasonCode::MachineCapacity
        );
    }

    #[test]
    fn worktree_availability_limits_dispatch() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut request = preflight(vec![lane("lane-1"), lane("lane-2")]);
        request.capacity.live_recovery_sessions = 1;
        request.capacity.available_worktrees = 1;

        let refusal = governor.preflight_dispatch(&request).unwrap_err();

        assert_eq!(refusal.code, DispatchReasonCode::WorktreeCapacity);
        assert_eq!(refusal.capacity.worktree_headroom, 1);
    }

    #[test]
    fn lane_ownership_and_dependencies_must_be_explicit_and_isolated() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut missing_owner = lane("lane-1");
        missing_owner.owner_id.clear();
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![missing_owner]))
                .unwrap_err()
                .code,
            DispatchReasonCode::MissingOwnership
        );

        let mut missing_dependencies = lane("lane-1");
        missing_dependencies.dependencies = None;
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![missing_dependencies]))
                .unwrap_err()
                .code,
            DispatchReasonCode::MissingDependencies
        );

        let first = lane("lane-1");
        let mut second = lane("lane-2");
        second.owner_id = first.owner_id.clone();
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![first, second]))
                .unwrap_err()
                .code,
            DispatchReasonCode::DuplicateOwner
        );
    }

    #[test]
    fn unknown_and_cyclic_dependencies_are_rejected() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut unknown = lane("lane-1");
        unknown.dependencies = Some(strings(&["missing"]));
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![unknown]))
                .unwrap_err()
                .code,
            DispatchReasonCode::UnknownDependency
        );

        let mut first = lane("lane-1");
        first.dependencies = Some(strings(&["lane-2"]));
        let mut second = lane("lane-2");
        second.dependencies = Some(strings(&["lane-1"]));
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![first, second]))
                .unwrap_err()
                .code,
            DispatchReasonCode::DependencyCycle
        );
    }

    #[test]
    fn known_but_incomplete_dependencies_are_not_dispatched_early() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let dependency = lane("lane-1");
        let mut dependent = lane("lane-2");
        dependent.dependencies = Some(strings(&["lane-1"]));
        let request = preflight(vec![dependency, dependent]);

        let refusal = governor.preflight_dispatch(&request).unwrap_err();

        assert_eq!(refusal.code, DispatchReasonCode::UnmetDependency);
        assert_eq!(refusal.lane_ids, vec!["lane-2"]);
        assert_eq!(refusal.resource.as_deref(), Some("lane-1"));
    }

    #[test]
    fn completed_lane_identities_cannot_be_reused() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut request = preflight(vec![lane("lane-1")]);
        request.satisfied_dependencies.insert("lane-1".into());

        let refusal = governor.preflight_dispatch(&request).unwrap_err();

        assert_eq!(refusal.code, DispatchReasonCode::DuplicateLane);
        assert_eq!(refusal.lane_ids, vec!["lane-1"]);
    }

    #[test]
    fn mutable_file_schema_and_interface_collisions_are_denied() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);

        let mut file_owner = lane("lane-1");
        file_owner.mutable_files = strings(&["apps/core"]);
        let mut nested_file_owner = lane("lane-2");
        nested_file_owner.mutable_files = strings(&["apps/core/src/api.rs"]);
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![file_owner, nested_file_owner]))
                .unwrap_err()
                .code,
            DispatchReasonCode::MutableFileCollision
        );

        let mut schema_owner = lane("lane-1");
        schema_owner.mutable_schemas = strings(&["captains-v4"]);
        let mut other_schema_owner = lane("lane-2");
        other_schema_owner.mutable_schemas = strings(&["captains-v4"]);
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![schema_owner, other_schema_owner]))
                .unwrap_err()
                .code,
            DispatchReasonCode::SchemaCollision
        );

        let mut interface_owner = lane("lane-1");
        interface_owner.mutable_interfaces = strings(&["control.dispatch"]);
        let mut other_interface_owner = lane("lane-2");
        other_interface_owner.mutable_interfaces = strings(&["control.dispatch"]);
        assert_eq!(
            governor
                .preflight_dispatch(&preflight(vec![interface_owner, other_interface_owner]))
                .unwrap_err()
                .code,
            DispatchReasonCode::InterfaceCollision
        );
    }

    #[test]
    fn mutable_file_claims_must_be_normalized_repository_relative_paths() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        for invalid in [
            "",
            " ",
            ".",
            "./apps/core",
            "/apps/core",
            "C:/apps/core",
            "c:apps/core",
            r"C:\apps\core",
            r"\\server\repo\apps\core",
            "apps/../core",
            "apps//core",
            "apps/core/",
            r"apps\core",
            "apps/*/core",
            "apps/**",
            "apps/core?.rs",
            "apps/[ab]/core",
            "apps/{core,ui}",
            " apps/core",
            "apps/core ",
        ] {
            let mut owner = lane("lane-invalid");
            owner.mutable_files = strings(&[invalid]);
            let refusal = governor
                .preflight_dispatch(&preflight(vec![owner]))
                .unwrap_err();
            assert_eq!(
                refusal.code,
                DispatchReasonCode::InvalidResourceClaim,
                "claim {invalid:?} must be rejected"
            );
            assert_eq!(refusal.resource.as_deref(), Some(invalid));
        }
    }

    #[test]
    fn logical_directory_prefixes_collide_across_independent_worktrees() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut first_worktree = lane("lane-worktree-a");
        first_worktree.mutable_files = strings(&["apps/desktop/src"]);
        let mut second_worktree = lane("lane-worktree-b");
        second_worktree.mutable_files = strings(&["apps/desktop/src/App.tsx"]);

        let refusal = governor
            .preflight_dispatch(&preflight(vec![first_worktree, second_worktree]))
            .unwrap_err();

        assert_eq!(refusal.code, DispatchReasonCode::MutableFileCollision);
        assert_eq!(
            refusal.resource.as_deref(),
            Some("apps/desktop/src <-> apps/desktop/src/App.tsx")
        );
    }

    #[test]
    fn one_integration_owner_and_ordering_contract_allow_shared_resources() {
        let governor = SpawnGovernor::new(32, 20.0, 8.0);
        let mut first = lane("lane-1");
        first.mutable_files = strings(&["apps/core/src/api.rs"]);
        first.mutable_schemas = strings(&["captains-v4"]);
        first.mutable_interfaces = strings(&["control.dispatch"]);
        let mut second = lane("lane-2");
        second.mutable_files = first.mutable_files.clone();
        second.mutable_schemas = first.mutable_schemas.clone();
        second.mutable_interfaces = first.mutable_interfaces.clone();
        let mut request = preflight(vec![first, second]);
        request.integration_contracts = vec![IntegrationContract {
            contract_id: "control-integration-order".to_owned(),
            integration_owner: "owner-integration".to_owned(),
            ordered_lane_ids: vec!["lane-1".to_owned(), "lane-2".to_owned()],
        }];

        assert!(governor.preflight_dispatch(&request).is_ok());
    }

    #[test]
    fn structured_reason_codes_serialize_stably() {
        assert_eq!(
            serde_json::to_string(&DispatchReasonCode::ReservedCapacity).unwrap(),
            "\"reserved-capacity\""
        );
        assert_eq!(
            DispatchReasonCode::InterfaceCollision.as_str(),
            "interface-collision"
        );
    }

    #[test]
    fn normal_captain_fanout_burst_is_admitted() {
        // The single most important test (design's spec): a captain fanning out 6
        // crew in an instant burst must NOT be refused. Default burst is 8.
        let gov = SpawnGovernor::default();
        let t0 = Instant::now();
        for i in 0..6 {
            assert!(
                gov.check_spawn(i, t0).is_ok(),
                "spawn {i} of a 6-crew burst was refused"
            );
        }
    }

    #[test]
    fn multi_level_simultaneous_fanout_stays_under_burst() {
        // General spawns 3 captains, each spawns... within one instant we admit up
        // to the burst (8). A realistic near-simultaneous wave of 8 passes.
        let gov = SpawnGovernor::default();
        let t0 = Instant::now();
        for i in 0..8 {
            assert!(
                gov.check_spawn(i, t0).is_ok(),
                "spawn {i} refused within burst"
            );
        }
        // The 9th instantaneous spawn (beyond burst, no time to refill) is refused
        // as a rate limit - the runaway-loop signal.
        let refusal = gov.check_spawn(8, t0).unwrap_err();
        assert_eq!(refusal.code, "refused-rate");
        assert!(refusal.message.contains("spawn rate limit (20/min)"));
    }

    #[test]
    fn spawn_rate_refills_over_time() {
        let gov = SpawnGovernor::default();
        let t0 = Instant::now();
        // Drain the burst.
        for i in 0..8 {
            assert!(gov.check_spawn(i, t0).is_ok());
        }
        assert!(gov.check_spawn(8, t0).is_err());
        // 20/min = one token every 3s. After 3s exactly one more is admitted.
        let t1 = t0 + Duration::from_secs(3);
        assert!(gov.check_spawn(8, t1).is_ok());
        assert!(gov.check_spawn(8, t1).is_err());
    }

    #[test]
    fn concurrent_cap_refuses_at_max() {
        let gov = SpawnGovernor::new(64, 20.0, 8.0);
        let t0 = Instant::now();
        // At the cap the spawn is refused with the exact §5 message, and no rate
        // token is spent (checked before the bucket).
        let refusal = gov.check_spawn(64, t0).unwrap_err();
        assert_eq!(refusal.code, "refused-cap");
        assert_eq!(
            refusal.message,
            "refused: fleet at concurrent-session cap (64/64); \
             close sessions or raise T_HUB_MAX_SESSIONS"
        );
        // One below the cap still passes.
        assert!(gov.check_spawn(63, t0).is_ok());
    }

    #[test]
    fn hard_ceiling_cannot_be_exceeded_by_override() {
        // A fat-fingered override is clamped to the hard ceiling.
        let gov = SpawnGovernor::new(100_000, 1000.0, 1000.0);
        assert_eq!(gov.max_sessions(), HARD_SESSION_CEILING);
        let t0 = Instant::now();
        let refusal = gov.check_spawn(HARD_SESSION_CEILING, t0).unwrap_err();
        assert_eq!(refusal.code, "refused-ceiling");
        assert!(refusal.message.contains("hard session ceiling (128)"));
    }

    #[test]
    fn destructive_throttle_burst_then_refuse() {
        let gov = SpawnGovernor::default();
        let t0 = Instant::now();
        // Burst 10 for destructive ops.
        for _ in 0..10 {
            assert!(gov.check_destructive(t0).is_ok());
        }
        let refusal = gov.check_destructive(t0).unwrap_err();
        assert_eq!(refusal.code, "refused-rate");
        assert!(refusal
            .message
            .contains("destructive-command rate limit (15/min)"));
        // 15/min = one every 4s; after 4s one more teardown is admitted.
        let t1 = t0 + Duration::from_secs(4);
        assert!(gov.check_destructive(t1).is_ok());
    }

    #[test]
    fn env_override_parses_and_clamps() {
        // Guard the env-parse helpers directly (no process-global env mutation).
        assert_eq!(env_usize("definitely_unset_var_xyz", 64), 64);
        assert_eq!(env_f64("definitely_unset_var_xyz", 20.0), 20.0);
    }
}
