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
import { create } from "zustand";
import type { TabReport, TerminalInfo, TerminalId, TerminalState } from "../ipc/types";
import { usePanels } from "./panels";
import { useTheme } from "./theme";
import { useSupervision } from "./supervision";
import { useSessionContext, sessionNameForTerminal } from "./sessionContext";
import { useActivity } from "./activity";
import { onControlEvent } from "../ipc/controlClient";

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

/**
 * localStorage key for the workspace snapshot. v2 introduced workspace tabs;
 * a v1 snapshot (flat order/focus) is migrated into a single tab on load.
 */
const PERSIST_KEY = "t-hub.workspace.v2";
const LEGACY_KEY = "t-hub.workspace.v1";

/** Global terminal font size (px) bounds + default, shared by every tile. */
const DEFAULT_FONT_SIZE = 13;
const MIN_FONT_SIZE = 6;
const MAX_FONT_SIZE = 28;

/** Default name for the first/auto-created tab. */
const DEFAULT_TAB_NAME = "Workspace 1";

/**
 * The RESERVED "Captains" workspace tab (captains-workspace-tab). A normal
 * workspace tab - it renders ordinary terminal tiles through the same Canvas /
 * pool as any tab - but it is the agents' home: the orchestrator tile and every
 * pinned captain tile live here, kept OUT of the work tabs. It is:
 *   - fixed-id (a stable well-known id, so no `reserved` field is needed and the
 *     backend tab registry can't drift a second copy into existence);
 *   - always present (finalizeLayout auto-creates it; adoptRegistry re-injects it
 *     so a server snapshot can't drop it);
 *   - not closeable (closeTab/closeWorkspace refuse it).
 * Its `order` is the authoritative record of which tiles are placed as agents,
 * which is how placement survives a server registry sync. adoptRegistry consults
 * the captain registry ONLY as a liveness fallback (see {@link captainRegistryIds}),
 * via an accessor the captain store registers - so this store keeps NO static
 * dependency on the captain store.
 */
export const CAPTAINS_TAB_ID = "captains-reserved";
export const CAPTAINS_TAB_NAME = "Captains";

/** A synchronous read of the authoritative AGENT id set (the orchestrator plus
 *  every pinned/claimed captain), registered by the captain store. captain.ts
 *  imports THIS store, so it registers its accessor here rather than us importing
 *  it back - keeping the workspace store free of a static captain-store cycle
 *  (mirrors the dynamic-import `forgetCaptain` call). adoptRegistry uses it as a
 *  liveness fallback: an externally claimed captain (e.g. one the orchestrator
 *  claimed over the control socket) whose tile the server does not report as a
 *  live work-tab tile is still an authoritative captain and must not be dropped.
 *  Defaults to empty until the captain store loads, so any pre-registration sync
 *  falls back cleanly to the plain serverTileIds liveness. */
let captainRegistryIds: () => Iterable<TerminalId> = () => [];

/** Register the captain store's agent-id accessor (see {@link captainRegistryIds}).
 *  Called once by captain.ts at module load. */
export function registerCaptainRegistry(fn: () => Iterable<TerminalId>): void {
  captainRegistryIds = fn;
}

/** Return `tabs` guaranteed to contain EXACTLY ONE reserved Captains tab: if
 *  absent, append a fresh empty one; if duplicated (a stale persisted snapshot,
 *  or a server report that echoed the client-only reserved tab back), collapse
 *  every copy into a single tab that keeps the first copy's slot and unions their
 *  orders. Shared by finalizeLayout (load) and adoptRegistry (server sync) so the
 *  reserved tab can never be lost NOR duplicated - a duplicate empty copy would
 *  render a stray "new terminal" placeholder next to the real, populated one. */
function ensureReservedCaptainsTab(tabs: WorkspaceTab[]): WorkspaceTab[] {
  const copies = tabs.filter((t) => t.id === CAPTAINS_TAB_ID);
  if (copies.length === 0) {
    return [...tabs, { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] }];
  }
  if (copies.length === 1) return tabs;
  // Merge duplicates: union their orders (dedup, first-seen wins) into the first
  // copy's slot; drop the rest.
  const mergedOrder: TerminalId[] = [];
  const seen = new Set<TerminalId>();
  for (const c of copies) {
    for (const id of c.order) {
      if (!seen.has(id)) {
        seen.add(id);
        mergedOrder.push(id);
      }
    }
  }
  const merged: WorkspaceTab = {
    ...copies[0],
    id: CAPTAINS_TAB_ID,
    name: CAPTAINS_TAB_NAME,
    order: mergedOrder,
    // A changed tile set invalidates manual grid ratios.
    sizes: copies[0].order.length === mergedOrder.length ? copies[0].sizes : undefined,
  };
  let injected = false;
  const out: WorkspaceTab[] = [];
  for (const t of tabs) {
    if (t.id !== CAPTAINS_TAB_ID) {
      out.push(t);
    } else if (!injected) {
      out.push(merged);
      injected = true;
    }
  }
  return out;
}

/**
 * Manual-mode size ratios for one tab's grid. `rows` holds a flex-grow weight
 * per grid row; `cols[r]` holds a weight per tile within row `r`. Weights are
 * relative (the grid normalizes them), so any positive numbers work. Empty /
 * missing arrays mean "even split" (default auto-grid behavior).
 */
export interface TabSizes {
  rows: number[];
  cols: number[][];
}

/** A user-named canvas: an ordered tile set plus optional manual size ratios. */
export interface WorkspaceTab {
  id: string;
  name: string;
  /** Tile order within this tab, by terminal id. */
  order: TerminalId[];
  /** Optional manual-mode grid ratios; absent => even auto-grid. */
  sizes?: TabSizes;
}

/** The subset of state we persist across UI reopens. */
interface PersistedLayout {
  tabs: WorkspaceTab[];
  activeTabId: string;
  focusedId: TerminalId | null;
  fontSize: number;
  /** User-set per-terminal labels (#labels), keyed by terminal id. Frontend-only
   *  state — NOT part of the backend TerminalInfo contract — so it is persisted
   *  here alongside the layout rather than re-fetched. Absent ids fall back to a
   *  derived label (see deriveLabel). */
  labels: Record<TerminalId, string>;
  /** Full records of tabs torn off into their own satellite window (#21). They
   *  are removed from `tabs` (so the strip + canvas don't render them — exactly
   *  one window renders a given tab; two attached tmux clients would interleave)
   *  but kept here so their name/order/sizes survive and can be re-adopted when
   *  the satellite closes. Empty in the common single-window case. */
  poppedOutTabs: WorkspaceTab[];
}

/** Which UI region currently has keyboard focus for navigation (left-hand nav,
 *  feat/keyboard-nav). Ctrl+B toggles between them; Ctrl+Tab cycles WITHIN the
 *  focused region (terminals when "terminal", workspace tabs when "sidebar").
 *  Transient — never persisted; a relaunch always starts on the terminal area. */
export type FocusRegion = "terminal" | "sidebar";

interface WorkspaceState {
  /** Live terminal set, keyed by id (re-fetched from the backend, not persisted). */
  terminals: Record<TerminalId, TerminalInfo>;
  /** All workspace tabs, in strip order (persisted). */
  tabs: WorkspaceTab[];
  /** The active tab's id; only its tiles render (persisted). */
  activeTabId: string;
  /** Currently focused tile across the active tab, or null (persisted). */
  focusedId: TerminalId | null;
  /** Which region keyboard navigation targets (terminal area vs sidebar). NOT
   *  persisted — always starts on the terminal area. Ctrl+B toggles it; Ctrl+Tab
   *  cycles within it. See `setFocusRegion` / `toggleFocusRegion`. */
  focusedRegion: FocusRegion;
  /** Global terminal font size in px, applied to every tile equally (persisted). */
  fontSize: number;
  /** The EFFECTIVE per-terminal label map the display reads (#labels). It merges
   *  two sources, user rename winning:
   *    - an explicit user rename (`setTerminalLabel`, persisted), and
   *    - a Claude-derived title fed live from the hooks (`setClaudeTitle`, NOT
   *      persisted — see `claudeTitles`).
   *  A friendly display name is derived via `deriveLabel`, which treats this map
   *  as its highest-priority input. Empty until something names a terminal. */
  labels: Record<TerminalId, string>;
  /** Claude-suggested titles keyed by terminal id, fed live by the working hooks
   *  (`setClaudeTitle`). This is the raw Claude signal; `labels` is the effective
   *  merge (a user rename always overrides it). NOT persisted: it is re-derived
   *  from live hook events each session, so it must never masquerade as a saved
   *  user rename across reloads. */
  claudeTitles: Record<TerminalId, string>;
  /** Explicit user renames keyed by terminal id — the persisted source of truth
   *  behind the effective `labels` map. `setTerminalLabel` writes here; `labels`
   *  is recomputed as `{...claudeTitles, ...userLabels}` (rename wins). Loading a
   *  saved snapshot restores this (the persisted `labels` key holds renames). */
  userLabels: Record<TerminalId, string>;
  /** Full records of tabs popped out into their own window (#21), removed from
   *  `tabs` so they don't render here. The main window holds the popped-out tabs
   *  while a satellite renders each; resynced live across windows via windows.ts
   *  + persisted so a relaunch restores the split. Empty in the single-window
   *  case and (effectively) in a satellite, which only knows its own tab. */
  poppedOutTabs: WorkspaceTab[];
  /** Pointer-drag state (transient, never persisted). T-Hub's drag-and-drop is
   *  built on pointer events + `elementFromPoint` rather than HTML5 DnD, which is
   *  unreliable over xterm's WebGL canvas in WebView2. `draggingTileId` /
   *  `draggingTabId` is the active drag SOURCE (a tile being moved, or a tab being
   *  reordered); `dropTileId` / `dropTabId` is the element currently under the
   *  pointer, used purely to highlight the live drop target. */
  draggingTileId: TerminalId | null;
  draggingTabId: string | null;
  dropTileId: TerminalId | null;
  dropTabId: string | null;

