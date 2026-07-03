//! **Grid rendering** for the native client (native-pivot T5, the render seam;
//! terminal UX completed by T6).
//!
//! This is the pivot's core: `PtyHandle` bytes -> [`crate::term::TermSession`] ->
//! damage-driven GPU paint, for N live grids in one GPUI window. The GPUI paint
//! boilerplate (a `canvas` element painting rows with `window.text_system()
//! .shape_line(...)` + `ShapedLine::paint`/`paint_background`, driven by
//! `request_animation_frame`) is lifted from the T2 spike (`gpui-spike/main.rs`),
//! which sustained 12-16 live grids at 90-180fps on gpui 0.2.2 (see
//! `docs/T2-GPUI-SPIKE-RESULTS.md`).
//!
//! ## What T5 added over the spike
//! 1. **Real sessions.** Grids are fed by [`crate::wire::PtyHandle`] output frames,
//!    not synthetic ANSI. One feeder thread per tile drains `output` into the tile's
//!    `TermSession`.
//! 2. **A near-square tile layout** sized to the window, deriving each tile's
//!    `cols x rows` from its pixel box (so window-resize reflows every terminal, and
//!    the PTY is resized to match). No cockpit chrome - that is T8; a one-line dim
//!    header per tile (id + geometry) is a debug aid, explicitly NOT the T8 header.
//! 3. **Damage-clipped painting.** Each frame, [`TermSession::take_damage`] tells us
//!    which rows changed; only those rows get their cell->`TextRun` transform rebuilt
//!    (the costly part), while unchanged rows reuse the cached run vector. gpui's
//!    internal `LineLayoutCache` already dedupes the *shaping* of repeated lines
//!    (§1.5), so we do NOT keep our own shaped-line cache - only the pre-shape run
//!    build is cached. A `THN_PAINT=full` mode rebuilds every row every frame (the
//!    spike's brute force) so the two can be measured side by side.
//! 4. **Per-frame budget instrumentation** (reused from the spike's fps logger):
//!    fps, scene-build ms, and rows-rebuilt-vs-rows-total per second, to
//!    `THN_LOG_DIR/render.log`.
//!
//! ## Terminal UX (T6, alacritty semantics)
//! - **Keyboard:** full xterm encoding via [`crate::render_support::key_action`]
//!   (app-cursor SS3, modified arrows, F-keys, AltGr); Ctrl+Shift+C/V copy/paste
//!   (bracketed-paste aware), Ctrl+Shift+F find; Shift+PageUp/Down + Shift+Home/End
//!   page the scrollback (handed to the app instead when the alt screen is active).
//! - **Mouse:** click focuses a tile; selection by drag (double=word, triple=line,
//!   ctrl+drag=block, shift+click extends); mouse-reporting passthrough (X10/UTF-8/
//!   SGR; press/release/drag/hover-motion/wheel) when the app asks, with Shift
//!   overriding for selection; wheel scrolls scrollback, or sends arrows in the
//!   alt screen (alternate-scroll mode), or reports to the app; middle-click pastes.
//! - **Scrollback UX:** a click-to-snap "scrolled back" badge; typing/paste snaps
//!   to the live bottom; output while scrolled back keeps the viewport pinned to
//!   its content (alacritty semantics - see the §5 T6 note on the brief's wording).
//! - **Find:** a per-tile find bar overlay (smart-case literal search over the
//!   whole scrollback), visible-match + focused-match highlights, enter/shift+enter
//!   to cycle with wraparound, match ordinal/total readout.
//! - **URLs:** grid-scanned links (D5 client plane), always subtly underlined;
//!   Ctrl+click opens in the system browser.
//!
//! ## Font subsystem (T7)
//! - **Segmented painting.** Rows are no longer one shaped line: [`crate::font::
//!   segment_cells`] splits each row into independently positioned segments so
//!   every cell paints at exactly `col * cell_w` (rationale in `font/`'s module
//!   doc - fallback glyph advances must never push neighbors off the grid).
//! - **Procedural sprites.** Box-drawing / block / Powerline cells paint as
//!   quads from [`crate::font::sprites`], never through a font (§1.5 frozen).
//! - **Cell backgrounds as quads.** SGR backgrounds are painted as explicit
//!   grid-column quads (merged spans), not via `TextRun.background_color`, so
//!   bg coverage is exact even under fallback glyphs with off-grid advances.
//! - **Per-tile fonts.** Each tile carries a [`crate::font::FontSpec`] (family,
//!   size, ligatures) resolved to its own gpui fonts + measured [`Metrics`];
//!   `THN_FONT` overrides the default. Emoji/symbol fallback families are
//!   attached to every tile font.
//! - **Wide cells.** The cursor covers the full width of a wide char; column
//!   accounting matches the T6 math (both derive from alacritty's flags).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, size, App, Bounds, ClipboardItem, Context, FocusHandle,
    Focusable, Font, FontFallbacks, FontFeatures, FontWeight, Hsla, IntoElement, KeyDownEvent,
    Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, Rgba,
    ScrollDelta, ScrollWheelEvent, SharedString, TextRun, Window,
};
use parking_lot::Mutex;

use crate::font::{self, sprites, FontSpec, SegKind};
use crate::render_support::{
    alt_scroll_bytes, cell_from_pixel, encode_mouse, encode_paste, grid_dims, key_action,
    search_key, CellHit, KeyAction, KeyChord, MouseKind, SearchKey, MOUSE_NO_BUTTON,
    MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
};
use crate::term::{
    grid_span_segs, CursorPos, Rgb, SearchHit, SelKind, SelSpan, Snapshot, TermSession, UrlSpan,
    DEFAULT_BG, DEFAULT_FG,
};
use crate::wire::PtyHandle;

// ---------------------------------------------------------------------------
// Layout / typography constants
// ---------------------------------------------------------------------------

const OUTER_PAD: f32 = 6.0;
const GAP: f32 = 6.0;
const TILE_PAD: f32 = 4.0;
/// One dim line per tile for the debug id/geometry label (NOT the T8 header).
const HEADER_H: f32 = 15.0;
const BORDER: f32 = 1.0;

const HEADER_FG: Rgb = Rgb { r: 128, g: 138, b: 154 };
const BORDER_RGB: Rgb = Rgb { r: 48, g: 54, b: 61 };
const BORDER_FOCUS: Rgb = Rgb { r: 128, g: 200, b: 255 };
const WINDOW_BG: Rgb = Rgb { r: 5, g: 7, b: 10 };
/// T6 overlay colors: link underline, search highlights, find bar.
const URL_FG: Rgb = Rgb { r: 110, g: 168, b: 254 };
const SEARCH_HL: Rgb = Rgb { r: 210, g: 170, b: 60 };
const SEARCH_FOCUS_HL: Rgb = Rgb { r: 255, g: 140, b: 0 };
const SEARCHBAR_BG: Rgb = Rgb { r: 22, g: 27, b: 34 };

// ---------------------------------------------------------------------------
// Per-frame instrumentation (reused from the spike's fps logger)
// ---------------------------------------------------------------------------

static FRAMES: AtomicU64 = AtomicU64::new(0);
static SCENE_NS: AtomicU64 = AtomicU64::new(0);
static SCENE_NS_MAX: AtomicU64 = AtomicU64::new(0);
/// Rows whose run vector was actually rebuilt this interval (the damage-clipped
/// work). Under `THN_PAINT=full` this equals ROWS_TOTAL.
static ROWS_REBUILT: AtomicU64 = AtomicU64::new(0);
/// Rows a full repaint *would* rebuild this interval (every visible row, every
/// frame). ROWS_REBUILT / ROWS_TOTAL is the damage-clipping win in one run.
static ROWS_TOTAL: AtomicU64 = AtomicU64::new(0);
static LOGGER: OnceLock<()> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaintMode {
    /// Rebuild only damaged rows (default). The production path.
    Damage,
    /// Rebuild every row every frame (the spike's brute force) - for A/B measurement.
    Full,
}

