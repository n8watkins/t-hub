//! Supervision cues (T24), gpui-free: per-tile output-age formatting, the
//! output/exit pulse the feeder threads stamp, and the tile life state machine
//! that tells the header WHY a tile looks frozen.
//!
//! The problem this solves: a cockpit tile can read as dead in three very
//! different situations - the tmux session was killed (session-gone), the pane
//! process exited, or only the VIEWER's attach dropped while the session keeps
//! working server-side. The pre-T24 chrome painted all three exactly like a
//! quiet-but-healthy pane (the wire even auto-reattaches invisibly), and the
//! general was repeatedly bitten by tiles that looked dead while work continued
//! underneath. [`TileLife`] makes the distinction explicit so the header can
//! say `DEAD` / `exited N` / `reconnecting` instead of nothing.
//!
//! Everything here is plain data + pure transitions, unit-tested under
//! `--no-default-features`; the observation inputs come from the cockpit
//! worker (`list_terminals` presence, the wire's link flag, the feeder's exit
//! stamp) and the paint loop only reads.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Output age
// ---------------------------------------------------------------------------

/// Compact age: seconds under a minute, then minutes, hours, days ("3s", "2m",
/// "1h", "4d"). Coarse on purpose - the header cue answers "is this thing
/// still talking?", not "when exactly".
pub fn format_age(elapsed_ms: u64) -> String {
    let s = elapsed_ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// The header's last-output label: "3s ago" style, empty when there is no
/// stamp yet (a tile that never attached has no output history to age).
pub fn age_label(last_output_ms: u64, now_ms: u64) -> String {
    if last_output_ms == 0 {
        return String::new();
    }
    format!("{} ago", format_age(now_ms.saturating_sub(last_output_ms)))
}

// ---------------------------------------------------------------------------
// The per-tile pulse (written by feeder threads, read by worker + paint)
// ---------------------------------------------------------------------------

/// `exit` sentinel: no exit frame seen.
const NO_EXIT: i64 = i64::MIN;

/// What a tile's feeder thread reports without locks: the last-output stamp
/// (ms since the cockpit epoch; the attach seed counts - it is the content
/// currently on screen arriving) and the PTY exit code once an exit frame
/// lands (sticky).
#[derive(Debug)]
pub struct TilePulse {
    out_ms: AtomicU64,
    exit: AtomicI64,
}

impl TilePulse {
    pub fn new(now_ms: u64) -> Self {
        TilePulse { out_ms: AtomicU64::new(now_ms.max(1)), exit: AtomicI64::new(NO_EXIT) }
    }

    pub fn stamp_output(&self, now_ms: u64) {
        self.out_ms.store(now_ms.max(1), Ordering::Relaxed);
    }

    pub fn last_output_ms(&self) -> u64 {
        self.out_ms.load(Ordering::Relaxed)
    }

    pub fn record_exit(&self, code: i32) {
        self.exit.store(code as i64, Ordering::Relaxed);
    }

    pub fn exit_code(&self) -> Option<i32> {
        match self.exit.load(Ordering::Relaxed) {
            NO_EXIT => None,
            c => Some(c as i32),
        }
    }
}

// ---------------------------------------------------------------------------
// The tile life state machine
// ---------------------------------------------------------------------------

/// Why a tile looks the way it does. `since_ms` stamps the transition so the
/// header can show "reconnecting 5s" and the linger window can expire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TileLife {
    /// Attached and streaming (or quietly idle) - the normal state.
    Live,
    /// The VIEWER's attach dropped but the session was still listed alive
    /// server-side: the wire retries with backoff; work may well continue
    /// underneath. This is the "tile reads as dead while work continued" case.
    Reconnecting { since_ms: u64 },
    /// The pane process exited (a real PTY exit frame, with its code).
    Exited { code: i32, since_ms: u64 },
    /// The session is GONE from `list_terminals` (tmux killed / server lost
    /// it) without a clean exit frame.
    Dead { since_ms: u64 },
}

impl TileLife {
    /// A dead-ish tile: keep it painted with its badge for the linger window,
    /// then let reconcile remove it.
    pub fn is_defunct(&self) -> bool {
        matches!(self, TileLife::Exited { .. } | TileLife::Dead { .. })
    }

