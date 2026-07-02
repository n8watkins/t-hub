//! T-Hub Native - GPUI client entry point (native-pivot T4 scaffold).
//!
//! Boots a GPUI window titled "T-Hub Native" and, over the §1.3 `wire`
//! ControlClient, lists live sessions, streams `status://snapshot` (and every
//! other) events, and attaches the first live session to prove the PTY plane.
//! No terminal rendering yet - that is T5. This binary is the visual proof; the
//! `wire-probe` binary is the headless proof (list + events + attach + write +
//! resize + reconnect) for WSL where the GPUI backend may not build.

fn main() {
    #[cfg(feature = "gui")]
    {
        t_hub_native::app::run();
    }
    #[cfg(not(feature = "gui"))]
    {
        eprintln!(
            "t-hub-native was built without the `gui` feature (wire-only). \
             Run the `wire-probe` binary to exercise the §1.3 contract."
        );
    }
}
