//! The gpui-free chrome state machine (T8): tabs, tiles, focus, rename mode,
//! layout math, and live-session reconciliation.
//!
//! Everything here is plain data + pure functions so it unit-tests in WSL under
//! `--no-default-features` (the same reason `term/` and `render_support` are
//! gpui-free). The GPUI view ([`crate::chrome::view`]) is a thin painter/input
//! adapter over this model.
//!
//! ## Semantics source
//! The webview cockpit is the spec: `apps/desktop/src/store/workspace.ts` (tab
//! model: "Workspace N" naming, never close the last tab, tiles of a closed tab
//! stay hidden while their sessions survive) and `apps/desktop/src/components/
//! Canvas.tsx` `splitRows()` (the auto-grid: near-square row buckets, earlier
//! rows take the extras, each row's tiles span the full width - no holes, unlike
//! the T5 uniform grid).

use std::collections::HashSet;

use crate::font::FontSpec;

// ---------------------------------------------------------------------------
// Plain geometry (gpui-free)
// ---------------------------------------------------------------------------

/// A plain float rect so layout math and hit-testing stay gpui-free/testable.
/// The view converts to `gpui::Bounds` at the paint boundary.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        RectF { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

// ---------------------------------------------------------------------------
// Auto-grid layout math (webview Canvas.tsx splitRows semantics)
// ---------------------------------------------------------------------------

/// Row buckets for `n` tiles, exactly as the webview's `splitRows()`:
/// `cols = ceil(sqrt(n))`, `rows = ceil(n/cols)`, `base = floor(n/rows)`,
/// `extra = n % rows`; the first `extra` rows get one extra tile.
/// 5 -> [3,2], 7 -> [3,2,2], 12 -> [4,4,4]. Every row is fully packed (its tiles
/// stretch across the whole width), so there are no holes.
pub fn split_rows(n: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let base = n / rows;
    let extra = n % rows;
    (0..rows).map(|r| base + usize::from(r < extra)).collect()
}

