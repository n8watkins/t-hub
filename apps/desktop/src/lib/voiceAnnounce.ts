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
// SCRIBE VOICE-GATE: Scribe events update the cached `listening` boolean when
// the event bridge is available. Until the first event arrives, a bounded
// one-second status fallback keeps older Scribe builds safe. The existing
// hold, tail, coalescing, and fail-open semantics remain unchanged.
import { useSupervision } from "../store/supervision";
import { DEFAULT_VOICE_SETTINGS, useVoice } from "../store/voice";
import { useWorkspace, tabIdForTerminal } from "../store/workspace";
import {
  claimVoiceAnnouncement,
  recordVoiceAnnouncementOutcome,
  seedVoiceAnnouncementBoundary,
  synthesizeVoice,
  type VoiceAnnouncementKind,
} from "../ipc/voice";
import { onJournal } from "../ipc/client05";
import { onScribeStatus, scribeStatus } from "../ipc/scribe";
import { playWavBase64, VoiceAudioError } from "./voiceAudio";
import { notify } from "./notify";
import { useEngineRuntime } from "../store/engineRuntime";
import { effectiveTarget } from "../ipc/engine";
import { createWarmup } from "./warmup";
import {
  captainSubjectForSession,
  terminalIdForSession,
} from "./captainAttribution";
import type { SessionStatus } from "../ipc/model";
import type { JournalEvent } from "../ipc/protocol";

/** Minimum gap between spoken announcements (the burst debounce). */
export const ANNOUNCE_MIN_GAP_MS = 5000;

/** Minimum gap between "voice engine unreachable" fallback alerts. A dead engine
 *  would otherwise fire the chime on every held/attempted cue; one alert per
 *  window is enough to break the silence without becoming its own nuisance. */
export const FALLBACK_ALERT_MIN_GAP_MS = 60000;

/** Compatibility fallback interval until Scribe emits its first event. */
export const SCRIBE_POLL_MS = 1000;
/** Events are trusted only while a fresh producer heartbeat is observed. */
export const SCRIBE_EVENT_TTL_MS = 5000;

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
 *  (handleStatusesChange) only ever READS this - it never awaits IPC. */
let scribeListening = false;
/** False while an enabled poll generation has not produced its first valid
 * result. Unknown is fail-safe: hold cues until Scribe confirms idle. */
let scribeStatusKnown = false;
/** The single held announcement while the general is dictating (coalesced to
 *  the latest transition - no backlog). Null when nothing is held. */
interface PendingAnnouncement {
  text: string;
  requireBlocked: boolean;
  kind: VoiceAnnouncementKind;
  sessionId?: string;
  attemptId?: string;
  outcomeInFlight?: boolean;
}

const ANNOUNCEMENT_SEVERITY: Record<VoiceAnnouncementKind, number> = {
  completion: 1,
  question: 2,
  permission: 3,
  failure: 4,
};

let pending: PendingAnnouncement | null = null;
let journalProcessing: Promise<void> = Promise.resolve();
let pollTimer: ReturnType<typeof setInterval> | null = null;
let tailTimer: ReturnType<typeof setTimeout> | null = null;
let scribeEventUnlisten: (() => void) | null = null;
let lastValidScribeEventAt = 0;
let lastScribeEventGeneration = 0;
/** Incremented whenever the poller starts or stops.
 * Results from an older generation are ignored after a settings transition. */
let pollGeneration = 0;
let pollLifecycleMounted = false;
let unsubscribePollLifecycle: (() => void) | null = null;

/** Synthesize + play one announcement. Guards a single in-flight request and
 *  charges the burst-debounce clock only on SUCCESS (a failed synthesis leaves
 *  the window open for the next transition). Shared by the normal path and the
 *  Scribe-flush path. Returns false when a request was ALREADY in flight (the
 *  caller then knows nothing was started - the flush path uses this to retry a
 *  held cue instead of dropping it). */
