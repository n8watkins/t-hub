// Inline SVG icons for the Theme settings surface (extracted verbatim from
// ThemeEditor.tsx). Stroke uses currentColor so they inherit the themed fg.
// The preset-action icon buttons and the ANSI-palette disclosure chevron use
// these; the modal-header CloseIcon stays in ThemeEditor (it's panel chrome).
import React from "react";

export function Svg({ children }: { children: React.ReactNode }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

/** Floppy-disk / save. */
export function SaveIcon() {
  return (
    <Svg>
      <path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2Z" />
      <path d="M17 21v-8H7v8" />
      <path d="M7 3v5h8" />
    </Svg>
  );
}

/** Up-arrow into a tray - export. */
export function ExportIcon() {
  return (
    <Svg>
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M12 3v12" />
      <path d="m7 8 5-5 5 5" />
    </Svg>
  );
}

/** Down-arrow into a tray - import. */
export function ImportIcon() {
  return (
    <Svg>
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M12 3v12" />
      <path d="m7 10 5 5 5-5" />
    </Svg>
  );
}

/** Circular arrow - reset. */
export function ResetIcon() {
  return (
    <Svg>
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
      <path d="M3 3v5h5" />
    </Svg>
  );
}

/** A small disclosure chevron; rotates when open. */
export function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className="shrink-0 transition-transform"
      style={{
        color: "var(--th-fg-muted)",
        transform: open ? "rotate(90deg)" : "rotate(0deg)",
      }}
    >
      <path d="m9 18 6-6-6-6" />
    </svg>
  );
}
