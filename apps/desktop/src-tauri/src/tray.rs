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

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};

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

/// Build the tray icon + menu and wire its events. Called from `lib.rs`'s
/// `setup()` once the `AppHandle` exists. Returns the build error (if any) so
/// the caller can decide how to surface it; startup should not abort on a tray
/// failure (the app is still usable via its window).
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show T-Hub", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("T-Hub")
        .menu(&menu)
        // Left-click toggles to showing the window (the menu handles the rest),
        // so don't pop the menu on a plain left click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
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