function speak(text: string, now: number, attemptId?: string): boolean {
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
  const onSynthesisFailure = async (error: unknown) => {
    const detail = error instanceof Error ? error.message : String(error);
    if (attemptId) {
      await recordVoiceAnnouncementOutcome(
        attemptId,
        "failed",
        `synthesis: ${detail}`,
      ).catch(() => {});
    }
    useVoice
      .getState()
      .recordDeliveryFailure("synthesis", detail || "Synthesis failed", now);
    const managed = !!useEngineRuntime.getState().status?.managed;
    if (!managed && now - lastFallbackAlertAt >= FALLBACK_ALERT_MIN_GAP_MS) {
      lastFallbackAlertAt = now;
      notify(
        "error",
        "Voice engine unreachable",
        `The ${engine} TTS server did not produce an announcement. Check Settings › Voice.`,
      );
    }
  };
  const onPlaybackFailure = async (error: unknown) => {
    const kind =
      error instanceof VoiceAudioError ? error.kind : ("playback" as const);
    const detail = error instanceof Error ? error.message : String(error);
    if (attemptId) {
      await recordVoiceAnnouncementOutcome(
        attemptId,
        "failed",
        `${kind}: ${detail}`,
      ).catch(() => {});
    }
    useVoice
      .getState()
      .recordDeliveryFailure(kind, detail || "Audio delivery failed", now);
    notify(
      "error",
      kind === "device" ? "Voice audio device failed" : "Voice playback failed",
      kind === "device"
        ? "The announcement was synthesized, but no usable audio device was available."
        : "The announcement was synthesized, but the audio clip could not be played.",
    );
  };
  void synthesizeVoice(text, target.voice, target.engine)
    .then(
      (b64) =>
        Promise.resolve(playWavBase64(b64, useVoice.getState().volume)).then(
          async () => {
            lastSpokenAt = now;
            useVoice.getState().clearDeliveryFailure();
            if (attemptId) {
              await recordVoiceAnnouncementOutcome(
                attemptId,
                "succeeded",
              ).catch(() => {});
            }
          },
          onPlaybackFailure,
        ),
      onSynthesisFailure,
    )
    .finally(() => {
      speaking = false;
      if (
        pending &&
        scribeStatusKnown &&
        !scribeListening &&
        !tailTimer
      ) {
        const delay = Math.max(
          0,
          ANNOUNCE_MIN_GAP_MS - (Date.now() - lastSpokenAt),
        );
        tailTimer = setTimeout(() => {
          tailTimer = null;
          flushPending(Date.now());
        }, delay);
      }
    });
  return true;
}

function queuePending(next: PendingAnnouncement): void {
  if (
    !pending ||
    ANNOUNCEMENT_SEVERITY[next.kind] >=
      ANNOUNCEMENT_SEVERITY[pending.kind]
  ) {
    if (pending?.attemptId) {
      void recordVoiceAnnouncementOutcome(
        pending.attemptId,
        "interrupted",
        "A higher-priority announcement replaced this queued cue.",
      ).catch(() => {});
    }
    pending = next;
  } else if (next.attemptId) {
    void recordVoiceAnnouncementOutcome(
      next.attemptId,
      "interrupted",
      "A higher-priority announcement was already queued.",
    ).catch(() => {});
  }
}

function announcementText(
  kind: VoiceAnnouncementKind,
  sessionId: string | undefined,
): string {
  const subject = sessionId
    ? captainSubjectForSession(sessionId) ??
      labelForSession(sessionId) ??
      "A session"
    : "A session";
  switch (kind) {
    case "permission":
      return `${subject} needs permission`;
    case "question":
      return `${subject} has a question`;
    case "completion":
      return `${subject} completed`;
    case "failure":
      return `${subject} failed`;
  }
}

/** Consume one normalized provider-neutral journal event.
 * The backend claim is the durable replay boundary and policy authority. */
