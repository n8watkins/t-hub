//! **Cockpit chrome** for the native client (native-pivot T8).
//!
//! The window stops being a demo grid and becomes the cockpit: a LEFT SIDEBAR
//! listing the workspaces (switch / create / rename / close - the webview's
//! long-standing design; workspace navigation is NOT a top tab strip), the
//! per-workspace tile grid with the webview's real auto-grid semantics
//! (`Canvas.tsx` `splitRows()`), and per-tile headers (title, session id,
//! geometry, liveness cue, close) replacing the T5 debug line. Below the
//! workspace section the sidebar reserves a mount area for the T9 overlay
//! sections (recents / usage / metrics / supervision) - see
//! [`model::SidebarLayout::overlay_mount`].
//!
//! ## Module split (mirrors the T5 term/render split)
//! - [`model`] - the gpui-free chrome state machine: which tabs exist, which tiles
//!   they hold, which tab is active, which tile is focused, tab-rename editing,
//!   the `splitRows` layout math, and live-session reconciliation. Unit-tested
//!   under `--no-default-features`.
//! - [`persist`] - the client-owned layout file (the SERVER owns sessions; the
//!   CLIENT owns layout, decision D5): tabs + active tab as JSON at
//!   `~/.t-hub/native-layout.json` (`THN_LAYOUT` overrides).
//! - [`windows`] - the gpui-free satellite-window registry (T10): which
//!   workspaces are torn off into their own OS windows, each window's focused
//!   tile and last known bounds. Unit-tested under `--no-default-features`.
//! - [`view`] (feature `gui`) - the GPUI `CockpitView` and `SatelliteView`:
//!   paint the sidebar, headers and grids, route input, and delegate every
//!   tile's terminal content to `render::sync_and_paint_content` (the row-paint
//!   seam T6 owns).
//!
//! ## The persistent-pool insight
//! Every tile in every workspace keeps its PTY attached; switching tabs only
//! changes which workspace is *painted*. Hidden tiles keep feeding their
//! `TermSession`s (damage accumulates), so a tab switch is instant - no
//! attach/detach churn, no scrollback re-seed.

pub mod cues;
pub mod model;
pub mod persist;
pub mod windows;

#[cfg(feature = "gui")]
pub mod view;
