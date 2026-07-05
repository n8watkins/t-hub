//! The GPUI cockpit view (T8, integrated with T6/T7/T9; multi-window in T10):
//! a LEFT SIDEBAR with the workspace list on top and the T9 [`OverlaySidebar`]
//! mounted below, plus the active workspace's tile grid with real per-tile
//! headers - and, per torn-off workspace, a satellite OS window hosting that
//! workspace's grid ([`SatelliteView`]).
//!
//! Workspace navigation lives in the sidebar (the webview's long-standing
//! design - there is NO top tab strip): switch by clicking a row, create with
//! the "+ new workspace" row, rename by double- or right-clicking a row, close
//! with the row's `x`, tear off into a satellite window (or bring one back)
//! with the row's `»`/`«`. The workspace section's height is what
//! [`crate::chrome::model::sidebar_layout`] says; everything below it is the
//! `overlay_mount`, realized here as a flex child hosting the T9 entity.
//!
//! This file is deliberately a THIN adapter: every decision (which workspace is
//! active, where tiles go, what a click hits, rename editing, satellite
//! invariants) lives in the gpui-free [`crate::chrome::model`] and
//! [`crate::chrome::windows`]; every terminal cell reaching the screen
//! goes through [`crate::render::sync_and_paint_content`]; and every terminal
//! input goes through the shared per-tile input core in `render` (T6's
//! selection / mouse reporting / find bar / URL opening), so the grid demo,
//! the cockpit, and every satellite behave identically inside a tile.
//!
//! ## Multi-window model (T10)
//! Every OS window paints from the SAME shared [`CockpitState`] (one model, one
//! attach pool - a tile lives in exactly one window, so no tile is ever painted
//! or damage-drained twice) but keeps its own transient input state and hit
//! zones, keyed by [`WinKey`]: two windows repaint on independent schedules, so
//! zones written by one window's paint must never be hit-tested by another
//! window's click. gpui gives each window its own renderer + sprite atlas
//! (per-window GPU surface); the process-wide cost is watched by
//! [`crate::render_support::proc_stats`] logging.
//!
//! ## Input routing
//! - Click: row-close > workspace row (double-click = rename) > tear-off zone >
//!   `+` row > tile-close > scrolled-back badge > tile (focus + T6 dispatch).
//!   Right-click: workspace row = rename; tile = T6 right-button dispatch.
//! - Keys: rename mode captures the workspace name buffer; otherwise the T6
//!   `tile_key_input` core (find bar, copy/paste, scrollback keys, PTY bytes)
//!   drives the window's focused tile (main: the model's `focused`, always in
//!   the ACTIVE workspace; satellite: the registry's per-window focus).
//! - Wheel: the tile under the pointer (T6 semantics: reporting/alt-screen
//!   aware), with fractional accumulation for touchpads.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, size, App, Bounds, Context, CursorStyle, Entity, FocusHandle,
    Focusable, Font, Hsla, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, TextRun,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions,
};
use parking_lot::Mutex;

use crate::chrome::actions::{dispatch_host, Effect, Region};
use crate::chrome::cues::{age_label, format_age, LifeTracker, TileLife, TilePulse};
use crate::chrome::keymap::{Handled, KeyController};
use crate::chrome::model::{
    apply_divider_split, divider_extent, divider_zones, sidebar_layout, split_rows,
    tile_boxes_ratio, ChromeModel, DividerId, GridRatios, RectF, SIDEBAR_ROW_H,
};
use crate::chrome::palette_view;
use crate::chrome::persist::{self, Layout};
use crate::chrome::windows::{SatBounds, WindowRegistry};
use crate::font::FontSpec;
use crate::overlays::model::SessionStatus;
use crate::overlays::{OverlayFeed, OverlaySidebar};
use crate::render::{
    chord_of, h, ha, notify_focus, paint_tile_frame, record_frame, spawn_logger,
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
/// The panels side surface width (N5): the `panel-window` bin's proven width.
const PANELS_W: f32 = 420.0;
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
/// Dead/exited cue (T24): the badge and dot of a tile whose session is gone.
const DEAD: Rgb = Rgb { r: 224, g: 96, b: 96 };
/// Header git-dirty dot (webview #eab308).
const DIRTY: Rgb = Rgb { r: 234, g: 179, b: 8 };
/// Header client glyph: Claude's clay spark (webview #D97757).
const CLAUDE: Rgb = Rgb { r: 217, g: 119, b: 87 };
/// Header client glyph: Codex blue.
const CODEX: Rgb = Rgb { r: 96, g: 165, b: 250 };
/// Context meter ramp (webview ContextMeter: emerald < 75, amber < 90, red).
const METER_OK: Rgb = Rgb { r: 52, g: 211, b: 153 };
const METER_WARN: Rgb = Rgb { r: 251, g: 191, b: 36 };
const METER_HOT: Rgb = Rgb { r: 248, g: 113, b: 113 };
/// Context meter bar width in px (the webview's 8-char track, scaled down).
const METER_W: f32 = 36.0;
const BUSY_WINDOW_MS: u64 = 2_000;
/// A defunct tile's content dims under this overlay alpha (the badge says why).
const DEFUNCT_DIM: f32 = 0.55;
/// The minimum pane extent a divider drag can shrink to (T26): enough for the
/// header plus a couple of terminal rows to stay legible.
const MIN_TILE_PX: f32 = 80.0;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// One tab-layout snapshot bound for the server's registry mirror (T12):
/// `(id, name, tileIds)` per workspace, in order. Plain data so the reporter
/// thread owns it without touching the model lock.
pub type TabsSnapshot = Vec<(String, String, Vec<String>)>;

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

/// An in-progress header drag-reorder (N2): armed by a left press on a tile's
/// header, it becomes a real drag once the pointer moves past a small
/// threshold (so plain header clicks stay focus-only), tracking the tile under
/// the pointer as the drop target.
struct TileDrag {
    id: String,
    start: (f32, f32),
    dragging: bool,
    /// The drop target under the pointer (never the dragged tile itself).
    target: Option<String>,
}

/// Pointer travel (px) before a header press becomes a reorder drag.
const DRAG_THRESHOLD_PX: f32 = 4.0;

/// Which OS window a paint or an input event belongs to. Every per-window bit
/// of [`CockpitState`] is keyed by this: windows repaint on independent
/// schedules, so state written by one window's paint (hit zones) or input
/// lifecycle (drags) must never leak into another's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WinKey {
    Main,
    /// A satellite window, by its workspace's stable wsid.
    Sat(u64),
}

/// A pending session-kill confirmation (N4): the tile and whether its agent
/// looked mid-turn when the kill was requested (the dialog says so).
pub(crate) struct ConfirmKill {
    pub id: String,
    pub busy: bool,
}

/// Per-tile header data resolved from the T9 sidebar state before the cockpit
/// lock is taken: semantic status plus the Claude context-window fill (N1).
#[derive(Clone, Copy, Default)]
pub struct TileMeta {
    pub status: Option<SessionStatus>,
    pub context_pct: Option<f32>,
}

/// Per-window tile hit zones, refreshed by that window's every paint. Tile
/// entries carry both the whole box and the terminal content box (cell math).
#[derive(Default)]
struct TileZones {
    tiles: Vec<(String, RectF, RectF)>,
    closes: Vec<(String, RectF)>,
    /// Fullscreen toggle zones in the headers (N3), left of the close `x`.
    fulls: Vec<(String, RectF)>,
    badges: Vec<(String, RectF)>,
    /// The header's editable work-name region (N1), per tile.
    names: Vec<(String, RectF)>,
    /// Divider hit zones between tiles (T26), matching this paint's boxes.
    dividers: Vec<(DividerId, RectF)>,
    /// The grid area those boxes were laid out in (divider drag math).
    area: RectF,
}

/// Per-window transient input state (a drag or a hover belongs to the window
/// the pointer is in).
#[derive(Default)]
struct WinInput {
    drag: Option<ChromeDrag>,
    /// An in-progress divider drag (T26).
    div_drag: Option<DividerId>,
    /// An in-progress header drag-reorder (N2).
    tile_drag: Option<TileDrag>,
    wheel_accum: f32,
    hover_cell: Option<(String, usize, usize)>,
    /// Last pointer position in this window (divider hover cue + cursor).
    last_pos: Option<(f32, f32)>,
}

/// Everything the cockpit paints from and the input handlers mutate. Shared
/// (`Arc<Mutex<..>>`) between ALL the GPUI views (main window + satellites) and
/// the background reconcile worker in [`crate::app`]; paint and input run on
/// the GPUI main thread, so the lock is never actually contended by them.
pub struct CockpitState {
    pub(crate) model: ChromeModel,
    /// The satellite-window registry (T10): per-window focus + bounds.
    pub(crate) sats: WindowRegistry,
    /// The persistent attach pool: EVERY tile in EVERY workspace, keyed by
    /// session id. Workspace switches only change what is painted; nothing
    /// detaches - and satellites paint from this same pool (never a second
    /// attach for a torn-off tile).
    pub(crate) tiles: HashMap<String, Tile>,
    /// Session titles from `list_terminals` (refreshed by the worker).
    pub(crate) titles: HashMap<String, String>,
    /// Session cwds from `list_terminals` (refreshed by the worker). T12 uses
    /// them for the worktree-removal detach match.
    pub(crate) cwds: HashMap<String, String>,
    /// Tab-layout snapshots for the server's registry mirror (T12):
    /// [`save_layout`](Self::save_layout) snapshots the tabs here and the
    /// background reporter sends the deduped `report_workspace_tabs` command.
    /// `None` outside the cockpit (grid mode / tests).
    report_tx: Option<crossbeam::channel::Sender<TabsSnapshot>>,
    /// Per-tile output/exit pulses (stamps in ms since `epoch`), written by
    /// the feeder threads - the header's output-age cue (T24) plus the exit
    /// code once a PTY exit frame lands.
    pub(crate) pulses: HashMap<String, Arc<TilePulse>>,
    /// Per-tile life states (T24): live / reconnecting / exited / dead,
    /// observed by the cockpit worker each pass, read by every paint.
    pub(crate) lives: LifeTracker,
    /// Git awareness per session cwd (N1): branch / linked-worktree / dirty
    /// count, polled over the socket `git_info` by the cockpit worker.
    pub(crate) git: HashMap<String, crate::panels::files::GitInfo>,
    /// A pending kill confirmation (N4): set by the KillSession command, shown
    /// as a modal overlay until confirmed (kill) or dismissed.
    pub(crate) confirm_kill: Option<ConfirmKill>,
    /// Process epoch the stamps are measured from.
    pub(crate) epoch: Instant,
    /// Main-window sidebar hit zones (satellites have no sidebar).
    hits: HitZones,
    /// Per-window tile hit zones and transient input state.
    tilezones: HashMap<WinKey, TileZones>,
    input: HashMap<WinKey, WinInput>,
    /// Per-window painted-cell counts (cols x rows summed over painted tiles),
    /// for the T10 windows-x-cells memory watch.
    pub(crate) visible_cells: HashMap<WinKey, u64>,
    /// The T-A keymap stack: bindings + armed prefix + palette + focus region.
    /// Main-window only (the webview keymap lived in its one window; satellite
    /// keys stay raw terminal input).
    pub(crate) keys: KeyController,
    /// Whether the panels side surface (N5: Files / Preview / Dev runner) is
    /// shown beside the grid. View state like the palette - transient, not
    /// persisted, main window only.
    pub(crate) panels_open: bool,
    ui: Option<UiMetrics>,
    ui_font: Font,
    ui_font_bold: Font,
    paint_mode: PaintMode,
    layout_path: PathBuf,
}