    fn since(&self) -> Option<u64> {
        match self {
            TileLife::Live => None,
            TileLife::Reconnecting { since_ms }
            | TileLife::Exited { since_ms, .. }
            | TileLife::Dead { since_ms } => Some(*since_ms),
        }
    }
}

/// One observation of a placed tile, gathered by the cockpit worker each pass.
#[derive(Clone, Copy, Debug)]
pub struct LifeObs {
    /// Present in the server's `list_terminals` answer this pass.
    pub listed: bool,
    /// The wire reports the attach connection down (reader mid-backoff).
    pub link_down: bool,
    /// The feeder saw a PTY exit frame (sticky, with the code).
    pub exit: Option<i32>,
    pub now_ms: u64,
}

/// What one observation did to a tile's life.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observed {
    pub life: TileLife,
    /// A defunct tile came back listed (tmux session re-created under the same
    /// name, or the server recovered): the caller should drop the stale pool
    /// entry so the reconcile attach path re-attaches it fresh.
    pub revived: bool,
}

/// Tracks every placed tile's [`TileLife`]. Owned by the shared cockpit state;
/// the worker observes, the paint loop reads.
#[derive(Debug, Default)]
pub struct LifeTracker {
    map: HashMap<String, TileLife>,
    /// Tiles seen LISTED at least once this run. Only these linger when they
    /// die: a layout tile whose session died while the app was closed is
    /// dead-on-arrival at boot - there is nothing on screen worth badging, it
    /// prunes straight away like pre-T24.
    seen: HashSet<String>,
}

impl LifeTracker {
    /// Fold one observation into tile `id`'s life. Transition rules:
    /// - an exit frame wins while it explains the facts: `Exited` (sticky
    ///   through delisting - "exited 1" is more informative than "dead");
    /// - delisted without an exit frame -> `Dead`;
    /// - a defunct tile that shows up listed again (no exit) revives to `Live`
    ///   (the session was re-created; the caller re-attaches);
    /// - listed + link down -> `Reconnecting` (the session lives server-side,
    ///   only the viewer's attach dropped);
    /// - otherwise `Live`.
    ///   `since_ms` is preserved across repeated observations of one episode.
    pub fn observe(&mut self, id: &str, obs: LifeObs) -> Observed {
        if obs.listed {
            self.seen.insert(id.to_string());
        }
        let prev = self.get(id);
        let life = match (obs.listed, obs.exit) {
            (_, Some(code)) => TileLife::Exited {
                code,
                since_ms: match prev {
                    TileLife::Exited { since_ms, .. } => since_ms,
                    _ => obs.now_ms,
                },
            },
            (false, None) => TileLife::Dead {
                since_ms: match prev {
                    TileLife::Dead { since_ms } => since_ms,
                    _ => obs.now_ms,
                },
            },
            (true, None) if obs.link_down => TileLife::Reconnecting {
                since_ms: match prev {
                    TileLife::Reconnecting { since_ms } => since_ms,
                    _ => obs.now_ms,
                },
            },
            (true, None) => TileLife::Live,
        };
        let revived = prev.is_defunct() && life == TileLife::Live;
        self.map.insert(id.to_string(), life);
        Observed { life, revived }
    }

    /// Fold a LINK-ONLY observation: the server did not answer
    /// `list_terminals` (unreachable/wedged), so there is no death verdict -
    /// but the wire still knows whether each attach is down. Defunct states
    /// hold (they were earned from a real session list); live tiles flip
    /// between `Live` and `Reconnecting` on the link alone. Without this, an
    /// unreachable server would show a wall of frozen, normal-looking tiles -
    /// the exact failure T24 exists to explain.
    pub fn observe_link(&mut self, id: &str, link_down: bool, now_ms: u64) -> TileLife {
        let prev = self.get(id);
        let life = match prev {
            TileLife::Exited { .. } | TileLife::Dead { .. } => prev,
            _ if link_down => TileLife::Reconnecting {
                since_ms: match prev {
                    TileLife::Reconnecting { since_ms } => since_ms,
                    _ => now_ms,
                },
            },
            _ => TileLife::Live,
        };
        self.map.insert(id.to_string(), life);
        life
    }

