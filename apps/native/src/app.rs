//! GPUI window for the native client.
//!
//! Two modes:
//!  - **grid** (default, T5): the render seam - N live terminal tiles fed by the
//!    §1.3 PTY plane, painted by [`crate::render::GridView`]. This is the pivot's
//!    core deliverable.
//!  - **debug** (`THN_DEBUG=1`, the T4 overlay): a scrolling proof that the wire is
//!    live (connection status, session list, event tail, first-attach byte count).
//!
//! GPUI boilerplate (Application, the canvas + `text_system().shape_line` paint loop
//! driven by `request_animation_frame`) is lifted from the T2 spike main.rs, which
//! is known-good on gpui 0.2.2.
//!
//! ## Session selection (T5)
//! `THN_SESSIONS=id1,id2,...` attaches exactly those sessions (used by the
//! acceptance harness with DISPOSABLE `t5-*` sessions, so typing tests never touch
//! sessions we did not create). Unset -> the first `THN_TILES` (default 12) live
//! sessions from `list_terminals`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, font, point, px, size, App, Application, Bounds, Context, Font, FontWeight,
    Hsla, IntoElement, Render, Rgba, SharedString, TextRun, TitlebarOptions, Window, WindowBounds,
    WindowOptions,
};
use parking_lot::Mutex;

use crate::render::{GridView, TileSpec};
use crate::term::TermSession;
use crate::wire::{push_capped, ControlClient, Event, PtyFrame};

/// Provisional attach geometry before the first paint reflows each tile to fit.
const PROVISIONAL_COLS: u16 = 80;
const PROVISIONAL_ROWS: u16 = 24;
/// Default tile count when `THN_SESSIONS` is unset.
const DEFAULT_TILES: usize = 12;

const EVENT_CAP: usize = 400;
const RELIST_SECS: u64 = 3;

/// Everything the debug overlay renders. Updated by the background worker
/// thread(s); read by the GPUI paint loop each frame.
#[derive(Default)]
struct DebugState {
    status: String,
    sessions: Vec<String>,
    attach: String,
    attach_bytes: usize,
    events: VecDeque<String>,
}

struct DebugView {
    state: Arc<Mutex<DebugState>>,
    font_normal: Font,
    font_bold: Font,
}

fn c2h(r: u8, g: u8, b: u8) -> Hsla {
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }.into()
}

const FG: (u8, u8, u8) = (216, 222, 233);
const FG_DIM: (u8, u8, u8) = (128, 138, 154);
const ACCENT: (u8, u8, u8) = (128, 200, 255);
const BG: (u8, u8, u8) = (13, 17, 23);

/// Snapshot the shared state into a flat list of (text, color, bold) lines.
fn compose_lines(state: &DebugState) -> Vec<(String, (u8, u8, u8), bool)> {
    let mut lines: Vec<(String, (u8, u8, u8), bool)> = Vec::new();
    lines.push(("T-Hub Native - wire debug overlay".to_string(), ACCENT, true));
    lines.push((format!("status: {}", state.status), FG, false));
    lines.push((String::new(), FG, false));

    lines.push((format!("sessions ({}):", state.sessions.len()), ACCENT, true));
    if state.sessions.is_empty() {
        lines.push(("  (none)".to_string(), FG_DIM, false));
    }
    for s in &state.sessions {
        lines.push((format!("  {s}"), FG, false));
    }
    lines.push((String::new(), FG, false));

    lines.push(("attach (first live session, read-only):".to_string(), ACCENT, true));
    lines.push((format!("  {}", state.attach), FG, false));
    lines.push((format!("  output bytes streamed: {}", state.attach_bytes), FG_DIM, false));
    lines.push((String::new(), FG, false));

    lines.push((format!("events (last {}):", state.events.len()), ACCENT, true));
    for e in state.events.iter().rev().take(30) {
        lines.push((format!("  {e}"), FG_DIM, false));
    }
    lines
}

