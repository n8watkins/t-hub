//! T11 visual demo (feature `gui`): a standalone window hosting ONLY the
//! [`PanelHost`] composite (Files | Preview | Run), sized like a side panel.
//! This is the demo entry point the brief requires instead of touching
//! `app.rs` (T10 owns that file this wave); the captain uses it for the live
//! visual check and wires the same mount into the cockpit later.
//!
//! `THN_PANEL_ROOT=/path` pre-binds the Files/Run tabs to a project root
//! (the same `feed.set_root` hook a per-tile mount would use).

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, size, App, Application, Bounds, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use t_hub_native::panels::{PanelHost, PanelsFeed};
use t_hub_native::wire::ControlClient;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let client = match ControlClient::connect_discovered() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("panel-window: could not connect to the control socket: {e}");
            std::process::exit(1);
        }
    };

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(420.), px(760.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("T-Hub Native - panels")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let focus = cx.focus_handle();
                let feed = PanelsFeed::spawn(client.clone());
                if let Ok(root) = std::env::var("THN_PANEL_ROOT") {
                    feed.set_root(&root);
                }
                let panels = cx.new(|_| PanelHost::new(feed.clone(), focus.clone()));
                // Keyboard goes to the host element (Files search box / Run
                // command line) - exactly what a cockpit mount must do when a
                // panel surface takes focus.
                window.focus(&focus);
                cx.new(|_| Shell { panels })
            },
        );
        if let Err(e) = opened {
            eprintln!("panel-window: failed to open the panels window: {e}");
            std::process::exit(1);
        }
        cx.activate(true);
    });
}

/// Minimal host: fills the window with the panels element, exactly how a
/// cockpit tile or side surface embeds it.
struct Shell {
    panels: gpui::Entity<PanelHost>,
}

impl Render for Shell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div().size_full().child(self.panels.clone())
    }
}
