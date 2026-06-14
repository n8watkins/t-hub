# Back burner

Deliberately deferred items — revisit "way later." Not bugs blocking daily use;
each has a reason it's parked. (Active, near-term work lives in the session
handoff / punch-list, not here.)

## 1. Windows 11 "Snap Layouts" maximize hover-flyout
**Status:** parked — the toggle works, the hover flyout does not.

When you hover the Windows maximize button, Win11 normally pops a flyout letting
you pick a snap arrangement (left/right halves, quadrants, etc.). TermHub's
window is frameless, and despite `win_snap.rs` extending a DWM frame +
reporting `HTMAXBUTTON`, the flyout still doesn't appear. Maximize/restore by
click works fine. This is a deep frameless-window + DWM interaction; not worth
more time right now. (Edge/corner drag-resize via `win_snap.rs` is separate and
should keep working — re-park only the flyout.)

## 2. Web preview for localhost dev servers
**Status:** parked — unreliable, and unclear it matters.

The in-app web preview is an `<iframe>`. External sites (google.com) can't load
(they send `X-Frame-Options`). Localhost *should* frame, but a dev server bound
inside WSL (e.g. a port like 7223) isn't reliably reachable from the Windows
WebView2 as `localhost:<port>` — WSL2 localhost-forwarding + bind address
(`127.0.0.1` vs `0.0.0.0`) gets in the way. The file VIEWER is the valued part;
the web preview is a nice-to-have. Revisit only if in-app dev-server preview
becomes important — likely needs a native child webview and/or a WSL→Windows
port bridge. The "Open externally" button is the current escape hatch.

---
*Add to this list as things get explicitly punted. Pull an item back up when it
starts mattering.*