impl CockpitState {
    pub fn new(model: ChromeModel, layout_path: PathBuf) -> Self {
        let ui_font = gpui::font(FontSpec::default().family);
        let mut ui_font_bold = ui_font.clone();
        ui_font_bold.weight = gpui::FontWeight::BOLD;
        CockpitState {
            model,
            sats: WindowRegistry::default(),
            tiles: HashMap::new(),
            titles: HashMap::new(),
            cwds: HashMap::new(),
            report_tx: None,
            pulses: HashMap::new(),
            lives: LifeTracker::default(),
            git: HashMap::new(),
            confirm_kill: None,
            epoch: Instant::now(),
            hits: HitZones::default(),
            tilezones: HashMap::new(),
            input: HashMap::new(),
            visible_cells: HashMap::new(),
            keys: KeyController::load_default(),
            panels_open: false,
            ui: None,
            ui_font,
            ui_font_bold,
            paint_mode: PaintMode::from_env(),
            layout_path,
        }
    }

    /// Wire the T12 tab reporter (once, from the cockpit boot).
    pub fn set_report_tx(&mut self, tx: crossbeam::channel::Sender<TabsSnapshot>) {
        self.report_tx = Some(tx);
    }

    /// Snapshot the tab layout to the registry reporter (T12: the write half of
    /// the `list_tabs` mirror, exactly what the webview's `startTabReporter`
    /// does on every tabs change). No-op until [`set_report_tx`](Self::set_report_tx).
    pub fn report_tabs(&self) {
        if let Some(tx) = &self.report_tx {
            let snap: TabsSnapshot = self
                .model
                .tabs
                .iter()
                .map(|t| (t.id.clone(), t.name.clone(), t.tiles.clone()))
                .collect();
            let _ = tx.send(snap);
        }
    }

    /// Persist the layout (best-effort: the cockpit must keep running even if
    /// the disk write fails; the next mutation retries). Every persisted layout
    /// change also re-reports the tabs to the server's registry mirror (T12).
    pub fn save_layout(&self) {
        if let Err(e) =
            persist::save(&self.layout_path, &Layout::from_state(&self.model, &self.sats))
        {
            log::warn!("layout save failed: {e:#}");
        }
        self.report_tabs();
    }

    /// Drop a tile's pool entries (its `PtyHandle` detaches on drop) and every
    /// per-window reference to it (drags, hovers, satellite focus).
    pub fn drop_tile(&mut self, id: &str) {
        self.tiles.remove(id);
        self.pulses.remove(id);
        self.lives.remove(id);
        self.cwds.remove(id);
        for input in self.input.values_mut() {
            if input.drag.as_ref().is_some_and(|d| d.id == id) {
                input.drag = None;
            }
            if input.tile_drag.as_ref().is_some_and(|d| d.id == id) {
                input.tile_drag = None;
            }
            if let Some(d) = input.tile_drag.as_mut() {
                if d.target.as_deref() == Some(id) {
                    d.target = None;
                }
            }
            if input.hover_cell.as_ref().is_some_and(|(hid, _, _)| hid == id) {
                input.hover_cell = None;
            }
        }
        self.sats.drop_tile(id);
    }

    /// Drop a closed satellite window's per-window state (registry entry stays
    /// the caller's business - the close paths own that ordering).
    fn drop_window_state(&mut self, key: WinKey) {
        self.tilezones.remove(&key);
        self.input.remove(&key);
        self.visible_cells.remove(&key);
    }
}

/// Tab-aware toast suppression (T9): tell the feed which sessions the user is
/// looking at - the active workspace's tiles plus every torn-off workspace's
/// tiles (satellite windows are on screen too), as `th_*` tmux names.
pub fn sync_active_sessions(st: &CockpitState, feed: &OverlayFeed) {
    let mut active: HashSet<String> =
        st.model.main_tiles().unwrap_or(&[]).iter().map(|id| format!("th_{id}")).collect();
    for (i, _) in st.model.satellite_tabs() {
        active.extend(st.model.tabs[i].tiles.iter().map(|id| format!("th_{id}")));
    }
    feed.set_active_sessions(active);
}

/// Main-window sidebar hit zones, refreshed by every main paint. `ws_*` are
/// the workspace rows, in workspace order.
#[derive(Default)]
struct HitZones {
    ws_rows: Vec<RectF>,
    ws_closes: Vec<RectF>,
    ws_tears: Vec<RectF>,
    plus: RectF,
    /// Right half of the plus row (T-B): spawn a terminal in the active
    /// workspace via the socket `spawn_terminal` — the webview "+" parity.
    plus_term: RectF,
}

/// What a main-window sidebar click landed on, resolved BEFORE mutating the
/// model (the zones and the model live in the same struct, so hit-testing
/// borrows must end first). Tile-area clicks fall through to
/// [`tiles_mouse_down`].
enum SidebarTarget {
    WorkspaceClose(usize),
    WorkspaceTear(usize),
    Workspace(usize),
    Plus,
    PlusTerminal,
}

fn sidebar_hit(hits: &HitZones, x: f32, y: f32) -> Option<SidebarTarget> {
    for (i, r) in hits.ws_closes.iter().enumerate() {
        if r.contains(x, y) {
            return Some(SidebarTarget::WorkspaceClose(i));
        }
    }
    for (i, r) in hits.ws_tears.iter().enumerate() {
        if r.contains(x, y) {
            return Some(SidebarTarget::WorkspaceTear(i));
        }
    }
    for (i, r) in hits.ws_rows.iter().enumerate() {
        if r.contains(x, y) {
            return Some(SidebarTarget::Workspace(i));
        }
    }
    if hits.plus_term.contains(x, y) {
        return Some(SidebarTarget::PlusTerminal);
    }
    if hits.plus.contains(x, y) {
        return Some(SidebarTarget::Plus);
    }
    None
}

/// What a tile-area click landed on, per window.
enum TileTarget {
    Close(String),
    /// The header's fullscreen toggle (N3).
    Fullscreen(String),
    Badge(String),
    /// The header's editable work-name region (N1).
    Name(String),
    /// The header strip outside the buttons (N2: the drag-reorder handle).
    Header(String),
    Tile(String),
}

fn tile_hit(zones: &TileZones, x: f32, y: f32) -> Option<TileTarget> {
    for (id, r) in &zones.closes {
        if r.contains(x, y) {
            return Some(TileTarget::Close(id.clone()));
        }
    }
    for (id, r) in &zones.fulls {
        if r.contains(x, y) {
            return Some(TileTarget::Fullscreen(id.clone()));
        }
    }
    for (id, r) in &zones.badges {
        if r.contains(x, y) {
            return Some(TileTarget::Badge(id.clone()));
        }
    }
    for (id, r) in &zones.names {
        if r.contains(x, y) {
            return Some(TileTarget::Name(id.clone()));
        }
    }
    for (id, r, content) in &zones.tiles {
        if r.contains(x, y) {
            // Above the content box = the header strip (the reorder handle);
            // T6 terminal dispatch owns everything below.
            return Some(if y < content.y {
                TileTarget::Header(id.clone())
            } else {
                TileTarget::Tile(id.clone())
            });
        }
    }
    None
}

/// The window's focused tile: main = the model's `focused` (always in the
/// active workspace); satellite = the registry's per-window focus, validated
/// against the workspace's live tiles with a first-tile fallback (sessions die
/// out from under a stored focus).
fn win_focused(st: &CockpitState, key: WinKey) -> Option<String> {
    match key {
        WinKey::Main => st.model.focused.clone(),
        WinKey::Sat(wsid) => {
            let i = st.model.tab_by_wsid(wsid)?;
            let tiles = &st.model.tabs[i].tiles;
            st.sats
                .focused_of(wsid)
                .filter(|f| tiles.iter().any(|t| t == f))
                .map(str::to_string)
                .or_else(|| tiles.first().cloned())
        }
    }
}

fn set_win_focused(st: &mut CockpitState, key: WinKey, id: &str) {
    match key {
        WinKey::Main => st.model.set_focused(id),
        WinKey::Sat(wsid) => st.sats.set_focused(wsid, Some(id.to_string())),
    }
}

/// Move the window's focus to tile `id`, telling terminals that track focus
/// (mode 1004) - the click-to-focus core, shared by content and header presses.
fn focus_tile(st: &mut CockpitState, key: WinKey, id: &str) {
    let old = win_focused(st, key);
    if old.as_deref() == Some(id) {
        return;
    }
    if let Some(old) = old {
        if let Some(t) = st.tiles.get(&old) {
            notify_focus(t, false);
        }
    }
    if let Some(t) = st.tiles.get(id) {
        notify_focus(t, true);
    }
    set_win_focused(st, key, id);
}

