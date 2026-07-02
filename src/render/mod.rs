//! **Grid rendering** for the native client (native-pivot T5, the render seam).
//!
//! This is the pivot's core: `PtyHandle` bytes -> [`crate::term::TermSession`] ->
//! damage-driven GPU paint, for N live grids in one GPUI window. The GPUI paint
//! boilerplate (a `canvas` element painting rows with `window.text_system()
//! .shape_line(...)` + `ShapedLine::paint`/`paint_background`, driven by
//! `request_animation_frame`) is lifted from the T2 spike (`gpui-spike/main.rs`),
//! which sustained 12-16 live grids at 90-180fps on gpui 0.2.2 (see
//! `docs/T2-GPUI-SPIKE-RESULTS.md`).
//!
//! ## What T5 adds over the spike
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
//! ## Input (T5 baseline; T6 completes it)
//! Keyboard is encoded to PTY bytes (printable text via `key_char`, plus Enter,
//! Backspace, Tab, arrows, Home/End/PgUp/PgDn, Ctrl-letter, Alt-prefix); a click
//! focuses the tile under the pointer; the wheel scrolls the focused tile's
//! scrollback. Full keymap/mouse-reporting/selection is T6.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, size, App, Bounds, Context, FocusHandle, Focusable, Font, Hsla,
    IntoElement, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, Pixels, Render, Rgba,
    ScrollDelta, ScrollWheelEvent, SharedString, TextRun, Window,
};
use parking_lot::Mutex;

use crate::render_support::{encode, grid_dims, KeyChord};
use crate::term::{Rgb, SelSpan, Snapshot, TermSession, DEFAULT_BG, DEFAULT_FG};
use crate::wire::PtyHandle;

// ---------------------------------------------------------------------------
// Layout / typography constants
// ---------------------------------------------------------------------------

const FONT_SIZE: f32 = 13.0;
const LINE_H: f32 = 16.0;
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
enum PaintMode {
    /// Rebuild only damaged rows (default). The production path.
    Damage,
    /// Rebuild every row every frame (the spike's brute force) - for A/B measurement.
    Full,
}

impl PaintMode {
    fn from_env() -> Self {
        match std::env::var("THN_PAINT").as_deref() {
            Ok("full") => PaintMode::Full,
            _ => PaintMode::Damage,
        }
    }
}

// ---------------------------------------------------------------------------
// Tiles and view state
// ---------------------------------------------------------------------------

/// A pre-shape row: the row text plus its merged style runs. Cached across frames
/// and only rebuilt when the row is damaged. Shaping (`shape_line`) still runs every
/// frame but hits gpui's `LineLayoutCache` on unchanged content, so it is ~free.
#[derive(Clone, Default)]
struct BuiltRow {
    text: SharedString,
    runs: Vec<TextRun>,
}

/// One live terminal tile: its session id, the shared `TermSession` (fed by a
/// background thread), the PTY handle for write/resize, and the per-row run cache.
struct Tile {
    id: String,
    term: Arc<Mutex<TermSession>>,
    pty: PtyHandle,
    built: Vec<BuiltRow>,
    cols: u16,
    rows: u16,
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
    metrics: Option<Metrics>,
    paint_mode: PaintMode,
}

/// Font-derived geometry, probed once from the real window/text system.
#[derive(Clone, Copy)]
struct Metrics {
    font_size: f32,
    line_h: f32,
    cell_w: f32,
}

/// The GPUI view: fonts, focus, shared render state, and a kept-alive control
/// client (so its event stream and the PTY reader threads stay up).
pub struct GridView {
    state: Arc<Mutex<GridState>>,
    font_normal: Font,
    font_bold: Font,
    focus: FocusHandle,
    _client: Arc<crate::wire::ControlClient>,
}

impl GridView {
    /// Build the view from ready-attached tiles. `client` is kept alive for the
    /// process lifetime. Called on the GPUI main thread from [`crate::app`].
    pub fn new(
        tiles: Vec<TileSpec>,
        client: Arc<crate::wire::ControlClient>,
        font_normal: Font,
        font_bold: Font,
        focus: FocusHandle,
    ) -> Self {
        spawn_logger();
        let tiles: Vec<Tile> = tiles
            .into_iter()
            .map(|t| Tile {
                id: t.id,
                term: t.term,
                pty: t.pty,
                built: Vec::new(),
                cols: t.cols,
                rows: t.rows,
            })
            .collect();
        GridView {
            state: Arc::new(Mutex::new(GridState {
                tiles,
                focused: 0,
                tile_rects: Vec::new(),
                metrics: None,
                paint_mode: PaintMode::from_env(),
            })),
            font_normal,
            font_bold,
            focus,
            _client: client,
        }
    }
}