impl PaintMode {
    pub(crate) fn from_env() -> Self {
        match std::env::var("THN_PAINT").as_deref() {
            Ok("full") => PaintMode::Full,
            _ => PaintMode::Damage,
        }
    }
}

// ---------------------------------------------------------------------------
// Tiles and view state
// ---------------------------------------------------------------------------

/// A pre-shape row (T7): merged background spans plus positioned segments.
/// Cached across frames and only rebuilt when the row is damaged. Shaping
/// (`shape_line`) still runs every frame but hits gpui's `LineLayoutCache` on
/// unchanged content, so it is ~free; sprite rects are precomputed here.
#[derive(Clone, Default)]
struct BuiltRow {
    /// Non-default SGR backgrounds as merged `(col, n_cols, color)` spans,
    /// painted as exact grid quads (never via `TextRun.background_color`).
    bgs: Vec<(usize, usize, Rgb)>,
    segs: Vec<BuiltSeg>,
}

/// One independently positioned slice of a row (see `font/` module doc).
#[derive(Clone)]
enum BuiltSeg {
    /// Shaped text painted at `col * cell_w`.
    Text { col: usize, text: SharedString, runs: Vec<TextRun> },
    /// Procedural cells (box drawing / blocks / Powerline), quads precomputed.
    Sprite { cells: Vec<SpriteCell> },
}

#[derive(Clone)]
struct SpriteCell {
    col: usize,
    fg: Rgb,
    rects: Vec<sprites::SpriteRect>,
}

/// Find-bar state for one tile (T6). The compiled regex lives in the tile's
/// `TermSession`; this is the UI side: query text, focused hit, ordinal/total.
#[derive(Default)]
struct SearchUi {
    open: bool,
    query: String,
    hit: Option<SearchHit>,
    stats: (usize, usize),
}

/// One live terminal tile: its session id, the shared `TermSession` (fed by a
/// background thread), the PTY handle for write/resize (`None` for offline
/// fixture tiles, e.g. the font-torture screen), the per-row run cache, the
/// T6 UX state (detected URLs, find bar), and the T7 font state (spec, gpui
/// fonts, measured metrics).
pub(crate) struct Tile {
    pub(crate) id: String,
    pub(crate) term: Arc<Mutex<TermSession>>,
    pty: Option<PtyHandle>,
    built: Vec<BuiltRow>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    urls: Vec<UrlSpan>,
    search: SearchUi,
    spec: FontSpec,
    font_normal: Font,
    font_bold: Font,
    pub(crate) metrics: Option<Metrics>,
    /// When set, a resize re-feeds these bytes into a FRESH TermSession instead
    /// of reflowing (fixture screens must stay pixel-deterministic at any size).
    fixture: Option<Arc<Vec<u8>>>,
}

impl Tile {
    /// A tile at provisional geometry with fonts resolved from `spec` (shared
    /// by the grid's `TileSpec` path and the T8 chrome's attach pool).
    pub(crate) fn new(
        id: String,
        term: Arc<Mutex<TermSession>>,
        pty: Option<PtyHandle>,
        cols: u16,
        rows: u16,
        spec: FontSpec,
        fixture: Option<Arc<Vec<u8>>>,
    ) -> Self {
        let (font_normal, font_bold) = tile_fonts(&spec);
        Tile {
            id,
            term,
            pty,
            built: Vec::new(),
            cols,
            rows,
            urls: Vec::new(),
            search: SearchUi::default(),
            spec,
            font_normal,
            font_bold,
            metrics: None,
            fixture,
        }
    }

    /// Write to the PTY if this tile has one (offline fixture tiles do not).
    pub(crate) fn write(&self, bytes: &[u8]) {
        if let Some(pty) = &self.pty {
            pty.write(bytes);
        }
    }

    /// Whether the attach connection is down and retrying (T24 supervision
    /// cue; `false` for detached/fixture tiles - they have no link to lose).
    pub(crate) fn link_down(&self) -> bool {
        self.pty.as_ref().is_some_and(|p| p.link_down())
    }

    /// Whether the find bar owns the keyboard (the T-A keymap's editable-target
    /// guard: chrome chords stand down while the user types a search query).
    pub(crate) fn search_open(&self) -> bool {
        self.search.open
    }

    /// Re-spec the font size in place (T-A zoom hotkeys). Fonts rebuild and the
    /// metrics + row cache drop, so the next [`sync_and_paint_content`] re-probes
    /// and reflows the PTY through the normal geometry path.
    pub(crate) fn set_font_size(&mut self, size: f32) {
        if (self.spec.size - size).abs() < 0.01 {
            return;
        }
        self.spec.size = size;
        let (normal, bold) = tile_fonts(&self.spec);
        self.font_normal = normal;
        self.font_bold = bold;
        self.metrics = None;
        self.built.clear();
    }

    /// Drop the PTY attach but keep the tile (grid content stays painted).
    /// The T24 dead-tile path: a session known-gone from `list_terminals` must
    /// stop its reader's futile reconnect churn, while the tile lingers on
    /// screen with its DEAD badge.
    pub(crate) fn take_pty(&mut self) {
        self.pty = None;
    }
}

/// Resolve a [`FontSpec`] into the tile's normal/bold gpui fonts: emoji/symbol
/// fallback families attached, OpenType `calt` dropped when ligatures are off.
fn tile_fonts(spec: &FontSpec) -> (Font, Font) {
    let mut normal = gpui::font(spec.family.clone());
    normal.fallbacks = Some(FontFallbacks::from_fonts(font::fallback_families()));
    if !spec.ligatures {
        normal.features = FontFeatures::disable_ligatures();
    }
    let mut bold = normal.clone();
    bold.weight = FontWeight::BOLD;
    (normal, bold)
}

/// An in-progress mouse drag: either extending a selection or streaming motion
/// reports to the app that owns the button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragMode {
    Select,
    Report(u8),
}

#[derive(Clone, Copy)]
struct Drag {
    tile: usize,
    mode: DragMode,
    last_cell: (usize, usize),
}

/// Mutable render state shared between the paint closure and the input listeners.
/// Behind a `Mutex` only to satisfy the borrow checker (the `canvas` paint closure
/// cannot borrow `&mut self`); it is never actually contended - paint and input
/// both run on the GPUI main thread.
struct GridState {
    tiles: Vec<Tile>,
    focused: usize,
    /// Screen rects of each tile's whole box, refreshed each paint for hit-testing.
    tile_rects: Vec<Bounds<Pixels>>,
    /// Screen rect of each tile's scrolled-back badge (click = snap to bottom).
    badge_rects: Vec<Option<Bounds<Pixels>>>,
    paint_mode: PaintMode,
    drag: Option<Drag>,
    /// Fractional wheel lines carried between events (smooth touchpad scrolling).
    wheel_accum: f32,
    /// Last hover-motion cell reported (mode 1003), as (tile, col, row).
    hover_cell: Option<(usize, usize, usize)>,
}

/// Font-derived geometry, probed per tile from the real window/text system
/// (each tile's [`FontSpec`] yields its own cell box).
#[derive(Clone, Copy)]
pub(crate) struct Metrics {
    pub(crate) font_size: f32,
    pub(crate) line_h: f32,
    pub(crate) cell_w: f32,
}

/// The GPUI view: focus, shared render state, and a kept-alive control client
/// (so its event stream and the PTY reader threads stay up; `None` for offline
/// fixture runs like the font-torture bin).
pub struct GridView {
    state: Arc<Mutex<GridState>>,
    focus: FocusHandle,
    _client: Option<Arc<crate::wire::ControlClient>>,
}

