//! The GPUI cockpit view (T8, integrated with T6/T7/T9): a LEFT SIDEBAR with
//! the workspace list on top and the T9 [`OverlaySidebar`] mounted below, plus
//! the active workspace's tile grid with real per-tile headers.
//!
//! Workspace navigation lives in the sidebar (the webview's long-standing
//! design - there is NO top tab strip): switch by clicking a row, create with
//! the "+ new workspace" row, rename by double- or right-clicking a row, close
//! with the row's `x`. The workspace section's height is what
//! [`crate::chrome::model::sidebar_layout`] says; everything below it is the
//! `overlay_mount`, realized here as a flex child hosting the T9 entity.
//!
//! This file is deliberately a THIN adapter: every decision (which workspace is
//! active, where tiles go, what a click hits, rename editing) lives in the
//! gpui-free [`crate::chrome::model`]; every terminal cell reaching the screen
//! goes through [`crate::render::sync_and_paint_content`]; and every terminal
//! input goes through the shared per-tile input core in `render` (T6's
//! selection / mouse reporting / find bar / URL opening), so the grid demo and
//! the cockpit behave identically inside a tile.
//!
//! ## Input routing
//! - Click: row-close > workspace row (double-click = rename) > `+` row >
//!   tile-close > scrolled-back badge > tile (focus + T6 dispatch).
//!   Right-click: workspace row = rename; tile = T6 right-button dispatch.
//! - Keys: rename mode captures the workspace name buffer; otherwise the T6
//!   `tile_key_input` core (find bar, copy/paste, scrollback keys, PTY bytes)
//!   drives the focused tile (always in the ACTIVE workspace, per the model).
//! - Wheel: the tile under the pointer (T6 semantics: reporting/alt-screen
//!   aware), with fractional accumulation for touchpads.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, size, App, Bounds, Context, Entity, FocusHandle, Focusable,
    Font, Hsla, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, TextRun, Window,
};
use parking_lot::Mutex;

use crate::chrome::model::{sidebar_layout, tile_boxes, ChromeModel, RectF, SIDEBAR_ROW_H};
use crate::chrome::persist::{self, Layout};
use crate::font::FontSpec;
use crate::overlays::model::SessionStatus;
use crate::overlays::{OverlayFeed, OverlaySidebar};
use crate::render::{
    chord_of, h, notify_focus, paint_tile_frame, record_frame, spawn_logger,
    sync_and_paint_content, tile_drag_motion, tile_hover_motion, tile_key_input,
    tile_mouse_down_dispatch, tile_report_release, tile_wheel_dispatch, DragMode, PaintMode, Tile,
};
use crate::render_support::cell_from_pixel;
use crate::term::Rgb;

// ---------------------------------------------------------------------------
// Chrome layout / palette constants
// ---------------------------------------------------------------------------

const PAD: f32 = 6.0;
const GAP: f32 = 6.0;
/// Width of the left sidebar (workspace list on top, T9 overlays below).
const SIDEBAR_W: f32 = 220.0;
/// The real cockpit header (title + id + geometry + close), replacing T5's
/// one-line debug label.
const HEADER_H: f32 = 20.0;
const BORDER: f32 = 1.0;
const TILE_PAD: f32 = 4.0;
/// Chrome UI text metrics (sidebar labels, headers). Fixed - independent of the
/// per-tile T7 FontSpec; only cell_w is probed from the real shaper.
const UI_FONT_SIZE: f32 = 13.0;
const UI_LINE_H: f32 = 16.0;

const WINDOW_BG: Rgb = Rgb { r: 5, g: 7, b: 10 };
const FG: Rgb = Rgb { r: 216, g: 222, b: 233 };
const FG_DIM: Rgb = Rgb { r: 128, g: 138, b: 154 };
const ACCENT: Rgb = Rgb { r: 128, g: 200, b: 255 };
/// Matches the T9 OverlaySidebar's background so the sidebar reads as one column.
const SIDEBAR_BG: Rgb = Rgb { r: 13, g: 17, b: 23 };
const ROW_BG: Rgb = Rgb { r: 18, g: 24, b: 31 };
const ROW_BG_ACTIVE: Rgb = Rgb { r: 33, g: 42, b: 54 };
/// Liveness cues: semantic status when the session is a known agent (from the
/// T9 SidebarState), output-recency otherwise.
const LIVE: Rgb = Rgb { r: 86, g: 211, b: 128 };
const NEEDS: Rgb = Rgb { r: 230, g: 180, b: 80 };
const IDLE_DOT: Rgb = Rgb { r: 70, g: 78, b: 90 };
const BUSY_WINDOW_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Chrome UI text metrics (not tile metrics - those are per-tile in `render`).
#[derive(Clone, Copy)]
struct UiMetrics {
    cell_w: f32,
}

