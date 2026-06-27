// Self-contained lifecycle keybindings + their confirm UI (feat/lifecycle).
//
// T-Hub distinguishes three terminal-lifecycle actions; this module owns the
// keyboard surface for the two destructive-or-adjacent ones and the confirm
// dialog the destructive one needs, so App.tsx only mounts <LifecycleKeybinds/>
// (one line) and never has to know the details:
//
//   • Ctrl/Cmd+W        — DETACH the focused tile (keep the tmux session alive).
//                         Already bound in Canvas.tsx → closeFocused(); listed
//                         here for documentation. We deliberately do NOT rebind
//                         it, to avoid double-detaching the same tile.
//   • Ctrl/Cmd+Shift+W  — DELETE the focused tile's session (kill tmux for good).
//                         Destructive, so it opens a themed confirm first.
//
// Chosen to be ergonomic and collision-free against the existing global binds
// (Ctrl/Cmd+T new, Ctrl/Cmd+W detach, Ctrl/Cmd+B sidebar, Ctrl/Cmd+Tab cycle,
// Ctrl/Cmd+1..9 tab jump, Ctrl/Cmd+=/-/0 zoom, Ctrl/Cmd+, settings): Shift+W
// reads as "the stronger W" — close vs. close-and-delete.
//
// The component renders nothing but a (conditionally shown) ConfirmDialog, so it
// is safe to mount once at the app root.
import { useEffect, useState } from "react";
import { useWorkspace } from "../store/workspace";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { isEditableTarget } from "../components/Canvas";

export function LifecycleKeybinds() {
  const deleteTerminal = useWorkspace((s) => s.deleteTerminal);
  // The id queued for a confirmed delete (null = no confirm showing). Captured at
  // keypress time so a focus change while the dialog is open can't retarget it.
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't fire a destructive lifecycle keybind while the user is typing in a
      // real text field (a form input/textarea/select or contentEditable host) —
      // e.g. a rename-tab or worktree-path input. isEditableTarget deliberately
      // excludes xterm's offscreen helper textarea, so Ctrl/Cmd+Shift+W still
      // works while a terminal is focused.
      if (isEditableTarget(e.target)) return;
      const mod = e.ctrlKey || e.metaKey;
      if (!mod || e.altKey) return;
      // Ctrl/Cmd+Shift+W → delete the focused terminal's session (with confirm).
      // `e.code === "KeyW"` is layout-robust and survives the Shift modifier
      // (e.key can report an uppercase "W" or a dead value depending on layout).
      if (e.shiftKey && e.code === "KeyW") {
        e.preventDefault();
        const id = useWorkspace.getState().focusedId;
        if (id) setPendingDelete(id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <ConfirmDialog
      open={pendingDelete !== null}
      title="Delete session?"
      body={
        <>
          This permanently kills the tmux session{" "}
          <span className="font-mono" style={{ color: "var(--th-fg)" }}>
            {pendingDelete}
          </span>{" "}
          and everything running in it. This can't be undone. To just close the
          tile and keep the session running, use Detach (Ctrl/Cmd+W) instead.
        </>
      }
      confirmLabel="Delete session"
      onConfirm={() => {
        if (pendingDelete) deleteTerminal(pendingDelete);
        setPendingDelete(null);
      }}
      onCancel={() => setPendingDelete(null)}
    />
  );
}
