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

use std::collections::{HashMap, HashSet};

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
/// share the width evenly (the webview's even flex split). The T26 manual
/// ratios ride [`tile_boxes_ratio`]; this is the no-ratio auto grid.
pub fn tile_boxes(n: usize, area: RectF, gap: f32) -> Vec<RectF> {
    tile_boxes_ratio(n, area, gap, None)
}

// ---------------------------------------------------------------------------
// Adjustable pane ratios (T26)
// ---------------------------------------------------------------------------

/// Manual grid ratios for one workspace (T26): each row's height fraction and
/// its tiles' width fractions. Fractions are of the USABLE extent (area minus
/// gaps); rows sum to 1 and each row's cols sum to 1 once
/// [sanitized](Self::sanitized).
///
/// Ratios describe ONE grid shape (the [`split_rows`] buckets at the tile
/// count they were dragged at). When the tile count reflows to a different
/// shape they simply do not apply (the auto grid paints) - but they are KEPT,
/// so session churn that returns to the old count restores the user's sizes.
/// A divider double-click clears them (back to auto for good).
#[derive(Clone, Debug, PartialEq)]
pub struct GridRatios {
    pub rows: Vec<RowRatio>,
}

/// One row of [`GridRatios`]: the row's height fraction and its tiles' width
/// fractions.
#[derive(Clone, Debug, PartialEq)]
pub struct RowRatio {
    pub h: f32,
    pub cols: Vec<f32>,
}

impl GridRatios {
    /// Even ratios for the given row buckets - the starting point when a drag
    /// begins on an auto grid.
    pub fn even(buckets: &[usize]) -> GridRatios {
        let rows = buckets.len().max(1) as f32;
        GridRatios {
            rows: buckets
                .iter()
                .map(|&count| RowRatio {
                    h: 1.0 / rows,
                    cols: vec![1.0 / count.max(1) as f32; count.max(1)],
                })
                .collect(),
        }
    }

    /// Whether these ratios describe exactly this grid shape.
    pub fn matches(&self, buckets: &[usize]) -> bool {
        self.rows.len() == buckets.len()
            && self.rows.iter().zip(buckets).all(|(r, &count)| r.cols.len() == count)
    }

    /// Validate + normalize (row heights sum to 1, each row's cols sum to 1).
    /// `None` when the shape or numbers are unusable (empty, non-finite,
    /// non-positive) - a hand-edited or future layout file must never poison
    /// the paint math, it just falls back to auto.
    pub fn sanitized(self) -> Option<GridRatios> {
        if self.rows.is_empty() {
            return None;
        }
        let ok = |v: f32| v.is_finite() && v > 0.0;
        let h_sum: f32 = self.rows.iter().map(|r| r.h).sum();
        if !ok(h_sum) || self.rows.iter().any(|r| !ok(r.h) || r.cols.is_empty()) {
            return None;
        }
        let mut out = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let c_sum: f32 = row.cols.iter().copied().sum();
            if !ok(c_sum) || row.cols.iter().any(|&c| !ok(c)) {
                return None;
            }
            out.push(RowRatio {
                h: row.h / h_sum,
                cols: row.cols.iter().map(|c| c / c_sum).collect(),
            });
        }
        Some(GridRatios { rows: out })
    }
}

/// The usable (gap-free) row heights and per-row tile widths, in pixels, for
/// `n` tiles in `area` - the single source [`tile_boxes_ratio`] and
/// [`divider_zones`] both build from. Ratios apply only when they match the
/// current shape; otherwise the even auto split.
fn grid_dims(n: usize, area: RectF, gap: f32, ratios: Option<&GridRatios>) -> (Vec<f32>, Vec<Vec<f32>>) {
    let buckets = split_rows(n);
    let rows = buckets.len();
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    let usable_h = (area.h - gap * (rows as f32 - 1.0)).max(rows as f32);
    let ratios = ratios.filter(|g| g.matches(&buckets));
    let heights: Vec<f32> = match ratios {
        Some(g) => g.rows.iter().map(|r| (r.h * usable_h).max(1.0)).collect(),
        None => vec![(usable_h / rows as f32).max(1.0); rows],
    };
    let widths: Vec<Vec<f32>> = buckets
        .iter()
        .enumerate()
        .map(|(r, &count)| {
            let usable_w = (area.w - gap * (count as f32 - 1.0)).max(count as f32);
            match ratios {
                Some(g) => g.rows[r].cols.iter().map(|c| (c * usable_w).max(1.0)).collect(),
                None => vec![(usable_w / count as f32).max(1.0); count],
            }
        })
        .collect();
    (heights, widths)
}

/// [`tile_boxes`] with optional per-workspace manual ratios (T26). Mismatched
/// or absent ratios paint the auto grid.
pub fn tile_boxes_ratio(n: usize, area: RectF, gap: f32, ratios: Option<&GridRatios>) -> Vec<RectF> {
    let (heights, widths) = grid_dims(n, area, gap, ratios);
    let mut out = Vec::with_capacity(n);
    let mut y = area.y;
    for (r, row_h) in heights.iter().enumerate() {
        let mut x = area.x;
        for w in &widths[r] {
            out.push(RectF::new(x, y, *w, *row_h));
            x += w + gap;
        }
        y += row_h + gap;
    }
    out
}

/// One draggable divider: the boundary between two rows, or between two tiles
/// of one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DividerId {
    /// Between row `r` and row `r + 1` (drag = row heights).
    Row(usize),
    /// Between tile `index` and `index + 1` of row `row` (drag = that row's
    /// column widths).
    Col { row: usize, index: usize },
}

