// Recall slice: re-spawn + resume a past Claude session into the active tab. It
// reuses the SAME spawn path as Canvas's "+" menu (`spawnTerminal` IPC then
// `addAfterFocused`) so there is exactly one way a tile gets created. The in-flight
// recall guard (#7) stays module-level in workspace.ts and is handed in via
// `deps.recallInFlight`. Action body is verbatim; `addAfterFocused` is reached
// through `get()`.
import {
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
} from "./internal";

export const createRecallSlice = (
  _set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<WorkspaceState, "recall"> => {
  void _set;
  const { recallInFlight } = deps;

  return {
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
        const { spawnTerminal } = await import("../../ipc/client");
        const { useSettings } = await import("../settings");
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
  };
};