/// Pixel boxes for `n` tiles inside `area`, in tile order (row-major over the
/// [`split_rows`] buckets): rows share the height evenly; within a row, tiles
/// share the width evenly (the webview's even flex split - manual ratios and
/// drag-reorder are a T8 follow-up).
pub fn tile_boxes(n: usize, area: RectF, gap: f32) -> Vec<RectF> {
    let buckets = split_rows(n);
    let rows = buckets.len();
    if rows == 0 {
        return Vec::new();
    }
    let row_h = ((area.h - gap * (rows as f32 - 1.0)) / rows as f32).max(1.0);
    let mut out = Vec::with_capacity(n);
    for (r, &count) in buckets.iter().enumerate() {
        let tile_w = ((area.w - gap * (count as f32 - 1.0)) / count as f32).max(1.0);
        let y = area.y + r as f32 * (row_h + gap);
        for c in 0..count {
            let x = area.x + c as f32 * (tile_w + gap);
            out.push(RectF::new(x, y, tile_w, row_h));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Sidebar layout math
// ---------------------------------------------------------------------------

/// Workspace navigation lives in the LEFT SIDEBAR (the webview's long-standing
/// design - workspaces are listed in the sidebar, not a top tab strip): a
/// vertical stack of workspace rows, a "+ new workspace" row after them, and
/// everything below reserved for the T9 sidebar overlay sections (recents,
/// usage, metrics, supervision) to mount into.
pub const SIDEBAR_ROW_H: f32 = 28.0;
const SIDEBAR_ROW_GAP: f32 = 2.0;

/// Hit zones for the sidebar's workspace section, in workspace order.
#[derive(Clone, Debug, Default)]
pub struct SidebarLayout {
    pub rows: Vec<RectF>,
    pub closes: Vec<RectF>,
    pub plus: RectF,
    /// The rest of the sidebar below the workspace section. T9's overlay
    /// sections mount here; the T8 chrome paints NOTHING inside it.
    pub overlay_mount: RectF,
}

/// Lay out the sidebar's workspace section inside `area` (the sidebar's inner
/// content box, below the section caption): `n` full-width rows, each with a
/// square close zone flush right, then the `+` row, then the T9 mount.
pub fn sidebar_layout(n: usize, area: RectF) -> SidebarLayout {
    let mut rows = Vec::with_capacity(n);
    let mut closes = Vec::with_capacity(n);
    let mut y = area.y;
    for _ in 0..n {
        rows.push(RectF::new(area.x, y, area.w, SIDEBAR_ROW_H));
        closes.push(RectF::new(
            area.x + area.w - SIDEBAR_ROW_H,
            y,
            SIDEBAR_ROW_H,
            SIDEBAR_ROW_H,
        ));
        y += SIDEBAR_ROW_H + SIDEBAR_ROW_GAP;
    }
    let plus = RectF::new(area.x, y, area.w, SIDEBAR_ROW_H);
    y += SIDEBAR_ROW_H + SIDEBAR_ROW_GAP;
    let overlay_mount =
        RectF::new(area.x, y, area.w, (area.h - (y - area.y)).max(0.0));
    SidebarLayout { rows, closes, plus, overlay_mount }
}

// ---------------------------------------------------------------------------
// The chrome model
// ---------------------------------------------------------------------------

/// One workspace tab: a stable id (T12 - the MCP addresses tabs by id, exactly
/// like the webview's store ids), a name, an ordered set of tile (session) ids,
/// and an optional font override (T7 [`FontSpec`]) applied to tiles attached
/// into this workspace. `None` falls back to the `THN_FONT` / built-in default.
/// There is no settings UI yet - the field persists and applies (edit the
/// layout JSON).
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub tiles: Vec<String>,
    pub font: Option<FontSpec>,
}

impl Workspace {
    /// A fresh workspace with a minted id and no tiles.
    pub fn new(name: impl Into<String>) -> Self {
        Workspace { id: mint_tab_id(), name: name.into(), tiles: Vec::new(), font: None }
    }
}

/// Mint a workspace-tab id. Uuid v4 strings, the same shape the webview store
/// and the core's `new_tab` mint, so ids from any source are interchangeable in
/// the server's tab registry.
pub fn mint_tab_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Tab-rename editing state: which tab, and the in-progress buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct Rename {
    pub tab: usize,
    pub buffer: String,
}

/// What a reconcile pass changed: `added` need a PTY attach; `removed` (sessions
/// gone from the server) need their pool entries dropped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reconcile {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// The chrome state machine: tabs, the active tab, the focused tile, rename
/// mode, and the set of tiles the user closed this run (their sessions survive
/// server-side; they stay out of the layout until they die or the client
/// restarts - matching the webview's mid-session close-tab behavior).
#[derive(Clone, Debug)]
pub struct ChromeModel {
    pub tabs: Vec<Workspace>,
    pub active: usize,
    pub focused: Option<String>,
    pub renaming: Option<Rename>,
    /// Tiles closed by the user this run. NOT persisted: a client restart
    /// re-lists everything live (self-healing, and the only "reopen" story until
    /// the T9 recents overlay).
    hidden: HashSet<String>,
    /// Named-placement intents from worktree applies (T12): (normalized worktree
    /// path, target tab id). The next reconciled session whose cwd lives under
    /// the path lands in that tab instead of the active one, consuming the
    /// entry. NOT persisted - an intent only makes sense within this run.
    pending_placements: Vec<(String, String)>,
}

impl Default for ChromeModel {
    fn default() -> Self {
        ChromeModel {
            tabs: vec![Workspace::new("Workspace 1")],
            active: 0,
            focused: None,
            renaming: None,
            hidden: HashSet::new(),
            pending_placements: Vec::new(),
        }
    }
}

impl ChromeModel {
    /// Rebuild from persisted layout, sanitized: at least one tab, `active` in
    /// range. Tile liveness is reconciled separately against `list_terminals`.
    pub fn from_layout(tabs: Vec<Workspace>, active: usize) -> Self {
        let mut m = ChromeModel::default();
        if !tabs.is_empty() {
            m.tabs = tabs;
            m.active = active.min(m.tabs.len() - 1);
        }
        m
    }

    // -- tabs ---------------------------------------------------------------

    /// Create a new empty workspace named "Workspace N" and activate it. N is the
    /// LOWEST FREE index (webview `addTab` parity: "Workspace 1" + "Workspace 3"
    /// present -> the new tab is "Workspace 2"; the same scheme the core's
    /// `new_tab` auto-name uses, so tabs from any source share one naming).
    pub fn add_tab(&mut self) -> usize {
        self.renaming = None;
        let used: HashSet<u32> = self
            .tabs
            .iter()
            .filter_map(|t| t.name.strip_prefix("Workspace ").and_then(|n| n.trim().parse().ok()))
            .collect();
        let mut n = 1u32;
        while used.contains(&n) {
            n += 1;
        }
        self.tabs.push(Workspace::new(format!("Workspace {n}")));
        self.active = self.tabs.len() - 1;
        self.fixup_focus();
        self.active
    }

    /// The index of the tab with this id, if any (T12 id-addressed mutations).
    pub fn tab_index_of_id(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Adopt a tab by id (T12 `new_tab` apply, webview `adoptTab` parity): if a
    /// tab with this id exists, just activate it (no rename); otherwise create it
    /// with this id + name (blank name -> "Workspace") and activate it. Returns
    /// whether a tab was created.
    pub fn adopt_tab(&mut self, id: &str, name: &str) -> bool {
        self.renaming = None;
        if let Some(i) = self.tab_index_of_id(id) {
            self.active = i;
            self.fixup_focus();
            return false;
        }
        let name = name.trim();
        self.tabs.push(Workspace {
            id: id.to_string(),
            name: if name.is_empty() { "Workspace".to_string() } else { name.to_string() },
            tiles: Vec::new(),
            font: None,
        });
        self.active = self.tabs.len() - 1;
        self.fixup_focus();
        true
    }

    /// Rename a tab by id (T12 `rename_tab` apply; webview parity: trims, a
    /// blank name and an unknown id are no-ops). Returns whether a rename landed.
    pub fn rename_tab_by_id(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        match self.tabs.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                t.name = name.to_string();
                true
            }
            None => false,
        }
    }

    /// Activate a tab by id (T12 `focus_tab` apply). Unknown id is a no-op.
    pub fn set_active_by_id(&mut self, id: &str) -> bool {
        match self.tab_index_of_id(id) {
            Some(i) => {
                self.set_active(i);
                true
            }
            None => false,
        }
    }

    /// The index of the tab holding tile `id`, if any.
    pub fn owning_tab_of(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.tiles.iter().any(|x| x == id))
    }

    /// Close tab `i`. Refuses on the last tab (webview: at least one tab always
    /// exists). Returns the tile ids that left the layout - their sessions
    /// SURVIVE server-side, but the caller must drop their pool attachments.
    pub fn close_tab(&mut self, i: usize) -> Option<Vec<String>> {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return None;
        }
        self.renaming = None;
        let removed = self.tabs.remove(i).tiles;
        for id in &removed {
            self.hidden.insert(id.clone());
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.fixup_focus();
        Some(removed)
    }

    /// Switch to tab `i`. Cancels a rename in progress and refocuses within the
    /// new tab (keys must never reach an invisible terminal).
    pub fn set_active(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
            self.renaming = None;
            self.fixup_focus();
        }
    }

    // -- tab rename mode ------------------------------------------------------

    pub fn begin_rename(&mut self, tab: usize) {
        if tab < self.tabs.len() {
            self.renaming = Some(Rename { tab, buffer: self.tabs[tab].name.clone() });
        }
    }

    pub fn rename_push(&mut self, s: &str) {
        if let Some(r) = &mut self.renaming {
            r.buffer.push_str(s);
        }
    }

    pub fn rename_backspace(&mut self) {
        if let Some(r) = &mut self.renaming {
            r.buffer.pop();
        }
    }

    /// Commit the rename. A blank buffer keeps the old name (mirrors the
    /// webview refusing empty tab names).
    pub fn commit_rename(&mut self) {
        if let Some(r) = self.renaming.take() {
            let name = r.buffer.trim();
            if !name.is_empty() && r.tab < self.tabs.len() {
                self.tabs[r.tab].name = name.to_string();
            }
        }
    }

    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }

    // -- tiles ----------------------------------------------------------------

    /// Every tile in the layout, across all tabs (the attach pool's target set).
    pub fn all_tiles(&self) -> Vec<String> {
        self.tabs.iter().flat_map(|t| t.tiles.iter().cloned()).collect()
    }

    pub fn contains_tile(&self, id: &str) -> bool {
        self.tabs.iter().any(|t| t.tiles.iter().any(|x| x == id))
    }

    /// The font override of the workspace holding `id` (applied at attach).
    pub fn font_for(&self, id: &str) -> Option<&FontSpec> {
        self.tabs
            .iter()
            .find(|t| t.tiles.iter().any(|x| x == id))
            .and_then(|t| t.font.as_ref())
    }

    /// The active workspace's tiles, in paint order.
    pub fn active_tiles(&self) -> &[String] {
        &self.tabs[self.active].tiles
    }

    /// Close one tile: remove it from the layout and hide it for this run. The
    /// session survives server-side (native close = detach; the webview's
    /// kill-with-confirm needs the busy signal and is deferred with the
    /// supervision UX). Returns whether the tile was present (caller detaches).
    pub fn close_tile(&mut self, id: &str) -> bool {
        let present = self.remove_tile(id);
        if present {
            self.hidden.insert(id.to_string());
        }
        present
    }

    /// Remove a tile from every tab WITHOUT hiding it (an attach failure uses
    /// this so the next reconcile retries). Returns whether it was present.
    pub fn remove_tile(&mut self, id: &str) -> bool {
        let mut present = false;
        for tab in &mut self.tabs {
            let before = tab.tiles.len();
            tab.tiles.retain(|x| x != id);
            present |= tab.tiles.len() != before;
        }
        if present {
            self.fixup_focus();
        }
        present
    }

    /// Focus a tile by id (must be in the layout; clicking guarantees that).
    pub fn set_focused(&mut self, id: &str) {
        if self.contains_tile(id) {
            self.focused = Some(id.to_string());
        }
    }

    /// Move a tile into another tab by tab id (T12 `move_tile` apply; webview
    /// `moveTileToTab` parity): unknown target tab or already-there is a no-op;
    /// the tile leaves its source tab and is APPENDED to the target's order; the
    /// target tab is NOT activated. Returns whether the move landed.
    pub fn move_tile_to_tab(&mut self, id: &str, tab_id: &str) -> bool {
        let Some(source) = self.owning_tab_of(id) else { return false };
        let Some(target) = self.tab_index_of_id(tab_id) else { return false };
        if source == target {
            return false;
        }
        self.tabs[source].tiles.retain(|x| x != id);
        self.tabs[target].tiles.push(id.to_string());
        self.fixup_focus();
        true
    }

    /// Reorder a tile before/after another WITHIN THE ACTIVE TAB (T12
    /// `move_tile` apply with `targetId`; webview `moveTile` parity): both ids
    /// must be in the active tab, the tile is spliced to the target's position,
    /// and the moved tile takes focus. Returns whether the reorder landed.
    pub fn reorder_tile(&mut self, id: &str, target_id: &str) -> bool {
        if id == target_id {
            return false;
        }
        let tiles = &mut self.tabs[self.active].tiles;
        let (Some(from), Some(to)) = (
            tiles.iter().position(|x| x == id),
            tiles.iter().position(|x| x == target_id),
        ) else {
            return false;
        };
        let moved = tiles.remove(from);
        tiles.insert(to, moved);
        self.focused = Some(id.to_string());
        true
    }

    /// Keep `focused` pointing at a tile IN THE ACTIVE TAB, so key input never
    /// goes to a hidden terminal: if it left the tab (switch/close/removal),
    /// fall back to the active tab's first tile.
    fn fixup_focus(&mut self) {
        let in_active = self
            .focused
            .as_ref()
            .is_some_and(|f| self.tabs[self.active].tiles.iter().any(|x| x == f));
        if !in_active {
            self.focused = self.tabs[self.active].tiles.first().cloned();
        }
    }

    // -- live reconciliation ---------------------------------------------------

    /// Record a named-placement intent (T12 `add_worktree_workspace` apply): the
    /// next new session whose cwd is `worktree_path` (or inside it) lands in tab
    /// `tab_id` instead of the active tab, consuming the intent. One intent per
    /// path (a re-apply replaces it).
    pub fn note_pending_placement(&mut self, worktree_path: &str, tab_id: &str) {
        let path = worktree_path.trim_end_matches('/').to_string();
        if path.is_empty() {
            return;
        }
        self.pending_placements.retain(|(p, _)| *p != path);
        self.pending_placements.push((path, tab_id.to_string()));
    }

    /// Reconcile the layout against the server's live session list
    /// (`list_terminals`): sessions that died leave every tab; live sessions not
    /// in any tab (and not user-hidden) join the ACTIVE tab (the webview boots
    /// the same way: persisted order first, unknown live terminals appended).
    /// Dead ids also leave the hidden set, so it never leaks.
    pub fn reconcile(&mut self, live: &[String]) -> Reconcile {
        let with_cwds: Vec<(String, String)> =
            live.iter().map(|id| (id.clone(), String::new())).collect();
        self.reconcile_with_cwds(&with_cwds)
    }

    /// [`reconcile`](Self::reconcile) with each live session's cwd, so pending
    /// worktree placements (T12) can route a new session into its named tab: a
    /// new unplaced session whose cwd is under a pending worktree path joins
    /// THAT tab (consuming the intent); everything else joins the active tab.
    pub fn reconcile_with_cwds(&mut self, live: &[(String, String)]) -> Reconcile {
        let live_set: HashSet<&str> = live.iter().map(|(id, _)| id.as_str()).collect();
        let mut out = Reconcile::default();

        for tab in &mut self.tabs {
            tab.tiles.retain(|id| {
                let alive = live_set.contains(id.as_str());
                if !alive {
                    out.removed.push(id.clone());
                }
                alive
            });
        }
        self.hidden.retain(|id| live_set.contains(id.as_str()));
        // An intent whose target tab was closed can never place; drop it.
        let tab_ids: HashSet<String> = self.tabs.iter().map(|t| t.id.clone()).collect();
        self.pending_placements.retain(|(_, tab_id)| tab_ids.contains(tab_id));

        let placed: HashSet<String> = self.all_tiles().into_iter().collect();
        for (id, cwd) in live {
            if placed.contains(id) || self.hidden.contains(id) {
                continue;
            }
            let target = match self.take_pending_for(cwd) {
                Some(tab_id) => self.tab_index_of_id(&tab_id).unwrap_or(self.active),
                None => self.active,
            };
            self.tabs[target].tiles.push(id.clone());
            out.added.push(id.clone());
        }

        if !out.added.is_empty() || !out.removed.is_empty() {
            self.fixup_focus();
        }
        out
    }

    /// Pop the pending placement matching `cwd` (exact dir or inside it), if any.
    /// The path-segment boundary mirrors the webview's worktree cwd match
    /// (`/x/wt` must not match `/x/wt-other`).
    fn take_pending_for(&mut self, cwd: &str) -> Option<String> {
        let cwd = cwd.trim_end_matches('/');
        if cwd.is_empty() {
            return None;
        }
        let i = self
            .pending_placements
            .iter()
            .position(|(path, _)| cwd == path || cwd.starts_with(&format!("{path}/")))?;
        Some(self.pending_placements.remove(i).1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // -- splitRows / tile_boxes -------------------------------------------------

    #[test]
    fn split_rows_matches_the_webview() {
        assert_eq!(split_rows(0), Vec::<usize>::new());
        assert_eq!(split_rows(1), vec![1]);
        assert_eq!(split_rows(2), vec![2]);
        assert_eq!(split_rows(3), vec![2, 1]);
        assert_eq!(split_rows(4), vec![2, 2]);
        assert_eq!(split_rows(5), vec![3, 2]); // NOT 3x2-with-a-hole
        assert_eq!(split_rows(7), vec![3, 2, 2]);
        assert_eq!(split_rows(12), vec![4, 4, 4]);
        assert_eq!(split_rows(13), vec![4, 3, 3, 3]);
    }

    #[test]
    fn tile_boxes_pack_rows_fully() {
        // 3 tiles in 100x100 with gap 10: rows [2,1]; the second row's single
        // tile spans the FULL width (the semantic difference from the T5 grid).
        let boxes = tile_boxes(3, RectF::new(0.0, 0.0, 100.0, 100.0), 10.0);
        assert_eq!(boxes.len(), 3);
        assert_eq!(boxes[0], RectF::new(0.0, 0.0, 45.0, 45.0));
        assert_eq!(boxes[1], RectF::new(55.0, 0.0, 45.0, 45.0));
        assert_eq!(boxes[2], RectF::new(0.0, 55.0, 100.0, 45.0));
    }

    #[test]
    fn tile_boxes_honor_the_area_origin() {
        let boxes = tile_boxes(1, RectF::new(20.0, 40.0, 60.0, 30.0), 6.0);
        assert_eq!(boxes, vec![RectF::new(20.0, 40.0, 60.0, 30.0)]);
    }

    #[test]
    fn sidebar_layout_stacks_rows_and_reserves_the_overlay_mount() {
        let sb = sidebar_layout(2, RectF::new(10.0, 50.0, 200.0, 800.0));
        assert_eq!(sb.rows.len(), 2);
        // Full-width rows stacked with the gap.
        assert_eq!(sb.rows[0], RectF::new(10.0, 50.0, 200.0, 28.0));
        assert_eq!(sb.rows[1], RectF::new(10.0, 80.0, 200.0, 28.0));
        // Close zones flush right inside their rows.
        assert_eq!(sb.closes[0], RectF::new(182.0, 50.0, 28.0, 28.0));
        // "+" row after the workspaces; T9 overlay mount consumes the rest.
        assert_eq!(sb.plus, RectF::new(10.0, 110.0, 200.0, 28.0));
        assert_eq!(sb.overlay_mount, RectF::new(10.0, 140.0, 200.0, 710.0));
        // The mount bottoms out exactly at the sidebar area's bottom.
        assert_eq!(sb.overlay_mount.y + sb.overlay_mount.h, 850.0);
    }

    // -- tab lifecycle -----------------------------------------------------------

    #[test]
    fn add_tab_names_and_activates_like_the_webview() {
        let mut m = ChromeModel::default();
        assert_eq!(m.tabs[0].name, "Workspace 1");
        let i = m.add_tab();
        assert_eq!(i, 1);
        assert_eq!(m.tabs[1].name, "Workspace 2");
        assert_eq!(m.active, 1);
    }

    #[test]
    fn close_tab_refuses_the_last_and_hides_its_tiles() {
        let mut m = ChromeModel::default();
        assert_eq!(m.close_tab(0), None); // never close the last tab

        m.tabs[0].tiles = ids(&["aa", "bb"]);
        m.add_tab();
        m.set_active(0);
        let removed = m.close_tab(0).unwrap();
        assert_eq!(removed, ids(&["aa", "bb"]));
        assert_eq!(m.tabs.len(), 1);
        assert_eq!(m.active, 0);
        // The hidden tiles do NOT come back on reconcile while they live...
        let out = m.reconcile(&ids(&["aa", "bb"]));
        assert_eq!(out, Reconcile::default());
        // ...but a dead one leaves the hidden set, and a fresh one arrives.
        let out = m.reconcile(&ids(&["bb", "cc"]));
        assert_eq!(out.added, ids(&["cc"]));
    }

    #[test]
    fn closing_an_earlier_tab_keeps_the_active_workspace() {
        let mut m = ChromeModel::default();
        m.add_tab();
        m.add_tab(); // active = 2
        m.close_tab(0).unwrap();
        assert_eq!(m.active, 1); // still the same workspace, shifted down
        assert_eq!(m.tabs.len(), 2);
    }

    // -- rename mode ---------------------------------------------------------------

    #[test]
    fn rename_flow_edits_commits_and_cancels() {
        let mut m = ChromeModel::default();
        m.begin_rename(0);
        assert_eq!(m.renaming.as_ref().unwrap().buffer, "Workspace 1");
        for _ in 0.."Workspace 1".len() {
            m.rename_backspace();
        }
        m.rename_push("ops");
        m.commit_rename();
        assert_eq!(m.tabs[0].name, "ops");
        assert!(m.renaming.is_none());

        m.begin_rename(0);
        m.rename_push("!!!");
        m.cancel_rename();
        assert_eq!(m.tabs[0].name, "ops"); // cancel keeps the old name

        m.begin_rename(0);
        for _ in 0..10 {
            m.rename_backspace();
        }
        m.commit_rename();
        assert_eq!(m.tabs[0].name, "ops"); // blank commit keeps the old name
    }

    #[test]
    fn switching_tabs_cancels_a_rename_in_progress() {
        let mut m = ChromeModel::default();
        m.add_tab();
        m.begin_rename(1);
        m.set_active(0);
        assert!(m.renaming.is_none());
    }

    // -- tiles, focus, reconcile -----------------------------------------------------

    #[test]
    fn reconcile_populates_the_active_tab_and_prunes_the_dead() {
        let mut m = ChromeModel::default();
        let out = m.reconcile(&ids(&["aa", "bb", "cc"]));
        assert_eq!(out.added, ids(&["aa", "bb", "cc"]));
        assert_eq!(m.active_tiles(), ids(&["aa", "bb", "cc"]).as_slice());
        assert_eq!(m.focused.as_deref(), Some("aa"));

        // "bb" dies; a new "dd" appears while tab 2 is active.
        m.add_tab();
        let out = m.reconcile(&ids(&["aa", "cc", "dd"]));
        assert_eq!(out.removed, ids(&["bb"]));
        assert_eq!(out.added, ids(&["dd"]));
        assert_eq!(m.tabs[0].tiles, ids(&["aa", "cc"]));
        assert_eq!(m.tabs[1].tiles, ids(&["dd"]));
    }

    #[test]
    fn close_tile_detaches_and_stays_hidden_until_death() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb"]));
        assert!(m.close_tile("aa"));
        assert!(!m.contains_tile("aa"));
        assert_eq!(m.focused.as_deref(), Some("bb"));
        // Still live -> stays hidden.
        let out = m.reconcile(&ids(&["aa", "bb"]));
        assert_eq!(out, Reconcile::default());
        assert!(!m.close_tile("aa")); // absent -> false
    }

    #[test]
    fn focus_follows_the_active_tab() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb"]));
        m.set_focused("bb");
        m.add_tab();
        m.reconcile(&ids(&["aa", "bb", "cc"])); // "cc" joins tab 2
        assert_eq!(m.focused.as_deref(), Some("cc"));
        m.set_active(0);
        assert_eq!(m.focused.as_deref(), Some("aa")); // never a hidden tile
        m.set_focused("bb");
        assert_eq!(m.focused.as_deref(), Some("bb"));
    }

    #[test]
    fn from_layout_sanitizes() {
        let m = ChromeModel::from_layout(Vec::new(), 7);
        assert_eq!(m.tabs.len(), 1);
        assert_eq!(m.active, 0);
        let m = ChromeModel::from_layout(
            vec![
                Workspace { id: "ta".into(), name: "a".into(), tiles: ids(&["x"]), font: None },
                Workspace { id: "tb".into(), name: "b".into(), tiles: Vec::new(), font: None },
            ],
            9,
        );
        assert_eq!(m.active, 1);
        assert_eq!(m.tabs[0].tiles, ids(&["x"]));
    }

    // -- T12: id-addressed mutations (the MCP apply surface) ---------------------

    #[test]
    fn add_tab_reuses_the_lowest_free_workspace_index() {
        // Webview addTab parity: with "Workspace 1" and "Workspace 3" present,
        // the next tab is "Workspace 2", not "Workspace 4".
        let mut m = ChromeModel::default();
        m.add_tab(); // Workspace 2
        m.tabs[1].name = "Workspace 3".to_string();
        let i = m.add_tab();
        assert_eq!(m.tabs[i].name, "Workspace 2");
        // Ids are minted and unique.
        assert_ne!(m.tabs[0].id, m.tabs[1].id);
        assert!(!m.tabs[i].id.is_empty());
    }

    #[test]
    fn adopt_tab_creates_by_id_or_activates_the_existing_one() {
        let mut m = ChromeModel::default();
        // Create: the tab carries the SERVER-minted id verbatim and activates.
        assert!(m.adopt_tab("core-id-1", "Logs"));
        assert_eq!(m.tabs[1].id, "core-id-1");
        assert_eq!(m.tabs[1].name, "Logs");
        assert_eq!(m.active, 1);
        // Existing id: activate only - no rename, no duplicate (webview parity).
        m.set_active(0);
        assert!(!m.adopt_tab("core-id-1", "Renamed"));
        assert_eq!(m.tabs.len(), 2);
        assert_eq!(m.tabs[1].name, "Logs");
        assert_eq!(m.active, 1);
        // Blank name defaults to "Workspace".
        m.adopt_tab("core-id-2", "   ");
        assert_eq!(m.tabs[2].name, "Workspace");
    }

    #[test]
    fn rename_tab_by_id_trims_and_refuses_blank_or_unknown() {
        let mut m = ChromeModel::default();
        let id = m.tabs[0].id.clone();
        assert!(m.rename_tab_by_id(&id, "  ops  "));
        assert_eq!(m.tabs[0].name, "ops");
        assert!(!m.rename_tab_by_id(&id, "   "));
        assert_eq!(m.tabs[0].name, "ops");
        assert!(!m.rename_tab_by_id("nope", "x"));
    }

    #[test]
    fn set_active_by_id_switches_and_ignores_unknown() {
        let mut m = ChromeModel::default();
        m.add_tab();
        let first = m.tabs[0].id.clone();
        assert!(m.set_active_by_id(&first));
        assert_eq!(m.active, 0);
        assert!(!m.set_active_by_id("nope"));
        assert_eq!(m.active, 0);
    }

    #[test]
    fn move_tile_to_tab_appends_without_activating() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb"]));
        m.add_tab();
        m.reconcile(&ids(&["aa", "bb", "cc"])); // "cc" joins tab 2
        let target = m.tabs[1].id.clone();
        m.set_active(0);

        assert!(m.move_tile_to_tab("aa", &target));
        assert_eq!(m.tabs[0].tiles, ids(&["bb"]));
        assert_eq!(m.tabs[1].tiles, ids(&["cc", "aa"])); // appended at the end
        assert_eq!(m.active, 0); // target NOT activated (webview parity)
        assert_eq!(m.focused.as_deref(), Some("bb")); // focus stays in the active tab

        // Already there / unknown target / unplaced tile: no-ops.
        assert!(!m.move_tile_to_tab("aa", &target));
        assert!(!m.move_tile_to_tab("bb", "nope"));
        assert!(!m.move_tile_to_tab("ghost", &target));
    }

    #[test]
    fn reorder_tile_splices_within_the_active_tab_and_focuses() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb", "cc"]));
        assert!(m.reorder_tile("cc", "aa"));
        assert_eq!(m.active_tiles(), ids(&["cc", "aa", "bb"]).as_slice());
        assert_eq!(m.focused.as_deref(), Some("cc"));
        // Target in another tab (or missing): refuse - active-tab-only semantics.
        m.add_tab();
        m.reconcile(&ids(&["aa", "bb", "cc", "dd"]));
        assert!(!m.reorder_tile("dd", "aa"));
        assert!(!m.reorder_tile("aa", "aa"));
    }

    #[test]
    fn pending_placement_routes_a_matching_new_session_into_its_tab() {
        let mut m = ChromeModel::default();
        m.adopt_tab("wt-tab", "feature-x");
        m.set_active(0);
        m.note_pending_placement("/repo/wt/", "wt-tab"); // trailing slash normalized

        // A new session INSIDE the worktree joins the named tab (consuming the
        // intent); an unrelated one still joins the active tab. `/repo/wt-other`
        // must NOT match (path-segment boundary).
        let out = m.reconcile_with_cwds(&[
            ("s1".into(), "/repo/wt-other".into()),
            ("s2".into(), "/repo/wt/sub".into()),
        ]);
        assert_eq!(out.added, ids(&["s1", "s2"]));
        assert_eq!(m.tabs[0].tiles, ids(&["s1"]));
        assert_eq!(m.tabs[1].tiles, ids(&["s2"]));

        // Consumed: the next worktree session lands in the active tab like any other.
        m.reconcile_with_cwds(&[
            ("s1".into(), "/repo/wt-other".into()),
            ("s2".into(), "/repo/wt/sub".into()),
            ("s3".into(), "/repo/wt".into()),
        ]);
        assert_eq!(m.tabs[0].tiles, ids(&["s1", "s3"]));
    }

    #[test]
    fn pending_placement_for_a_closed_tab_is_dropped() {
        let mut m = ChromeModel::default();
        m.adopt_tab("wt-tab", "feature-x");
        m.note_pending_placement("/repo/wt", "wt-tab");
        m.set_active(0);
        m.close_tab(1).unwrap();
        // The intent's tab is gone: the session falls back to the active tab.
        m.reconcile_with_cwds(&[("s1".into(), "/repo/wt".into())]);
        assert_eq!(m.tabs[0].tiles, ids(&["s1"]));
    }
}