    /// The tile's current life; an unobserved tile is `Live` (it just arrived).
    pub fn get(&self, id: &str) -> TileLife {
        self.map.get(id).copied().unwrap_or(TileLife::Live)
    }

    pub fn remove(&mut self, id: &str) {
        self.map.remove(id);
        self.seen.remove(id);
    }

    /// The defunct tiles still inside their linger window: reconcile keeps
    /// these placed (badge visible) even though the server no longer lists
    /// them; once the window passes they fall out and reconcile prunes them.
    /// Dead-on-arrival tiles (never seen listed this run) never linger.
    pub fn lingering(&self, now_ms: u64, linger_ms: u64) -> HashSet<String> {
        self.map
            .iter()
            .filter(|(id, life)| {
                life.is_defunct()
                    && self.seen.contains(*id)
                    && life.since().is_some_and(|s| now_ms.saturating_sub(s) < linger_ms)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- age formatting -------------------------------------------------------

    #[test]
    fn format_age_is_compact_and_coarse() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(900), "0s");
        assert_eq!(format_age(3_000), "3s");
        assert_eq!(format_age(59_999), "59s");
        assert_eq!(format_age(60_000), "1m");
        assert_eq!(format_age(59 * 60_000), "59m");
        assert_eq!(format_age(3_600_000), "1h");
        assert_eq!(format_age(23 * 3_600_000), "23h");
        assert_eq!(format_age(86_400_000 * 4), "4d");
    }

    #[test]
    fn age_label_reads_like_the_header_and_hides_when_unstamped() {
        assert_eq!(age_label(0, 10_000), ""); // never stamped
        assert_eq!(age_label(7_000, 10_000), "3s ago");
        assert_eq!(age_label(10_000, 10_000), "0s ago");
        // A stamp "from the future" (clock skew across threads) never underflows.
        assert_eq!(age_label(11_000, 10_000), "0s ago");
    }

    // -- pulse -----------------------------------------------------------------

    #[test]
    fn pulse_stamps_and_holds_the_exit_code() {
        let p = TilePulse::new(5_000);
        assert_eq!(p.last_output_ms(), 5_000);
        assert_eq!(p.exit_code(), None);
        p.stamp_output(9_000);
        assert_eq!(p.last_output_ms(), 9_000);
        p.record_exit(1);
        assert_eq!(p.exit_code(), Some(1));
        // A zero now_ms still yields a non-zero stamp (0 means "never").
        let p0 = TilePulse::new(0);
        assert_ne!(p0.last_output_ms(), 0);
    }

    // -- life state machine ----------------------------------------------------

    fn obs(listed: bool, link_down: bool, exit: Option<i32>, now_ms: u64) -> LifeObs {
        LifeObs { listed, link_down, exit, now_ms }
    }

    #[test]
    fn healthy_tile_stays_live() {
        let mut t = LifeTracker::default();
        assert_eq!(t.get("a"), TileLife::Live); // unobserved default
        let o = t.observe("a", obs(true, false, None, 100));
        assert_eq!(o, Observed { life: TileLife::Live, revived: false });
    }

    #[test]
    fn attach_loss_reconnects_then_recovers() {
        let mut t = LifeTracker::default();
        t.observe("a", obs(true, false, None, 100));
        // Link drops while the session stays listed: reconnecting, since=200.
        let o = t.observe("a", obs(true, true, None, 200));
        assert_eq!(o.life, TileLife::Reconnecting { since_ms: 200 });
        // Still down later: the episode keeps its original stamp.
        let o = t.observe("a", obs(true, true, None, 900));
        assert_eq!(o.life, TileLife::Reconnecting { since_ms: 200 });
        // The reattach lands: live again, and NOT a revive (pool entry is fine).
        let o = t.observe("a", obs(true, false, None, 1_000));
        assert_eq!(o, Observed { life: TileLife::Live, revived: false });
    }

    #[test]
    fn delisting_without_an_exit_frame_is_dead() {
        let mut t = LifeTracker::default();
        t.observe("a", obs(true, false, None, 100));
        let o = t.observe("a", obs(false, true, None, 300));
        assert_eq!(o.life, TileLife::Dead { since_ms: 300 });
        assert!(o.life.is_defunct());
        // Dead keeps its stamp across passes.
        let o = t.observe("a", obs(false, true, None, 800));
        assert_eq!(o.life, TileLife::Dead { since_ms: 300 });
    }

    #[test]
    fn an_exit_frame_wins_and_sticks_through_delisting() {
        let mut t = LifeTracker::default();
        t.observe("a", obs(true, false, None, 100));
        // remain-on-exit: exited but still listed.
        let o = t.observe("a", obs(true, false, Some(0), 200));
        assert_eq!(o.life, TileLife::Exited { code: 0, since_ms: 200 });
        // tmux reaps the session: the exit code stays (more informative than DEAD).
        let o = t.observe("a", obs(false, false, Some(0), 500));
        assert_eq!(o.life, TileLife::Exited { code: 0, since_ms: 200 });
    }

    #[test]
    fn a_relisted_defunct_tile_revives_for_a_fresh_attach() {
        let mut t = LifeTracker::default();
        t.observe("a", obs(false, false, None, 100)); // dead
        let o = t.observe("a", obs(true, false, None, 400)); // same name, new session
        assert_eq!(o, Observed { life: TileLife::Live, revived: true });
    }

    #[test]
    fn lingering_holds_defunct_tiles_for_the_window_then_releases() {
        let mut t = LifeTracker::default();
        // All four were alive first (linger is for tiles that died THIS run).
        for id in ["dead", "exited", "live", "reconn"] {
            t.observe(id, obs(true, false, None, 500));
        }
        t.observe("dead", obs(false, false, None, 1_000));
        t.observe("exited", obs(false, false, Some(1), 2_000));
        t.observe("reconn", obs(true, true, None, 2_000));

        // Inside the window: both defunct tiles linger; live states never do.
        let l = t.lingering(3_000, 10_000);
        assert!(l.contains("dead") && l.contains("exited"));
        assert!(!l.contains("live") && !l.contains("reconn"));

        // Past the window for "dead" only (since 1s vs 2s, window 10s).
        let l = t.lingering(11_500, 10_000);
        assert!(!l.contains("dead"));
        assert!(l.contains("exited"));

        // remove() forgets a tile entirely.
        t.remove("exited");
        assert!(!t.lingering(3_000, 10_000).contains("exited"));
        assert_eq!(t.get("exited"), TileLife::Live);
    }

    #[test]
    fn link_only_observations_flag_reconnecting_without_a_session_list() {
        // The server stops answering list_terminals entirely (unreachable or
        // the attach-churn wedge): links go down, no death verdicts exist.
        let mut t = LifeTracker::default();
        t.observe("a", obs(true, false, None, 100));
        t.observe("gone", obs(true, false, Some(1), 150)); // already exited
        let l = t.observe_link("a", true, 200);
        assert_eq!(l, TileLife::Reconnecting { since_ms: 200 });
        // The episode keeps its stamp; defunct states hold their verdicts.
        assert_eq!(t.observe_link("a", true, 900), TileLife::Reconnecting { since_ms: 200 });
        assert_eq!(t.observe_link("gone", true, 900), TileLife::Exited { code: 1, since_ms: 150 });
        // A reattach landing (link back up) is proof of life on its own.
        assert_eq!(t.observe_link("a", false, 1_000), TileLife::Live);
        // And link-only observations never produce Dead (no list, no verdict).
    }

    #[test]
    fn dead_on_arrival_tiles_never_linger() {
        // A layout tile whose session died while the app was closed: its first
        // observation is already delisted. It shows Dead but must NOT linger -
        // there is no on-screen content worth badging; boot prunes it.
        let mut t = LifeTracker::default();
        let o = t.observe("stale", obs(false, false, None, 500));
        assert_eq!(o.life, TileLife::Dead { since_ms: 500 });
        assert!(t.lingering(600, 45_000).is_empty());
    }
}
