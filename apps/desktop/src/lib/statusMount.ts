// Side-effect mount for the Claude USAGE status feed.
//
// Importing THIS module once at app startup wires the `status://snapshot` event
// straight into the supervision store's `setSnapshot` — no function call needed
// at the import site. It mirrors `src/lib/notifyMount.ts` so the orchestrator
// can mount the feature with a single side-effect import in `src/main.tsx`
// (next to `import "./lib/notifyMount";`):
//
//     import "./lib/statusMount";
//
// ## Why this exists (the USAGE-dashes fix)
// The sidebar's USAGE strip reads `useSupervision().snapshots`. Those snapshots
// only arrive via the `status://snapshot` event. That event WAS already consumed
// inside `useAgentTelemetry` (mounted by the Sidebar), but only while the
// Sidebar is mounted. This module adds an ALWAYS-ON, app-lifetime subscription
// so usage flows regardless of which views are mounted, and emits a `usage`
// diag line at the ingest point so the orchestrator can SEE snapshots arriving
// in the file diag log. `setSnapshot` is idempotent (keyed by session id), so a
// second subscription alongside telemetry's is harmless — last write wins with
// identical data.
import { onStatus } from "../ipc/client05";
import { useSupervision } from "../store/supervision";
import { tlog } from "./diag";

let mounted = false;
let unlisten: (() => void) | null = null;

/** Idempotent app-startup mount. Repeated calls are no-ops. */
export function mountStatusFeed(): void {
  if (mounted) return;
  mounted = true;
  tlog("usage", "mountStatusFeed: subscribing to status://snapshot");
  void onStatus((snap) => {
    // The supervision store logs the snapshot details under the same `usage`
    // tag; here we just feed it. setSnapshot is idempotent per session id.
    useSupervision.getState().setSnapshot(snap);
  })
    .then((fn) => {
      unlisten = fn;
      tlog("usage", "mountStatusFeed: subscription active");
    })
    .catch((err) => {
      mounted = false;
      tlog("usage", "mountStatusFeed failed", String(err));
    });
}

/** Tear down the self-mounted subscription (mainly for tests / hot-reload). */
export function unmountStatusFeed(): void {
  unlisten?.();
  unlisten = null;
  mounted = false;
}

// Self-mount on import (the side-effect this module exists for).
mountStatusFeed();
