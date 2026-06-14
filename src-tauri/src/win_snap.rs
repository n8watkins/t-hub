//! Windows 11 Snap Layouts + native edge-resize for the frameless main window.
//!
//! TermHub's main window is frameless (`decorations: false`, see
//! `tauri.conf.json`) with a custom titlebar (`src/components/Titlebar.tsx`).
//! Dropping the native frame also dropped two OS affordances that depend on the
//! non-client area:
//!
//!   * the Windows 11 **Snap Layouts** flyout (hovering the maximize button pops
//!     a snap-zone picker), which the shell only offers when a window reports
//!     `HTMAXBUTTON` from `WM_NCHITTEST` *and* still owns a DWM-managed frame the
//!     flyout can anchor to; and
//!   * native **edge/corner resize** affordances (the `HTLEFT`/`HTTOP`/... codes).
//!
//! ## Why the first attempt (just returning `HTMAXBUTTON`) did not show the flyout
//!
//! Tauri's Windows backend (tao) keeps the window's *styles* (`WS_CAPTION`,
//! `WS_THICKFRAME`, `WS_MAXIMIZEBOX`) but makes the window look frameless by
//! answering `WM_NCCALCSIZE` with `0`, which **collapses the non-client area to
//! zero**. With a zero-height non-client frame there is no DWM caption frame for
//! the OS to anchor the Snap Layouts flyout to, so even a correct `HTMAXBUTTON`
//! from `WM_NCHITTEST` produces *nothing* on hover. This is the canonical Win32
//! "custom frame" gotcha (see Microsoft's "Custom Window Frame Using DWM").
//!
//! ## The fix (keeps the frameless custom T-Hub bar)
//!
//! Two missing ingredients, both standard for custom-frame Win32 windows:
//!
//!   1. **`DwmExtendFrameIntoClientArea`** with a tiny (1px top) margin. This
//!      re-establishes a DWM-managed frame *behind* the client area without
//!      changing how the window looks (tao's `WM_NCCALCSIZE` still gives us the
//!      full client rect), restoring the surface the Snap flyout attaches to.
//!   2. **`DwmDefWindowProc` first** in the subclass proc. DWM's default proc is
//!      what actually drives the maximize-button hover highlight and the Snap
//!      Layouts flyout off the `HTMAXBUTTON` we report. We must give every
//!      message to it first and honor a handled result.
//!
//! With those in place we still answer `WM_NCHITTEST` ourselves for the resize
//! bands, the `HTMAXBUTTON` slot, and `HTCAPTION` over the draggable titlebar.
//!
//! Everything here is `#[cfg(windows)]`; on unix this module compiles to an
//! empty `install` no-op so the rest of the app is untouched.
//!
//! ## Geometry (kept in sync with `src/components/Titlebar.tsx`)
//! The titlebar is `h-8` = 32 logical px tall. The window controls are each
//! `w-11` = 44 logical px wide, anchored at the top-right in the order (from the
//! right edge): Close, Maximize, Minimize. So the **maximize** button occupies
//! the slot `[width - 88, width - 44)` horizontally and `[0, 32)` vertically, in
//! logical pixels. We scale those by the window DPI to get physical pixels.
//!
//! These are constants mirrored from the frontend (phase 1); if the titlebar
//! layout changes materially, update [`TITLEBAR_H_LOGICAL`] / [`CONTROL_W_LOGICAL`]
//! here too. A later phase could plumb the exact device-pixel bounds from the
//! frontend (e.g. via a Tauri command that reports the maximize button's
//! `getBoundingClientRect()`), removing the duplication.

/// Install the Snap-Layouts / edge-resize window subclass on the given Tauri
/// window. On non-Windows targets this is a no-op. Errors are returned so the
/// caller can log them; a failure here must never abort app startup (the window
/// stays usable, just without the native snap flyout / edge resize).
#[cfg(windows)]
pub fn install<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> tauri::Result<()> {
    imp::install(window)
}