impl GridView {
    /// Build the view from ready-attached tiles. `client` is kept alive for the
    /// process lifetime. Called on the GPUI main thread from [`crate::app`].
    pub fn new(
        tiles: Vec<TileSpec>,
        client: Option<Arc<crate::wire::ControlClient>>,
        focus: FocusHandle,
    ) -> Self {
        spawn_logger();
        let default_spec = FontSpec::from_env();
        let tiles: Vec<Tile> = tiles
            .into_iter()
            .map(|t| {
                let spec = t.font.unwrap_or_else(|| default_spec.clone());
                Tile::new(t.id, t.term, t.pty, t.cols, t.rows, spec, t.fixture)
            })
            .collect();
        GridView {
            state: Arc::new(Mutex::new(GridState {
                tiles,
                focused: 0,
                tile_rects: Vec::new(),
                badge_rects: Vec::new(),
                paint_mode: PaintMode::from_env(),
                drag: None,
                wheel_accum: 0.0,
                hover_cell: None,
            })),
            focus,
            _client: client,
        }
    }
}

/// What [`crate::app`] hands [`GridView::new`] for each attached session: the id,
/// the shared session, and its PTY handle (`None` for an offline fixture tile).
/// Geometry is provisional (resized to fit on the first paint). `font` overrides
/// the tile's [`FontSpec`] (default: `THN_FONT` or Cascadia Mono 13). `fixture`
/// makes resizes re-feed the bytes instead of reflowing (torture screens).
pub struct TileSpec {
    pub id: String,
    pub term: Arc<Mutex<TermSession>>,
    pub pty: Option<PtyHandle>,
    pub cols: u16,
    pub rows: u16,
    pub font: Option<FontSpec>,
    pub fixture: Option<Arc<Vec<u8>>>,
}

// ---------------------------------------------------------------------------
// Layout math
// ---------------------------------------------------------------------------

/// The screen box of tile `i` given the window bounds and tile count.
fn tile_box(i: usize, n: usize, win: Bounds<Pixels>) -> Bounds<Pixels> {
    let (gc, gr) = grid_dims(n);
    let win_w: f32 = win.size.width.into();
    let win_h: f32 = win.size.height.into();
    let ox: f32 = win.origin.x.into();
    let oy: f32 = win.origin.y.into();

    let avail_w = (win_w - 2.0 * OUTER_PAD - (gc as f32 - 1.0) * GAP).max(1.0);
    let avail_h = (win_h - 2.0 * OUTER_PAD - (gr as f32 - 1.0) * GAP).max(1.0);
    let tile_w = avail_w / gc as f32;
    let tile_h = avail_h / gr as f32;

    let cx = (i % gc) as f32;
    let cy = (i / gc) as f32;
    let x = ox + OUTER_PAD + cx * (tile_w + GAP);
    let y = oy + OUTER_PAD + cy * (tile_h + GAP);
    Bounds::new(point(px(x), px(y)), size(px(tile_w), px(tile_h)))
}

/// The content box of a grid tile: inside the border/padding, below the one-line
/// debug header. This is what [`sync_and_paint_content`] derives geometry from.
fn tile_content_box(tbox: Bounds<Pixels>) -> Bounds<Pixels> {
    let tw: f32 = tbox.size.width.into();
    let th: f32 = tbox.size.height.into();
    let x: f32 = tbox.origin.x.into();
    let y: f32 = tbox.origin.y.into();
    Bounds::new(
        point(px(x + BORDER + TILE_PAD), px(y + BORDER + HEADER_H)),
        size(
            px((tw - 2.0 * (TILE_PAD + BORDER)).max(1.0)),
            px((th - HEADER_H - 2.0 * (TILE_PAD + BORDER)).max(1.0)),
        ),
    )
}

