// The workspace store holds the live terminal set and a list of user-named
// workspace *tabs* (PRD §5.2). Each tab is its own canvas: an ordered set of
// terminal tiles plus optional manual-mode size ratios for the grid's rows and
// columns (PRD §5.3). Exactly one tab is active; the active tab's canvas is the
// one the user interacts with. Every tab stays MOUNTED at all times (shell v2):
// the canvas toggles inactive tabs with CSS `display:none` and keeps passing
// `visible={true}` to their tiles, so xterm/PTY clients stay attached in the
// background and switching tabs never reloads a terminal. The global terminal
// font size (zoom) is shared by every tile.
//
// Persistence (PRD §6.5, FR-010) is localStorage for now (SQLite lands later):
// we persist the tab list, the active tab, and the font size. The live
// `terminals` map is NOT persisted -- it is re-fetched from the backend on
// mount via listTerminals() and reconciled back onto the persisted tabs.
//
// STRUCTURE: this file is the store's COMPOSITION ROOT. The action bodies live in
// slice factories under ./workspace/*.ts, each `createXSlice(set, get, deps)`
// returning a chunk of actions; `useWorkspace` spreads them all into one
// `create<WorkspaceState>` call (a single store, one `WorkspaceState` shape). The
// PURE helpers, constants, and types the slices share live in ./workspace/internal
// (no reference to `useWorkspace`, so the slices import from there without a cycle).
// The persistence/hydration infrastructure and shared MODULE state below
// (`persist`/`savePersisted`/`saveToBackend`/`loadPersisted`/`hydrateFromBackend`/
// `adoptDurableLayout`, the satellite scope, the captain-registry accessor, the
// in-flight recall guard, and the label-title subscription) stay module-level here
// and run exactly as before; the store-bound ones are handed to the slices via the
// `deps` object built inside the store closure.
import { create } from "zustand";
import type { TerminalInfo, TerminalId } from "../ipc/types";
import { usePanels } from "./panels";
import { useTheme } from "./theme";
import { useSupervision } from "./supervision";
import { useSessionContext, sessionNameForTerminal } from "./sessionContext";
import { useActivity } from "./activity";
import { onControlEvent } from "../ipc/controlClient";
import {
  CAPTAINS_TAB_ID,
  DEFAULT_TAB_NAME,
  PERSIST_KEY,
  adoptOrphans,
  cwdBasename,
  loadPersisted,
  mergeLabels,
  newTabId,
  parseV2Snapshot,
  satelliteTabId,
  scopeToSatellite,
  workspaceKind,
  type PersistedLayout,
  type SliceDeps,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace/internal";
import { createTerminalsSlice } from "./workspace/terminals";
import { createLabelsSlice } from "./workspace/labels";
import { createTabsSlice } from "./workspace/tabs";
import { createTilesSlice } from "./workspace/tiles";
import { createLifecycleSlice } from "./workspace/lifecycle";
import { createNavigationSlice } from "./workspace/navigation";
import { createRecallSlice } from "./workspace/recall";
import { createWorktreesSlice } from "./workspace/worktrees";
import { createZoomSlice } from "./workspace/zoom";

// Re-export the public surface the rest of the app imports from this module so
// consumers keep importing from `../store/workspace` byte-for-byte unchanged.
export {
  CAPTAINS_TAB_ID,
  CAPTAINS_TAB_NAME,
  deriveLabel,
  mergeLabels,
} from "./workspace/internal";
export type {
  FocusRegion,
  LabelSource,
  TabSizes,
  WorkspaceKind,
  WorkspaceState,
  WorkspaceTab,
} from "./workspace/internal";

/**
 * Clean up the per-tile side state that lives OUTSIDE this store when a
 * terminal's tile goes away for good (detach / delete / close-tab). This is the
 * SINGLE close-cleanup hub: every per-terminal / per-session map keyed by an id
 * that is minted fresh per spawn must be pruned here or it grows forever (the
 * perf-audit leak). Covers:
 *   - the per-tile panel state (active view, detected/typed URLs) in usePanels;
 *   - the per-terminal color overrides in useTheme;
 *   - this store's own per-terminal LABEL maps (labels / userLabels /
 *     claudeTitles — userLabels is persisted, so its prune is persisted too);
 *   - the supervision store's session-keyed maps (trees / statuses / snapshots /
 *     sessionIdByTmux) via its `remove(sessionId)`, resolving the session from
 *     this terminal's `th_<id>` tmux name through the reverse index;
 *   - the context-meter reading (useSessionContext.forget);
 *   - the output-activity entry + its idle timer (useActivity.forget);
 *   - the DevTab module-level state + its live backend listener (forgetDevState);
 *   - any managed dev server still running for it (a fire-and-forget Tauri call,
 *     dynamically imported so the store stays web/test-safe — a no-op if there's
 *     no dev server for this id or no Tauri runtime).
 * Called from `remove` (which funnels detach + delete) and `closeTab`. NOT called
 * on a tab MOVE — a moved tile keeps its panel state.
 */
function cleanupTileSideState(id: TerminalId): void {
  usePanels.getState().forget(id);
  // Drop any per-terminal color override so a recycled id can't inherit it.
  useTheme.getState().clearTermOverride(id);
  useTheme.getState().clearTermFocusRing(id);
  // (The cosmetic work name is keyed by CWD, not terminal id, so it is durable —
  // intentionally NOT cleared here; it persists with the project.)

  // Prune THIS store's per-terminal label maps so they don't grow once per
  // spawned terminal. userLabels is persisted, so dropping it here keeps the
  // saved snapshot from accumulating dead ids too. Done via setState (we're a
  // module-level helper, not inside the store closure); persisting is left to the
  // caller's own persist() in remove()/closeTab().
  forgetTerminalLabels(id);

  // Supervision store: its trees/statuses/snapshots/sessionIdByTmux are keyed by
  // Claude session id (a fresh UUID per spawn/resume) and were never pruned. The
  // tile only knows its terminal id, so resolve the session via the reverse index
  // sessionIdByTmux[`th_<id>`] (the tmux session T-Hub gives every terminal),
  // then drop it. No-op when this terminal never ran a Claude session.
  const sup = useSupervision.getState();
  const sessionId = sup.sessionIdByTmux[sessionNameForTerminal(id)];
  if (sessionId) sup.remove(sessionId);

  // Context-meter reading (keyed by `th_<id>` session name) and output-activity
  // entry (keyed by terminal id, + its idle timer) — both grow per spawn.
  useSessionContext.getState().forget(id);
  useActivity.getState().forget(id);

  // DevTab module-level state + its live backend listener. Dynamic import keeps
  // the store from eagerly pulling a React component module and stays web/test
  // safe (same pattern as the devserver stop below).
  void import("../components/DevTab")
    .then((m) => m.forgetDevState(id))
    .catch(() => {
      /* DevTab never loaded for this id, or no runtime — nothing to forget */
    });

  void import("../ipc/devserver")
    .then((m) => m.stopDevServer(id))
    .catch(() => {
      /* no dev server for this id, or no Tauri runtime — nothing to stop */
    });

  // Captain designation (captain-list): if this terminal was one of the pinned
  // captains, unpin it (and drop the overlay if it was the summoned one) so no
  // designation ever points at a dead id. Dynamic import like DevTab above -
  // captain.ts imports this store, so a static import here would form a cycle.
  void import("./captain")
    .then((m) => m.forgetCaptain(id))
    .catch(() => {
      /* captain store never loaded - nothing pinned */
    });
}

/**
 * Kill the OLD tmux session behind a restart, retrying ONCE. `restartTerminal`
 * has already swapped in the fresh tile, so this runs detached from the return —
 * but a silently-failed kill would leak a frozen tmux session with no tile bound
 * to it, so on a SECOND failure we surface a visible notice (an OS toast via
 * lib/notify + a console.error) so the user knows the old session may still be
 * running and can kill it manually. `killTerminal` is injected so the store stays
 * free of a hard Tauri dependency (and the retry path is unit-testable).
 */
async function killOldSessionWithRetry(
  id: TerminalId,
  killTerminal: (id: TerminalId) => Promise<void>,
): Promise<void> {
  try {
    await killTerminal(id);
    return;
  } catch (first) {
    console.error("restartTerminal: kill old session failed, retrying", first);
  }
  try {
    await killTerminal(id);
  } catch (second) {
    console.error(
      "restartTerminal: kill old session failed after retry",
      second,
    );
    // Surface it: the old tmux session may still be running with no tile bound.
    void import("../lib/notify")
      .then((m) =>
        m.notify(
          "error",
          "Old session may still be running",
          `Couldn't kill the old tmux session ${id} after restarting it. It may still be running in the background — kill it manually if it lingers.`,
        ),
      )
      .catch(() => {
        /* no notify runtime (web/test) — the console.error above stands */
      });
  }
}

/**
 * Delete a terminal's entries from all three label maps (effective `labels`, the
 * persisted `userLabels` source of truth, and the live `claudeTitles`). A module
 * helper rather than a store action because `cleanupTileSideState` runs outside
 * the store closure; it writes via setState and no-ops when nothing is keyed
 * under `id` (so it never thrashes subscribers on the common no-label close).
 */
function forgetTerminalLabels(id: TerminalId): void {
  const { labels, userLabels, claudeTitles } = useWorkspace.getState();
  if (!(id in labels) && !(id in userLabels) && !(id in claudeTitles)) return;
  const nextLabels = { ...labels };
  const nextUser = { ...userLabels };
  const nextClaude = { ...claudeTitles };
  delete nextLabels[id];
  delete nextUser[id];
  delete nextClaude[id];
  useWorkspace.setState({
    labels: nextLabels,
    userLabels: nextUser,
    claudeTitles: nextClaude,
  });
}

/** A synchronous read of the server-backed AGENT id set, registered by the
 *  captain store. captain.ts imports THIS store, so it registers its accessor
 *  here rather than us importing it back - keeping the workspace store free of a
 *  static captain-store cycle. adoptRegistry uses this as its only non-tab
 *  liveness fallback: a presentation-only local pin must never resurrect a
 *  retired backend identity during restart reconciliation. */
let captainRegistryIds: () => Iterable<TerminalId> = () => [];

/** Local presentation IDs remain useful for protective UX: a workspace close
 *  moves a pinned agent instead of killing it, and registry-less boot recovery
 *  keeps its tile in Captain Workspace. They are deliberately separate from
 *  authoritative liveness because local pins can outlive their processes. */
let agentPresentationIds: () => Iterable<TerminalId> = () => [];

/** Register the captain store's server-backed claim accessor. */
export function registerCaptainRegistry(fn: () => Iterable<TerminalId>): void {
  captainRegistryIds = fn;
}

/** Register the captain store's local presentation/protection accessor. */
export function registerAgentPresentation(
  fn: () => Iterable<TerminalId>,
): void {
  agentPresentationIds = fn;
}

/**
 * Mirror the layout JSON into the durable SQLite copy (#sqlite phase 1), in
 * addition to localStorage. Best-effort and fire-and-forget: the import is
 * dynamic so the store keeps no hard dependency on Tauri (a plain web/test
 * context without a backend must not throw), and failures are swallowed — the
 * localStorage copy above remains the live source whenever the backend is
 * absent. Skipped in a satellite window (it holds only its own pruned tab and
 * must never clobber the shared snapshot the main window owns).
 */
function saveToBackend(json: string): void {
  if (SATELLITE_TAB) return;
  void import("../ipc/persistence")
    .then((m) => {
      // Per-variant durable copy (SQLite) - the primary durable store. Each call
      // carries its own catch: "failures are swallowed" must hold for the invoke
      // itself too (without a backend these reject asynchronously).
      m.saveWorkspaceSnapshot(json).catch(() => {});
      // Shared, all-variants copy (~/.config/t-hub/workspaces.json, #9): the
      // cross-variant carrier so a dev↔prod switch keeps your workspaces.
      m.saveSharedLayout(json).catch(() => {});
    })
    .catch(() => {});
}

/** Persist the layout subset (best-effort; ignore quota/serialization errors).
 *  Writes the localStorage mirror synchronously, then fans the same JSON out to
 *  the durable SQLite copy (fire-and-forget). */
function savePersisted(layout: PersistedLayout): void {
  let json: string;
  try {
    json = JSON.stringify(layout);
  } catch {
    return; // un-serializable layout — nothing to persist
  }
  if (typeof localStorage !== "undefined") {
    try {
      localStorage.setItem(PERSIST_KEY, json);
    } catch {
      /* ignore quota errors — the durable copy below still runs */
    }
  }
  saveToBackend(json);
}

// The satellite tab id for THIS window (null in the main window). Captured once
// at module load: a satellite scopes its initial layout to that one tab and
// never persists (so it can't overwrite the main window's full snapshot).
const SATELLITE_TAB = satelliteTabId();

// In-flight recall guard (#7): recall (Recent resume / Recovery Restore) spawns
// a tmux session + claude --resume, which takes a moment. A double-click would
// otherwise fire it twice and stack DUPLICATE spawns for the same session. We
// track the ids whose recall is currently in flight (keyed by sessionId) and
// ignore a second invocation until the first settles. Module-level (not store
// state) since it's transient plumbing, never rendered or persisted.
const recallInFlight = new Set<string>();

const loaded = loadPersisted();
const initial = SATELLITE_TAB
  ? scopeToSatellite(loaded, SATELLITE_TAB)
  : adoptOrphans(loaded);

/** Whether this window is a satellite (popped-out tab) window. Exported for the
 *  control bridge: a satellite holds ONE tab of the layout, so it must neither
 *  apply global organization mutations nor up-sync its scoped tab list over the
 *  main window's report (which would clobber the registry down to one tab). */
export function isSatelliteWindow(): boolean {
  return SATELLITE_TAB !== null;
}

export const useWorkspace = create<WorkspaceState>((set, get) => {
  // Persist the current (tabs, activeTabId, focusedId, fontSize, poppedOutTabs).
  // Suppressed in a satellite window: it holds only its own tab, so writing would
  // clobber the shared snapshot the main window owns.
  const persist = () => {
    if (SATELLITE_TAB) return;
    const { tabs, activeTabId, focusedId, fontSize, userLabels, poppedOutTabs } =
      get();
    savePersisted({
      tabs,
      activeTabId,
      focusedId,
      fontSize,
      // Persist ONLY explicit user renames (the `labels` key); Claude-derived
      // titles are live-only and must not survive a reload as fake renames.
      labels: userLabels,
      poppedOutTabs,
    });
  };

  /** The active tab (always present: the store guarantees >=1 tab). */
  const activeTab = (): WorkspaceTab => {
    const { tabs, activeTabId } = get();
    return tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  };

  /** Place a spawned WORK tile into a work tab, never the reserved Captains tab
   *  (only captain/orchestrator AGENT tiles belong there, placed via
   *  moveTileToCaptainsTab). Prefers `preferredTabId` when it names a real work
   *  tab, else the first existing work tab, else a freshly minted one. Activates
   *  the target tab and focuses the tile. Used by the spawn primitives to keep a
   *  plain spawn out of Captains when it happens to be the active tab. */
  const placeWorkTile = (info: TerminalInfo, preferredTabId?: string): void => {
    const { tabs, terminals } = get();
    const isWork = (t: WorkspaceTab): boolean => workspaceKind(t) === "work";
    const preferred =
      preferredTabId && preferredTabId !== CAPTAINS_TAB_ID
        ? tabs.find((t) => t.id === preferredTabId && isWork(t))
        : undefined;
    const target = preferred ?? tabs.find(isWork);
    if (target) {
      set({
        terminals: { ...terminals, [info.id]: info },
        tabs: tabs.map((t) =>
          t.id === target.id ? { ...t, order: [...t.order, info.id] } : t,
        ),
        activeTabId: target.id,
        focusedId: info.id,
      });
    } else {
      // All-reserved edge: no work tab exists. Mint one BEFORE the reserved tab
      // so Captains stays last.
      const fresh: WorkspaceTab = {
        id: newTabId(),
        name: DEFAULT_TAB_NAME,
        order: [info.id],
      };
      set({
        terminals: { ...terminals, [info.id]: info },
        tabs: [...tabs.filter(isWork), fresh, ...tabs.filter((t) => !isWork(t))],
        activeTabId: fresh.id,
        focusedId: info.id,
      });
    }
    persist();
  };

  // The store-bound helpers each slice needs (the ones that close over this
  // store instance or over module state that stays in this file). Built once and
  // handed to every slice factory below.
  const deps: SliceDeps = {
    persist,
    activeTab,
    placeWorkTile,
    cleanupTileSideState,
    killOldSessionWithRetry,
    captainRegistryIds: () => captainRegistryIds(),
    agentPresentationIds: () => agentPresentationIds(),
    satelliteTab: SATELLITE_TAB,
    recallInFlight,
  };

  return {
    terminals: {},
    tabs: initial.tabs,
    activeTabId: initial.activeTabId,
    focusedId: initial.focusedId,
    focusedRegion: "terminal",
    fontSize: initial.fontSize,
    // `initial.labels` is the persisted user-rename set. The effective `labels`
    // starts equal to it (no Claude titles yet this session); `claudeTitles`
    // fills in live as the hooks fire.
    labels: initial.labels,
    userLabels: initial.labels,
    claudeTitles: {},
    poppedOutTabs: initial.poppedOutTabs,
    draggingTileId: null,
    draggingTabId: null,
    dropTileId: null,
    dropTabId: null,
    registryAdopted: false,

    ...createTerminalsSlice(set, get, deps),
    ...createLabelsSlice(set, get, deps),
    ...createTabsSlice(set, get, deps),
    ...createTilesSlice(set, get, deps),
    ...createLifecycleSlice(set, get, deps),
    ...createNavigationSlice(set, get, deps),
    ...createRecallSlice(set, get, deps),
    ...createWorktreesSlice(set, get, deps),
    ...createZoomSlice(set, get, deps),
  };
});

// ---------------------------------------------------------------------------
// Memoized terminal -> owning-tab lookup (#5). Building the tile chrome, every
// Tile used to subscribe to the whole `tabs` array and run
// `tabs.find(t => t.order.includes(id))` per render — O(tabs × order) PER TILE on
// every tabs change. Instead we cache one `terminalId -> tabId` Map and rebuild
// it ONLY when the `tabs` reference changes (the store always replaces `tabs`
// immutably, so a reference check is exact). Each tile then does an O(1) Map get
// against the same memoized result, with the IDENTICAL tabId outcome.
// ---------------------------------------------------------------------------
let tileTabCacheRef: WorkspaceTab[] | null = null;
let tileTabMap: Map<TerminalId, string> = new Map();

/** The `terminalId -> tabId` map for the current `tabs`, rebuilt only when the
 *  `tabs` array reference changes. The FIRST tab containing an id wins (the
 *  `!has` guard), exactly matching `tabs.find(t => t.order.includes(id))` if an
 *  id somehow appeared in two tabs — though ids are unique across tabs in
 *  practice. */
function tileTabLookup(tabs: WorkspaceTab[]): Map<TerminalId, string> {
  if (tabs !== tileTabCacheRef) {
    const next = new Map<TerminalId, string>();
    // Iterate so the FIRST tab containing an id wins, exactly matching the old
    // `tabs.find(t => t.order.includes(id))` semantics.
    for (const t of tabs) {
      for (const id of t.order) {
        if (!next.has(id)) next.set(id, t.id);
      }
    }
    tileTabMap = next;
    tileTabCacheRef = tabs;
  }
  return tileTabMap;
}

/** The id of the tab that owns `terminalId`, or undefined. Use as a selector
 *  (`useWorkspace((s) => tabIdForTerminal(s, id))`): it subscribes to `tabs` but
 *  returns a stable string, so a tile only re-renders when ITS tab id changes —
 *  and the per-call work is an O(1) Map get off the memoized lookup. */
export function tabIdForTerminal(
  s: WorkspaceState,
  terminalId: TerminalId,
): string | undefined {
  return tileTabLookup(s.tabs).get(terminalId);
}

/**
 * Hydrate the live store from the durable SQLite snapshot (#sqlite phase 1),
 * preferring it over the localStorage copy the store already booted from. Runs
 * once at module load, off the critical path:
 *
 *   - SATELLITE windows are skipped entirely. They scope to a single tab and
 *     never persist, so they must not pull (or seed) the shared full snapshot.
 *   - If SQLite HAS a snapshot, parse + finalize it (same invariants as the
 *     localStorage path), adopt any orphaned popped-out tabs (a fresh main-window
 *     launch owns every popped tab — see adoptOrphans), and apply ONLY the
 *     persisted fields. The live `terminals` map and transient drag state are
 *     left untouched; setTerminals() will reconcile real tiles from the backend.
 *     The localStorage mirror is refreshed via persist() so both copies align.
 *   - If SQLite is EMPTY (fresh install, or first run after this feature ships),
 *     seed it once from whatever the store booted with — migrating the existing
 *     localStorage arrangement into the durable copy.
 *
 * Best-effort: a missing backend (plain web / test) or any error is swallowed,
 * leaving the localStorage-derived boot state in place. The dynamic import keeps
 * the store free of a hard Tauri dependency.
 *
 * Race note: this resolves a microtask/IPC hop after module load, typically
 * before components mount and call setTerminals(). If a spawn/reconcile lands
 * first, applying the snapshot's tab ORDER here would still be correct — the
 * next setTerminals() re-prunes to live ids — but to avoid yanking a tile the
 * user just acted on, we only adopt the SQLite layout when it is non-trivially
 * present and the store still holds its initial (un-reconciled) terminal set.
 */
/** Adopt a durable layout (the per-variant SQLite snapshot OR the shared file)
 *  onto the store — but only while no live terminals have reconciled yet, so we
 *  never yank a tile the user just acted on. Re-mirrors to localStorage + both
 *  durable copies afterward so all three agree. */
function adoptDurableLayout(snapshot: PersistedLayout): void {
  const layout = adoptOrphans(snapshot);
  if (Object.keys(useWorkspace.getState().terminals).length > 0) return;
  const claudeTitles = useWorkspace.getState().claudeTitles;
  useWorkspace.setState({
    tabs: layout.tabs,
    activeTabId: layout.activeTabId,
    focusedId: layout.focusedId,
    fontSize: layout.fontSize,
    userLabels: layout.labels,
    labels: mergeLabels(layout.labels, claudeTitles),
    poppedOutTabs: layout.poppedOutTabs,
  });
  savePersisted({
    tabs: layout.tabs,
    activeTabId: layout.activeTabId,
    focusedId: layout.focusedId,
    fontSize: layout.fontSize,
    labels: layout.labels,
    poppedOutTabs: layout.poppedOutTabs,
  });
}

async function hydrateFromBackend(): Promise<void> {
  if (SATELLITE_TAB) return;
  if (typeof window === "undefined") return; // no webview → no backend
  try {
    const { loadWorkspaceSnapshot } = await import("../ipc/persistence");
    const json = await loadWorkspaceSnapshot();
    const snapshot = parseV2Snapshot(json);

    if (!snapshot) {
      // No per-variant durable copy yet (a FRESH variant / first run). Before
      // seeding defaults, try the SHARED, all-variants layout (#9) — this is what
      // carries your workspaces across a dev↔prod switch.
      let shared: PersistedLayout | null = null;
      try {
        const { loadSharedLayout } = await import("../ipc/persistence");
        shared = parseV2Snapshot(await loadSharedLayout());
      } catch {
        /* no backend — fall through to seeding */
      }
      if (shared) {
        // adoptDurableLayout re-mirrors into localStorage + the per-variant copy.
        adoptDurableLayout(shared);
        return;
      }
      // Nothing shared either: seed BOTH durable copies (SQLite + shared file)
      // once from the current (localStorage-derived) layout so later boots and
      // other variants can prefer the durable copies.
      const {
        tabs,
        activeTabId,
        focusedId,
        fontSize,
        userLabels,
        poppedOutTabs,
      } = useWorkspace.getState();
      saveToBackend(
        JSON.stringify({
          tabs,
          activeTabId,
          focusedId,
          fontSize,
          // Persist only explicit user renames; Claude titles are live-only.
          labels: userLabels,
          poppedOutTabs,
        }),
      );
      return;
    }

    // The durable per-variant copy wins (adopted only while no live terminals have
    // reconciled yet — that guard lives inside adoptDurableLayout, which also
    // re-mirrors to localStorage + the shared file so all copies agree).
    adoptDurableLayout(snapshot);
  } catch {
    // No backend or a transient error — keep the localStorage-derived boot state.
  }
}

// Kick off durable hydration once, fire-and-forget. Never blocks module load.
void hydrateFromBackend();

// ---------------------------------------------------------------------------
// GOAL NAMES: feed Claude-suggested titles from the lifecycle hooks into the
// label map. The backend emits `agent://title` ({ sessionId, cwd, title }) when
// a hook (UserPromptSubmit / SessionStart) yields a usable summary. T-Hub
// terminals are keyed by their own tmux id, not the Claude session id, so we
// correlate by working directory (both are WSL-side paths). The matched
// terminal's Claude title is then merged into `labels` (a user rename always
// wins) so `deriveLabel` prefers what Claude is doing over the raw command·cwd.
//
// Subscribed here (not via client05) to keep the wiring inside the label region
// this store owns. Satellite windows also listen; setClaudeTitle is a no-op for
// ids they don't render, so the extra entries are inert.
// ---------------------------------------------------------------------------

/** Normalize a cwd for correlation: strip trailing separators, lower-case (WSL
 *  paths are case-sensitive but our match is a best-effort heuristic). */
function normCwd(cwd: string | undefined): string {
  if (!cwd) return "";
  return cwd.replace(/[/\\]+$/, "").toLowerCase();
}

/** Find the terminal whose cwd best matches a hook event's cwd: an exact
 *  (normalized) path match first, then a unique cwd-basename match as a looser
 *  fallback. Returns the terminal id, or null if there is no unambiguous match. */
function terminalForCwd(
  terminals: Record<TerminalId, TerminalInfo>,
  hookCwd: string | undefined,
): TerminalId | null {
  const target = normCwd(hookCwd);
  if (!target) return null;
  const entries = Object.values(terminals);
  const exact = entries.filter((t) => normCwd(t.cwd) === target);
  if (exact.length === 1) return exact[0].id;
  if (exact.length > 1) return null; // ambiguous: don't mislabel
  // Looser fallback: a single terminal sharing the basename (e.g. /mnt/c vs
  // /home symlink skew). Only when it is unambiguous.
  const base = cwdBasename(hookCwd);
  if (!base) return null;
  const byBase = entries.filter((t) => cwdBasename(t.cwd) === base);
  return byBase.length === 1 ? byBase[0].id : null;
}

/** Subscribe to `agent://title` and route each Claude-derived title onto the
 *  matching terminal's label. Delivered over the loopback control socket like
 *  every other bridge channel — the backend emits it through the SocketEmitter and
 *  the forwarder re-emits it as `control://event`, which `onControlEvent` demuxes
 *  by channel. (Previously a raw `listen("agent://title")` on the in-process Tauri
 *  leg; that leg is gone now that bridge events are single-sourced over the
 *  socket.) Synchronous + fire-and-forget; outside a Tauri runtime the backing
 *  listener simply never fires (non-fatal). */
function subscribeClaudeTitles(): void {
  onControlEvent("agent://title", (p) => {
    const { sessionId, cwd, title } = p as {
      sessionId: string;
      cwd?: string;
      title: string;
    };
    if (!title) return;
    const terminals = useWorkspace.getState().terminals;
    // Prefer EXACT session→terminal routing. The supervision store maps each tmux
    // session (`th_<id>`) to its Claude sessionId, so we can land the title on the
    // precise terminal — even when TWO terminals share a cwd (or cwd-basename),
    // which cwd correlation CANNOT disambiguate: `terminalForCwd` returns null on
    // a tie, so both same-folder terminals would otherwise miss their own goal
    // title and collapse to the identical `claude · <folder>` fallback (the bug
    // where same-folder terminals looked linked). Fall back to cwd only until the
    // session→tmux map is populated (before the first status snapshot lands).
    let id: TerminalId | null = null;
    if (sessionId) {
      const { sessionIdByTmux } = useSupervision.getState();
      for (const t of Object.values(terminals)) {
        if (sessionIdByTmux[sessionNameForTerminal(t.id)] === sessionId) {
          id = t.id;
          break;
        }
      }
    }
    if (!id) id = terminalForCwd(terminals, cwd);
    if (id) useWorkspace.getState().setClaudeTitle(id, title);
  });
}

subscribeClaudeTitles();
