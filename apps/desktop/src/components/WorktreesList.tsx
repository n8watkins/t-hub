// The worktrees list / re-open modal (WS-9e). Mounted once at the app root next
// to the CommandPalette + WorktreePrompt.
//
// Opened by the `openWorktreesList` command (default Ctrl/Cmd+B l). The flow:
//   1. The executor reads the focused tile's LIVE cwd and opens this modal.
//   2. On open we resolve the repo via gitWorktreeList(focusedCwd): the MAIN
//      root is the first `isLinked === false` entry (same rule as the resolver);
//      no such entry → a "not in a repo" empty state.
//   3. Each worktree row shows branch + path + a main/linked tag, with actions:
//        - Open   → addWorktreeWorkspace(repoRoot, path, branch, {alreadyCreated})
//                   (re-open; does NOT re-run `git worktree add`), then close.
//        - Remove → confirm, then removeWorktreeWorkspace(repoRoot, path); refresh.
//
// Styling + the open-state store + the opener-registration pattern deliberately
// MIRROR CommandPalette.tsx / WorktreePrompt.tsx so the executor can open us
// without an import cycle (the executor imports nothing from here; we register a
// callback on mount).
import { useCallback, useEffect, useState } from "react";
import { create } from "zustand";
import { useWorkspace } from "../store/workspace";
import { gitWorktreeList, type WorktreeInfo } from "../ipc/git";
import { registerWorktreesListOpener } from "../lib/keymapExecutor";

// --- Open-state store -------------------------------------------------------
// A tiny store (same shape as the prompt's) so the executor can open the modal
// imperatively, carrying the focused tile's live cwd captured at trigger time.
interface ListState {
  open: boolean;
  /** The focused tile's live cwd at the moment the command fired (the repo to
   *  list worktrees for). Undefined when there was no focused tile. */
  cwd: string | undefined;
  openWith: (cwd: string | undefined) => void;
  close: () => void;
}
const useList = create<ListState>((set) => ({
  open: false,
  cwd: undefined,
  openWith: (cwd) => set({ open: true, cwd }),
  close: () => set({ open: false }),
}));

