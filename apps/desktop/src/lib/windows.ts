// Multi-window tear-off plumbing (#21, PHASE 1).
//
// TermHub is normally a single frameless window with a strip of workspace tabs,
// each tab being a canvas of terminal tiles. This module lets the user "pop" a
// tab out into a SECOND TermHub window:
//
//   * The new window loads the SAME app bundle with `?tab=<id>` in the URL. On
//     boot the workspace store reads that param: when present the window is a
//     SATELLITE that holds ONLY that one tab (so the shared Canvas renders just
//     its canvas, and only its terminals attach); when absent it is the MAIN
//     window (all tabs, minus any popped out).
//   * A popped-out tab's full record is moved out of the main window's `tabs`
//     into `poppedOutTabs` (see workspace.ts), so the strip + canvas stop
//     rendering it WITHOUT touching Canvas.tsx — exactly ONE window ever renders
//     a given tab. (Two tmux clients attached to one session interleave output.)
//   * Windows stay in sync LIVE via a Tauri event (`workspace://popout`): pop-out
//     / pop-in broadcasts an intent tagged with the sender's window label (so we
//     ignore our own echo); the main window re-adopts a tab when its satellite
//     reports popping back in. Persistence (shared localStorage) covers a
//     freshly launched window; the event covers already-open ones.
//
// LATER PHASES (NOT done here — clean seam only): dragging a tile/tab BETWEEN
// windows, and a drag-to-return gesture. Closing a satellite already returns its
// tab to the main window (the minimal pop-in below); there is no drag yet.
//
// We deliberately use the JS `WebviewWindow` API (no Rust window command needed).
// Satellites force-close via `destroy()` rather than `close()` so they bypass the
// app's close-to-tray interception (src-tauri/src/tray.rs hides on CloseRequested
// — desired for the MAIN window, wrong for a throwaway satellite).
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useWorkspace } from "../store/workspace";
import type { WorkspaceTab } from "../store/workspace";

/** Cross-window resync channel. */
const POPOUT_EVENT = "workspace://popout";
/** Window-label prefix for satellites; the suffix is the tab id it renders. */
const SATELLITE_PREFIX = "th-pop-";

/**
 * Cross-window intent. `out`: a tab was torn off (informational for phase 1 —
 * the main window that emitted it already updated; sent for symmetry + future
 * multi-main). `in`: a satellite is closing and hands its tab (latest record)
 * back to the main window to re-adopt.
 */
interface PopoutMsg {
  kind: "out" | "in";
  tabId: string;
  /** The tab's latest record, sent with `in` so the main window restores order. */
  tab?: WorkspaceTab;
  /** Label of the emitting window, so receivers can ignore self-echo. */
  from: string;
}

/** This window's Tauri label (e.g. "main" or "th-pop-tab-abc-1"). */
function selfLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main"; // plain `pnpm dev` (no Tauri): behave as the main window.
  }
}

/**
 * The tab id this window was opened to render in isolation, or null for the main
 * window. Read from the `?tab=<id>` URL param. A satellite shows only this tab.
 */
export function readSatelliteTab(): string | null {
  if (typeof location === "undefined") return null;
  try {
    const id = new URLSearchParams(location.search).get("tab");
    return id && id.trim() ? id : null;
  } catch {
    return null;
  }
}

/** True when running as a satellite (URL carried a `?tab=`). */
export function isSatellite(): boolean {
  return readSatelliteTab() !== null;
}

/** Broadcast a tear-off intent to the other window(s). Best-effort. */
function broadcast(kind: "out" | "in", tabId: string, tab?: WorkspaceTab): void {
  const msg: PopoutMsg = { kind, tabId, tab, from: selfLabel() };
  void emit(POPOUT_EVENT, msg).catch(() => {});
}

/**
 * Pop a workspace tab out into its own TermHub window.
 *
 * Moves the tab into the popped-out set (so THIS — the main — window stops
 * rendering it), broadcasts, then creates the satellite window loading
 * `index.html?tab=<id>`. Reuses + focuses an existing satellite for the same tab
 * rather than spawning a duplicate.
 */
export async function popOutTab(tabId: string): Promise<void> {
  const label = SATELLITE_PREFIX + tabId;
  const existing = await WebviewWindow.getByLabel(label).catch(() => null);
  if (existing) {
    await existing.setFocus().catch(() => {});
    return;
  }

  // Update local state + tell the other window BEFORE creating the satellite, so
  // the main window has already stopped rendering the tab by the time the
  // satellite attaches — only one client is ever attached to the session.
  useWorkspace.getState().popOutTab(tabId);
  broadcast("out", tabId);

  // Same app bundle, scoped to this tab via the query param. Frameless to match
  // the main window (its own <Titlebar/> is the chrome). Modest default size.
  const win = new WebviewWindow(label, {
    url: `index.html?tab=${encodeURIComponent(tabId)}`,
    title: "TermHub",
    decorations: false,
    width: 1000,
    height: 700,
    minWidth: 480,
    minHeight: 320,
  });

  win.once("tauri://error", (e) => {
    console.error("popOutTab: failed to create satellite window", e);
  });
}

/**
 * Close THIS satellite window, returning its tab to the main window. Broadcasts
 * an `in` with the tab's CURRENT record (so the main window restores the latest
 * tile order), then force-destroys the window (bypassing close-to-tray, which
 * would merely hide a still-attached client). Safe to call only from a satellite.
 */
export async function closeSatellite(): Promise<void> {
  const tabId = readSatelliteTab();
  if (tabId) {
    const tab = useWorkspace.getState().tabs.find((t) => t.id === tabId);
    broadcast("in", tabId, tab);
  }
  // Let the broadcast flush before tearing the window down.
  await new Promise((r) => setTimeout(r, 0));
  await getCurrentWindow()
    .destroy()
    .catch(() => {});
}

/**
 * Wire cross-window resync. Every window calls this once at boot:
 *
 *   * MAIN: on an `in` from a satellite, re-adopt that tab (it reappears in the
 *     strip + canvas). On an `out`, ensure it's popped out here too (defensive;
 *     normally the main window initiated it).
 *   * SATELLITE: on an `in` for ITS OWN tab from elsewhere (a future drag-to-
 *     return, or the main window reclaiming it), it has nothing left to render —
 *     it self-destroys.
 *
 * Returns an unlisten fn. Self-emitted events are ignored via the `from` label.
 */
export async function initWindowSync(): Promise<UnlistenFn> {
  const me = selfLabel();
  const satelliteTab = readSatelliteTab();

  const unlisten = await listen<PopoutMsg>(POPOUT_EVENT, (ev) => {
    const msg = ev.payload;
    if (!msg || msg.from === me) return; // ignore our own echo

    if (msg.kind === "in") {
      if (satelliteTab && msg.tabId === satelliteTab) {
        // Someone reclaimed our tab: stop rendering it (no double client).
        void getCurrentWindow()
          .destroy()
          .catch(() => {});
        return;
      }
      // Main window: re-adopt the returned tab with its latest record.
      useWorkspace.getState().popInTab(msg.tabId, msg.tab);
      return;
    }

    // kind === "out": make sure this window isn't also rendering that tab.
    if (msg.tabId !== satelliteTab) {
      useWorkspace.getState().popOutTab(msg.tabId);
    }
  });

  // On a fresh satellite launch, announce ourselves so the main window hides our
  // tab even if it missed the spawn-time broadcast (e.g. it was busy). The main
  // window's popOutTab is idempotent. (The store already scoped us to this tab.)
  if (satelliteTab) {
    broadcast("out", satelliteTab);
  }

  return unlisten;
}
