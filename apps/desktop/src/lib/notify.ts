// Notification sounds + desktop notifications for key session events.
//
// Frontend-first by design: this runs entirely inside WebView2, so it works the
// same on Windows and (dev) Linux without any OS-specific Rust. Sounds are
// SYNTHESIZED with the WebAudio API — no binary assets are bundled. Desktop
// notifications use the Tauri notification plugin *if it's installed*; if not,
// we degrade gracefully to sound-only (see the dynamic import in `osNotify`).
//
// Wiring: `installSessionNotifications()` subscribes once to the backend session
// events (see ./client05 + ./client) and maps them onto three notification
// "kinds". Both sounds and notifications respect the persisted settings toggles
// (`soundsEnabled` / `notificationsEnabled`) read live from the settings store.

import { useSettings } from "../store/settings";
import { onSessionStatus, onSupervision } from "../ipc/client05";
import { onExit, onState } from "../ipc/client";
import type { SessionStatus } from "../ipc/model";
import { createWarmup } from "./warmup";
import { captainSubjectForSession } from "./captainAttribution";

/** The three event flavors the rest of the app fires. */
export type NotifyKind = "attention" | "done" | "error";

// ---------------------------------------------------------------------------
// Sound — synthesized chimes (no bundled assets)
// ---------------------------------------------------------------------------

/** Lazily-created shared AudioContext. Created on first sound so we don't open
 *  an audio device until something actually plays (and so it's created inside a
 *  user-gesture-adjacent path on browsers that gate autoplay). */
let audioCtx: AudioContext | null = null;

function getAudioContext(): AudioContext | null {
  if (typeof window === "undefined") return null;
  try {
    if (!audioCtx) {
      const Ctor =
        window.AudioContext ??
        (window as unknown as { webkitAudioContext?: typeof AudioContext })
          .webkitAudioContext;
      if (!Ctor) return null;
      audioCtx = new Ctor();
    }
    // A context can be left "suspended" by autoplay policy until a gesture
    // resumes it; best-effort resume so a chime isn't silently dropped.
    if (audioCtx.state === "suspended") void audioCtx.resume();
    return audioCtx;
  } catch {
    return null;
  }
}

/** One short tone in a chime: frequency (Hz), start offset (s), duration (s). */
interface Tone {
  freq: number;
  at: number;
  dur: number;
}

/** Per-kind chime recipes. Kept short and quiet — these are ambient cues, not
 *  alarms. `attention`/`done` are soft two-note rises; `error` is a lower,
 *  more insistent descending pair. */
const CHIMES: Record<NotifyKind, Tone[]> = {
  // Soft "someone needs you" — gentle two-note rise.
  attention: [
    { freq: 660, at: 0, dur: 0.12 },
    { freq: 880, at: 0.1, dur: 0.16 },
  ],
  // Pleasant "turn finished" — bright major third.
  done: [
    { freq: 784, at: 0, dur: 0.12 },
    { freq: 988, at: 0.1, dur: 0.18 },
  ],
  // Alert "something failed" — lower descending pair, a touch louder.
  error: [
    { freq: 440, at: 0, dur: 0.16 },
    { freq: 330, at: 0.14, dur: 0.22 },
  ],
};

const PEAK_GAIN: Record<NotifyKind, number> = {
  attention: 0.18,
  done: 0.2,
  error: 0.28,
};

function playTone(ctx: AudioContext, tone: Tone, peak: number, base: number) {
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.type = "sine";
  osc.frequency.value = tone.freq;

  const start = base + tone.at;
  const end = start + tone.dur;
  // Quick attack, smooth exponential release — avoids the click of a hard stop.
  gain.gain.setValueAtTime(0.0001, start);
  gain.gain.exponentialRampToValueAtTime(peak, start + 0.012);
  gain.gain.exponentialRampToValueAtTime(0.0001, end);

  osc.connect(gain).connect(ctx.destination);
  osc.start(start);
  osc.stop(end + 0.02);
}

/** Play the short bundled-by-synthesis chime for `kind`. No-op when sounds are
 *  disabled in settings or no AudioContext is available (e.g. SSR/headless). */
export function playSound(kind: NotifyKind): void {
  if (!useSettings.getState().soundsEnabled) return;
  const ctx = getAudioContext();
  if (!ctx) return;
  const base = ctx.currentTime + 0.01;
  const peak = PEAK_GAIN[kind];
  for (const tone of CHIMES[kind]) playTone(ctx, tone, peak, base);
}

// ---------------------------------------------------------------------------
// Desktop notification — optional Tauri plugin, graceful fallback
// ---------------------------------------------------------------------------

// `@tauri-apps/plugin-notification` IS a dependency now (package.json +
// tauri-plugin-notification in Cargo.toml), so OS toasts fire. We still load it
// DYNAMICALLY and tolerate its absence so a plain `pnpm dev` (no Tauri host) or a
// build without the plugin still compiles + runs (sound-only). `@vite-ignore`
// keeps Vite from trying to pre-bundle a module that may be absent at build time.
type NotificationModule = {
  isPermissionGranted: () => Promise<boolean>;
  requestPermission: () => Promise<"granted" | "denied" | "default">;
  sendNotification: (opts: { title: string; body?: string }) => void;
};

