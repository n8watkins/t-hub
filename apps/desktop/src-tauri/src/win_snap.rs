//! Windows 11 Snap Layouts + native edge-resize for the frameless main window.
//!
//! T-Hub's main window is frameless (`decorations: false`, see
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
//! ## Geometry — frontend-reported maximize-button rect (the durable fix)
//! The maximize button's exact bounds are NOT hard-coded here. Pixel constants
//! mirrored from the React layout are fragile: they silently desync if the
//! controls are rearranged (e.g. a Settings gear later joins the top-right
//! cluster) and they can simply be *wrong* on real Win11 (the rendered button is
//! wherever flexbox + the live tab strip + DPI rounding put it, which a fixed
//! "two control-widths from the right edge" guess only approximates). When the
//! `HTMAXBUTTON` region misses the visible button by even a few px, the Snap
//! Layouts flyout never triggers.
//!
//! So the FRONTEND owns the geometry: `src/components/Titlebar.tsx` refs the
//! maximize `<button>`, and on mount / resize / DPI change / maximize-state
//! change it computes `getBoundingClientRect()`, scales it to **physical pixels
//! relative to the window's top-left** (× the Tauri window scale factor), and
//! pushes it to the backend via the [`set_maximize_button_rect`] Tauri command
//! (see `commands::set_maximize_button_rect` registered in `lib.rs`). We stash
//! the latest rect in a process-global ([`MAX_BUTTON_RECT`]) and `WM_NCHITTEST`
//! reports `HTMAXBUTTON` only when the (window-relative, physical-px) cursor
//! point lands inside it. Until the frontend reports a rect (the brief window
//! before React mounts), no slot is claimed — hover does nothing, which is the
//! safe default. The titlebar HEIGHT is still needed for the caption / edge-band
//! logic and remains a constant ([`TITLEBAR_H_LOGICAL`]), matching the `h-8` row;
//! that one is stable and not tied to the controls' horizontal arrangement.
//!
//! Coordinate contract (important, easy to get wrong):
//!   * `WM_NCHITTEST` lparam is a **screen** point in **physical** px. `hit_test`
//!     subtracts `GetWindowRect().left/top` to get a point **relative to the
//!     window's top-left**, still in physical px.
//!   * The frontend's `getBoundingClientRect()` is CSS px relative to the
//!     **webview client** top-left. For this frameless window tao collapses the
//!     non-client area (answers `WM_NCCALCSIZE` with 0), so the client rect ==
//!     the window rect: client-relative == window-relative. Multiplying by the
//!     scale factor yields physical px relative to the window top-left — the
//!     SAME space as the backend's `(x, y)`. No screen-position plumbing (and
//!     thus no multi-monitor offset math) is needed on either side.

/// The maximize button's rectangle, in **physical pixels relative to the
/// window's top-left** (NOT screen coords; see the module-level coordinate
/// contract). Reported by the frontend via [`set_maximize_button_rect`]; read by
/// the `WM_NCHITTEST` handler on Windows. `serde`-derived so it crosses the Tauri
/// IPC boundary as the `{ x, y, width, height }` JSON the frontend sends.
///
/// All fields are physical px. `width`/`height` are `f64` so a sub-pixel
/// `getBoundingClientRect()` × scale survives the wire without premature
/// rounding; the hit-test rounds once, at compare time.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
// On unix nothing reads the fields back (only the `#[cfg(windows)]` hit-test
// does), so the otherwise-correct dead-code lint would fire on the Linux build.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct MaxButtonRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The latest maximize-button rect reported by the frontend, or `None` before
/// React has mounted / reported one. A plain global `Mutex<Option<…>>` is enough:
/// T-Hub's frameless main window is the only window that installs the subclass,
/// the rect is tiny + `Copy`, and writes (a handful, on resize/DPI/maximize) and
/// reads (one per `WM_NCHITTEST`) never contend meaningfully. Cross-platform so
/// the command compiles on unix; only the Windows hit-test actually reads it.
static MAX_BUTTON_RECT: std::sync::Mutex<Option<MaxButtonRect>> = std::sync::Mutex::new(None);

/// Store the maximize-button rect reported by the frontend. Cross-platform (the
/// command must compile on unix); on unix nothing reads it back, so it is inert.
/// A poisoned lock is recovered from — a stale rect must never be fatal.
pub fn store_max_button_rect(rect: MaxButtonRect) {
    let mut guard = MAX_BUTTON_RECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(rect);
}

