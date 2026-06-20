// Shared sidebar chrome bits (#9): the disclosure ChevronIcon and the small
// CountBadge chip. Sidebar.tsx and WorkspacesList.tsx each defined IDENTICAL
// copies of both; this is the single source so they can't drift. Markup +
// styling are byte-for-byte what the two files had.

/** A small disclosure chevron that points right when collapsed, down when open. */
export function ChevronIcon({ open }: { open: boolean }) {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="3"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none shrink-0 transition-transform"
      style={{ transform: open ? "rotate(90deg)" : "rotate(0deg)" }}
      aria-hidden
    >
      <path d="M9 6l6 6-6 6" />
    </svg>
  );
}

/** A small count chip shown in a section / workspace-row header. */
export function CountBadge({ n }: { n: number }) {
  return (
    <span
      className="shrink-0 rounded-full px-1.5 text-[10px] tabular-nums"
      style={{ backgroundColor: "var(--th-tile-bg)", color: "var(--th-fg-muted)" }}
    >
      {n}
    </span>
  );
}
