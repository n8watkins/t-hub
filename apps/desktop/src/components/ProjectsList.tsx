// The sidebar's "Projects" list (feat/projects-sidebar, Agent A).
//
// In the new product model a PROJECT is one terminal session, pinned to a
// directory and named after it; a workspace tab holds one or more projects
// (tiles). This list shows the projects (terminals) open in the CURRENT (active)
// workspace tab — NOT every tab, and NOT the Claude supervision tree. Clicking a
// row reveals + focuses that tile (App wires `onSelect` to setActiveTab +
// setFocus on the owning tab).
//
// Names come from `deriveLabel` (a user rename, else the spawn command · cwd
// basename, else the short id) — the same naming the tiles/titlebar use — so a
// project reads by its directory as intended. A themed lifecycle dot mirrors
// Tile.tsx's palette. This is pure navigation: it never mutates the store.
import { deriveLabel } from "../store/workspace";
import type { TerminalId, TerminalInfo, TerminalState } from "../ipc/types";

/**
 * Lifecycle-dot color per terminal state, mirroring Tile.tsx / the old sidebar's
 * DOT_VAR so the Projects list reads the same themed `--th-dot-*` palette
 * (amber=starting / green=live / gray=detached / dim=exited / red=error).
 */
const DOT_VAR: Record<TerminalState, string> = {
  starting: "var(--th-dot-starting)",
  live: "var(--th-dot-live)",
  detached: "var(--th-dot-detached)",
  exited: "var(--th-dot-exited)",
  error: "var(--th-dot-error)",
};

export interface ProjectsListProps {
  /** The active tab's tile order (terminal ids), top-to-bottom. */
  order: TerminalId[];
  /** Live terminal records (for state/title/cwd → label + dot). */
  terminals: Record<TerminalId, TerminalInfo>;
  /** Effective per-terminal labels (#labels): user rename overlaid on Claude title. */
  labels: Record<TerminalId, string>;
  /** The focused tile in the active tab (accent-highlighted here). */
  focusedId: TerminalId | null;
  /** Reveal + focus a project's tile (App: setActiveTab(owner) + setFocus(id)). */
  onSelect: (id: TerminalId) => void;
}

/** The active workspace tab's projects, one clickable row each. Empty-state hint
 *  when the tab has no terminals yet. */
export function ProjectsList({
  order,
  terminals,
  labels,
  focusedId,
  onSelect,
}: ProjectsListProps) {
  if (order.length === 0) {
    return (
      <div className="px-2 py-1 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        No projects in this workspace yet.
      </div>
    );
  }
  return (
    <ul>
      {order.map((id) => (
        <ProjectRow
          key={id}
          id={id}
          info={terminals[id]}
          userLabel={labels[id]}
          active={id === focusedId}
          onClick={() => onSelect(id)}
        />
      ))}
    </ul>
  );
}

/** One project row: a themed lifecycle dot + the project's friendly name (with
 *  the short id faint beside it when the name isn't already the id). The record
 *  may be missing if the live map hasn't seeded that id yet — fall back gracefully
 *  to a "starting" dot and the derived label. */
function ProjectRow({
  id,
  info,
  userLabel,
  active,
  onClick,
}: {
  id: TerminalId;
  info?: TerminalInfo;
  userLabel?: string;
  active: boolean;
  onClick: () => void;
}) {
  const state: TerminalState = info?.state ?? "starting";
  const label = deriveLabel({
    id,
    label: userLabel,
    title: info?.title,
    cwd: info?.cwd,
  });
  const showShortId = label !== id;
  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        aria-current={active ? "true" : undefined}
        className="flex w-full cursor-pointer items-center gap-2 py-1 pr-2 pl-2 text-left text-sm hover:bg-neutral-900"
        style={{
          color: "var(--th-fg)",
          ...(active ? { backgroundColor: "var(--th-accent)" } : {}),
        }}
        title={`${showShortId ? `${label} · ${id}` : label} — ${state}`}
      >
        <span
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: DOT_VAR[state] }}
          aria-hidden
        />
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {showShortId && (
          <span
            className="shrink-0 font-mono text-[0.9em]"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {id}
          </span>
        )}
      </button>
    </li>
  );
}
