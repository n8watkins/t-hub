// Typed wrapper over the scribe_status Tauri command (src-tauri/src/scribe.rs):
// "is the general dictating right now?" via Scribe's v1 dictation-state
// interface (loopback GET /v1/status discovered from ~/.scribe/control.json),
// with Scribe's status.json file as the fallback transport. Its own module
// (not ipc/voice) so the voiceAnnounce gate has one mockable seam.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** The Scribe voice-gate status. `listening` is the COMPUTED effective value,
 *  sourced from Scribe's level-triggered `busy` flag (fail-open: false
 *  whenever the backend can't positively confirm an active dictation cycle -
 *  unreachable endpoint, missing/torn/stale/dead-pid fallback file). `status`
 *  and `since` are informational pass-throughs from the snapshot; `source`
 *  names the transport that answered ("v1" or "file"). The optional fields
 *  are omitted when nothing was reachable. */
export interface ScribeStatus {
  listening: boolean;
  status?: string | null;
  since?: string | number | null;
  source?: string | null;
  generation?: number;
  observedAtMs?: number;
  sourceIdentity?: string;
}

/** Read the current Scribe status. Rejects only on an IPC failure; the backend
 *  itself always resolves (fail-open) rather than erroring on a bad source. */
export function scribeStatus(): Promise<ScribeStatus> {
  return invoke("scribe_status");
}

/** Start the native Scribe event producer for the enabled voice lifecycle. */
export function startScribeStatusEmitter(): Promise<void> {
  return invoke("scribe_status_start");
}

/** Stop the native Scribe event producer when voice announcements are disabled. */
export function stopScribeStatusEmitter(): Promise<void> {
  return invoke("scribe_status_stop");
}

/** Subscribe to Scribe's event-driven dictation state when the Scribe build
 * exposes its Tauri event bridge. The voice gate keeps a bounded one-second
 * fallback poll until the first event arrives, so older Scribe builds retain
 * the previous fail-open semantics without a sustained 250 ms loop. */
export function onScribeStatus(
  callback: (status: ScribeStatus) => void,
): Promise<UnlistenFn> {
  return listen<ScribeStatus>("scribe://status", (event) => callback(event.payload));
}
