//! T9 visual demo (feature `gui`): a standalone window hosting ONLY the
//! [`OverlaySidebar`], sized like the webview sidebar. This is the demo entry
//! point the brief allows instead of touching `app.rs` (T8 owns that file);
//! the captain uses it for the live visual check.
//!
//! Resume clicks land on the feed's [`HostRequest`] channel; with no workspace
//! shell here, the demo just logs them - proving the wiring T8 will consume.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, size, App, Application, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions};

use t_hub_native::overlays::model::{now_ms, SessionStatus};
use t_hub_native::overlays::toasts::WARMUP_INITIAL_MS;
use t_hub_native::overlays::OverlaySidebar;
use t_hub_native::wire::ControlClient;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let client = match ControlClient::connect_discovered() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("overlay-window: could not connect to the control socket: {e}");
            std::process::exit(1);
        }
    };

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(320.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native - overlays")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| {
                let (view, feed) = OverlaySidebar::mount(client.clone(), cx);
                // Log what a real shell (T8) would wire to its tile-spawn path.
                let host_rx = feed.host_requests();
                thread::spawn(move || {
                    while let Ok(req) = host_rx.recv() {
                        log::info!("host request (T8 would spawn a tile): {req:?}");
                    }
                });
                // `THN_TOAST_DEMO=1`: once warmup expires, fold three synthetic
                // status transitions through the REAL event path so the toast
                // cards can be inspected without waiting for a live transition.
                if std::env::var("THN_TOAST_DEMO").as_deref() == Ok("1") {
                    let state = feed.state();
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(WARMUP_INITIAL_MS + 500));
                        let mut st = state.lock();
                        for (id, status) in [
                            ("demo-attention", SessionStatus::NeedsQuestion),
                            ("demo-done", SessionStatus::Completed),
                            ("demo-error", SessionStatus::Failed),
                        ] {
                            st.toasts.fold_status(id, status, None, now_ms());
                        }
                    });
                }
                cx.new(|_| Shell { overlays: view })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

/// Minimal host: fills the window with the sidebar element, exactly how T8's
/// shell embeds it below the workspace list.
struct Shell {
    overlays: gpui::Entity<OverlaySidebar>,
}

impl Render for Shell {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div().size_full().child(self.overlays.clone())
    }
}