/// The grabbable thickness of a divider hit zone, centered on the visual gap
/// (wider than the gap itself so the zone is actually hittable; it eats ~3px
/// off each neighboring tile's edge, well inside the tile border/padding).
pub const DIVIDER_HIT: f32 = 12.0;

/// Hit zones for every divider of the current grid, matching what
/// [`tile_boxes_ratio`] painted: row dividers span the area's width at each
/// row gap; column dividers span their row's height at each intra-row gap.
pub fn divider_zones(
    n: usize,
    area: RectF,
    gap: f32,
    ratios: Option<&GridRatios>,
) -> Vec<(DividerId, RectF)> {
    let (heights, widths) = grid_dims(n, area, gap, ratios);
    let mut out = Vec::new();
    let mut y = area.y;
    for (r, row_h) in heights.iter().enumerate() {
        let mut x = area.x;
        for (i, w) in widths[r].iter().enumerate() {
            if i + 1 < widths[r].len() {
                let cx = x + w + gap / 2.0;
                out.push((
                    DividerId::Col { row: r, index: i },
                    RectF::new(cx - DIVIDER_HIT / 2.0, y, DIVIDER_HIT, *row_h),
                ));
            }
            x += w + gap;
        }
        if r + 1 < heights.len() {
            let cy = y + row_h + gap / 2.0;
            out.push((
                DividerId::Row(r),
                RectF::new(area.x, cy - DIVIDER_HIT / 2.0, area.w, DIVIDER_HIT),
            ));
        }
        y += row_h + gap;
    }
    out
}

/// The pixel extent a divider drags within: for [`DividerId::Row(r)`] the
/// combined usable height of rows `r` and `r+1` plus where it starts; for a
/// column divider the combined usable width of the two neighboring tiles.
/// The view maps the pointer into this extent and hands the resulting split
/// fraction to [`apply_divider_split`]. `None` when the id is stale (the grid
/// reflowed under a drag).
pub fn divider_extent(
    n: usize,
    area: RectF,
    gap: f32,
    ratios: Option<&GridRatios>,
    id: DividerId,
) -> Option<(f32, f32)> {
    let (heights, widths) = grid_dims(n, area, gap, ratios);
    match id {
        DividerId::Row(r) => {
            if r + 1 >= heights.len() {
                return None;
            }
            let top = area.y + heights[..r].iter().map(|h| h + gap).sum::<f32>();
            Some((top, heights[r] + heights[r + 1]))
        }
        DividerId::Col { row, index } => {
            let w = widths.get(row)?;
            if index + 1 >= w.len() {
                return None;
            }
            let left = area.x + w[..index].iter().map(|x| x + gap).sum::<f32>();
            Some((left, w[index] + w[index + 1]))
        }
    }
}

/// Re-split the two fractions a divider separates: `t` is the first
/// neighbor's share of their combined extent (0..1), clamped so neither
/// neighbor drops below `min_t` of that extent. A stale id or an extent too
/// small to honor the minimum is a no-op. Returns whether anything changed.
pub fn apply_divider_split(g: &mut GridRatios, id: DividerId, t: f32, min_t: f32) -> bool {
    let split = |a: &mut f32, b: &mut f32| -> bool {
        let combined = *a + *b;
        let min_t = min_t.clamp(0.0, 0.5);
        let t = t.clamp(min_t, 1.0 - min_t);
        let na = combined * t;
        if (na - *a).abs() < f32::EPSILON {
            return false;
        }
        *b = combined - na;
        *a = na;
        true
    };
    if min_t > 0.5 {
        return false; // extent too small for two minimum-sized panes
    }
    match id {
        DividerId::Row(r) => {
            if r + 1 >= g.rows.len() {
                return false;
            }
            let (left, right) = g.rows.split_at_mut(r + 1);
            split(&mut left[r].h, &mut right[0].h)
        }
        DividerId::Col { row, index } => {
            let Some(cols) = g.rows.get_mut(row).map(|r| &mut r.cols) else { return false };
            if index + 1 >= cols.len() {
                return false;
            }
            let (left, right) = cols.split_at_mut(index + 1);
            split(&mut left[index], &mut right[0])
        }
    }
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
    /// Tear-off zones (T10), one per row, just left of the close zone: tears a
    /// main workspace into a satellite window / returns a torn-off one.
    pub tears: Vec<RectF>,
    pub plus: RectF,
    /// The rest of the sidebar below the workspace section. T9's overlay
    /// sections mount here; the T8 chrome paints NOTHING inside it.
    pub overlay_mount: RectF,
}

