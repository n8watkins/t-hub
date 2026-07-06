// Frontend bridge for the MCP control channel's Organization-tier actions.
//
// The backend control listener (`src-tauri/src/control.rs`) accepts + audits
// Organization commands, applies them to the SERVER's authoritative tab registry
// (headless-org), and forwards the accepted `{command, args}` - with the registry
// snapshot under `args.sync` - to the frontend by emitting a Tauri
// `control://apply` event (wired in `lib.rs`). This module subscribes to that
// event and applies each mutation by adopting the snapshot into the workspace
// store, so a control-driven placement/move/close lands even when its target tab
// is hidden - and WITHOUT switching the user's active tab or stealing focus
// (only `focus_tab`/`focus_session` move the view, by design).
//
// The reverse direction (the tab reporter below) up-syncs USER-originated layout
// changes, carrying the last registry revision this window applied (`baseSeq`);
// the core rejects a stale report and answers with the snapshot to adopt, so the
// old lost-update race (a UI report clobbering a headless move_tile) is closed.
//
// It is a SIDE-EFFECT module (no React render): it sets up its subscription on
// import. It is pulled into the bundle by a single <script type="module"> in
// index.html, mirroring how `themeBootstrap` is loaded — so it needs no hook in
// App.tsx / main.tsx. Importing it outside a webview must not throw, so the
// Tauri `listen` is imported lazily and guarded. In a SATELLITE window the whole
// bridge is inert: a satellite holds one tab, so applying global mutations or
// reporting its scoped tab list would corrupt the main window's layout/registry.
import { listen } from "@tauri-apps/api/event";
import { isSatelliteWindow, useWorkspace } from "../store/workspace";
import type { TabReport, TabReportResult } from "./types";

/** Exact Tauri event the backend control listener emits to apply a UI mutation. */
export const CONTROL_APPLY_EVENT = "control://apply";

/** The payload shape carried by `control://apply`: a forwarded command + its args. */
interface ControlApply {
  command: string;
  args: Record<string, unknown> | null;
}

/** Read a string field from a loose args object (tolerates absent/null args). */
function str(
  args: Record<string, unknown> | null | undefined,
  key: string,
): string | undefined {
  const v = args?.[key];
  return typeof v === "string" && v ? v : undefined;
}

// The last authoritative registry revision this window applied (from an apply's
// `sync` payload or a report response). Rides every report as `baseSeq`.
let lastSeq = 0;
// True while adopting a server snapshot into the store: the tab reporter must
// not echo that change back up (it would bump the revision for nothing and
// widen the stale-report window). Zustand notifies subscribers synchronously
// inside set(), so a plain flag is race-free here.
let adoptingRegistry = false;

/**
 * Adopt an apply's authoritative registry snapshot (`args.sync`) into the
 * workspace store. Returns true if a snapshot was present. This is THE apply
 * path for organization mutations: the UI renders FROM the registry rather than
 * re-deriving the mutation (whose target may be hidden or unknown locally).
 */
function adoptSync(args: ControlApply["args"]): boolean {
  const sync = args?.sync;
  if (!sync || typeof sync !== "object") return false;
  const { seq, tabs } = sync as { seq?: unknown; tabs?: unknown };
  if (typeof seq !== "number" || !Array.isArray(tabs)) return false;
  lastSeq = seq;
  adoptingRegistry = true;
  try {
    useWorkspace.getState().adoptRegistry(tabs as TabReport[]);
  } finally {
    adoptingRegistry = false;
  }
  return true;
}

/**
 * Apply one forwarded Organization-tier command to the workspace store. Best-effort
 * and total: an unknown command or missing/mismatched args is a no-op (the store
 * methods themselves also no-op on unknown ids), so a malformed forward never
 * throws inside the event listener.
 */
