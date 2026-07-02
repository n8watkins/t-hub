//! The GPUI cockpit view (T8): a LEFT SIDEBAR listing the workspaces + the
//! active workspace's tile grid + real per-tile headers, painted over the same
//! canvas/`shape_line` machinery as the T5 grid.
//!
//! Workspace navigation lives in the sidebar (the webview's long-standing
//! design - there is NO top tab strip): switch by clicking a row, create with
//! the "+ new workspace" row, rename by double- or right-clicking a row, close
//! with the row's `x`. Below the workspace section the sidebar reserves the
//! [`crate::chrome::model::SidebarLayout::overlay_mount`] area for the T9
//! sidebar overlay sections (recents / usage / metrics / supervision) - the T8
//! chrome paints nothing there.
//!
//! This file is deliberately a THIN adapter: every decision (which workspace is
//! active, where tiles go, what a click hits, rename editing) lives in the
//! gpui-free [`crate::chrome::model`], and every terminal cell reaching the
//! screen goes through [`crate::render::sync_and_paint_content`] - the row-paint
//! seam T6 owns. What is left here is painting chrome rectangles/labels and
//! translating input events into model calls.
//!
//! ## Input routing
//! - Click: row-close > workspace row (double-click = rename) > `+` row >
//!   tile-close > tile (focus). Right-click a row also starts a rename.
//! - Keys: rename mode captures Enter/Escape/Backspace/printables for the
//!   workspace name buffer; otherwise keystrokes encode to PTY bytes for the
//!   focused tile (which the model guarantees is in the ACTIVE workspace -
//!   never a hidden one).
//! - Wheel: scrollback on the focused tile (same as the T5 grid).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, size, App, Bounds, Context, FocusHandle, Focusable, Font, Hsla,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, TextRun, Window,
};
use parking_lot::Mutex;

use crate::chrome::model::{sidebar_layout, tile_boxes, ChromeModel, RectF};
use crate::chrome::persist::{self, Layout};
use crate::render::{
    h, paint_tile_frame, probe_metrics, record_frame, spawn_logger, sync_and_paint_content,
    Metrics, PaintMode, Tile,
};
use crate::render_support::{encode, KeyChord};
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

const WINDOW_BG: Rgb = Rgb { r: 5, g: 7, b: 10 };
const FG: Rgb = Rgb { r: 216, g: 222, b: 233 };
const FG_DIM: Rgb = Rgb { r: 128, g: 138, b: 154 };
const ACCENT: Rgb = Rgb { r: 128, g: 200, b: 255 };
const SIDEBAR_BG: Rgb = Rgb { r: 11, g: 15, b: 20 };
const ROW_BG: Rgb = Rgb { r: 18, g: 24, b: 31 };
const ROW_BG_ACTIVE: Rgb = Rgb { r: 33, g: 42, b: 54 };
/// Liveness cue: output within the last 2s reads as "busy".
const LIVE: Rgb = Rgb { r: 86, g: 211, b: 128 };
const IDLE_DOT: Rgb = Rgb { r: 70, g: 78, b: 90 };
const BUSY_WINDOW_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Everything the cockpit paints from and the input handlers mutate. Shared
/// (`Arc<Mutex<..>>`) between the GPUI view and the background reconcile worker
/// in [`crate::app`]; like the T5 `GridState`, paint and input run on the GPUI
/// main thread, so the lock is never actually contended by them.
pub struct CockpitState {
    pub(crate) model: ChromeModel,
    /// The persistent attach pool: EVERY tile in EVERY workspace, keyed by
    /// session id. Tab switches only change what is painted; nothing detaches.
    pub(crate) tiles: HashMap<String, Tile>,
    /// Session titles from `list_terminals` (refreshed by the worker).
    pub(crate) titles: HashMap<String, String>,
    /// Per-tile "last output" stamps (ms since `epoch`), written by the feeder
    /// threads - the header's busy/idle liveness cue.
    pub(crate) last_output_ms: HashMap<String, Arc<AtomicU64>>,
    /// Process epoch the stamps are measured from.
    pub(crate) epoch: Instant,
    hits: HitZones,
    metrics: Option<Metrics>,
    paint_mode: PaintMode,
    layout_path: PathBuf,
}

