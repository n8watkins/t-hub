//! The gpui-free satellite-window registry (T10).
//!
//! One entry per torn-off workspace, keyed by the workspace's stable runtime
//! id ([`crate::chrome::model::Workspace::wsid`]). The registry owns the
//! per-window state that is NOT the workspace itself: which tile the window
//! has focused (each OS window routes its own keyboard input) and the window's
//! last known bounds (refreshed every paint, persisted with the layout so a
//! restart reopens satellites where they were).
//!
//! What does NOT live here: gpui `WindowHandle`s (main-thread-only, they stay
//! in the gui layer) and the tiles themselves (the [`ChromeModel`] keeps owning
//! every workspace, torn off or not - a satellite is a *view* of a workspace,
//! not a second home for it).
//!
//! [`ChromeModel`]: crate::chrome::model::ChromeModel

use std::collections::BTreeMap;

/// Window bounds in logical pixels, as reported by the windowing system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SatBounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Runtime state of one open satellite window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SatWindow {
    /// The tile this window's keyboard input goes to. Validated on read against
    /// the workspace's live tiles (sessions die out from under it).
    pub focused: Option<String>,
    /// Last known window bounds (refreshed by the window's paint loop; the
    /// layout save that happens to run next persists them).
    pub bounds: Option<SatBounds>,
}

/// All open satellite windows, keyed by wsid. `BTreeMap` for deterministic
/// iteration (boot restore, tests).
#[derive(Debug, Default)]
pub struct WindowRegistry {
    sats: BTreeMap<u64, SatWindow>,
    /// Bounds remembered from satellites closed earlier this run, so re-tearing
    /// the same workspace reopens its window where the user left it. Runtime
    /// only - while torn off, bounds persist via the layout file instead.
    memo: BTreeMap<u64, SatBounds>,
}

impl WindowRegistry {
    /// Register a newly opened satellite. `focused` seeds the window's keyboard
    /// target (the tile the user had focused when tearing off, or the first
    /// tile). Explicit `bounds` (a persisted layout) win over the re-tear memo.
    pub fn open(&mut self, wsid: u64, focused: Option<String>, bounds: Option<SatBounds>) {
        let bounds = bounds.or_else(|| self.memo.get(&wsid).copied());
        self.sats.insert(wsid, SatWindow { focused, bounds });
    }

    /// Unregister a closing satellite, remembering its bounds for a re-tear.
    /// Returns the entry, or `None` when the wsid was not open.
    pub fn close(&mut self, wsid: u64) -> Option<SatWindow> {
        let sat = self.sats.remove(&wsid)?;
        if let Some(b) = sat.bounds {
            self.memo.insert(wsid, b);
        }
        Some(sat)
    }

    pub fn contains(&self, wsid: u64) -> bool {
        self.sats.contains_key(&wsid)
    }

    pub fn len(&self) -> usize {
        self.sats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sats.is_empty()
    }

    /// Open satellite wsids, ascending.
    pub fn wsids(&self) -> Vec<u64> {
        self.sats.keys().copied().collect()
    }

    pub fn focused_of(&self, wsid: u64) -> Option<&str> {
        self.sats.get(&wsid)?.focused.as_deref()
    }

    pub fn set_focused(&mut self, wsid: u64, id: Option<String>) {
        if let Some(sat) = self.sats.get_mut(&wsid) {
            sat.focused = id;
        }
    }

    pub fn bounds_of(&self, wsid: u64) -> Option<SatBounds> {
        self.sats.get(&wsid)?.bounds
    }

    pub fn set_bounds(&mut self, wsid: u64, bounds: SatBounds) {
        if let Some(sat) = self.sats.get_mut(&wsid) {
            sat.bounds = Some(bounds);
        }
    }

    /// Drop every reference to a tile that left the layout (session died or the
    /// user closed it), so a stale focus never routes keys to a dropped tile.
    pub fn drop_tile(&mut self, id: &str) {
        for sat in self.sats.values_mut() {
            if sat.focused.as_deref() == Some(id) {
                sat.focused = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: f32) -> SatBounds {
        SatBounds { x, y: 10.0, w: 800.0, h: 600.0 }
    }

    #[test]
    fn open_close_roundtrip_with_bounds_memo() {
        let mut r = WindowRegistry::default();
        assert!(r.is_empty());
        r.open(7, Some("aa".into()), None);
        assert!(r.contains(7));
        assert_eq!(r.len(), 1);
        assert_eq!(r.focused_of(7), Some("aa"));
        assert_eq!(r.bounds_of(7), None);

        // The paint loop refreshes bounds; close remembers them.
        r.set_bounds(7, b(100.0));
        let closed = r.close(7).unwrap();
        assert_eq!(closed.bounds, Some(b(100.0)));
        assert!(!r.contains(7));

        // Re-tearing the same workspace reopens where the user left it.
        r.open(7, None, None);
        assert_eq!(r.bounds_of(7), Some(b(100.0)));

        // ...but explicit (persisted) bounds win over the memo.
        r.close(7);
        r.open(7, None, Some(b(500.0)));
        assert_eq!(r.bounds_of(7), Some(b(500.0)));

        // Closing an unknown wsid is a no-op.
        assert_eq!(r.close(99), None);
    }

    #[test]
    fn drop_tile_clears_stale_focus_everywhere() {
        let mut r = WindowRegistry::default();
        r.open(1, Some("aa".into()), None);
        r.open(2, Some("aa".into()), None);
        r.open(3, Some("bb".into()), None);
        r.drop_tile("aa");
        assert_eq!(r.focused_of(1), None);
        assert_eq!(r.focused_of(2), None);
        assert_eq!(r.focused_of(3), Some("bb"));
        // set_focused on an unknown wsid is a no-op.
        r.set_focused(99, Some("cc".into()));
        assert_eq!(r.focused_of(99), None);
    }

    #[test]
    fn wsids_iterate_deterministically() {
        let mut r = WindowRegistry::default();
        r.open(9, None, None);
        r.open(3, None, None);
        r.open(5, None, None);
        assert_eq!(r.wsids(), vec![3, 5, 9]);
    }
}