/// The workspace tab a window's grid shows: main = the active tab (unless it
/// is torn off - then the main grid is empty and there is nothing to resize);
/// satellite = its bound workspace.
fn win_tab_index(model: &ChromeModel, key: WinKey) -> Option<usize> {
    match key {
        WinKey::Main => (!model.tabs[model.active].satellite).then_some(model.active),
        WinKey::Sat(wsid) => model.tab_by_wsid(wsid),
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// The gpui `WindowHandle`s of open satellite windows, by wsid. Main-thread
/// only (gpui windows live there), hence `Rc<RefCell<..>>` - NOT in the
/// [`CockpitState`] mutex, which worker threads lock.
pub type SatHandles = Rc<RefCell<HashMap<u64, WindowHandle<SatelliteView>>>>;

/// The cockpit window content: the sidebar column (workspace list + the T9
/// overlays below it) and the active workspace's tile grid. Holds the shared
/// state, the overlay entity, the feed handle, focus, the satellite window
/// handles, and the kept-alive `ControlClient` (its PTY readers live for the
/// process; the feed owns the process's single event subscription).
pub struct CockpitView {
    state: Arc<Mutex<CockpitState>>,
    overlays: Entity<OverlaySidebar>,
    feed: OverlayFeed,
    focus: FocusHandle,
    handles: SatHandles,
    client: Arc<crate::wire::ControlClient>,
    /// The T11 panels side surface (N5): the pre-built composite entity and
    /// its poll-only feed (composes beside the [`OverlayFeed`] - it takes no
    /// event subscription, honoring the single-drainer rule).
    panels: Entity<crate::panels::PanelHost>,
    panels_feed: crate::panels::PanelsFeed,
    /// T-B "+ terminal" double-click gate (webview #7 parity): one spawn in
    /// flight at a time, released when the request round-trips.
    spawn_in_flight: Arc<std::sync::atomic::AtomicBool>,
}

impl CockpitView {
    #[allow(clippy::too_many_arguments)] // the cockpit boot wires every subsystem once, here
    pub fn new(
        state: Arc<Mutex<CockpitState>>,
        overlays: Entity<OverlaySidebar>,
        feed: OverlayFeed,
        client: Arc<crate::wire::ControlClient>,
        handles: SatHandles,
        focus: FocusHandle,
        panels: Entity<crate::panels::PanelHost>,
        panels_feed: crate::panels::PanelsFeed,
    ) -> Self {
        spawn_logger();
        CockpitView {
            state,
            overlays,
            feed,
            focus,
            handles,
            client,
            panels,
            panels_feed,
            spawn_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// T-B: spawn a plain shell tile via the socket `spawn_terminal` (the same
    /// request path MCP/resume use, so there is exactly one way a session gets
    /// created). No cwd — the server roots the pane at $HOME, byte-for-byte the
    /// webview "+" Shell preset. Placement rides the `control://apply`
    /// broadcast (server-minted id -> `drain_applies` places + focuses in the
    /// active workspace). The request runs off-thread: it can block for the
    /// tmux spawn round-trip, and paint must not.
    fn spawn_terminal_local(&self) {
        use std::sync::atomic::Ordering;
        if self.spawn_in_flight.swap(true, Ordering::SeqCst) {
            return; // a spawn is already in flight (double-click guard)
        }
        let client = self.client.clone();
        let gate = self.spawn_in_flight.clone();
        thread::spawn(move || {
            match client.request("spawn_terminal", serde_json::json!({})) {
                Ok(v) => log::info!(
                    "+ terminal: spawn accepted (id={})",
                    v.get("id").and_then(|i| i.as_str()).unwrap_or("webview-minted")
                ),
                Err(e) => log::warn!("+ terminal: spawn_terminal failed: {e}"),
            }
            gate.store(false, Ordering::SeqCst);
        });
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
        let satellite = st.model.tabs[i].satellite;
        // A torn-off workspace paints in its own window, so it is never the
        // main grid's active workspace.
        let active = i == st.model.active && !satellite;
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
            (((row.w - 14.0 - sb.closes[i].w - sb.tears[i].w) / ui.cell_w).floor() as usize)
                .max(4);
        paint_parts(
            &[(truncate(&labels[i], label_cells), label_color, active)],
            row.x + 10.0,
            row.y + row_dy,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
        // Tear-off zone: `»` sends the workspace out to a satellite window,
        // `«` (accented while torn off) brings it back.
        paint_parts(
            &[(
                if satellite { "«".to_string() } else { "»".to_string() },
                if satellite { h(ACCENT) } else { h(FG_DIM) },
                false,
            )],
            sb.tears[i].x + (sb.tears[i].w - ui.cell_w) / 2.0,
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
    // The plus row splits in two (T-B): "+ workspace" (new tab, the original
    // affordance) on the left, "+ terminal" (spawn a shell tile in the active
    // workspace over the socket — the webview "+" parity) on the right.
    let plus_ws = RectF::new(sb.plus.x, sb.plus.y, sb.plus.w / 2.0, sb.plus.h);
    let plus_term = RectF::new(
        sb.plus.x + sb.plus.w / 2.0,
        sb.plus.y,
        sb.plus.w / 2.0,
        sb.plus.h,
    );
    paint_parts(
        &[("+ workspace".to_string(), h(FG_DIM), false)],
        plus_ws.x + 10.0,
        plus_ws.y + row_dy,
        &font_normal,
        &font_bold,
        window,
        cx,
    );
    paint_parts(
        &[("+ terminal".to_string(), h(FG_DIM), false)],
        plus_term.x + 10.0,
        plus_term.y + row_dy,
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
    st.hits.ws_tears = sb.tears;
    st.hits.plus = plus_ws;
    st.hits.plus_term = plus_term;
}

// ---------------------------------------------------------------------------
// Paint: the tile grid
// ---------------------------------------------------------------------------

/// Per-tile semantic status (T9 SidebarState) for the header dots: tile id
/// (`th_`-stripped) -> status. Gathered BEFORE the cockpit lock is taken so the
/// two locks never nest.
fn gather_tile_meta(feed: &OverlayFeed) -> HashMap<String, TileMeta> {
    let state = feed.state();
    let st = state.lock();
    let mut map = HashMap::new();
    for (uuid, tmux) in st.index.tmux_aliases() {
        let tile_id = tmux.strip_prefix("th_").unwrap_or(tmux);
        map.insert(
            tile_id.to_string(),
            TileMeta {
                status: Some(st.supervision.status_of(uuid)),
                context_pct: st.usage.context_pct_of(uuid),
            },
        );
    }
    map
}

/// Whether a session's agent is mid-turn (webview `tmuxSessionMidTurn`): the
/// N4 kill dialog flags these as busy.
fn session_busy(feed: &OverlayFeed, id: &str) -> bool {
    let state = feed.state();
    let st = state.lock();
    let uuid = st
        .index
        .tmux_aliases()
        .find(|(_, tmux)| tmux.strip_prefix("th_").unwrap_or(tmux) == id)
        .map(|(uuid, _)| uuid.to_string());
    uuid.is_some_and(|uuid| {
        matches!(
            st.supervision.status_of(&uuid),
            SessionStatus::Working
                | SessionStatus::WaitingOnSubagents
                | SessionStatus::NeedsQuestion
                | SessionStatus::NeedsPermission
        )
    })
}

/// Paint one window's tile grid (the main grid area or a whole satellite
/// window) from the shared state, writing THAT window's hit zones and painted
/// cell count. A tile lives in exactly one window, so damage syncing here never
/// races another window's paint.
fn paint_tiles_area(
    state: &Arc<Mutex<CockpitState>>,
    key: WinKey,
    metas: &HashMap<String, TileMeta>,
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

    let focused_id = win_focused(&st, key);
    // The window's divider-drag/hover state, copied out before the split
    // borrow (paint only reads it for the highlight + cursor).
    let (div_drag, last_pos) = st
        .input
        .get(&key)
        .map(|i| (i.div_drag, i.last_pos))
        .unwrap_or((None, None));
    // The window's active header drag-reorder (N2), for the drop highlight.
    let drag_target = st
        .input
        .get(&key)
        .and_then(|i| i.tile_drag.as_ref())
        .filter(|d| d.dragging)
        .and_then(|d| d.target.clone());

    // Split-borrow the state so the tiles map can be mutated while the model,
    // titles, and stamps are read (all disjoint fields of one MutexGuard).
    let CockpitState { model, tiles, titles, cwds, git, pulses, lives, epoch, .. } = &mut *st;
    let now_ms = epoch.elapsed().as_millis() as u64;
    let (ids, ratios, empty_msg): (Vec<String>, Option<GridRatios>, &str) = match key {
        WinKey::Main => match model.main_tiles() {
            Some(t) => (
                t.to_vec(),
                model.tabs[model.active].grid.clone(),
                "This workspace is empty - new sessions land in the active workspace.",
            ),
            None => (
                Vec::new(),
                None,
                "Every workspace is torn off into its own window - « brings one back.",
            ),
        },
        WinKey::Sat(wsid) => match model.tab_by_wsid(wsid) {
            Some(i) => (
                model.tabs[i].tiles.clone(),
                model.tabs[i].grid.clone(),
                "This workspace is empty.",
            ),
            // Workspace deleted from the main sidebar; the window is closing.
            None => (Vec::new(), None, ""),
        },
    };

    // N3: a fullscreen tile takes the whole grid; the others stay attached but
    // unpainted this frame (exactly like a workspace switch - no detach).
    let fullscreen = win_tab_index(model, key)
        .and_then(|i| model.tabs[i].fullscreen.clone())
        .filter(|f| ids.iter().any(|x| x == f));
    let (ids, ratios) = match &fullscreen {
        Some(f) => (vec![f.clone()], None),
        None => (ids, ratios),
    };

    let mut tile_hits = Vec::with_capacity(ids.len());
    let mut close_hits = Vec::with_capacity(ids.len());
    let mut full_hits = Vec::with_capacity(ids.len());
    let mut badge_hits = Vec::new();
    let mut name_hits = Vec::new();
    let mut rebuilt_frame: u64 = 0;
    let mut total_frame: u64 = 0;
    let mut cells_frame: u64 = 0;

    if ids.is_empty() && !empty_msg.is_empty() {
        paint_parts(
            &[(empty_msg.to_string(), h(FG_DIM), false)],
            area.x + 8.0,
            area.y + 8.0,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
    }

    for (id, bx) in ids.iter().zip(tile_boxes_ratio(ids.len(), area, GAP, ratios.as_ref())) {
        let is_focused = focused_window && focused_id.as_deref() == Some(id.as_str());
        let life = lives.get(id);
        paint_tile_frame(b(bx), is_focused, window);

        // Close zone in the header's right corner; the fullscreen toggle (N3)
        // sits just left of it.
        let close_w = ui.cell_w + 10.0;
        let close = RectF::new(bx.x + bx.w - BORDER - close_w, bx.y + BORDER, close_w, HEADER_H);
        let full = RectF::new(close.x - close_w, bx.y + BORDER, close_w, HEADER_H);

        // Content box: inside the border/padding, below the cockpit header.
        let content = RectF::new(
            bx.x + BORDER + TILE_PAD,
            bx.y + BORDER + HEADER_H,
            (bx.w - 2.0 * (BORDER + TILE_PAD)).max(1.0),
            (bx.h - HEADER_H - 2.0 * BORDER - TILE_PAD).max(1.0),
        );

        if let Some(tile) = tiles.get_mut(id) {
            let paint =
                sync_and_paint_content(tile, b(content), mode, is_focused, window, cx);
            rebuilt_frame += paint.rebuilt;
            total_frame += paint.total;
            cells_frame += tile.cols as u64 * tile.rows as u64;
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
        } else if !life.is_defunct() {
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
        // A dead/exited tile's last content stays visible but dims under a
        // scrim - frozen-and-explained, not normal-looking (T24).
        if life.is_defunct() {
            window.paint_quad(fill(b(content), ha(WINDOW_BG, DEFUNCT_DIM)));
        }

        // --- the cockpit header (N1 anatomy): liveness dot, client glyph,
        // folder, git branch + worktree badge + dirty dot, editable work-name,
        // context meter, age, life badge, close --------------------------------
        // Dot: the T24 life state wins (a dead session's stale agent status is
        // noise); then semantic agent status (T9); then output-recency.
        let meta_info = metas.get(id).copied().unwrap_or_default();
        let dot = match life {
            TileLife::Dead { .. } | TileLife::Exited { .. } => h(DEAD),
            TileLife::Reconnecting { .. } => h(NEEDS),
            TileLife::Live => match meta_info.status {
                Some(SessionStatus::Working) | Some(SessionStatus::WaitingOnSubagents) => h(LIVE),
                Some(SessionStatus::NeedsQuestion) | Some(SessionStatus::NeedsPermission) => {
                    h(NEEDS)
                }
                _ => {
                    let busy = pulses
                        .get(id)
                        .map(|p| now_ms.saturating_sub(p.last_output_ms()) < BUSY_WINDOW_MS)
                        .unwrap_or(false);
                    if busy {
                        h(LIVE)
                    } else {
                        h(IDLE_DOT)
                    }
                }
            },
        };
        let title = titles.get(id).filter(|t| !t.is_empty()).cloned().unwrap_or_else(|| id.clone());
        // Client glyph off the pane's foreground command (the server already
        // substitutes `claude`/`codex` for runtime wrappers): the webview's
        // clay spark / ">_" icons, as colored text.
        let (client_glyph, client_color) = match title.as_str() {
            "claude" => ("✳ ", h(CLAUDE)),
            "codex" => (">_ ", h(CODEX)),
            _ => ("", h(FG_DIM)),
        };
        let cwd = cwds.get(id).cloned().unwrap_or_default();
        // Folder = cwd basename (webview `cwdBasename`); the raw title is the
        // fallback for sessions the server lists without a cwd.
        let folder = if cwd.is_empty() {
            title.clone()
        } else {
            crate::overlays::model::cwd_basename(&cwd).to_string()
        };
        let g = git.get(&cwd).filter(|g| g.is_repo);
        let branch = g.and_then(|g| g.branch.clone()).unwrap_or_default();
        let wt = g.is_some_and(|g| g.is_linked_worktree);
        let dirty = g.is_some_and(|g| g.dirty_count > 0);
        // Work-name (N1): the in-progress edit buffer wins; else the stored
        // name; else the webview's "name this work…" placeholder.
        let editing = model.naming.as_ref().filter(|n| n.tile == *id);
        let (name_text, name_color, name_bold) = match editing {
            Some(n) => (format!("{}_", n.buffer), h(ACCENT), true),
            None => match model.work_name_for(&cwd) {
                Some(n) => (n.to_string(), h(FG), false),
                None => ("name this work…".to_string(), h(FG_DIM), false),
            },
        };

        // The right-aligned cue cluster: context meter (Claude fill %), then
        // last-output age (T24) plus the life badge saying WHY a tile looks
        // frozen. On a narrow tile the badge outranks the age, and truncates
        // itself before it may ever overflow left across the dot/title.
        // Budget subtracts BOTH header buttons (fullscreen + close, N3) and
        // the meter's pixel width.
        let meter_pct = meta_info.context_pct.filter(|p| p.is_finite());
        let meter_px = if meter_pct.is_some() { METER_W + 6.0 } else { 0.0 };
        let header_chars = (((bx.w - 2.0 * (BORDER + TILE_PAD) - 2.0 * close_w - meter_px)
            / ui.cell_w)
            .floor() as usize)
            .saturating_sub(2); // the dot
        let max_cue = header_chars.saturating_sub(1);
        let mut age =
            pulses.get(id).map(|p| age_label(p.last_output_ms(), now_ms)).unwrap_or_default();
        let (badge_text, badge_color, badge_bold) = match life {
            TileLife::Live => (String::new(), h(FG_DIM), false),
            TileLife::Reconnecting { since_ms } => (
                format!("reconnecting {}", format_age(now_ms.saturating_sub(since_ms))),
                h(NEEDS),
                true,
            ),
            TileLife::Exited { code, .. } => (format!("EXITED {code}"), h(DEAD), true),
            TileLife::Dead { .. } => ("DEAD".to_string(), h(DEAD), true),
        };
        let mut pct_text = meter_pct.map(|p| format!("{:.0}% ", p)).unwrap_or_default();
        let mut cue_gap = if !age.is_empty() && !badge_text.is_empty() { "  " } else { "" };
        if pct_text.chars().count() + age.chars().count() + cue_gap.len()
            + badge_text.chars().count()
            > max_cue
        {
            age.clear();
            cue_gap = "";
            pct_text.clear();
        }
        let badge_text = truncate(&badge_text, max_cue);
        let cue_chars = pct_text.chars().count()
            + age.chars().count()
            + cue_gap.len()
            + badge_text.chars().count();

        let avail =
            header_chars.saturating_sub(if cue_chars > 0 { cue_chars + 2 } else { 0 });
        // Budget the left cluster: folder gets priority, then branch cues,
        // then the work-name; each segment drops whole when it cannot fit.
        let fixed = client_glyph.chars().count();
        let folder_text = truncate(&folder, avail.saturating_sub(fixed).max(4));
        let mut used = fixed + folder_text.chars().count();
        let branch_text = if !branch.is_empty() && used + branch.chars().count() + 3 <= avail {
            format!("  {}", truncate(&branch, avail - used - 2))
        } else {
            String::new()
        };
        used += branch_text.chars().count();
        let wt_text = if wt && used + 5 <= avail { " [wt]" } else { "" };
        used += wt_text.chars().count();
        let dirty_text = if dirty && used + 2 <= avail { " •" } else { "" };
        used += dirty_text.chars().count();
        let name_text = if used + 8 <= avail {
            format!("  {}", truncate(&name_text, avail - used - 2))
        } else {
            String::new()
        };
        let name_offset = used + 2 + 2; // dot(2) + gap(2) before the name
        let header_y = bx.y + BORDER + (HEADER_H - UI_LINE_H) / 2.0 + 1.0;
        let header_x = bx.x + BORDER + TILE_PAD;
        if !name_text.is_empty() {
            // The clickable work-name zone (N1): at least 8 cells so the
            // placeholder-less state stays targetable.
            let name_w = (name_text.chars().count().max(8) as f32 - 2.0) * ui.cell_w;
            name_hits.push((
                id.clone(),
                RectF::new(
                    header_x + (name_offset as f32 - 2.0) * ui.cell_w,
                    bx.y + BORDER,
                    name_w + 2.0 * ui.cell_w,
                    HEADER_H,
                ),
            ));
        }
        paint_parts(
            &[
                ("● ".to_string(), dot, false),
                (client_glyph.to_string(), client_color, false),
                (folder_text, if is_focused { h(ACCENT) } else { h(FG) }, true),
                (branch_text, h(FG_DIM), false),
                (wt_text.to_string(), h(NEEDS), false),
                (dirty_text.to_string(), h(DIRTY), false),
                (name_text, name_color, name_bold),
            ],
            header_x,
            header_y,
            &font_normal,
            &font_bold,
            window,
            cx,
        );
        if cue_chars > 0 {
            paint_parts(
                &[
                    (pct_text, h(FG_DIM), false),
                    (age, h(FG_DIM), false),
                    (cue_gap.to_string(), h(FG_DIM), false),
                    (badge_text, badge_color, badge_bold),
                ],
                full.x - cue_chars as f32 * ui.cell_w - 4.0,
                header_y,
                &font_normal,
                &font_bold,
                window,
                cx,
            );
        }
        // The context meter bar (N1): a small track + fill left of the cue
        // text (which itself sits left of the fullscreen button), colored by
        // the webview's fill ramp.
        if let Some(pct) = meter_pct {
            let pct = pct.clamp(0.0, 100.0);
            let track = RectF::new(
                full.x - cue_chars as f32 * ui.cell_w - 4.0 - meter_px,
                bx.y + BORDER + (HEADER_H - 4.0) / 2.0,
                METER_W,
                4.0,
            );
            let fill_color = if pct >= 90.0 {
                METER_HOT
            } else if pct >= 75.0 {
                METER_WARN
            } else {
                METER_OK
            };
            window.paint_quad(fill(b(track), ha(FG_DIM, 0.25)));
            window.paint_quad(fill(
                b(RectF::new(track.x, track.y, METER_W * pct / 100.0, track.h)),
                h(fill_color),
            ));
        }
        // Fullscreen toggle (N3): ⤢ expands this tile over the grid, ⤡ (accented
        // while fullscreen) restores - the webview Tile header button.
        let is_fs = fullscreen.as_deref() == Some(id.as_str());
        paint_parts(
            &[(
                if is_fs { "⤡".to_string() } else { "⤢".to_string() },
                if is_fs { h(ACCENT) } else { h(FG_DIM) },
                false,
            )],
            full.x + (full.w - ui.cell_w) / 2.0,
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

        // Drop-target highlight (N2): the tile a header drag would splice at
        // washes accent and gets a 2px frame.
        if drag_target.as_deref() == Some(id.as_str()) {
            window.paint_quad(fill(b(bx), ha(ACCENT, 0.12)));
            for edge in [
                RectF::new(bx.x, bx.y, bx.w, 2.0),
                RectF::new(bx.x, bx.y + bx.h - 2.0, bx.w, 2.0),
                RectF::new(bx.x, bx.y, 2.0, bx.h),
                RectF::new(bx.x + bx.w - 2.0, bx.y, 2.0, bx.h),
            ] {
                window.paint_quad(fill(b(edge), h(ACCENT)));
            }
        }

        tile_hits.push((id.clone(), bx, content));
        close_hits.push((id.clone(), close));
        full_hits.push((id.clone(), full));
    }

    // --- dividers (T26): hover/drag highlight + resize cursor -----------------
    let dividers = divider_zones(ids.len(), area, GAP, ratios.as_ref());
    let hover_div = if div_drag.is_none() {
        last_pos
            .and_then(|(x, y)| dividers.iter().find(|(_, r)| r.contains(x, y)))
            .map(|(d, _)| *d)
    } else {
        None
    };
    for (d, r) in &dividers {
        let dragging = div_drag == Some(*d);
        if !dragging && hover_div != Some(*d) {
            continue;
        }
        // A thin bar centered in the visual gap (the hit band is wider).
        let bar = match d {
            DividerId::Row(_) => RectF::new(r.x, r.y + (r.h - 2.0) / 2.0, r.w, 2.0),
            DividerId::Col { .. } => RectF::new(r.x + (r.w - 2.0) / 2.0, r.y, 2.0, r.h),
        };
        window.paint_quad(fill(b(bar), if dragging { h(ACCENT) } else { ha(ACCENT, 0.5) }));
    }
    if let Some(d) = div_drag.or(hover_div) {
        window.set_window_cursor_style(match d {
            DividerId::Row(_) => CursorStyle::ResizeRow,
            DividerId::Col { .. } => CursorStyle::ResizeColumn,
        });
    }

    st.tilezones.insert(
        key,
        TileZones {
            tiles: tile_hits,
            closes: close_hits,
            fulls: full_hits,
            badges: badge_hits,
            names: name_hits,
            dividers,
            area,
        },
    );
    st.visible_cells.insert(key, cells_frame);
    drop(st);

    // The T5 fps logger keeps its single-window semantics: only the main
    // window records (satellite paint cost shows up in the T10 winstat log).
    if key == WinKey::Main {
        record_frame(t0.elapsed().as_nanos() as u64, rebuilt_frame, total_frame);
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Resolve a window position to a cell of tile `id`, via the content rect the
/// window's last paint recorded. Returns the cell plus whether it was inside.
fn chrome_cell(
    st: &CockpitState,
    key: WinKey,
    id: &str,
    pos: gpui::Point<Pixels>,
) -> Option<crate::render_support::CellHit> {
    let zones = st.tilezones.get(&key)?;
    let (_, _, content) = zones.tiles.iter().find(|(tid, _, _)| tid == id)?;
    let tile = st.tiles.get(id)?;
    let m = tile.metrics?;
    let rel_x = f32::from(pos.x) - content.x;
    let rel_y = f32::from(pos.y) - content.y;
    Some(cell_from_pixel(rel_x, rel_y, m.cell_w, m.line_h, tile.cols, tile.rows))
}

// ---------------------------------------------------------------------------
// The per-window tile input core, shared by the cockpit and every satellite
// (the T6 semantics live one level down in `render`; this layer only resolves
// WHICH tile of WHICH window an event belongs to).
// ---------------------------------------------------------------------------

/// A left press on a divider (T26): double-click resets the workspace to the
/// auto grid; a single press starts a drag (materializing even ratios first
/// when the workspace was still auto). Returns whether the press was consumed.
fn divider_mouse_down(
    st: &mut CockpitState,
    key: WinKey,
    ev: &MouseDownEvent,
    x: f32,
    y: f32,
) -> bool {
    let Some(id) = st
        .tilezones
        .get(&key)
        .and_then(|z| z.dividers.iter().find(|(_, r)| r.contains(x, y)))
        .map(|(d, _)| *d)
    else {
        return false;
    };
    let Some(tab) = win_tab_index(&st.model, key) else { return true };
    if ev.click_count >= 2 {
        // Reset to auto: clear the ratios for good (they were the only thing
        // making this workspace non-auto).
        st.input.entry(key).or_default().div_drag = None;
        st.model.tabs[tab].grid = None;
        st.save_layout();
        return true;
    }
    let buckets = split_rows(st.model.tabs[tab].tiles.len());
    let needs_seed =
        !st.model.tabs[tab].grid.as_ref().is_some_and(|g| g.matches(&buckets));
    if needs_seed {
        st.model.tabs[tab].grid = Some(GridRatios::even(&buckets));
    }
    st.input.entry(key).or_default().div_drag = Some(id);
    true
}

/// Drag a divider to the pointer (T26): map the pointer into the neighbor
/// pair's extent and re-split their fractions, clamped to [`MIN_TILE_PX`].
/// The reflowed boxes land next paint; each tile's PTY refits through the
/// existing `sync_and_paint_content` geometry path. Returns whether the
/// ratios changed (the caller repaints).
fn divider_drag_motion(st: &mut CockpitState, key: WinKey, id: DividerId, x: f32, y: f32) -> bool {
    let Some(tab) = win_tab_index(&st.model, key) else { return false };
    let Some(area) = st.tilezones.get(&key).map(|z| z.area) else { return false };
    let n = st.model.tabs[tab].tiles.len();
    let Some(mut g) = st.model.tabs[tab].grid.clone() else { return false };
    // A reconcile can reflow the grid mid-drag: the seeded ratios no longer
    // describe what is painted, so the drag stops tracking instead of mutating
    // dormant ratios (or resizing the wrong pair through a stale id).
    if !g.matches(&split_rows(n)) {
        return false;
    }
    let Some((start, combined)) = divider_extent(n, area, GAP, Some(&g), id) else {
        return false;
    };
    let pointer = match id {
        DividerId::Row(_) => y,
        DividerId::Col { .. } => x,
    };
    let t = (pointer - start - GAP / 2.0) / combined.max(1.0);
    let min_t = MIN_TILE_PX / combined.max(1.0);
    if !apply_divider_split(&mut g, id, t, min_t) {
        return false;
    }
    st.model.tabs[tab].grid = Some(g);
    true
}

fn tiles_mouse_down(
    st: &mut CockpitState,
    key: WinKey,
    feed: &OverlayFeed,
    button: MouseButton,
    ev: &MouseDownEvent,
    cx: &mut App,
) {
    let x: f32 = ev.position.x.into();
    let y: f32 = ev.position.y.into();
    if std::env::var("THN_HIT_DEBUG").as_deref() == Ok("1") {
        let z = st.tilezones.get(&key);
        log::info!(
            "hitdbg[{key:?}] down at ({x:.1},{y:.1}); dividers={:?} area={:?}",
            z.map(|z| z.dividers.clone()),
            z.map(|z| z.area)
        );
    }
    // Dividers first: their hit bands straddle the gaps and overlap the
    // neighboring tiles' outer edges by a few pixels.
    if button == MouseButton::Left && divider_mouse_down(st, key, ev, x, y) {
        return;
    }
    let target = st.tilezones.get(&key).and_then(|z| tile_hit(z, x, y));
    // A click anywhere but the name zone commits an in-progress work-name
    // edit (the webview's blur-commit).
    if !matches!(target, Some(TileTarget::Name(_)))
        && st.model.naming.is_some()
        && st.model.commit_name()
    {
        st.save_layout();
    }
    match target {
        Some(TileTarget::Name(id)) if button == MouseButton::Left && key == WinKey::Main => {
            // Begin the work-name edit (N1). Names key by cwd, so a session
            // the server lists without one has no slot to edit.
            if let Some(cwd) = st.cwds.get(&id).cloned().filter(|c| !c.is_empty()) {
                st.model.begin_name_edit(&id, &cwd);
            }
        }
        Some(TileTarget::Close(id)) if button == MouseButton::Left => {
            if st.model.close_tile(&id) {
                st.drop_tile(&id);
                st.save_layout();
                sync_active_sessions(st, feed);
            }
        }
        Some(TileTarget::Fullscreen(id)) if button == MouseButton::Left => {
            // N3: the header ⤢/⤡ toggles this tile fullscreen (focus follows,
            // like the webview button's onFocus).
            if let Some(tab) = win_tab_index(&st.model, key) {
                if st.model.toggle_fullscreen(tab, &id) {
                    set_win_focused(st, key, &id);
                }
            }
        }
        Some(TileTarget::Badge(id)) if button == MouseButton::Left => {
            if let Some(tile) = st.tiles.get(&id) {
                tile.term.lock().scroll_to_bottom();
            }
        }
        Some(TileTarget::Header(id)) if button == MouseButton::Left => {
            // N2: a left press on the header focuses the tile and ARMS a
            // drag-reorder; it only becomes one past the movement threshold,
            // so a plain header click stays focus-only. Terminal selection
            // drags never start here (they need a content-area press), and
            // dividers were consumed above.
            focus_tile(st, key, &id);
            st.input.entry(key).or_default().tile_drag =
                Some(TileDrag { id, start: (x, y), dragging: false, target: None });
        }
        Some(TileTarget::Header(id)) | Some(TileTarget::Tile(id)) => {
            // Focus follows click; tell terminals that track focus (1004).
            focus_tile(st, key, &id);
            // T6 dispatch: URL open, mouse reporting, selection, paste.
            if let Some(cell) = chrome_cell(st, key, &id, ev.position) {
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
                        st.input.entry(key).or_default().drag =
                            Some(ChromeDrag { id, mode, last_cell: (cell.col, cell.row) });
                    }
                }
            }
        }
        _ => {}
    }
}

fn tiles_mouse_up(st: &mut CockpitState, key: WinKey, button: MouseButton, ev: &MouseUpEvent) {
    // A divider drag commits on release: persist the ratios once (not per
    // motion event - dragging would hammer the layout file).
    if button == MouseButton::Left {
        if let Some(i) = st.input.get_mut(&key) {
            if i.div_drag.take().is_some() {
                st.save_layout();
                return;
            }
        }
        // A header drag-reorder commits on release (N2): splice the dragged
        // tile at the drop target's slot in THIS window's workspace.
        if let Some(d) = st.input.get_mut(&key).and_then(|i| i.tile_drag.take()) {
            if d.dragging {
                if let (Some(tab), Some(target)) = (win_tab_index(&st.model, key), d.target) {
                    if st.model.reorder_tile_in(tab, &d.id, &target) {
                        set_win_focused(st, key, &d.id);
                        st.save_layout();
                    }
                }
            }
            return;
        }
    }
    let Some(drag) = st.input.get_mut(&key).and_then(|i| i.drag.take()) else { return };
    let ends_drag = match drag.mode {
        DragMode::Select => button == MouseButton::Left,
        DragMode::Report(btn) => crate::render::report_button(button) == Some(btn),
    };
    if !ends_drag {
        st.input.entry(key).or_default().drag = Some(drag);
        return;
    }
    if let DragMode::Report(btn) = drag.mode {
        if let Some(cell) = chrome_cell(st, key, &drag.id, ev.position) {
            if let Some(tile) = st.tiles.get(&drag.id) {
                tile_report_release(tile, btn, cell, ev.modifiers.alt, ev.modifiers.control);
            }
        }
    }
}

/// Returns whether a selection or divider drag advanced (the caller repaints).
fn tiles_mouse_move(st: &mut CockpitState, key: WinKey, ev: &MouseMoveEvent) -> bool {
    let x: f32 = ev.position.x.into();
    let y: f32 = ev.position.y.into();
    // Remember the pointer for the divider hover highlight + resize cursor.
    st.input.entry(key).or_default().last_pos = Some((x, y));

    if let Some(id) = st.input.get(&key).and_then(|i| i.div_drag) {
        return divider_drag_motion(st, key, id, x, y);
    }

    // A header drag-reorder in progress (N2): arm past the threshold, then
    // track the tile under the pointer as the drop target for the highlight.
    if st.input.get(&key).is_some_and(|i| i.tile_drag.is_some()) {
        let target = st
            .tilezones
            .get(&key)
            .and_then(|z| z.tiles.iter().find(|(_, r, _)| r.contains(x, y)))
            .map(|(id, _, _)| id.clone());
        let Some(d) = st.input.get_mut(&key).and_then(|i| i.tile_drag.as_mut()) else {
            return false;
        };
        if !d.dragging
            && (x - d.start.0).abs().max((y - d.start.1).abs()) > DRAG_THRESHOLD_PX
        {
            d.dragging = true;
        }
        if !d.dragging {
            return false;
        }
        let target = target.filter(|t| *t != d.id);
        let changed = d.target != target;
        d.target = target;
        return changed;
    }

    if let Some(drag) = st.input.get(&key).and_then(|i| i.drag.as_ref()) {
        let id = drag.id.clone();
        let mode = drag.mode;
        let last = drag.last_cell;
        let Some(cell) = chrome_cell(st, key, &id, ev.position) else { return false };
        if (cell.col, cell.row) == last {
            return false;
        }
        if let Some(tile) = st.tiles.get(&id) {
            tile_drag_motion(tile, mode, cell, ev.modifiers.alt, ev.modifiers.control);
        }
        if let Some(d) = st.input.get_mut(&key).and_then(|i| i.drag.as_mut()) {
            d.last_cell = (cell.col, cell.row);
        }
        return mode == DragMode::Select;
    }

    // Buttonless hover motion (mode 1003) on the tile under the pointer.
    if ev.modifiers.shift {
        return false;
    }
    let hovered = st
        .tilezones
        .get(&key)
        .and_then(|z| z.tiles.iter().find(|(_, r, _)| r.contains(x, y)))
        .map(|(id, _, _)| id.clone());
    let Some(id) = hovered else {
        if let Some(i) = st.input.get_mut(&key) {
            i.hover_cell = None;
        }
        return false;
    };
    let Some(cell) = chrome_cell(st, key, &id, ev.position) else { return false };
    if !cell.inside {
        if let Some(i) = st.input.get_mut(&key) {
            i.hover_cell = None;
        }
        return false;
    }
    let input = st.input.entry(key).or_default();
    if input.hover_cell.as_ref() == Some(&(id.clone(), cell.col, cell.row)) {
        return false;
    }
    input.hover_cell = Some((id.clone(), cell.col, cell.row));
    if let Some(tile) = st.tiles.get(&id) {
        tile_hover_motion(tile, cell, ev.modifiers.alt, ev.modifiers.control);
    }
    false
}

/// Terminal key input through the shared T6 core (find bar, copy/paste,
/// scrollback keys, PTY encoding) on the window's focused tile.
fn tiles_key(st: &mut CockpitState, key: WinKey, ks: &gpui::Keystroke, cx: &mut App) {
    let chord = chord_of(ks);
    // Esc restores the grid while a tile is fullscreen (N3) - the webview's
    // capture-phase Escape handler; it never reaches the terminal then.
    if chord.key == "escape" && !chord.control && !chord.alt && !chord.shift && !chord.platform {
        if let Some(tab) = win_tab_index(&st.model, key) {
            if st.model.clear_fullscreen(tab) {
                return;
            }
        }
    }
    if let Some(id) = win_focused(st, key) {
        if let Some(tile) = st.tiles.get_mut(&id) {
            tile_key_input(tile, &chord, cx);
        }
    }
}

fn tiles_scroll(st: &mut CockpitState, key: WinKey, ev: &ScrollWheelEvent) {
    // The wheel acts on the tile under the pointer (fallback: the focused one).
    let x: f32 = ev.position.x.into();
    let y: f32 = ev.position.y.into();
    let id = st
        .tilezones
        .get(&key)
        .and_then(|z| z.tiles.iter().find(|(_, r, _)| r.contains(x, y)))
        .map(|(id, _, _)| id.clone())
        .or_else(|| win_focused(st, key));
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
    let input = st.input.entry(key).or_default();
    input.wheel_accum += dy;
    let lines = input.wheel_accum as i32;
    if lines == 0 {
        return;
    }
    input.wheel_accum -= lines as f32;

    let Some(cell) = chrome_cell(st, key, &id, ev.position) else { return };
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
        // A click outside the kill dialog's buttons dismisses it (the
        // webview's backdrop-click cancel; the buttons stop propagation).
        {
            let mut st = self.state.lock();
            if st.confirm_kill.is_some() {
                st.confirm_kill = None;
                cx.notify();
                return;
            }
        }
        // Any click dismisses the palette (the webview's backdrop-click close;
        // the native palette takes no mouse input) and, in the tile area,
        // returns the keyboard focus region to the tiles.
        self.state.lock().keys.palette.close();
        let target = sidebar_hit(&self.state.lock().hits, x, y);
        match target {
            Some(SidebarTarget::WorkspaceClose(i)) if button == MouseButton::Left => {
                self.close_workspace(i, cx);
            }
            Some(SidebarTarget::WorkspaceTear(i)) if button == MouseButton::Left => {
                let torn = {
                    let st = self.state.lock();
                    st.model.tabs.get(i).map(|t| (t.wsid, t.satellite))
                };
                match torn {
                    Some((wsid, true)) => {
                        // `«`: bring the satellite home and close its window.
                        close_satellite_home(
                            cx,
                            &self.state,
                            &self.feed,
                            &self.handles,
                            wsid,
                        );
                    }
                    Some((_, false)) => {
                        tear_off_workspace(cx, &self.state, &self.feed, &self.handles, i);
                    }
                    None => {}
                }
            }
            Some(SidebarTarget::Workspace(i)) => {
                if button == MouseButton::Right || ev.click_count >= 2 {
                    self.state.lock().model.begin_rename(i);
                } else if button == MouseButton::Left {
                    let sat = {
                        let mut st = self.state.lock();
                        st.model.commit_rename();
                        let sat = st.model.tabs.get(i).and_then(|t| {
                            t.satellite.then_some(t.wsid)
                        });
                        if sat.is_none() {
                            st.model.set_active(i);
                            st.save_layout();
                            sync_active_sessions(&st, &self.feed);
                        }
                        sat
                    };
                    // Clicking a torn-off workspace raises its window instead.
                    if let Some(wsid) = sat {
                        if let Some(h) = self.handles.borrow().get(&wsid) {
                            h.update(cx, |_, window, _| window.activate_window()).ok();
                        }
                    }
                }
            }
            Some(SidebarTarget::Plus) if button == MouseButton::Left => {
                let mut st = self.state.lock();
                st.model.add_tab();
                st.save_layout();
                sync_active_sessions(&st, &self.feed);
            }
            Some(SidebarTarget::PlusTerminal) if button == MouseButton::Left => {
                self.spawn_terminal_local();
            }
            _ => {
                let mut st = self.state.lock();
                st.model.commit_rename();
                st.keys.region = Region::Tiles;
                tiles_mouse_down(&mut st, WinKey::Main, &self.feed, button, ev, cx);
            }
        }
        // Clicks route the keyboard: inside the open panels column they focus
        // the PanelHost (its Files search / Run command line take typing -
        // never the focused tile's PTY); anywhere else, back to the cockpit.
        let in_panels = self.state.lock().panels_open
            && x >= f32::from(window.viewport_size().width) - PANELS_W;
        if in_panels {
            window.focus(&self.panels.focus_handle(cx));
        } else {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    /// Close workspace `i` from the sidebar `x`: drop its tiles from the pool
    /// and, if it was torn off, close its satellite window too.
    fn close_workspace(&mut self, i: usize, cx: &mut Context<Self>) {
        let sat_to_close = {
            let mut st = self.state.lock();
            let torn = st.model.tabs.get(i).and_then(|t| t.satellite.then_some(t.wsid));
            let Some(removed) = st.model.close_tab(i) else { return };
            for id in &removed {
                st.drop_tile(id);
            }
            if let Some(wsid) = torn {
                st.sats.close(wsid);
                st.drop_window_state(WinKey::Sat(wsid));
            }
            st.save_layout();
            sync_active_sessions(&st, &self.feed);
            torn
        };
        if let Some(wsid) = sat_to_close {
            if let Some(h) = self.handles.borrow_mut().remove(&wsid) {
                h.update(cx, |_, window, _| window.remove_window()).ok();
            }
            log_winstat(&self.state, "workspace-closed");
        }
    }

    fn mouse_up(
        &mut self,
        button: MouseButton,
        ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tiles_mouse_up(&mut self.state.lock(), WinKey::Main, button, ev);
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if tiles_mouse_move(&mut self.state.lock(), WinKey::Main, ev) {
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let mut st = self.state.lock();

        // The kill confirm dialog (N4) captures the keyboard: Enter kills,
        // Esc cancels, everything else is swallowed while it is up.
        if st.confirm_kill.is_some() {
            match ks.key.as_str() {
                "enter" => self.kill_confirmed(&mut st),
                "escape" => st.confirm_kill = None,
                _ => {}
            }
            cx.notify();
            return;
        }

        // Work-name edit mode (N1) captures the keyboard like rename mode.
        if st.model.naming.is_some() {
            match ks.key.as_str() {
                "enter" => {
                    if st.model.commit_name() {
                        st.save_layout();
                    }
                }
                "escape" => st.model.cancel_name(),
                "backspace" => st.model.name_backspace(),
                _ => {
                    if !ks.modifiers.control && !ks.modifiers.platform {
                        if let Some(kc) = ks.key_char.as_deref() {
                            if !kc.is_empty() && !kc.chars().any(char::is_control) {
                                st.model.name_push(kc);
                            }
                        }
                    }
                }
            }
            cx.notify();
            return;
        }

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

        // The T-A keymap sees every keystroke BEFORE the tile core (the
        // webview's capture-phase handler): prefix chords, direct chords, and
        // the open palette consume; everything else falls through unchanged.
        // The focused tile's find bar is the editable-target guard.
        let guard = win_focused(&st, WinKey::Main)
            .and_then(|id| st.tiles.get(&id))
            .is_some_and(|t| t.search_open());
        let now = st.epoch.elapsed().as_millis() as u64;
        let handled = {
            let CockpitState { keys, model, .. } = &mut *st;
            keys.on_key(&chord_of(ks), model, guard, now)
        };
        // While the panels side surface holds keyboard focus, its own key
        // handler (a child of this root, so it already ran) owns plain keys -
        // typing a Files search must never fall through into the focused
        // tile's PTY. Keymap chords above still work (prefix `f` closes it).
        let panels_focused =
            st.panels_open && self.panels.focus_handle(cx).is_focused(window);
        match handled {
            Handled::Pass if panels_focused => {
                // Esc escalates OUT of the panels once they have nothing left
                // to dismiss (Files: viewer -> query; Run: command draft):
                // hand the keyboard back to the tiles, so the NEXT Esc reaches
                // `tiles_key` and can restore a fullscreen tile (N3). Modal
                // surfaces above (kill confirm, name edit, rename) already
                // returned before this point - they keep Esc priority.
                let bare_esc = ks.key == "escape"
                    && !ks.modifiers.control
                    && !ks.modifiers.alt
                    && !ks.modifiers.shift
                    && !ks.modifiers.platform;
                if bare_esc && !self.panels.read(cx).wants_escape() {
                    window.focus(&self.focus);
                }
            }
            Handled::Pass => tiles_key(&mut st, WinKey::Main, ks, cx),
            Handled::Consumed(effects) => self.apply_effects(&mut st, effects, window, cx),
        }
        cx.notify();
    }

    /// Run the view-side follow-ups of a keymap command (the gpui-free
    /// executor already mutated the model): persistence, focus-notify bytes,
    /// pool drops, satellite raising, font re-specs, PTY literals, and the
    /// T-B host seam. Mirrors what the equivalent mouse paths do.
    fn apply_effects(
        &self,
        st: &mut CockpitState,
        effects: Vec<Effect>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for effect in effects {
            match effect {
                Effect::PersistLayout => {
                    st.save_layout();
                    sync_active_sessions(st, &self.feed);
                }
                Effect::FocusChanged { old, new } => {
                    // Model focus is already set; tell terminals that track
                    // focus (mode 1004), like the click-to-focus path.
                    if let Some(old) = old {
                        if let Some(t) = st.tiles.get(&old) {
                            notify_focus(t, false);
                        }
                    }
                    if let Some(t) = st.tiles.get(&new) {
                        notify_focus(t, true);
                    }
                    // Open panels follow the focused tile's project (the
                    // webview panels are per-tile; the side surface tracks
                    // focus instead). Sticky in the feed if the cwd repeats.
                    if st.panels_open {
                        if let Some(cwd) = st.cwds.get(&new) {
                            self.panels_feed.set_root(cwd);
                        }
                    }
                }
                Effect::TileClosed(id) => {
                    // The tile-close `x` path: the model already dropped it.
                    st.drop_tile(&id);
                    st.save_layout();
                    sync_active_sessions(st, &self.feed);
                }
                Effect::RaiseSatellite(wsid) => {
                    if let Some(h) = self.handles.borrow().get(&wsid) {
                        h.update(cx, |_, window, _| window.activate_window()).ok();
                    }
                }
                Effect::FontChanged { tab } => {
                    let Some(t) = st.model.tabs.get(tab) else { continue };
                    let Some(spec) = t.font.clone() else { continue };
                    for id in t.tiles.clone() {
                        if let Some(tile) = st.tiles.get_mut(&id) {
                            tile.set_font_size(spec.size);
                        }
                    }
                }
                Effect::Literal(bytes) => {
                    // Prefix double-tap: type the leader's control byte, with
                    // typed-input semantics (snap to bottom, drop selection).
                    if let Some(id) = win_focused(st, WinKey::Main) {
                        if let Some(tile) = st.tiles.get(&id) {
                            tile.write(&bytes);
                            let mut term = tile.term.lock();
                            term.scroll_to_bottom();
                            term.clear_selection();
                        }
                    }
                }
                Effect::Host(cmd) => {
                    let cwd = win_focused(st, WinKey::Main)
                        .and_then(|id| st.cwds.get(&id).cloned());
                    dispatch_host(&cmd, cwd.as_deref());
                }
                Effect::ConfirmKill(id) => {
                    // Kill is destructive (N4): open the confirm dialog with
                    // the busy verdict resolved now, from the T9 state.
                    let busy = session_busy(&self.feed, &id);
                    st.confirm_kill = Some(ConfirmKill { id, busy });
                }
                Effect::TogglePanels => {
                    st.panels_open = !st.panels_open;
                    if st.panels_open {
                        // Root the panels at the focused tile's project and
                        // hand them the keyboard (Files fuzzy search types
                        // immediately - the panel-window wiring).
                        if let Some(cwd) = win_focused(st, WinKey::Main)
                            .and_then(|id| st.cwds.get(&id).cloned())
                        {
                            self.panels_feed.set_root(&cwd);
                        }
                        window.focus(&self.panels.focus_handle(cx));
                    } else {
                        // Keyboard back to the cockpit (tiles).
                        window.focus(&self.focus);
                    }
                }
            }
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        tiles_scroll(&mut self.state.lock(), WinKey::Main, ev);
        cx.notify();
    }

    /// The confirmed kill (N4): fire the socket `close_terminal` (tmux
    /// kill-session, process tree and all) off-thread, and drop the tile
    /// locally right away - the reconcile pass would prune it within 2s
    /// anyway; this just skips the DEAD-badge flash.
    fn kill_confirmed(&self, st: &mut CockpitState) {
        let Some(ck) = st.confirm_kill.take() else { return };
        let id = ck.id;
        let client = self.client.clone();
        {
            let sid = id.clone();
            thread::spawn(move || {
                match client.request("close_terminal", serde_json::json!({ "sessionId": sid })) {
                    Ok(_) => log::info!("kill: close_terminal accepted for {sid}"),
                    Err(e) => log::warn!("kill: close_terminal failed for {sid}: {e}"),
                }
            });
        }
        st.model.close_tile(&id);
        st.drop_tile(&id);
        st.save_layout();
        sync_active_sessions(st, &self.feed);
    }

    /// The N4 kill confirm dialog, palette-modal styling. The buttons stop
    /// propagation so the root's backdrop-cancel never sees their clicks.
    fn confirm_kill_overlay(&self, id: String, busy: bool, cx: &mut Context<Self>) -> gpui::Div {
        let body = if busy {
            format!(
                "th_{id} looks busy (agent mid-turn). Killing ends the tmux \
session and every process in it."
            )
        } else {
            format!(
                "This kills tmux session th_{id} and every process in it. \
The tile's × only detaches; the session survives that."
            )
        };
        let cancel = cx.listener(|v: &mut Self, _: &MouseDownEvent, _w, cx| {
            v.state.lock().confirm_kill = None;
            cx.stop_propagation();
            cx.notify();
        });
        let kill = cx.listener(|v: &mut Self, _: &MouseDownEvent, _w, cx| {
            {
                let mut st = v.state.lock();
                v.kill_confirmed(&mut st);
            }
            cx.stop_propagation();
            cx.notify();
        });
        div().absolute().inset_0().flex().flex_col().items_center().text_size(px(12.)).child(
            div()
                .mt(px(140.))
                .w(px(480.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(14.))
                .bg(ha(SIDEBAR_BG, 0.97))
                .border_1()
                .border_color(h(ROW_BG_ACTIVE))
                .rounded_md()
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(h(FG))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(SharedString::from("Kill this session?")),
                )
                .child(div().text_color(h(FG_DIM)).child(SharedString::from(body)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(h(FG_DIM))
                                .child(SharedString::from("Enter kill · Esc cancel")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(h(ROW_BG))
                                        .text_color(h(FG))
                                        .cursor_pointer()
                                        .on_mouse_down(MouseButton::Left, cancel)
                                        .child(SharedString::from("Cancel")),
                                )
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(h(DEAD))
                                        .text_color(h(WINDOW_BG))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .cursor_pointer()
                                        .on_mouse_down(MouseButton::Left, kill)
                                        .child(SharedString::from("Kill session")),
                                ),
                        ),
                ),
        )
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

        let (ws_h, panels_open) = {
            let st = self.state.lock();
            (workspace_section_height(st.model.tabs.len()), st.panels_open)
        };
        let metas = gather_tile_meta(&self.feed);
        let focused_window = self.focus.is_focused(window);

        // The T-A keymap overlay (palette / prefix HUD / region pill), built
        // from the controller's state this frame; `None` almost always.
        let key_overlay = {
            let mut st = self.state.lock();
            let now = st.epoch.elapsed().as_millis() as u64;
            palette_view::key_overlay(&mut st.keys, now)
        };

        // The N4 kill confirm dialog, if pending (plain data out of the lock;
        // the overlay builder needs `cx` for its button listeners).
        let confirm = {
            let st = self.state.lock();
            st.confirm_kill.as_ref().map(|c| (c.id.clone(), c.busy))
        };
        let confirm_overlay =
            confirm.map(|(id, busy)| self.confirm_kill_overlay(id, busy, cx));

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
                            paint_tiles_area(
                                &grid_state,
                                WinKey::Main,
                                &metas,
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
            .when(panels_open, |root| {
                // The N5 panels side surface: the T11 PanelHost in a fixed
                // right column (the panel-window bin's width), toggled by the
                // `togglePanels` command. It handles its own keys/clicks.
                root.child(
                    div()
                        .w(px(PANELS_W))
                        .h_full()
                        .border_l_1()
                        .border_color(h(SIDEBAR_BG))
                        .child(self.panels.clone()),
                )
            })
            .children(key_overlay)
            .children(confirm_overlay)
    }
}

// ---------------------------------------------------------------------------
// Satellite windows (T10)
// ---------------------------------------------------------------------------

/// One instrumentation line for the T10 watch item (atlas/memory scaling with
/// window count x visible cells): every window-lifecycle event logs window
/// count, summed painted cells, RSS, and open fd count (PTY attaches are fds,
/// so a stable count across tear/close cycles means no session leaks).
pub fn log_winstat(state: &Arc<Mutex<CockpitState>>, event: &str) {
    let (windows, cells) = {
        let st = state.lock();
        (1 + st.sats.len(), st.visible_cells.values().sum::<u64>())
    };
    let (rss_mb, fds) = crate::render_support::proc_stats();
    log::info!(
        "winstat[{event}]: windows={windows} visible_cells={cells} rss_mb={rss_mb:.1} fds={fds}"
    );
}

/// Tear workspace `tab_idx` off into its own OS window. The sidebar `»` click
/// and the `THN_SAT_CYCLE` harness share this exact path. Returns the wsid.
pub fn tear_off_workspace(
    cx: &mut App,
    state: &Arc<Mutex<CockpitState>>,
    feed: &OverlayFeed,
    handles: &SatHandles,
    tab_idx: usize,
) -> Option<u64> {
    let wsid = {
        let mut st = state.lock();
        let focused_before = st.model.focused.clone();
        let wsid = st.model.tear_off(tab_idx)?;
        // Seed the satellite's focus: the tile the user had focused when it
        // lived in the main window, else the workspace's first tile.
        let i = st.model.tab_by_wsid(wsid).expect("torn-off tab exists");
        let tiles = &st.model.tabs[i].tiles;
        let sat_focus = focused_before
            .filter(|f| tiles.iter().any(|t| t == f))
            .or_else(|| tiles.first().cloned());
        st.sats.open(wsid, sat_focus, None);
        st.save_layout();
        sync_active_sessions(&st, feed);
        wsid
    };
    open_satellite(cx, state, feed, handles, wsid);
    log_winstat(state, "tear-off");
    Some(wsid)
}

/// Return a torn-off workspace to the main window: model + registry + layout +
/// per-window state. The OS window is closed by the CALLER (the OS `x` path is
/// already closing it; the `«`/harness path closes it via the handle).
fn return_workspace_home(st: &mut CockpitState, feed: &OverlayFeed, wsid: u64) -> bool {
    if st.model.close_back(wsid).is_none() {
        return false;
    }
    st.sats.close(wsid);
    st.drop_window_state(WinKey::Sat(wsid));
    st.save_layout();
    sync_active_sessions(st, feed);
    true
}

/// Close a satellite programmatically (the sidebar `«` and the harness): bring
/// the workspace home, then remove the OS window.
pub fn close_satellite_home(
    cx: &mut App,
    state: &Arc<Mutex<CockpitState>>,
    feed: &OverlayFeed,
    handles: &SatHandles,
    wsid: u64,
) {
    {
        let mut st = state.lock();
        return_workspace_home(&mut st, feed, wsid);
    }
    if let Some(h) = handles.borrow_mut().remove(&wsid) {
        h.update(cx, |_, window, _| window.remove_window()).ok();
    }
    log_winstat(state, "close-back");
}

fn to_sat_bounds(b: Bounds<Pixels>) -> SatBounds {
    SatBounds {
        x: b.origin.x.into(),
        y: b.origin.y.into(),
        w: b.size.width.into(),
        h: b.size.height.into(),
    }
}

/// Open the OS window for a torn-off workspace (already flagged in the model
/// and registered in the registry - boot restore and [`tear_off_workspace`]
/// both come through here). On failure the workspace returns to the main
/// window rather than becoming invisible.
pub fn open_satellite(
    cx: &mut App,
    state: &Arc<Mutex<CockpitState>>,
    feed: &OverlayFeed,
    handles: &SatHandles,
    wsid: u64,
) {
    let (title, bounds) = {
        let st = state.lock();
        let name = st
            .model
            .tab_by_wsid(wsid)
            .map(|i| st.model.tabs[i].name.clone())
            .unwrap_or_else(|| format!("workspace {wsid}"));
        (format!("T-Hub - {name}"), st.sats.bounds_of(wsid))
    };
    let window_bounds = match bounds {
        Some(b) => WindowBounds::Windowed(Bounds::new(
            point(px(b.x), px(b.y)),
            size(px(b.w), px(b.h)),
        )),
        None => WindowBounds::Windowed(Bounds::centered(None, size(px(1000.), px(700.)), cx)),
    };

    let build_state = state.clone();
    let build_feed = feed.clone();
    let build_handles = handles.clone();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(title.clone())),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            let focus = cx.focus_handle();
            // The user's OS close button returns the workspace to the main
            // window (record the final bounds first, for the re-tear memo).
            {
                let state = build_state.clone();
                let feed = build_feed.clone();
                let handles = build_handles.clone();
                window.on_window_should_close(cx, move |window, _cx| {
                    let mut st = state.lock();
                    st.sats.set_bounds(wsid, to_sat_bounds(window.bounds()));
                    return_workspace_home(&mut st, &feed, wsid);
                    drop(st);
                    handles.borrow_mut().remove(&wsid);
                    log_winstat(&state, "close-back");
                    true
                });
            }
            window.focus(&focus);
            cx.new(|_| SatelliteView {
                state: build_state,
                feed: build_feed,
                wsid,
                focus,
                title,
            })
        },
    );
    match opened {
        Ok(handle) => {
            handles.borrow_mut().insert(wsid, handle);
        }
        Err(e) => {
            log::error!("satellite window for wsid {wsid} failed to open: {e:#}");
            let mut st = state.lock();
            return_workspace_home(&mut st, feed, wsid);
        }
    }
}

/// A satellite window's content: one torn-off workspace's tile grid, painted
/// from the SAME shared state and attach pool as the main window - its own
/// gpui window (surface + atlas), its own input routing, no sidebar.
pub struct SatelliteView {
    state: Arc<Mutex<CockpitState>>,
    feed: OverlayFeed,
    wsid: u64,
    focus: FocusHandle,
    /// Last title pushed to the OS window (retitled when the workspace is
    /// renamed from the main sidebar).
    title: String,
}

impl SatelliteView {
    fn mouse_down(
        &mut self,
        button: MouseButton,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let mut st = self.state.lock();
            tiles_mouse_down(&mut st, WinKey::Sat(self.wsid), &self.feed, button, ev, cx);
        }
        window.focus(&self.focus);
        cx.notify();
    }
}

impl Focusable for SatelliteView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SatelliteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Continuous repaint like the main window; damage clipping bounds the
        // work. Each window drives its own frame loop.
        window.request_animation_frame();

        let key = WinKey::Sat(self.wsid);
        let retitle = {
            let mut st = self.state.lock();
            // Keep the registry's bounds fresh so any layout save persists
            // where this window currently sits (cheap: two floats compare).
            st.sats.set_bounds(self.wsid, to_sat_bounds(window.bounds()));
            st.model.tab_by_wsid(self.wsid).map(|i| format!("T-Hub - {}", st.model.tabs[i].name))
        };
        if let Some(want) = retitle {
            if want != self.title {
                self.title = want.clone();
                window.set_window_title(&want);
            }
        }

        let metas = gather_tile_meta(&self.feed);
        let focused_window = self.focus.is_focused(window);
        let grid_state = self.state.clone();

        div()
            .size_full()
            .track_focus(&self.focus)
            .bg(h(WINDOW_BG))
            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, _w, cx| {
                tiles_key(&mut v.state.lock(), WinKey::Sat(v.wsid), &ev.keystroke, cx);
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|v, ev, _w, cx| {
                tiles_scroll(&mut v.state.lock(), WinKey::Sat(v.wsid), ev);
                cx.notify();
            }))
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
                cx.listener(|v, ev, _w, cx| {
                    tiles_mouse_up(&mut v.state.lock(), WinKey::Sat(v.wsid), MouseButton::Left, ev);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|v, ev, _w, cx| {
                    tiles_mouse_up(
                        &mut v.state.lock(),
                        WinKey::Sat(v.wsid),
                        MouseButton::Middle,
                        ev,
                    );
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|v, ev, _w, cx| {
                    tiles_mouse_up(
                        &mut v.state.lock(),
                        WinKey::Sat(v.wsid),
                        MouseButton::Right,
                        ev,
                    );
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|v, ev, _w, cx| {
                if tiles_mouse_move(&mut v.state.lock(), WinKey::Sat(v.wsid), ev) {
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        paint_tiles_area(
                            &grid_state,
                            key,
                            &metas,
                            focused_window,
                            bounds,
                            window,
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The merged header's hit routing (N1+N2+N3): the close `x`, the
    /// fullscreen toggle, and the work-name zone each win over the header
    /// strip; the strip (anything above the content box outside those zones)
    /// is the N2 drag handle; the content box is terminal dispatch.
    #[test]
    fn tile_hit_routes_header_buttons_name_drag_and_content() {
        let id = "t1".to_string();
        let zones = TileZones {
            // Tile box (0,0,200,120); content starts below the 20px header.
            tiles: vec![(id.clone(), RectF::new(0.0, 0.0, 200.0, 120.0), RectF::new(5.0, 20.0, 190.0, 95.0))],
            closes: vec![(id.clone(), RectF::new(170.0, 0.0, 30.0, 20.0))],
            fulls: vec![(id.clone(), RectF::new(140.0, 0.0, 30.0, 20.0))],
            badges: Vec::new(),
            names: vec![(id.clone(), RectF::new(60.0, 0.0, 40.0, 20.0))],
            dividers: Vec::new(),
            area: RectF::new(0.0, 0.0, 200.0, 120.0),
        };
        assert!(matches!(tile_hit(&zones, 185.0, 10.0), Some(TileTarget::Close(t)) if t == id));
        assert!(
            matches!(tile_hit(&zones, 155.0, 10.0), Some(TileTarget::Fullscreen(t)) if t == id)
        );
        assert!(matches!(tile_hit(&zones, 70.0, 10.0), Some(TileTarget::Name(t)) if t == id));
        // The bare header strip (left of the name zone) is the drag handle.
        assert!(matches!(tile_hit(&zones, 20.0, 10.0), Some(TileTarget::Header(t)) if t == id));
        // Below the header: terminal dispatch.
        assert!(matches!(tile_hit(&zones, 100.0, 60.0), Some(TileTarget::Tile(t)) if t == id));
        // Outside everything: no target.
        assert!(tile_hit(&zones, 300.0, 60.0).is_none());
    }
}