  /** Replace the live set from a listTerminals() result, reconciling tabs/order/focus. */
  setTerminals: (list: TerminalInfo[]) => void;
  /** Refresh ONLY the live metadata (cwd/title/state) of already-known terminals
   *  from a fresh listTerminals(), without touching tab order, focus, or
   *  persisting. The Files tree roots at the focused terminal's cwd, but cwd is
   *  otherwise captured only at mount — so this keeps the tree (and tile labels)
   *  following a terminal as it `cd`s around. New/removed terminals are ignored
   *  here (they flow through setTerminals). */
  updateTerminalsMeta: (list: TerminalInfo[]) => void;
  /** Insert a freshly-spawned terminal after the focused tile in the active tab (else append) and focus it. */
  addAfterFocused: (info: TerminalInfo) => void;
  /** Insert an already-spawned tile into a SPECIFIC tab by id, activate that tab,
   *  and focus the tile. Deterministic sibling of addAfterFocused that never reads
   *  the active tab — used by the control/MCP path so a tile lands where the caller
   *  targeted (by name/id), not where UI focus happens to be (TASK C / #22). No-op
   *  if the tab id is unknown. */
  addToTab: (tabId: string, info: TerminalInfo) => void;
  /** Create a tab with a SPECIFIC id + name and activate it (the control/MCP
   *  `new_tab` path, where the CORE mints the id so it can return it to the caller).
   *  If a tab with this id already exists, just activate it. */
  adoptTab: (id: string, name: string) => void;
  /** Resolve a tab by id, else by exact name; if neither exists, create one with
   *  the given id + name and activate it. Returns the resolved tab id. Deterministic
   *  (never reads the active tab) — the named-placement primitive for create_worktree
   *  (TASK C / #22). */
  ensureTab: (id: string, name: string) => string;
  /** Adopt the SERVER's authoritative tab-registry snapshot (headless-org): the
   *  tab set, tab names, and tile membership come from the registry; activeTabId,
   *  focus, and per-tab sizes stay LOCAL (kept valid, never stolen - a headless
   *  placement/move/close must not switch the user's view). Tiles that vanish
   *  from every rendered tab (and are not popped out) were closed headlessly:
   *  their live entries + side state are dropped. Deep-equal snapshots are a
   *  no-op so apply echoes don't churn persistence or the tab reporter. */
  adoptRegistry: (tabs: TabReport[]) => void;
  /** Register a SERVER-spawned terminal in the live map without placing or
   *  focusing anything (placement arrives via the registry snapshot; metadata is
   *  refreshed by the ~5s poll). No-op if the id is already known. */
  adoptTerminal: (info: TerminalInfo) => void;
  /** Spawn a NEW terminal (the control/MCP `spawn_terminal` path) via the same
   *  spawnTerminal IPC the "+" menu uses, then place the tile in `opts.tabId` if
   *  given (else the tab active at call time — captured synchronously so the async
   *  spawn can't misplace it). Best-effort: a spawn failure is logged, not thrown.
   *  Returns the new terminal id, or null. */
  spawnWorkspaceTerminal: (opts?: {
    cwd?: string;
    name?: string;
    shell?: string;
    /** Optional command run inside the new pane's login shell (the "+" presets'
     *  field; T-B: forwarded from the socket `spawn_terminal` so a control-side
     *  resume — `claude --resume <id>` — completes through this path too). */
    startupCommand?: string;
    tabId?: string;
  }) => Promise<TerminalId | null>;
  /** Drop a terminal from every tab + the map, moving focus to a neighbor. */
  remove: (id: TerminalId) => void;
  /** Lifecycle: DETACH a tile — remove it from the layout but KEEP the tmux
   *  session alive (so it can be re-adopted later). Calls the backend
   *  `close_terminal` (detach the PTY client, tmux survives) then drops the tile
   *  via `remove`. The default "X" / Ctrl-W action. */
  detachTile: (id: TerminalId) => void;
  /** Lifecycle: DELETE a terminal — KILL its tmux session for good (backend
   *  `kill_terminal`, terminating the process tree) then drop the tile via
   *  `remove`. Destructive; callers gate this behind a confirm. */
  deleteTerminal: (id: TerminalId) => void;
  /** Lifecycle: KILL + RESTART — recover a frozen session. Spawns a FRESH tmux
   *  session rooted at the same cwd, drops it into the SAME tab at the SAME slot
   *  the old tile held, then kills the old session (process tree). Reuses the
   *  spawn + kill IPCs (no new tmux logic). Destructive; callers gate it behind a
   *  confirm. Returns the new terminal id, or null on spawn failure. */
  restartTerminal: (id: TerminalId) => Promise<TerminalId | null>;
  /** Set the focused tile. Focusing a tile also returns navigation focus to the
   *  terminal region (a click/keypress on a terminal implies you're working in
   *  the canvas, not the sidebar). */
  setFocus: (id: TerminalId) => void;
  /** Set which region keyboard navigation targets (terminal area vs sidebar). */
  setFocusRegion: (region: FocusRegion) => void;
  /** Toggle navigation focus between the terminal area and the sidebar (Ctrl+B).
   *  Returns the region now focused so the caller can reveal/blur the right
   *  surface (App reveals a hidden sidebar; Canvas refocuses the live xterm). */
  toggleFocusRegion: () => FocusRegion;
  /** Update a terminal's lifecycle state from a terminal://state event. */
  updateState: (id: TerminalId, state: TerminalState) => void;
  /** Set (or, with a blank value, clear) the user label for a terminal (#labels).
   *  A blank/whitespace value removes the override so the derived label takes over
   *  again; the trimmed value is stored otherwise. Persisted. */
  setTerminalLabel: (id: TerminalId, label: string) => void;
  /** Feed a Claude-suggested title for a terminal (from the working lifecycle
   *  hooks). Stored in `claudeTitles` and merged into the effective `labels` map
   *  ONLY when the user has not explicitly renamed the terminal — an explicit
   *  rename always wins. Blank/whitespace clears the Claude title. NOT persisted. */
  setClaudeTitle: (id: TerminalId, title: string) => void;

  // --- Recall (feat/projects-sidebar, Agent A) ---
  /** Recall a past Claude session into the ACTIVE workspace tab: spawn a NEW
   *  terminal rooted at `cwd` running `claude --resume <sessionId>` (resuming the
   *  conversation in place), insert the tile after the focused one, and focus it.
   *  Reuses the existing spawn path (the same `spawnTerminal` IPC + `addAfterFocused`
   *  the "+" menu / Canvas use) — recall is just a spawn with a cwd + a resume
   *  startup command. Best-effort: a spawn failure is logged, not thrown, so a
   *  click can never crash the sidebar. Returns the new terminal id, or null on
   *  failure.
   *
   *  Whether the resume command actually runs is normally the passive global
   *  `resumeStartsClaude` setting (default on) — the sidebar's Recent recall honors
   *  it. But an EXPLICIT "resume THIS session" action (e.g. Recovery's Restore)
   *  must always resume regardless: pass `opts.forceResume` to ALWAYS issue
   *  `claude --resume <id>`, ignoring the setting. */
  recall: (
    sessionId: string,
    cwd: string,
    opts?: { forceResume?: boolean },
  ) => Promise<TerminalId | null>;

  // --- Git worktrees (WS-4) ---
  /** Atomically: create a git worktree at `worktreePath` (via `gitWorktreeAdd`,
   *  unless `opts.alreadyCreated` says it already exists on disk), open a NEW
   *  workspace tab, spawn a terminal in the worktree dir, place it in that tab, and
   *  focus it. The new tab is named after `branch` / the path's final component, or
   *  `opts.tabName` when given. Reuses the existing spawn path (`spawnTerminal` IPC
   *  + a fresh tab) so a worktree tile is created exactly like any other tile.
   *  Returns the new terminal id, or null on failure (a `gitWorktreeAdd` failure
   *  is propagated so a UI caller can surface git's message; the MCP path passes
   *  `alreadyCreated` so git has already run). */
  addWorktreeWorkspace: (
    repoRoot: string,
    worktreePath: string,
    branch?: string,
    opts?: {
      tabName?: string;
      alreadyCreated?: boolean;
      /** Deterministic placement (TASK C / #22): the control/MCP path passes a tab
       *  id resolved CORE-side by name, so the tile lands in THAT tab (reused or
       *  created by id+name) rather than a fresh tab / the focused one. Absent for
       *  the UI (FilePanel) path, which creates a fresh tab as before. */
      tabId?: string;
    },
  ) => Promise<TerminalId | null>;
  /** Remove a git worktree SAFELY: first DETACH every live tile whose cwd is the
   *  worktree dir (or inside it) — their tmux sessions survive a detach, so no
   *  process is orphaned — then call `gitWorktreeRemove`. Detaching before git
   *  tears the dir down is the whole point; a forced removal with live, unsaved
   *  work is still gated on `force`. Resolves when git has removed the worktree;
   *  rejects with git's message on failure (the tiles are already detached). */
  removeWorktreeWorkspace: (
    repoRoot: string,
    worktreePath: string,
    force?: boolean,
  ) => Promise<void>;

  // --- Tabs (PRD §5.2) ---
  /** Create a new empty tab (auto-named) and activate it; returns its id. */
  addTab: () => string;
  /** Rename a tab (no-op on blank/unknown id). */
  renameTab: (id: string, name: string) => void;
  /** Close a tab and drop its tiles from this window's layout; refuses only if
   *  it is the last tab. An EMPTY tab closes outright; a NON-EMPTY tab is closed
   *  too (the caller is responsible for confirming + detaching its terminals via
   *  closeTerminal first — tmux survives, the sessions are not killed). The
   *  removed tile ids are returned so the caller can detach them. */
  closeTab: (id: string) => TerminalId[];
  /** Tier 3 reap — the workspace × (close/delete). KILLS every session in the tab
   *  (SIGKILL the process tree, so the orphan leak stops), then removes the tab via
   *  closeTab. Recall stays available via Recent (the on-disk transcript survives
   *  the kill), and the just-closed projects are forced to appear in Recent
   *  immediately. PRESERVES sessions on switch (setActiveTab) and pop-out
   *  (popOutTab) — those never call this. No-op on the last tab (mirrors closeTab). */
  closeWorkspace: (id: string) => void;
  /** Activate a tab (moves focus onto one of its tiles). */
  setActiveTab: (id: string) => void;
  /** Activate the tab at strip index `i` (0-based); no-op if out of range. */
  setActiveTabByIndex: (i: number) => void;
  /** Cycle to the next (+1) / previous (-1) tab, wrapping. */
  cycleTab: (dir: 1 | -1) => void;
  /** Cycle the FOCUSED TILE within the active tab (+1 next / -1 previous,
   *  wrapping). No-op when the active tab has fewer than two tiles. */
  cycleTile: (dir: 1 | -1) => void;
  /** Cycle the focused tile across EVERY workspace tab (+1 next / -1 previous,
   *  wrapping over the flattened tile order of all tabs in strip order). Used by
   *  Ctrl+Tab while the terminal region is focused so any terminal in any
   *  workspace is reachable. Crosses a tab boundary by switching the active tab to
   *  the one that owns the next terminal, then focusing it (which also snaps the
   *  nav focus back to the terminal region). No-op when there are fewer than two
   *  tiles total. */
  cycleTileGlobal: (dir: 1 | -1) => void;
  /** Reorder the tab strip: move tab `id` to occupy `targetId`'s slot. */
  moveTab: (id: string, targetId: string) => void;