/// Unix / non-Windows stub: there is no non-client hit-testing to restore, so
/// this does nothing and always succeeds.
#[cfg(not(windows))]
pub fn install<R: tauri::Runtime>(_window: &tauri::WebviewWindow<R>) -> tauri::Result<()> {
    Ok(())
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::{DwmDefWindowProc, DwmExtendFrameIntoClientArea};
    use windows::Win32::UI::Controls::MARGINS;
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, IsZoomed, SendMessageW, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION,
        HTCLIENT, HTLEFT, HTMAXBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, SC_MAXIMIZE,
        SC_RESTORE, WM_ACTIVATE, WM_DPICHANGED, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDOWN,
        WM_NCLBUTTONUP, WM_SYSCOMMAND,
    };

    /// Titlebar height in *logical* (CSS) pixels - must match the `h-8` row in
    /// `src/components/Titlebar.tsx`.
    const TITLEBAR_H_LOGICAL: f64 = 32.0;
    /// Width of a single window-control button in *logical* pixels - the `w-11`
    /// minimize / maximize / close buttons in `src/components/Titlebar.tsx`.
    const CONTROL_W_LOGICAL: f64 = 44.0;
    /// The maximize button is the 2nd control from the right edge (Close is 1st),
    /// so its right edge is one control-width in from the window's right edge.
    const MAXBTN_RIGHT_OFFSET_LOGICAL: f64 = CONTROL_W_LOGICAL; // 44
    /// ...and its left edge is two control-widths in.
    const MAXBTN_LEFT_OFFSET_LOGICAL: f64 = CONTROL_W_LOGICAL * 2.0; // 88
    /// Thickness (logical px) of the invisible window-edge band that triggers a
    /// native resize. A few px wider than the OS default makes the frameless
    /// edges easier to grab without eating into the content.
    const RESIZE_BORDER_LOGICAL: f64 = 6.0;

    /// A private, stable subclass id (any constant unique within this HWND's
    /// subclass chain works; "THSN" = TermHub SNap, as ASCII bytes).
    const SUBCLASS_ID: usize = 0x5448_534E;

    /// Install the subclass + extend the DWM frame. Idempotent-ish: Tauri builds
    /// the main window once at startup, and we install exactly once from `setup()`.
    pub fn install<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> tauri::Result<()> {
        let hwnd = window.hwnd()?;
        // SAFETY: `hwnd` is a live top-level window owned by Tauri for the life of
        // the app. `subclass_proc` matches the `SUBCLASSPROC` ABI. We pass no ref
        // data. comctl32 keeps the subclass until the window is destroyed (we also
        // remove it on WM_NCDESTROY in the proc as a belt-and-braces cleanup).
        unsafe {
            let ok = SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0);
            if !ok.as_bool() {
                // Surface as a (non-fatal) IO error so the caller can log it. The
                // OS reason, if any, is in GetLastError; we keep the message simple.
                return Err(tauri::Error::Io(std::io::Error::other(
                    "SetWindowSubclass failed to install the Snap-Layouts hit-test hook",
                )));
            }
            // Re-establish a DWM-managed frame so the Snap Layouts flyout has
            // something to anchor to. Without this, tao's zero non-client area
            // (it answers WM_NCCALCSIZE with 0) means the flyout never appears
            // even though we report HTMAXBUTTON. A 1px top margin is enough; the
            // window still *looks* frameless because tao keeps the full client
            // rect. A failure here is logged but non-fatal (resize still works).
            extend_frame(hwnd);
            eprintln!(
                "termhub: win_snap installed (subclass + DWM frame) on HWND {:?}",
                hwnd.0
            );
        }
        Ok(())
    }

    /// Extend the DWM frame by a tiny top margin so the window keeps a DWM-managed
    /// caption frame (the surface the Snap Layouts flyout attaches to) while still
    /// presenting a full client area visually. Per Microsoft's custom-frame guide
    /// this is re-applied on activation / DPI change.
    unsafe fn extend_frame(hwnd: HWND) {
        // A 1px top sliver is enough to give DWM a frame; left/right/bottom 0 so we
        // do not paint any visible glass border. (A negative "-1" sheet-of-glass
        // margin would also work but can tint the whole window; 1px top is the
        // least-invasive value that restores the flyout anchor.)
        let margins = MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: 1,
            cyBottomHeight: 0,
        };
        if let Err(e) = DwmExtendFrameIntoClientArea(hwnd, &margins) {
            eprintln!("termhub: win_snap DwmExtendFrameIntoClientArea failed: {e}");
        }
    }

    /// The window subclass proc. DWM's default proc gets first crack at every
    /// message (so the maximize-button hover highlight + Snap Layouts flyout
    /// render); then we special-case `WM_NCHITTEST` (resize bands + caption +
    /// the `HTMAXBUTTON` slot). Everything else - including all the messages
    /// Tauri/wry rely on - falls through to `DefSubclassProc`.
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref_data: usize,
    ) -> LRESULT {
        // Let DWM handle the message first. For caption-button messages (incl. the
        // maximize-button hover that drives Snap Layouts) it returns TRUE and fills
        // `dwm_result`; we then return that and do nothing else. This is required
        // by the custom-frame contract - the flyout will NOT appear if we skip it.
        let mut dwm_result = LRESULT(0);
        let dwm_handled = DwmDefWindowProc(hwnd, msg, wparam, lparam, &mut dwm_result).as_bool();
        if dwm_handled {
            return dwm_result;
        }

        match msg {
            // Keep the DWM frame anchored across activation + DPI changes (Microsoft
            // recommends re-extending here rather than only at creation). We then
            // fall through to default handling so Tauri/wry still see the message.
            WM_ACTIVATE | WM_DPICHANGED => {
                extend_frame(hwnd);
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            WM_NCHITTEST => {
                if let Some(code) = hit_test(hwnd, lparam) {
                    return LRESULT(code as isize);
                }
                // No special region: defer to the default (which, for a window
                // with no real non-client frame, returns HTCLIENT).
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            // The maximize button slot now reports HTMAXBUTTON (so the Win11 Snap
            // Layouts flyout appears on hover). Because that takes the press out of
            // the WebView's hands, the frontend's React onClick(toggleMaximize) no
            // longer fires for a plain click on it - so we toggle maximize here on
            // a press+release over HTMAXBUTTON, restoring the click-to-maximize
            // behavior. Snap Layouts itself is driven by DWM off the HTMAXBUTTON
            // hover (via DwmDefWindowProc above), independent of these messages.
            WM_NCLBUTTONDOWN if wparam.0 as u32 == HTMAXBUTTON => {
                // Swallow the down so DefWindowProc doesn't enter its own caption-
                // button tracking loop; we act on the up.
                LRESULT(0)
            }
            WM_NCLBUTTONUP if wparam.0 as u32 == HTMAXBUTTON => {
                toggle_maximize(hwnd);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                // Remove ourselves before the window goes away.
                let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            _ => DefSubclassProc(hwnd, msg, wparam, lparam),
        }
    }

    /// Decide the non-client hit-test code for a cursor at the screen point packed
    /// into `lparam`, or `None` to fall back to the default (HTCLIENT) behavior.
    ///
    /// Order matters: edge/corner resize bands win over the maximize button and
    /// caption (so you can still grab the very top edge to resize), then the
    /// maximize button slot (the Snap Layouts trigger), then the rest of the
    /// titlebar as caption (drag-to-move).
    unsafe fn hit_test(hwnd: HWND, lparam: LPARAM) -> Option<u32> {
        // WM_NCHITTEST packs a *screen* point: low 16 bits = x, high 16 = y, both
        // signed (windows can sit at negative coords on multi-monitor setups).
        let raw = lparam.0;
        let sx = (raw & 0xFFFF) as i16 as i32;
        let sy = ((raw >> 16) & 0xFFFF) as i16 as i32;

        // Whole-window rect in screen coordinates. For a frameless window the
        // client area fills this rect, so client point = screen point - top-left.
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }
        let win_w = rect.right - rect.left;
        let win_h = rect.bottom - rect.top;
        if win_w <= 0 || win_h <= 0 {
            return None;
        }
        // Cursor relative to the window's top-left.
        let x = sx - rect.left;
        let y = sy - rect.top;
        // Out of bounds (shouldn't happen for NCHITTEST, but be safe).
        if x < 0 || y < 0 || x >= win_w || y >= win_h {
            return None;
        }

        // Physical-pixel sizes from the logical constants via the window DPI.
        let dpi = GetDpiForWindow(hwnd);
        // GetDpiForWindow returns 0 only on an invalid HWND; default to 96 (1.0x).
        let scale = if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 };
        let border = (RESIZE_BORDER_LOGICAL * scale).round() as i32;
        let titlebar_h = (TITLEBAR_H_LOGICAL * scale).round() as i32;
        let maxbtn_left = win_w - (MAXBTN_LEFT_OFFSET_LOGICAL * scale).round() as i32;
        let maxbtn_right = win_w - (MAXBTN_RIGHT_OFFSET_LOGICAL * scale).round() as i32;

        // --- Edge / corner resize bands (skip when maximized: no edges to drag).
        if !is_maximized(hwnd) {
            let on_left = x < border;
            let on_right = x >= win_w - border;
            let on_top = y < border;
            let on_bottom = y >= win_h - border;
            let edge = match (on_top, on_bottom, on_left, on_right) {
                (true, _, true, _) => Some(HTTOPLEFT),
                (true, _, _, true) => Some(HTTOPRIGHT),
                (_, true, true, _) => Some(HTBOTTOMLEFT),
                (_, true, _, true) => Some(HTBOTTOMRIGHT),
                (true, _, _, _) => Some(HTTOP),
                (_, true, _, _) => Some(HTBOTTOM),
                (_, _, true, _) => Some(HTLEFT),
                (_, _, _, true) => Some(HTRIGHT),
                _ => None,
            };
            if let Some(code) = edge {
                return Some(code);
            }
        }

        // --- Inside the titlebar row?
        if y < titlebar_h {
            // The custom maximize button slot -> HTMAXBUTTON is what makes Win11
            // pop the Snap Layouts flyout on hover. Because reporting HTMAXBUTTON
            // moves this slot into the non-client area, the WebView no longer sees
            // a plain click here; the WM_NCLBUTTONDOWN/UP arm of `subclass_proc`
            // restores click-to-maximize for it.
            if x >= maxbtn_left && x < maxbtn_right {
                return Some(HTMAXBUTTON);
            }
            // Anywhere else along the titlebar row is draggable caption. This is
            // belt-and-braces alongside the frontend's `data-tauri-drag-region`;
            // returning HTCAPTION here makes the WHOLE row draggable at the OS
            // level (including over the tab strip's gaps), matching native windows.
            // NOTE: returning HTCAPTION over the custom buttons (settings/min/close)
            // would swallow their clicks, so we must NOT claim those slots. They
            // sit to the right of the tab strip; we only claim caption to the LEFT
            // of the rightmost three control slots + the settings gear (4 slots).
            let controls_left = win_w - (CONTROL_W_LOGICAL * 4.0 * scale).round() as i32;
            if x < controls_left {
                return Some(HTCAPTION);
            }
            // Over the min/close/settings buttons (and the maximize handled above):
            // let the WebView receive the click so the custom buttons work.
            return Some(HTCLIENT);
        }

        // Below the titlebar and not on an edge: normal client area.
        None
    }

    /// Whether the window is currently maximized (so we suppress edge-resize).
    unsafe fn is_maximized(hwnd: HWND) -> bool {
        IsZoomed(hwnd).as_bool()
    }

    /// Toggle maximize/restore via the standard `WM_SYSCOMMAND` path so the rest
    /// of the windowing stack (Tauri/wry state, the frontend's is_maximized icon
    /// swap) observes the change the same way it would for a native title bar.
    unsafe fn toggle_maximize(hwnd: HWND) {
        let cmd = if is_maximized(hwnd) {
            SC_RESTORE
        } else {
            SC_MAXIMIZE
        };
        let _ = SendMessageW(hwnd, WM_SYSCOMMAND, Some(WPARAM(cmd as usize)), Some(LPARAM(0)));
    }

    // Re-import RemoveWindowSubclass here (used only in the proc) to keep the
    // top-of-module `use` list focused on the install path.
    use windows::Win32::UI::Shell::RemoveWindowSubclass;
}
