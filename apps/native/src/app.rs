//! GPUI window for the native client.
//!
//! Three modes:
//!  - **cockpit** (default, T8): the chrome - a left sidebar listing the
//!    workspaces, the per-workspace tile grid, real per-tile headers -
//!    live-populated from `list_terminals` and reconciled as sessions come and
//!    go. [`crate::chrome::view::CockpitView`].
//!  - **grid** (`THN_GRID=1`, the T5 seam demo): a flat near-square grid of the
//!    first N sessions, kept for the damage/full A/B benches.
//!  - **debug** (`THN_DEBUG=1`, the T4 overlay): a scrolling proof that the wire is
//!    live (connection status, session list, event tail, first-attach byte count).
//!
//! GPUI boilerplate (Application, the canvas + `text_system().shape_line` paint loop
//! driven by `request_animation_frame`) is lifted from the T2 spike main.rs, which
//! is known-good on gpui 0.2.2.
//!
//! ## Session selection (T5 grid mode only)
//! `THN_SESSIONS=id1,id2,...` attaches exactly those sessions (used by the
//! acceptance harness with DISPOSABLE `t5-*` sessions, so typing tests never touch
//! sessions we did not create). Unset -> the first `THN_TILES` (default 12) live
//! sessions from `list_terminals`. The cockpit ignores both: it tracks the live
//! session list and the persisted layout.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, font, point, px, size, AnyWindowHandle, App, Application, AsyncApp, Bounds,
    Context, Font, FontWeight, Hsla, IntoElement, Render, Rgba, SharedString, TextRun,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use parking_lot::Mutex;

use crate::chrome::model::ChromeModel;
use crate::chrome::persist;
use crate::chrome::view::{
    close_satellite_home, log_winstat, open_satellite, sync_active_sessions, tear_off_workspace,
    CockpitState, CockpitView, SatHandles,
};
use crate::chrome::windows::SatBounds;
use crate::font::FontSpec;
use crate::overlays::{OverlayFeed, OverlaySidebar};
use crate::render::{GridView, Tile, TileSpec};
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

/// Entry point: the T8 cockpit by default; `THN_GRID=1` for the T5 seam demo,
/// `THN_DEBUG=1` for the T4 wire overlay.
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("t-hub-native starting, pid={}", std::process::id());
    if std::env::var("THN_DEBUG").as_deref() == Ok("1") {
        run_debug();
    } else if std::env::var("THN_GRID").as_deref() == Ok("1") {
        run_grid();
    } else {
        run_cockpit();
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
        specs.push(TileSpec {
            id,
            term,
            pty: Some(handle),
            cols: PROVISIONAL_COLS,
            rows: PROVISIONAL_ROWS,
            font: None,
            fixture: None,
        });
    }

    if specs.is_empty() {
        eprintln!("t-hub-native: every attach failed; nothing to render.");
        std::process::exit(1);
    }

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
        let opened = cx.open_window(
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
                let view =
                    cx.new(|_| GridView::new(specs, Some(client), focus.clone()));
                window.focus(&focus);
                view
            },
        );
        if let Err(e) = opened {
            eprintln!("t-hub-native: failed to open the grid window: {e:#}");
            std::process::exit(1);
        }
        cx.activate(true);
    });
}

// ---------------------------------------------------------------------------
// T8 cockpit
// ---------------------------------------------------------------------------

/// How often the cockpit worker re-lists sessions when no event hints arrive.
/// The control socket has no terminal-lifecycle channel (§1.2 lists only
/// status/session/supervision/agent), so adds/removals ride this poll,
/// accelerated to ~[`HINT_TICK`] by any session-ish event.
const RELIST_INTERVAL: Duration = Duration::from_secs(2);
/// How often the worker checks the T9 SidebarState's folded-event counter for
/// a hint that the session set (or its metadata) changed. The T9 `OverlayFeed`
/// is the process's SINGLE event drainer (its docs: events receivers COMPETE
/// for frames); the chrome no longer subscribes - it reads the fold counter.
const HINT_TICK: Duration = Duration::from_millis(250);

