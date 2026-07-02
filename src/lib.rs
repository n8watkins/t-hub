//! T-Hub native client (render-pivot). See `docs/NATIVE-PIVOT-EXECUTION.md`.
//!
//! The `wire` module is the ControlClient contract (§1.3) and is graphics-free, so
//! it compiles and unit-tests in WSL independent of the GPUI backend. The `app`
//! module (feature `gui`) is the GPUI window; it is what T5 will grow into the
//! real render seam.

pub mod wire;

/// Terminal emulation core (T5). gpui-free, so it compiles and unit-tests under
/// `--no-default-features` the same way `wire` does.
pub mod term;

/// Font subsystem (T7): glyph classification, procedural box-drawing/Powerline
/// sprite geometry, row segmentation, per-tile font config, and the torture-test
/// fixture. gpui-free, so it compiles and unit-tests under
/// `--no-default-features`; the GPUI glue lives in `render`.
pub mod font;

/// gpui-free render helpers (key encoding, layout math) - split out of `render` so
/// they unit-test in WSL without linking the graphics backend.
pub mod render_support;

#[cfg(feature = "gui")]
pub mod app;

/// Grid rendering (T5 render seam). GPUI-dependent, so gated behind `gui`.
#[cfg(feature = "gui")]
pub mod render;