export function WorktreesList() {
  const open = useList((s) => s.open);
  const cwd = useList((s) => s.cwd);
  const close = useList((s) => s.close);

  // Resolved repo root (the non-linked entry) + its worktrees, loaded on open.
  const [repoRoot, setRepoRoot] = useState<string | null>(null);
  const [rows, setRows] = useState<WorktreeInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Which row's Remove is awaiting confirmation (path), or null. A busy path is
  // mid-action (Open/Remove) so we disable that row's buttons.
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [busyPath, setBusyPath] = useState<string | null>(null);
  const [selected, setSelected] = useState(0);

  // The executor opens the modal through this opener; register on mount (same
  // pattern as registerWorktreePromptOpener). The opener receives the live cwd.
  useEffect(() => {
    registerWorktreesListOpener((c) => useList.getState().openWith(c));
    return () => registerWorktreesListOpener(null);
  }, []);

  // Load (or reload) the worktree list for the current anchor cwd. The repo root
  // is the first non-linked entry (git lists the main worktree first) — the same
  // rule resolveWorktreeTarget uses. No such entry → "not in a repo".
  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await gitWorktreeList(cwd ?? "");
      const root = list.find((w) => !w.isLinked)?.path ?? null;
      setRepoRoot(root);
      setRows(root ? list : []);
      setSelected(0);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setRepoRoot(null);
      setRows([]);
    } finally {
      setLoading(false);
    }
  }, [cwd]);

  // Resolve + reset transient state each time the modal opens.
  useEffect(() => {
    if (!open) return;
    setConfirmRemove(null);
    setBusyPath(null);
    void reload();
  }, [open, reload]);

  const onOpenWorktree = useCallback(
    async (w: WorktreeInfo) => {
      if (!repoRoot || busyPath) return;
      setBusyPath(w.path);
      setError(null);
      try {
        // Re-open: alreadyCreated skips `git worktree add` (the dir exists).
        const id = await useWorkspace
          .getState()
          .addWorktreeWorkspace(repoRoot, w.path, w.branch ?? undefined, {
            alreadyCreated: true,
          });
        if (!id) {
          // Worktree exists but the tile spawn failed (the store swallows it).
          setError("Couldn't open a terminal in that worktree.");
          setBusyPath(null);
          return;
        }
        close();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
        setBusyPath(null);
      }
    },
    [repoRoot, busyPath, close],
  );

  const onRemoveWorktree = useCallback(
    async (w: WorktreeInfo) => {
      if (!repoRoot || busyPath) return;
      setBusyPath(w.path);
      setConfirmRemove(null);
      setError(null);
      try {
        await useWorkspace
          .getState()
          .removeWorktreeWorkspace(repoRoot, w.path);
        // Refresh so the removed row drops out of the list.
        await reload();
      } catch (err) {
        // git refuses dirty worktrees without --force; surface its message.
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusyPath(null);
      }
    },
    [repoRoot, busyPath, reload],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        // Esc backs out of a pending remove-confirm first, else closes.
        if (confirmRemove) setConfirmRemove(null);
        else close();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => (rows.length ? (s + 1) % rows.length : 0));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) =>
          rows.length ? (s - 1 + rows.length) % rows.length : 0,
        );
      } else if (e.key === "Enter") {
        e.preventDefault();
        const hit = rows[selected];
        if (hit) void onOpenWorktree(hit);
      }
    },
    [confirmRemove, close, rows, selected, onOpenWorktree],
  );

  if (!open) return null;

  const noRepo = !loading && !error && repoRoot === null;

  return (
    <div
      className="fixed inset-0 z-[55] flex items-start justify-center p-6 pt-[12vh]"
      onMouseDown={close}
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        className="flex max-h-[64vh] w-[560px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
      >
        {/* Title */}
        <div
          className="shrink-0 border-b px-3 py-2 text-sm"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
        >
          Worktrees
        </div>

        {/* Inline error (git failure). */}
        {error && (
          <div
            className="shrink-0 border-b px-3 py-2 text-xs"
            style={{ borderColor: "var(--th-border)", color: "var(--th-accent)" }}
          >
            {error}
          </div>
        )}

        {/* Body: loading / empty / list. */}
        <div className="th-scroll min-h-0 flex-1 overflow-y-auto py-1">
          {loading ? (
            <div
              className="px-3 py-6 text-center text-sm"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Loading worktrees…
            </div>
          ) : noRepo ? (
            <div
              className="px-3 py-6 text-center text-sm"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Not in a git repo — focus a terminal inside one to list its
              worktrees.
            </div>
          ) : rows.length === 0 ? (
            <div
              className="px-3 py-6 text-center text-sm"
              style={{ color: "var(--th-fg-muted)" }}
            >
              No worktrees
            </div>
          ) : (
            rows.map((w, i) => {
              const isSel = i === selected;
              const isBusy = busyPath === w.path;
              const isConfirming = confirmRemove === w.path;
              return (
                <div
                  key={w.path}
                  data-idx={i}
                  onMouseMove={() => setSelected(i)}
                  className="mx-1 flex items-center justify-between gap-3 rounded px-2.5 py-2"
                  style={{
                    backgroundColor: isSel ? "var(--th-tile-bg)" : "transparent",
                  }}
                >
                  <div className="min-w-0">
                    <div
                      className="flex items-center gap-2 truncate text-sm"
                      style={{ color: "var(--th-fg)" }}
                    >
                      <span className="truncate">
                        {w.branch ?? "(detached)"}
                      </span>
                      <span
                        className="shrink-0 rounded border px-1 py-0.5 text-[10px] uppercase tracking-wide"
                        style={{
                          borderColor: "var(--th-border)",
                          color: "var(--th-fg-muted)",
                        }}
                      >
                        {w.isLinked ? "linked" : "main"}
                      </span>
                    </div>
                    <div
                      className="truncate text-xs"
                      style={{ color: "var(--th-fg-muted)" }}
                    >
                      {w.path}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <button
                      type="button"
                      onMouseDown={(e) => {
                        e.preventDefault();
                        void onOpenWorktree(w);
                      }}
                      disabled={isBusy}
                      className="rounded border px-1.5 py-0.5 text-xs transition-colors hover:bg-neutral-700/40 disabled:opacity-50"
                      style={{
                        borderColor: "var(--th-border)",
                        color: "var(--th-fg)",
                      }}
                      title="Open this worktree in a new tab"
                    >
                      Open
                    </button>
                    {/* The MAIN worktree can't be removed (`git worktree remove`
                        refuses it); only linked worktrees get a Remove action. */}
                    {w.isLinked &&
                      (isConfirming ? (
                        <button
                          type="button"
                          onMouseDown={(e) => {
                            e.preventDefault();
                            void onRemoveWorktree(w);
                          }}
                          disabled={isBusy}
                          className="rounded border px-1.5 py-0.5 text-xs transition-colors disabled:opacity-50"
                          style={{
                            borderColor: "var(--th-accent)",
                            color: "var(--th-accent)",
                          }}
                          title="Confirm removing this worktree"
                        >
                          confirm remove
                        </button>
                      ) : (
                        <button
                          type="button"
                          onMouseDown={(e) => {
                            e.preventDefault();
                            setConfirmRemove(w.path);
                          }}
                          disabled={isBusy}
                          className="rounded px-1.5 py-0.5 text-xs transition-colors hover:bg-neutral-700/40 disabled:opacity-50"
                          style={{ color: "var(--th-fg-muted)" }}
                          title="Remove this worktree (deletes the folder)"
                        >
                          Remove
                        </button>
                      ))}
                  </div>
                </div>
              );
            })
          )}
        </div>

        {/* Footer hint */}
        <div
          className="flex shrink-0 items-center justify-between border-t px-3 py-1.5 text-xs"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
        >
          <span>↑↓ navigate · Enter open · Esc close</span>
          <span>re-open or remove a worktree</span>
        </div>
      </div>
    </div>
  );
}