/// T8 cockpit: tabs + per-workspace grid + headers over the live session list;
/// T10: plus one satellite window per torn-off workspace (restored from the
/// layout at boot). Loads the persisted layout, spawns the reconcile worker
/// (which attaches every placed session - the persistent pool), and opens the
/// cockpit window.
fn run_cockpit() {
    let client = match ControlClient::connect_discovered() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("t-hub-native: could not connect to the control socket: {e}");
            eprintln!("  is the T-Hub app running? (control.json handshake missing/unreadable)");
            std::process::exit(1);
        }
    };

    let layout_path = persist::layout_path();
    // Satellite seeds are (tab index, persisted window bounds); the registry is
    // keyed by wsid, which `into_model` assigns - map through the model below.
    let (model, sat_seed): (ChromeModel, Vec<(usize, Option<SatBounds>)>) =
        match persist::load(&layout_path) {
            Ok(Some(layout)) => {
                let seed = layout.satellite_bounds();
                (layout.into_model(), seed)
            }
            Ok(None) => (ChromeModel::default(), Vec::new()),
            Err(e) => {
                log::warn!("layout unreadable, starting fresh: {e:#}");
                (ChromeModel::default(), Vec::new())
            }
        };
    log::info!(
        "cockpit: {} tab(s) from {}, active {}, {} satellite(s)",
        model.tabs.len(),
        layout_path.display(),
        model.active,
        sat_seed.len()
    );

    // The T9 feed is the process's SINGLE control-event drainer; the chrome
    // reads its shared SidebarState instead of subscribing itself.
    let feed = OverlayFeed::spawn(client.clone());

    // Resume clicks from the recents overlay need a tile-spawn path the socket
    // does not provide yet (`spawn_terminal` only forwards to a UI sink); log
    // them until that lands (T11/T12 territory).
    {
        let host_rx = feed.host_requests();
        thread::spawn(move || {
            while let Ok(req) = host_rx.recv() {
                log::info!("host request (needs a tile-spawn path, not wired yet): {req:?}");
            }
        });
    }

    let state = Arc::new(Mutex::new(CockpitState::new(model, layout_path)));

    // Register the persisted satellites in the window registry (their OS
    // windows open after the main window below, inside Application::run).
    {
        let mut st = state.lock();
        let seeds: Vec<(u64, Option<String>, Option<SatBounds>)> = sat_seed
            .into_iter()
            .filter_map(|(i, bounds)| {
                st.model
                    .tabs
                    .get(i)
                    .map(|t| (t.wsid, t.tiles.first().cloned(), bounds))
            })
            .collect();
        for (wsid, first_tile, bounds) in seeds {
            st.sats.open(wsid, first_tile, bounds);
        }
    }

    spawn_cockpit_worker(state.clone(), client.clone(), feed.clone());
    spawn_winstat_ticker(state.clone());

    Application::new().run(move |cx: &mut App| {
        let handles: SatHandles = Rc::new(RefCell::new(HashMap::new()));
        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            {
                let state = state.clone();
                let feed = feed.clone();
                let handles = handles.clone();
                move |window, cx| {
                    let focus = cx.focus_handle();
                    let overlays = cx.new(|_| OverlaySidebar::new(feed.clone()));
                    // Closing the main window quits the whole app; save the
                    // layout first so satellite bounds (refreshed per paint)
                    // land on disk even when no other mutation happened.
                    {
                        let state = state.clone();
                        window.on_window_should_close(cx, move |_, _| {
                            state.lock().save_layout();
                            true
                        });
                    }
                    let view = cx.new(|_| {
                        CockpitView::new(state, overlays, feed, client, handles, focus.clone())
                    });
                    window.focus(&focus);
                    view
                }
            },
        );
        let main_window: AnyWindowHandle = match opened {
            Ok(handle) => handle.into(),
            Err(e) => {
                eprintln!("t-hub-native: failed to open the cockpit window: {e:#}");
                std::process::exit(1);
            }
        };

        // The main window owns the process: when it closes, quit (satellites
        // die with the process; their workspaces stay satellites in the layout
        // and are restored on the next boot).
        cx.on_window_closed(move |cx| {
            if !cx.windows().contains(&main_window) {
                cx.quit();
            }
        })
        .detach();

        // Reopen the persisted satellites.
        let wsids = state.lock().sats.wsids();
        for wsid in wsids {
            open_satellite(cx, &state, &feed, &handles, wsid);
        }
        log_winstat(&state, "boot");

        spawn_sat_cycle_harness(cx, &state, &feed, &handles);

        cx.activate(true);
    });
}