/// An in-progress drag on a tile, tracked by session id (tiles can reflow
/// between paints as sessions come and go).
struct ChromeDrag {
    id: String,
    mode: DragMode,
    last_cell: (usize, usize),
}

/// Everything the cockpit paints from and the input handlers mutate. Shared
/// (`Arc<Mutex<..>>`) between the GPUI view and the background reconcile worker
/// in [`crate::app`]; like the grid's `GridState`, paint and input run on the
/// GPUI main thread, so the lock is never actually contended by them.
pub struct CockpitState {
    pub(crate) model: ChromeModel,
    /// The persistent attach pool: EVERY tile in EVERY workspace, keyed by
    /// session id. Workspace switches only change what is painted; nothing
    /// detaches.
    pub(crate) tiles: HashMap<String, Tile>,
    /// Session titles from `list_terminals` (refreshed by the worker).
    pub(crate) titles: HashMap<String, String>,
    /// Per-tile "last output" stamps (ms since `epoch`), written by the feeder
    /// threads - the recency fallback for the header's liveness cue.
    pub(crate) last_output_ms: HashMap<String, Arc<AtomicU64>>,
    /// Process epoch the stamps are measured from.
    pub(crate) epoch: Instant,
    hits: HitZones,
    ui: Option<UiMetrics>,
    ui_font: Font,
    ui_font_bold: Font,
    paint_mode: PaintMode,
    layout_path: PathBuf,
    drag: Option<ChromeDrag>,
    wheel_accum: f32,
    hover_cell: Option<(String, usize, usize)>,
}

impl CockpitState {
    pub fn new(model: ChromeModel, layout_path: PathBuf) -> Self {
        let ui_font = gpui::font(FontSpec::default().family);
        let mut ui_font_bold = ui_font.clone();
        ui_font_bold.weight = gpui::FontWeight::BOLD;
        CockpitState {
            model,
            tiles: HashMap::new(),
            titles: HashMap::new(),
            last_output_ms: HashMap::new(),
            epoch: Instant::now(),
            hits: HitZones::default(),
            ui: None,
            ui_font,
            ui_font_bold,
            paint_mode: PaintMode::from_env(),
            layout_path,
            drag: None,
            wheel_accum: 0.0,
            hover_cell: None,
        }
    }

    /// Persist the layout (best-effort: the cockpit must keep running even if
    /// the disk write fails; the next mutation retries).
    pub fn save_layout(&self) {
        if let Err(e) = persist::save(&self.layout_path, &Layout::from_model(&self.model)) {
            log::warn!("layout save failed: {e:#}");
        }
    }

    /// Drop a tile's pool entries (its `PtyHandle` detaches on drop).
    pub fn drop_tile(&mut self, id: &str) {
        self.tiles.remove(id);
        self.last_output_ms.remove(id);
        if self.drag.as_ref().is_some_and(|d| d.id == id) {
            self.drag = None;
        }
    }
}

/// Tab-aware toast suppression (T9): tell the feed which sessions the user is
/// looking at - the active workspace's tiles, as `th_*` tmux names.
pub fn sync_active_sessions(st: &CockpitState, feed: &OverlayFeed) {
    let active: HashSet<String> =
        st.model.active_tiles().iter().map(|id| format!("th_{id}")).collect();
    feed.set_active_sessions(active);
}

