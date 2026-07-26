// Shared internals for the workspace store slices (store/workspace/*). These are
// the PURE helpers, constants, and types that the store's action slices need but
// that carry no dependency on the store instance itself, so they live here rather
// than in workspace.ts. Keeping them free of any `useWorkspace` reference lets the
// slices import from THIS module without forming a cycle back through workspace.ts
// (which imports the slice factories). The impure, store-bound helpers (`persist`,
// `activeTab`, `placeWorkTile`, `cleanupTileSideState`, `killOldSessionWithRetry`,
// the captain-registry accessor, the satellite flag, and the in-flight recall
// guard) are built inside the store closure in workspace.ts and handed to each
// slice via the {@link SliceDeps} object.
import type { StoreApi } from "zustand";
import type { TabReport, TerminalInfo, TerminalId, TerminalState } from "../../ipc/types";

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
 * the authoritative Captain claims ONLY as a liveness fallback (see
 * {@link SliceDeps.captainRegistryIds}), via an accessor the captain store
 * registers - so this store keeps NO static dependency on the captain store.
 */
export const CAPTAINS_TAB_ID = "captains-reserved";
export const CAPTAINS_TAB_NAME = "Captain Workspace";
export type WorkspaceKind = "work" | "captain";

export function workspaceKind(tab: Pick<WorkspaceTab, "id" | "kind">): WorkspaceKind {
  return tab.kind ?? (tab.id === CAPTAINS_TAB_ID ? "captain" : "work");
}

/** Return `tabs` guaranteed to contain EXACTLY ONE reserved Captains tab: if
 *  absent, append a fresh empty one; if duplicated (a stale persisted snapshot,
 *  or a server report that echoed the client-only reserved tab back), collapse
 *  every copy into a single tab that keeps the first copy's slot and unions their
 *  orders. Shared by finalizeLayout (load) and adoptRegistry (server sync) so the
 *  reserved tab can never be lost NOR duplicated - a duplicate empty copy would
 *  render a stray "new terminal" placeholder next to the real, populated one. */
export function ensureReservedCaptainsTab(tabs: WorkspaceTab[]): WorkspaceTab[] {
  const copies = tabs.filter((t) => t.id === CAPTAINS_TAB_ID);
  if (copies.length === 0) {
    return [
      ...tabs,
      {
        schemaVersion: 1,
        id: CAPTAINS_TAB_ID,
        name: CAPTAINS_TAB_NAME,
        kind: "captain",
        order: [],
      },
    ];
  }
  if (copies.length === 1) {
    return tabs.map((tab) =>
      tab.id === CAPTAINS_TAB_ID
        ? {
            ...tab,
            schemaVersion: 1,
            name: CAPTAINS_TAB_NAME,
            kind: "captain",
          }
        : tab,
    );
  }
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
    schemaVersion: 1,
    kind: "captain",
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
  schemaVersion?: 1;
  id: string;
  name: string;
  kind?: WorkspaceKind;
  /** Tile order within this tab, by terminal id. */
  order: TerminalId[];
  /** Optional manual-mode grid ratios; absent => even auto-grid. */
  sizes?: TabSizes;
}

