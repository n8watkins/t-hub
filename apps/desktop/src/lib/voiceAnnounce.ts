// Announce-on-attention (Settings > Voice): speak "<label> needs your
// attention" when any agent session TRANSITIONS INTO needsQuestion or
// needsPermission - the same status spine the notify chimes and the titlebar
// attention affordances read.
//
// Shape mirrors lib/notify.ts: an imperative watcher armed once at startup
// (lib/voiceAnnounceMount.ts), transition detection via a previous-statuses
// map (the supervision store itself has no transition events), speak gated on
// the voice store (master `enabled` AND opt-in `announceOnAttention`, both
// from ~/.t-hub/voice.json) and debounced so a BURST of transitions (several
// crew hitting prompts at once) speaks at most once per ANNOUNCE_MIN_GAP_MS.
//
// Raw reducer statuses on purpose (not displayStatus): rateLimited is an
// overlay on a WORKING session, not a needs-input state, so it must not talk.
//
// SCRIBE VOICE-GATE: the general dictates with Scribe. A ~250ms poll of the
// scribe_status command maintains a cached `listening` boolean (the hot path
// never blocks on IPC). While the general is talking, an announcement that
// would fire is HELD in a single pending slot (coalesced to the latest, no
// backlog) instead of spoken. When they stop, after a short tail delay we
// re-scan for anything still blocked and deliver the held cue (or drop it if
// the situation resolved while they talked). Fail-open: the backend returns
// listening=false whenever it can't tell, so a missing/dead Scribe never
// silences T-Hub.
import { useSupervision } from "../store/supervision";
import { useVoice } from "../store/voice";
import { useWorkspace, tabIdForTerminal } from "../store/workspace";
import { synthesizeVoice } from "../ipc/voice";
import { scribeStatus } from "../ipc/scribe";
import { playWavBase64 } from "./voiceAudio";
import { notify } from "./notify";
import { useEngineRuntime } from "../store/engineRuntime";
import { effectiveTarget } from "../ipc/engine";
import { createWarmup } from "./warmup";
import { captainSubjectForSession } from "./captainAttribution";
import type { SessionStatus } from "../ipc/model";

/** Minimum gap between spoken announcements (the burst debounce). */
export const ANNOUNCE_MIN_GAP_MS = 5000;

/** Minimum gap between "voice engine unreachable" fallback alerts. A dead engine
 *  would otherwise fire the chime on every held/attempted cue; one alert per
 *  window is enough to break the silence without becoming its own nuisance. */
export const FALLBACK_ALERT_MIN_GAP_MS = 60000;

/** How often to poll the Scribe voice-gate status (cheap loopback file read). */
export const SCRIBE_POLL_MS = 250;

/** After the general STOPS dictating, wait this long before delivering a held
 *  announcement - a brief pause between phrases should not trigger delivery
 *  (a resume within the tail cancels it). */
export const SCRIBE_TAIL_MS = 500;

// Startup warmup - swallow the journal-replay burst. On the first connect the
// agent replays every existing session's last status, so a session that was
// ALREADY blocked before launch re-emits needsPermission/needsQuestion; those
// are not new transitions and must not speak (they would, twice, if the
// replay spanned the debounce window). Same machinery + tuning as
// lib/notify.ts: during warmup the transitions are RECORDED (prevStatuses
// seeds) but never spoken.
const WARMUP_INITIAL_MS = 6000;
const WARMUP_GRACE_MS = 1500;
const warmup = createWarmup({
  initialMs: WARMUP_INITIAL_MS,
  graceMs: WARMUP_GRACE_MS,
});

const NEEDS_INPUT: ReadonlySet<SessionStatus> = new Set<SessionStatus>([
  "needsQuestion",
  "needsPermission",
]);

let prevStatuses: Record<string, SessionStatus> = {};
let lastSpokenAt = Number.NEGATIVE_INFINITY;
/** When we last raised the "engine unreachable" fallback alert (debounced by
 *  FALLBACK_ALERT_MIN_GAP_MS). Negative infinity = never. */
let lastFallbackAlertAt = Number.NEGATIVE_INFINITY;
/** One synthesis in flight at a time (keeps the burst gate closed while the
 *  request runs WITHOUT charging the debounce window before success). */