/// Periodic winstat line (window count x visible cells vs RSS/fds), the T10
/// watch item, on a 5s cadence. Opt-in via `THN_MEM_LOG=1` (the tear/close
/// lifecycle events log unconditionally).
fn spawn_winstat_ticker(state: Arc<Mutex<CockpitState>>) {
    if std::env::var("THN_MEM_LOG").as_deref() != Ok("1") {
        return;
    }
    thread::Builder::new()
        .name("t-hub-native-winstat".into())
        .spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            log_winstat(&state, "tick");
        })
        .expect("spawn winstat ticker thread");
}

/// The T10 acceptance harness: `THN_SAT_CYCLE=N` runs N tear-off/close-back
/// cycles on the active workspace through the EXACT functions the sidebar
/// clicks use, logging a winstat line per transition - the 10-cycle leak check
/// without brittle synthetic input. `THN_SAT_CYCLE_MS` sets the dwell (default
/// 2000ms) so terminals stream visibly inside the satellite between moves.
fn spawn_sat_cycle_harness(
    cx: &mut App,
    state: &Arc<Mutex<CockpitState>>,
    feed: &OverlayFeed,
    handles: &SatHandles,
) {
    let n: u32 = std::env::var("THN_SAT_CYCLE").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    if n == 0 {
        return;
    }
    let dwell = Duration::from_millis(
        std::env::var("THN_SAT_CYCLE_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(2000),
    );
    let state = state.clone();
    let feed = feed.clone();
    let handles = handles.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        for cycle in 1..=n {
            cx.background_executor().timer(dwell).await;
            let torn = cx
                .update(|cx| {
                    let idx = {
                        let st = state.lock();
                        let i = st.model.active;
                        (!st.model.tabs[i].satellite).then_some(i)
                    };
                    idx.and_then(|i| tear_off_workspace(cx, &state, &feed, &handles, i))
                })
                .ok()
                .flatten();
            let Some(wsid) = torn else {
                log::warn!("sat-cycle {cycle}/{n}: no main workspace to tear off");
                continue;
            };
            log::info!("sat-cycle {cycle}/{n}: tore off wsid {wsid}");
            cx.background_executor().timer(dwell).await;
            cx.update(|cx| close_satellite_home(cx, &state, &feed, &handles, wsid)).ok();
            log::info!("sat-cycle {cycle}/{n}: closed back wsid {wsid}");
        }
        log_winstat(&state, "cycle-done");
        log::info!("sat-cycle: T10-SAT-CYCLE-DONE");
    })
    .detach();
}

/// The cockpit reconcile worker: keep the model's tile set matched to the
/// server's live sessions. Poll `list_terminals` every [`RELIST_INTERVAL`];
/// a change in the T9 SidebarState's folded-event counter short-circuits the
/// wait, so a session spawned by an agent shows up in well under a second.
/// The chrome holds NO events receiver - the feed is the sole drainer.
fn spawn_cockpit_worker(
    state: Arc<Mutex<CockpitState>>,
    client: Arc<ControlClient>,
    feed: OverlayFeed,
) {
    thread::Builder::new()
        .name("t-hub-native-cockpit".into())
        .spawn(move || {
            let sidebar = feed.state();
            let folded = |sb: &Arc<Mutex<crate::overlays::SidebarState>>| sb.lock().events_folded;
            loop {
                reconcile_once(&state, &client);
                {
                    let st = state.lock();
                    sync_active_sessions(&st, &feed);
                }

                // Wait out the poll interval, cutting it short when the feed
                // folded new events (session-ish state moved somewhere).
                let mut seen = folded(&sidebar);
                let deadline = Instant::now() + RELIST_INTERVAL;
                loop {
                    thread::sleep(HINT_TICK);
                    if Instant::now() >= deadline {
                        break;
                    }
                    let now = folded(&sidebar);
                    if now != seen {
                        seen = now;
                        break;
                    }
                }
            }
        })
        .expect("spawn cockpit worker thread");
}

