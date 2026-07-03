// Frontend bridge for the MCP control channel's Organization-tier actions.
//
// The backend control listener (`src-tauri/src/control.rs`) accepts + audits the
// Organization tools `focus_session`, `move_tile`, `rename_tab` and then forwards
// the accepted `{command, args}` to the frontend by emitting a Tauri
// `control://apply` event (wired in `lib.rs`). This module subscribes to that
// event and applies the mutation by calling into the workspace store — so an MCP
// client (Claude) driving those tools actually reorganizes the live UI.
//
// It is a SIDE-EFFECT module (no React render): it sets up its subscription on
// import. It is pulled into the bundle by a single <script type="module"> in
// index.html, mirroring how `themeBootstrap` is loaded — so it needs no hook in
// App.tsx / main.tsx. Importing it outside a webview must not throw, so the
// Tauri `listen` is imported lazily and guarded.
import { listen } from "@tauri-apps/api/event";
import { useWorkspace } from "../store/workspace";

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
      // MCP schema: { terminalId, tabId } -> move the tile to another tab.
      // Also accept a `targetId`/`targetTerminalId` for a within-tab reorder
      // (the store's moveTile(id, targetId)), so either mapping works.
      const terminalId = str(args, "terminalId") ?? str(args, "id");
      if (!terminalId) return;
      const targetId = str(args, "targetId") ?? str(args, "targetTerminalId");
      const tabId = str(args, "tabId");
      if (targetId) ws.moveTile(terminalId, targetId);
      else if (tabId) ws.moveTileToTab(terminalId, tabId);
      return;
    }

    case "rename_tab": {
      // MCP schema: { tabId, name }.
      const tabId = str(args, "tabId") ?? str(args, "id");
      const name = str(args, "name");
      if (tabId && name) ws.renameTab(tabId, name);
      return;
    }

    case "new_tab": {
      // Control/MCP path (TASK C / #22): the CORE mints the tab id (so it can
      // return it to the caller) and forwards it here as `id`; adopt it verbatim so
      // the live tab's id matches what the caller holds and can address later.
      // Fall back to a local create if an older core sent no id.
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

    case "spawn_terminal": {
      // Process-changing (PRD §11.2): the backend accepted the confirmation-gated
      // spawn and forwarded it here (#17). Spawn a REAL tile via the normal
      // spawnTerminal IPC — so the session is a UI-adopted terminal, not the
      // untracked tmux session the old gate refused. Placed in the tab active at
      // call time (captured synchronously inside spawnWorkspaceTerminal).
      const cwd = str(args, "cwd");
      const name = str(args, "name");
      const shell = str(args, "shell");
      // T-B: the socket spawn's optional startup command (e.g. the native
      // resume flow's `claude --resume <id>`) rides the same presets field the
      // "+" menu uses — dropped here before T-B, which would strand a
      // control-side resume in a plain shell.
      const startupCommand = str(args, "startupCommand");
      void ws
        .spawnWorkspaceTerminal({ cwd, name, shell, startupCommand })
        .catch((e) => console.error("spawn_terminal failed", e));
      return;
    }

    case "focus_tab": {
      // MCP schema: { tabId } -> activate that workspace tab.
      const tabId = str(args, "tabId") ?? str(args, "id");
      if (tabId && ws.tabs.some((t) => t.id === tabId)) ws.setActiveTab(tabId);
      return;
    }

    case "add_worktree_workspace": {
      // WS-4: the backend already created the git worktree (create_worktree ran
      // `git worktree add` before forwarding), so the store skips its own
      // gitWorktreeAdd via `alreadyCreated`. We just open the tab + spawn a
      // terminal in the worktree dir. Best-effort: a spawn failure is swallowed
      // by addWorktreeWorkspace (logged, returns null) so the listener never throws.
      const worktreePath = str(args, "worktreePath") ?? str(args, "worktree_path");
      if (!worktreePath) return;
      const repoRoot = str(args, "repoRoot") ?? str(args, "repo_root") ?? "";
      const branch = str(args, "branch");
      const tabName = str(args, "tabName") ?? str(args, "tab_name");
      // TASK C / #22: the backend resolved the target tab by NAME and forwards its
      // id, so the tile lands in that tab (reused or created by id+name) rather than
      // the focused workspace.
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
 * module outside a Tauri webview (e.g. a test/SSR context) is a safe no-op.
 */
export function startControlBridge(): void {
  if (typeof window === "undefined") return;
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
 * Report the live workspace-tab layout up to the core's addressable tab registry
 * (TASK C / #22) whenever it changes, so the control/MCP `list_tabs` mirrors the
 * UI — including UI-created tabs and real tile membership. The frontend is the
 * source of truth for tabs; this is the write half of the mirror. We report on
 * mount and on every `tabs`-array change; a report failure (e.g. not under Tauri)
 * is swallowed.
 */
function startTabReporter(): void {
  const report = (): void => {
    const { tabs } = useWorkspace.getState();
    const payload = tabs.map((t) => ({
      id: t.id,
      name: t.name,
      tileIds: t.order,
    }));
    void import("./client")
      .then((m) => m.reportWorkspaceTabs(payload))
      .catch(() => {
        // Not under Tauri, or the command isn't available — safe to ignore.
      });
  };
  // Fire on every tab-layout change (identity of the `tabs` array changes on any
  // add/remove/rename/reorder/move).
  useWorkspace.subscribe((state, prev) => {
    if (state.tabs !== prev.tabs) report();
  });
  // Initial snapshot so list_tabs reflects the default tab before any change.
  report();
}

// Run the subscription on import (side-effect module, mirroring themeBootstrap).
startControlBridge();
