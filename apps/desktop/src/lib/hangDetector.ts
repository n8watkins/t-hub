// Main-thread HANG DETECTOR (sporadic "Not Responding" / Alt-Tab ghost-icon hunt).
//
// SYMPTOM this catches: the WebView2 UI/JS thread is FULLY BLOCKED for multiple
// seconds, sporadically, during normal work. Windows ghosts a window whose thread
// fails to pump its message loop for ~5s, so the single UI thread is the suspect.
// A sporadic blocker can't be reliably found by reading code — so we INSTRUMENT
// the running app to catch it in the act and write attribution to the diag file.
//
// Two independent, low-overhead detectors, both ALWAYS ON (not behind the
// `t-hub.debug` tlog gate — we must capture a hang even when debug is off):
//
//   1) HEARTBEAT: a 500ms setInterval. Each tick measures how late it actually
//      fired vs. schedule. A tick that's late by > THRESHOLD means the thread was
//      blocked for ~that long (the event loop couldn't service the timer). This is
//      the PRIMARY signal — it directly measures the exact thing Windows measures
//      (message-pump starvation), and it fires even when a longtask entry doesn't.
//
//   2) PerformanceObserver(['longtask','event']): the browser/WebView2 reports any
//      single task that ran > 50ms as a `longtask` entry (with a coarse
//      attribution name), and slow input handling as `event` entries. Gives a
//      second, lower-floor view and helps attribute the blockage.
//
// Both write straight to the backend diag file via `invoke("diag_log", ...)`
// (NOT `tlog` — tlog is gated behind diagEnabled). The line is single-line JSON so
// the WSL-side orchestrator can `tail`/grep `<home>/.t-hub/diag.log`
// (Windows: %USERPROFILE%\.t-hub\diag.log, from WSL: /mnt/c/Users/<win>/.t-hub/diag.log).
//
// Mounted once as a side-effect import from src/main.tsx (next to statusMount).
import { invoke } from "@tauri-apps/api/core";

/** Log a heartbeat-detected stall only when a tick is late by at least this much.
 *  500ms is well below the ~5s ghost threshold (so we see the ramp, not just the
 *  catastrophic case) yet high enough to ignore ordinary GC/scheduling jitter. */
const HEARTBEAT_STALL_MS = 500;
/** Heartbeat cadence. Drift = (actual - expected) interval; a clean tick drifts
 *  a few ms. The interval is cheap: one Date.now() + a subtract per 500ms. */
const HEARTBEAT_PERIOD_MS = 500;
/** Only ship longtask entries at/above this; the spec floor is 50ms, but we only
 *  care about pump-threatening tasks, so raise it to cut log volume. */
const LONGTASK_LOG_MS = 200;

let mounted = false;

/** Fire-and-forget one already-built JSON line to the diag file. Mirrors diag.ts
 *  `shipToFile` but UNGATED: a hang must be recorded even with debug off. Never
 *  awaits, never throws (a thrown invoke in a plain browser dev server is fine). */
function ship(line: string): void {
  try {
    void invoke("diag_log", { line }).catch(() => {});
  } catch {
    /* no Tauri IPC (plain web/dev) — diagnostics must never break the app */
  }
}

/** Current JS heap, in MB, when the engine exposes it (Chromium/WebView2 do under
 *  performance.memory). Lets us correlate a stall with a GC / heap spike. */
function heapMB(): number | undefined {
  const mem = (performance as { memory?: { usedJSHeapSize?: number } }).memory;
  const used = mem?.usedJSHeapSize;
  return typeof used === "number" ? Math.round(used / 1048576) : undefined;
}

/** Idempotent app-startup mount. */
export function mountHangDetector(): void {
  if (mounted) return;
  mounted = true;
  if (typeof window === "undefined") return;

  // --- 1) HEARTBEAT: measure timer drift. A late tick == a blocked thread. -----
  let expected = Date.now() + HEARTBEAT_PERIOD_MS;
  setInterval(() => {
    const now = Date.now();
    const drift = now - expected; // how much LATER than scheduled this tick fired
    expected = now + HEARTBEAT_PERIOD_MS;
    if (drift >= HEARTBEAT_STALL_MS) {
      // `blockedMs` is the closest single number to "how long the pump starved".
      ship(
        JSON.stringify({
          t: "hang",
          src: "heartbeat",
          blockedMs: drift,
          ghostRisk: drift >= 5000, // Windows ghosts at ~5s
          at: new Date(now).toISOString(),
          heapMB: heapMB(),
        }),
      );
    }
  }, HEARTBEAT_PERIOD_MS);

  // --- 2) PerformanceObserver: attribute long tasks + slow input. --------------
  if (typeof PerformanceObserver !== "undefined") {
    try {
      const obs = new PerformanceObserver((list) => {
        for (const e of list.getEntries()) {
          const dur = Math.round(e.duration);
          if (e.entryType === "longtask") {
            if (dur < LONGTASK_LOG_MS) continue;
            // `attribution` (when present) names the frame/source of the task.
            const attr = (e as PerformanceEntry & {
              attribution?: { name?: string; containerType?: string }[];
            }).attribution?.[0];
            ship(
              JSON.stringify({
                t: "hang",
                src: "longtask",
                blockedMs: dur,
                ghostRisk: dur >= 5000,
                name: e.name,
                attr: attr?.name ?? attr?.containerType,
                heapMB: heapMB(),
              }),
            );
          } else if (e.entryType === "event") {
            // A slow input handler (typing into a busy terminal, a tab switch).
            const ev = e as PerformanceEntry & { processingEnd?: number };
            if (dur < LONGTASK_LOG_MS) continue;
            ship(
              JSON.stringify({
                t: "hang",
                src: "event",
                blockedMs: dur,
                name: e.name, // e.g. "keydown", "pointerdown", "click"
                heapMB: heapMB(),
              }),
            );
          }
        }
      });
      // `buffered:true` catches entries from before the observer attached (startup).
      obs.observe({ type: "longtask", buffered: true });
      // `event` needs its own observe call with a durationThreshold (defaults 104ms).
      try {
        obs.observe({
          type: "event",
          buffered: true,
          durationThreshold: LONGTASK_LOG_MS,
        } as PerformanceObserverInit);
      } catch {
        /* `event` timing unsupported here — longtask alone still covers it */
      }
    } catch {
      /* longtask unsupported — the heartbeat alone still catches the hang */
    }
  }

  // Startup marker so the orchestrator can confirm the detector is live.
  ship(JSON.stringify({ t: "hang", src: "mount", at: new Date().toISOString() }));
}

// Self-mount on import (side-effect module, mirroring statusMount / repaintMount).
// Kept ON by default so it keeps catching any residual/regression stall; it only
// writes when a real stall (>=200ms longtask / >=500ms heartbeat) occurs.
mountHangDetector();
