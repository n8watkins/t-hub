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
//! `PERIOD` via `run_on_main_thread`, and each run records the GAP since the
//! previous run — i.e. how long the main-thread queue/pump was starved between two
//! services (the exact thing Windows measures for the ~5s ghost). Measuring the
//! inter-run gap (rather than a single probe's tail wait) catches a stall regardless
//! of its phase and emits ONE line per stall instead of a burst. It also reports the
//! hot-path emit delta over that gap, to correlate a block with an emit-dispatch
//! burst. Logs single-line JSON via [`crate::diag::diag_log`], shape-matching the JS
//! detector's `{"t":"hang",...}` lines so one grep catches both.
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

/// Probe cadence + the stall threshold, measured as the GAP between consecutive
/// main-thread probe RUNS. LOWERED to hunt the residual "staggered" sub-500ms
/// stutter (A1): probe every 100ms and log whenever two consecutive probe runs land
/// >=200ms apart — i.e. the main-thread pump was starved for ~PERIOD beyond the
/// > normal cadence. With PERIOD=100 a true block of duration D yields a gap in
/// > [D, D+PERIOD], so a >=200ms stall is RELIABLY caught regardless of phase (the old
/// > per-probe "remaining wait" metric only saw one probe's tail and missed
/// > sub-PERIOD-aligned blocks). The gap metric also emits ONE line per stall: the
/// > queued probes that burst out right after a block see a ~0 gap. Raise back toward
/// > 500 once the culprit is fixed to quiet the log.
const PERIOD: Duration = Duration::from_millis(100);
const STALL_MS: u64 = 200;

/// Spawn the watchdog thread. Call once from `.setup`.
pub fn spawn(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // The main-thread probe must do NO blocking work — it runs on the very thread
        // it measures, so a diag_log file write THERE would lengthen the block it's
        // timing. So the probe only computes the gap since the PREVIOUS probe run +
        // the emit delta, then hands the numbers back over this channel; the
        // (blocking) diag_log write happens HERE, on the watchdog's own bg thread.
        let (tx, rx) = std::sync::mpsc::channel::<(u64, u64)>();
        let start = Instant::now();
        // Shared across probe closures (only the main thread mutates them, so the two
        // swaps never race): ms-since-`start` of the last probe RUN, and the emit
        // count at that run. The gap between consecutive runs measures the FULL
        // main-thread starvation (independent of when within a block a probe was
        // posted); a 0 sentinel skips the first probe (no prior run to diff).
        let last_run = std::sync::Arc::new(AtomicU64::new(0));
        let last_emit = std::sync::Arc::new(AtomicU64::new(0));
        loop {
            // Drain + log any stalls the probe handed back (off the main thread).
            while let Ok((gap, emits)) = rx.try_recv() {
                crate::diag::diag_log(format!(
                    "{{\"t\":\"hang\",\"src\":\"rust-main\",\"blockedMs\":{},\"ghostRisk\":{},\"emitsDuringBlock\":{}}}",
                    gap,
                    gap >= 5000,
                    emits
                ));
            }
            std::thread::sleep(PERIOD);
            let tx = tx.clone();
            let last_run = last_run.clone();
            let last_emit = last_emit.clone();
            // Post a probe to the main thread; it runs once the pump services it. The
            // GAP between this run and the previous one spans any block that starved
            // the pump in between. (Err only if the loop is shutting down.)
            let _ = app.run_on_main_thread(move || {
                let now_ms = start.elapsed().as_millis() as u64;
                let now_emits = EMIT_COUNT.load(Ordering::Relaxed);
                let prev_ms = last_run.swap(now_ms, Ordering::Relaxed);
                let prev_emits = last_emit.swap(now_emits, Ordering::Relaxed);
                if prev_ms != 0 {
                    let gap = now_ms.saturating_sub(prev_ms);
                    if gap >= STALL_MS {
                        let emits = now_emits.wrapping_sub(prev_emits);
                        let _ = tx.send((gap, emits)); // non-blocking handoff; NO I/O here
                    }
                }
            });
        }
    });
}