fn paint(view: &DebugView, bounds: Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
    let font_size = 15.0_f32;
    let line_h = 20.0_f32;
    let pad = 16.0_f32;

    window.paint_quad(fill(bounds, c2h(BG.0, BG.1, BG.2)));

    let lines = compose_lines(&view.state.lock());
    let ox = bounds.origin.x + px(pad);
    let mut oy = bounds.origin.y + px(pad);
    let max_y: f32 = (bounds.origin.y + bounds.size.height - px(pad)).into();

    for (text, color, bold) in lines {
        let oy_f: f32 = oy.into();
        if oy_f + line_h > max_y {
            break;
        }
        if !text.is_empty() {
            let run = TextRun {
                len: text.len(),
                font: if bold { view.font_bold.clone() } else { view.font_normal.clone() },
                color: c2h(color.0, color.1, color.2),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(text),
                px(font_size),
                &[run],
                None,
            );
            shaped.paint(point(ox, oy), px(line_h), window, cx).ok();
        }
        oy += px(line_h);
    }
}

impl Render for DebugView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Repaint continuously so background-thread state changes show up (T5 will
        // move to damage-driven paints; a debug overlay just spins).
        window.request_animation_frame();
        let state = self.state.clone();
        let font_normal = self.font_normal.clone();
        let font_bold = self.font_bold.clone();
        div().size_full().bg(c2h(BG.0, BG.1, BG.2)).child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let v = DebugView { state, font_normal, font_bold };
                    paint(&v, bounds, window, cx);
                },
            )
            .size_full(),
        )
    }
}

/// Background worker: connect the wire, list sessions, pump events into the shared
/// state, and attach the first live session read-only to prove the PTY plane. Owns
/// the `ControlClient` and `PtyHandle` for the process lifetime (loops forever).
fn spawn_worker(state: Arc<Mutex<DebugState>>) {
    thread::spawn(move || {
        let client = match ControlClient::connect_discovered() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                state.lock().status = format!("connect failed: {e}");
                return;
            }
        };
        state.lock().status = "connected".to_string();

        // Event pump.
        let ev_rx = client.events();
        let ev_state = state.clone();
        thread::spawn(move || {
            while let Ok(Event { channel, payload }) = ev_rx.recv() {
                let brief = payload.to_string();
                let brief = if brief.len() > 120 { format!("{}...", &brief[..120]) } else { brief };
                let mut st = ev_state.lock();
                push_capped(&mut st.events, format!("{channel}  {brief}"), EVENT_CAP);
            }
        });

        // Initial list; grab the first live session to attach read-only.
        let mut attach_target: Option<String> = None;
        match client.list_sessions() {
            Ok(sessions) => {
                let mut st = state.lock();
                st.sessions = sessions.iter().map(|s| format!("{} [{}]", s.id, s.state)).collect();
                attach_target = sessions.iter().find(|s| !s.id.is_empty()).map(|s| s.id.clone());
            }
            Err(e) => state.lock().status = format!("connected; list failed: {e}"),
        }

        // Attach the first live session (read-only: we never write to a session we
        // did not create). Proves scrollback + streaming output on the PTY plane.
        let _pty = if let Some(id) = attach_target {
            match client.attach_pty(&id, 100, 30) {
                Ok(handle) => {
                    let seed = String::from_utf8_lossy(&handle.scrollback);
                    let head: String = seed.lines().next().unwrap_or("").chars().take(80).collect();
                    {
                        let mut st = state.lock();
                        st.attach =
                            format!("th session '{id}': seed {}B, head: {head:?}", handle.scrollback.len());
                        st.attach_bytes = handle.scrollback.len();
                    }
                    let out_rx = handle.output.clone();
                    let out_state = state.clone();
                    thread::spawn(move || {
                        while let Ok(frame) = out_rx.recv() {
                            let mut st = out_state.lock();
                            match frame {
                                PtyFrame::Out(bytes) => st.attach_bytes += bytes.len(),
                                PtyFrame::Exit(code) => {
                                    st.attach = format!("{} (exited {code})", st.attach);
                                    break;
                                }
                            }
                        }
                    });
                    Some(handle)
                }
                Err(e) => {
                    state.lock().attach = format!("attach failed: {e}");
                    None
                }
            }
        } else {
            state.lock().attach = "no live session to attach".to_string();
            None
        };

        // Periodic re-list so the overlay tracks sessions coming and going.
        loop {
            thread::sleep(Duration::from_secs(RELIST_SECS));
            if let Ok(sessions) = client.list_sessions() {
                let mut st = state.lock();
                st.sessions = sessions.iter().map(|s| format!("{} [{}]", s.id, s.state)).collect();
            }
        }
    });
}