let speaking = false;
let mounted = false;

/** Cached Scribe voice-gate state, refreshed by the poll. The hot path
 *  (handleStatusesChange) only ever READS this - it never awaits IPC. Defaults
 *  false (fail-open: speak) until the first poll says otherwise. */
let scribeListening = false;
/** The single held announcement while the general is dictating (coalesced to
 *  the latest transition - no backlog). Null when nothing is held. */
let pending: { text: string } | null = null;
let pollTimer: ReturnType<typeof setInterval> | null = null;
let tailTimer: ReturnType<typeof setTimeout> | null = null;
/** True while a scribe_status IPC is in flight, so a slow read never stacks
 *  overlapping poll ticks. */
let scribePolling = false;

/** Synthesize + play one announcement. Guards a single in-flight request and
 *  charges the burst-debounce clock only on SUCCESS (a failed synthesis leaves
 *  the window open for the next transition). Shared by the normal path and the
 *  Scribe-flush path. Returns false when a request was ALREADY in flight (the
 *  caller then knows nothing was started - the flush path uses this to retry a
 *  held cue instead of dropping it). */
function speak(text: string, now: number): boolean {
  if (speaking) return false;
  speaking = true;
  const voice = useVoice.getState();
  // Route to the ACTIVE engine when the managed lifecycle has fallen back, with
  // the standby's valid voice (the selected Kokoro voice would 400 on Piper).
  // Unmanaged: this passes through the selected engine + voice unchanged.
  const target = effectiveTarget(
    useEngineRuntime.getState().status,
    voice.engine,
    voice.voice,
  );
  const engine = target.engine;
  void synthesizeVoice(text, target.voice, target.engine)
    .then((b64) => {
      lastSpokenAt = now;
      playWavBase64(b64, useVoice.getState().volume);
    })
    .catch(() => {
      // TTS server down / no backend: the visual attention cues still stand,
      // and the debounce window stays open for the next transition. NEVER let
      // this be silent - the incident this feature exists for was a kokoro death
      // that fell through with zero surfacing. Raise the notify "error" chime +
      // toast (WebAudio, independent of the dead TTS server) so a dropped
      // announcement is heard/seen, debounced so a persistently-down engine
      // alerts at most once per window rather than on every attempt.
      //
      // F6: when the managed lifecycle is running, the SUPERVISOR owns the
      // fallback narrative (its own "Voice fell back" toast + amber state, and
      // effectiveTarget already rerouted to the live engine), so suppress this
      // #52 chime to avoid a double-chime in the down-debounce window.
      const managed = !!useEngineRuntime.getState().status?.managed;
      if (!managed && now - lastFallbackAlertAt >= FALLBACK_ALERT_MIN_GAP_MS) {
        lastFallbackAlertAt = now;
        notify(
          "error",
          "Voice engine unreachable",
          `The ${engine} TTS server didn't answer - an attention announcement ` +
            `could not be spoken. Check Settings › Voice.`,
        );
      }
    })
    .finally(() => {
      speaking = false;
    });
  return true;
}

/** A STABLE spoken name for a session, via the statusline's tmux index (the
 *  same sessionId -> th_<terminalId> chain rulesMount walks). Null when the
 *  session has no resolvable terminal (caller falls back to "A session").
 *
 *  Deliberately does NOT use deriveLabel / info.title: the Claude-suggested
 *  session title is volatile and reflects the user's TYPED INPUT, so speaking
 *  it announced the wrong thing (the general's dictated text instead of the
 *  captain). We use only stable sources, in order:
 *    1. the user's persisted rename (userLabels - not the merged `labels`,
 *       which folds in the volatile claudeTitles that caused the bug);
 *    2. the name of the workspace TAB holding the tile (the same
 *       tabIdForTerminal -> tabs.find(name) path the sidebar uses);
 *    3. the cwd basename.
 *  Plain function (not a hook), so it reads the store via getState(). */
