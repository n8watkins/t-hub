//! Toasts overlay (T9): transient notifications on session status transitions.
//!
//! Port of the webview's `lib/notify.ts` state logic, rendered as visual toast
//! cards instead of sounds/OS notifications (documented §5):
//!  - status -> toast mapping (needsQuestion/needsPermission/completed/failed/
//!    rateLimited; routine statuses are silent),
//!  - per-session dedup (the same status never fires twice in a row),
//!  - a warmup window that suppresses the journal-replay burst after (re)connect
//!    while still seeding dedup baselines,
//!  - tab-aware suppression: transitions for sessions the user is already
//!    looking at (the active-tab set T8 feeds in) do not toast,
//!  - a bounded queue with TTL expiry and click-to-dismiss.

use std::collections::{HashMap, HashSet, VecDeque};

use super::model::SessionStatus;

/// Webview `WARMUP_INITIAL_MS`: hard cap on the replay-suppression window.
pub const WARMUP_INITIAL_MS: u64 = 6_000;
/// Webview `WARMUP_GRACE_MS`: quiet time after the last burst event that ends
/// warmup early.
pub const WARMUP_GRACE_MS: u64 = 1_500;
/// How long a toast stays visible (the webview plays a ~0.2s chime + an OS
/// notification instead; a visual card needs time to read).
pub const TOAST_TTL_MS: u64 = 8_000;
/// Max simultaneously queued toasts; the oldest drops first.
pub const MAX_TOASTS: usize = 4;

/// Toast severity, driving the card accent color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Attention,
    Done,
    Error,
}

impl ToastKind {
    pub fn color(self) -> (u8, u8, u8) {
        match self {
            ToastKind::Attention => (251, 191, 36), // amber
            ToastKind::Done => (52, 211, 153),      // green
            ToastKind::Error => (248, 113, 113),    // red
        }
    }
}

/// One visible toast.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    /// Monotonic id for dismissal.
    pub seq: u64,
    pub session_id: String,
    pub kind: ToastKind,
    pub title: String,
    pub body: String,
    pub created_at_ms: u64,
}

/// Map a status to its toast content; `None` = routine, stay silent
/// (webview `notify.ts` mapping, same titles).
pub fn map_status(status: SessionStatus) -> Option<(ToastKind, &'static str, &'static str)> {
    match status {
        SessionStatus::NeedsQuestion => {
            Some((ToastKind::Attention, "Claude needs an answer", "A session is waiting on your input."))
        }
        SessionStatus::NeedsPermission => {
            Some((ToastKind::Attention, "Claude needs permission", "A session is asking to use a tool."))
        }
        SessionStatus::Completed => {
            Some((ToastKind::Done, "Session completed", "The agent finished its turn."))
        }
        SessionStatus::Failed => {
            Some((ToastKind::Error, "Session failed", "The session ended with an error."))
        }
        SessionStatus::RateLimited => {
            Some((ToastKind::Error, "Rate limited", "A session hit a rate limit."))
        }
        _ => None,
    }
}

/// The replay-burst suppression window (webview `lib/warmup.ts`). Armed at feed
/// start and re-armed when the agent bridge replays; every folded status event
/// inside the window extends the grace tail, up to the hard cap.
#[derive(Debug, Default)]
struct Warmup {
    started_at_ms: Option<u64>,
    last_event_ms: u64,
}

impl Warmup {
    fn arm(&mut self, now_ms: u64) {
        self.started_at_ms = Some(now_ms);
        self.last_event_ms = now_ms;
    }

    fn note_event(&mut self, now_ms: u64) {
        if self.is_active(now_ms) {
            self.last_event_ms = now_ms;
        }
    }

    fn is_active(&self, now_ms: u64) -> bool {
        match self.started_at_ms {
            Some(start) => {
                now_ms < start + WARMUP_INITIAL_MS && now_ms < self.last_event_ms + WARMUP_GRACE_MS
            }
            None => false,
        }
    }
}

/// Toast state machine. All inputs carry `now_ms` so every path is deterministic
/// under test.
#[derive(Debug, Default)]
pub struct ToastsState {
    queue: VecDeque<Toast>,
    next_seq: u64,
    /// Dedup baseline: the last status folded per session (recorded even when
    /// suppressed, so warmup seeds baselines exactly like the webview).
    last_status: HashMap<String, SessionStatus>,
    /// Tab-aware suppression: session keys the user is currently looking at.
    /// Accepts BOTH id spaces (Claude session UUIDs and `th_*` tmux names);
    /// the fold checks the UUID plus the alias the caller passes.
    active_sessions: HashSet<String>,
    warmup: Warmup,
}