export function applyControl(command: string, args: ControlApply["args"]): void {
  const ws = useWorkspace.getState();

  switch (command) {
    case "move_tile": {
      // Server-registry move: adopt the snapshot (applies into a hidden tab,
      // no focus change). A `targetId` form is the legacy within-tab reorder.
      const terminalId = str(args, "terminalId") ?? str(args, "id");
      const targetId = str(args, "targetId") ?? str(args, "targetTerminalId");
      if (terminalId && targetId) {
        ws.moveTile(terminalId, targetId);
        return;
      }
      if (adoptSync(args)) return;
      // Legacy core (no snapshot): best-effort direct move.
      const tabId = str(args, "tabId");
      if (terminalId && tabId) ws.moveTileToTab(terminalId, tabId);
      return;
    }

    case "rename_tab": {
      if (adoptSync(args)) return;
      const tabId = str(args, "tabId") ?? str(args, "id");
      const name = str(args, "name");
      if (tabId && name) ws.renameTab(tabId, name);
      return;
    }

    case "new_tab": {
      // Headless-org: the tab already exists in the registry snapshot - adopt it
      // WITHOUT activating (agents stage tabs in the background; focus_tab is the
      // explicit way to switch). Legacy fallback adopts by id (which activates).
      if (adoptSync(args)) return;
      const id = str(args, "id");
      const name = str(args, "name");
      if (id) {
        ws.adoptTab(id, name ?? "Workspace");
      } else {
        const local = ws.addTab();
        if (name) useWorkspace.getState().renameTab(local, name);
      }
      return;
    }

    case "close_tab":
    case "sync_tabs": {
      // close_tab: the tab left the registry; sync_tabs: a bare snapshot push
      // (e.g. close_terminal dropped a tile). Adopting handles both.
      adoptSync(args);
      return;
    }

    case "spawn_terminal": {
      // Headless-org: the SERVER already spawned the session (args.id) and placed
      // it in the registry; register the live terminal and adopt the snapshot -
      // no focus change, no view switch. The tile's xterm attaches on mount like
      // any adopted session. Legacy fallback (no id): spawn client-side.
      const id = str(args, "id");
      const cwd = str(args, "cwd");
      const name = str(args, "name");
      if (id) {
        ws.adoptTerminal({
          id,
          tmuxSession: str(args, "tmuxSession") ?? `th_${id}`,
          cwd: cwd ?? "",
          // Mirror commands::resolve_title (name || shell || "terminal"); the
          // ~5s meta poll refreshes it live afterwards.
          title: name ?? str(args, "shell") ?? "terminal",
          state: "live",
        });
        adoptSync(args);
        return;
      }
      const shell = str(args, "shell");
      const startupCommand = str(args, "startupCommand");
      void ws
        .spawnWorkspaceTerminal({ cwd, name, shell, startupCommand })
        .catch((e) => console.error("spawn_terminal failed", e));
      return;
    }

    case "focus_tab": {
      // MCP schema: { tabId } -> activate that workspace tab (the explicit,
      // intentional view switch).
      const tabId = str(args, "tabId") ?? str(args, "id");
      if (tabId && ws.tabs.some((t) => t.id === tabId)) ws.setActiveTab(tabId);
      return;
    }

    case "add_worktree_workspace": {
      // WS-4 / headless-org: the backend created the git worktree AND spawned the
      // worktree terminal (args.terminalId), placing it in the named tab in the
      // registry. Adopt both - no activation, no focus steal. Legacy fallback
      // (no terminalId): run the store's client-side create→tab→spawn helper.
      const worktreePath = str(args, "worktreePath") ?? str(args, "worktree_path");
      if (!worktreePath) return;
      const terminalId = str(args, "terminalId") ?? str(args, "terminal_id");
      if (terminalId) {
        ws.adoptTerminal({
          id: terminalId,
          tmuxSession: `th_${terminalId}`,
          cwd: worktreePath,
          title:
            str(args, "tabName") ??
            str(args, "branch") ??
            worktreePath.split("/").filter(Boolean).pop() ??
            "Worktree",
          state: "live",
        });
        adoptSync(args);
        return;
      }
      const repoRoot = str(args, "repoRoot") ?? str(args, "repo_root") ?? "";
      const branch = str(args, "branch");
      const tabName = str(args, "tabName") ?? str(args, "tab_name");
      const tabId = str(args, "tabId") ?? str(args, "tab_id");
      void ws
        .addWorktreeWorkspace(repoRoot, worktreePath, branch, {
          tabName,
          tabId,
          alreadyCreated: true,
        })
        .catch((e) => console.error("add_worktree_workspace failed", e));
      return;
    }

    case "remove_worktree_workspace": {
      // WS-4: detach any live tiles in the worktree dir (no orphaned process),
      // then `git worktree remove`. The backend forwarded this INSTEAD of running
      // git itself so the detach happens before the dir is torn down.
      const worktreePath = str(args, "worktreePath") ?? str(args, "worktree_path");
      if (!worktreePath) return;
      const repoRoot = str(args, "repoRoot") ?? str(args, "repo_root") ?? "";
      const force = args?.force === true;
      void ws
        .removeWorktreeWorkspace(repoRoot, worktreePath, force)
        .catch((e) => console.error("remove_worktree_workspace failed", e));
      return;
    }

    case "focus_session": {
      // MCP schema: { sessionId } -> switch to the session's tab and focus its
      // tile. The id may name a terminal/tile id, the owning tab's id, or a tab
      // index; we map best-effort. A terminal id activates its owning tab first,
      // then focuses the tile, so focus lands on a visible canvas.
      const id =
        str(args, "sessionId") ??
        str(args, "terminalId") ??
        str(args, "tabId") ??
        str(args, "id");
      if (!id) return;

      const { tabs } = ws;
      // 1) Treat `id` as a terminal/tile id: focus it (activating its tab).
      const owningTab = tabs.find((t) => t.order.includes(id));
      if (owningTab) {
        if (owningTab.id !== ws.activeTabId) ws.setActiveTab(owningTab.id);
        ws.setFocus(id);
        return;
      }
      // 2) Treat `id` as a tab id: activate that tab.
      if (tabs.some((t) => t.id === id)) {
        ws.setActiveTab(id);
        return;
      }
      // 3) Unknown id: best-effort no-op (the session may live in another window).
      return;
    }

    default:
      // Not an Organization-tier mutation we apply here; ignore.
      return;
  }
}