/// One reconcile pass: refresh titles, diff the layout against the live list,
/// drop dead tiles from the pool, attach what needs attaching, persist changes.
fn reconcile_once(state: &Arc<Mutex<CockpitState>>, client: &Arc<ControlClient>) {
    let sessions = match client.list_sessions() {
        Ok(s) => s,
        Err(e) => {
            log::debug!("cockpit: list_terminals failed ({e}); retrying");
            return;
        }
    };
    let live: Vec<String> =
        sessions.iter().map(|s| s.id.clone()).filter(|id| !id.is_empty()).collect();

    let need_attach: Vec<String> = {
        let mut st = state.lock();
        for s in &sessions {
            if !s.id.is_empty() {
                st.titles.insert(s.id.clone(), s.title.clone());
            }
        }
        let out = st.model.reconcile(&live);
        for id in &out.removed {
            log::info!("cockpit: session {id} gone; dropping its tile");
            st.drop_tile(id);
        }
        if !out.added.is_empty() || !out.removed.is_empty() {
            st.save_layout();
        }
        // Attach every placed tile missing from the pool - not just this pass's
        // `added`: tiles restored from the persisted layout boot un-pooled (the
        // bug the first live run caught), and a died-and-relisted session heals
        // the same way.
        st.model
            .all_tiles()
            .into_iter()
            .filter(|id| !st.tiles.contains_key(id))
            .collect()
    };

    // Attach outside the lock (each attach is a network round-trip).
    for id in need_attach {
        attach_tile(state, client, &id);
    }
}

/// Attach one session into the pool: PTY attach at the provisional geometry
/// (the first paint reflows it), seed the `TermSession` with the scrollback,
/// and spawn the feeder thread (bytes -> `advance`, stamping the liveness cue).
fn attach_tile(state: &Arc<Mutex<CockpitState>>, client: &Arc<ControlClient>, id: &str) {
    let handle = match client.attach_pty(id, PROVISIONAL_COLS, PROVISIONAL_ROWS) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("cockpit: attach {id} failed ({e}); a later reconcile retries");
            // Not hidden - just out of the layout, so reconcile can re-add it.
            state.lock().model.remove_tile(id);
            return;
        }
    };
    let term = Arc::new(Mutex::new(TermSession::new(PROVISIONAL_COLS, PROVISIONAL_ROWS)));
    if !handle.scrollback.is_empty() {
        term.lock().advance(&handle.scrollback);
    }

    let stamp = Arc::new(AtomicU64::new(0));
    let mut st = state.lock();
    if !st.model.contains_tile(id) {
        // Closed while the attach was in flight; drop the handle (detaches).
        return;
    }
    let epoch = st.epoch;
    let rx = handle.output.clone();
    let feed_term = term.clone();
    let feed_stamp = stamp.clone();
    let feed_id = id.to_string();
    thread::spawn(move || {
        while let Ok(frame) = rx.recv() {
            match frame {
                PtyFrame::Out(bytes) => {
                    feed_term.lock().advance(&bytes);
                    feed_stamp.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                }
                PtyFrame::Exit(code) => {
                    // The next reconcile removes the tile once the server
                    // drops the session from list_terminals.
                    log::info!("feeder[{feed_id}]: session exited ({code})");
                    break;
                }
            }
        }
    });
    // T7 per-tile font: the workspace's persisted override, else THN_FONT/default.
    let spec = st.model.font_for(id).cloned().unwrap_or_else(FontSpec::from_env);
    st.tiles.insert(
        id.to_string(),
        Tile::new(
            id.to_string(),
            term,
            Some(handle),
            PROVISIONAL_COLS,
            PROVISIONAL_ROWS,
            spec,
            None,
        ),
    );
    st.last_output_ms.insert(id.to_string(), stamp);
    if st.model.focused.is_none() {
        st.model.set_focused(id);
    }
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
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| DebugView { state, font_normal, font_bold }),
        );
        if let Err(e) = opened {
            eprintln!("t-hub-native: failed to open the debug window: {e:#}");
            std::process::exit(1);
        }
        cx.activate(true);
    });
}
