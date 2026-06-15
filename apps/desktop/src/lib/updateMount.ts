// Side-effect mount for the on-launch update check + optional silent install
// (feat/auto-updater).
//
// Importing THIS module once at app startup schedules a single background update
// check shortly after launch — no function call needed at the import site. It
// mirrors notifyMount.ts so the orchestrator can mount the feature with one
// side-effect import in src/main.tsx (next to `import "./lib/notifyMount";`):
//
//     import "./lib/updateMount";
//
// Behavior (all opt-out via the persisted settings toggles):
//   - autoUpdateCheckEnabled off  -> do nothing.
//   - update found, autoInstallUpdates off -> leave it; the user installs from
//     Settings -> Updates. (We don't nag mid-session.)
//   - update found, autoInstallUpdates on  -> download + verify + install the
//     signed package silently, then relaunch.
//
// HARD GUARDRAILS so a missing latest.json / offline / unsigned release can
// never block the app or loop:
//   - one-shot guard: the install runs at most once per launch.
//   - stall watchdog: if the download produces no event for STALL_MS, abort.
//   - everything is best-effort: any failure is logged and swallowed; the app
//     keeps running and the manual Settings -> Updates path still works.

import { useSettings } from "../store/settings";

/** How long after launch to run the first check. Long enough that the window has
 *  painted and the WSL/agent hops aren't competing for the first moments. */
const CHECK_DELAY_MS = 6000;

/** Inactivity watchdog: if a download produces no progress event for this long,
 *  treat it as stalled so a silent install can't hang forever. */
const STALL_MS = 90_000;

let mounted = false;

/** Idempotent app-startup mount. Import this module once — repeated imports are
 *  no-ops (ES modules are evaluated once, and this guard covers hot-reload). */
function mountUpdateCheck(): void {
  if (mounted) return;
  mounted = true;
  // Defer past first paint, then run the check once. Failures are swallowed.
  window.setTimeout(() => {
    void runLaunchUpdateFlow().catch((err) => {
      console.warn("updateMount: launch update flow failed", err);
    });
  }, CHECK_DELAY_MS);
}

/** The one-shot launch flow: respect the toggles, detect, and (optionally)
 *  silently install. Never throws to the caller. */
async function runLaunchUpdateFlow(): Promise<void> {
  // Read the live, persisted settings at run time (the settings store is
  // hydrated from localStorage synchronously at import, so this reflects the
  // user's saved choice, not a default race).
  const { autoUpdateCheckEnabled, autoInstallUpdates } = useSettings.getState();
  if (!autoUpdateCheckEnabled) return;

  // detectUpdate() is tolerant: offline / no-Tauri / missing latest.json all
  // resolve to "no update" rather than throwing.
  const { detectUpdate } = await import("./updates");
  const result = await detectUpdate();
  if (!result.updateAvailable) return;

  // Found one. If the user opted out of silent install, stop here — the chip /
  // Settings -> Updates surface will show it; we don't interrupt their session.
  if (!autoInstallUpdates) return;

  await silentInstall();
}

/** Download + verify + install the signed update package, then relaunch. Guarded
 *  by a stall watchdog. Best-effort: any failure is logged and swallowed so the
 *  manual Settings -> Updates path remains the fallback. */
async function silentInstall(): Promise<void> {
  const { check } = await import("@tauri-apps/plugin-updater");
  const { relaunch } = await import("@tauri-apps/plugin-process");

  // check() resolves the *signed* package — the same call the manual Install
  // button uses. null means no updater artifact is published yet (e.g. the tag
  // exists but latest.json hasn't landed) OR we're current; on the silent path
  // that's not an error, just skip.
  const update = await check();
  if (!update) {
    console.warn("updateMount: no signed update package yet; skipping silently.");
    return;
  }

  let lastActivity = Date.now();
  let stallTimer: number | undefined;
  const stalled = new Promise<never>((_, reject) => {
    const tick = () => {
      if (Date.now() - lastActivity > STALL_MS) {
        reject(new Error("The update download stalled — check your connection."));
      } else {
        stallTimer = window.setTimeout(tick, 5000);
      }
    };
    stallTimer = window.setTimeout(tick, 5000);
  });

  try {
    await Promise.race([
      update.downloadAndInstall((event) => {
        // Any event resets the watchdog.
        lastActivity = Date.now();
        // We don't render progress on the silent launch path (no overlay here),
        // but draining the events keeps the watchdog fed.
        void event;
      }),
      stalled,
    ]);
    // On Windows the installer typically restarts the app itself, so we rarely
    // get here; relaunch covers the paths/platforms where execution continues.
    await relaunch();
  } finally {
    if (stallTimer) window.clearTimeout(stallTimer);
  }
}

mountUpdateCheck();
