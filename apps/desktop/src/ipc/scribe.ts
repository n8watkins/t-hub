// Typed wrapper over the scribe_status Tauri command (src-tauri/src/scribe.rs):
// "is the general dictating right now?" via Scribe's v1 dictation-state
// interface (loopback GET /v1/status discovered from ~/.scribe/control.json),
// with Scribe's status.json file as the fallback transport. Its own module
// (not ipc/voice) so the voiceAnnounce gate has one mockable seam.
import { invoke } from "@tauri-apps/api/core";

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
}

/** Read the current Scribe status. Rejects only on an IPC failure; the backend
 *  itself always resolves (fail-open) rather than erroring on a bad source. */
export function scribeStatus(): Promise<ScribeStatus> {
  return invoke("scribe_status");
}