  // --- Multi-window tear-off (#21) ---
  /** Pop a tab out into its own window: move its record from `tabs` into
   *  `poppedOutTabs`, so this (main) window stops rendering it. Idempotent.
   *  Re-points activeTabId to a still-visible tab if the popped one was active.
   *  Callers (windows.ts) also spawn the satellite + broadcast the resync. */
  popOutTab: (id: string) => void;
  /** Re-adopt a popped-out tab back into `tabs` (e.g. when its satellite closes).
   *  Restores the provided record (the satellite's latest order/name), or the
   *  stashed one if `tab` is omitted. Idempotent; no-op for an unknown id. */
  popInTab: (id: string, tab?: WorkspaceTab) => void;

  // --- Manual layout (PRD §5.3) ---
  /** Reorder tiles within the active tab: pull `id` out and re-insert it at
   *  `targetId`'s position, so a tile can be dropped onto ANY other tile
   *  (including a diagonal grid neighbor), not just an adjacent one. */
  moveTile: (id: TerminalId, targetId: TerminalId) => void;
  /** Mark a tile as the active drag source (or null to clear at drag end). */
  setDraggingTile: (id: TerminalId | null) => void;
  /** Move a tile to a DIFFERENT tab (drag-a-tile-onto-a-tab): pull it from its
   *  current tab and append it to `tabId`. The terminal/agent stay attached and
   *  alive; the active tab and (where possible) focus are left untouched. */
  moveTileToTab: (id: TerminalId, tabId: string) => void;
  /** Ensure the reserved Captains tab exists; returns its id (CAPTAINS_TAB_ID). */
  ensureCaptainsTab: () => string;
  /** Place a tile in the reserved Captains tab - designating it as an agent
   *  (orchestrator / captain). Creates the tab if needed, then pulls the tile
   *  from whatever tab it's in (or appends it if unplaced). Never steals the
   *  active tab; hands focus to a neighbor if the moved tile was the active tab's
   *  focused tile. No-op if the tile is already in the Captains tab. */
  moveTileToCaptainsTab: (id: TerminalId) => void;
  /** Return a tile from the Captains tab to a normal work tab - un-designating an
   *  agent. Moves it to the first non-reserved tab (creating one if none exists).
   *  No-op if the tile is not currently in the Captains tab. */
  moveTileToWorkTab: (id: TerminalId) => void;
  /** Mark a tab as the active drag source (reorder), or null to clear. */
  setDraggingTab: (id: string | null) => void;
  /** Set the tile currently under the drag pointer (highlight only), or null. */
  setDropTile: (id: TerminalId | null) => void;
  /** Set the tab currently under the drag pointer (highlight only), or null. */
  setDropTab: (id: string | null) => void;
  /** Persist manual size ratios for a tab. */
  setTabSizes: (id: string, sizes: TabSizes) => void;

  // --- Global zoom ---
  zoomIn: () => void;
  zoomOut: () => void;
  zoomReset: () => void;
}

function clampFont(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_FONT_SIZE;
  return Math.max(MIN_FONT_SIZE, Math.min(MAX_FONT_SIZE, Math.round(n)));
}

let tabSeq = 0;
/** Monotonic-ish tab id (timestamp + counter so rapid creates stay unique). */
function newTabId(): string {
  tabSeq += 1;
  return `tab-${Date.now().toString(36)}-${tabSeq.toString(36)}`;
}

/**
 * Migrate a pre-#16 terminal id to the id the backend now uses. Before #16,
 * spawn minted a full 36-char UUID while tmux/list keyed off its first 8 chars;
 * #16 made spawn use that same 8-char id. A layout persisted before #16 holds
 * full-UUID ids that no longer match live sessions, so shorten them to the
 * 8-char form -- otherwise a saved arrangement stops matching and every tile
 * gets dumped into the active tab on the first load after the fix.
 */
function shortenId(id: string): string {
  return id.length > 8 && id.includes("-") ? id.slice(0, 8) : id;
}

/**
 * Inputs `deriveLabel` reads to build a friendly terminal name. This is a thin
 * shape over what the store already knows about a session (`TerminalInfo` plus
 * the optional effective label) — the single extension point for richer signals.
 *
 * The richer signal is now wired: a Claude-suggested title arrives live from the
 * lifecycle hooks (`setClaudeTitle`) and is merged into the effective `labels`
 * map, which the display passes here as `label`. So `label` carries, in order of
 * preference: an explicit user rename, else the latest Claude-suggested title.
 */
export interface LabelSource {
  /** The 8-char tmux session id (the raw value we're replacing in the UI). */
  id: TerminalId;
  /** The effective label (highest priority): an explicit user rename if present,
   *  otherwise the live Claude-suggested title fed from the hooks. */
  label?: string;
  /** Backend `TerminalInfo.title`: the spawn preset/command/name at spawn, but on
   *  a reload it degrades to the tmux session name (`th_<id>`) or the generic
   *  "terminal"/id — so it is only used when it carries real signal (see below). */
  title?: string;
  /** Backend working directory; its basename is the cwd part of a derived label. */
  cwd?: string;
}

/** Final path segment of a (possibly trailing-slashed) cwd, or "" if none. POSIX
 *  and Windows separators both split so a WSL or native path yields a basename. */
function cwdBasename(cwd: string | undefined): string {
  if (!cwd) return "";
  const parts = cwd.replace(/[/\\]+$/, "").split(/[/\\]+/);
  const last = parts[parts.length - 1] ?? "";
  return last === "~" ? "" : last;
}

/**
 * The "command/preset" part of a derived label, drawn from the backend title.
 * Returns "" when the title carries no real signal — i.e. when it is empty, the
 * raw id, the tmux session name (`th_<id>`, which `list_terminals` uses as the
 * title on reload), or the generic spawn fallback "terminal". Otherwise the title
 * IS a meaningful preset/command/name (e.g. `claude`, `zsh`) and is used as-is.
 */
function commandPart(id: TerminalId, title: string | undefined): string {
  const t = (title ?? "").trim();
  if (!t) return "";
  if (t === id || t === `th_${id}` || t.toLowerCase() === "terminal") return "";
  return t;
}

/**
 * Derive a human-friendly terminal label from what the store knows, in priority
 * order (PRD #labels):
 *   1. an explicit user label (a rename), used verbatim;
 *   2. a label derived from the spawn preset/command and/or the cwd basename,
 *      e.g. `claude · tools`, `zsh · n8builds`, or just one part if only one is
 *      known;
 *   3. the short 8-char id as a last resort.
 * Pure + exported so the display sites (Tile/Titlebar/Sidebar) and any test share
 * one definition; the short id is always available separately for the dimmed
 * secondary detail, so callers render `deriveLabel(src)` prominently with `src.id`
 * faint next to it.
 */
export function deriveLabel(src: LabelSource): string {
  const user = (src.label ?? "").trim();
  if (user) return user;
  const cmd = commandPart(src.id, src.title);
  const dir = cwdBasename(src.cwd);
  if (cmd && dir) return `${cmd} · ${dir}`;
  return cmd || dir || src.id;
}

/** Sanitize a parsed labels map: keep only string→non-empty-string pairs. */
function cleanLabels(value: unknown): Record<TerminalId, string> {
  if (!value || typeof value !== "object") return {};
  const out: Record<TerminalId, string> = {};
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    if (typeof v === "string" && v.trim()) out[shortenId(k)] = v.trim();
  }
  return out;
}

/** Compute the effective display label map from its two sources: Claude-suggested
 *  titles overlaid by explicit user renames (a rename always wins). This is what
 *  the display reads as `labels`. */
function mergeLabels(
  userLabels: Record<TerminalId, string>,
  claudeTitles: Record<TerminalId, string>,
): Record<TerminalId, string> {
  return { ...claudeTitles, ...userLabels };
}

/** Sanitize an arbitrary parsed value into a clean order array of string ids. */
function cleanOrder(value: unknown): TerminalId[] {
  return Array.isArray(value)
    ? value
        .filter((id): id is TerminalId => typeof id === "string")
        .map(shortenId)
    : [];
}

/** Sanitize parsed TabSizes; drops anything malformed (=> even split). */
function cleanSizes(value: unknown): TabSizes | undefined {
  if (!value || typeof value !== "object") return undefined;
  const v = value as { rows?: unknown; cols?: unknown };
  const rows = Array.isArray(v.rows)
    ? v.rows.filter((n): n is number => typeof n === "number" && n > 0)
    : [];
  const cols = Array.isArray(v.cols)
    ? v.cols.map((row) =>
        Array.isArray(row)
          ? row.filter((n): n is number => typeof n === "number" && n > 0)
          : [],
      )
    : [];
  if (rows.length === 0 && cols.length === 0) return undefined;
  return { rows, cols };
}

/** Sanitize one parsed tab record (id/name/order/sizes) into a clean WorkspaceTab. */
function cleanTab(t: Partial<WorkspaceTab>): WorkspaceTab {
  return {
    id: typeof t.id === "string" && t.id ? t.id : newTabId(),
    name: typeof t.name === "string" && t.name ? t.name : "Workspace",
    order: cleanOrder(t.order),
    sizes: cleanSizes(t.sizes),
  };
}

/** Sanitize a parsed array of tab records (drops non-objects). */
function cleanTabs(value: unknown): WorkspaceTab[] {
  return Array.isArray(value)
    ? value
        .filter((t): t is Partial<WorkspaceTab> => !!t && typeof t === "object")
        .map(cleanTab)
    : [];
}

