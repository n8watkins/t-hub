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
import type { TerminalInfo, TerminalId, TerminalState } from "../ipc/types";
import { usePanels } from "./panels";
import { useTheme } from "./theme";

/**
 * Clean up the per-tile side state that lives OUTSIDE this store when a
 * terminal's tile goes away for good (detach / delete / close-tab):
 *   - the per-tile panel state (active view, detected/typed URLs) in usePanels;
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
  void import("../ipc/devserver")
    .then((m) => m.stopDevServer(id))
    .catch(() => {
      /* no dev server for this id, or no Tauri runtime — nothing to stop */
    });
}

/**
 * localStorage key for the workspace snapshot. v2 introduced workspace tabs;
 * a v1 snapshot (flat order/focus) is migrated into a single tab on load.
 */
const PERSIST_KEY = "termhub.workspace.v2";
const LEGACY_KEY = "termhub.workspace.v1";

/** Global terminal font size (px) bounds + default, shared by every tile. */
const DEFAULT_FONT_SIZE = 13;
const MIN_FONT_SIZE = 6;
const MAX_FONT_SIZE = 28;

/** Default name for the first/auto-created tab. */
const DEFAULT_TAB_NAME = "Workspace 1";

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
  /** Pointer-drag state (transient, never persisted). TermHub's drag-and-drop is
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
   *  failure. */
  recall: (sessionId: string, cwd: string) => Promise<TerminalId | null>;

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
    .then((m) => m.saveWorkspaceSnapshot(json))
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

const loaded = loadPersisted();
const initial = SATELLITE_TAB
  ? scopeToSatellite(loaded, SATELLITE_TAB)
  : adoptOrphans(loaded);

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
      const { tabs, focusedId, terminals } = get();
      const active = activeTab();
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

    // --- Recall (feat/projects-sidebar, Agent A) ------------------------------
    // Re-spawn + resume a past Claude session into the active tab. This is the
    // store-side spawn helper the sidebar's Recent list uses; it deliberately
    // reuses the SAME spawn path as Canvas's "+" menu (`spawnTerminal` IPC then
    // `addAfterFocused`) so there is exactly one way a tile gets created. We add
    // it here (rather than reaching into Canvas, which another agent owns) per the
    // build split. The dynamic `../ipc/client` import keeps the store free of a
    // hard Tauri dependency, matching detachTile/deleteTerminal/saveToBackend.
    recall: async (sessionId, cwd) => {
      const id = sessionId.trim();
      const dir = cwd.trim();
      if (!id) return null;
      try {
        const { spawnTerminal } = await import("../ipc/client");
        const { useSettings } = await import("./settings");
        // Spawn rooted at the session's cwd. Whether we actually launch Claude is
        // a SETTING (resumeStartsClaude, default on): on -> `claude --resume <id>`
        // resumes that conversation directly; off -> just a terminal in the dir
        // (no Claude). Quoting the id is a defensive guard (ids are plain UUIDs).
        const startClaude = useSettings.getState().resumeStartsClaude;
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
      }
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

    closeTab: (id) => {
      const { tabs, activeTabId, focusedId, terminals } = get();
      if (tabs.length <= 1) return []; // keep at least one tab
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
      next.splice(to, 0, moved);
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
async function hydrateFromBackend(): Promise<void> {
  if (SATELLITE_TAB) return;
  if (typeof window === "undefined") return; // no webview → no backend
  try {
    const { loadWorkspaceSnapshot } = await import("../ipc/persistence");
    const json = await loadWorkspaceSnapshot();
    const snapshot = parseV2Snapshot(json);

    if (!snapshot) {
      // Nothing durable yet: seed SQLite once from the current (localStorage-
      // derived) layout so subsequent boots can prefer the durable copy.
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

    // The durable copy wins. Adopt orphaned popped-out tabs (a fresh main-window
    // launch has no satellites yet) just like the localStorage boot path does.
    const layout = adoptOrphans(snapshot);

    // Only overwrite if the store hasn't already reconciled live terminals onto
    // a different arrangement (i.e. a spawn/listTerminals beat us). If it has, the
    // backend's setTerminals reconciliation is authoritative for this session;
    // we still refresh the durable copy from that state via the next persist().
    if (Object.keys(useWorkspace.getState().terminals).length > 0) return;

    // `layout.labels` is the persisted user-rename set: adopt it as userLabels
    // and recompute the effective `labels` over any live Claude titles.
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
    // Re-mirror to localStorage so both copies agree after adopting SQLite.
    savePersisted({
      tabs: layout.tabs,
      activeTabId: layout.activeTabId,
      focusedId: layout.focusedId,
      fontSize: layout.fontSize,
      labels: layout.labels,
      poppedOutTabs: layout.poppedOutTabs,
    });
  } catch {
    // No backend or a transient error — keep the localStorage-derived boot state.
  }
}

// Kick off durable hydration once, fire-and-forget. Never blocks module load.
void hydrateFromBackend();

// ---------------------------------------------------------------------------
// GOAL NAMES: feed Claude-suggested titles from the lifecycle hooks into the
// label map. The backend emits `agent://title` ({ sessionId, cwd, title }) when
// a hook (UserPromptSubmit / SessionStart) yields a usable summary. TermHub
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
 *  matching terminal's label. Fire-and-forget; a missing Tauri runtime (e.g. a
 *  test/SSR context) is tolerated. */
async function subscribeClaudeTitles(): Promise<void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    await listen<{ sessionId: string; cwd?: string; title: string }>(
      "agent://title",
      (ev) => {
        const { cwd, title } = ev.payload;
        if (!title) return;
        const id = terminalForCwd(useWorkspace.getState().terminals, cwd);
        // TODO(claude-title): when the backend can map a Claude session id to a
        // terminal directly (shared layer), prefer that over cwd correlation.
        if (id) useWorkspace.getState().setClaudeTitle(id, title);
      },
    );
  } catch {
    // No Tauri event runtime — titles simply won't stream (non-fatal).
  }
}

void subscribeClaudeTitles();