impl CockpitState {
    pub fn new(model: ChromeModel, layout_path: PathBuf) -> Self {
        CockpitState {
            model,
            tiles: HashMap::new(),
            titles: HashMap::new(),
            last_output_ms: HashMap::new(),
            epoch: Instant::now(),
            hits: HitZones::default(),
            metrics: None,
            paint_mode: PaintMode::from_env(),
            layout_path,
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
    }
}

/// Hit zones refreshed by every paint, consumed by mouse handlers. `ws_*` are
/// the sidebar's workspace rows, in workspace order.
#[derive(Default)]
struct HitZones {
    ws_rows: Vec<RectF>,
    ws_closes: Vec<RectF>,
    plus: RectF,
    tiles: Vec<(String, RectF)>,
    tile_closes: Vec<(String, RectF)>,
}

/// What a click landed on, resolved BEFORE mutating the model (the zones and
/// the model live in the same struct, so hit-testing borrows must end first).
enum HitTarget {
    WorkspaceClose(usize),
    Workspace(usize),
    Plus,
    TileClose(String),
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
    for (id, r) in &hits.tiles {
        if r.contains(x, y) {
            return Some(HitTarget::Tile(id.clone()));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// The cockpit window content: tab strip on top, the active workspace's tile
/// grid below. Holds the shared state, fonts, focus, and the kept-alive
/// `ControlClient` (its event stream + PTY readers live for the process).
pub struct CockpitView {
    state: Arc<Mutex<CockpitState>>,
    font_normal: Font,
    font_bold: Font,
    focus: FocusHandle,
    _client: Arc<crate::wire::ControlClient>,
}

impl CockpitView {
    pub fn new(
        state: Arc<Mutex<CockpitState>>,
        client: Arc<crate::wire::ControlClient>,
        font_normal: Font,
        font_bold: Font,
        focus: FocusHandle,
    ) -> Self {
        spawn_logger();
        CockpitView { state, font_normal, font_bold, focus, _client: client }
    }
}

// ---------------------------------------------------------------------------
// Paint helpers
// ---------------------------------------------------------------------------

fn b(r: RectF) -> Bounds<Pixels> {
    Bounds::new(point(px(r.x), px(r.y)), size(px(r.w), px(r.h)))
}

/// Paint one line of styled text parts at (x, y). Monospace, so callers can
/// budget widths as `chars * cell_w`.
#[allow(clippy::too_many_arguments)]
fn paint_parts(
    parts: &[(String, Hsla, bool)],
    x: f32,
    y: f32,
    m: &Metrics,
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
    let shaped = window.text_system().shape_line(
        SharedString::from(text),
        px(m.font_size),
        &runs,
        None,
    );
    shaped.paint(point(px(x), px(y)), px(m.line_h), window, cx).ok();
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

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

fn paint_cockpit(
    state: &Arc<Mutex<CockpitState>>,
    font_normal: &Font,
    font_bold: &Font,
    focused_window: bool,
    win: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let t0 = Instant::now();
    let mut st = state.lock();
    if st.metrics.is_none() {
        st.metrics = Some(probe_metrics(font_normal, window));
    }
    let m = st.metrics.expect("metrics set");
    let mode = st.paint_mode;

    window.paint_quad(fill(win, h(WINDOW_BG)));

    let wx: f32 = win.origin.x.into();
    let wy: f32 = win.origin.y.into();
    let ww: f32 = win.size.width.into();
    let wh: f32 = win.size.height.into();

    // Split-borrow the state so the tiles map can be mutated while the model,
    // titles, and stamps are read (all disjoint fields of one MutexGuard).
    let CockpitState { model, tiles, titles, last_output_ms, epoch, hits, .. } = &mut *st;
    let now_ms = epoch.elapsed().as_millis() as u64;

    // --- the left sidebar: workspace list, then the T9 overlay mount ---------
    let labels: Vec<String> = model
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| match &model.renaming {
            Some(r) if r.tab == i => format!("{}_", r.buffer),
            _ => t.name.clone(),
        })
        .collect();

    window.paint_quad(fill(b(RectF::new(wx, wy, SIDEBAR_W, wh)), h(SIDEBAR_BG)));
    paint_parts(
        &[("WORKSPACES".to_string(), h(FG_DIM), false)],
        wx + PAD + 4.0,
        wy + PAD + 2.0,
        &m,
        font_normal,
        font_bold,
        window,
        cx,
    );
    let ws_area = RectF::new(
        wx + PAD,
        wy + PAD + m.line_h + 8.0,
        SIDEBAR_W - 2.0 * PAD,
        wh - (PAD + m.line_h + 8.0) - PAD,
    );
    let sb = sidebar_layout(labels.len(), ws_area);
    let row_dy = (crate::chrome::model::SIDEBAR_ROW_H - m.line_h) / 2.0;

    for (i, row) in sb.rows.iter().enumerate() {
        let active = i == model.active;
        window.paint_quad(fill(b(*row), h(if active { ROW_BG_ACTIVE } else { ROW_BG })));
        if active {
            // Accent edge bar marks the active workspace.
            window.paint_quad(fill(b(RectF::new(row.x, row.y, 3.0, row.h)), h(ACCENT)));
        }
        let renaming_this = matches!(&model.renaming, Some(r) if r.tab == i);
        let label_color = if renaming_this {
            h(ACCENT)
        } else if active {
            h(FG)
        } else {
            h(FG_DIM)
        };
        let label_cells =
            (((row.w - 14.0 - sb.closes[i].w) / m.cell_w).floor() as usize).max(4);
        paint_parts(
            &[(truncate(&labels[i], label_cells), label_color, active)],
            row.x + 10.0,
            row.y + row_dy,
            &m,
            font_normal,
            font_bold,
            window,
            cx,
        );
        paint_parts(
            &[("×".to_string(), h(FG_DIM), false)],
            sb.closes[i].x + (sb.closes[i].w - m.cell_w) / 2.0,
            row.y + row_dy,
            &m,
            font_normal,
            font_bold,
            window,
            cx,
        );
    }
    paint_parts(
        &[("+ new workspace".to_string(), h(FG_DIM), false)],
        sb.plus.x + 10.0,
        sb.plus.y + row_dy,
        &m,
        font_normal,
        font_bold,
        window,
        cx,
    );
    // A hairline under the workspace section; everything below (`sb.overlay_mount`)
    // is the T9 overlay sections' mount area - deliberately left unpainted.
    window.paint_quad(fill(
        b(RectF::new(sb.overlay_mount.x, sb.overlay_mount.y, sb.overlay_mount.w, 1.0)),
        h(ROW_BG_ACTIVE),
    ));

    // --- the active workspace's tile grid (right of the sidebar) -------------
    let area = RectF::new(
        wx + SIDEBAR_W + PAD,
        wy + PAD,
        ww - SIDEBAR_W - 2.0 * PAD,
        wh - 2.0 * PAD,
    );
    let ids: Vec<String> = model.active_tiles().to_vec();
    let focused_id = model.focused.clone();

    let mut tile_hits = Vec::with_capacity(ids.len());
    let mut close_hits = Vec::with_capacity(ids.len());
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
            &m,
            font_normal,
            font_bold,
            window,
            cx,
        );
    }

    for (id, bx) in ids.iter().zip(tile_boxes(ids.len(), area, GAP)) {
        let is_focused = focused_window && focused_id.as_deref() == Some(id.as_str());
        paint_tile_frame(b(bx), is_focused, window);

        // Close zone in the header's right corner.
        let close_w = m.cell_w + 10.0;
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
            let (rebuilt, total) = sync_and_paint_content(
                tile,
                b(content),
                &m,
                mode,
                is_focused,
                font_normal,
                font_bold,
                window,
                cx,
            );
            rebuilt_frame += rebuilt;
            total_frame += total;
            geom = format!("{}x{}", tile.cols, tile.rows);
        } else {
            paint_parts(
                &[("attaching…".to_string(), h(FG_DIM), false)],
                content.x,
                content.y,
                &m,
                font_normal,
                font_bold,
                window,
                cx,
            );
        }

        // --- the cockpit header: liveness dot, title, id, geometry, close ----
        let busy = last_output_ms
            .get(id)
            .map(|s| now_ms.saturating_sub(s.load(Ordering::Relaxed)) < BUSY_WINDOW_MS)
            .unwrap_or(false);
        let title = titles.get(id).filter(|t| !t.is_empty()).cloned().unwrap_or_else(|| id.clone());

        let avail = (((bx.w - 2.0 * (BORDER + TILE_PAD) - close_w) / m.cell_w).floor() as usize)
            .saturating_sub(2); // the dot
        let meta = if title == *id { format!("  {geom}") } else { format!("  {id}  {geom}") };
        let (title_text, meta_text) = if avail > meta.chars().count() + 8 {
            (truncate(&title, avail - meta.chars().count()), meta)
        } else {
            (truncate(&title, avail), String::new())
        };
        paint_parts(
            &[
                ("● ".to_string(), if busy { h(LIVE) } else { h(IDLE_DOT) }, false),
                (title_text, if is_focused { h(ACCENT) } else { h(FG) }, true),
                (meta_text, h(FG_DIM), false),
            ],
            bx.x + BORDER + TILE_PAD,
            bx.y + BORDER + (HEADER_H - m.line_h) / 2.0 + 1.0,
            &m,
            font_normal,
            font_bold,
            window,
            cx,
        );
        paint_parts(
            &[("×".to_string(), h(FG_DIM), false)],
            close.x + (close.w - m.cell_w) / 2.0,
            bx.y + BORDER + (HEADER_H - m.line_h) / 2.0 + 1.0,
            &m,
            font_normal,
            font_bold,
            window,
            cx,
        );

        tile_hits.push((id.clone(), bx));
        close_hits.push((id.clone(), close));
    }

