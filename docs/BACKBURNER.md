# Back burner

Deliberately deferred items — revisit "way later." Not bugs blocking daily use;
each has a reason it's parked. (Active, near-term work lives in the session
handoff / punch-list, not here.)

## 1. Windows 11 "Snap Layouts" maximize hover-flyout
**Status:** parked — the toggle works, the hover flyout does not.

When you hover the Windows maximize button, Win11 normally pops a flyout letting
you pick a snap arrangement (left/right halves, quadrants, etc.). T-Hub's
window is frameless, and despite `win_snap.rs` extending a DWM frame +
reporting `HTMAXBUTTON`, the flyout still doesn't appear. Maximize/restore by
click works fine. This is a deep frameless-window + DWM interaction; not worth
more time right now. (Edge/corner drag-resize via `win_snap.rs` is separate and
should keep working — re-park only the flyout.)

## 2. Web preview for localhost dev servers
**Status:** superseded by Package 3 source implementation; packaged Windows and WSL reachability acceptance remains open.

The original note recorded unreliable iframe reachability for WSL-bound dev servers and remains useful historical context.
Package 3 now provides a shared Preview discovery and lifecycle service with explicit UI, MCP, and CLI operations.
The service recognizes validated loopback and WSL-host representations, owns the selected URL, and keeps arbitrary conversation URLs untrusted.
The remaining work is live packaged acceptance for representative Vite, Next.js, static, and nested-monorepo targets, including WSL reachability and cleanup.
Use [UX-RELIABILITY-AND-PERFORMANCE-ITINERARY.md](./UX-RELIABILITY-AND-PERFORMANCE-ITINERARY.md) and [PACKAGE-5-LIVE-ACCEPTANCE.md](./PACKAGE-5-LIVE-ACCEPTANCE.md) as the active requirements.

---
*Add to this list as things get explicitly punted. Pull an item back up when it
starts mattering.*