/// Tauri command: the frontend reports the maximize button's current rect (in
/// physical px relative to the window top-left). Registered in `lib.rs`'s
/// `invoke_handler`. Infallible from the frontend's perspective: it just updates
/// the global the Windows hit-test consults. No-op effect on unix.
#[tauri::command]
pub fn set_maximize_button_rect(rect: MaxButtonRect) {
    store_max_button_rect(rect);
}

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

    use super::MAX_BUTTON_RECT;

    /// Titlebar height in *logical* (CSS) pixels - must match the `h-8` row in
    /// `src/components/Titlebar.tsx`. Still a constant: the row height is stable
    /// and (unlike the controls' horizontal positions) not affected by adding /
    /// rearranging top-right buttons. Used for the caption + edge-band logic.
    const TITLEBAR_H_LOGICAL: f64 = 32.0;
    /// Thickness (logical px) of the invisible window-edge band that triggers a
    /// native resize. A few px wider than the OS default makes the frameless
    /// edges easier to grab without eating into the content.
    const RESIZE_BORDER_LOGICAL: f64 = 6.0;

    /// A private, stable subclass id (any constant unique within this HWND's
    /// subclass chain works; "THSN" = T-Hub SNap, as ASCII bytes).
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
                "t-hub: win_snap installed (subclass + DWM frame) on HWND {:?}",
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
            eprintln!("t-hub: win_snap DwmExtendFrameIntoClientArea failed: {e}");
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
        // BUG 3 FIX (max/restore did nothing on Win11): the maximize-button CLICK
        // must be handled by US, BEFORE DWM. We report HTMAXBUTTON for that slot so
        // the Win11 Snap Layouts flyout appears on hover (DWM drives that off the
        // hover/move messages, below). But that also moves the slot into the
        // non-client area, so the WebView's React onClick(toggleMaximize) never
        // fires. We therefore toggle maximize ourselves on a press+release over
        // HTMAXBUTTON. Previously these click messages fell THROUGH to
        // `DwmDefWindowProc` first; DWM returns "handled" for caption-button
        // messages on a tao frameless window WITHOUT actually maximizing (tao
        // answers WM_NCCALCSIZE with 0, so there's no real caption frame to drive
        // the maximize), and we returned that result early - so our toggle arm
        // never ran and the button was inert. Intercepting the click here (ahead of
        // DWM) restores click-to-maximize while leaving the hover/flyout path
        // (every OTHER message, including the NC mouse-move that pops the flyout)
        // untouched.
        if wparam.0 as u32 == HTMAXBUTTON {
            match msg {
                // Swallow the down so DefWindowProc doesn't enter its own caption-
                // button tracking loop; we act on the up.
                WM_NCLBUTTONDOWN => return LRESULT(0),
                WM_NCLBUTTONUP => {
                    toggle_maximize(hwnd);
                    return LRESULT(0);
                }
                _ => {}
            }
        }

        // Let DWM handle the message first. For caption-button messages (incl. the
        // maximize-button HOVER that drives Snap Layouts) it returns TRUE and fills
        // `dwm_result`; we then return that and do nothing else. This is required
        // by the custom-frame contract - the flyout will NOT appear if we skip it.
        // (The maximize-button CLICK is handled above, ahead of this, so DWM can't
        // swallow it.)
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
            // NOTE: the maximize-button click (WM_NCLBUTTONDOWN/UP over
            // HTMAXBUTTON) is intercepted at the TOP of this proc, ahead of
            // DwmDefWindowProc, so those arms are not repeated here.
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
        // The maximize-button slot in window-relative physical px, as REPORTED by
        // the frontend (`set_maximize_button_rect`). `None` until React mounts +
        // reports it, in which case we claim no HTMAXBUTTON slot (safe default).
        let maxbtn = max_button_slot();

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
            // restores click-to-maximize for it. The slot is the exact
            // frontend-reported rect (no hand-mirrored constants).
            if let Some(r) = maxbtn {
                if r.contains(x, y) {
                    return Some(HTMAXBUTTON);
                }
            }
            // Anywhere else along the titlebar row is draggable caption. This is
            // belt-and-braces alongside the frontend's `data-tauri-drag-region`;
            // returning HTCAPTION here makes the WHOLE row draggable at the OS
            // level (including over the tab strip's gaps), matching native windows.
            // NOTE: returning HTCAPTION over the custom buttons (min/close, and the
            // maximize handled above) would swallow their clicks, so we must NOT
            // claim those slots. The window controls cluster (min / max / close)
            // sits at the top-right; we anchor "where the controls begin" to the
            // LEFT edge of the reported maximize-button rect (min is to the left of
            // max, close to the right, all the same width). Everything to the LEFT
            // of that is caption (drag); everything from there rightward is HTCLIENT
            // so the webview's own buttons receive their clicks.
            match maxbtn {
                Some(r) if x < r.left => return Some(HTCAPTION),
                Some(_) => return Some(HTCLIENT),
                // No reported rect yet (the brief pre-mount window): don't risk
                // stealing a control click, so return HTCLIENT for the whole row.
                // The frontend's `data-tauri-drag-region` attributes still drive
                // window drag where appropriate until the rect arrives.
                None => return Some(HTCLIENT),
            }
        }

        // Below the titlebar and not on an edge: normal client area.
        None
    }

    /// An axis-aligned hit rectangle in window-relative physical px (ints).
    struct Slot {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    impl Slot {
        /// Half-open containment: `[left, right) × [top, bottom)`, matching the
        /// edge conventions used for the resize bands / titlebar row.
        fn contains(&self, x: i32, y: i32) -> bool {
            x >= self.left && x < self.right && y >= self.top && y < self.bottom
        }
    }

    /// The frontend-reported maximize-button rect, converted to a window-relative
    /// physical-px [`Slot`] (rounded once, here). `None` until the frontend has
    /// reported a rect (`set_maximize_button_rect`) — before React mounts, or if a
    /// degenerate (zero-area / negative) rect was sent, in which case we claim no
    /// slot rather than guess. A poisoned lock is recovered from (a stale read is
    /// harmless and must never panic the window proc).
    fn max_button_slot() -> Option<Slot> {
        let guard = MAX_BUTTON_RECT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let r = (*guard)?;
        // Round the (already physical-px) rect to integer pixels. Reject anything
        // non-finite or non-positive so a bad report can't produce a giant or
        // inverted slot.
        if !(r.x.is_finite() && r.y.is_finite() && r.width.is_finite() && r.height.is_finite()) {
            return None;
        }
        if r.width <= 0.0 || r.height <= 0.0 {
            return None;
        }
        let left = r.x.round() as i32;
        let top = r.y.round() as i32;
        let right = (r.x + r.width).round() as i32;
        let bottom = (r.y + r.height).round() as i32;
        if right <= left || bottom <= top {
            return None;
        }
        Some(Slot {
            left,
            top,
            right,
            bottom,
        })
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
