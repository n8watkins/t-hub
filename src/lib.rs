//! T-Hub native client (render-pivot). See `docs/NATIVE-PIVOT-EXECUTION.md`.
//!
//! The `wire` module is the ControlClient contract (§1.3) and is graphics-free, so
//! it compiles and unit-tests in WSL independent of the GPUI backend. The `app`
//! module (feature `gui`) is the GPUI window; it is what T5 will grow into the
//! real render seam.

pub mod wire;

#[cfg(feature = "gui")]
pub mod app;