export async function handleJournalEvent(
  event: JournalEvent,
  now: number = Date.now(),
): Promise<void> {
  const authority = event.voice_announcement;
  if (!authority) return;
  const kind = authority.kind;

  let shouldAnnounce = false;
  let attemptId: string | undefined;
  try {
    const claim = await claimVoiceAnnouncement(
      event.entry.seq,
      kind,
      event.entry.event_id,
    );
    shouldAnnounce = claim.shouldAnnounce;
    attemptId = claim.attemptId;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    notify(
      "error",
      "Voice announcement policy unavailable",
      detail || "The durable announcement cursor could not be updated.",
    );
    return;
  }
  if (!shouldAnnounce) return;

  // Voice is intentionally scoped to sessions represented in the user's
  // Captain/Crew/Assignment workspace. Provider-internal sessions are claimed
  // so replay stays suppressed, but never spoken.
  const terminalId = terminalIdForSession(authority.sessionId);
  if (!terminalId || !useWorkspace.getState().terminals[terminalId]) {
    if (attemptId) {
      await recordVoiceAnnouncementOutcome(
        attemptId,
        "interrupted",
        "The event was outside the active Captain, Crew, or Assignment workspace.",
      ).catch(() => {});
    }
    return;
  }

  const text = announcementText(kind, authority.sessionId);
  const requireBlocked = kind === "permission" || kind === "question";
  if (!scribeStatusKnown || scribeListening) {
    queuePending({
      text,
      requireBlocked,
      kind,
      sessionId: authority.sessionId,
      attemptId,
    });
    return;
  }
  if (speaking || now - lastSpokenAt < ANNOUNCE_MIN_GAP_MS) {
    // Bounded delivery queue: retain the latest cue rather than silently
    // losing an event while another clip is playing or the burst gap is open.
    // The durable claim is intentionally at-most-once, so this one-slot queue
    // is the only retry and coalescing boundary.
    queuePending({
      text,
      requireBlocked,
      kind,
      sessionId: authority.sessionId,
      attemptId,
    });
    if (!tailTimer) {
      const delay = Math.max(0, ANNOUNCE_MIN_GAP_MS - (now - lastSpokenAt));
      tailTimer = setTimeout(() => {
        tailTimer = null;
        flushPending(Date.now());
      }, delay);
    }
    return;
  }
  speak(text, now, attemptId);
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
  if (!scribeStatusKnown || scribeListening) {
    const kind =
      entered[0][1] === "needsPermission" ? "permission" : "question";
    queuePending({ text, requireBlocked: true, kind, sessionId: sid });
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
  const wasKnown = scribeStatusKnown;
  scribeStatusKnown = true;
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
    return;
  }
  if (!wasKnown && !listening && pending) {
    // The first result for this enabled generation confirms Scribe is idle.
    // A cue held while status was unknown can now follow the normal tail path.
    if (tailTimer) clearTimeout(tailTimer);
    tailTimer = setTimeout(() => {
      tailTimer = null;
      flushPending(Date.now());
    }, SCRIBE_TAIL_MS);
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
export async function flushPending(now: number = Date.now()): Promise<void> {
  const held = pending;
  if (!held) return;
  const statuses = useSupervision.getState().statuses;
  const stillBlocked = held.sessionId
    ? NEEDS_INPUT.has(statuses[held.sessionId])
    : Object.values(statuses).some((st) => NEEDS_INPUT.has(st));
  if (held.requireBlocked && !stillBlocked) {
    if (held.attemptId) {
      if (held.outcomeInFlight) return;
      pending = { ...held, outcomeInFlight: true };
      try {
        await recordVoiceAnnouncementOutcome(
          held.attemptId,
          "interrupted",
          "The requested input was resolved before the held announcement could be delivered.",
        );
        if (pending?.attemptId === held.attemptId) {
          pending = null;
          const failure = useVoice.getState().deliveryFailure;
          if (
            failure?.kind === "interrupted" &&
            failure.detail.startsWith(
              "Could not persist the interrupted announcement outcome:",
            )
          ) {
            useVoice.getState().clearDeliveryFailure();
          }
        }
      } catch (error) {
        if (pending?.attemptId === held.attemptId) {
          pending = { ...held, outcomeInFlight: false };
        }
        const detail = error instanceof Error ? error.message : String(error);
        useVoice.getState().recordDeliveryFailure(
          "interrupted",
          `Could not persist the interrupted announcement outcome: ${detail}`,
          now,
        );
        if (!tailTimer) {
          tailTimer = setTimeout(() => {
            tailTimer = null;
            void flushPending(Date.now());
          }, SCRIBE_TAIL_MS);
        }
      }
      return;
    }
    pending = null;
    return;
  }
  if (speak(held.text, now, held.attemptId)) {
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

/** Arm the watcher once. Journal delivery occurs only after normalized event
 * deduplication and redaction in the backend. */
export function mountVoiceAnnounce(): void {
  if (mounted) return;
  mounted = true;
  void onJournal((event) => {
    journalProcessing = journalProcessing.then(async () => {
      if (!event.replayed) {
        await handleJournalEvent(event);
        return;
      }
      try {
        await seedVoiceAnnouncementBoundary(event.entry.seq);
      } catch (error) {
        notify(
          "error",
          "Voice announcement replay boundary unavailable",
          error instanceof Error ? error.message : String(error),
        );
      }
    });
  }).catch((error) => {
    notify(
      "error",
      "Voice announcement replay boundary unavailable",
      error instanceof Error ? error.message : String(error),
    );
  });
}

function voiceAnnouncementsEnabled(): boolean {
  const voice = useVoice.getState();
  const policy =
    voice.announcementPolicy ?? DEFAULT_VOICE_SETTINGS.announcementPolicy!;
  return (
    voice.enabled &&
    Object.values(policy).some((enabled) => enabled)
  );
}

/** Stop the Scribe poll and discard all voice-gate state.
 * A disabled announcement feature must not later flush a cue that it held while enabled. */
function stopScribePoll(): void {
  pollGeneration += 1;
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  scribeEventUnlisten?.();
  scribeEventUnlisten = null;
  lastValidScribeEventAt = 0;
  lastScribeEventGeneration = 0;
  scribeListening = false;
  scribeStatusKnown = false;
  if (pending?.attemptId) {
    void recordVoiceAnnouncementOutcome(
      pending.attemptId,
      "interrupted",
      "Voice announcements were disabled before delivery.",
    ).catch(() => {});
  }
  pending = null;
  journalProcessing = Promise.resolve();
  if (tailTimer) {
    clearTimeout(tailTimer);
    tailTimer = null;
  }
}

/** Arm the event-first Scribe voice gate.
 * The one-shot status read establishes a safe initial state, then the
 * `scribe://status` event stream owns updates when available. Older Scribe
 * builds that do not emit an event retain a bounded one-second fallback.
 * An IPC failure fails open and a slow read never stacks overlapping ticks. */
function armScribePoll(): void {
  if (pollTimer) return;
  const generation = ++pollGeneration;
  scribeStatusKnown = false;
  scribeListening = false;
  let polling = false;
  lastValidScribeEventAt = 0;
  lastScribeEventGeneration = 0;
  void onScribeStatus((s) => {
    if (generation !== pollGeneration) return;
    if (
      typeof s?.listening !== "boolean" ||
      !Number.isFinite(s.generation) ||
      !Number.isFinite(s.observedAtMs) ||
      typeof s.sourceIdentity !== "string" ||
      s.sourceIdentity.length === 0 ||
      (s.generation ?? 0) <= lastScribeEventGeneration ||
      Math.abs(Date.now() - (s.observedAtMs ?? 0)) > SCRIBE_EVENT_TTL_MS
    ) {
      return;
    }
    lastScribeEventGeneration = s.generation ?? 0;
    lastValidScribeEventAt = Date.now();
    applyScribeListening(!!s.listening, Date.now());
  })
    .then((unlisten) => {
      if (generation !== pollGeneration) {
        unlisten();
        return;
      }
      scribeEventUnlisten = unlisten;
    })
    .catch(() => {
      // The one-second fallback remains active for older/non-Tauri Scribe.
    });
  const tick = () => {
    if (
      lastValidScribeEventAt > 0 &&
      Date.now() - lastValidScribeEventAt < SCRIBE_EVENT_TTL_MS
    ) return;
    if (polling) return;
    polling = true;
    void scribeStatus()
      .then((s) => {
        if (generation === pollGeneration) {
          applyScribeListening(!!s.listening, Date.now());
        }
      })
      .catch(() => {
        if (generation === pollGeneration) {
          applyScribeListening(false, Date.now());
        }
      })
      .finally(() => {
        polling = false;
      });
  };
  tick();
  pollTimer = setInterval(tick, SCRIBE_POLL_MS);
}

/** Mount the settings-driven Scribe poll lifecycle once.
 * Polling is needed only while both the voice master switch and announce-on-attention are enabled.
 * Store changes synchronize the timer immediately, while the idempotent mount prevents duplicate subscriptions. */
export function startScribePoll(): void {
  if (pollLifecycleMounted) return;
  pollLifecycleMounted = true;

  let enabled = voiceAnnouncementsEnabled();
  if (enabled) armScribePoll();
  unsubscribePollLifecycle = useVoice.subscribe(() => {
    const next = voiceAnnouncementsEnabled();
    if (next === enabled) return;
    enabled = next;
    if (enabled) armScribePoll();
    else stopScribePoll();
  });
}

/** Test-only: clear the transition/debounce + Scribe-gate state between cases. */
export function _resetVoiceAnnounceForTest(): void {
  prevStatuses = {};
  lastSpokenAt = Number.NEGATIVE_INFINITY;
  lastFallbackAlertAt = Number.NEGATIVE_INFINITY;
  speaking = false;
  scribeListening = false;
  scribeStatusKnown = false;
  pending = null;
  pollLifecycleMounted = false;
  unsubscribePollLifecycle?.();
  unsubscribePollLifecycle = null;
  if (tailTimer) {
    clearTimeout(tailTimer);
    tailTimer = null;
  }
  stopScribePoll();
}

/** Test-only: set the cached Scribe listening state directly (no edge/timer),
 *  so hold/flush are unit-testable without a real Scribe or the poll. */
export function _setScribeListeningForTest(listening: boolean): void {
  scribeStatusKnown = true;
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
