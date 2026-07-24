//! Idempotency: the completed-request outcome cache (`RequestCache`), split out
//! of `control.rs` to shrink that module. Dedupes retried `requestId`-keyed
//! spawn-class requests - a completed id replays its stored outcome, a racing
//! duplicate is told InFlight. `ControlContext` holds a `RequestCache`; the
//! dispatch path drives it via begin/complete (`BeginOutcome`), and
//! `handlers_status::get_request_status` reads `RequestStatus`.

use super::*;

// ---------------------------------------------------------------------------
// Idempotency: completed-request outcome cache (ask #1)
// ---------------------------------------------------------------------------

/// How many completed request outcomes to retain before evicting the oldest.
/// Spawn-class traffic is low volume (a fleet spawns dozens, not thousands, of
/// sessions), so a few hundred entries covers every realistic in-flight retry
/// window with a trivial memory cost.
pub(super) const REQUEST_CACHE_CAPACITY: usize = 512;

/// How long a completed outcome stays queryable via `get_request_status`. Longer
/// than any client's overall retry deadline so a caller recovering from an
/// ambiguous response leg can always still learn what happened; short enough that
/// the cache is self-cleaning without the eviction cap ever being the sole bound.
pub(super) const REQUEST_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Default window an InFlight reservation survives before it is presumed DEAD and
/// reaped, so a handler thread that panicked or hung (e.g. a wedged `git worktree
/// add`, Incident D) cannot leave a request id permanently blocking every retry.
///
/// 600s (matching [`REQUEST_CACHE_TTL`]) deliberately sits WELL above any realistic
/// slow spawn - including a `git worktree add` against the OneDrive-backed store
/// (the very slow-I/O surface Incident D was about). At 120s a slow-but-ALIVE
/// create_worktree could be reaped mid-flight, letting a retry see `Fresh` and both
/// apply -> the exact A/B duplicate (each spawn mints a fresh uuid). 600s makes a
/// still-running op far less plausible than a truly dead one; the env override
/// (`T_HUB_REQUEST_INFLIGHT_REAP_SECS`) lets an operator tune it.
///
/// This window is now the OUTER BOUND, not the only guard: the full fix landed as
/// [`reprobe_reaped_request`] - on reaping a reservation, a same-id retry re-probes
/// reality (`git worktree list` for a `create_worktree`) BEFORE re-applying, so a
/// reaped-but-alive op resolves against what actually happened instead of being
/// blindly duplicated regardless of the window. The window still bounds how long a
/// truly-dead reservation blocks retries; the re-probe removes the duplicate risk.
pub(super) const REQUEST_INFLIGHT_REAP_DEFAULT: std::time::Duration = std::time::Duration::from_secs(600);

