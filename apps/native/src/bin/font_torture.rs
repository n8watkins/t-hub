//! **T7 font torture-test screen** - the visual acceptance harness for the font
//! subsystem (native-pivot T7, `docs/NATIVE-PIVOT-EXECUTION.md` §3).
//!
//! Two modes:
//! - `font-torture` (default): opens a GPUI window with one OFFLINE tile per
//!   configured font (no server, no PTY - the fixture bytes are fed straight
//!   into each tile's `TermSession`). This exercises the whole T7 paint path:
//!   procedural sprites, segmented text positioning, emoji fallback, wide chars,
//!   combining marks, per-tile font family/size/ligature config.
//! - `font-torture --emit`: writes the raw fixture bytes to stdout, so the SAME
//!   content can be piped into WezTerm / Windows Terminal for the visual diff
//!   the T7 acceptance asks for (e.g. `font-torture --emit | more` in WT, or
//!   `font-torture.exe --emit > torture.ans; type torture.ans`).
//!
//! `THN_TORTURE_FONTS` overrides the tile set: comma-separated `FontSpec`s,
//! e.g. `THN_TORTURE_FONTS="Cascadia Mono:13,Cascadia Code:14:lig"`. Default is
//! Cascadia Mono 13 (the production default) next to Cascadia Code 13 with
//! ligatures (the ligature assessment tile).
//!
//! This bin exists because §3 T7 forbids restructuring `app.rs` (T8 owns it) -
//! it boots its own window the way the T13b probe added its own entry point.

use std::io::Write as _;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{px, size, App, Application, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions};
use parking_lot::Mutex;

use t_hub_native::font::torture::{torture_bytes, torture_rows, TORTURE_COLS};
use t_hub_native::font::FontSpec;
use t_hub_native::render::{GridView, TileSpec};
use t_hub_native::term::TermSession;

const DEFAULT_TILE_FONTS: &str = "Cascadia Mono:13,Cascadia Code:13:lig";

fn main() {
    if std::env::args().any(|a| a == "--emit") {
        let mut out = std::io::stdout().lock();
        out.write_all(&torture_bytes()).expect("write fixture to stdout");
        out.flush().ok();
        return;
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let fonts_env =
        std::env::var("THN_TORTURE_FONTS").unwrap_or_else(|_| DEFAULT_TILE_FONTS.to_string());
    let mut fonts: Vec<FontSpec> =
        fonts_env.split(',').filter_map(|s| FontSpec::parse(s.trim())).collect();
    if fonts.is_empty() {
        eprintln!("font-torture: THN_TORTURE_FONTS parsed to nothing; using the default spec");
        fonts.push(FontSpec::default());
    }

    let fixture = Arc::new(torture_bytes());
    let (cols, rows) = (TORTURE_COLS, torture_rows());
    let specs: Vec<TileSpec> = fonts
        .into_iter()
        .map(|spec| {
            let mut term = TermSession::new(cols, rows);
            term.advance(&fixture);
            TileSpec {
                id: "torture".to_string(),
                term: Arc::new(Mutex::new(term)),
                pty: None,
                cols,
                rows,
                font: Some(spec),
                fixture: Some(fixture.clone()),
            }
        })
        .collect();

    log::info!("font-torture: {} tile(s), fixture {} bytes", specs.len(), fixture.len());
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native - font torture")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let focus = cx.focus_handle();
                let view = cx.new(|_| GridView::new(specs, None, focus.clone()));
                window.focus(&focus);
                view
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
