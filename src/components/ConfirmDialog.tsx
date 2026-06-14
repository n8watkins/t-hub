// A small, reusable themed confirmation modal for destructive actions (the
// "Close & delete session" path in particular). It reads the `--th-*` theme
// tokens like every other surface, so it tracks the active theme; nothing is
// rendered unless `open` is true.
//
// Behaviour:
//   - A dimmed scrim covers the viewport; clicking it cancels.
//   - Escape cancels; Enter confirms (so the keyboard-driven destructive
//     keybinds can be confirmed without reaching for the mouse).
//   - The confirm button autofocuses so the dialog is operable from the
//     keyboard, and is styled to read as destructive (`danger`) by default.
//
// It is deliberately stateless/controlled: the caller owns the open flag and the
// confirm/cancel callbacks, so one dialog instance can be reused for any
// destructive action (kill a session, etc.).
import { useEffect, useRef } from "react";

export interface ConfirmDialogProps {
  /** Whether the dialog is shown. Renders nothing when false. */
  open: boolean;
  /** Bold heading line (e.g. "Delete session?"). */
  title: string;
  /** Explanatory body — what will happen and that it can't be undone. */
  body?: React.ReactNode;
  /** Confirm button label (default "Delete"). */
  confirmLabel?: string;
  /** Cancel button label (default "Cancel"). */
  cancelLabel?: string;
  /** When true (default) the confirm button is styled as a destructive action. */
  danger?: boolean;
  /** Called when the user confirms (button, or Enter). */
  onConfirm: () => void;
  /** Called when the user cancels (button, scrim click, or Escape). */
  onCancel: () => void;
}

export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel = "Delete",
  cancelLabel = "Cancel",
  danger = true,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const confirmRef = useRef<HTMLButtonElement | null>(null);

  // Esc cancels, Enter confirms — only while open. Captured at the window so it
  // fires regardless of focus, and stops propagation so an underlying surface
  // (e.g. a tile's own Escape handler) doesn't also react.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCancel();
      } else if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        onConfirm();
      }
    };
    // Capture phase so we win over component-level handlers while the modal is up.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onConfirm, onCancel]);

  // Move focus onto the confirm button when the dialog opens so it's keyboard-
  // operable immediately.
  useEffect(() => {
    if (open) confirmRef.current?.focus();
  }, [open]);

  if (!open) return null;

  return (
    // Dimmed scrim: a click anywhere outside the panel cancels.
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      onMouseDown={onCancel}
      role="presentation"
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        role="alertdialog"
        aria-modal="true"
        aria-label={title}
        // Stop propagation so clicks inside the panel don't hit the scrim cancel.
        onMouseDown={(e) => e.stopPropagation()}
        className="flex w-[380px] max-w-[92vw] flex-col gap-4 rounded-lg border p-5 shadow-2xl"
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
          borderRadius: "var(--th-radius)",
        }}
      >
        <div className="text-base font-semibold" style={{ color: "var(--th-fg)" }}>
          {title}
        </div>
        {body && (
          <div className="text-sm leading-relaxed" style={{ color: "var(--th-fg-muted)" }}>
            {body}
          </div>
        )}
        <div className="mt-1 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="rounded border px-3 py-1.5 text-sm transition-colors hover:bg-neutral-700/30"
            style={{
              borderColor: "var(--th-border)",
              color: "var(--th-fg)",
              borderRadius: "var(--th-radius)",
            }}
          >
            {cancelLabel}
          </button>
          <button
            ref={confirmRef}
            type="button"
            onClick={onConfirm}
            className="rounded border px-3 py-1.5 text-sm font-medium transition-colors"
            style={
              danger
                ? {
                    // Destructive: a red fill that reads as "this deletes things"
                    // regardless of the active theme (the dot-error token is the
                    // theme's red; fall back to a sane red if a theme omits it).
                    backgroundColor: "var(--th-dot-error, #dc2626)",
                    borderColor: "var(--th-dot-error, #dc2626)",
                    color: "#fff",
                    borderRadius: "var(--th-radius)",
                  }
                : {
                    borderColor: "var(--th-accent)",
                    color: "var(--th-fg)",
                    borderRadius: "var(--th-radius)",
                  }
            }
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
