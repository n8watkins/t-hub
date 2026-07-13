//! System-tray integration + close-to-tray behavior (#17).
//!
//! Closing the main window used to quit the app, which locked the user out.
//! Here we instead:
//!   * intercept the window close request, prevent it, and hide the window;
//!   * install a tray icon (using the app's default window icon) with a menu
//!     ("Show T-Hub" / "Quit");
//!   * left-clicking the tray icon shows + focuses the main window;
//!   * "Quit" actually exits the process.
//!
//! Tauri's tray API is cross-platform, so no `cfg` gating is needed. On Linux
//! the tray is backed by libayatana-appindicator (present in this env).
//!
//! ## Recovery actions (light tier)
//!
//! Two low-risk "unwedge without a full restart" items live in the menu:
//!   * **Reload window** — re-render the React frontend (`WebviewWindow::reload`)
//!     without touching the backend, so a frozen/wedged UI recovers while every
//!     tmux session and the agent connection stay alive.
//!   * **Reconnect agent bridge** — tear down + re-establish the `t-hub-agent`
//!     connection (fixes "supervision / cost stopped updating") without touching
//!     any terminal. Delegates to [`crate::agent::AgentBridge::reconnect`], which
//!     does the safe teardown.
//!
//! The heavier recovery actions (restart tmux server, full WSL shutdown) are
//! deferred — they're rarer and riskier than these two.

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};

use crate::AppState;

/// Show + focus the main webview window, unminimizing it first if needed.
/// No-op (beyond a logged warning) if the window can't be found.
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        eprintln!("t-hub: tray could not find the 'main' window to show");
    }
}

/// Recovery: re-render the React frontend without restarting the backend. Uses
/// the native [`tauri::WebviewWindow::reload`] (reloads the current page), which
/// keeps every tmux session and the live agent connection intact — only the
/// webview's JS heap is thrown away and rebuilt. Fixes a wedged/frozen UI.
/// No-op (beyond a logged warning) if the main window can't be found.
fn reload_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.reload() {
            eprintln!("t-hub: tray 'Reload window' failed to reload the webview: {e}");
        }
    } else {
        eprintln!("t-hub: tray could not find the 'main' window to reload");
    }
}

/// Recovery: re-establish the `t-hub-agent` connection (fixes "supervision /
/// cost stopped updating") without touching terminals. Pulls the managed
/// [`AppState`] for the bridge handle and resolves the distro the same way
/// `lib.rs` does (`T_HUB_DISTRO`, default `Ubuntu-24.04`; ignored on unix).
///
/// Runs the reconnect on a detached thread: [`AgentBridge::reconnect`] does a
/// WSL hop + handshake (+ possibly a journal replay) and blocks until Live, which
/// must never freeze the tray/UI thread. The bridge emits `agent://state` as it
/// transitions, so the UI's health area reflects progress live.
fn reconnect_agent_bridge(app: &AppHandle) {
    let bridge = app.state::<AppState>().agent.clone();
    // Mirror lib.rs::default_distro() (kept private there); `T_HUB_DISTRO`
    // overrides, default Ubuntu-24.04. Irrelevant on unix (direct spawn).
    let distro = std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
    std::thread::Builder::new()
        .name("t-hub-agent-reconnect".into())
        .spawn(move || {
            eprintln!("t-hub: tray 'Reconnect agent bridge' (distro={distro:?})");
            if let Err(e) = bridge.reconnect(&distro) {
                eprintln!("t-hub: agent bridge reconnect failed: {e}");
            }
        })
        .ok();
}

/// Build the tray icon + menu and wire its events. Called from `lib.rs`'s
/// `setup()` once the `AppHandle` exists. Returns the build error (if any) so
/// the caller can decide how to surface it; startup should not abort on a tray
/// failure (the app is still usable via its window).
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(
        app,
        "show",
        format!("Show {}", crate::brand_name()),
        true,
        None::<&str>,
    )?;
    // --- Recovery (light tier): unwedge without a full restart. ---
    let reload_item = MenuItem::with_id(app, "reload_window", "Reload window", true, None::<&str>)?;
    let reconnect_item = MenuItem::with_id(
        app,
        "reconnect_agent",
        "Reconnect agent bridge",
        true,
        None::<&str>,
    )?;
    // Two distinct separator instances (a menu item belongs to a single slot, so
    // we don't append one instance twice). They bracket the recovery group so it
    // reads as a distinct cluster between "Show T-Hub" and "Quit".
    let sep_top = PredefinedMenuItem::separator(app)?;
    let sep_bottom = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &sep_top,
            &reload_item,
            &reconnect_item,
            &sep_bottom,
            &quit_item,
        ],
    )?;

    let mut builder = TrayIconBuilder::new()
        .tooltip(crate::brand_name())
        .menu(&menu)
        // Left-click toggles to showing the window (the menu handles the rest),
        // so don't pop the menu on a plain left click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "reload_window" => reload_main_window(app),
            "reconnect_agent" => reconnect_agent_bridge(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    // Use the app's configured default window icon for the tray. If one was set
    // in tauri.conf.json's bundle icons, it's available here.
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

/// Window-event handler installed on the Tauri builder: a close request on any
/// window is intercepted, prevented, and the window is hidden to the tray
/// instead of quitting. "Quit" (tray menu) is the only path that exits.
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
    }
}