/** Build the default single-tab layout (empty canvas). */
function defaultLayout(): PersistedLayout {
  return {
    tabs: [{ id: newTabId(), name: DEFAULT_TAB_NAME, order: [] }],
    activeTabId: "",
    focusedId: null,
    fontSize: DEFAULT_FONT_SIZE,
    labels: {},
    poppedOutTabs: [],
  };
}

/**
 * Sanitize/repair a parsed layout into a valid `PersistedLayout` (>=1 tab, a
 * valid activeTabId, an in-range focusedId, a clamped fontSize, and no popped
 * tab id that collides with a visible one). Shared by the localStorage and the
 * SQLite (#sqlite) load paths so both apply identical invariants.
 */
function finalizeLayout(layout: PersistedLayout): PersistedLayout {
  // A popped-out tab id must never also appear in `tabs` (it would render in
  // two places). Drop any popped record whose id collides with a visible tab.
  const visibleIds = new Set(layout.tabs.map((t) => t.id));
  layout.poppedOutTabs = (layout.poppedOutTabs ?? []).filter(
    (t) => !visibleIds.has(t.id),
  );
  // Keep >=1 tab. If EVERY tab is currently popped out (all windows are
  // satellites of the same set), re-adopt the first popped one so the main
  // window still has a canvas; its satellite's resync will hide it again.
  if (layout.tabs.length === 0) {
    if (layout.poppedOutTabs.length > 0) {
      layout.tabs = [layout.poppedOutTabs.shift()!];
    } else {
      layout.tabs = [{ id: newTabId(), name: DEFAULT_TAB_NAME, order: [] }];
    }
  }
  // The reserved Captains tab is always present (appended last so it never
  // becomes the default-active tab, which is the first work tab).
  layout.tabs = ensureReservedCaptainsTab(layout.tabs);
  if (!layout.tabs.some((t) => t.id === layout.activeTabId)) {
    layout.activeTabId = layout.tabs[0].id;
  }
  const active = layout.tabs.find((t) => t.id === layout.activeTabId)!;
  if (!layout.focusedId || !active.order.includes(layout.focusedId)) {
    layout.focusedId = active.order[0] ?? null;
  }
  layout.fontSize = clampFont(layout.fontSize);
  return layout;
}

/**
 * Parse + sanitize a raw v2-snapshot JSON string into a finalized layout, or
 * `null` if it is missing/unparseable. Used by both the localStorage v2 branch
 * and the durable SQLite (#sqlite) load path.
 */
function parseV2Snapshot(raw: string | null | undefined): PersistedLayout | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedLayout>;
    return finalizeLayout({
      tabs: cleanTabs(parsed.tabs),
      activeTabId:
        typeof parsed.activeTabId === "string" ? parsed.activeTabId : "",
      focusedId:
        typeof parsed.focusedId === "string"
          ? shortenId(parsed.focusedId)
          : null,
      fontSize:
        typeof parsed.fontSize === "number"
          ? parsed.fontSize
          : DEFAULT_FONT_SIZE,
      labels: cleanLabels(parsed.labels),
      poppedOutTabs: cleanTabs(parsed.poppedOutTabs),
    });
  } catch {
    return null;
  }
}

/**
 * Read the persisted layout. Prefers the v2 (tabbed) snapshot; if absent, a
 * legacy v1 (flat order/focus) snapshot is migrated into a single tab so an
 * upgrading user keeps their terminals. Always returns >=1 tab and a valid
 * activeTabId.
 */
