// The worktree branch-name prompt (WS-9c). Mounted once at the app root next to
// the CommandPalette.
//
// Opened by the `newWorktreeWorkspace` command (default Ctrl/Cmd+B w). The flow:
//   1. The executor reads the focused tile's LIVE cwd and opens this prompt.
//   2. The user types a branch name; Enter submits, Esc cancels.
//   3. We resolve the worktree TARGET from (cwd, branch) via resolveWorktreeTarget:
//        - no-repo  -> inline message (the 9d repo picker will replace this);
//                      the prompt stays open so the user can cancel cleanly.
//        - ok       -> addWorktreeWorkspace(repoRoot, worktreePath, branch);
//                      success closes the prompt. A thrown git error is shown
//                      inline (the prompt stays open so the user can retry/cancel).
//
// Styling + the open-state store + the opener-registration pattern deliberately
// MIRROR CommandPalette.tsx so the executor can open us without an import cycle
// (the executor imports nothing from here; we register a callback on mount).
import { useCallback, useEffect, useRef, useState } from "react";
import { create } from "zustand";
import { useWorkspace } from "../store/workspace";
import { resolveWorktreeTarget } from "../lib/worktreeTarget";
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
  // Inline status: an error/no-repo message, or a transient "creating…" note.
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

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
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [open]);

  const submit = useCallback(async () => {
    const name = branch.trim();
    if (!name || busy) return;
    setBusy(true);
    setError(null);
    try {
      const t = await resolveWorktreeTarget(cwd ?? "", name);
      if (t.kind === "no-repo") {
        // 9d repo picker will replace this; for now message + keep prompt open.
        setError("Not in a git repo — pick a repo (coming soon)");
        setBusy(false);
        return;
      }
      await useWorkspace
        .getState()
        .addWorktreeWorkspace(t.repoRoot, t.worktreePath, t.branch);
      close();
    } catch (err) {
      // Surface git's (or any) failure inline; keep the prompt open to retry.
      setError(err instanceof Error ? err.message : String(err));
      setBusy(false);
    }
  }, [branch, busy, cwd, close]);

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      } else if (e.key === "Enter") {
        e.preventDefault();
        void submit();
      }
    },
    [close, submit],
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
          New worktree workspace
        </div>

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

        {/* Inline status (error / no-repo / creating). */}
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
      </div>
    </div>
  );
}