impl ToastsState {
    /// Arm (or re-arm) the warmup window - at feed start and whenever the agent
    /// bridge starts replaying its journal.
    pub fn arm_warmup(&mut self, now_ms: u64) {
        self.warmup.arm(now_ms);
    }

    pub fn in_warmup(&self, now_ms: u64) -> bool {
        self.warmup.is_active(now_ms)
    }

    /// Replace the active-tab session set (T8 calls this on tab/focus changes).
    pub fn set_active_sessions(&mut self, sessions: HashSet<String>) {
        self.active_sessions = sessions;
    }

    /// Record a status as the dedup baseline WITHOUT ever toasting - for the
    /// feed's initial supervision pull, whose statuses are point-in-time state,
    /// not transitions (and may land after warmup expires on a slow connect).
    pub fn seed_status(&mut self, session_id: &str, status: SessionStatus) {
        self.last_status.insert(session_id.to_string(), status);
    }

    /// Fold one status transition. `alias` is the session's other-id-space name
    /// (the `th_*` tmux session for a UUID) when known, for suppression matching.
    /// Returns `true` when a toast was enqueued.
    pub fn fold_status(
        &mut self,
        session_id: &str,
        status: SessionStatus,
        alias: Option<&str>,
        now_ms: u64,
    ) -> bool {
        // Dedup on repeat emissions of the same status (tree + status events
        // both carry it; the server also re-emits on replay).
        let prev = self.last_status.insert(session_id.to_string(), status);
        if prev == Some(status) {
            return false;
        }
        // Warmup: replayed transitions seed the baseline silently.
        if self.warmup.is_active(now_ms) {
            self.warmup.note_event(now_ms);
            return false;
        }
        // Tab-aware suppression: the user is already looking at this session.
        if self.active_sessions.contains(session_id)
            || alias.is_some_and(|a| self.active_sessions.contains(a))
        {
            return false;
        }
        let Some((kind, title, body)) = map_status(status) else { return false };
        self.next_seq += 1;
        self.queue.push_back(Toast {
            seq: self.next_seq,
            session_id: session_id.to_string(),
            kind,
            title: title.to_string(),
            body: body.to_string(),
            created_at_ms: now_ms,
        });
        while self.queue.len() > MAX_TOASTS {
            self.queue.pop_front();
        }
        true
    }

    /// Drop expired toasts. Called on every render tick.
    pub fn tick(&mut self, now_ms: u64) {
        self.queue.retain(|t| now_ms < t.created_at_ms + TOAST_TTL_MS);
    }

    /// Click-to-dismiss.
    pub fn dismiss(&mut self, seq: u64) {
        self.queue.retain(|t| t.seq != seq);
    }

    /// The toasts to render, oldest first.
    pub fn visible(&self) -> Vec<Toast> {
        self.queue.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state whose warmup has already passed (the common test fixture).
    fn warm() -> (ToastsState, u64) {
        let mut st = ToastsState::default();
        st.arm_warmup(0);
        (st, WARMUP_INITIAL_MS + 1)
    }

    #[test]
    fn map_status_covers_the_notifying_statuses_only() {
        assert!(map_status(SessionStatus::NeedsQuestion).is_some());
        assert!(map_status(SessionStatus::NeedsPermission).is_some());
        assert!(map_status(SessionStatus::Completed).is_some());
        assert!(map_status(SessionStatus::Failed).is_some());
        assert!(map_status(SessionStatus::RateLimited).is_some());
        for s in [
            SessionStatus::Working,
            SessionStatus::WaitingOnSubagents,
            SessionStatus::Detached,
            SessionStatus::Restoring,
            SessionStatus::Expired,
            SessionStatus::Unknown,
        ] {
            assert!(map_status(s).is_none(), "{s:?} must be silent");
        }
    }

    #[test]
    fn a_real_transition_fires_a_toast() {
        let (mut st, now) = warm();
        assert!(st.fold_status("s1", SessionStatus::NeedsQuestion, None, now));
        let v = st.visible();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "Claude needs an answer");
        assert_eq!(v[0].kind, ToastKind::Attention);
    }

