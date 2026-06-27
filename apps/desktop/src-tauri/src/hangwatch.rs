//! Host MAIN-THREAD hang watchdog (the sporadic "Not Responding" / Alt-Tab ghost).
//!
//! The JS-side `hangDetector` is BLIND to this freeze: in WebView2 the JS runs in a
//! separate RENDERER process, while Windows' hung-window ghosting is the HOST main
//! thread — the tao event loop that owns the HWND, pumps the Win32 message loop, and
//! dispatches every `app.emit` as a user-event (+ `ExecuteScript`). Evidence from a
//! live freeze: zero JS-side hang lines AND background worker threads (recent/usage)
//! kept logging — only the main thread stalled. So the detector must live here.
//!
//! Mechanism: a background thread posts a tiny closure to the main thread every
//! 500ms via `run_on_main_thread` and records how long that closure WAITED to run —
//! i.e. how long the main-thread queue/pump was starved (the exact thing Windows
//! measures for the ~5s ghost). It also reports how many hot-path emits landed during
//! the wait, to correlate a block with an emit-dispatch burst (the leading
//! hypothesis). Logs single-line JSON via [`crate::diag::diag_log`], shape-matching
//! the JS detector's `{"t":"hang",...}` lines so one grep catches both.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Bumped on every hot-path webview-bound `app.emit` (terminal output + the
/// `control://event` re-emit, which carries the whole bridge/supervision/status
/// stream). The watchdog reports the delta observed DURING a detected block.
pub static EMIT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Count one hot-path emit. Cheap relaxed increment; called from the emit sites.
#[inline]
pub fn note_emit() {
    EMIT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Probe cadence + the "this was a stall" threshold (well under the ~5s ghost line so
/// we see the ramp, above ordinary scheduling jitter).
const PERIOD: Duration = Duration::from_millis(500);
const STALL_MS: u128 = 500;

/// Spawn the watchdog thread. Call once from `.setup`.
pub fn spawn(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(PERIOD);
        let sent = Instant::now();
        let emits_at_send = EMIT_COUNT.load(Ordering::Relaxed);
        // Post a probe to the main thread; it runs once the pump services it. If the
        // main thread is blocked, the probe queues behind whatever is blocking it, so
        // `waited` measures the block. (Err only if the loop is shutting down.)
        let _ = app.run_on_main_thread(move || {
            let waited = sent.elapsed().as_millis();
            if waited >= STALL_MS {
                let emits = EMIT_COUNT
                    .load(Ordering::Relaxed)
                    .wrapping_sub(emits_at_send);
                crate::diag::diag_log(format!(
                    "{{\"t\":\"hang\",\"src\":\"rust-main\",\"blockedMs\":{},\"ghostRisk\":{},\"emitsDuringBlock\":{}}}",
                    waited,
                    waited >= 5000,
                    emits
                ));
            }
        });
    });
}