/// What [`crate::app`] hands [`GridView::new`] for each attached session: the id,
/// the shared session, and its PTY handle. Geometry is provisional (resized to fit
/// on the first paint).
pub struct TileSpec {
    pub id: String,
    pub term: Arc<Mutex<TermSession>>,
    pub pty: PtyHandle,
    pub cols: u16,
    pub rows: u16,
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

/// The terminal `cols x rows` that fit inside a tile box, given font metrics.
fn tile_geometry(tile: Bounds<Pixels>, m: &Metrics) -> (u16, u16) {
    let tw: f32 = tile.size.width.into();
    let th: f32 = tile.size.height.into();
    let inner_w = tw - 2.0 * (TILE_PAD + BORDER);
    let inner_h = th - HEADER_H - 2.0 * (TILE_PAD + BORDER);
    let cols = (inner_w / m.cell_w).floor().max(1.0) as u16;
    let rows = (inner_h / m.line_h).floor().max(1.0) as u16;
    (cols.min(400), rows.min(200))
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

fn h(rgb: Rgb) -> Hsla {
    Rgba { r: rgb.r as f32 / 255.0, g: rgb.g as f32 / 255.0, b: rgb.b as f32 / 255.0, a: 1.0 }.into()
}

fn ha(rgb: Rgb, a: f32) -> Hsla {
    Rgba { r: rgb.r as f32 / 255.0, g: rgb.g as f32 / 255.0, b: rgb.b as f32 / 255.0, a }.into()
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

/// Probe font metrics from the real text system (once). cell_w is measured by
/// shaping a run of `M`s and dividing - the honest monospace advance for this font.
fn ensure_metrics(state: &mut GridState, font_normal: &Font, window: &mut Window) {
    if state.metrics.is_some() {
        return;
    }
    let probe: SharedString = "M".repeat(80).into();
    let run = TextRun {
        len: probe.len(),
        font: font_normal.clone(),
        color: h(DEFAULT_FG),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(probe, px(FONT_SIZE), &[run], None);
    let w: f32 = line.width.into();
    let cell_w = (w / 80.0).max(1.0);
    log::info!("render metrics: font={FONT_SIZE}px line_h={LINE_H}px cell_w={cell_w:.3}px");
    state.metrics = Some(Metrics { font_size: FONT_SIZE, line_h: LINE_H, cell_w });
}

/// Build one row's merged style runs from its snapshot cells (the costly transform,
/// mirrored from the spike): consecutive same-style cells collapse into one run.
fn build_row(cells: &[crate::term::SnapCell], font_normal: &Font, font_bold: &Font) -> BuiltRow {
    let mut text = String::with_capacity(cells.len());
    let mut runs: Vec<TextRun> = Vec::new();
    let mut last: Option<(Rgb, Option<Rgb>, bool, bool)> = None;
    for c in cells {
        let style = (c.fg, c.bg, c.bold, c.underline);
        let ch_len = c.c.len_utf8();
        if last == Some(style) && !runs.is_empty() {
            runs.last_mut().unwrap().len += ch_len;
        } else {
            runs.push(TextRun {
                len: ch_len,
                font: if c.bold { font_bold.clone() } else { font_normal.clone() },
                color: h(c.fg),
                background_color: c.bg.map(h),
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
    }
    BuiltRow { text: text.into(), runs }
}

fn paint_grid(
    state: &Arc<Mutex<GridState>>,
    font_normal: &Font,
    font_bold: &Font,
    focused_window: bool,
    win: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let t0 = Instant::now();
    let mut st = state.lock();
    ensure_metrics(&mut st, font_normal, window);
    let m = st.metrics.expect("metrics set");
    let mode = st.paint_mode;
    let focused = st.focused;

    window.paint_quad(fill(win, h(WINDOW_BG)));

    let n = st.tiles.len();
    let mut rects = Vec::with_capacity(n);
    let mut rebuilt_this_frame: u64 = 0;
    let mut total_this_frame: u64 = 0;

    for i in 0..n {
        let tbox = tile_box(i, n, win);
        rects.push(tbox);
        let (want_cols, want_rows) = tile_geometry(tbox, &m);

        // --- geometry: reflow the terminal + PTY to the tile if it changed -----
        {
            let tile = &mut st.tiles[i];
            if (want_cols, want_rows) != (tile.cols, tile.rows) {
                tile.term.lock().resize(want_cols, want_rows);
                tile.pty.resize(want_cols, want_rows);
                tile.cols = want_cols;
                tile.rows = want_rows;
                tile.built.clear(); // force a full rebuild at the new size
            }
        }

        // --- snapshot + damage-clipped run rebuild -----------------------------
        // Clone the term Arc so the lock guard does not borrow `st.tiles[i]`; that
        // lets us mutate `tile.built` while reading the terminal.
        let rows = st.tiles[i].rows as usize;
        total_this_frame += rows as u64;
        let term_arc = st.tiles[i].term.clone();
        let mut term = term_arc.lock();
        let damage = term.take_damage();
        let need_full = mode == PaintMode::Full
            || st.tiles[i].built.len() != rows
            || matches!(damage, crate::term::Damage::Full);

        let (cursor, selection) = if need_full {
            let snap = term.renderable();
            drop(term);
            rebuild_all(&mut st.tiles[i], &snap, font_normal, font_bold);
            rebuilt_this_frame += rows as u64;
            (snap.cursor, snap.selection)
        } else if let crate::term::Damage::Lines(lines) = damage {
            if lines.is_empty() {
                // Nothing changed: fetch only cursor/selection (cheap), no rebuild.
                let partial = term.renderable_rows(&[]);
                (partial.cursor, partial.selection)
            } else {
                let partial = term.renderable_rows(&lines);
                drop(term);
                for (r, cells) in &partial.rows {
                    if *r < st.tiles[i].built.len() {
                        st.tiles[i].built[*r] = build_row(cells, font_normal, font_bold);
                    }
                }
                rebuilt_this_frame += lines.len() as u64;
                (partial.cursor, partial.selection)
            }
        } else {
            (None, None)
        };

        paint_tile(&st.tiles[i], tbox, &m, cursor, selection, i == focused && focused_window, window, cx);
    }

    st.tile_rects = rects;
    drop(st);

    let dt = t0.elapsed().as_nanos() as u64;
    SCENE_NS.fetch_add(dt, Ordering::Relaxed);
    SCENE_NS_MAX.fetch_max(dt, Ordering::Relaxed);
    ROWS_REBUILT.fetch_add(rebuilt_this_frame, Ordering::Relaxed);
    ROWS_TOTAL.fetch_add(total_this_frame, Ordering::Relaxed);
    FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// Rebuild every row's run cache from a full snapshot.
fn rebuild_all(tile: &mut Tile, snap: &Snapshot, font_normal: &Font, font_bold: &Font) {
    tile.built = snap
        .rows_cells
        .iter()
        .map(|cells| build_row(cells, font_normal, font_bold))
        .collect();
    // Guard against a snapshot shorter than the geometry (shouldn't happen).
    tile.built.resize(tile.rows as usize, BuiltRow::default());
}

/// Paint one tile: border, header label, cached rows, cursor, selection.
#[allow(clippy::too_many_arguments)]
fn paint_tile(
    tile: &Tile,
    tbox: Bounds<Pixels>,
    m: &Metrics,
    cursor: Option<crate::term::CursorPos>,
    selection: Option<SelSpan>,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    // Border (a filled rect under a slightly inset background = 1px frame).
    let border_rgb = if focused { BORDER_FOCUS } else { BORDER_RGB };
    window.paint_quad(fill(tbox, h(border_rgb)));
    let inner = Bounds::new(
        point(tbox.origin.x + px(BORDER), tbox.origin.y + px(BORDER)),
        size(tbox.size.width - px(2.0 * BORDER), tbox.size.height - px(2.0 * BORDER)),
    );
    window.paint_quad(fill(inner, h(DEFAULT_BG)));

    let ox = tbox.origin.x + px(BORDER + TILE_PAD);
    let header_y = tbox.origin.y + px(BORDER + 1.0);

    // Debug header (id + geometry) - NOT the T8 cockpit header.
    let label = format!("{}  {}x{}", tile.id, tile.cols, tile.rows);
    paint_text(&label, ox, header_y, h(HEADER_FG), m.font_size, window, cx);

    // Rows.
    let grid_top = tbox.origin.y + px(BORDER + HEADER_H);
    for (i, row) in tile.built.iter().enumerate() {
        if row.runs.is_empty() {
            continue;
        }
        let ry = grid_top + px(i as f32 * m.line_h);
        let origin = point(ox, ry);
        let shaped = window.text_system().shape_line(
            row.text.clone(),
            px(m.font_size),
            &row.runs,
            None,
        );
        shaped.paint_background(origin, px(m.line_h), window, cx).ok();
        shaped.paint(origin, px(m.line_h), window, cx).ok();
    }

    // Selection (minimal linewise highlight; full selection UX is T6).
    if let Some(sel) = selection {
        paint_selection(sel, ox, grid_top, m, tile, window);
    }

    // Cursor: a translucent block over the cell (solid-ish when focused).
    if let Some(cur) = cursor {
        if cur.visible && cur.line < tile.rows as usize && cur.col < tile.cols as usize {
            let cxp = ox + px(cur.col as f32 * m.cell_w);
            let cyp = grid_top + px(cur.line as f32 * m.line_h);
            let alpha = if focused { 0.55 } else { 0.28 };
            window.paint_quad(fill(
                Bounds::new(point(cxp, cyp), size(px(m.cell_w), px(m.line_h))),
                ha(BORDER_FOCUS, alpha),
            ));
        }
    }
}

/// Paint a single text line with one uniform color (used for the debug header).
fn paint_text(
    text: &str,
    x: Pixels,
    y: Pixels,
    color: Hsla,
    font_size: f32,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() {
        return;
    }
    // Header uses the same normal font as the grid; grab it from a 1-run shape.
    let run = TextRun {
        len: text.len(),
        font: gpui::font("Cascadia Mono"),
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
fn chord_of(ks: &Keystroke) -> KeyChord {
    KeyChord {
        control: ks.modifiers.control,
        alt: ks.modifiers.alt,
        shift: ks.modifiers.shift,
        platform: ks.modifiers.platform,
        key: ks.key.clone(),
        key_char: ks.key_char.clone(),
    }
}

impl GridView {
    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(bytes) = encode(&chord_of(&ev.keystroke)) {
            let st = self.state.lock();
            if let Some(tile) = st.tiles.get(st.focused) {
                tile.pty.write(&bytes);
                tile.term.lock().scroll_to_bottom();
            }
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let st = self.state.lock();
        let line_h = st.metrics.map(|m| m.line_h).unwrap_or(LINE_H);
        let dy = match ev.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / line_h,
        };
        let lines = (dy.round() as i32) * 3; // 3 rows per wheel notch
        if lines != 0 {
            if let Some(tile) = st.tiles.get(st.focused) {
                tile.term.lock().scroll(lines);
            }
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mut st = self.state.lock();
        for (i, r) in st.tile_rects.iter().enumerate() {
            if hit(r, ev.position) {
                st.focused = i;
                if let Some(tile) = st.tiles.get(i) {
                    tile.term.lock().scroll_to_bottom();
                }
                break;
            }
        }
        drop(st);
        window.focus(&self.focus);
        cx.notify();
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
        // full-repaint run. (Idle frame-gating is a T6 optimization.)
        window.request_animation_frame();

        let state = self.state.clone();
        let font_normal = self.font_normal.clone();
        let font_bold = self.font_bold.clone();
        let focused_window = self.focus.is_focused(window);

        div()
            .size_full()
            .track_focus(&self.focus)
            .bg(h(WINDOW_BG))
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        paint_grid(
                            &state,
                            &font_normal,
                            &font_bold,
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
// FPS / damage logger (reused from the spike)
// ---------------------------------------------------------------------------

fn log_dir() -> std::path::PathBuf {
    std::env::var("THN_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
}

fn spawn_logger() {
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
