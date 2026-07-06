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
import { useSupervision } from "../store/supervision";
import { useVoice } from "../store/voice";
import { useWorkspace, deriveLabel } from "../store/workspace";
import { synthesizeVoice } from "../ipc/voice";
import { playWavBase64 } from "./voiceAudio";
import { createWarmup } from "./warmup";
import type { SessionStatus } from "../ipc/model";

/** Minimum gap between spoken announcements (the burst debounce). */
export const ANNOUNCE_MIN_GAP_MS = 5000;

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
/** One synthesis in flight at a time (keeps the burst gate closed while the
 *  request runs WITHOUT charging the debounce window before success). */
let speaking = false;
let mounted = false;

/** The human label for a session, via the statusline's tmux index (the same
 *  sessionId -> th_<terminalId> chain rulesMount walks) and the workspace
 *  store's deriveLabel - so the spoken name matches the tile/sidebar name.
 *  Null when the session has no resolvable terminal (label falls back). */
function labelForSession(sessionId: string): string | null {
  const sup = useSupervision.getState();
  const tmux = Object.entries(sup.sessionIdByTmux).find(
    ([, sid]) => sid === sessionId,
  )?.[0];
  if (!tmux || !tmux.startsWith("th_")) return null;
  const terminalId = tmux.slice("th_".length);
  const ws = useWorkspace.getState();
  const info = ws.terminals[terminalId];
  return deriveLabel({
    id: terminalId,
    label: ws.labels[terminalId],
    title: info?.title,
    cwd: info?.cwd,
  });
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

  // Burst debounce: one spoken cue per window, however many sessions flipped.
  // The in-flight flag holds the gate while a request runs; lastSpokenAt is
  // charged only on SUCCESS, so a failed synthesis does not eat the window.
  if (speaking || now - lastSpokenAt < ANNOUNCE_MIN_GAP_MS) return;
  speaking = true;

  const label = labelForSession(entered[0][0]) ?? "A session";
  void synthesizeVoice(`${label} needs your attention`, voice.voice)
    .then((b64) => {
      lastSpokenAt = now;
      playWavBase64(b64, useVoice.getState().volume);
    })
    .catch(() => {
      // TTS server down / no backend: the visual attention cues still stand,
      // and the debounce window stays open for the next transition.
    })
    .finally(() => {
      speaking = false;
    });
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

/** Test-only: clear the transition/debounce state between cases. */
export function _resetVoiceAnnounceForTest(): void {
  prevStatuses = {};
  lastSpokenAt = Number.NEGATIVE_INFINITY;
  speaking = false;
}

/** Test-only: start the startup warmup window (production starts it in
 *  mountVoiceAnnounce; tests must not mount, which would leave a live store
 *  subscription behind). */
export function _startVoiceAnnounceWarmupForTest(): void {
  warmup.start();
}
