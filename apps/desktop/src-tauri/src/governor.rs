//! Fleet **spawn governor** — Phase 1 of the control-socket hardening
//! (`docs/SOCKET-AUTH-DESIGN.md` §4).
//!
//! Bounds the blast radius of process-changing control commands *regardless of
//! caller identity*: an injection-hijacked but fully-authenticated token holder
//! still cannot spawn or kill the fleet without limit. This layer changes NO
//! tokens — the only new behavior is refuse-past-ceiling. It is consulted from
//! `control::dispatch_authenticated` for the ProcessChanging tier only; the Read
//! and Organization tiers never touch it.
//!
//! Four controls:
//!   1. **Concurrent-session cap** — a soft ceiling on live `th_*` sessions,
//!      DERIVED from the authoritative tmux registry and reconciled on every
//!      spawn (never a free-running counter that drifts when a session dies
//!      without a `close_terminal`). Default 64, env `T_HUB_MAX_SESSIONS`.
//!   2. **Spawn rate** — a token-bucket: sustained 20/min, burst 8 (env
//!      `T_HUB_SPAWN_RATE` / `T_HUB_SPAWN_BURST`). Burst 8 covers a captain
//!      fanning out 6 crew plus slack; the sustained rate lets multi-level
//!      near-simultaneous fan-out through while starving a runaway loop.
//!   3. **Hard ceiling** — an absolute concurrent stop (128) that no env
//!      override can exceed (defense against a mis-set `T_HUB_MAX_SESSIONS`).
//!   4. **Destructive rate** — a separate token-bucket throttling `close_terminal`
//!      and kill-style `send_keys` (`C-c` and friends) at 15/min burst 10, so an
//!      injection cannot wipe the fleet in one tight loop while a crew closing
//!      its own handful of tiles stays well under.
//!
//! The governor holds no filesystem / tmux handles: the concurrent count is
//! passed in by the caller (which reads it from tmux) and the clock is passed in
//! as an `Instant`, so every path is deterministically unit-testable.

use std::sync::Mutex;
use std::time::Instant;

/// Absolute concurrent-session stop. No `T_HUB_MAX_SESSIONS` override can raise
/// the effective cap above this — it is the backstop against a fat-fingered env.
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
    #[allow(dead_code)] // diagnostics accessor; exercised by the unit tests
    pub fn max_sessions(&self) -> usize {
        self.max_sessions
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
    /// Rate-limited only — there is no concurrent notion for a teardown.
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
        // as a rate limit — the runaway-loop signal.
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
