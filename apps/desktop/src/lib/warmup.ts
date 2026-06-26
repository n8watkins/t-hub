// Shared startup-warmup factory — the "swallow the journal-replay burst" window
// used by lib/notify.ts and lib/rulesMount.ts.
//
// On the FIRST connect, the agent replays its event journal: every existing
// session re-emits its last status (and historical exits/errors fire too). With
// many sessions that is a wall of chimes / a flurry of rule fires at launch. The
// callers treat the launch window as "warmup": events are RECORDED (so the dedup
// baseline is seeded) but NOT acted on. Two mechanisms end the window:
//   - a GRACE window: a quiet interval after the last event, re-armed on each
//     event so a slow burst stays covered. It can only end warmup EARLIER.
//   - an ABSOLUTE deadline (`initialMs` after `start()`): a hard cap captured
//     ONCE, enforced directly by `inWarmup()` (not by a clearable timer), so a
//     sustained event stream can NEVER keep warmup muted past it.
//
// This is the (fixed) absolute-cap behavior both call sites already had; the
// factory just centralizes it. Each call site keeps its own dedup map + event
// wiring — only this timing machinery is shared.

/** Tuning for one warmup window. */
export interface WarmupOptions {
  /** Hard cap (ms after `start()`): warmup always ends by this absolute
   *  deadline, regardless of how many events keep arriving. */
  initialMs: number;
  /** Quiet interval (ms) after the last event before warmup ends EARLY — the
   *  grace window, re-armed on each `inWarmup()` call. */
  graceMs: number;
  /** Duration (ms) of the FIRST grace arm done by `start()`. Defaults to
   *  `graceMs`. (lib/rulesMount.ts arms its initial window with `initialMs`,
   *  which it passes here so its behavior is preserved exactly.) */
  initialGraceMs?: number;
}

/** A warmup window: `start()` it once at install, then gate side effects on
 *  `inWarmup()` (true => record state but skip the action). */
export interface Warmup {
  /** Begin warmup: record the absolute deadline (once) and arm the initial grace
   *  window. After this, warmup is guaranteed to end by the absolute deadline
   *  even under a never-ending event stream. In a non-browser/SSR context
   *  (`window === undefined`) warmup never activates (start() leaves it off). */
  start: () => void;
  /** True while we're still swallowing the startup replay. Re-arms the grace
   *  window on each call so a slow burst stays covered, but returns false the
   *  moment the absolute deadline passes — so a sustained event stream can never
   *  keep us muted past `initialMs`. */
  inWarmup: () => boolean;
}

/** Create an independent warmup window with the EXACT absolute-cap semantics the
 *  two call sites share: the grace timer can only END warmup early, while the
 *  absolute deadline (`now >= warmupDeadline`) is enforced inside `inWarmup()`
 *  itself so re-arming the grace window can never push past it. */
export function createWarmup({
  initialMs,
  graceMs,
  initialGraceMs,
}: WarmupOptions): Warmup {
  // The first grace arm uses `initialGraceMs` when given, else `graceMs`.
  const firstGraceMs = initialGraceMs ?? graceMs;

  let warmupActive = true;
  // Epoch ms at which warmup must be over, regardless of later events. Set once
  // in `start()`; `inWarmup()` treats `now >= deadline` as over even if the grace
  // timer hasn't fired yet, so it's a true hard cap. 0 until `start()` runs.
  let warmupDeadline = 0;
  // The single grace timer. It can only END warmup early (after a quiet
  // interval); the absolute deadline is enforced by the `now >= warmupDeadline`
  // check, NOT by this clearable timer, so re-arming can't push warmup out.
  let graceTimer: ReturnType<typeof setTimeout> | undefined;

  /** (Re)arm the grace timer so warmup ends `ms` after the last event. Only ever
   *  SHORTENS the window — it cannot extend past the absolute deadline. */
  function armGrace(ms: number): void {
    if (graceTimer) clearTimeout(graceTimer);
    graceTimer = setTimeout(() => {
      warmupActive = false;
    }, ms);
  }

  function start(): void {
    if (typeof window === "undefined") {
      warmupActive = false;
      return;
    }
    warmupDeadline = Date.now() + initialMs;
    armGrace(firstGraceMs);
  }

  function inWarmup(): boolean {
    if (!warmupActive) return false;
    if (Date.now() >= warmupDeadline) {
      // Absolute cap reached: latch off and stop covering further events.
      warmupActive = false;
      if (graceTimer) {
        clearTimeout(graceTimer);
        graceTimer = undefined;
      }
      return false;
    }
    armGrace(graceMs);
    return true;
  }

  return { start, inWarmup };
}