/// Resolve a window position to a cell within tile `idx` (grid-relative).
fn tile_cell(st: &GridState, idx: usize, pos: gpui::Point<Pixels>, m: &Metrics) -> CellHit {
    let tbox = st.tile_rects[idx];
    let rel_x = f32::from(pos.x) - (f32::from(tbox.origin.x) + BORDER + TILE_PAD);
    let rel_y = f32::from(pos.y) - (f32::from(tbox.origin.y) + BORDER + HEADER_H);
    cell_from_pixel(rel_x, rel_y, m.cell_w, m.line_h, st.tiles[idx].cols, st.tiles[idx].rows)
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

pub(crate) fn h(rgb: Rgb) -> Hsla {
    Rgba { r: rgb.r as f32 / 255.0, g: rgb.g as f32 / 255.0, b: rgb.b as f32 / 255.0, a: 1.0 }.into()
}

pub(crate) fn ha(rgb: Rgb, a: f32) -> Hsla {
    Rgba { r: rgb.r as f32 / 255.0, g: rgb.g as f32 / 255.0, b: rgb.b as f32 / 255.0, a }.into()
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

/// Probe a tile's font metrics from the real text system (once per tile).
/// cell_w is measured by shaping a run of `M`s and dividing - the honest
/// monospace advance for this tile's font at this tile's size.
fn ensure_metrics(tile: &mut Tile, window: &mut Window) {
    if tile.metrics.is_some() {
        return;
    }
    let probe: SharedString = "M".repeat(80).into();
    let run = TextRun {
        len: probe.len(),
        font: tile.font_normal.clone(),
        color: h(DEFAULT_FG),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let size = tile.spec.size;
    let line = window.text_system().shape_line(probe, px(size), &[run], None);
    let w: f32 = line.width.into();
    let cell_w = (w / 80.0).max(1.0);
    let line_h = tile.spec.line_height();
    log::info!(
        "render metrics[{}]: family={:?} font={size}px line_h={line_h}px cell_w={cell_w:.3}px",
        tile.id,
        tile.spec.family,
    );
    // A missing family silently substitutes a platform default; if that default
    // is proportional, pure-ASCII runs drift off the grid (the T7 bug). Probe a
    // narrow-glyph run against the wide one and warn - metrics stay usable.
    let probe_i: SharedString = "i".repeat(80).into();
    let run_i = TextRun {
        len: probe_i.len(),
        font: tile.font_normal.clone(),
        color: h(DEFAULT_FG),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line_i = window.text_system().shape_line(probe_i, px(size), &[run_i], None);
    let wi: f32 = line_i.width.into();
    if crate::font::looks_proportional(w, wi) {
        log::warn!(
            "render metrics[{}]: family {:?} shaped proportionally (80xM {w:.1}px vs 80xi \
             {wi:.1}px) - the family is probably not installed and the platform substituted \
             one; cells stay grid-aligned but glyphs will look wrong",
            tile.id,
            tile.spec.family,
        );
    }
    tile.metrics = Some(Metrics { font_size: size, line_h, cell_w });
}

/// Build one row's paint plan from its snapshot cells (the costly transform):
/// merged background spans, positioned text segments with merged style runs,
/// and precomputed sprite quads. See the T7 section of the module doc.
fn build_row(
    cells: &[crate::term::SnapCell],
    font_normal: &Font,
    font_bold: &Font,
    m: &Metrics,
) -> BuiltRow {
    // Background spans over grid columns (exact math, independent of shaping).
    let mut bgs: Vec<(usize, usize, Rgb)> = Vec::new();
    let mut col = 0usize;
    for c in cells {
        let w = c.width.max(1) as usize;
        if let Some(bg) = c.bg {
            match bgs.last_mut() {
                Some((c0, n, color)) if *c0 + *n == col && *color == bg => *n += w,
                _ => bgs.push((col, w, bg)),
            }
        }
        col += w;
    }

    // Positioned segments.
    let mut segs: Vec<BuiltSeg> = Vec::new();
    for seg in font::segment_cells(cells) {
        match seg.kind {
            SegKind::Sprite => {
                let mut col = seg.col;
                let mut out = Vec::with_capacity(seg.end - seg.start);
                for c in &cells[seg.start..seg.end] {
                    out.push(SpriteCell {
                        col,
                        fg: c.fg,
                        rects: sprites::sprite_rects(c.c, m.cell_w, m.line_h)
                            .unwrap_or_default(),
                    });
                    col += c.width.max(1) as usize;
                }
                segs.push(BuiltSeg::Sprite { cells: out });
            }
            SegKind::Text => {
                let mut text = String::new();
                let mut runs: Vec<TextRun> = Vec::new();
                let mut last: Option<(Rgb, bool, bool)> = None;
                for c in &cells[seg.start..seg.end] {
                    // bg is painted as grid quads above, so it is not part of
                    // the run style (and never merges/splits runs).
                    let style = (c.fg, c.bold, c.underline);
                    let mut len = c.c.len_utf8();
                    for z in &c.zw {
                        len += z.len_utf8();
                    }
                    if last == Some(style) && !runs.is_empty() {
                        runs.last_mut().unwrap().len += len;
                    } else {
                        runs.push(TextRun {
                            len,
                            font: if c.bold { font_bold.clone() } else { font_normal.clone() },
                            color: h(c.fg),
                            background_color: None,
                            underline: c.underline.then(|| gpui::UnderlineStyle {
                                thickness: px(1.0),
                                color: Some(h(c.fg)),
                                wavy: false,
                            }),
                            strikethrough: None,
                        });
                        last = Some(style);
                    }
                    text.push(c.c);
                    for z in &c.zw {
                        text.push(*z);
                    }
                }
                segs.push(BuiltSeg::Text { col: seg.col, text: text.into(), runs });
            }
        }
    }
    BuiltRow { bgs, segs }
}

/// Everything paint-time about one tile that is NOT the cached rows: cursor,
/// selection, scroll offset (badge), search highlights, find-bar text.
struct TileOverlay {
    cursor: Option<CursorPos>,
    selection: Option<SelSpan>,
    offset: usize,
    search_segs: Vec<(usize, usize, usize)>,
    focus_segs: Vec<(usize, usize, usize)>,
    search_bar: Option<String>,
}

fn paint_grid(
    state: &Arc<Mutex<GridState>>,
    focused_window: bool,
    win: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let t0 = Instant::now();
    let mut st = state.lock();
    let mode = st.paint_mode;
    let focused = st.focused;

    window.paint_quad(fill(win, h(WINDOW_BG)));

    let n = st.tiles.len();
    let mut rects = Vec::with_capacity(n);
    let mut badges: Vec<Option<Bounds<Pixels>>> = Vec::with_capacity(n);
    let mut rebuilt_this_frame: u64 = 0;
    let mut total_this_frame: u64 = 0;

    for i in 0..n {
        let tbox = tile_box(i, n, win);
        rects.push(tbox);
        let is_focused = i == focused && focused_window;
        let tile = &mut st.tiles[i];

        paint_tile_frame(tbox, is_focused, window);
        let paint = sync_and_paint_content(
            tile,
            tile_content_box(tbox),
            mode,
            is_focused,
            window,
            cx,
        );
        rebuilt_this_frame += paint.rebuilt;
        total_this_frame += paint.total;
        badges.push(paint.badge);

        // Debug header (id + geometry + font) - NOT the T8 cockpit header.
        // Painted after the content sync so it shows this frame's geometry; size
        // is capped so a large tile font cannot overflow the fixed header strip.
        let m = tile.metrics.expect("metrics set by sync_and_paint_content");
        let label = format!(
            "{}  {}x{}  {} {}px",
            tile.id, tile.cols, tile.rows, tile.spec.family, tile.spec.size
        );
        let header_size = m.font_size.min(12.0);
        paint_text(
            &label,
            tbox.origin.x + px(BORDER + TILE_PAD),
            tbox.origin.y + px(BORDER + 1.0),
            h(HEADER_FG),
            header_size,
            &tile.font_normal,
            window,
            cx,
        );
    }

    st.tile_rects = rects;
    st.badge_rects = badges;
    drop(st);

    record_frame(t0.elapsed().as_nanos() as u64, rebuilt_this_frame, total_this_frame);
}

/// Record one painted frame in the fps/damage counters (grid and chrome paints
/// both report here; [`spawn_logger`] drains the counters once a second).
pub(crate) fn record_frame(scene_ns: u64, rebuilt_rows: u64, total_rows: u64) {
    SCENE_NS.fetch_add(scene_ns, Ordering::Relaxed);
    SCENE_NS_MAX.fetch_max(scene_ns, Ordering::Relaxed);
    ROWS_REBUILT.fetch_add(rebuilt_rows, Ordering::Relaxed);
    ROWS_TOTAL.fetch_add(total_rows, Ordering::Relaxed);
    FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// What one [`sync_and_paint_content`] call did: rows rebuilt vs total (for the
/// fps logger) and the scrolled-back badge's screen rect (for click hit-testing).
pub(crate) struct ContentPaint {
    pub(crate) rebuilt: u64,
    pub(crate) total: u64,
    pub(crate) badge: Option<Bounds<Pixels>>,
}

/// The row-paint seam: sync one tile's terminal to its `content` box (per-tile
/// font metrics, geometry reflow incl. the fixture path, damage-clipped run
/// rebuild) and paint its content (bg spans, text segments, sprites, selection,
/// search highlights, URL underlines, cursor, scrolled-back badge, find bar).
/// The T5 grid and the T8 chrome both paint tile content EXCLUSIVELY through
/// this function; its internals are T6/T7 territory.
pub(crate) fn sync_and_paint_content(
    tile: &mut Tile,
    content: Bounds<Pixels>,
    mode: PaintMode,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) -> ContentPaint {
    // Per-tile font metrics (T7: each tile can carry its own family/size).
    ensure_metrics(tile, window);
    let m = tile.metrics.expect("metrics set");
    let font_n = tile.font_normal.clone();
    let font_b = tile.font_bold.clone();

    // --- geometry: reflow the terminal + PTY to the content box if changed ----
    let cw: f32 = content.size.width.into();
    let ch: f32 = content.size.height.into();
    let want_cols = (((cw / m.cell_w).floor().max(1.0)) as u16).min(400);
    let want_rows = (((ch / m.line_h).floor().max(1.0)) as u16).min(200);
    if (want_cols, want_rows) != (tile.cols, tile.rows) {
        if let Some(fx) = tile.fixture.clone() {
            // Fixture tiles re-feed from scratch: alacritty's reflow would
            // scramble the deterministic torture art. Land at the top so the
            // screen reads first-section-first.
            let mut fresh = TermSession::new(want_cols, want_rows);
            fresh.advance(&fx);
            fresh.scroll_to_top();
            *tile.term.lock() = fresh;
        } else {
            tile.term.lock().resize(want_cols, want_rows);
        }
        if let Some(pty) = &tile.pty {
            pty.resize(want_cols, want_rows);
        }
        tile.cols = want_cols;
        tile.rows = want_rows;
        tile.built.clear(); // force a full rebuild at the new size
    }

    // --- snapshot + damage-clipped run rebuild --------------------------------
    // Clone the term Arc so the lock guard borrows the local Arc, not `tile`;
    // that lets us mutate the tile while reading the terminal.
    let rows = tile.rows as usize;
    let term_arc = tile.term.clone();
    let mut term = term_arc.lock();
    let damage_lines = match term.take_damage() {
        crate::term::Damage::Full => None,
        crate::term::Damage::Lines(lines) => Some(lines),
    };
    let need_full =
        mode == PaintMode::Full || tile.built.len() != rows || damage_lines.is_none();

    let (cursor, selection, rebuilt) = if need_full {
        let snap = term.renderable();
        rebuild_all(tile, &snap, &font_n, &font_b, &m);
        (snap.cursor, snap.selection, rows as u64)
    } else {
        // `need_full` already covered the full-damage (None) case.
        let lines = damage_lines.unwrap_or_default();
        let partial = term.renderable_rows(&lines);
        for (r, cells) in &partial.rows {
            if *r < tile.built.len() {
                tile.built[*r] = build_row(cells, &font_n, &font_b, &m);
            }
        }
        (partial.cursor, partial.selection, lines.len() as u64)
    };

    // --- T6 overlay data, gathered while the terminal lock is held ------------
    if rebuilt > 0 {
        // Content changed (or scrolled: scroll_display damages fully), so the
        // visible URL set may have moved. A pure scan, far cheaper than the
        // run rebuild that gates it.
        tile.urls = term.visible_urls();
    }
    let offset = term.display_offset();
    let (search_segs, focus_segs, search_bar) = {
        let ui = &tile.search;
        if ui.open {
            let segs = if ui.query.is_empty() { Vec::new() } else { term.visible_search_hits() };
            let focus = ui
                .hit
                .map(|hit| grid_span_segs(hit.start, hit.end, offset, rows, want_cols as usize))
                .unwrap_or_default();
            let bar = if ui.query.is_empty() {
                "find: (type to search)   enter=next  shift+enter=prev  esc=close".to_string()
            } else {
                format!("find: {}   {}/{}", ui.query, ui.stats.0, ui.stats.1)
            };
            (segs, focus, Some(bar))
        } else {
            (Vec::new(), Vec::new(), None)
        }
    };
    drop(term);

    let overlay = TileOverlay { cursor, selection, offset, search_segs, focus_segs, search_bar };
    let badge = paint_content(tile, content, &m, &overlay, focused, window, cx);
    ContentPaint { rebuilt, total: rows as u64, badge }
}

/// Rebuild every row's run cache from a full snapshot.
fn rebuild_all(tile: &mut Tile, snap: &Snapshot, font_normal: &Font, font_bold: &Font, m: &Metrics) {
    tile.built = snap
        .rows_cells
        .iter()
        .map(|cells| build_row(cells, font_normal, font_bold, m))
        .collect();
    // Guard against a snapshot shorter than the geometry (shouldn't happen).
    tile.built.resize(tile.rows as usize, BuiltRow::default());
}

/// Paint one tile's frame: a 1px border (focus-colored) under a slightly inset
/// terminal-background fill. Shared by the grid and the T8 chrome; everything
/// inside the frame is painted by [`sync_and_paint_content`] + a header.
pub(crate) fn paint_tile_frame(tbox: Bounds<Pixels>, focused: bool, window: &mut Window) {
    let border_rgb = if focused { BORDER_FOCUS } else { BORDER_RGB };
    window.paint_quad(fill(tbox, h(border_rgb)));
    let inner = Bounds::new(
        point(tbox.origin.x + px(BORDER), tbox.origin.y + px(BORDER)),
        size(tbox.size.width - px(2.0 * BORDER), tbox.size.height - px(2.0 * BORDER)),
    );
    window.paint_quad(fill(inner, h(DEFAULT_BG)));
}

/// Paint one tile's terminal content into its content box: cached rows, T6
/// overlays (selection, search highlights, URL underlines, scrolled-back badge,
/// find bar), cursor. Returns the badge's screen rect (if painted) for click
/// hit-testing. Only called from [`sync_and_paint_content`].
#[allow(clippy::too_many_arguments)]
fn paint_content(
    tile: &Tile,
    content: Bounds<Pixels>,
    m: &Metrics,
    overlay: &TileOverlay,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) -> Option<Bounds<Pixels>> {
    let ox = content.origin.x;

    // Rows (T7): bg spans as exact grid quads, then positioned segments - text
    // shaped per segment at col * cell_w, sprites as precomputed quads.
    let grid_top = content.origin.y;
    for (i, row) in tile.built.iter().enumerate() {
        let ry = grid_top + px(i as f32 * m.line_h);
        for &(col, ncols, bg) in &row.bgs {
            window.paint_quad(fill(
                Bounds::new(
                    point(ox + px(col as f32 * m.cell_w), ry),
                    size(px(ncols as f32 * m.cell_w), px(m.line_h)),
                ),
                h(bg),
            ));
        }
        for seg in &row.segs {
            match seg {
                BuiltSeg::Text { col, text, runs } => {
                    if runs.is_empty() {
                        continue;
                    }
                    let origin = point(ox + px(*col as f32 * m.cell_w), ry);
                    let shaped = window.text_system().shape_line(
                        text.clone(),
                        px(m.font_size),
                        runs,
                        None,
                    );
                    shaped.paint(origin, px(m.line_h), window, cx).ok();
                }
                BuiltSeg::Sprite { cells } => {
                    for sc in cells {
                        let x0 = ox + px(sc.col as f32 * m.cell_w);
                        for r in &sc.rects {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(x0 + px(r.x), ry + px(r.y)),
                                    size(px(r.w), px(r.h)),
                                ),
                                ha(sc.fg, r.alpha),
                            ));
                        }
                    }
                }
            }
        }
    }

    let rows = tile.rows as usize;
    let cols = tile.cols as usize;
    let cell_quad = |row: usize, c0: usize, c1: usize, height: f32, dy: f32| {
        let x = ox + px(c0 as f32 * m.cell_w);
        let y = grid_top + px(row as f32 * m.line_h + dy);
        let w = px(((c1.min(cols.saturating_sub(1)) - c0 + 1) as f32) * m.cell_w);
        Bounds::new(point(x, y), size(w, px(height)))
    };

    // Selection.
    if let Some(sel) = overlay.selection {
        paint_selection(sel, ox, grid_top, m, tile, window);
    }

    // Search highlights: every visible match dim, the focused match strong.
    for &(row, c0, c1) in &overlay.search_segs {
        if row < rows && c0 <= c1 {
            window.paint_quad(fill(cell_quad(row, c0, c1, m.line_h, 0.0), ha(SEARCH_HL, 0.30)));
        }
    }
    for &(row, c0, c1) in &overlay.focus_segs {
        if row < rows && c0 <= c1 {
            window
                .paint_quad(fill(cell_quad(row, c0, c1, m.line_h, 0.0), ha(SEARCH_FOCUS_HL, 0.45)));
        }
    }

    // URL underlines (always-on affordance; ctrl+click opens).
    for span in &tile.urls {
        for &(row, c0, c1) in &span.segs {
            if row < rows && c0 <= c1 {
                window.paint_quad(fill(
                    cell_quad(row, c0, c1, 1.0, m.line_h - 2.0),
                    ha(URL_FG, 0.7),
                ));
            }
        }
    }

    // Cursor: a translucent block over the cell (solid-ish when focused).
    // Covers both columns of a wide char (T7).
    if let Some(cur) = overlay.cursor {
        if cur.visible && cur.line < rows && cur.col < cols {
            let cxp = ox + px(cur.col as f32 * m.cell_w);
            let cyp = grid_top + px(cur.line as f32 * m.line_h);
            let alpha = if focused { 0.55 } else { 0.28 };
            let cur_w = cur.width.max(1) as f32 * m.cell_w;
            window.paint_quad(fill(
                Bounds::new(point(cxp, cyp), size(px(cur_w), px(m.line_h))),
                ha(BORDER_FOCUS, alpha),
            ));
        }
    }

    // Scrolled-back badge (top-right; click snaps to the live bottom).
    let mut badge_rect = None;
    if overlay.offset > 0 {
        let label = format!("^ {} lines", overlay.offset);
        let w = label.chars().count() as f32 * m.cell_w + 10.0;
        let hgt = m.line_h + 4.0;
        let x = content.origin.x + content.size.width - px(w);
        let y = grid_top;
        let rect = Bounds::new(point(x, y), size(px(w), px(hgt)));
        window.paint_quad(fill(rect, ha(BORDER_RGB, 0.92)));
        paint_text(
            &label,
            x + px(5.0),
            y + px(2.0),
            h(BORDER_FOCUS),
            m.font_size,
            &tile.font_normal,
            window,
            cx,
        );
        badge_rect = Some(rect);
    }

    // Find bar (bottom overlay, alacritty-style), spanning the content width.
    if let Some(bar) = &overlay.search_bar {
        let bar_h = m.line_h + 6.0;
        let y = content.origin.y + content.size.height - px(bar_h);
        let rect = Bounds::new(point(content.origin.x, y), size(content.size.width, px(bar_h)));
        window.paint_quad(fill(rect, h(SEARCHBAR_BG)));
        window.paint_quad(fill(
            Bounds::new(point(rect.origin.x, y), size(rect.size.width, px(1.0))),
            ha(BORDER_FOCUS, 0.8),
        ));
        paint_text(
            bar,
            rect.origin.x + px(TILE_PAD + 2.0),
            y + px(3.0),
            h(DEFAULT_FG),
            m.font_size,
            &tile.font_normal,
            window,
            cx,
        );
    }

    badge_rect
}

/// Paint a single text line with one uniform color in the given font (debug
/// header, scrolled-back badge, find bar).
#[allow(clippy::too_many_arguments)]
fn paint_text(
    text: &str,
    x: Pixels,
    y: Pixels,
    color: Hsla,
    font_size: f32,
    font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() {
        return;
    }
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped =
        window.text_system().shape_line(SharedString::from(text.to_string()), px(font_size), &[run], None);
    shaped.paint(point(x, y), px(font_size + 2.0), window, cx).ok();
}

fn paint_selection(
    sel: SelSpan,
    ox: Pixels,
    grid_top: Pixels,
    m: &Metrics,
    tile: &Tile,
    window: &mut Window,
) {
    let cols = tile.cols as usize;
    let rows = tile.rows as usize;
    let (sl, sc) = sel.start;
    let (el, ec) = sel.end;
    for line in sl..=el.min(rows.saturating_sub(1)) {
        let (from, to) = if sel.is_block {
            (sc.min(ec), sc.max(ec))
        } else if line == sl && line == el {
            (sc, ec)
        } else if line == sl {
            (sc, cols.saturating_sub(1))
        } else if line == el {
            (0, ec)
        } else {
            (0, cols.saturating_sub(1))
        };
        let to = to.min(cols.saturating_sub(1));
        if from > to {
            continue;
        }
        let x = ox + px(from as f32 * m.cell_w);
        let y = grid_top + px(line as f32 * m.line_h);
        let w = px(((to - from + 1) as f32) * m.cell_w);
        window.paint_quad(fill(
            Bounds::new(point(x, y), size(w, px(m.line_h))),
            ha(BORDER_FOCUS, 0.25),
        ));
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Adapt a `gpui::Keystroke` into the gpui-free [`KeyChord`] the encoder consumes.
pub(crate) fn chord_of(ks: &Keystroke) -> KeyChord {
    KeyChord {
        control: ks.modifiers.control,
        alt: ks.modifiers.alt,
        shift: ks.modifiers.shift,
        platform: ks.modifiers.platform,
        key: ks.key.clone(),
        key_char: ks.key_char.clone(),
    }
}

/// Map a gpui button to the mouse-reporting button code (None = unreported kind).
pub(crate) fn report_button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

/// The URL under a viewport cell, if any.
fn url_at(urls: &[UrlSpan], row: usize, col: usize) -> Option<String> {
    urls.iter()
        .find(|u| u.segs.iter().any(|&(r, c0, c1)| r == row && (c0..=c1).contains(&col)))
        .map(|u| u.url.clone())
}

/// Open a URL with the platform handler. The scanner only produces
/// http/https/file/ftp, but re-check defensively before shelling out.
/// `pub(crate)`: the T11 panels open preview URLs through the same path.
pub(crate) fn open_url(url: &str) {
    let lower = url.to_ascii_lowercase();
    if !["http://", "https://", "file://", "ftp://"].iter().any(|s| lower.starts_with(s)) {
        return;
    }
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(url).spawn();
    match spawned {
        Ok(_) => log::info!("opened url: {url}"),
        Err(e) => log::warn!("open url {url} failed: {e}"),
    }
}

/// Read the clipboard and write it to the tile's PTY with the right framing
/// (bracketed paste when the app asked for it). Snaps to the live bottom and
/// clears the selection, like typed input.
fn paste_into(tile: &Tile, cx: &App) {
    let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
    if text.is_empty() {
        return;
    }
    let bracketed = {
        let mut term = tile.term.lock();
        term.scroll_to_bottom();
        term.clear_selection();
        term.mode_info().bracketed_paste
    };
    tile.write(&encode_paste(&text, bracketed));
}

/// Re-run the search after a query edit: recompile, jump to the first match from
/// the viewport top, refresh the ordinal/total readout.
fn search_requery(tile: &mut Tile) {
    let mut term = tile.term.lock();
    if !term.set_search(&tile.search.query) {
        tile.search.hit = None;
        tile.search.stats = (0, 0);
        return;
    }
    tile.search.hit = term.find_next(None, true);
    finish_search_step(&mut term, &mut tile.search);
}

/// Advance to the next/previous match (with wraparound) and refresh the readout.
fn search_step(tile: &mut Tile, forward: bool) {
    let mut term = tile.term.lock();
    tile.search.hit = term.find_next(tile.search.hit.as_ref(), forward);
    finish_search_step(&mut term, &mut tile.search);
}

fn finish_search_step(term: &mut TermSession, ui: &mut SearchUi) {
    match &ui.hit {
        Some(hit) => {
            term.scroll_to_line(hit.start.0);
            ui.stats = term.match_stats(hit);
        }
        None => ui.stats = (0, 0),
    }
}

fn close_search(tile: &mut Tile) {
    tile.search = SearchUi::default();
    tile.term.lock().clear_search();
}

// ---------------------------------------------------------------------------
// Per-tile input core (shared by the T5 grid and the T8 chrome; the views are
// thin adapters that resolve WHICH tile/cell an event hits, then call these)
// ---------------------------------------------------------------------------

/// Keyboard input for one tile: the find bar owns the keyboard while open;
/// otherwise the T6 `key_action` switch (write / scroll / copy / paste / search).
pub(crate) fn tile_key_input(tile: &mut Tile, chord: &KeyChord, cx: &mut App) {
    // The find bar owns the keyboard while open on the focused tile.
    if tile.search.open {
        match search_key(chord) {
            SearchKey::Input(text) => {
                tile.search.query.push_str(&text);
                search_requery(tile);
            }
            SearchKey::Backspace => {
                tile.search.query.pop();
                search_requery(tile);
            }
            SearchKey::Next => search_step(tile, true),
            SearchKey::Prev => search_step(tile, false),
            SearchKey::Close => close_search(tile),
            SearchKey::Ignore => {}
        }
        return;
    }

    let mode_info = tile.term.lock().mode_info();
    match key_action(chord, &mode_info) {
        KeyAction::Write(bytes) => {
            tile.write(&bytes);
            let mut term = tile.term.lock();
            term.scroll_to_bottom();
            term.clear_selection();
        }
        KeyAction::ScrollPageUp => tile.term.lock().scroll_page_up(),
        KeyAction::ScrollPageDown => tile.term.lock().scroll_page_down(),
        KeyAction::ScrollTop => tile.term.lock().scroll_to_top(),
        KeyAction::ScrollBottom => tile.term.lock().scroll_to_bottom(),
        KeyAction::Copy => {
            if let Some(text) = tile.term.lock().selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        KeyAction::Paste => paste_into(tile, cx),
        KeyAction::OpenSearch => tile.search.open = true,
        KeyAction::Ignore => {}
    }
}

/// Wheel input for one tile after the view resolved the line count: the app
/// owns the wheel under mouse reporting, alt-screen maps to arrows (mode 1007),
/// otherwise the viewport scrolls.
pub(crate) fn tile_wheel_dispatch(
    tile: &Tile,
    lines: i32,
    cell: CellHit,
    shift: bool,
    alt: bool,
    ctrl: bool,
) {
    let mi = tile.term.lock().mode_info();
    if mi.any_mouse() && !shift {
        // The app owns the wheel: one report per line, at the pointer cell.
        if cell.inside {
            let btn = if lines > 0 { MOUSE_WHEEL_UP } else { MOUSE_WHEEL_DOWN };
            let mods = (false, alt, ctrl);
            let bytes = encode_mouse(MouseKind::Press, btn, (cell.col, cell.row), mods, &mi);
            for _ in 0..lines.unsigned_abs() {
                tile.write(&bytes);
            }
        }
    } else if mi.alt_screen && mi.alternate_scroll && !shift {
        // Alt screen without mouse mode: wheel = arrow keys (mode 1007).
        tile.write(&alt_scroll_bytes(lines, mi.app_cursor));
    } else {
        tile.term.lock().scroll(lines);
    }
}

/// Button-down on a tile cell: ctrl+click URL open, mouse-reporting
/// passthrough (shift overrides for selection), selection start/extend, middle
/// paste. Returns the drag mode the view should track until button-up.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tile_mouse_down_dispatch(
    tile: &Tile,
    cell: CellHit,
    button: MouseButton,
    click_count: usize,
    shift: bool,
    alt: bool,
    ctrl: bool,
    cx: &mut App,
) -> Option<DragMode> {
    let mi = tile.term.lock().mode_info();

    // Ctrl+click on a URL opens it (never forwarded to the app).
    if button == MouseButton::Left && ctrl {
        if let Some(url) = url_at(&tile.urls, cell.row, cell.col) {
            open_url(&url);
            return None;
        }
    }

    // Mouse-reporting passthrough; Shift overrides for selection.
    if mi.any_mouse() && !shift && cell.inside {
        if let Some(btn) = report_button(button) {
            let mods = (false, alt, ctrl);
            tile.write(&encode_mouse(MouseKind::Press, btn, (cell.col, cell.row), mods, &mi));
            return Some(DragMode::Report(btn));
        }
        return None;
    }

    match button {
        MouseButton::Left => {
            let mut term = tile.term.lock();
            if shift && term.has_selection() {
                // Shift+click extends the existing selection.
                term.update_selection(cell.row, cell.col, cell.right_side);
            } else {
                let kind = match (click_count.max(1) - 1) % 3 {
                    0 if ctrl => SelKind::Block,
                    0 => SelKind::Simple,
                    1 => SelKind::Semantic,
                    _ => SelKind::Lines,
                };
                term.start_selection(kind, cell.row, cell.col, cell.right_side);
            }
            Some(DragMode::Select)
        }
        MouseButton::Middle => {
            paste_into(tile, cx);
            None
        }
        _ => None,
    }
}

/// Drag motion for one tile: extend the selection or stream motion reports.
pub(crate) fn tile_drag_motion(tile: &Tile, mode: DragMode, cell: CellHit, alt: bool, ctrl: bool) {
    match mode {
        DragMode::Select => {
            tile.term.lock().update_selection(cell.row, cell.col, cell.right_side);
        }
        DragMode::Report(btn) => {
            let mi = tile.term.lock().mode_info();
            if (mi.mouse_drag || mi.mouse_motion) && cell.inside {
                let mods = (false, alt, ctrl);
                tile.write(&encode_mouse(MouseKind::Motion, btn, (cell.col, cell.row), mods, &mi));
            }
        }
    }
}

/// Button-up ending a report drag: send the release report.
pub(crate) fn tile_report_release(tile: &Tile, btn: u8, cell: CellHit, alt: bool, ctrl: bool) {
    let mi = tile.term.lock().mode_info();
    if mi.any_mouse() {
        let mods = (false, alt, ctrl);
        tile.write(&encode_mouse(MouseKind::Release, btn, (cell.col, cell.row), mods, &mi));
    }
}

/// Buttonless hover motion over a tile cell (mode 1003).
pub(crate) fn tile_hover_motion(tile: &Tile, cell: CellHit, alt: bool, ctrl: bool) {
    let mi = tile.term.lock().mode_info();
    if mi.mouse_motion {
        let mods = (false, alt, ctrl);
        tile.write(&encode_mouse(
            MouseKind::Motion,
            MOUSE_NO_BUTTON,
            (cell.col, cell.row),
            mods,
            &mi,
        ));
    }
}

/// Tell a terminal that tracks focus (mode 1004) it gained/lost focus.
pub(crate) fn notify_focus(tile: &Tile, gained: bool) {
    if tile.term.lock().mode_info().focus_in_out {
        tile.write(if gained { b"\x1b[I" } else { b"\x1b[O" });
    }
}

impl GridView {
    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let chord = chord_of(&ev.keystroke);
        let mut st = self.state.lock();
        let focused = st.focused;
        let Some(tile) = st.tiles.get_mut(focused) else { return };
        tile_key_input(tile, &chord, cx);
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let mut st = self.state.lock();
        // The wheel acts on the tile under the pointer (fallback: the focused one).
        let idx =
            st.tile_rects.iter().position(|r| hit(r, ev.position)).unwrap_or(st.focused);
        if idx >= st.tiles.len() {
            return;
        }
        let Some(m) = st.tiles[idx].metrics else { return };
        let dy = match ev.delta {
            // One wheel notch = 3 rows; pixel deltas (touchpads) map 1:1 by row
            // height, with the fraction carried across events.
            ScrollDelta::Lines(p) => p.y * 3.0,
            ScrollDelta::Pixels(p) => f32::from(p.y) / m.line_h,
        };
        st.wheel_accum += dy;
        let lines = st.wheel_accum as i32;
        if lines == 0 {
            return;
        }
        st.wheel_accum -= lines as f32;

        let cell = tile_cell(&st, idx, ev.position, &m);
        tile_wheel_dispatch(
            &st.tiles[idx],
            lines,
            cell,
            ev.modifiers.shift,
            ev.modifiers.alt,
            ev.modifiers.control,
        );
        cx.notify();
    }

    fn mouse_down(
        &mut self,
        button: MouseButton,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut st = self.state.lock();
        let Some(idx) = st.tile_rects.iter().position(|r| hit(r, ev.position)) else {
            return;
        };

        // Focus follows click; tell the terminals that track focus (mode 1004).
        let old = st.focused;
        if old != idx {
            if let Some(t) = st.tiles.get(old) {
                notify_focus(t, false);
            }
            notify_focus(&st.tiles[idx], true);
            st.focused = idx;
        }

        'actions: {
            let Some(m) = st.tiles[idx].metrics else { break 'actions };

            // Scrolled-back badge: click snaps to the live bottom.
            if button == MouseButton::Left {
                if let Some(Some(badge)) = st.badge_rects.get(idx) {
                    if hit(badge, ev.position) {
                        st.tiles[idx].term.lock().scroll_to_bottom();
                        break 'actions;
                    }
                }
            }

            let cell = tile_cell(&st, idx, ev.position, &m);
            if let Some(mode) = tile_mouse_down_dispatch(
                &st.tiles[idx],
                cell,
                button,
                ev.click_count,
                ev.modifiers.shift,
                ev.modifiers.alt,
                ev.modifiers.control,
                cx,
            ) {
                st.drag = Some(Drag { tile: idx, mode, last_cell: (cell.col, cell.row) });
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
        let Some(drag) = st.drag else { return };
        let ends_drag = match drag.mode {
            DragMode::Select => button == MouseButton::Left,
            DragMode::Report(btn) => report_button(button) == Some(btn),
        };
        if !ends_drag {
            return;
        }
        st.drag = None;
        if let DragMode::Report(btn) = drag.mode {
            if drag.tile < st.tiles.len() {
                if let Some(m) = st.tiles[drag.tile].metrics {
                    let cell = tile_cell(&st, drag.tile, ev.position, &m);
                    tile_report_release(
                        &st.tiles[drag.tile],
                        btn,
                        cell,
                        ev.modifiers.alt,
                        ev.modifiers.control,
                    );
                }
            }
        }
        // DragMode::Select: the selection simply stays; copy is explicit
        // (Ctrl+Shift+C), matching alacritty on non-X11 platforms.
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let mut st = self.state.lock();

        if let Some(drag) = st.drag {
            if drag.tile >= st.tiles.len() {
                st.drag = None;
                return;
            }
            let Some(m) = st.tiles[drag.tile].metrics else { return };
            let cell = tile_cell(&st, drag.tile, ev.position, &m);
            if (cell.col, cell.row) == drag.last_cell {
                return;
            }
            tile_drag_motion(
                &st.tiles[drag.tile],
                drag.mode,
                cell,
                ev.modifiers.alt,
                ev.modifiers.control,
            );
            if drag.mode == DragMode::Select {
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
        let Some(idx) = st.tile_rects.iter().position(|r| hit(r, ev.position)) else {
            st.hover_cell = None;
            return;
        };
        let Some(m) = st.tiles.get(idx).and_then(|t| t.metrics) else { return };
        let cell = tile_cell(&st, idx, ev.position, &m);
        if !cell.inside {
            st.hover_cell = None;
            return;
        }
        if st.hover_cell == Some((idx, cell.col, cell.row)) {
            return;
        }
        st.hover_cell = Some((idx, cell.col, cell.row));
        tile_hover_motion(&st.tiles[idx], cell, ev.modifiers.alt, ev.modifiers.control);
    }
}

fn hit(b: &Bounds<Pixels>, p: gpui::Point<Pixels>) -> bool {
    let x: f32 = p.x.into();
    let y: f32 = p.y.into();
    let ox: f32 = b.origin.x.into();
    let oy: f32 = b.origin.y.into();
    let w: f32 = b.size.width.into();
    let hgt: f32 = b.size.height.into();
    x >= ox && x <= ox + w && y >= oy && y <= oy + hgt
}

impl Focusable for GridView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GridView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Continuous repaint: correctness is trivial and matches the spike; the
        // damage clipping happens in the per-frame work, measured against a
        // full-repaint run. (Idle frame-gating stays a future optimization.)
        window.request_animation_frame();

        let state = self.state.clone();
        let focused_window = self.focus.is_focused(window);

        div()
            .size_full()
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
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        paint_grid(&state, focused_window, bounds, window, cx);
                    },
                )
                .size_full(),
            )
    }
}

// ---------------------------------------------------------------------------
// FPS / damage logger (reused from the spike)
// ---------------------------------------------------------------------------

fn log_dir() -> std::path::PathBuf {
    std::env::var("THN_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
}

pub(crate) fn spawn_logger() {
    if LOGGER.set(()).is_err() {
        return; // already running
    }
    let mode = PaintMode::from_env();
    let bench_secs: Option<u64> = std::env::var("THN_BENCH_SECS").ok().and_then(|s| s.parse().ok());
    thread::spawn(move || {
        use std::io::Write as _;
        let path = log_dir().join("render.log");
        let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                log::error!("render.log open failed: {e}");
                return;
            }
        };
        let mode_s = if mode == PaintMode::Full { "full" } else { "damage" };
        writeln!(f, "--- render run start mode={mode_s} bench_secs={bench_secs:?} ---").ok();

        // Wait for the first painted frame, then zero the counters.
        while FRAMES.load(Ordering::Relaxed) == 0 {
            thread::sleep(Duration::from_millis(50));
        }
        FRAMES.store(0, Ordering::Relaxed);
        SCENE_NS.store(0, Ordering::Relaxed);
        SCENE_NS_MAX.store(0, Ordering::Relaxed);
        ROWS_REBUILT.store(0, Ordering::Relaxed);
        ROWS_TOTAL.store(0, Ordering::Relaxed);

        let start = Instant::now();
        let mut fps_samples: Vec<u64> = Vec::new();
        let mut scene_samples: Vec<f64> = Vec::new();
        let mut rebuilt_sum: u64 = 0;
        let mut total_sum: u64 = 0;
        loop {
            thread::sleep(Duration::from_secs(1));
            let t = start.elapsed().as_secs();
            let frames = FRAMES.swap(0, Ordering::Relaxed);
            let ns = SCENE_NS.swap(0, Ordering::Relaxed);
            let mx = SCENE_NS_MAX.swap(0, Ordering::Relaxed);
            let rebuilt = ROWS_REBUILT.swap(0, Ordering::Relaxed);
            let total = ROWS_TOTAL.swap(0, Ordering::Relaxed);
            let avg_ms = if frames > 0 { ns as f64 / frames as f64 / 1e6 } else { 0.0 };
            let max_ms = mx as f64 / 1e6;
            let pct = if total > 0 { 100.0 * rebuilt as f64 / total as f64 } else { 0.0 };
            let work_ms_per_s = ns as f64 / 1e6; // total scene-build ms spent this wall-second
            writeln!(
                f,
                "t={t:03} mode={mode_s} fps={frames} scene_avg_ms={avg_ms:.2} scene_max_ms={max_ms:.2} rebuilt_rows={rebuilt} total_rows={total} rebuild_pct={pct:.1} work_ms_per_s={work_ms_per_s:.1}"
            )
            .ok();
            f.flush().ok();

            // Collect steady-state samples (skip 2s warmup).
            if t > 2 {
                fps_samples.push(frames);
                scene_samples.push(avg_ms);
                rebuilt_sum += rebuilt;
                total_sum += total;
            }

            if let Some(limit) = bench_secs {
                if t >= limit {
                    let n = fps_samples.len().max(1);
                    let fps_avg = fps_samples.iter().sum::<u64>() as f64 / n as f64;
                    let fps_min = fps_samples.iter().copied().min().unwrap_or(0);
                    let sc_avg = scene_samples.iter().sum::<f64>() / n as f64;
                    let sc_max = scene_samples.iter().cloned().fold(0.0f64, f64::max);
                    let pct = if total_sum > 0 {
                        100.0 * rebuilt_sum as f64 / total_sum as f64
                    } else {
                        0.0
                    };
                    writeln!(
                        f,
                        "SUMMARY mode={mode_s} secs={n} fps_avg={fps_avg:.1} fps_min={fps_min} scene_avg_ms={sc_avg:.2} scene_max_ms={sc_max:.2} rebuild_pct={pct:.1}"
                    )
                    .ok();
                    f.flush().ok();
                    eprintln!(
                        "RENDER-SUMMARY mode={mode_s} fps_avg={fps_avg:.1} fps_min={fps_min} scene_avg_ms={sc_avg:.2} rebuild_pct={pct:.1}"
                    );
                    std::process::exit(0);
                }
            }
        }
    });
}