    #[test]
    fn the_same_status_never_fires_twice_in_a_row() {
        let (mut st, now) = warm();
        assert!(st.fold_status("s1", SessionStatus::Completed, None, now));
        assert!(!st.fold_status("s1", SessionStatus::Completed, None, now + 100));
        // A different status, then back: fires again.
        assert!(!st.fold_status("s1", SessionStatus::Working, None, now + 200)); // silent status
        assert!(st.fold_status("s1", SessionStatus::Completed, None, now + 300));
    }

    #[test]
    fn warmup_suppresses_but_seeds_the_dedup_baseline() {
        let mut st = ToastsState::default();
        st.arm_warmup(1000);
        // Replayed transition inside warmup: silent.
        assert!(!st.fold_status("s1", SessionStatus::NeedsQuestion, None, 1100));
        // After warmup, the SAME status is deduped (baseline was seeded)...
        let after = 1000 + WARMUP_INITIAL_MS + 1;
        assert!(!st.fold_status("s1", SessionStatus::NeedsQuestion, None, after));
        // ...but a new transition fires.
        assert!(st.fold_status("s1", SessionStatus::Completed, None, after + 10));
    }

    #[test]
    fn warmup_grace_ends_early_and_events_extend_it() {
        let mut st = ToastsState::default();
        st.arm_warmup(0);
        assert!(st.in_warmup(0));
        // Quiet for the grace window: warmup over even before the hard cap.
        assert!(!st.in_warmup(WARMUP_GRACE_MS));
        // Events keep extending the grace tail...
        let mut st = ToastsState::default();
        st.arm_warmup(0);
        st.fold_status("s1", SessionStatus::Working, None, 1000);
        assert!(st.in_warmup(1000 + WARMUP_GRACE_MS - 1));
        // ...but never past the hard cap.
        st.fold_status("s2", SessionStatus::Working, None, WARMUP_INITIAL_MS - 1);
        assert!(!st.in_warmup(WARMUP_INITIAL_MS));
    }

    #[test]
    fn warmup_can_rearm_for_an_agent_replay() {
        let (mut st, now) = warm();
        assert!(!st.in_warmup(now));
        st.arm_warmup(now + 500);
        assert!(st.in_warmup(now + 600));
    }

    #[test]
    fn active_tab_suppression_matches_uuid_and_alias() {
        let (mut st, now) = warm();
        st.set_active_sessions(["uuid-visible".to_string(), "th_deadbeef".to_string()].into());
        // Suppressed by UUID.
        assert!(!st.fold_status("uuid-visible", SessionStatus::Completed, None, now));
        // Suppressed by the tmux alias.
        assert!(!st.fold_status("uuid-other", SessionStatus::Completed, Some("th_deadbeef"), now));
        // A session not on the active tab still fires.
        assert!(st.fold_status("uuid-bg", SessionStatus::Completed, Some("th_cafe0001"), now));
    }

    #[test]
    fn seeding_records_the_baseline_without_toasting_even_after_warmup() {
        let (mut st, now) = warm();
        st.seed_status("s1", SessionStatus::NeedsQuestion);
        assert!(st.visible().is_empty());
        // The seeded status is the baseline: re-folding it is a dedup no-op...
        assert!(!st.fold_status("s1", SessionStatus::NeedsQuestion, None, now));
        // ...while a real transition still fires.
        assert!(st.fold_status("s1", SessionStatus::Completed, None, now + 10));
    }

    #[test]
    fn queue_caps_expires_and_dismisses() {
        let (mut st, now) = warm();
        for i in 0..(MAX_TOASTS + 2) {
            let s = format!("s{i}");
            assert!(st.fold_status(&s, SessionStatus::Failed, None, now + i as u64));
        }
        assert_eq!(st.visible().len(), MAX_TOASTS); // oldest dropped
        let first_seq = st.visible()[0].seq;
        st.dismiss(first_seq);
        assert_eq!(st.visible().len(), MAX_TOASTS - 1);
        // TTL expiry relative to each toast's creation.
        st.tick(now + 1 + TOAST_TTL_MS + MAX_TOASTS as u64);
        assert!(st.visible().is_empty());
    }
}