function labelForSession(sessionId: string): string | null {
  const sup = useSupervision.getState();
  const tmux = Object.entries(sup.sessionIdByTmux).find(
    ([, sid]) => sid === sessionId,
  )?.[0];
  if (!tmux || !tmux.startsWith("th_")) return null;
  const terminalId = tmux.slice("th_".length);
  const ws = useWorkspace.getState();

  const rename = ws.userLabels[terminalId]?.trim();
  if (rename) return rename;

  const tabId = tabIdForTerminal(ws, terminalId);
  const tabName = tabId
    ? ws.tabs.find((t) => t.id === tabId)?.name?.trim()
    : undefined;
  if (tabName) return tabName;

  const cwd = ws.terminals[terminalId]?.cwd ?? "";
  const parts = cwd
    .replace(/[/\\]+$/, "")
    .split(/[/\\]+/)
    .filter(Boolean);
  return parts[parts.length - 1] ?? null;
}

/**
 * Process one statuses snapshot against the previous one. Exported (with an
 * injectable clock) so tests drive transitions directly; production calls it
 * from the store subscription in mountVoiceAnnounce.
 *
 * The previous-statuses map updates UNCONDITIONALLY (even while announcements
 * are off) so flipping the setting on never replays a backlog of transitions
 * that happened while it was off.
 */
export function handleStatusesChange(
  statuses: Record<string, SessionStatus>,
  now: number = Date.now(),
): void {
  const prev = prevStatuses;
  prevStatuses = statuses;
  if (statuses === prev) return; // same snapshot object: nothing changed

  // Startup replay window: the baseline above is seeded, but nothing speaks.
  // (inWarmup() also re-arms the grace timer, so a slow replay stays covered.)
  if (warmup.inWarmup()) return;

  // Sessions that ENTERED a needs-input state this snapshot (a flip between
  // the two needs-input states is not an entry - the user is already alerted).
  const entered = Object.entries(statuses).filter(([sid, st]) => {
    if (!NEEDS_INPUT.has(st)) return false;
    const before = prev[sid];
    return before === undefined || !NEEDS_INPUT.has(before);
  });
  if (entered.length === 0) return;

  const voice = useVoice.getState();
  // Master switch off = never speak; announce is a separate opt-in (default
  // OFF per the PRD - the general opts in explicitly).
  if (!voice.enabled || !voice.announceOnAttention) return;

  // Attribution: a CAPTAIN's cue names the ship ("Captain alpha needs your
  // attention") so the general knows WHICH captain wants them; a regular session
  // keeps its stable label. (Naming only - the gate above is untouched.)
  const sid = entered[0][0];
  const subject =
    captainSubjectForSession(sid) ?? labelForSession(sid) ?? "A session";
  const text = `${subject} needs your attention`;

  // Scribe voice-gate: the general is dictating - HOLD the cue in the single
  // pending slot (coalesced to the latest) instead of talking over them. It
  // is delivered on the listening falling edge (flushPending). Reads the
  // cached boolean only; never blocks on IPC.
  if (scribeListening) {
    pending = { text };
    return;
  }

  // Burst debounce: one spoken cue per window, however many sessions flipped.
  // The in-flight guard + success-only clock live in speak().
  if (now - lastSpokenAt < ANNOUNCE_MIN_GAP_MS) return;
  speak(text, now);
}

/**
 * Apply a fresh Scribe listening reading (from the poll). Maintains the cached
 * boolean and drives the tail-delayed delivery on the true->false falling edge
 * (the general stopped talking); a false->true rising edge within the tail
 * cancels a pending flush (they only paused). Exported so a test can drive the
 * edges with an injected clock instead of a real Scribe.
 */
export function applyScribeListening(
  listening: boolean,
  now: number = Date.now(),
): void {
  const was = scribeListening;
  scribeListening = listening;
  if (!was && listening) {
    // Rising edge / resumed within the tail: keep holding, cancel any flush.
    if (tailTimer) {
      clearTimeout(tailTimer);
      tailTimer = null;
    }
    return;
  }
  if (was && !listening) {
    // Falling edge: deliver after a short tail (in case they resume).
    if (tailTimer) clearTimeout(tailTimer);
    tailTimer = setTimeout(() => {
      tailTimer = null;
      flushPending(Date.now());
    }, SCRIBE_TAIL_MS);
    void now;
  }
}

