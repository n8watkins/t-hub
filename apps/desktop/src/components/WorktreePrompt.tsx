// The worktree branch-name prompt (WS-9c). Mounted once at the app root next to
// the CommandPalette.
//
// Opened by the `newWorktreeWorkspace` command (default Ctrl/Cmd+B w). The flow:
//   1. The executor reads the focused tile's LIVE cwd and opens this prompt.
//   2. The user types a branch name; Enter submits, Esc cancels.
//   3. We resolve the worktree TARGET from (cwd, branch) via resolveWorktreeTarget:
//        - no-repo  -> show the WS-9d REPO PICKER: a selectable list of candidate
//                      anchor cwds (open tiles + recent sessions). Picking one sets
//                      it as the anchor and re-runs the SAME branch -> resolve ->
//                      addWorktreeWorkspace flow. (A manual "pick a repo…" link is
//                      always available too.)
//        - ok       -> addWorktreeWorkspace(repoRoot, worktreePath, branch);
//                      success closes the prompt. A thrown git error is shown
//                      inline (the prompt stays open so the user can retry/cancel).
//                      If the store created the worktree but couldn't spawn a tile
//                      (it returns null), we keep the prompt open with a notice.
//
// Styling + the open-state store + the opener-registration pattern deliberately
// MIRROR CommandPalette.tsx so the executor can open us without an import cycle
// (the executor imports nothing from here; we register a callback on mount).
import { useCallback, useEffect, useRef, useState } from "react";
import { create } from "zustand";
import { useWorkspace } from "../store/workspace";
import { resolveWorktreeTarget, posixBasename } from "../lib/worktreeTarget";
import { candidateRepoCwds } from "../lib/recentRepos";
import { registerWorktreePromptOpener } from "../lib/keymapExecutor";

// --- Open-state store -------------------------------------------------------
// A tiny store (same shape as the palette's) so the executor can open the prompt
// imperatively, carrying the focused tile's live cwd captured at trigger time.
interface PromptState {
  open: boolean;
  /** The focused tile's live cwd at the moment the command fired (anchor for
   *  repo resolution). Undefined when there was no focused tile. */
  cwd: string | undefined;
  openWith: (cwd: string | undefined) => void;
  close: () => void;
}
const usePrompt = create<PromptState>((set) => ({
  open: false,
  cwd: undefined,
  openWith: (cwd) => set({ open: true, cwd }),
  close: () => set({ open: false }),
}));

