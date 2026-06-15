// Frontend updater wrapper (feat/auto-updater).
//
// A thin, safe layer over the official Tauri updater plugin. `detectUpdate()`
// asks the updater whether a newer signed release is published at the
// `latest.json` endpoint (a GitHub Releases asset, see plugins.updater in
// tauri.conf.json) and reports it alongside the current runtime version.
//
// Why latest.json (not the GitHub REST API): the updater's check() reads a
// release CDN file, so it isn't subject to api.github.com's ~60 req/hr
// unauthenticated rate limit that frequent polling would trip. It's also the
// exact same source the actual install uses, so detection and install can never
// disagree. (This mirrors the sibling "scribe" app's lib/updates.ts.)
//
// Robustness: outside Tauri (a plain `pnpm dev` browser session) or when the
// endpoint is unreachable, this resolves to a safe "no update" rather than
// throwing, so callers (the Settings UI, the on-launch mount) never break.

import { check } from "@tauri-apps/plugin-updater";
import { getVersion } from "@tauri-apps/api/app";

/** The GitHub releases list — the changelog the user can always reference. */
export const RELEASES_URL = "https://github.com/n8watkins/termhub/releases";

export interface UpdateCheckResult {
  /** True when a newer signed release is available at the endpoint. */
  updateAvailable: boolean;
  /** The version this build is running (Tauri runtime version). */
  currentVersion: string;
  /** The newest available version (equals currentVersion when up to date). */
  latestVersion: string;
}

/**
 * Detect whether an update is available via the updater's `latest.json`
 * endpoint. Shaped as {@link UpdateCheckResult} for the Settings UI and the
 * on-launch mount.
 *
 * Tolerant by design: any failure (no Tauri, offline, missing/unsigned
 * latest.json) resolves to a safe "no update" instead of throwing, so a
 * background poll or launch check can never block the app. If you need the raw
 * error (e.g. a manual "Check for updates" button wants to surface it), call the
 * plugin's `check()` directly.
 */
export async function detectUpdate(): Promise<UpdateCheckResult> {
  // getVersion() is only meaningful inside Tauri; fall back to "0.0.0" so the
  // result stays well-formed in a plain browser dev session.
  let currentVersion = "0.0.0";
  try {
    currentVersion = await getVersion();
  } catch {
    // Not inside Tauri — keep the placeholder; we'll report "no update" below.
    return {
      updateAvailable: false,
      currentVersion,
      latestVersion: currentVersion,
    };
  }

  try {
    const update = await check();
    if (update) {
      return {
        updateAvailable: true,
        currentVersion,
        latestVersion: update.version,
      };
    }
  } catch {
    // Offline, rate-limited, or no signed artifact published yet — treat as
    // "no update" so the caller never has to handle a thrown error.
  }

  return {
    updateAvailable: false,
    currentVersion,
    latestVersion: currentVersion,
  };
}
