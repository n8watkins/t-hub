// Typed wrapper over the scribe_status Tauri command (src-tauri/src/scribe.rs):
// "is the general dictating right now?" via Scribe's status file. Its own
// module (not ipc/voice) so the voiceAnnounce gate has one mockable seam.
import { invoke } from "@tauri-apps/api/core";

/** The Scribe voice-gate status. `listening` is the COMPUTED effective value
 *  (fail-open: false whenever the backend can't confirm an active dictation -
 *  missing/torn/stale/dead-pid file). `status` and `since` are informational
 *  pass-throughs from the file (omitted when it was missing/torn). */
export interface ScribeStatus {
  listening: boolean;
  status?: string | null;
  since?: string | number | null;
}

/** Read the current Scribe status. Rejects only on an IPC failure; the backend
 *  itself always resolves (fail-open) rather than erroring on a bad file. */
export function scribeStatus(): Promise<ScribeStatus> {
  return invoke("scribe_status");
}