    *hits = HitZones {
        ws_rows: sb.rows,
        ws_closes: sb.closes,
        plus: sb.plus,
        tiles: tile_hits,
        tile_closes: close_hits,
    };
    drop(st);

    record_frame(t0.elapsed().as_nanos() as u64, rebuilt_frame, total_frame);
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

impl CockpitView {
    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let x: f32 = ev.position.x.into();
        let y: f32 = ev.position.y.into();
        let mut st = self.state.lock();
        let target = hit_test(&st.hits, x, y);
        match target {
            Some(HitTarget::WorkspaceClose(i)) => {
                if let Some(removed) = st.model.close_tab(i) {
                    for id in &removed {
                        st.drop_tile(id);
                    }
                    st.save_layout();
                }
            }
            Some(HitTarget::Workspace(i)) => {
                if ev.click_count >= 2 {
                    st.model.begin_rename(i);
                } else {
                    st.model.commit_rename();
                    st.model.set_active(i);
                    st.save_layout();
                }
            }
            Some(HitTarget::Plus) => {
                st.model.add_tab();
                st.save_layout();
            }
            Some(HitTarget::TileClose(id)) => {
                if st.model.close_tile(&id) {
                    st.drop_tile(&id);
                    st.save_layout();
                }
            }
            Some(HitTarget::Tile(id)) => {
                st.model.commit_rename();
                st.model.set_focused(&id);
                if let Some(tile) = st.tiles.get(&id) {
                    tile.term.lock().scroll_to_bottom();
                }
            }
            None => {
                st.model.commit_rename();
            }
        }
        drop(st);
        window.focus(&self.focus);
        cx.notify();
    }

