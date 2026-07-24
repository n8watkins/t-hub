// Git-worktree slice (WS-4): atomically create a worktree + tab + terminal
// (addWorktreeWorkspace), and remove a worktree after the backend's safety
// preflight admits it, detaching any UI tile rooted inside it first
// (removeWorktreeWorkspace). Action bodies are verbatim; cross-slice calls
// (ensureTab / addTab / renameTab / addToTab / detachTile) go through `get()`. No
// store closure helpers are needed here, so `deps` is unused.
import {
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
} from "./internal";

export const createWorktreesSlice = (
  _set: StoreSet,
  get: StoreGet,
  _deps: SliceDeps,
): Pick<WorkspaceState, "addWorktreeWorkspace" | "removeWorktreeWorkspace"> => {
  void _set;
  void _deps;

  return {
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
        const { gitWorktreeAdd } = await import("../../ipc/git");
        // Let a git failure reject — the caller (FilePanel) surfaces the message.
        await gitWorktreeAdd(repo, path, branch?.trim() || undefined);
      }

      // 2) Resolve the target tab, then spawn a terminal in it.
      try {
        const { spawnTerminal } = await import("../../ipc/client");
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

      // 1) Ask the backend for the complete authoritative removal verdict BEFORE
      // changing this window. The current implementation fails closed here.
      const { gitWorktreeRemovalPreflight, gitWorktreeRemove } = await import(
        "../../ipc/git"
      );
      await gitWorktreeRemovalPreflight(path);

      // 2) Any matching UI tile is now stale rather than live. Detach it before
      // Git removes the directory. We match on a path-segment boundary so
      // `/x/wt` does not match `/x/wt-other`.
      const { terminals } = get();
      const prefix = path + "/";
      const victims = Object.values(terminals)
        .filter((t) => {
          const cwd = (t.cwd ?? "").replace(/\/+$/, "");
          return cwd === path || cwd.startsWith(prefix);
        })
        .map((t) => t.id);
      for (const id of victims) get().detachTile(id);

      // 3) Remove the worktree. Once the unified service is activated, the backend
      // recomputes the complete verdict immediately before Git mutation.
      await gitWorktreeRemove(repo, path, force);
    },
  };
};