/**
 * Subscribe to `control://apply` and apply each forwarded command. Returns a
 * promise of the Tauri unlisten fn (unused at the module top level — the
 * subscription lives for the lifetime of the window). Guarded so importing this
 * module outside a Tauri webview (e.g. a test/SSR context) is a safe no-op, and
 * inert in a satellite window (see the module header).
 */
export function startControlBridge(): void {
  if (typeof window === "undefined") return;
  if (isSatelliteWindow()) return;
  void listen<ControlApply>(CONTROL_APPLY_EVENT, (ev) => {
    const payload = ev.payload;
    if (!payload || typeof payload.command !== "string") return;
    try {
      applyControl(payload.command, payload.args ?? null);
    } catch {
      // Never let a malformed forward crash the listener.
    }
  }).catch(() => {
    // `listen` rejects when not running under Tauri — safe to ignore.
  });

  startTabReporter();
}

/**
 * Up-sync USER-originated workspace-tab changes to the core's AUTHORITATIVE tab
 * registry (TASK C / #22, headless-org) whenever the layout or active tab
 * changes. Reports are SERIALIZED (at most one in flight; trailing changes
 * coalesce into one follow-up) and carry `baseSeq`; on a stale rejection the
 * returned authoritative snapshot is adopted - the rare concurrent local change
 * loses to the server, by design. Failures (e.g. not under Tauri) are swallowed.
 */
function startTabReporter(): void {
  let inFlight = false;
  let pending = false;

  const report = (): void => {
    if (adoptingRegistry) return; // never echo a server-applied snapshot back up
    if (inFlight) {
      pending = true;
      return;
    }
    inFlight = true;
    const { tabs, activeTabId } = useWorkspace.getState();
    const payload = tabs.map((t) => ({
      id: t.id,
      name: t.name,
      tileIds: t.order,
    }));
    void import("./client")
      .then((m) => m.reportWorkspaceTabs(payload, activeTabId, lastSeq))
      .then((res: TabReportResult | void) => {
        if (res && typeof res.seq === "number") {
          lastSeq = res.seq;
          if (res.stale && Array.isArray(res.tabs)) {
            // A server mutation raced this report: converge on the registry.
            adoptingRegistry = true;
            try {
              useWorkspace.getState().adoptRegistry(res.tabs);
            } finally {
              adoptingRegistry = false;
            }
          }
        }
      })
      .catch(() => {
        // Not under Tauri, or the command isn't available — safe to ignore.
      })
      .finally(() => {
        inFlight = false;
        if (pending) {
          pending = false;
          report();
        }
      });
  };
  // Fire on every tab-layout change (identity of the `tabs` array changes on any
  // add/remove/rename/reorder/move) and on active-tab switches (the registry
  // mirrors the active tab for default spawn placement + focus proofs).
  useWorkspace.subscribe((state, prev) => {
    if (state.tabs !== prev.tabs || state.activeTabId !== prev.activeTabId) {
      report();
    }
  });
  // Initial snapshot so list_tabs reflects the default tab before any change.
  report();
}

// Run the subscription on import (side-effect module, mirroring themeBootstrap).
startControlBridge();
