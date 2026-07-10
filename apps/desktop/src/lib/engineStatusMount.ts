// Side-effect module (mirrors voiceAnnounceMount/notifyMount): hydrate the
// managed-engine runtime store and subscribe to the supervisor's live pushes.
// main.tsx imports this for its side effect.
//
// Two subscriptions:
//   * runtime_status -> keep the store current (Settings degraded state + the
//     announce path's synthesis routing read it).
//   * toast -> fire the notify() chime + OS toast the supervisor asked for on a
//     fallback ("Voice fell back") or recovery ("Kokoro recovered"), so the
//     general is ALWAYS told even with Settings closed - the "never silent"
//     requirement, now for the auto-fallback event too.
import { useEngineRuntime } from "../store/engineRuntime";
import { onEngineRuntimeStatus, onEngineToast } from "../ipc/engine";
import { notify } from "./notify";

let mounted = false;

/** Arm the runtime-status subscriptions once (idempotent). Exported so a test
 *  can invoke it deterministically; main.tsx calls it via the import below. */
export function mountEngineStatus(): void {
  if (mounted) return;
  mounted = true;
  // Hydrate once (managed:false when the flag is off - the UI then uses the #52
  // probes and this store stays inert).
  void useEngineRuntime.getState().load();
  onEngineRuntimeStatus((s) => useEngineRuntime.getState().apply(s));
  onEngineToast((t) => notify(t.kind, t.title, t.body));
}

mountEngineStatus();