/**
 * Deliver the held announcement if the blocking situation still stands. Called
 * after the tail delay once the general stops dictating: re-scans the CURRENT
 * supervision statuses and speaks the pending cue only if something is still in
 * a needs-input state, else drops it silently (it resolved while they talked).
 * Exported so a test can fire the flush directly without the tail timer.
 *
 * Deliberately BYPASSES the 5s burst debounce: the held cue has already been
 * waiting (often many seconds) and is a deferred distinct event, not part of a
 * burst, so it should deliver promptly. It still respects the single-in-flight
 * guard: if a normal cue is mid-synthesis right now (a fresh transition landed
 * during the tail), we keep `pending` and re-arm the tail rather than DROP the
 * very cue this feature exists to preserve.
 */
export function flushPending(now: number = Date.now()): void {
  const held = pending;
  if (!held) return;
  const statuses = useSupervision.getState().statuses;
  const stillBlocked = Object.values(statuses).some((st) => NEEDS_INPUT.has(st));
  if (!stillBlocked) {
    pending = null; // resolved during dictation - drop silently
    return;
  }
  if (speak(held.text, now)) {
    pending = null; // delivered
    return;
  }
  // A synthesis was in flight: retry after another tail so the held cue is not
  // lost (a resume flips scribeListening true and cancels this via the rising
  // edge, so we never retry into an active dictation).
  if (tailTimer) clearTimeout(tailTimer);
  tailTimer = setTimeout(() => {
    tailTimer = null;
    flushPending(Date.now());
  }, SCRIBE_TAIL_MS);
}

/** Arm the watcher once (idempotent). Subscribes the supervision store - the
 *  statuses map identity only changes on a real status write, so the handler
 *  early-outs for snapshot/tree-only updates. */
export function mountVoiceAnnounce(): void {
  if (mounted) return;
  mounted = true;
  warmup.start();
  prevStatuses = useSupervision.getState().statuses;
  useSupervision.subscribe((s) => handleStatusesChange(s.statuses));
}

/** Start the ~250ms Scribe voice-gate poll (idempotent). Each tick reads the
 *  cached listening state off the loopback command and feeds the edge machine;
 *  an IPC failure fails open (listening=false). Called once from
 *  voiceAnnounceMount (which the tests never import), so no real poll spins in
 *  the unit suite - the gate is driven there via applyScribeListening /
 *  _setScribeListeningForTest instead. The `pollTimer` guard keeps it
 *  single-armed even if called twice. */
export function startScribePoll(): void {
  if (pollTimer) return;
  const tick = () => {
    if (scribePolling) return; // a prior read is still in flight - skip
    scribePolling = true;
    void scribeStatus()
      .then((s) => applyScribeListening(!!s.listening, Date.now()))
      .catch(() => applyScribeListening(false, Date.now()))
      .finally(() => {
        scribePolling = false;
      });
  };
  tick(); // seed immediately so the gate reflects reality without a poll wait
  pollTimer = setInterval(tick, SCRIBE_POLL_MS);
}

/** Test-only: clear the transition/debounce + Scribe-gate state between cases. */
export function _resetVoiceAnnounceForTest(): void {
  prevStatuses = {};
  lastSpokenAt = Number.NEGATIVE_INFINITY;
  lastFallbackAlertAt = Number.NEGATIVE_INFINITY;
  speaking = false;
  scribeListening = false;
  pending = null;
  scribePolling = false;
  if (tailTimer) {
    clearTimeout(tailTimer);
    tailTimer = null;
  }
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

/** Test-only: set the cached Scribe listening state directly (no edge/timer),
 *  so hold/flush are unit-testable without a real Scribe or the poll. */
export function _setScribeListeningForTest(listening: boolean): void {
  scribeListening = listening;
}

/** Test-only: read whether an announcement is currently held. */
export function _pendingTextForTest(): string | null {
  return pending?.text ?? null;
}

/** Test-only: start the startup warmup window (production starts it in
 *  mountVoiceAnnounce; tests must not mount, which would leave a live store
 *  subscription behind). */
export function _startVoiceAnnounceWarmupForTest(): void {
  warmup.start();
}