/** The subset of state we persist across UI reopens. */
export interface PersistedLayout {
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

export interface WorkspaceState {
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
  /** True once the SERVER has delivered its authoritative tab/tile registry
   *  (`adoptRegistry` with a non-empty snapshot). Transient, never persisted.
   *  Gates `setTerminals`' legacy blind-append: while the server owns placement
   *  (headless-org), an unplaced live `th_*` session must NOT be auto-dumped onto
   *  the active tab - that is how 13 leaked ghost sessions got adopted onto the
   *  canvas and blanked the UI. Before the first registry (a registry-less boot),
   *  the blind-append stays as a fallback so pre-existing sessions still show. */
  registryAdopted: boolean;

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
  /** Remove a git worktree only after the backend's unified safety service admits
   *  it. The current backend fails closed before this store detaches any tile;
   *  activation waits for canonical Git, ownership, and lease decisions. */
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

/** The store's own `setState`, exactly as zustand types it for `WorkspaceState`. */
export type StoreSet = StoreApi<WorkspaceState>["setState"];
/** The store's own `getState`, exactly as zustand types it for `WorkspaceState`. */
export type StoreGet = StoreApi<WorkspaceState>["getState"];

/**
 * The store-bound helpers each slice needs but which cannot live here (they close
 * over the store instance or module state that stays in workspace.ts). Built once
 * inside the store closure and handed to every slice factory. Keeping these in a
 * single object means an action body reads `deps.persist()` etc. exactly where the
 * inlined closure previously read the bare helper — a mechanical rename, no logic
 * change.
 */
export interface SliceDeps {
  /** Persist the current (tabs, activeTabId, focusedId, fontSize, poppedOutTabs). */
  persist: () => void;
  /** The active tab (always present: the store guarantees >=1 tab). */
  activeTab: () => WorkspaceTab;
  /** Place a spawned WORK tile into a work tab, never the reserved Captains tab. */
  placeWorkTile: (info: TerminalInfo, preferredTabId?: string) => void;
  /** Clean up the per-tile side state that lives OUTSIDE this store. */
  cleanupTileSideState: (id: TerminalId) => void;
  /** Kill the OLD tmux session behind a restart, retrying ONCE. */
  killOldSessionWithRetry: (
    id: TerminalId,
    killTerminal: (id: TerminalId) => Promise<void>,
  ) => Promise<void>;
  /** Server-backed Captain/Cortana terminal IDs used as authoritative liveness. */
  captainRegistryIds: () => Iterable<TerminalId>;
  /** Local presentation IDs protected during pre-registry recovery and workspace close. */
  agentPresentationIds: () => Iterable<TerminalId>;
  /** The tab id this window was opened to render in isolation, or null (main window). */
  satelliteTab: string | null;
  /** In-flight recall guard (#7), keyed by sessionId. */
  recallInFlight: Set<string>;
}

/**
 * localStorage key for the workspace snapshot. v2 introduced workspace tabs;
 * a v1 snapshot (flat order/focus) is migrated into a single tab on load.
 */
export const PERSIST_KEY = "t-hub.workspace.v2";
export const LEGACY_KEY = "t-hub.workspace.v1";

/** Global terminal font size (px) bounds + default, shared by every tile. */
export const DEFAULT_FONT_SIZE = 13;
export const MIN_FONT_SIZE = 6;
export const MAX_FONT_SIZE = 28;

/** Default name for the first/auto-created tab. */
export const DEFAULT_TAB_NAME = "Workspace 1";

export function clampFont(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_FONT_SIZE;
  return Math.max(MIN_FONT_SIZE, Math.min(MAX_FONT_SIZE, Math.round(n)));
}

let tabSeq = 0;
/** Monotonic-ish tab id (timestamp + counter so rapid creates stay unique). */
export function newTabId(): string {
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
export function shortenId(id: string): string {
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
export function cwdBasename(cwd: string | undefined): string {
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
export function mergeLabels(
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
  const id = typeof t.id === "string" && t.id ? t.id : newTabId();
  const kind: WorkspaceKind = id === CAPTAINS_TAB_ID ? "captain" : "work";
  return {
    schemaVersion: 1,
    id,
    name:
      kind === "captain"
        ? CAPTAINS_TAB_NAME
        : typeof t.name === "string" && t.name
          ? t.name
          : "Workspace",
    kind,
    order: cleanOrder(t.order),
    sizes: cleanSizes(t.sizes),
  };
}

/** Sanitize a parsed array of tab records (drops non-objects). */
function cleanTabs(value: unknown): WorkspaceTab[] {
  return Array.isArray(value)
    ? value
        .filter((t): t is Partial<WorkspaceTab> => !!t && typeof t === "object")
        .filter(
          (tab) =>
            tab.kind === undefined ||
            tab.kind === (tab.id === CAPTAINS_TAB_ID ? "captain" : "work"),
        )
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
export function parseV2Snapshot(raw: string | null | undefined): PersistedLayout | null {
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
export function loadPersisted(): PersistedLayout {
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
export function satelliteTabId(): string | null {
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
export function scopeToSatellite(layout: PersistedLayout, tabId: string): PersistedLayout {
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
export function adoptOrphans(layout: PersistedLayout): PersistedLayout {
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
export function neighborFocus(
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
export function tabOf(tabs: WorkspaceTab[], id: TerminalId): WorkspaceTab | undefined {
  return tabs.find((t) => t.order.includes(id));
}