    /// Right-click a workspace row to rename it (double-click works too).
    fn on_right_down(&mut self, ev: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let x: f32 = ev.position.x.into();
        let y: f32 = ev.position.y.into();
        let mut st = self.state.lock();
        if let Some(HitTarget::Workspace(i)) = hit_test(&st.hits, x, y) {
            st.model.begin_rename(i);
            cx.notify();
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

        // Terminal input: encode to the focused tile's PTY.
        let chord = KeyChord {
            control: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            key: ks.key.clone(),
            key_char: ks.key_char.clone(),
        };
        if let Some(bytes) = encode(&chord) {
            if let Some(id) = st.model.focused.clone() {
                if let Some(tile) = st.tiles.get(&id) {
                    tile.pty.write(&bytes);
                    tile.term.lock().scroll_to_bottom();
                }
            }
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let st = self.state.lock();
        let line_h = st.metrics.map(|m| m.line_h).unwrap_or(16.0);
        let dy = match ev.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / line_h,
        };
        let lines = (dy.round() as i32) * 3; // 3 rows per wheel notch
        if lines != 0 {
            if let Some(id) = &st.model.focused {
                if let Some(tile) = st.tiles.get(id) {
                    tile.term.lock().scroll(lines);
                }
            }
        }
    }
}

impl Focusable for CockpitView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CockpitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Continuous repaint like the T5 grid; damage clipping bounds the work
        // (idle frame-gating remains a T6 optimization).
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
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        paint_cockpit(
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