export function WorktreePrompt() {
  const open = usePrompt((s) => s.open);
  const cwd = usePrompt((s) => s.cwd);
  const close = usePrompt((s) => s.close);

  const [branch, setBranch] = useState("");
  // Inline status: an error message, or a transient "creating…" note.
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // --- WS-9d repo picker -----------------------------------------------------
  // When the focused tile has no repo (a `~` scratch shell / empty workspace) we
  // can't anchor a worktree path, so we ask instead of guessing. `picking` flips
  // the modal to the candidate list; selecting one sets `pickedCwd` (the override
  // anchor) and re-runs submit. `pickedCwd` also lets a user RE-anchor via the
  // always-available "pick a repo…" link even when the focused tile IS a repo.
  const [picking, setPicking] = useState(false);
  const [candidates, setCandidates] = useState<string[]>([]);
  const [pickSel, setPickSel] = useState(0);
  const [pickedCwd, setPickedCwd] = useState<string | null>(null);
  // The effective anchor: an explicit pick wins over the focused tile's cwd.
  const anchorCwd = pickedCwd ?? cwd;

  // The executor opens the prompt through this opener; register on mount (same
  // pattern as registerPaletteOpener). The opener receives the live cwd.
  useEffect(() => {
    registerWorktreePromptOpener((c) => usePrompt.getState().openWith(c));
    return () => registerWorktreePromptOpener(null);
  }, []);

  // Reset transient state each time the prompt opens, and focus the input.
  useEffect(() => {
    if (!open) return;
    setBranch("");
    setError(null);
    setBusy(false);
    setPicking(false);
    setCandidates([]);
    setPickSel(0);
    setPickedCwd(null);
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [open]);

  // Switch to the repo-picker sub-view: load candidate anchor cwds (open tiles +
  // recent sessions, deduped, most-relevant first) and show the list. Cheap — no
  // per-candidate git probe; the root is resolved on selection via the resolver.
  const openPicker = useCallback(async () => {
    if (busy) return;
    setError(null);
    setPicking(true);
    setPickSel(0);
    const list = await candidateRepoCwds();
    setCandidates(list);
  }, [busy]);

  const submit = useCallback(async () => {
    const name = branch.trim();
    if (!name || busy) return;
    setBusy(true);
    setError(null);
    try {
      const t = await resolveWorktreeTarget(anchorCwd ?? "", name);
      if (t.kind === "no-repo") {
        // No repo in the anchor cwd → ask, don't guess. Drop into the WS-9d repo
        // picker (keeping the typed branch); the user picks an anchor and we re-run.
        setBusy(false);
        void openPicker();
        return;
      }
      const id = await useWorkspace
        .getState()
        .addWorktreeWorkspace(t.repoRoot, t.worktreePath, t.branch);
      // The store git-creates the worktree, THEN spawns a tile — and a tile-spawn
      // failure is swallowed there (logged + returns null) rather than thrown. So a
      // falsy return means "worktree created on disk, but no terminal/tab opened":
      // don't silently close as success. Keep the prompt open with a clear notice.
      if (!id) {
        setError("Worktree created, but couldn't open a terminal in it.");
        setBusy(false);
        return;
      }
      close();
    } catch (err) {
      // Surface git's (or any) failure inline; keep the prompt open to retry.
      setError(err instanceof Error ? err.message : String(err));
      setBusy(false);
    }
  }, [branch, busy, anchorCwd, close, openPicker]);

  // The user picked an anchor cwd from the list: set it as the override anchor,
  // leave the picker, and re-run submit with the SAME typed branch. resolve runs
  // against the picked cwd now; if THAT isn't a repo either we land back here.
  const pickCandidate = useCallback(
    (c: string) => {
      setPickedCwd(c);
      setPicking(false);
      // submit reads `anchorCwd = pickedCwd ?? cwd`; defer a tick so the state
      // update lands before we resolve against the freshly-picked anchor.
      requestAnimationFrame(() => void submit());
    },
    [submit],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      // --- Picker sub-view: arrow-key nav + Enter to select, Esc to go back. ---
      if (picking) {
        if (e.key === "Escape") {
          e.preventDefault();
          setPicking(false); // back to the branch input (Esc again closes)
        } else if (e.key === "ArrowDown") {
          e.preventDefault();
          setPickSel((s) =>
            candidates.length ? (s + 1) % candidates.length : 0,
          );
        } else if (e.key === "ArrowUp") {
          e.preventDefault();
          setPickSel((s) =>
            candidates.length
              ? (s - 1 + candidates.length) % candidates.length
              : 0,
          );
        } else if (e.key === "Enter") {
          e.preventDefault();
          const hit = candidates[pickSel];
          if (hit) pickCandidate(hit);
        }
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      } else if (e.key === "Enter") {
        e.preventDefault();
        void submit();
      }
    },
    [picking, candidates, pickSel, pickCandidate, close, submit],
  );

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[55] flex items-start justify-center p-6 pt-[12vh]"
      onMouseDown={close}
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        className="flex w-[480px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
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
          {picking ? "Pick a repo" : "New worktree workspace"}
        </div>

        {picking ? (
          // --- WS-9d repo picker sub-view -------------------------------------
          // A selectable list of candidate anchor cwds. Label = basename, subtitle
          // = full path. Selecting one re-runs the create flow against that repo.
          <>
            <div
              className="th-scroll max-h-[40vh] min-h-0 overflow-y-auto py-1"
              style={{ borderBottom: "1px solid var(--th-border)" }}
            >
              {candidates.length === 0 ? (
                <div
                  className="px-3 py-6 text-center text-sm"
                  style={{ color: "var(--th-fg-muted)" }}
                >
                  No recent repos to pick from
                </div>
              ) : (
                candidates.map((c, i) => {
                  const isSel = i === pickSel;
                  return (
                    <div
                      key={c}
                      data-idx={i}
                      onMouseMove={() => setPickSel(i)}
                      onMouseDown={(e) => {
                        e.preventDefault();
                        pickCandidate(c);
                      }}
                      className="mx-1 cursor-pointer rounded px-2.5 py-2"
                      style={{
                        backgroundColor: isSel
                          ? "var(--th-tile-bg)"
                          : "transparent",
                      }}
                    >
                      <div
                        className="truncate text-sm"
                        style={{ color: "var(--th-fg)" }}
                      >
                        {posixBasename(c) || c}
                      </div>
                      <div
                        className="truncate text-xs"
                        style={{ color: "var(--th-fg-muted)" }}
                      >
                        {c}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
            <div
              className="flex shrink-0 items-center justify-between px-3 py-1.5 text-xs"
              style={{ color: "var(--th-fg-muted)" }}
            >
              <span>↑↓ navigate · Enter pick · Esc back</span>
              <span>branch the picked repo</span>
            </div>
          </>
        ) : (
          // --- Branch-input view ----------------------------------------------
          <>
            {/* Branch input */}
            <div
              className="shrink-0 border-b px-3 py-2.5"
              style={{ borderColor: "var(--th-border)" }}
            >
              <input
                ref={inputRef}
                value={branch}
                onChange={(e) => {
                  setBranch(e.target.value);
                  if (error) setError(null);
                }}
                placeholder="Branch name… (e.g. login-fix)"
                disabled={busy}
                className="w-full bg-transparent text-sm outline-none placeholder:opacity-60"
                style={{ color: "var(--th-fg)" }}
                spellCheck={false}
                autoComplete="off"
              />
            </div>

            {/* Anchor line: which repo this will branch, + a "pick a repo…" link.
                When the user has picked an override anchor, show its basename. */}
            <div
              className="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-1.5 text-xs"
              style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
            >
              <span className="min-w-0 truncate">
                {pickedCwd
                  ? `Repo: ${posixBasename(pickedCwd) || pickedCwd}`
                  : "Repo: the focused tile's repo"}
              </span>
              <button
                type="button"
                onMouseDown={(e) => {
                  e.preventDefault();
                  void openPicker();
                }}
                disabled={busy}
                className="shrink-0 rounded px-1.5 py-0.5 transition-colors hover:bg-neutral-700/40"
                style={{ color: "var(--th-accent)" }}
                title="Pick a different repo to branch"
              >
                pick a repo…
              </button>
            </div>

            {/* Inline status (error / creating). */}
            {(error || busy) && (
              <div
                className="shrink-0 border-b px-3 py-2 text-xs"
                style={{
                  borderColor: "var(--th-border)",
                  color: error ? "var(--th-accent)" : "var(--th-fg-muted)",
                }}
              >
                {error ?? "Creating worktree…"}
              </div>
            )}

            {/* Footer hint */}
            <div
              className="flex shrink-0 items-center justify-between px-3 py-1.5 text-xs"
              style={{ color: "var(--th-fg-muted)" }}
            >
              <span>Enter create · Esc cancel</span>
              <span>sibling folder · branch the focused repo</span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