/// Lay out the sidebar's workspace section inside `area` (the sidebar's inner
/// content box, below the section caption): `n` full-width rows, each with a
/// square close zone flush right and a square tear-off zone left of it, then
/// the `+` row, then the T9 mount.
pub fn sidebar_layout(n: usize, area: RectF) -> SidebarLayout {
    let mut rows = Vec::with_capacity(n);
    let mut closes = Vec::with_capacity(n);
    let mut tears = Vec::with_capacity(n);
    let mut y = area.y;
    for _ in 0..n {
        rows.push(RectF::new(area.x, y, area.w, SIDEBAR_ROW_H));
        closes.push(RectF::new(
            area.x + area.w - SIDEBAR_ROW_H,
            y,
            SIDEBAR_ROW_H,
            SIDEBAR_ROW_H,
        ));
        tears.push(RectF::new(
            area.x + area.w - 2.0 * SIDEBAR_ROW_H,
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
    SidebarLayout { rows, closes, tears, plus, overlay_mount }
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
    /// Manual pane ratios (T26), persisted per workspace. `None` = the auto
    /// grid; ratios whose shape no longer matches the tile count lie dormant
    /// (auto paints) until the count returns or a divider double-click clears
    /// them.
    pub grid: Option<GridRatios>,
    /// Torn off into its own OS window (T10 satellite). The workspace STAYS in
    /// `tabs` - one source of truth for tiles/reconcile/persist - this flag only
    /// says where it paints: the satellite window instead of the main grid.
    pub satellite: bool,
    /// Stable runtime identity binding a satellite window to its workspace (tab
    /// indices shift as tabs close). Assigned by the model, monotonic per run;
    /// NOT persisted - the layout file's tab order is the durable identity.
    pub wsid: u64,
}

impl Workspace {
    /// A fresh main-window workspace with a minted id and no tiles. `wsid` is a
    /// placeholder (0): the model stamps a real one when the workspace enters
    /// `tabs` ([`ChromeModel::from_layout`] reassigns; `add_tab`/`adopt_tab`
    /// mint from `next_wsid`).
    pub fn new(name: impl Into<String>) -> Self {
        Workspace {
            id: mint_tab_id(),
            name: name.into(),
            tiles: Vec::new(),
            font: None,
            grid: None,
            satellite: false,
            wsid: 0,
        }
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

/// Work-name editing state (N1): which tile's header is being edited, the cwd
/// the name will be stored under, and the in-progress buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct NameEdit {
    pub tile: String,
    pub cwd: String,
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
///
/// ## Satellite invariants (T10)
/// - `active` points at a NON-satellite tab whenever one exists ([`Self::fixup_active`]);
///   only when every workspace is torn off may it rest on a satellite, and then
///   the main grid paints nothing ([`Self::main_tiles`] returns `None`).
/// - `focused` is the MAIN window's focused tile (satellite windows keep their
///   own focus in the window registry), so it is `None` while `active` rests on
///   a satellite.
/// - New sessions always land in a main (non-satellite) workspace; if none
///   exists, [`Self::reconcile`] creates one.
#[derive(Clone, Debug)]
pub struct ChromeModel {
    pub tabs: Vec<Workspace>,
    pub active: usize,
    pub focused: Option<String>,
    pub renaming: Option<Rename>,
    /// In-progress work-name edit (N1), captured by the header click.
    pub naming: Option<NameEdit>,
    /// User-assigned work names, keyed by session cwd (webview parity:
    /// `t-hub.theme.workNames` keys by project path, so every tile on that
    /// cwd shares the name). Persisted with the layout.
    pub work_names: HashMap<String, String>,
    /// Tiles closed by the user this run. NOT persisted: a client restart
    /// re-lists everything live (self-healing, and the only "reopen" story until
    /// the T9 recents overlay).
    hidden: HashSet<String>,
    /// Named-placement intents from worktree applies (T12): (normalized worktree
    /// path, target tab id). The next reconciled session whose cwd lives under
    /// the path lands in that tab instead of the active one, consuming the
    /// entry. NOT persisted - an intent only makes sense within this run.
    pending_placements: Vec<(String, String)>,
    /// Next workspace id to assign (see [`Workspace::wsid`]).
    next_wsid: u64,
}

impl Default for ChromeModel {
    fn default() -> Self {
        let mut first = Workspace::new("Workspace 1");
        first.wsid = 1;
        ChromeModel {
            tabs: vec![first],
            active: 0,
            focused: None,
            renaming: None,
            naming: None,
            work_names: HashMap::new(),
            hidden: HashSet::new(),
            pending_placements: Vec::new(),
            next_wsid: 2,
        }
    }
}

impl ChromeModel {
    /// Rebuild from persisted layout, sanitized: at least one tab, `active` in
    /// range (and never resting on a satellite while a main tab exists), fresh
    /// `wsid`s. Tile liveness is reconciled separately against `list_terminals`.
    pub fn from_layout(tabs: Vec<Workspace>, active: usize) -> Self {
        let mut m = ChromeModel::default();
        if !tabs.is_empty() {
            m.tabs = tabs;
            m.active = active.min(m.tabs.len() - 1);
        }
        // Persisted wsids are meaningless (runtime identity); reassign.
        m.next_wsid = 1;
        for tab in &mut m.tabs {
            tab.wsid = m.next_wsid;
            m.next_wsid += 1;
        }
        m.fixup_active();
        m.fixup_focus();
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
        let mut ws = Workspace::new(format!("Workspace {n}"));
        ws.wsid = self.next_wsid;
        self.next_wsid += 1;
        self.tabs.push(ws);
        self.active = self.tabs.len() - 1;
        self.fixup_focus();
        self.active
    }

    /// The index of the tab with this id, if any (T12 id-addressed mutations).
    pub fn tab_index_of_id(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Adopt a tab by id (T12 `new_tab` apply, webview `adoptTab` parity): if a
    /// tab with this id exists, just activate it (no rename; a torn-off satellite
    /// is NOT activated - its tiles paint in their own window); otherwise create
    /// it with this id + name (blank name -> "Workspace") and activate it.
    /// Returns whether a tab was created.
    pub fn adopt_tab(&mut self, id: &str, name: &str) -> bool {
        self.renaming = None;
        if let Some(i) = self.tab_index_of_id(id) {
            self.set_active(i);
            return false;
        }
        let name = name.trim();
        self.tabs.push(Workspace {
            id: id.to_string(),
            name: if name.is_empty() { "Workspace".to_string() } else { name.to_string() },
            tiles: Vec::new(),
            font: None,
            grid: None,
            satellite: false,
            wsid: self.next_wsid,
        });
        self.next_wsid += 1;
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

    /// Activate a tab by id (T12 `focus_tab` apply). Unknown id is a no-op, and
    /// so is a torn-off satellite ([`Self::set_active`] refuses those - the main
    /// grid cannot show them). Returns whether the tab is now active.
    pub fn set_active_by_id(&mut self, id: &str) -> bool {
        match self.tab_index_of_id(id) {
            Some(i) => {
                self.set_active(i);
                self.active == i
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
    /// SURVIVE server-side, but the caller must drop their pool attachments
    /// (and, if the tab was a satellite, close its OS window).
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
        self.fixup_active();
        self.fixup_focus();
        Some(removed)
    }

    /// Switch to tab `i`. Cancels a rename in progress and refocuses within the
    /// new tab (keys must never reach an invisible terminal). Refuses satellite
    /// tabs - their tiles paint in their own window, so "activating" one in the
    /// main grid would double-paint it (the view activates the OS window instead).
    pub fn set_active(&mut self, i: usize) {
        if i < self.tabs.len() && !self.tabs[i].satellite {
            self.active = i;
            self.renaming = None;
            self.fixup_focus();
        }
    }

    // -- satellites (T10) -----------------------------------------------------

    /// The tab index currently holding workspace `wsid` (indices shift as tabs
    /// close; the wsid is the stable handle satellite windows keep).
    pub fn tab_by_wsid(&self, wsid: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.wsid == wsid)
    }

    /// All torn-off workspaces, as `(tab index, wsid)` in tab order.
    pub fn satellite_tabs(&self) -> Vec<(usize, u64)> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.satellite)
            .map(|(i, t)| (i, t.wsid))
            .collect()
    }

    /// Tear workspace `i` off into its own window: flag it, move `active` (and
    /// main focus) off it. Returns its wsid, or `None` when `i` is out of range
    /// or already torn off. The caller opens the OS window.
    pub fn tear_off(&mut self, i: usize) -> Option<u64> {
        let tab = self.tabs.get_mut(i)?;
        if tab.satellite {
            return None;
        }
        tab.satellite = true;
        let wsid = tab.wsid;
        self.renaming = None;
        self.fixup_active();
        self.fixup_focus();
        Some(wsid)
    }

    /// Return a torn-off workspace to the main window and activate it (the user
    /// just brought it home - show it). Returns its tab index, or `None` when
    /// the wsid is unknown or not a satellite. The caller closes the OS window.
    pub fn close_back(&mut self, wsid: u64) -> Option<usize> {
        let i = self.tab_by_wsid(wsid)?;
        if !self.tabs[i].satellite {
            return None;
        }
        self.tabs[i].satellite = false;
        self.active = i;
        self.fixup_focus();
        Some(i)
    }

    /// Keep `active` off satellite tabs whenever a main (non-satellite) tab
    /// exists; when every workspace is torn off it stays where it is and
    /// [`Self::main_tiles`] reports the main grid as empty.
    fn fixup_active(&mut self) {
        if self.tabs[self.active].satellite {
            if let Some(i) = self.tabs.iter().position(|t| !t.satellite) {
                self.active = i;
            }
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

    // -- work names (N1) ------------------------------------------------------

    /// The user-assigned work name for a cwd, if any.
    pub fn work_name_for(&self, cwd: &str) -> Option<&str> {
        self.work_names.get(cwd).map(String::as_str)
    }

    /// Begin editing tile `id`'s work name (header click). Seeds the buffer
    /// with the current name so Enter without typing keeps it.
    pub fn begin_name_edit(&mut self, id: &str, cwd: &str) {
        let buffer = self.work_names.get(cwd).cloned().unwrap_or_default();
        self.naming = Some(NameEdit { tile: id.to_string(), cwd: cwd.to_string(), buffer });
    }

    pub fn name_push(&mut self, s: &str) {
        if let Some(n) = &mut self.naming {
            n.buffer.push_str(s);
        }
    }

    pub fn name_backspace(&mut self) {
        if let Some(n) = &mut self.naming {
            n.buffer.pop();
        }
    }

    /// Commit the work-name edit: a trimmed non-empty buffer stores the name,
    /// a blank buffer clears the slot (webview `setWorkName` semantics).
    /// Returns whether anything changed (the caller persists).
    pub fn commit_name(&mut self) -> bool {
        let Some(n) = self.naming.take() else { return false };
        let name = n.buffer.trim();
        if name.is_empty() {
            self.work_names.remove(&n.cwd).is_some()
        } else if self.work_names.get(&n.cwd).map(String::as_str) == Some(name) {
            false
        } else {
            self.work_names.insert(n.cwd, name.to_string());
            true
        }
    }

    pub fn cancel_name(&mut self) {
        self.naming = None;
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

    /// The tiles the MAIN window's grid paints: the active workspace's, unless
    /// every workspace is torn off (then the main grid is empty - a satellite
    /// paints those tiles, and painting them twice would fight over damage).
    pub fn main_tiles(&self) -> Option<&[String]> {
        let tab = &self.tabs[self.active];
        (!tab.satellite).then_some(tab.tiles.as_slice())
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

    /// Place a live tile into the ACTIVE tab and focus it (T12 spawn placement:
    /// the server minted the session; the reconcile attach follows). Refused
    /// when the tile is already placed or user-hidden, so racing the reconcile
    /// is idempotent. Like [`Self::reconcile`], a new tile must land in a MAIN
    /// workspace: when every workspace is torn off, a fresh one is created.
    pub fn place_tile(&mut self, id: &str) -> bool {
        if self.contains_tile(id) || self.hidden.contains(id) {
            return false;
        }
        let target = if self.tabs[self.active].satellite { self.add_tab() } else { self.active };
        self.tabs[target].tiles.push(id.to_string());
        self.focused = Some(id.to_string());
        true
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
    /// and the moved tile takes focus (unless the active tab is a torn-off
    /// satellite - the main window focuses nothing then).
    pub fn reorder_tile(&mut self, id: &str, target_id: &str) -> bool {
        if id == target_id {
            return false;
        }
        let tab = &mut self.tabs[self.active];
        let satellite = tab.satellite;
        let tiles = &mut tab.tiles;
        let (Some(from), Some(to)) = (
            tiles.iter().position(|x| x == id),
            tiles.iter().position(|x| x == target_id),
        ) else {
            return false;
        };
        let moved = tiles.remove(from);
        tiles.insert(to, moved);
        if !satellite {
            self.focused = Some(id.to_string());
        }
        true
    }

    /// Keep `focused` pointing at a tile IN THE ACTIVE TAB, so key input never
    /// goes to a hidden terminal: if it left the tab (switch/close/removal),
    /// fall back to the active tab's first tile. While the active tab is a
    /// satellite (every workspace torn off), the main window focuses nothing -
    /// each satellite window tracks its own focus in the window registry.
    fn fixup_focus(&mut self) {
        let tab = &self.tabs[self.active];
        if tab.satellite {
            self.focused = None;
            return;
        }
        let in_active = self.focused.as_ref().is_some_and(|f| tab.tiles.iter().any(|x| x == f));
        if !in_active {
            self.focused = tab.tiles.first().cloned();
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
    /// Dead ids also leave the hidden set, so it never leaks. New sessions must
    /// land where the user can see them arrive: a MAIN workspace - when every
    /// workspace is torn off, a fresh main workspace is created to receive them.
    pub fn reconcile(&mut self, live: &[String]) -> Reconcile {
        let with_cwds: Vec<(String, String)> =
            live.iter().map(|id| (id.clone(), String::new())).collect();
        self.reconcile_with_cwds(&with_cwds)
    }

    /// [`reconcile`](Self::reconcile) with each live session's cwd, so pending
    /// worktree placements (T12) can route a new session into its named tab: a
    /// new unplaced session whose cwd is under a pending worktree path joins
    /// THAT tab (consuming the intent). Everything else joins the active tab -
    /// unless that is a torn-off satellite (T10: new sessions must land where
    /// the user can see them arrive), then a fresh main workspace receives them.
    pub fn reconcile_with_cwds(&mut self, live: &[(String, String)]) -> Reconcile {
        self.reconcile_with_cwds_lingering(live, &HashSet::new())
    }

    /// [`reconcile_with_cwds`](Self::reconcile_with_cwds), keeping the
    /// `lingering` tiles PLACED even though the server no longer lists them
    /// (T24: a dead/exited tile stays visible with its badge for a linger
    /// window instead of silently vanishing - the supervision cue the general
    /// asked for). A lingering tile is never re-added or refocused; once its
    /// id leaves the set, the next pass removes it like any dead session.
    pub fn reconcile_with_cwds_lingering(
        &mut self,
        live: &[(String, String)],
        lingering: &HashSet<String>,
    ) -> Reconcile {
        let live_set: HashSet<&str> = live.iter().map(|(id, _)| id.as_str()).collect();
        let mut out = Reconcile::default();

        for tab in &mut self.tabs {
            tab.tiles.retain(|id| {
                let alive = live_set.contains(id.as_str()) || lingering.contains(id);
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
        // The no-intent target, resolved lazily ONCE (T10) so at most one
        // workspace is created per pass.
        let mut fallback: Option<usize> = None;
        for (id, cwd) in live {
            if placed.contains(id) || self.hidden.contains(id) {
                continue;
            }
            let target = match self.take_pending_for(cwd) {
                Some(tab_id) => self.tab_index_of_id(&tab_id).unwrap_or(self.active),
                None => *fallback.get_or_insert_with(|| {
                    if self.tabs[self.active].satellite {
                        self.add_tab() // all torn off: new arrivals need a main home
                    } else {
                        self.active
                    }
                }),
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

    // -- work names (N1) --------------------------------------------------------

    #[test]
    fn work_name_edit_commits_clears_and_cancels() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["t1"]));

        // Commit stores the trimmed name under the cwd.
        m.begin_name_edit("t1", "/repo/app");
        m.name_push("  auth");
        m.name_push(" fix ");
        assert!(m.commit_name());
        assert_eq!(m.work_name_for("/repo/app"), Some("auth fix"));
        assert!(m.naming.is_none());

        // Re-editing seeds the buffer with the current name; committing the
        // same name changes nothing.
        m.begin_name_edit("t1", "/repo/app");
        assert_eq!(m.naming.as_ref().unwrap().buffer, "auth fix");
        assert!(!m.commit_name());

        // Backspace edits; Esc cancels without touching the stored name.
        m.begin_name_edit("t1", "/repo/app");
        m.name_backspace();
        m.cancel_name();
        assert_eq!(m.work_name_for("/repo/app"), Some("auth fix"));

        // A blanked buffer clears the slot (webview setWorkName semantics).
        m.begin_name_edit("t1", "/repo/app");
        for _ in 0.."auth fix".len() {
            m.name_backspace();
        }
        assert!(m.commit_name());
        assert_eq!(m.work_name_for("/repo/app"), None);
        // Clearing an already-empty slot is a no-op.
        m.begin_name_edit("t1", "/repo/app");
        assert!(!m.commit_name());
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

    // -- grid ratios (T26) --------------------------------------------------------

    #[test]
    fn even_ratios_match_their_shape_and_reproduce_the_auto_grid() {
        let area = RectF::new(0.0, 0.0, 100.0, 100.0);
        let even = GridRatios::even(&split_rows(3));
        assert!(even.matches(&[2, 1]));
        assert!(!even.matches(&[3]));
        assert!(!even.matches(&[2, 2]));
        // Even ratios paint EXACTLY what the auto grid paints.
        assert_eq!(tile_boxes_ratio(3, area, 10.0, Some(&even)), tile_boxes(3, area, 10.0));
    }

    #[test]
    fn ratio_boxes_resize_rows_and_columns() {
        let area = RectF::new(0.0, 0.0, 100.0, 100.0);
        // 3 tiles -> rows [2,1]; give row 0 two thirds of the height and its
        // first tile two thirds of the width.
        let g = GridRatios {
            rows: vec![
                RowRatio { h: 2.0 / 3.0, cols: vec![2.0 / 3.0, 1.0 / 3.0] },
                RowRatio { h: 1.0 / 3.0, cols: vec![1.0] },
            ],
        };
        let boxes = tile_boxes_ratio(3, area, 10.0, Some(&g));
        // usable_h = 90 -> rows 60/30; row 0 usable_w = 90 -> 60/30.
        assert_eq!(boxes[0], RectF::new(0.0, 0.0, 60.0, 60.0));
        assert_eq!(boxes[1], RectF::new(70.0, 0.0, 30.0, 60.0));
        assert_eq!(boxes[2], RectF::new(0.0, 70.0, 100.0, 30.0));
    }

    #[test]
    fn mismatched_ratios_fall_back_to_the_auto_grid() {
        let area = RectF::new(0.0, 0.0, 100.0, 100.0);
        let g = GridRatios::even(&[2, 1]); // shaped for 3 tiles
        assert_eq!(tile_boxes_ratio(4, area, 10.0, Some(&g)), tile_boxes(4, area, 10.0));
    }

    #[test]
    fn sanitized_normalizes_and_rejects_garbage() {
        // Unnormalized fractions come out summing to 1.
        let g = GridRatios {
            rows: vec![
                RowRatio { h: 2.0, cols: vec![3.0, 1.0] },
                RowRatio { h: 2.0, cols: vec![5.0] },
            ],
        }
        .sanitized()
        .unwrap();
        assert!((g.rows[0].h - 0.5).abs() < 1e-6);
        assert!((g.rows[0].cols[0] - 0.75).abs() < 1e-6);
        assert!((g.rows[1].cols[0] - 1.0).abs() < 1e-6);
        // Garbage (hand-edited layout files) -> None, never a poisoned paint.
        assert!(GridRatios { rows: vec![] }.sanitized().is_none());
        assert!(GridRatios { rows: vec![RowRatio { h: 0.0, cols: vec![1.0] }] }
            .sanitized()
            .is_none());
        assert!(GridRatios { rows: vec![RowRatio { h: f32::NAN, cols: vec![1.0] }] }
            .sanitized()
            .is_none());
        assert!(GridRatios { rows: vec![RowRatio { h: 1.0, cols: vec![] }] }
            .sanitized()
            .is_none());
        assert!(GridRatios { rows: vec![RowRatio { h: 1.0, cols: vec![-1.0, 2.0] }] }
            .sanitized()
            .is_none());
    }

    #[test]
    fn divider_zones_sit_centered_on_the_gaps() {
        let area = RectF::new(0.0, 0.0, 100.0, 100.0);
        // 3 tiles -> rows [2,1]: one column divider in row 0, one row divider.
        let zones = divider_zones(3, area, 10.0, None);
        assert_eq!(zones.len(), 2);
        // Column divider: gap spans x 45..55, center 50; hit band is
        // DIVIDER_HIT wide, spanning the row's height.
        assert_eq!(
            zones[0],
            (DividerId::Col { row: 0, index: 0 }, RectF::new(50.0 - DIVIDER_HIT / 2.0, 0.0, DIVIDER_HIT, 45.0))
        );
        // Row divider: gap spans y 45..55, center 50; band spans the full width.
        assert_eq!(
            zones[1],
            (DividerId::Row(0), RectF::new(0.0, 50.0 - DIVIDER_HIT / 2.0, 100.0, DIVIDER_HIT))
        );
        // Counts scale with the shape: 12 tiles -> [4,4,4] = 3x3 col + 2 row.
        assert_eq!(divider_zones(12, area, 10.0, None).len(), 11);
        // 0/1 tiles have nothing to drag.
        assert!(divider_zones(0, area, 10.0, None).is_empty());
        assert!(divider_zones(1, area, 10.0, None).is_empty());
    }

    #[test]
    fn divider_extent_locates_the_neighbor_pair() {
        let area = RectF::new(0.0, 0.0, 100.0, 100.0);
        // Row divider of a 3-tile grid: rows are 45px each, pair starts at y=0.
        assert_eq!(divider_extent(3, area, 10.0, None, DividerId::Row(0)), Some((0.0, 90.0)));
        // Column divider in row 0: tiles are 45px each, pair starts at x=0.
        assert_eq!(
            divider_extent(3, area, 10.0, None, DividerId::Col { row: 0, index: 0 }),
            Some((0.0, 90.0))
        );
        // Stale ids (grid reflowed mid-drag) resolve to None, not a panic.
        assert_eq!(divider_extent(3, area, 10.0, None, DividerId::Row(1)), None);
        assert_eq!(
            divider_extent(3, area, 10.0, None, DividerId::Col { row: 1, index: 0 }),
            None
        );
        assert_eq!(
            divider_extent(3, area, 10.0, None, DividerId::Col { row: 5, index: 0 }),
            None
        );
    }

    #[test]
    fn apply_divider_split_moves_the_pair_and_clamps() {
        let mut g = GridRatios::even(&[2, 1]);
        // Row split at 70/30.
        assert!(apply_divider_split(&mut g, DividerId::Row(0), 0.7, 0.1));
        assert!((g.rows[0].h - 0.7).abs() < 1e-6);
        assert!((g.rows[1].h - 0.3).abs() < 1e-6);
        // Column split clamps to the minimum share.
        assert!(apply_divider_split(&mut g, DividerId::Col { row: 0, index: 0 }, 0.01, 0.2));
        assert!((g.rows[0].cols[0] - 0.2).abs() < 1e-6);
        assert!((g.rows[0].cols[1] - 0.8).abs() < 1e-6);
        // The fractions stay normalized through any sequence of drags.
        let h_sum: f32 = g.rows.iter().map(|r| r.h).sum();
        let c_sum: f32 = g.rows[0].cols.iter().sum();
        assert!((h_sum - 1.0).abs() < 1e-6 && (c_sum - 1.0).abs() < 1e-6);
        // Stale ids and an impossible minimum are no-ops.
        assert!(!apply_divider_split(&mut g, DividerId::Row(1), 0.5, 0.1));
        assert!(!apply_divider_split(&mut g, DividerId::Col { row: 1, index: 0 }, 0.5, 0.1));
        assert!(!apply_divider_split(&mut g, DividerId::Row(0), 0.5, 0.6));
    }

    // -- lingering dead tiles (T24) ----------------------------------------------

    #[test]
    fn lingering_tiles_stay_placed_until_released() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb"]));
        // "aa" dies but lingers: it stays placed, is NOT reported removed, and
        // is not re-added either.
        let linger: HashSet<String> = [String::from("aa")].into();
        let out = m.reconcile_with_cwds_lingering(&[("bb".into(), String::new())], &linger);
        assert_eq!(out, Reconcile::default());
        assert_eq!(m.active_tiles(), ids(&["aa", "bb"]).as_slice());
        // The linger window passes: the next pass removes it normally.
        let out = m.reconcile_with_cwds_lingering(&[("bb".into(), String::new())], &HashSet::new());
        assert_eq!(out.removed, ids(&["aa"]));
        assert_eq!(m.active_tiles(), ids(&["bb"]).as_slice());
    }

    #[test]
    fn sidebar_layout_stacks_rows_and_reserves_the_overlay_mount() {
        let sb = sidebar_layout(2, RectF::new(10.0, 50.0, 200.0, 800.0));
        assert_eq!(sb.rows.len(), 2);
        // Full-width rows stacked with the gap.
        assert_eq!(sb.rows[0], RectF::new(10.0, 50.0, 200.0, 28.0));
        assert_eq!(sb.rows[1], RectF::new(10.0, 80.0, 200.0, 28.0));
        // Close zones flush right inside their rows; tear-off zones left of them.
        assert_eq!(sb.closes[0], RectF::new(182.0, 50.0, 28.0, 28.0));
        assert_eq!(sb.tears[0], RectF::new(154.0, 50.0, 28.0, 28.0));
        assert_eq!(sb.tears[1], RectF::new(154.0, 80.0, 28.0, 28.0));
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

    fn ws(name: &str, tiles: &[&str], satellite: bool) -> Workspace {
        Workspace {
            id: mint_tab_id(),
            name: name.into(),
            tiles: ids(tiles),
            font: None,
            grid: None,
            satellite,
            wsid: 0,
        }
    }

    #[test]
    fn from_layout_sanitizes() {
        let m = ChromeModel::from_layout(Vec::new(), 7);
        assert_eq!(m.tabs.len(), 1);
        assert_eq!(m.active, 0);
        let m = ChromeModel::from_layout(
            vec![ws("a", &["x"], false), ws("b", &[], false)],
            9,
        );
        assert_eq!(m.active, 1);
        assert_eq!(m.tabs[0].tiles, ids(&["x"]));
        // Fresh, unique wsids are assigned regardless of what was loaded.
        assert_eq!(m.tabs[0].wsid, 1);
        assert_eq!(m.tabs[1].wsid, 2);
    }

    // -- satellites (T10) ---------------------------------------------------------

    #[test]
    fn tear_off_moves_active_and_focus_to_a_main_tab() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb"]));
        m.add_tab();
        m.reconcile(&ids(&["aa", "bb", "cc"])); // tab 1 holds "cc", active
        assert_eq!(m.active, 1);

        let wsid = m.tear_off(1).unwrap();
        assert_eq!(wsid, 2);
        assert!(m.tabs[1].satellite);
        // Active and main focus left the torn-off tab.
        assert_eq!(m.active, 0);
        assert_eq!(m.focused.as_deref(), Some("aa"));
        assert_eq!(m.main_tiles(), Some(ids(&["aa", "bb"]).as_slice()));
        assert_eq!(m.satellite_tabs(), vec![(1, 2)]);
        // Tearing off again refuses; out-of-range refuses.
        assert_eq!(m.tear_off(1), None);
        assert_eq!(m.tear_off(9), None);
    }

    #[test]
    fn tearing_off_everything_empties_the_main_grid() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa"]));
        let wsid = m.tear_off(0).unwrap();
        assert!(m.tabs[0].satellite);
        assert_eq!(m.main_tiles(), None); // main grid paints nothing
        assert_eq!(m.focused, None); // main window focuses nothing
        assert_eq!(m.active, 0); // parked - no main tab to move to

        // close_back returns it home and activates it.
        assert_eq!(m.close_back(wsid), Some(0));
        assert!(!m.tabs[0].satellite);
        assert_eq!(m.main_tiles(), Some(ids(&["aa"]).as_slice()));
        assert_eq!(m.focused.as_deref(), Some("aa"));
        // Closing back twice / an unknown wsid refuses.
        assert_eq!(m.close_back(wsid), None);
        assert_eq!(m.close_back(99), None);
    }

    #[test]
    fn close_back_activates_the_returned_workspace() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa"]));
        m.add_tab();
        m.reconcile(&ids(&["aa", "bb"])); // tab 1: "bb"
        let wsid = m.tear_off(1).unwrap();
        assert_eq!(m.active, 0);
        assert_eq!(m.close_back(wsid), Some(1));
        assert_eq!(m.active, 1);
        assert_eq!(m.focused.as_deref(), Some("bb"));
    }

    #[test]
    fn set_active_refuses_satellite_tabs() {
        let mut m = ChromeModel::default();
        m.add_tab();
        m.tear_off(1).unwrap();
        assert_eq!(m.active, 0);
        m.set_active(1); // a satellite: refused
        assert_eq!(m.active, 0);
    }

    #[test]
    fn wsid_binding_survives_tab_index_shifts() {
        let mut m = ChromeModel::default();
        m.add_tab(); // wsid 2
        m.add_tab(); // wsid 3
        let wsid = m.tear_off(2).unwrap();
        assert_eq!(wsid, 3);
        // Closing an EARLIER tab shifts indices; the wsid still resolves.
        m.close_tab(0).unwrap();
        assert_eq!(m.tab_by_wsid(wsid), Some(1));
        assert_eq!(m.satellite_tabs(), vec![(1, 3)]);
    }

    #[test]
    fn closing_a_tab_never_leaves_active_on_a_satellite() {
        let mut m = ChromeModel::default();
        m.add_tab(); // tab 1
        m.add_tab(); // tab 2, active
        m.tear_off(1).unwrap();
        assert_eq!(m.active, 2);
        // Closing the active main tab: active must skip over the satellite.
        m.close_tab(2).unwrap();
        assert_eq!(m.active, 0);
        assert!(!m.tabs[m.active].satellite);
    }

    #[test]
    fn reconcile_lands_new_sessions_in_a_main_workspace() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa"]));
        m.tear_off(0).unwrap(); // everything torn off
        let out = m.reconcile(&ids(&["aa", "bb"]));
        // A fresh MAIN workspace was created to receive "bb".
        assert_eq!(out.added, ids(&["bb"]));
        assert_eq!(m.tabs.len(), 2);
        assert!(!m.tabs[1].satellite);
        assert_eq!(m.tabs[1].tiles, ids(&["bb"]));
        assert_eq!(m.active, 1);
        // The satellite kept its own tiles.
        assert_eq!(m.tabs[0].tiles, ids(&["aa"]));

        // With a main workspace present, new sessions land there, never in the
        // satellite.
        let out = m.reconcile(&ids(&["aa", "bb", "cc"]));
        assert_eq!(out.added, ids(&["cc"]));
        assert_eq!(m.tabs[1].tiles, ids(&["bb", "cc"]));
    }

    #[test]
    fn dead_sessions_leave_satellite_workspaces_too() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa", "bb"]));
        m.add_tab();
        m.reconcile(&ids(&["aa", "bb", "cc"]));
        m.tear_off(0).unwrap();
        let out = m.reconcile(&ids(&["bb", "cc"])); // "aa" died in the satellite
        assert_eq!(out.removed, ids(&["aa"]));
        assert_eq!(m.tabs[0].tiles, ids(&["bb"]));
        assert!(m.tabs[0].satellite); // an emptied satellite stays a satellite
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

    // -- T10 x T12: satellites meet the apply surface -----------------------------

    #[test]
    fn pending_placement_routes_into_a_satellite_while_others_get_a_main_home() {
        // Everything torn off: an intent still routes into ITS tab (even torn
        // off - that window shows it), while the no-intent arrival gets a fresh
        // MAIN workspace (T10 fallback), not the satellite.
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa"]));
        m.note_pending_placement("/repo/wt", &m.tabs[0].id.clone());
        m.tear_off(0).unwrap();
        let out = m.reconcile_with_cwds(&[
            ("aa".into(), String::new()),
            ("s1".into(), "/repo/wt".into()),
            ("s2".into(), "/elsewhere".into()),
        ]);
        assert_eq!(out.added, ids(&["s1", "s2"]));
        assert_eq!(m.tabs[0].tiles, ids(&["aa", "s1"])); // intent honored
        assert!(m.tabs[0].satellite);
        assert_eq!(m.tabs.len(), 2);
        assert!(!m.tabs[1].satellite); // fresh main home for the rest
        assert_eq!(m.tabs[1].tiles, ids(&["s2"]));
    }

    #[test]
    fn place_tile_creates_a_main_home_when_everything_is_torn_off() {
        let mut m = ChromeModel::default();
        m.reconcile(&ids(&["aa"]));
        m.tear_off(0).unwrap();
        assert!(m.place_tile("bb"));
        assert_eq!(m.tabs.len(), 2);
        assert!(!m.tabs[1].satellite);
        assert_eq!(m.tabs[1].tiles, ids(&["bb"]));
        assert_eq!(m.focused.as_deref(), Some("bb"));
        // The satellite kept its own tiles.
        assert_eq!(m.tabs[0].tiles, ids(&["aa"]));
    }

    #[test]
    fn id_addressed_activation_refuses_satellite_tabs() {
        let mut m = ChromeModel::default();
        m.add_tab();
        let torn = m.tabs[1].id.clone();
        m.tear_off(1).unwrap();
        assert_eq!(m.active, 0);
        // focus_tab apply: refused - the main grid cannot show a satellite.
        assert!(!m.set_active_by_id(&torn));
        assert_eq!(m.active, 0);
        // new_tab apply on an existing torn-off id: adopted (no dup), not activated.
        assert!(!m.adopt_tab(&torn, "whatever"));
        assert_eq!(m.tabs.len(), 2);
        assert_eq!(m.active, 0);
    }
}