let notifModule: NotificationModule | null | undefined;

async function loadNotificationModule(): Promise<NotificationModule | null> {
  if (notifModule !== undefined) return notifModule;
  try {
    // The specifier is held in a variable so TypeScript does NOT statically
    // resolve it — the plugin is an *optional* dependency that may be absent at
    // build time. `@vite-ignore` likewise stops Vite from trying to pre-bundle
    // a module that might not exist. When the plugin IS installed this resolves
    // normally at runtime; otherwise the catch puts us in sound-only mode.
    const specifier = "@tauri-apps/plugin-notification";
    notifModule = (await import(
      /* @vite-ignore */ specifier
    )) as unknown as NotificationModule;
  } catch {
    // Plugin not installed (or not running under Tauri) — sound-only mode.
    notifModule = null;
  }
  return notifModule;
}

async function osNotify(title: string, body: string): Promise<void> {
  if (!useSettings.getState().notificationsEnabled) return;
  const mod = await loadNotificationModule();
  if (!mod) return; // graceful fallback: sound already played
  try {
    let granted = await mod.isPermissionGranted();
    if (!granted) granted = (await mod.requestPermission()) === "granted";
    if (granted) mod.sendNotification({ title, body });
  } catch {
    // Permission flow / transport failed — non-fatal, sound still played.
  }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/** Fire a notification of `kind`: always plays the matching chime (if sounds
 *  are enabled) and shows a desktop notification (if enabled + plugin present).
 *  Both halves independently respect their settings toggle. */
export function notify(kind: NotifyKind, title: string, body = ""): void {
  playSound(kind);
  void osNotify(title, body);
}

// ---------------------------------------------------------------------------
// Session-event wiring
// ---------------------------------------------------------------------------

/** Map an FR-012 session status onto a notification, or null to stay silent.
 *  Only *actionable* transitions notify — `working`/`detached`/`restoring`/etc.
 *  are routine and would be noisy.
 *
 *  `subject` NAMES the originating captain ("Captain alpha", or the orchestrator
 *  brand) when the session is a captain, so the general can tell WHICH captain
 *  wants attention. Null for a regular session → the generic wording stands. */
function statusToNotification(
  status: SessionStatus,
  subject?: string | null,
): { kind: NotifyKind; title: string; body: string } | null {
  // The actor named in the notification: the captain's ship when known, else a
  // generic noun so non-captain sessions read exactly as before.
  const who = subject ?? "Claude";
  const whoSession = subject ?? "A session";
  const whoAgent = subject ?? "An agent";
  switch (status) {
    // The "Claude is asking for input" signals — exactly the asks-signal the
    // task hoped for. These ARE first-class statuses on the bridge.
    case "needsQuestion":
      return {
        kind: "attention",
        title: `${who} needs an answer`,
        body: `${whoSession} is waiting on your input.`,
      };
    case "needsPermission":
      return {
        kind: "attention",
        title: `${who} needs permission`,
        body: `${whoSession} is asking to use a tool.`,
      };
    // A turn finished cleanly.
    case "completed":
      return {
        kind: "done",
        title: subject ? `${subject} completed` : "Session completed",
        body: `${whoAgent} finished its turn.`,
      };
    // Hard failure — alert sound + notification.
    case "failed":
      return {
        kind: "error",
        title: subject ? `${subject} failed` : "Session failed",
        body: subject
          ? `${subject}'s run ended with an error.`
          : "An agent run ended with an error.",
      };
    case "rateLimited":
      return {
        kind: "error",
        title: "Rate limited",
        body: `${whoSession} hit a rate limit.`,
      };
    default:
      // working / waitingOnSubagents / detached / restoring / expired / unknown
      return null;
  }
}

/** Guards against firing the same notification twice for a status the backend
 *  re-emits without an actual change (the bridge can re-emit on every journal
 *  entry). Keyed by session id → last notified status. */
const lastNotifiedStatus = new Map<string, SessionStatus>();

// ---------------------------------------------------------------------------
// Startup warmup — swallow the journal-replay burst
//
// On the FIRST connect, the agent replays its event journal: every existing
// session re-emits its last status, and historical exits/errors fire too. With
// many sessions that was a wall of chimes at launch. We treat the launch window
// as "warmup": events are recorded (so the dedup baseline is seeded) but NOT
// sounded. The window ends a short, quiet interval after the last startup event
// (so a slow connect's burst is still covered), or after a hard cap if no events
// arrive. Reconnects don't re-burst because `lastNotifiedStatus` already knows
// every session, so replayed statuses dedup out.
// ---------------------------------------------------------------------------

/** Hard cap so warmup always ends — captured ONCE as an absolute deadline at
 *  install, so it holds even under sustained transitions (the grace window below
 *  may only end warmup EARLIER, never push past this cap). */
const WARMUP_INITIAL_MS = 6000;
/** Quiet interval after the last startup event before we consider events live. */
const WARMUP_GRACE_MS = 1500;

// The absolute-cap warmup machinery, shared with lib/rulesMount.ts (see
// lib/warmup.ts). The initial grace arm is `WARMUP_GRACE_MS` (the factory's
// default), exactly as before: with no events, warmup ends after the grace
// window; with events, the absolute deadline still caps it at `WARMUP_INITIAL_MS`.
const warmup = createWarmup({
  initialMs: WARMUP_INITIAL_MS,
  graceMs: WARMUP_GRACE_MS,
});
const { start: startWarmup, inWarmup } = warmup;

/** Subscribe to the backend session events and fire notifications. Returns an
 *  unlisten that tears down every subscription. Safe to call once at startup. */
export async function installSessionNotifications(): Promise<() => void> {
  const unlisteners: Array<() => void> = [];

  // Primary signal: FR-012 session status (needs-question / completed / failed
  // / rate-limited). This is the richest event and carries the asks-signal.
  // Kick off the warmup window so the journal-replay burst at first connect is
  // seeded (for dedup) but silent. The absolute deadline holds even under a
  // steady event stream; `inWarmup()`'s grace re-arm can only end it sooner.
  startWarmup();

  unlisteners.push(
    await onSessionStatus(({ sessionId, status }) => {
      if (lastNotifiedStatus.get(sessionId) === status) return;
      lastNotifiedStatus.set(sessionId, status);
      // Warmup: record the baseline but don't sound the replayed status.
      if (inWarmup()) return;
      // Attribution: name the captain/ship when this session is one, so the
      // general knows which captain wants attention (null → generic wording).
      const n = statusToNotification(status, captainSubjectForSession(sessionId));
      if (n) notify(n.kind, n.title, n.body);
    }),
  );

  // Supervision tree also carries a per-orchestrator status; treat it as a
  // secondary source for the same mapping (e.g. a parent that just completed
  // after its subagents finished). Deduped through the same map.
  unlisteners.push(
    await onSupervision((tree) => {
      if (lastNotifiedStatus.get(tree.sessionId) === tree.status) return;
      lastNotifiedStatus.set(tree.sessionId, tree.status);
      if (inWarmup()) return;
      const n = statusToNotification(
        tree.status,
        captainSubjectForSession(tree.sessionId),
      );
      if (n) notify(n.kind, n.title, n.body);
    }),
  );

  // Terminal lifecycle: a tile that transitions to `error` is a backend-level
  // failure worth an alert even when no agent status exists.
  unlisteners.push(
    await onState(({ state }) => {
      if (state === "error") {
        if (inWarmup()) return; // swallow replayed historical errors at launch
        notify("error", "Terminal error", "A terminal entered an error state.");
      }
    }),
  );

  // A non-zero process exit is an error; a clean exit (code 0) is a soft "done".
  unlisteners.push(
    await onExit(({ code }) => {
      if (code != null && code !== 0) {
        if (inWarmup()) return; // swallow replayed historical exits at launch
        notify(
          "error",
          "Terminal exited",
          `A terminal exited with code ${code}.`,
        );
      }
    }),
  );

  // TODO(asks-signal): `needsQuestion` / `needsPermission` already give us the
  // "Claude is asking for input" cue via the status bridge. If a finer-grained
  // per-tool-prompt event is added later (e.g. a dedicated `agent://ask`
  // channel), subscribe to it here and route it to notify('attention', ...).

  return () => {
    for (const un of unlisteners) {
      try {
        un();
      } catch {
        /* best-effort teardown */
      }
    }
    lastNotifiedStatus.clear();
  };
}

// ---------------------------------------------------------------------------
// Self-mounting init (single source of truth for "wire up notifications")
// ---------------------------------------------------------------------------

let mounted = false;
let mountedUnlisten: (() => void) | null = null;

/** Idempotent app-startup mount. Import this module and call once — repeated
 *  calls are no-ops. Mirrors the `void init().then()` pattern App.tsx already
 *  uses for `initWindowSync`. The orchestrator should add the single line:
 *
 *    import { mountSessionNotifications } from "./lib/notify";  // side-effect-free import
 *    // ...then call once at app startup, e.g. in src/main.tsx:
 *    mountSessionNotifications();
 */
export function mountSessionNotifications(): void {
  if (mounted) return;
  mounted = true;
  void installSessionNotifications()
    .then((un) => {
      mountedUnlisten = un;
    })
    .catch((err) => {
      mounted = false;
      console.error("mountSessionNotifications failed", err);
    });
}

/** Tear down the self-mounted subscriptions (mainly for tests / hot-reload). */
export function unmountSessionNotifications(): void {
  mountedUnlisten?.();
  mountedUnlisten = null;
  mounted = false;
}