/// Entry point: dispatch to the debug overlay or the T5 render grid.
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("t-hub-native starting, pid={}", std::process::id());
    if std::env::var("THN_DEBUG").as_deref() == Ok("1") {
        run_debug();
    } else {
        run_grid();
    }
}

/// Fonts used by both modes. "Cascadia Mono" is present on the Windows box (§1.5).
fn fonts() -> (Font, Font) {
    let font_normal = font("Cascadia Mono");
    let mut font_bold = font_normal.clone();
    font_bold.weight = FontWeight::BOLD;
    (font_normal, font_bold)
}

/// Which session ids to attach: `THN_SESSIONS` (explicit, disposable for tests), or
/// the first `THN_TILES` (default 12) live sessions.
fn select_sessions(client: &ControlClient) -> Vec<String> {
    if let Ok(list) = std::env::var("THN_SESSIONS") {
        let ids: Vec<String> =
            list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if !ids.is_empty() {
            return ids;
        }
    }
    let n = std::env::var("THN_TILES").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_TILES);
    match client.list_sessions() {
        Ok(sessions) => sessions.into_iter().map(|s| s.id).filter(|id| !id.is_empty()).take(n).collect(),
        Err(e) => {
            log::error!("list_sessions failed: {e}");
            Vec::new()
        }
    }
}

/// T5 render grid: connect, attach the selected sessions, spawn one feeder thread
/// per tile (drains `PtyHandle::output` into the tile's `TermSession`), and open the
/// grid window.
fn run_grid() {
    let client = match ControlClient::connect_discovered() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("t-hub-native: could not connect to the control socket: {e}");
            eprintln!("  is the T-Hub app running? (control.json handshake missing/unreadable)");
            std::process::exit(1);
        }
    };

    let ids = select_sessions(&client);
    if ids.is_empty() {
        eprintln!("t-hub-native: no sessions to attach (set THN_SESSIONS or start sessions).");
        std::process::exit(1);
    }
    log::info!("grid: attaching {} session(s): {:?}", ids.len(), ids);

    let mut specs: Vec<TileSpec> = Vec::new();
    for id in ids {
        let handle = match client.attach_pty(&id, PROVISIONAL_COLS, PROVISIONAL_ROWS) {
            Ok(h) => h,
            Err(e) => {
                log::warn!("attach {id} failed: {e}; skipping tile");
                continue;
            }
        };
        let term = Arc::new(Mutex::new(TermSession::new(PROVISIONAL_COLS, PROVISIONAL_ROWS)));
        // Seed the grid with the opening scrollback frame.
        if !handle.scrollback.is_empty() {
            term.lock().advance(&handle.scrollback);
        }
        // One feeder thread per tile: PtyFrame::Out bytes -> TermSession.advance.
        let rx = handle.output.clone();
        let feed_term = term.clone();
        let feed_id = id.clone();
        thread::spawn(move || {
            while let Ok(frame) = rx.recv() {
                match frame {
                    PtyFrame::Out(bytes) => feed_term.lock().advance(&bytes),
                    PtyFrame::Exit(code) => {
                        log::info!("feeder[{feed_id}]: session exited ({code})");
                        break;
                    }
                }
            }
        });
        specs.push(TileSpec { id, term, pty: handle, cols: PROVISIONAL_COLS, rows: PROVISIONAL_ROWS });
    }

    if specs.is_empty() {
        eprintln!("t-hub-native: every attach failed; nothing to render.");
        std::process::exit(1);
    }

    let (font_normal, font_bold) = fonts();
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let focus = cx.focus_handle();
                let view = cx.new(|_| {
                    GridView::new(specs, client, font_normal, font_bold, focus.clone())
                });
                window.focus(&focus);
                view
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

/// The T4 wire-debug overlay (opt-in via `THN_DEBUG=1`).
fn run_debug() {
    let state = Arc::new(Mutex::new(DebugState {
        status: "connecting...".to_string(),
        ..Default::default()
    }));
    spawn_worker(state.clone());

    let (font_normal, font_bold) = fonts();
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(700.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| DebugView { state, font_normal, font_bold }),
        )
        .unwrap();
        cx.activate(true);
    });
}