/// Hit zones refreshed by every paint, consumed by mouse handlers. `ws_*` are
/// the sidebar's workspace rows, in workspace order; tile entries carry both
/// the whole box and the terminal content box (for cell math).
#[derive(Default)]
struct HitZones {
    ws_rows: Vec<RectF>,
    ws_closes: Vec<RectF>,
    plus: RectF,
    tiles: Vec<(String, RectF, RectF)>,
    tile_closes: Vec<(String, RectF)>,
    badges: Vec<(String, RectF)>,
}

/// What a click landed on, resolved BEFORE mutating the model (the zones and
/// the model live in the same struct, so hit-testing borrows must end first).
enum HitTarget {
    WorkspaceClose(usize),
    Workspace(usize),
    Plus,
    TileClose(String),
    Badge(String),
    Tile(String),
}

fn hit_test(hits: &HitZones, x: f32, y: f32) -> Option<HitTarget> {
    for (i, r) in hits.ws_closes.iter().enumerate() {
        if r.contains(x, y) {
            return Some(HitTarget::WorkspaceClose(i));
        }
    }
    for (i, r) in hits.ws_rows.iter().enumerate() {
        if r.contains(x, y) {
            return Some(HitTarget::Workspace(i));
        }
    }
    if hits.plus.contains(x, y) {
        return Some(HitTarget::Plus);
    }
    for (id, r) in &hits.tile_closes {
        if r.contains(x, y) {
            return Some(HitTarget::TileClose(id.clone()));
        }
    }
    for (id, r) in &hits.badges {
        if r.contains(x, y) {
            return Some(HitTarget::Badge(id.clone()));
        }
    }
    for (id, r, _) in &hits.tiles {
        if r.contains(x, y) {
            return Some(HitTarget::Tile(id.clone()));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// The cockpit window content: the sidebar column (workspace list + the T9
/// overlays below it) and the active workspace's tile grid. Holds the shared
/// state, the overlay entity, the feed handle, focus, and the kept-alive
/// `ControlClient` (its PTY readers live for the process; the feed owns the
/// process's single event subscription).
pub struct CockpitView {
    state: Arc<Mutex<CockpitState>>,
    overlays: Entity<OverlaySidebar>,
    feed: OverlayFeed,
    focus: FocusHandle,
    _client: Arc<crate::wire::ControlClient>,
}

impl CockpitView {
    pub fn new(
        state: Arc<Mutex<CockpitState>>,
        overlays: Entity<OverlaySidebar>,
        feed: OverlayFeed,
        client: Arc<crate::wire::ControlClient>,
        focus: FocusHandle,
    ) -> Self {
        spawn_logger();
        CockpitView { state, overlays, feed, focus, _client: client }
    }
}

// ---------------------------------------------------------------------------
// Paint helpers
// ---------------------------------------------------------------------------

fn b(r: RectF) -> Bounds<Pixels> {
    Bounds::new(point(px(r.x), px(r.y)), size(px(r.w), px(r.h)))
}

/// Paint one line of styled UI text parts at (x, y). Monospace, so callers can
/// budget widths as `chars * cell_w`.
#[allow(clippy::too_many_arguments)]
fn paint_parts(
    parts: &[(String, Hsla, bool)],
    x: f32,
    y: f32,
    font_normal: &Font,
    font_bold: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    for (part, color, bold) in parts {
        if part.is_empty() {
            continue;
        }
        text.push_str(part);
        runs.push(TextRun {
            len: part.len(),
            font: if *bold { font_bold.clone() } else { font_normal.clone() },
            color: *color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    if text.is_empty() {
        return;
    }
    let shaped =
        window.text_system().shape_line(SharedString::from(text), px(UI_FONT_SIZE), &runs, None);
    shaped.paint(point(px(x), px(y)), px(UI_LINE_H), window, cx).ok();
}

/// Truncate to `max` display cells with a trailing ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Probe the UI font's cell advance once (shape a run of `M`s and divide).
fn ensure_ui_metrics(st: &mut CockpitState, window: &mut Window) {
    if st.ui.is_some() {
        return;
    }
    let probe: SharedString = "M".repeat(40).into();
    let run = TextRun {
        len: probe.len(),
        font: st.ui_font.clone(),
        color: h(FG),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(probe, px(UI_FONT_SIZE), &[run], None);
    let w: f32 = line.width.into();
    st.ui = Some(UiMetrics { cell_w: (w / 40.0).max(1.0) });
}

/// The pixel height of the sidebar's workspace section (caption + rows + the
/// `+` row + the separator), used to size the canvas above the T9 mount.
fn workspace_section_height(n_tabs: usize) -> f32 {
    PAD + UI_LINE_H + 8.0 + (n_tabs as f32 + 1.0) * (SIDEBAR_ROW_H + 2.0) + PAD
}

// ---------------------------------------------------------------------------
// Paint: the sidebar's workspace section
// ---------------------------------------------------------------------------

fn paint_workspace_section(
    state: &Arc<Mutex<CockpitState>>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut st = state.lock();
    ensure_ui_metrics(&mut st, window);
    let ui = st.ui.expect("ui metrics set");
    let font_normal = st.ui_font.clone();
    let font_bold = st.ui_font_bold.clone();

    let bx: f32 = bounds.origin.x.into();
    let by: f32 = bounds.origin.y.into();
    let bw: f32 = bounds.size.width.into();
    let bh: f32 = bounds.size.height.into();

    window.paint_quad(fill(bounds, h(SIDEBAR_BG)));
    paint_parts(
        &[("WORKSPACES".to_string(), h(FG_DIM), false)],
        bx + PAD + 4.0,
        by + PAD + 2.0,
        &font_normal,
        &font_bold,
        window,
        cx,
    );

    let labels: Vec<String> = st
        .model
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| match &st.model.renaming {
            Some(r) if r.tab == i => format!("{}_", r.buffer),
            _ => t.name.clone(),
        })
        .collect();
    let ws_area = RectF::new(
        bx + PAD,
        by + PAD + UI_LINE_H + 8.0,
        bw - 2.0 * PAD,
        bh - (PAD + UI_LINE_H + 8.0),
    );
    let sb = sidebar_layout(labels.len(), ws_area);
    let row_dy = (SIDEBAR_ROW_H - UI_LINE_H) / 2.0;

    for (i, row) in sb.rows.iter().enumerate() {
        let active = i == st.model.active;
        window.paint_quad(fill(b(*row), h(if active { ROW_BG_ACTIVE } else { ROW_BG })));
        if active {
            // Accent edge bar marks the active workspace.
            window.paint_quad(fill(b(RectF::new(row.x, row.y, 3.0, row.h)), h(ACCENT)));
        }
        let renaming_this = matches!(&st.model.renaming, Some(r) if r.tab == i);
        let label_color = if renaming_this {
            h(ACCENT)
        } else if active {
            h(FG)
        } else {
            h(FG_DIM)
        };
        let label_cells =
            (((row.w - 14.0 - sb.closes[i].w) / ui.cell_w).floor() as usize).max(4);
        paint_parts(
            &[(truncate(&labels[i], label_cells), label_color, active)],
            row.x + 10.0,
            row.y + row_dy,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
        paint_parts(
            &[("×".to_string(), h(FG_DIM), false)],
            sb.closes[i].x + (sb.closes[i].w - ui.cell_w) / 2.0,
            row.y + row_dy,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
    }
    paint_parts(
        &[("+ new workspace".to_string(), h(FG_DIM), false)],
        sb.plus.x + 10.0,
        sb.plus.y + row_dy,
        &font_normal,
        &font_bold,
        window,
        cx,
    );
    // A hairline closes the workspace section; everything below this canvas is
    // the T9 overlay mount (`sb.overlay_mount`), hosted as a flex child.
    window.paint_quad(fill(
        b(RectF::new(sb.overlay_mount.x, sb.overlay_mount.y, sb.overlay_mount.w, 1.0)),
        h(ROW_BG_ACTIVE),
    ));

    st.hits.ws_rows = sb.rows;
    st.hits.ws_closes = sb.closes;
    st.hits.plus = sb.plus;
}

// ---------------------------------------------------------------------------
// Paint: the tile grid
// ---------------------------------------------------------------------------

/// Per-tile semantic status (T9 SidebarState) for the header dots: tile id
/// (`th_`-stripped) -> status. Gathered BEFORE the cockpit lock is taken so the
/// two locks never nest.
fn gather_statuses(feed: &OverlayFeed) -> HashMap<String, SessionStatus> {
    let state = feed.state();
    let st = state.lock();
    let mut map = HashMap::new();
    for (uuid, tmux) in st.index.tmux_aliases() {
        let tile_id = tmux.strip_prefix("th_").unwrap_or(tmux);
        map.insert(tile_id.to_string(), st.supervision.status_of(uuid));
    }
    map
}

fn paint_grid_area(
    state: &Arc<Mutex<CockpitState>>,
    statuses: &HashMap<String, SessionStatus>,
    focused_window: bool,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let t0 = Instant::now();
    let mut st = state.lock();
    ensure_ui_metrics(&mut st, window);
    let ui = st.ui.expect("ui metrics set");
    let font_normal = st.ui_font.clone();
    let font_bold = st.ui_font_bold.clone();
    let mode = st.paint_mode;

    window.paint_quad(fill(bounds, h(WINDOW_BG)));

    let bx: f32 = bounds.origin.x.into();
    let by: f32 = bounds.origin.y.into();
    let bw: f32 = bounds.size.width.into();
    let bh: f32 = bounds.size.height.into();
    let area = RectF::new(bx + PAD, by + PAD, bw - 2.0 * PAD, bh - 2.0 * PAD);

    // Split-borrow the state so the tiles map can be mutated while the model,
    // titles, and stamps are read (all disjoint fields of one MutexGuard).
    let CockpitState { model, tiles, titles, last_output_ms, epoch, hits, .. } = &mut *st;
    let now_ms = epoch.elapsed().as_millis() as u64;
    let ids: Vec<String> = model.active_tiles().to_vec();
    let focused_id = model.focused.clone();

    let mut tile_hits = Vec::with_capacity(ids.len());
    let mut close_hits = Vec::with_capacity(ids.len());
    let mut badge_hits = Vec::new();
    let mut rebuilt_frame: u64 = 0;
    let mut total_frame: u64 = 0;

    if ids.is_empty() {
        paint_parts(
            &[(
                "This workspace is empty - new sessions land in the active workspace."
                    .to_string(),
                h(FG_DIM),
                false,
            )],
            area.x + 8.0,
            area.y + 8.0,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
    }

    for (id, bx) in ids.iter().zip(tile_boxes(ids.len(), area, GAP)) {
        let is_focused = focused_window && focused_id.as_deref() == Some(id.as_str());
        paint_tile_frame(b(bx), is_focused, window);

        // Close zone in the header's right corner.
        let close_w = ui.cell_w + 10.0;
        let close = RectF::new(bx.x + bx.w - BORDER - close_w, bx.y + BORDER, close_w, HEADER_H);

        // Content box: inside the border/padding, below the cockpit header.
        let content = RectF::new(
            bx.x + BORDER + TILE_PAD,
            bx.y + BORDER + HEADER_H,
            (bx.w - 2.0 * (BORDER + TILE_PAD)).max(1.0),
            (bx.h - HEADER_H - 2.0 * BORDER - TILE_PAD).max(1.0),
        );

        let mut geom = String::new();
        if let Some(tile) = tiles.get_mut(id) {
            let paint =
                sync_and_paint_content(tile, b(content), mode, is_focused, window, cx);
            rebuilt_frame += paint.rebuilt;
            total_frame += paint.total;
            if let Some(badge) = paint.badge {
                badge_hits.push((
                    id.clone(),
                    RectF::new(
                        badge.origin.x.into(),
                        badge.origin.y.into(),
                        badge.size.width.into(),
                        badge.size.height.into(),
                    ),
                ));
            }
            geom = format!("{}x{}", tile.cols, tile.rows);
        } else {
            paint_parts(
                &[("attaching…".to_string(), h(FG_DIM), false)],
                content.x,
                content.y,
                &font_normal,
                &font_bold,
                window,
                cx,
            );
        }

        // --- the cockpit header: liveness dot, title, id, geometry, close ----
        // Dot: semantic status when the T9 SidebarState knows this session as
        // an agent; otherwise output-recency (busy within 2s).
        let dot = match statuses.get(id) {
            Some(SessionStatus::Working) | Some(SessionStatus::WaitingOnSubagents) => h(LIVE),
            Some(SessionStatus::NeedsQuestion) | Some(SessionStatus::NeedsPermission) => {
                h(NEEDS)
            }
            _ => {
                let busy = last_output_ms
                    .get(id)
                    .map(|s| now_ms.saturating_sub(s.load(Ordering::Relaxed)) < BUSY_WINDOW_MS)
                    .unwrap_or(false);
                if busy {
                    h(LIVE)
                } else {
                    h(IDLE_DOT)
                }
            }
        };
        let title = titles.get(id).filter(|t| !t.is_empty()).cloned().unwrap_or_else(|| id.clone());

        let avail = (((bx.w - 2.0 * (BORDER + TILE_PAD) - close_w) / ui.cell_w).floor()
            as usize)
            .saturating_sub(2); // the dot
        let meta = if title == *id { format!("  {geom}") } else { format!("  {id}  {geom}") };
        let (title_text, meta_text) = if avail > meta.chars().count() + 8 {
            (truncate(&title, avail - meta.chars().count()), meta)
        } else {
            (truncate(&title, avail), String::new())
        };
        let header_y = bx.y + BORDER + (HEADER_H - UI_LINE_H) / 2.0 + 1.0;
        paint_parts(
            &[
                ("● ".to_string(), dot, false),
                (title_text, if is_focused { h(ACCENT) } else { h(FG) }, true),
                (meta_text, h(FG_DIM), false),
            ],
            bx.x + BORDER + TILE_PAD,
            header_y,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
        paint_parts(
            &[("×".to_string(), h(FG_DIM), false)],
            close.x + (close.w - ui.cell_w) / 2.0,
            header_y,
            &font_normal,
            &font_bold,
            window,
            cx,
        );

        tile_hits.push((id.clone(), bx, content));
        close_hits.push((id.clone(), close));
    }

    hits.tiles = tile_hits;
    hits.tile_closes = close_hits;
    hits.badges = badge_hits;
    drop(st);

    record_frame(t0.elapsed().as_nanos() as u64, rebuilt_frame, total_frame);
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Resolve a window position to a cell of tile `id`, via the content rect the
/// last paint recorded. Returns the cell plus whether the tile was found.
fn chrome_cell(
    st: &CockpitState,
    id: &str,
    pos: gpui::Point<Pixels>,
) -> Option<crate::render_support::CellHit> {
    let (_, _, content) = st.hits.tiles.iter().find(|(tid, _, _)| tid == id)?;
    let tile = st.tiles.get(id)?;
    let m = tile.metrics?;
    let rel_x = f32::from(pos.x) - content.x;
    let rel_y = f32::from(pos.y) - content.y;
    Some(cell_from_pixel(rel_x, rel_y, m.cell_w, m.line_h, tile.cols, tile.rows))
}

impl CockpitView {
    fn mouse_down(
        &mut self,
        button: MouseButton,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x: f32 = ev.position.x.into();
        let y: f32 = ev.position.y.into();
        let mut st = self.state.lock();
        let target = hit_test(&st.hits, x, y);
        match target {
            Some(HitTarget::WorkspaceClose(i)) if button == MouseButton::Left => {
                if let Some(removed) = st.model.close_tab(i) {
                    for id in &removed {
                        st.drop_tile(id);
                    }
                    st.save_layout();
                    sync_active_sessions(&st, &self.feed);
                }
            }
            Some(HitTarget::Workspace(i)) => {
                if button == MouseButton::Right || ev.click_count >= 2 {
                    st.model.begin_rename(i);
                } else if button == MouseButton::Left {
                    st.model.commit_rename();
                    st.model.set_active(i);
                    st.save_layout();
                    sync_active_sessions(&st, &self.feed);
                }
            }
            Some(HitTarget::Plus) if button == MouseButton::Left => {
                st.model.add_tab();
                st.save_layout();
                sync_active_sessions(&st, &self.feed);
            }
            Some(HitTarget::TileClose(id)) if button == MouseButton::Left => {
                if st.model.close_tile(&id) {
                    st.drop_tile(&id);
                    st.save_layout();
                    sync_active_sessions(&st, &self.feed);
                }
            }
            Some(HitTarget::Badge(id)) if button == MouseButton::Left => {
                if let Some(tile) = st.tiles.get(&id) {
                    tile.term.lock().scroll_to_bottom();
                }
            }
            Some(HitTarget::Tile(id)) => {
                st.model.commit_rename();
                // Focus follows click; tell terminals that track focus (1004).
                if st.model.focused.as_deref() != Some(id.as_str()) {
                    if let Some(old) = st.model.focused.clone() {
                        if let Some(t) = st.tiles.get(&old) {
                            notify_focus(t, false);
                        }
                    }
                    if let Some(t) = st.tiles.get(&id) {
                        notify_focus(t, true);
                    }
                    st.model.set_focused(&id);
                }
                // T6 dispatch: URL open, mouse reporting, selection, paste.
                if let Some(cell) = chrome_cell(&st, &id, ev.position) {
                    if let Some(tile) = st.tiles.get(&id) {
                        if let Some(mode) = tile_mouse_down_dispatch(
                            tile,
                            cell,
                            button,
                            ev.click_count,
                            ev.modifiers.shift,
                            ev.modifiers.alt,
                            ev.modifiers.control,
                            cx,
                        ) {
                            st.drag =
                                Some(ChromeDrag { id, mode, last_cell: (cell.col, cell.row) });
                        }
                    }
                }
            }
            _ => {
                st.model.commit_rename();
            }
        }
        drop(st);
        window.focus(&self.focus);
        cx.notify();
    }

    fn mouse_up(
        &mut self,
        button: MouseButton,
        ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut st = self.state.lock();
        let Some(drag) = st.drag.take() else { return };
        let ends_drag = match drag.mode {
            DragMode::Select => button == MouseButton::Left,
            DragMode::Report(btn) => {
                crate::render::report_button(button) == Some(btn)
            }
        };
        if !ends_drag {
            st.drag = Some(drag);
            return;
        }
        if let DragMode::Report(btn) = drag.mode {
            if let Some(cell) = chrome_cell(&st, &drag.id, ev.position) {
                if let Some(tile) = st.tiles.get(&drag.id) {
                    tile_report_release(tile, btn, cell, ev.modifiers.alt, ev.modifiers.control);
                }
            }
        }
        drop(st);
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let mut st = self.state.lock();

        if let Some(drag) = &st.drag {
            let id = drag.id.clone();
            let mode = drag.mode;
            let last = drag.last_cell;
            let Some(cell) = chrome_cell(&st, &id, ev.position) else { return };
            if (cell.col, cell.row) == last {
                return;
            }
            if let Some(tile) = st.tiles.get(&id) {
                tile_drag_motion(tile, mode, cell, ev.modifiers.alt, ev.modifiers.control);
            }
            if mode == DragMode::Select {
                cx.notify();
            }
            if let Some(d) = st.drag.as_mut() {
                d.last_cell = (cell.col, cell.row);
            }
            return;
        }

        // Buttonless hover motion (mode 1003) on the tile under the pointer.
        if ev.modifiers.shift {
            return;
        }
        let x: f32 = ev.position.x.into();
        let y: f32 = ev.position.y.into();
        let Some(id) = st
            .hits
            .tiles
            .iter()
            .find(|(_, r, _)| r.contains(x, y))
            .map(|(id, _, _)| id.clone())
        else {
            st.hover_cell = None;
            return;
        };
        let Some(cell) = chrome_cell(&st, &id, ev.position) else { return };
        if !cell.inside {
            st.hover_cell = None;
            return;
        }
        if st.hover_cell.as_ref() == Some(&(id.clone(), cell.col, cell.row)) {
            return;
        }
        st.hover_cell = Some((id.clone(), cell.col, cell.row));
        if let Some(tile) = st.tiles.get(&id) {
            tile_hover_motion(tile, cell, ev.modifiers.alt, ev.modifiers.control);
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let mut st = self.state.lock();

        // Rename mode captures the keyboard until commit/cancel.
        if st.model.renaming.is_some() {
            match ks.key.as_str() {
                "enter" => {
                    st.model.commit_rename();
                    st.save_layout();
                }
                "escape" => st.model.cancel_rename(),
                "backspace" => st.model.rename_backspace(),
                _ => {
                    if !ks.modifiers.control && !ks.modifiers.platform {
                        if let Some(kc) = ks.key_char.as_deref() {
                            if !kc.is_empty() && !kc.chars().any(char::is_control) {
                                st.model.rename_push(kc);
                            }
                        }
                    }
                }
            }
            cx.notify();
            return;
        }

        // Terminal input through the shared T6 core (find bar, copy/paste,
        // scrollback keys, PTY encoding) on the focused tile.
        let chord = chord_of(ks);
        if let Some(id) = st.model.focused.clone() {
            if let Some(tile) = st.tiles.get_mut(&id) {
                tile_key_input(tile, &chord, cx);
            }
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let mut st = self.state.lock();
        // The wheel acts on the tile under the pointer (fallback: the focused one).
        let x: f32 = ev.position.x.into();
        let y: f32 = ev.position.y.into();
        let id = st
            .hits
            .tiles
            .iter()
            .find(|(_, r, _)| r.contains(x, y))
            .map(|(id, _, _)| id.clone())
            .or_else(|| st.model.focused.clone());
        let Some(id) = id else { return };
        let Some(line_h) = st.tiles.get(&id).and_then(|t| t.metrics).map(|m| m.line_h) else {
            return;
        };
        let dy = match ev.delta {
            // One wheel notch = 3 rows; pixel deltas (touchpads) map 1:1 by row
            // height, with the fraction carried across events.
            ScrollDelta::Lines(p) => p.y * 3.0,
            ScrollDelta::Pixels(p) => f32::from(p.y) / line_h,
        };
        st.wheel_accum += dy;
        let lines = st.wheel_accum as i32;
        if lines == 0 {
            return;
        }
        st.wheel_accum -= lines as f32;

        let Some(cell) = chrome_cell(&st, &id, ev.position) else { return };
        if let Some(tile) = st.tiles.get(&id) {
            tile_wheel_dispatch(
                tile,
                lines,
                cell,
                ev.modifiers.shift,
                ev.modifiers.alt,
                ev.modifiers.control,
            );
        }
        cx.notify();
    }
}

impl Focusable for CockpitView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CockpitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Continuous repaint like the grid; damage clipping bounds the work.
        window.request_animation_frame();

        let ws_h = workspace_section_height(self.state.lock().model.tabs.len());
        let statuses = gather_statuses(&self.feed);
        let focused_window = self.focus.is_focused(window);

        let ws_state = self.state.clone();
        let grid_state = self.state.clone();

        div()
            .size_full()
            .flex()
            .flex_row()
            .track_focus(&self.focus)
            .bg(h(WINDOW_BG))
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, ev, w, cx| v.mouse_down(MouseButton::Left, ev, w, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|v, ev, w, cx| v.mouse_down(MouseButton::Middle, ev, w, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|v, ev, w, cx| v.mouse_down(MouseButton::Right, ev, w, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, ev, w, cx| v.mouse_up(MouseButton::Left, ev, w, cx)),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|v, ev, w, cx| v.mouse_up(MouseButton::Middle, ev, w, cx)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|v, ev, w, cx| v.mouse_up(MouseButton::Right, ev, w, cx)),
            )
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(
                // The sidebar column: workspace section (canvas) on top, the T9
                // overlay sections below (the model's `overlay_mount`).
                div()
                    .w(px(SIDEBAR_W))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(h(SIDEBAR_BG))
                    .child(
                        div().w_full().h(px(ws_h)).child(
                            canvas(
                                |_, _, _| (),
                                move |bounds, _, window, cx| {
                                    paint_workspace_section(&ws_state, bounds, window, cx);
                                },
                            )
                            .size_full(),
                        ),
                    )
                    .child(div().flex_1().min_h(px(0.)).child(self.overlays.clone())),
            )
            .child(
                div().flex_1().h_full().child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, cx| {
                            paint_grid_area(
                                &grid_state,
                                &statuses,
                                focused_window,
                                bounds,
                                window,
                                cx,
                            );
                        },
                    )
                    .size_full(),
                ),
            )
    }
}