/// The effective InFlight reap window: `$T_HUB_REQUEST_INFLIGHT_REAP_SECS` (seconds)
/// if set to a positive integer, else [`REQUEST_INFLIGHT_REAP_DEFAULT`].
pub(super) fn inflight_reap_window() -> std::time::Duration {
    std::env::var("T_HUB_REQUEST_INFLIGHT_REAP_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(REQUEST_INFLIGHT_REAP_DEFAULT)
}

/// The state of a request id in the [`RequestCache`].
pub(super) enum RequestSlot {
    /// A first caller reserved this id and is running the command now. A
    /// concurrent duplicate (a retry that raced the original, Incident B) sees
    /// this and must NOT run the command again.
    InFlight {
        since: std::time::Instant,
        signature: String,
        reservation: u64,
    },
    /// The command finished; its outcome is cached for replay to a retry.
    Done {
        at: std::time::Instant,
        signature: String,
        outcome: Result<Value, String>,
    },
}

/// What [`RequestCache::begin`] decided for an incoming request id.
pub(super) enum BeginOutcome {
    /// This id is new (never seen): reserved InFlight, the caller must run the
    /// command and then call [`RequestCache::finish`].
    Fresh,
    /// This id was a still-InFlight reservation that aged PAST the reap window and
    /// was just presumed-dead + re-reserved for this caller (M1 full fix). Behaves
    /// like [`Fresh`] EXCEPT the caller must first RE-PROBE reality
    /// ([`reprobe_reaped_request`]): a slow-but-alive original (e.g. a `git worktree
    /// add` on the OneDrive-backed store) may have actually LANDED before the reap,
    /// so blindly re-applying would duplicate it. If the artifact already exists,
    /// resolve the retry against it; otherwise the original truly died - apply fresh.
    FreshAfterReap,
    /// This exact request already completed - replay its outcome, do NOT re-run.
    Duplicate(Result<Value, String>),
    /// This exact request is still running on another connection - do NOT re-run;
    /// the caller should poll `get_request_status` (or retry) until it completes.
    InFlight,
}

/// The queryable status of a request id (`get_request_status`).
pub(super) enum RequestStatus {
    Unknown,
    InFlight,
    Completed(Result<Value, String>),
}

/// A bounded, TTL'd cache of spawn-class request outcomes keyed by a
/// client-supplied `requestId` (ask #1). It makes a spawn-class command safely
/// RETRYABLE across an ambiguous response leg: the server applies the side effect
/// exactly once per id, and a retry of the same id replays the stored outcome
/// instead of double-applying (the Incident A/B duplicate-maker). A concurrent
/// duplicate that races the original is told InFlight rather than spawning again.
///
/// Keyed only when the client opts in by supplying a `requestId`; a request with
/// no id behaves exactly as before (no dedup), preserving backward compatibility.
pub struct RequestCache {
    inner: Mutex<RequestCacheInner>,
    capacity: usize,
    ttl: std::time::Duration,
    /// Window after which a still-InFlight reservation is presumed dead and reaped
    /// (see [`inflight_reap_window`]). A field (not the bare const) so a test can
    /// drive a tiny one and an operator can tune it via env.
    inflight_reap: std::time::Duration,
}

#[derive(Default)]
pub(super) struct RequestCacheInner {
    slots: std::collections::HashMap<String, RequestSlot>,
    /// Insertion order of ids, for capacity eviction (oldest first).
    order: std::collections::VecDeque<String>,
    next_reservation: u64,
}

impl RequestCache {
    pub(super) fn new() -> Self {
        Self {
            inner: Mutex::new(RequestCacheInner::default()),
            capacity: REQUEST_CACHE_CAPACITY,
            ttl: REQUEST_CACHE_TTL,
            inflight_reap: inflight_reap_window(),
        }
    }

    /// Test-only constructor with explicit bounds so eviction/TTL/reap behavior can
    /// be exercised without inserting the full production capacity or waiting out
    /// the real windows.
    #[cfg(test)]
    pub(super) fn with_bounds(
        capacity: usize,
        ttl: std::time::Duration,
        inflight_reap: std::time::Duration,
    ) -> Self {
        Self {
            inner: Mutex::new(RequestCacheInner::default()),
            capacity,
            ttl,
            inflight_reap,
        }
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, RequestCacheInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drop entries that have aged out: a Done entry past the TTL, or an InFlight
    /// reservation past `inflight_reap` (presumed dead - a panicked or hung handler
    /// must never leave an id permanently blocking retries).
    pub(super) fn evict_expired(
        inner: &mut RequestCacheInner,
        now: std::time::Instant,
        ttl: std::time::Duration,
        inflight_reap: std::time::Duration,
    ) {
        let RequestCacheInner { slots, order, .. } = inner;
        order.retain(|id| {
            let expired = match slots.get(id) {
                Some(RequestSlot::Done { at, .. }) => now.duration_since(*at) >= ttl,
                Some(RequestSlot::InFlight { since, .. }) => {
                    now.duration_since(*since) >= inflight_reap
                }
                None => true,
            };
            if expired {
                slots.remove(id);
            }
            !expired
        });
    }

    /// Reserve `id` for a first caller, or report that it is a duplicate/in-flight.
    /// The reservation (InFlight) and the completed-outcome lookup are one atomic
    /// step so two racing retries can never both reserve the same id.
    #[cfg(test)]
    pub(super) fn begin(&self, id: &str) -> BeginOutcome {
        self.begin_bound(id, "")
    }

    #[cfg(test)]
    pub(super) fn begin_bound(&self, id: &str, signature: &str) -> BeginOutcome {
        self.begin_bound_with_reservation(id, signature).0
    }

    pub(super) fn begin_bound_with_reservation(
        &self,
        id: &str,
        signature: &str,
    ) -> (BeginOutcome, Option<u64>) {
        let now = std::time::Instant::now();
        let mut inner = self.lock();
        // M1 full fix: was THIS id a reservation that just aged out? Capture it
        // BEFORE `evict_expired` removes it, so the re-reservation below can tell a
        // genuinely-new request (Fresh) from a reaped-but-maybe-alive retry
        // (FreshAfterReap) that must re-probe reality before re-applying.
        let reaped = matches!(
            inner.slots.get(id),
            Some(RequestSlot::InFlight { since, .. }) if now.duration_since(*since) >= self.inflight_reap
        );
        Self::evict_expired(&mut inner, now, self.ttl, self.inflight_reap);
        match inner.slots.get(id) {
            Some(RequestSlot::Done {
                signature: existing,
                outcome,
                ..
            }) => {
                if existing == signature {
                    (BeginOutcome::Duplicate(outcome.clone()), None)
                } else {
                    (
                        BeginOutcome::Duplicate(Err(
                            "request_conflict: requestId is already bound to a different command or argument set"
                                .to_string(),
                        )),
                        None,
                    )
                }
            }
            Some(RequestSlot::InFlight {
                signature: existing,
                ..
            }) => {
                if existing == signature {
                    (BeginOutcome::InFlight, None)
                } else {
                    (
                        BeginOutcome::Duplicate(Err(
                            "request_conflict: requestId is already bound to a different command or argument set"
                                .to_string(),
                        )),
                        None,
                    )
                }
            }
            None => {
                inner.next_reservation = inner.next_reservation.saturating_add(1).max(1);
                let reservation = inner.next_reservation;
                inner.slots.insert(
                    id.to_string(),
                    RequestSlot::InFlight {
                        since: now,
                        signature: signature.to_string(),
                        reservation,
                    },
                );
                inner.order.push_back(id.to_string());
                // Capacity bound: evict the oldest COMPLETED entries (never an
                // in-flight reservation) until back under the cap.
                while inner.order.len() > self.capacity {
                    let Some(oldest) = inner.order.front().cloned() else {
                        break;
                    };
                    match inner.slots.get(&oldest) {
                        Some(RequestSlot::Done { .. }) | None => {
                            inner.order.pop_front();
                            inner.slots.remove(&oldest);
                        }
                        // Oldest is still running: stop evicting (the cap is soft
                        // under a burst of concurrent in-flight requests).
                        Some(RequestSlot::InFlight { .. }) => break,
                    }
                }
                if reaped {
                    (BeginOutcome::FreshAfterReap, Some(reservation))
                } else {
                    (BeginOutcome::Fresh, Some(reservation))
                }
            }
        }
    }

    /// Record the outcome of a request reserved by [`begin`](Self::begin), and
    /// return it (cloned) so the caller can respond with the very value now cached
    /// for any future retry.
    #[cfg(test)]
    pub(super) fn finish(&self, id: &str, outcome: Result<Value, String>) -> Result<Value, String> {
        let (reservation, signature, absent) = {
            let inner = self.lock();
            match inner.slots.get(id) {
                Some(RequestSlot::InFlight {
                    reservation,
                    signature,
                    ..
                }) => (Some(*reservation), Some(signature.clone()), false),
                None => (None, None, true),
                Some(RequestSlot::Done { .. }) => (None, None, false),
            }
        };
        match reservation {
            Some(reservation) => self.finish_reserved(
                id,
                reservation,
                signature.as_deref().expect("in-flight signature"),
                outcome,
            ),
            None if absent => self.finish_reserved(id, 0, "", outcome),
            None => outcome,
        }
    }

    pub(super) fn finish_reserved(
        &self,
        id: &str,
        reservation: u64,
        original_signature: &str,
        outcome: Result<Value, String>,
    ) -> Result<Value, String> {
        let mut inner = self.lock();
        let signature = match inner.slots.get(id) {
            Some(RequestSlot::InFlight {
                signature,
                reservation: current,
                ..
            }) if *current == reservation => signature.clone(),
            // A status query may reap an old reservation before its legitimate
            // handler returns. With no replacement owner, preserve that late
            // authoritative result under its original signature.
            None => original_signature.to_string(),
            // A reaped request may finish after a replacement reserved the same
            // id. Never let that stale completion overwrite or complete the newer
            // reservation/outcome.
            _ => return outcome,
        };
        // M2: normally `begin` already put the id in `order`, so we must NOT
        // double-insert. BUT if the reservation outlived the reap window (a
        // >`inflight_reap` handler still running), `evict_expired` already dropped
        // the id from BOTH maps - so this Done entry would be recorded in `slots`
        // with no `order` membership: never TTL/capacity-evictable, a permanent
        // leak that also breaches the cap and reports `completed` forever. Re-
        // establish order membership when (and only when) it is missing.
        if !inner.order.iter().any(|x| x == id) {
            inner.order.push_back(id.to_string());
        }
        inner.slots.insert(
            id.to_string(),
            RequestSlot::Done {
                at: std::time::Instant::now(),
                signature,
                outcome: outcome.clone(),
            },
        );
        outcome
    }

    /// Release an InFlight reservation WITHOUT recording an outcome - used when a
    /// pre-side-effect gate (the spawn governor) refuses a reserved request, so a
    /// later retry (after budget frees) is not permanently stuck seeing InFlight /
    /// a cached refusal. A no-op if the id already completed.
    #[cfg(test)]
    pub(super) fn cancel(&self, id: &str) {
        let mut inner = self.lock();
        if matches!(inner.slots.get(id), Some(RequestSlot::InFlight { .. })) {
            inner.slots.remove(id);
            inner.order.retain(|x| x != id);
        }
    }

    pub(super) fn cancel_reserved(&self, id: &str, reservation: u64) {
        let mut inner = self.lock();
        if matches!(
            inner.slots.get(id),
            Some(RequestSlot::InFlight {
                reservation: current,
                ..
            }) if *current == reservation
        ) {
            inner.slots.remove(id);
            inner.order.retain(|entry| entry != id);
        }
    }

    /// Query the status of a request id (`get_request_status`).
    pub(super) fn status(&self, id: &str) -> RequestStatus {
        let now = std::time::Instant::now();
        let mut inner = self.lock();
        Self::evict_expired(&mut inner, now, self.ttl, self.inflight_reap);
        match inner.slots.get(id) {
            None => RequestStatus::Unknown,
            Some(RequestSlot::InFlight { .. }) => RequestStatus::InFlight,
            Some(RequestSlot::Done { outcome, .. }) => RequestStatus::Completed(outcome.clone()),
        }
    }
}

impl Default for RequestCache {
    fn default() -> Self {
        Self::new()
    }
}

// --- Captain control leases (short-lived orchestration authority) ---
/// Short-lived authority minted for one exact durable orchestration identity.
///
/// The secret exists only in the app and MCP process memories. It is never part
/// of discovery, durable identity state, audit arguments, or global provider
/// configuration.
#[derive(Clone)]
pub(super) struct CaptainControlLease {
    pub(super) identity_id: String,
    pub(super) terminal_id: String,
    pub(super) authority: LeaseAuthority,
    pub(super) expires_at: Instant,
    pub(super) expires_at_epoch_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LeaseAuthority {
    Captain {
        ship_slug: String,
        project_id: String,
        generation: ScopedAuthorityGeneration,
    },
    Cortana {
        generation: u64,
    },
    DelegatedAdmin {
        grant_id: String,
        grant_generation: u64,
        role: crate::delegated_admin::DelegatedAdminRole,
        scope: crate::delegated_admin::AdminScope,
    },
}

#[derive(Default)]
pub(super) struct CaptainControlLeases {
    pub(super) state: Mutex<CaptainControlLeaseState>,
}

#[derive(Default)]
pub(super) struct CaptainControlLeaseState {
    pub(super) by_secret: HashMap<String, CaptainControlLease>,
    pub(super) by_identity: HashMap<String, String>,
}

pub(super) const CAPTAIN_CONTROL_LEASE_TTL: Duration = Duration::from_secs(90);
pub(super) const MAX_CAPTAIN_CONTROL_LEASES: usize = 1024;

pub(super) fn captain_control_lease_ttl() -> Duration {
    std::env::var("T_HUB_CONTROL_LEASE_TTL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.clamp(1, 90)))
        .unwrap_or(CAPTAIN_CONTROL_LEASE_TTL)
}

impl CaptainControlLeases {
    pub(super) fn retain_live(state: &mut CaptainControlLeaseState) {
        let now = Instant::now();
        state.by_secret.retain(|_, lease| lease.expires_at > now);
        state.by_identity.retain(|identity_id, secret| {
            state
                .by_secret
                .get(secret)
                .is_some_and(|lease| lease.identity_id == *identity_id)
        });
    }

    pub(super) fn issue(&self, lease: CaptainControlLease) -> (String, u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::retain_live(&mut state);
        if let Some(secret) = state.by_identity.get(&lease.identity_id).cloned() {
            if let Some(existing) = state.by_secret.get_mut(&secret) {
                if existing.terminal_id == lease.terminal_id
                    && existing.authority == lease.authority
                {
                    // Renewal is a sliding deadline, not merely an idempotent lookup.
                    // Advance both clocks while holding the one-per-identity maps'
                    // single lock so an MCP refresh cannot receive the old near-expiry
                    // deadline or race a second live credential into existence.
                    let minimum_monotonic_extension = existing
                        .expires_at
                        .checked_add(Duration::from_millis(1))
                        .unwrap_or(existing.expires_at);
                    existing.expires_at = lease.expires_at.max(minimum_monotonic_extension);
                    existing.expires_at_epoch_ms = lease
                        .expires_at_epoch_ms
                        .max(existing.expires_at_epoch_ms.saturating_add(1));
                    return (secret, existing.expires_at_epoch_ms);
                }
            }
            state.by_secret.remove(&secret);
            state.by_identity.remove(&lease.identity_id);
        }
        if state.by_secret.len() >= MAX_CAPTAIN_CONTROL_LEASES {
            if let Some(oldest) = state
                .by_secret
                .iter()
                .min_by_key(|(_, existing)| existing.expires_at)
                .map(|(secret, _)| secret.clone())
            {
                if let Some(evicted) = state.by_secret.remove(&oldest) {
                    state.by_identity.remove(&evicted.identity_id);
                }
            }
        }
        let secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires_at_epoch_ms = lease.expires_at_epoch_ms;
        state
            .by_identity
            .insert(lease.identity_id.clone(), secret.clone());
        state.by_secret.insert(secret.clone(), lease);
        (secret, expires_at_epoch_ms)
    }

    pub(super) fn get(&self, secret: &str) -> Option<CaptainControlLease> {
        if secret.is_empty() {
            return None;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::retain_live(&mut state);
        state
            .by_secret
            .iter()
            .find(|(candidate, _)| ct_token_eq(candidate, secret))
            .map(|(_, lease)| lease.clone())
    }

    pub(super) fn revoke_identity(&self, identity_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(secret) = state.by_identity.remove(identity_id) {
            state.by_secret.remove(&secret);
        }
        state
            .by_secret
            .retain(|_, lease| lease.identity_id != identity_id);
    }

    #[cfg(test)]
    pub(super) fn insert_test(&self, secret: &str, lease: CaptainControlLease) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .by_identity
            .insert(lease.identity_id.clone(), secret.to_string());
        state.by_secret.insert(secret.to_string(), lease);
    }
}