function loadPersisted(): PersistedLayout {
  if (typeof localStorage === "undefined") {
    const d = defaultLayout();
    d.activeTabId = d.tabs[0].id;
    return d;
  }

  const finalize = finalizeLayout;

  // Preferred: v2 tabbed snapshot.
  const v2 = parseV2Snapshot(localStorage.getItem(PERSIST_KEY));
  if (v2) return v2;

  // Migration: legacy v1 flat snapshot -> a single tab.
  try {
    const raw = localStorage.getItem(LEGACY_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as {
        order?: unknown;
        focusedId?: unknown;
        fontSize?: unknown;
      };
      const order = cleanOrder(parsed.order);
      const tab: WorkspaceTab = {
        id: newTabId(),
        name: DEFAULT_TAB_NAME,
        order,
      };
      return finalize({
        tabs: [tab],
        activeTabId: tab.id,
        focusedId:
          typeof parsed.focusedId === "string"
            ? shortenId(parsed.focusedId)
            : null,
        fontSize:
          typeof parsed.fontSize === "number"
            ? parsed.fontSize
            : DEFAULT_FONT_SIZE,
        labels: {},
        poppedOutTabs: [],
      });
    }
  } catch {
    /* fall through to default */
  }

  return finalize(defaultLayout());
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

/**
 * The tab id this window was opened to render in isolation (the `?tab=<id>` URL
 * param), or null for the main window (#21). Read directly here — rather than
 * importing src/lib/windows.ts — to avoid an import cycle (windows.ts imports
 * this store). A SATELLITE window:
 *   - keeps ONLY its own tab in `tabs`, so the shared Canvas renders just that
 *     one canvas and only its terminals attach (the main window renders the
 *     rest; two tmux clients on one session would interleave); and
 *   - does NOT persist, so its pruned 1-tab view never clobbers the shared
 *     localStorage snapshot the MAIN window owns.
 */
function satelliteTabId(): string | null {
  if (typeof location === "undefined") return null;
  try {
    const id = new URLSearchParams(location.search).get("tab");
    return id && id.trim() ? id : null;
  } catch {
    return null;
  }
}

/**
 * Narrow a freshly-loaded layout to a single tab for a satellite window. If the
 * tab isn't in the snapshot yet (persistence lagged the spawn), synthesize an
 * empty one so the satellite still has a valid canvas to attach terminals into;
 * setTerminals() will reconcile the real tile order from the backend.
 */
function scopeToSatellite(layout: PersistedLayout, tabId: string): PersistedLayout {
  // The tab may live in `tabs` or (if the main window already popped it out and
  // persisted before we booted) in `poppedOutTabs`; check both. Fall back to an
  // empty tab so the satellite still has a canvas (setTerminals reconciles tiles).
  const own =
    layout.tabs.find((t) => t.id === tabId) ??
    layout.poppedOutTabs.find((t) => t.id === tabId) ??
    ({ id: tabId, name: "Workspace", order: [] } as WorkspaceTab);
  return {
    tabs: [own],
    activeTabId: own.id,
    focusedId: own.order[0] ?? null,
    fontSize: layout.fontSize,
    // Carry the full label map: it's tiny metadata and the satellite only renders
    // its own terminals, so the extra entries are inert but keep labels consistent.
    labels: layout.labels,
    poppedOutTabs: [], // a satellite tracks only its own (visible) tab
  };
}

/**
 * On a fresh MAIN-window launch, satellites from a previous run no longer exist
 * (they are runtime-created by pop-out and never respawned at boot), so any tab
 * left in `poppedOutTabs` is orphaned -- it would render in no window at all.
 * Re-adopt every popped tab back into `tabs` so its terminals stay reachable.
 * Net effect: pop-out is a within-session split; a restart/redeploy returns every
 * popped tab to the main window (#21 phase 1). No-op when nothing is popped.
 */
function adoptOrphans(layout: PersistedLayout): PersistedLayout {
  if (layout.poppedOutTabs.length === 0) return layout;
  return {
    ...layout,
    tabs: [...layout.tabs, ...layout.poppedOutTabs],
    poppedOutTabs: [],
  };
}

/**
 * Pick a sensible focus target after `removedId` leaves `prevOrder`.
 * Prefers the next tile, then the previous, then null. `nextOrder` is the
 * order with `removedId` already removed.
 */
function neighborFocus(
  prevOrder: TerminalId[],
  nextOrder: TerminalId[],
  removedId: TerminalId,
  currentFocus: TerminalId | null,
): TerminalId | null {
  if (currentFocus !== removedId) {
    return currentFocus && nextOrder.includes(currentFocus)
      ? currentFocus
      : nextOrder[0] ?? null;
  }
  if (nextOrder.length === 0) return null;
  const idx = prevOrder.indexOf(removedId);
  return nextOrder[idx] ?? nextOrder[idx - 1] ?? nextOrder[0] ?? null;
}

/** The tab whose `order` contains `id`, or undefined. */
function tabOf(tabs: WorkspaceTab[], id: TerminalId): WorkspaceTab | undefined {
  return tabs.find((t) => t.order.includes(id));
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
    const isWork = (t: WorkspaceTab): boolean => t.id !== CAPTAINS_TAB_ID;
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

    setTerminals: (list) => {
      const terminals: Record<TerminalId, TerminalInfo> = {};
      for (const t of list) terminals[t.id] = t;
      const liveIds = new Set(list.map((t) => t.id));

      const { tabs, activeTabId, poppedOutTabs } = get();
      // Keep each tab's ordering for ids that still exist; prune dead ids.
      const placed = new Set<TerminalId>();
      const nextTabs = tabs.map((t) => {
        const order = t.order.filter((id) => liveIds.has(id));
        for (const id of order) placed.add(id);
        return { ...t, order };
      });

      // Popped-out tabs live in other windows but their terminals are still in
      // the backend's list. Prune their orders to live ids and count them as
      // PLACED so they aren't yanked back onto this window's active tab below.
      const nextPopped = poppedOutTabs.map((t) => {
        const order = t.order.filter((id) => liveIds.has(id));
        for (const id of order) placed.add(id);
        return { ...t, order };
      });

      // Any live terminal not already placed in some tab is appended to the
      // active tab (covers first load with pre-existing sessions, or sessions
      // spawned out-of-band by another surface). NOT in a satellite window: its
      // unplaced terminals belong to the OTHER windows' tabs, so adopting them
      // would drag every session into the satellite. A satellite shows exactly
      // the tiles its own tab record lists.
      const appended = SATELLITE_TAB
        ? []
        : list.map((t) => t.id).filter((id) => !placed.has(id));
      if (appended.length > 0) {
        const activeIdx = nextTabs.findIndex((t) => t.id === activeTabId);
        const idx = activeIdx >= 0 ? activeIdx : 0;
        nextTabs[idx] = {
          ...nextTabs[idx],
          order: [...nextTabs[idx].order, ...appended],
        };
      }

      // SATELLITE blank-boot (#4): DEFERRED — needs scoped recovery, not the
      // unscoped list. An earlier attempt repopulated an empty satellite tab from
      // `list.map(t => t.id)`, but `list` (listTerminals) is EVERY window's
      // sessions, so the satellite would adopt the MAIN window's terminals and a
      // second tmux client would attach to each → interleaved/garbled output (the
      // exact case the `appended` block above avoids for satellites). We can't
      // scope by id once the tab's own ids pruned away (they're gone), so a correct
      // fix needs per-terminal owning-tab metadata from the backend (or a
      // persist-before-pop-out guarantee). Until then a satellite that pruned to
      // empty stays empty (pre-v0.3.20 behavior) rather than dual-attaching.

      const active = nextTabs.find((t) => t.id === activeTabId) ?? nextTabs[0];
      const focusedId =
        get().focusedId && active.order.includes(get().focusedId as TerminalId)
          ? get().focusedId
          : active.order[0] ?? null;

      set({ terminals, tabs: nextTabs, poppedOutTabs: nextPopped, focusedId });
      persist();
    },

    updateTerminalsMeta: (list) => {
      const { terminals } = get();
      let changed = false;
      const next: Record<TerminalId, TerminalInfo> = { ...terminals };
      for (const t of list) {
        const ex = next[t.id];
        if (!ex) continue; // unknown id: new terminals arrive via setTerminals
        // Overwrite cwd with the backend's value (which `list_terminals` fills
        // from the pane's LIVE `#{pane_current_path}`), so `terminals[id].cwd`
        // tracks the CURRENT pane directory — refreshed on the ~5s poll — not
        // just the spawn dir. We keep the single `cwd` field (no separate
        // spawn/live field): the spawn value seeds it and is then replaced live,
        // so existing `cwd` consumers (Files tree root, worktree anchor) read the
        // live path with no rename. Title/state ride along on the same diff.
        if (ex.cwd !== t.cwd || ex.title !== t.title || ex.state !== t.state) {
          next[t.id] = { ...ex, cwd: t.cwd, title: t.title, state: t.state };
          changed = true;
        }
      }
      // No order/focus change and NOT persisted (live metadata only): avoids
      // thrashing the layout snapshot on every poll.
      if (changed) set({ terminals: next });
    },

    addAfterFocused: (info) => {
      const active = activeTab();
      // A plain work spawn must never land in the reserved Captains tab (only
      // agent tiles belong there, via moveTileToCaptainsTab): if the active tab
      // is Captains, redirect the tile into a work tab instead.
      if (active.id === CAPTAINS_TAB_ID) {
        placeWorkTile(info);
        return;
      }
      const { tabs, focusedId, terminals } = get();
      const nextOrder = active.order.slice();
      const focusIdx = focusedId ? nextOrder.indexOf(focusedId) : -1;
      if (focusIdx >= 0) nextOrder.splice(focusIdx + 1, 0, info.id);
      else nextOrder.push(info.id);

      const nextTabs = tabs.map((t) =>
        t.id === active.id ? { ...t, order: nextOrder } : t,
      );

      set({
        terminals: { ...terminals, [info.id]: info },
        tabs: nextTabs,
        focusedId: info.id,
      });
      persist();
    },

    addToTab: (tabId, info) => {
      // A work tile targeting the reserved Captains tab (e.g. a "+" spawn while
      // Captains is the active tab, which spawnWorkspaceTerminal forwards as the
      // target) is redirected into a work tab - only agent tiles belong in
      // Captains (via moveTileToCaptainsTab).
      if (tabId === CAPTAINS_TAB_ID) {
        placeWorkTile(info);
        return;
      }
      const { tabs, terminals } = get();
      if (!tabs.some((t) => t.id === tabId)) return; // unknown tab: no-op
      const nextTabs = tabs.map((t) =>
        t.id === tabId ? { ...t, order: [...t.order, info.id] } : t,
      );
      set({
        terminals: { ...terminals, [info.id]: info },
        tabs: nextTabs,
        activeTabId: tabId,
        focusedId: info.id,
      });
      persist();
    },

    adoptTab: (id, name) => {
      const { tabs } = get();
      if (tabs.some((t) => t.id === id)) {
        set({ activeTabId: id });
        return;
      }
      const tab: WorkspaceTab = { id, name: name.trim() || "Workspace", order: [] };
      set({ tabs: [...tabs, tab], activeTabId: id, focusedId: null });
      persist();
    },

    ensureTab: (id, name) => {
      const { tabs } = get();
      const byId = tabs.find((t) => t.id === id);
      if (byId) return byId.id;
      const byName = tabs.find((t) => t.name === name);
      if (byName) return byName.id;
      const tab: WorkspaceTab = { id, name: name.trim() || "Workspace", order: [] };
      set({ tabs: [...tabs, tab], activeTabId: id, focusedId: null });
      persist();
      return id;
    },

    adoptRegistry: (regTabs) => {
      // Defensive: the server never sends an empty snapshot (close_tab refuses
      // the last tab); an empty one would zero the canvas, so ignore it.
      if (regTabs.length === 0) return;
      const { tabs, activeTabId, focusedId, terminals, poppedOutTabs } = get();

      const byId = new Map(tabs.map((t) => [t.id, t]));
      // The reserved Captains tab is CLIENT-ONLY (the backend registry doesn't
      // track it), and its `order` is the authoritative list of tiles placed as
      // agents. Keep an agent tile that is EITHER still reported live by the
      // server (serverTileIds - which includes the reserved tab the reporter
      // up-syncs and the server echoes back) OR still present in the authoritative
      // CAPTAINS REGISTRY (an externally claimed captain - e.g. one the
      // orchestrator claimed over the control socket - whose tile the server does
      // not yet echo as a live work-tab tile). A tile in NEITHER was genuinely
      // closed (server-closed AND released from the registry, which sync_captains
      // keeps in step) and drops out of Captains, cleaned up below like any gone
      // tile. The surviving ids are then held out of the server-derived work tabs
      // so an agent tile never reappears in a work tab after a sync.
      const serverTileIds = new Set(regTabs.flatMap((r) => r.tileIds));
      const registeredCaptains = new Set(captainRegistryIds());
      const localCaptains = tabs.find((t) => t.id === CAPTAINS_TAB_ID);
      const captainsOrder = (localCaptains?.order ?? []).filter(
        (id) => serverTileIds.has(id) || registeredCaptains.has(id),
      );
      // ADOPT an agent the SERVER placed DIRECTLY into the reserved Captains tab
      // that the client never tracked locally - a captain commissioned over the
      // control socket (spawn_terminal with tabId=captains-reserved), whose tile
      // lands in the server's reserved-tab snapshot but is in NEITHER the local
      // captains order (the client never pinned it) NOR any work tab. Without this
      // it is filtered out of every rebuilt tab, so the agents plane renders no
      // tile and never attaches a PTY client to it, and the cleanup pass below
      // then garbage-collects its live entry. The KEEP filter above only prunes
      // the existing local order - it can never ADD such a tile - so append it
      // here at the tail (least-recently-summoned, like a fresh local pin),
      // preserving the established order. Idempotent across the reporter round-
      // trip: once adopted it is already in the local order on the next sync.
      const serverCaptainsTiles =
        regTabs.find((r) => r.id === CAPTAINS_TAB_ID)?.tileIds ?? [];
      for (const id of serverCaptainsTiles) {
        if (!captainsOrder.includes(id)) captainsOrder.push(id);
      }
      const agentSet = new Set(captainsOrder);

      // The reserved Captains tab is CLIENT-ONLY, but the tab reporter up-syncs it
      // to the server like any other tab, so the server echoes it back in this
      // snapshot. Drop every incoming copy of it here (its agent tiles are held in
      // `captainsOrder` and re-appended authoritatively below) - otherwise the
      // echoed copy would render ALONGSIDE the re-appended one as a duplicate tab,
      // and since the echoed copy's tiles are all agent tiles filtered out by
      // `agentSet`, that duplicate has an empty `order` and shows the stray "new
      // terminal" placeholder even though the real Captains tab has terminals.
      const serverTabs: WorkspaceTab[] = regTabs
        .filter((r) => r.id !== CAPTAINS_TAB_ID)
        .map((r) => {
          const existing = byId.get(r.id);
          const order = r.tileIds.filter((id) => !agentSet.has(id));
          const sameOrder =
            existing !== undefined &&
            existing.order.length === order.length &&
            existing.order.every((x, i) => x === order[i]);
          return {
            id: r.id,
            name: r.name.trim() || existing?.name || "Workspace",
            order,
            // Manual grid ratios survive only if the tile set didn't change.
            sizes: sameOrder ? existing.sizes : undefined,
          };
        });
      // Re-append the reserved Captains tab (never dropped by a server sync).
      const nextTabs: WorkspaceTab[] = [
        ...serverTabs,
        {
          id: CAPTAINS_TAB_ID,
          name: CAPTAINS_TAB_NAME,
          order: captainsOrder,
          sizes:
            localCaptains &&
            localCaptains.order.length === captainsOrder.length &&
            localCaptains.order.every((x, i) => x === captainsOrder[i])
              ? localCaptains.sizes
              : undefined,
        },
      ];

      // Deep-equal snapshots are a no-op (apply echoes must not churn persist /
      // the tab reporter).
      const unchanged =
        nextTabs.length === tabs.length &&
        nextTabs.every((t, i) => {
          const o = tabs[i];
          return (
            t.id === o.id &&
            t.name === o.name &&
            t.order.length === o.order.length &&
            t.order.every((x, j) => x === o.order[j])
          );
        });
      if (unchanged) return;

      // Keep the user's view valid but NEVER steal it: activeTabId moves only if
      // its tab was closed; focus moves only if the focused tile left the active
      // tab.
      let nextActive = activeTabId;
      if (!nextTabs.some((t) => t.id === nextActive)) nextActive = nextTabs[0].id;
      const active = nextTabs.find((t) => t.id === nextActive)!;
      const nextFocus =
        focusedId && active.order.includes(focusedId)
          ? focusedId
          : active.order[0] ?? null;

      // Tiles gone from every rendered tab (and not popped out) were closed
      // headlessly - drop their live entries + side state like closeTab does.
      const after = new Set(nextTabs.flatMap((t) => t.order));
      const popped = new Set(poppedOutTabs.flatMap((t) => t.order));
      const nextTerminals = { ...terminals };
      for (const t of tabs) {
        for (const id of t.order) {
          if (!after.has(id) && !popped.has(id)) {
            delete nextTerminals[id];
            cleanupTileSideState(id);
          }
        }
      }

      set({
        tabs: nextTabs,
        activeTabId: nextActive,
        focusedId: nextFocus,
        terminals: nextTerminals,
      });
      persist();
    },

    adoptTerminal: (info) => {
      const { terminals } = get();
      if (terminals[info.id]) return;
      // Live map only: placement/persist ride the registry snapshot adopt.
      set({ terminals: { ...terminals, [info.id]: info } });
    },

    spawnWorkspaceTerminal: async (opts) => {
      // Capture the target tab id SYNCHRONOUSLY (before the async spawn) so a focus
      // change mid-spawn can't misplace the tile.
      const tabId = opts?.tabId ?? get().activeTabId;
      try {
        const { spawnTerminal } = await import("../ipc/client");
        const info = await spawnTerminal({
          cwd: opts?.cwd?.trim() || undefined,
          name: opts?.name?.trim() || undefined,
          shell: opts?.shell?.trim() || undefined,
          startupCommand: opts?.startupCommand?.trim() || undefined,
        });
        if (get().tabs.some((t) => t.id === tabId)) get().addToTab(tabId, info);
        else get().addAfterFocused(info);
        return info.id;
      } catch (err) {
        console.error("spawnWorkspaceTerminal failed", err);
        return null;
      }
    },

    // --- Recall (feat/projects-sidebar, Agent A) ------------------------------
    // Re-spawn + resume a past Claude session into the active tab. This is the
    // store-side spawn helper the sidebar's Recent list uses; it deliberately
    // reuses the SAME spawn path as Canvas's "+" menu (`spawnTerminal` IPC then
    // `addAfterFocused`) so there is exactly one way a tile gets created. We add
    // it here (rather than reaching into Canvas, which another agent owns) per the
    // build split. The dynamic `../ipc/client` import keeps the store free of a
    // hard Tauri dependency, matching detachTile/deleteTerminal/saveToBackend.
    recall: async (sessionId, cwd, opts) => {
      const id = sessionId.trim();
      const dir = cwd.trim();
      if (!id) return null;
      // Drop a second recall of the same session while the first is still in
      // flight (#7) — a double-click would otherwise stack duplicate spawns.
      if (recallInFlight.has(id)) return null;
      recallInFlight.add(id);
      try {
        const { spawnTerminal } = await import("../ipc/client");
        const { useSettings } = await import("./settings");
        // Spawn rooted at the session's cwd. Whether we actually launch Claude is
        // normally a SETTING (resumeStartsClaude, default on): on -> `claude --resume
        // <id>` resumes that conversation directly; off -> just a terminal in the dir
        // (no Claude). `forceResume` overrides that: an EXPLICIT "resume THIS session"
        // action (Recovery's Restore) must always resume, regardless of the passive
        // default. Quoting the id is a defensive guard (ids are plain UUIDs).
        const startClaude =
          opts?.forceResume || useSettings.getState().resumeStartsClaude;
        const info = await spawnTerminal({
          cwd: dir || undefined,
          startupCommand: startClaude ? `claude --resume '${id}'` : undefined,
        });
        // Insert after the focused tile in the active tab and focus it — exactly
        // how a "+" spawn lands. Persistence + reconcile come for free.
        get().addAfterFocused(info);
        return info.id;
      } catch (err) {
        console.error("recall failed", err);
        return null;
      } finally {
        // Release the in-flight guard once this spawn has settled (success or
        // failure), so a later, deliberate resume of the same session works.
        recallInFlight.delete(id);
      }
    },

    // --- Git worktrees (WS-4) -------------------------------------------------
    // Atomic create→tab→spawn. `gitWorktreeAdd` makes the worktree on disk (unless
    // it already exists — the MCP path creates it backend-side and passes
    // `alreadyCreated`), then we open a NEW tab and spawn a terminal in the
    // worktree dir, placing it in that tab. The same `spawnTerminal` IPC the "+"
    // menu / recall use creates the tile, so a worktree tile is just a tile. A
    // `gitWorktreeAdd` failure is PROPAGATED (so a UI caller can show git's message
    // — e.g. "branch already checked out elsewhere"); a spawn failure is logged and
    // returns null after the worktree already exists.
    addWorktreeWorkspace: async (repoRoot, worktreePath, branch, opts) => {
      const repo = repoRoot.trim();
      const path = worktreePath.trim();
      if (!path) return null;

      // 1) Create the worktree on disk unless it already exists (MCP path).
      if (!opts?.alreadyCreated) {
        const { gitWorktreeAdd } = await import("../ipc/git");
        // Let a git failure reject — the caller (FilePanel) surfaces the message.
        await gitWorktreeAdd(repo, path, branch?.trim() || undefined);
      }

      // 2) Resolve the target tab, then spawn a terminal in it.
      try {
        const { spawnTerminal } = await import("../ipc/client");
        const name =
          opts?.tabName?.trim() ||
          branch?.trim() ||
          path.split("/").filter(Boolean).pop() ||
          "Worktree";
        // Deterministic placement (TASK C / #22): when the control/MCP path supplies
        // a tab id (resolved CORE-side by name), reuse/create THAT tab by id+name —
        // never the focused tab. The UI (FilePanel) path passes no id, so it creates
        // a fresh tab as before.
        let tabId: string;
        if (opts?.tabId) {
          tabId = get().ensureTab(opts.tabId, name);
        } else {
          tabId = get().addTab(); // creates + activates a fresh tab
          get().renameTab(tabId, name);
        }

        const info = await spawnTerminal({ cwd: path, name });
        // Place the tile in the resolved worktree tab (by id, not active state) and
        // focus it.
        get().addToTab(tabId, info);
        return info.id;
      } catch (err) {
        console.error("addWorktreeWorkspace: spawn failed", err);
        return null;
      }
    },

    removeWorktreeWorkspace: async (repoRoot, worktreePath, force) => {
      const repo = repoRoot.trim();
      const path = worktreePath.trim().replace(/\/+$/, "");
      if (!path) return;

      // 1) DETACH every live tile whose cwd is the worktree dir (or inside it),
      // BEFORE git removes the dir. Detaching keeps the tmux session alive (no
      // orphaned process); the tile just leaves this window's layout. We match on a
      // path-segment boundary so `/x/wt` does not match `/x/wt-other`.
      const { terminals } = get();
      const prefix = path + "/";
      const victims = Object.values(terminals)
        .filter((t) => {
          const cwd = (t.cwd ?? "").replace(/\/+$/, "");
          return cwd === path || cwd.startsWith(prefix);
        })
        .map((t) => t.id);
      for (const id of victims) get().detachTile(id);

      // 2) Now that no process is rooted in the dir, remove the worktree. A
      // failure (e.g. uncommitted changes without `force`) rejects with git's
      // message; the tiles are already safely detached regardless.
      const { gitWorktreeRemove } = await import("../ipc/git");
      await gitWorktreeRemove(repo, path, force);
    },

    remove: (id) => {
      // The tile is going away (detach + delete both funnel here) -> drop its
      // external per-tile state (panel view + any managed dev server). Not
      // reached by a tab MOVE, so a moved tile keeps its panel state.
      cleanupTileSideState(id);
      const { tabs, focusedId, terminals, activeTabId } = get();
      const owner = tabOf(tabs, id);
      const nextTabs = tabs.map((t) =>
        t.order.includes(id)
          ? { ...t, order: t.order.filter((x) => x !== id) }
          : t,
      );

      // Only recompute focus if the removed tile lived in the active tab.
      let nextFocus = focusedId;
      if (owner && owner.id === activeTabId) {
        const prevOrder = owner.order;
        const newOrder = prevOrder.filter((x) => x !== id);
        nextFocus = neighborFocus(prevOrder, newOrder, id, focusedId);
      }

      const nextTerminals = { ...terminals };
      delete nextTerminals[id];

      set({ terminals: nextTerminals, tabs: nextTabs, focusedId: nextFocus });
      persist();
    },

    detachTile: (id) => {
      // Detach the PTY client but DO NOT kill tmux: the backing session keeps
      // running so the terminal can be re-adopted later. Drop the tile from the
      // layout immediately; the backend call is fire-and-forget. The dynamic
      // import keeps the store free of a hard Tauri dependency (web/test safe),
      // matching saveToBackend's pattern.
      void import("../ipc/client")
        .then((m) => m.closeTerminal(id))
        .catch((err) => console.error("closeTerminal failed", err));
      get().remove(id);
    },

    deleteTerminal: (id) => {
      // Destructive: kill the tmux session for good (terminates its process
      // tree) via the backend, then drop the tile. Dynamic import as above.
      void import("../ipc/client")
        .then((m) => m.killTerminal(id))
        .catch((err) => console.error("killTerminal failed", err));
      get().remove(id);
    },

    restartTerminal: async (id) => {
      // Recover a frozen session: spawn a FRESH tmux session in the same cwd,
      // swap it into the OLD tile's exact tab + slot, then kill the old session.
      // Reuses the same spawnTerminal / killTerminal IPCs the "+" and "×" use, so
      // there is no new tmux logic and the tile lands exactly where it was.
      const info = get().terminals[id];
      if (!info) return null;
      const cwd = (info.cwd ?? "").trim();
      try {
        const { spawnTerminal, killTerminal } = await import("../ipc/client");
        const fresh = await spawnTerminal({ cwd: cwd || undefined });
        // Drop the old tile's per-tile side state (panel view, dev server,
        // captain pin, context reading, …) BEFORE the swap — same cleanup
        // `remove` runs, but here we place the fresh tile in the vacated slot
        // rather than closing the cell.
        cleanupTileSideState(id);
        const s = get();
        let placed = false;
        const nextTabs = s.tabs.map((t) => {
          const at = t.order.indexOf(id);
          if (at === -1) return t;
          const order = [...t.order];
          order[at] = fresh.id; // in-place: SAME slot the old tile held
          placed = true;
          return { ...t, order };
        });
        // Fallback: if the old tile had already left every tab mid-restart, drop
        // the fresh tile into the active tab so it is never orphaned.
        if (!placed) {
          const active = s.activeTabId;
          for (let i = 0; i < nextTabs.length; i++) {
            if (nextTabs[i].id === active) {
              nextTabs[i] = {
                ...nextTabs[i],
                order: [...nextTabs[i].order, fresh.id],
              };
              break;
            }
          }
        }
        const nextTerminals = { ...s.terminals, [fresh.id]: fresh };
        delete nextTerminals[id];
        set({
          tabs: nextTabs,
          terminals: nextTerminals,
          focusedId: s.focusedId === id ? fresh.id : s.focusedId,
        });
        persist();
        // Kill the OLD tmux session (process tree) now that the tile is replaced.
        // Fire-and-forget relative to the swap (which already stands), but retry
        // once and surface a visible notice on a second failure — otherwise a
        // failed kill leaks a frozen tmux session with no tile bound to it.
        void killOldSessionWithRetry(id, killTerminal);
        return fresh.id;
      } catch (err) {
        // The only await before the synchronous swap is the spawn, so a throw
        // here means the fresh session never came up: the old tile is untouched.
        console.error("restartTerminal failed", err);
        return null;
      }
    },

    setFocus: (id) => {
      // Focusing a tile implies the user is working in the canvas, so navigation
      // focus returns to the terminal region (so a subsequent Ctrl+Tab cycles
      // terminals, and Ctrl+B toggles back to the sidebar). Only `focusedId` is
      // persisted; the region is transient.
      const cur = get();
      if (cur.focusedId === id) {
        if (cur.focusedRegion !== "terminal") set({ focusedRegion: "terminal" });
        return;
      }
      set({ focusedId: id, focusedRegion: "terminal" });
      persist();
    },

    setFocusRegion: (region) => {
      if (get().focusedRegion === region) return;
      set({ focusedRegion: region });
    },

    toggleFocusRegion: () => {
      const next: FocusRegion =
        get().focusedRegion === "sidebar" ? "terminal" : "sidebar";
      set({ focusedRegion: next });
      return next;
    },

    updateState: (id, state) => {
      const existing = get().terminals[id];
      if (existing) {
        if (existing.state === state) return;
        set({
          terminals: { ...get().terminals, [id]: { ...existing, state } },
        });
        return;
      }
      // No record yet, but the terminal lives in a tab (it was restored from a
      // persisted layout): a terminal://state event raced ahead of the
      // listTerminals() seed. Upsert a minimal record so the transition isn't
      // dropped -- otherwise an attach's `live` event arriving before setTerminals
      // is lost and the tile stays stuck on the amber "starting" fallback (#16).
      // Ignore states for ids we don't track at all (no tab, no record).
      const { tabs } = get();
      if (!tabs.some((t) => t.order.includes(id))) return;
      set({
        terminals: {
          ...get().terminals,
          [id]: { id, tmuxSession: `th_${id}`, cwd: "", title: id, state },
        },
      });
    },

    setTerminalLabel: (id, label) => {
      const trimmed = label.trim();
      const { userLabels, claudeTitles } = get();
      // Blank clears the override (the Claude title / derived label takes back
      // over); no-op if unchanged so a redundant set doesn't thrash persist.
      let nextUser: Record<TerminalId, string>;
      if (!trimmed) {
        if (!(id in userLabels)) return;
        nextUser = { ...userLabels };
        delete nextUser[id];
      } else {
        if (userLabels[id] === trimmed) return;
        nextUser = { ...userLabels, [id]: trimmed };
      }
      // Recompute the effective map the display reads (rename overlays Claude).
      set({ userLabels: nextUser, labels: mergeLabels(nextUser, claudeTitles) });
      persist();
    },

    setClaudeTitle: (id, title) => {
      const trimmed = title.trim();
      const { userLabels, claudeTitles } = get();
      let nextClaude: Record<TerminalId, string>;
      if (!trimmed) {
        if (!(id in claudeTitles)) return;
        nextClaude = { ...claudeTitles };
        delete nextClaude[id];
      } else {
        if (claudeTitles[id] === trimmed) return;
        nextClaude = { ...claudeTitles, [id]: trimmed };
      }
      // Update the live Claude signal and the effective map. A user rename (in
      // `userLabels`) still wins via mergeLabels, so we never clobber a rename.
      // Not persisted: Claude titles are re-derived from hooks each session.
      set({
        claudeTitles: nextClaude,
        labels: mergeLabels(userLabels, nextClaude),
      });
    },

    addTab: () => {
      const { tabs } = get();
      // Auto-name "Workspace N" using the lowest free index.
      const used = new Set(
        tabs
          .map((t) => /^Workspace (\d+)$/.exec(t.name)?.[1])
          .filter((n): n is string => !!n)
          .map((n) => Number(n)),
      );
      let n = 1;
      while (used.has(n)) n += 1;
      const tab: WorkspaceTab = {
        id: newTabId(),
        name: `Workspace ${n}`,
        order: [],
      };
      set({ tabs: [...tabs, tab], activeTabId: tab.id, focusedId: null });
      persist();
      return tab.id;
    },

    renameTab: (id, name) => {
      const trimmed = name.trim();
      if (!trimmed) return;
      const { tabs } = get();
      if (!tabs.some((t) => t.id === id)) return;
      set({ tabs: tabs.map((t) => (t.id === id ? { ...t, name: trimmed } : t)) });
      persist();
    },

    closeWorkspace: (id) => {
      // The reserved Captains tab is never closeable.
      if (id === CAPTAINS_TAB_ID) return;
      // Tier 3 reap. ONLY the workspace × calls this; switch/pop-out never do, so
      // they can't kill. Mirror closeTab's last-tab guard BEFORE killing so a kill
      // never fires when closeTab would refuse to remove the tab.
      const { tabs } = get();
      // Guard on the WORK-tab count: the reserved Captains tab is ALWAYS present,
      // so it must not count toward the last-tab check - else the last work tab
      // could be closed, parking the user on the Captains-only view.
      if (tabs.filter((t) => t.id !== CAPTAINS_TAB_ID).length <= 1) return;
      const target = tabs.find((t) => t.id === id);
      if (!target) return;
      const ids = target.order.slice();

      const refreshRecent = (): void => {
        if (typeof window !== "undefined") {
          window.dispatchEvent(new Event("t-hub:recent-changed"));
        }
      };
      // RECALL-FIRST: drop the daemon's Recent cache, THEN force the re-fetch — the
      // dispatch is chained AFTER the invalidate resolves so RecentList re-scans a
      // freshly-dropped cache, not the stale 15s-TTL one (a brand-new project closed
      // within 15s of its first scan would otherwise lag). On a failure we still
      // refresh (the on-disk transcript — Recent's source of truth — survives the
      // SIGKILL, so `claude --resume` works regardless, and the open-cwd filter
      // un-hides the closed projects synchronously via closeTab below).
      void import("../ipc/recent")
        .then((m) => m.invalidateRecentCache())
        .then(refreshRecent)
        .catch((err) => {
          console.error("invalidateRecentCache failed", err);
          refreshRecent();
        });

      // SIGKILL each session's process tree via the SAME backend path the per-tile
      // × uses (killTerminal → kill_terminal → tmux::kill_session_tree). Fire-and-
      // forget (mirrors deleteTerminal); a kill error is logged, not surfaced.
      void import("../ipc/client").then((m) => {
        for (const tid of ids) {
          m.killTerminal(tid).catch((err) =>
            console.error("killTerminal failed (closeWorkspace)", err),
          );
        }
      });

      // Layout removal/prune/persist (also deletes the tiles from `terminals`, so
      // RecentList's open-cwd filter reactively un-hides the closed projects — the
      // immediate visible recall, independent of the cache re-fetch above).
      get().closeTab(id);
    },

    closeTab: (id) => {
      const { tabs, activeTabId, focusedId, terminals } = get();
      if (id === CAPTAINS_TAB_ID) return []; // reserved: never closeable
      // Keep at least one WORK tab: the reserved Captains tab is always present
      // and must not count toward the guard, so closing the last work tab is
      // refused (else the user is parked on the Captains-only view).
      if (tabs.filter((t) => t.id !== CAPTAINS_TAB_ID).length <= 1) return [];
      const target = tabs.find((t) => t.id === id);
      if (!target) return [];

      // Tiles this tab held; returned so the caller can detach their terminals
      // (closeTerminal — tmux survives). Also dropped from the live map here so
      // the canvas stops rendering them once the tab is gone.
      const removed = target.order.slice();

      const idx = tabs.findIndex((t) => t.id === id);
      const nextTabs = tabs.filter((t) => t.id !== id);

      // If we closed the active tab, activate a neighbor.
      let nextActive = activeTabId;
      let nextFocus = focusedId;
      if (activeTabId === id) {
        const neighbor = nextTabs[idx] ?? nextTabs[idx - 1] ?? nextTabs[0];
        nextActive = neighbor.id;
        nextFocus = neighbor.order[0] ?? null;
      }

      const nextTerminals = { ...terminals };
      for (const tid of removed) {
        delete nextTerminals[tid];
        cleanupTileSideState(tid); // closing the tab takes its tiles with it
      }
      // The tab is gone for good — drop its color identity so a recycled tab id
      // can't inherit it. (A POP-OUT keeps the record, so popOutTab must NOT.)
      useTheme.getState().clearWorkspaceColor(id);

      set({
        terminals: nextTerminals,
        tabs: nextTabs,
        activeTabId: nextActive,
        focusedId: nextFocus,
      });
      persist();
      return removed;
    },

    setActiveTab: (id) => {
      const { tabs, activeTabId } = get();
      if (id === activeTabId) return;
      const tab = tabs.find((t) => t.id === id);
      if (!tab) return;
      set({ activeTabId: id, focusedId: tab.order[0] ?? null });
      persist();
    },

    setActiveTabByIndex: (i) => {
      const { tabs, activeTabId } = get();
      const tab = tabs[i];
      if (!tab || tab.id === activeTabId) return;
      set({ activeTabId: tab.id, focusedId: tab.order[0] ?? null });
      persist();
    },

    cycleTab: (dir) => {
      const { tabs, activeTabId } = get();
      if (tabs.length <= 1) return;
      const idx = tabs.findIndex((t) => t.id === activeTabId);
      const nextIdx = (idx + dir + tabs.length) % tabs.length;
      const next = tabs[nextIdx];
      set({ activeTabId: next.id, focusedId: next.order[0] ?? null });
      persist();
    },

    cycleTile: (dir) => {
      const order = activeTab().order;
      if (order.length <= 1) return;
      const { focusedId } = get();
      const cur = focusedId ? order.indexOf(focusedId) : -1;
      const base = cur >= 0 ? cur : 0;
      const nextIdx = (base + dir + order.length) % order.length;
      const nextId = order[nextIdx];
      if (nextId === focusedId) return;
      // Reuse setFocus so navigation focus snaps back to the terminal region.
      get().setFocus(nextId);
    },

    cycleTileGlobal: (dir) => {
      const { tabs, activeTabId, focusedId } = get();
      // Flatten EVERY tab's tile order (strip order) into one global ring, each
      // entry tagged with its owning tab so we can switch tabs when we cross a
      // boundary. Popped-out tabs live in other windows, so they're excluded.
      const flat: { id: TerminalId; tabId: string }[] = [];
      for (const t of tabs) {
        for (const id of t.order) flat.push({ id, tabId: t.id });
      }
      if (flat.length <= 1) return;
      // Locate the current focused tile in the global ring (prefer the entry in
      // the ACTIVE tab when an id somehow appears twice; ids are unique across
      // tabs in practice, but this keeps the step deterministic). Default to the
      // start so an unset focus still cycles.
      let cur = flat.findIndex(
        (e) => e.id === focusedId && e.tabId === activeTabId,
      );
      if (cur < 0) cur = flat.findIndex((e) => e.id === focusedId);
      const base = cur >= 0 ? cur : 0;
      const next = flat[(base + dir + flat.length) % flat.length];
      if (next.id === focusedId && next.tabId === activeTabId) return;
      // Cross a workspace boundary by activating the owning tab first, then focus
      // the tile. setActiveTab re-points focus to that tab's first tile; the
      // following setFocus overrides it with the exact target AND snaps the nav
      // focus back to the terminal region.
      if (next.tabId !== activeTabId) get().setActiveTab(next.tabId);
      get().setFocus(next.id);
    },

    moveTab: (id, targetId) => {
      if (id === targetId) return;
      const { tabs } = get();
      const from = tabs.findIndex((t) => t.id === id);
      const to = tabs.findIndex((t) => t.id === targetId);
      if (from < 0 || to < 0) return;
      const next = tabs.slice();
      const [moved] = next.splice(from, 1);
      // Insert at the TARGET's slot regardless of drag direction. Removing an
      // earlier source (from < to) shifts the target down one, so without this
      // adjustment a downward move would land one slot PAST the target (the
      // off-by-one the insertion highlight didn't match). Upward moves are
      // unaffected (from > to => adj === to).
      const adj = from < to ? to - 1 : to;
      next.splice(adj, 0, moved);
      // Reordering doesn't change which tab is active; activeTabId is untouched.
      set({ tabs: next });
      persist();
    },

    popOutTab: (id) => {
      const { tabs, poppedOutTabs, activeTabId, focusedId } = get();
      const tab = tabs.find((t) => t.id === id);
      if (!tab) return; // unknown / already popped out
      // Move the record out of the rendered set so the strip + canvas drop it.
      const nextTabs = tabs.filter((t) => t.id !== id);
      const nextPopped = poppedOutTabs.some((t) => t.id === id)
        ? poppedOutTabs
        : [...poppedOutTabs, tab];

      // Keep >=1 rendered tab. If this was the only tab, leave a fresh empty one
      // so the main window still has a canvas to work with.
      const renderedTabs =
        nextTabs.length > 0
          ? nextTabs
          : [{ id: newTabId(), name: DEFAULT_TAB_NAME, order: [] }];

      // If the popped tab was active, hand activeness to a still-rendered tab.
      let nextActive = activeTabId;
      let nextFocus = focusedId;
      if (activeTabId === id) {
        nextActive = renderedTabs[0].id;
        nextFocus = renderedTabs[0].order[0] ?? null;
      }
      set({
        tabs: renderedTabs,
        poppedOutTabs: nextPopped,
        activeTabId: nextActive,
        focusedId: nextFocus,
      });
      persist();
    },

    popInTab: (id, tab) => {
      const { tabs, poppedOutTabs } = get();
      const stashed = poppedOutTabs.find((t) => t.id === id);
      // Nothing to re-adopt, or it's somehow already visible: clear any stash.
      if (!stashed && !tab) return;
      if (tabs.some((t) => t.id === id)) {
        set({ poppedOutTabs: poppedOutTabs.filter((t) => t.id !== id) });
        persist();
        return;
      }
      const record = tab ?? stashed!;
      set({
        tabs: [...tabs, record],
        poppedOutTabs: poppedOutTabs.filter((t) => t.id !== id),
      });
      persist();
    },

    moveTile: (id, targetId) => {
      if (id === targetId) return;
      const { tabs } = get();
      const active = activeTab();
      const from = active.order.indexOf(id);
      const to = active.order.indexOf(targetId);
      if (from < 0 || to < 0) return; // both must be in the active tab

      const order = active.order.slice();
      const [moved] = order.splice(from, 1);
      order.splice(to, 0, moved);

      // Tile count/shape may change rows -> drop stale manual sizes for safety.
      const nextTabs = tabs.map((t) =>
        t.id === active.id ? { ...t, order, sizes: undefined } : t,
      );
      set({ tabs: nextTabs, focusedId: id });
      persist();
    },

    setDraggingTile: (id) => {
      // Transient drag UI only — never persisted. No-op if unchanged so a
      // pointermove-driven re-set doesn't thrash subscribers.
      if (get().draggingTileId === id) return;
      set({ draggingTileId: id });
    },

    moveTileToTab: (id, tabId) => {
      const { tabs, activeTabId, focusedId } = get();
      const source = tabOf(tabs, id);
      if (!source || source.id === tabId) return; // unknown, or already there
      if (!tabs.some((t) => t.id === tabId)) return; // unknown target tab

      // Pull the tile from its source tab and append it to the target tab. Both
      // tabs' manual size ratios are dropped (their grid shapes changed).
      const nextTabs = tabs.map((t) => {
        if (t.id === source.id) {
          return {
            ...t,
            order: t.order.filter((x) => x !== id),
            sizes: undefined,
          };
        }
        if (t.id === tabId) {
          return { ...t, order: [...t.order, id], sizes: undefined };
        }
        return t;
      });

      // If the moved tile was the focused tile of the (still-active) source tab,
      // hand focus to a neighbor; otherwise leave focus + active tab untouched.
      let nextFocus = focusedId;
      if (source.id === activeTabId && focusedId === id) {
        const newOrder = source.order.filter((x) => x !== id);
        nextFocus = neighborFocus(source.order, newOrder, id, focusedId);
      }

      set({ tabs: nextTabs, focusedId: nextFocus });
      persist();
    },

    ensureCaptainsTab: () => {
      const { tabs } = get();
      if (tabs.some((t) => t.id === CAPTAINS_TAB_ID)) return CAPTAINS_TAB_ID;
      set({
        tabs: [
          ...tabs,
          { id: CAPTAINS_TAB_ID, name: CAPTAINS_TAB_NAME, order: [] },
        ],
      });
      persist();
      return CAPTAINS_TAB_ID;
    },

    moveTileToCaptainsTab: (id) => {
      get().ensureCaptainsTab();
      const { tabs, activeTabId, focusedId } = get();
      const source = tabOf(tabs, id);
      if (source && source.id === CAPTAINS_TAB_ID) return; // already placed

      const nextTabs = tabs.map((t) => {
        if (source && t.id === source.id) {
          return { ...t, order: t.order.filter((x) => x !== id), sizes: undefined };
        }
        if (t.id === CAPTAINS_TAB_ID) {
          const order = t.order.includes(id) ? t.order : [...t.order, id];
          return { ...t, order, sizes: undefined };
        }
        return t;
      });

      // Only touch focus if the moved tile was the active work tab's focused
      // tile - never steal the active tab or the user's view.
      let nextFocus = focusedId;
      if (source && source.id === activeTabId && focusedId === id) {
        const newOrder = source.order.filter((x) => x !== id);
        nextFocus = neighborFocus(source.order, newOrder, id, focusedId);
      }

      set({ tabs: nextTabs, focusedId: nextFocus });
      persist();
    },

    moveTileToWorkTab: (id) => {
      const { tabs } = get();
      const captains = tabs.find((t) => t.id === CAPTAINS_TAB_ID);
      if (!captains || !captains.order.includes(id)) return; // not an agent tile

      const target = tabs.find((t) => t.id !== CAPTAINS_TAB_ID);
      if (target) {
        const nextTabs = tabs.map((t) => {
          if (t.id === CAPTAINS_TAB_ID) {
            return {
              ...t,
              order: t.order.filter((x) => x !== id),
              sizes: undefined,
            };
          }
          if (t.id === target.id) {
            return { ...t, order: [...t.order, id], sizes: undefined };
          }
          return t;
        });
        set({ tabs: nextTabs });
      } else {
        // No work tab exists (all-reserved edge): mint a fresh one, work tabs
        // first so the reserved tab stays at the end.
        const fresh: WorkspaceTab = {
          id: newTabId(),
          name: DEFAULT_TAB_NAME,
          order: [id],
        };
        const pruned = tabs.map((t) =>
          t.id === CAPTAINS_TAB_ID
            ? { ...t, order: t.order.filter((x) => x !== id), sizes: undefined }
            : t,
        );
        set({ tabs: [fresh, ...pruned] });
      }
      persist();
    },

    setDraggingTab: (id) => {
      if (get().draggingTabId === id) return;
      set({ draggingTabId: id });
    },
    setDropTile: (id) => {
      if (get().dropTileId === id) return;
      set({ dropTileId: id });
    },
    setDropTab: (id) => {
      if (get().dropTabId === id) return;
      set({ dropTabId: id });
    },

    setTabSizes: (id, sizes) => {
      const { tabs } = get();
      if (!tabs.some((t) => t.id === id)) return;
      set({ tabs: tabs.map((t) => (t.id === id ? { ...t, sizes } : t)) });
      persist();
    },

    zoomIn: () => {
      set({ fontSize: clampFont(get().fontSize + 1) });
      persist();
    },
    zoomOut: () => {
      set({ fontSize: clampFont(get().fontSize - 1) });
      persist();
    },
    zoomReset: () => {
      set({ fontSize: DEFAULT_FONT_SIZE });
      persist();
    },
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
