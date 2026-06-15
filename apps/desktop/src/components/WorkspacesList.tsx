// The sidebar's "Workspaces" list (feat/workspaces-lifecycle).
//
// In the product model a WORKSPACE is a top tab and a PROJECT is one terminal
// tile inside it. The existing "Projects" section only ever shows the ACTIVE
// workspace's terminals; this section sits ABOVE it and shows EVERY workspace as
// a collapsible row, with the terminals nested inside each. It's the global
// "switch to any tab / any tile from one place" surface the user asked for.
//
// Behavior:
//   - Click a workspace row header  -> switch to that workspace (setActiveTab).
//     The active workspace is marked with a SUBTLE accent tint + thin accent left
//     bar (matching ProjectsList's focused-row treatment), not a bright fill.
//   - Expand/collapse a workspace   -> reveal/hide its terminals. The ACTIVE
//     workspace is expanded by default; the rest start collapsed. This is purely
//     local UI state (a per-tab open flag), not persisted.
//   - Click a nested terminal       -> switch to its owning workspace AND focus
//     it (setActiveTab(owner) + setFocus(id)).
//   - Each workspace row shows a count of the terminals it holds.
//
// This is pure navigation over the workspace store: it reads tabs/terminals/
// labels/focus and calls setActiveTab/setFocus, never mutating layout. The row
// look (lifecycle dot + derived label) deliberately mirrors ProjectsList so the
// two sections read as one family; we re-derive it here rather than importing
// ProjectsList's private row (kept untouched per the build split).
import { useState } from "react";
import { useWorkspace, deriveLabel } from "../store/workspace";
import type { WorkspaceTab } from "../store/workspace";
import type { TerminalId, TerminalInfo, TerminalState } from "../ipc/types";

/**
 * Lifecycle-dot color per terminal state — the SAME `--th-dot-*` palette
 * ProjectsList / Tile use, so a terminal reads identically wherever it appears
 * (amber=starting / green=live / gray=detached / dim=exited / red=error).
 */
const DOT_VAR: Record<TerminalState, string> = {
  starting: "var(--th-dot-starting)",
  live: "var(--th-dot-live)",
  detached: "var(--th-dot-detached)",
  exited: "var(--th-dot-exited)",
  error: "var(--th-dot-error)",
};

/** The full list of workspaces, each a collapsible row over its terminals. Reads
 *  the store directly (no props) so it stays self-contained and App needs no new
 *  wiring. Empty-state hint when there are somehow no workspaces. */
export function WorkspacesList() {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const terminals = useWorkspace((s) => s.terminals);
  const labels = useWorkspace((s) => s.labels);
  const focusedId = useWorkspace((s) => s.focusedId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const setFocus = useWorkspace((s) => s.setFocus);

  // Local expand/collapse state, keyed by tab id. Undefined = "use the default"
  // (expanded iff this is the active workspace); an explicit boolean overrides
  // it once the user toggles a row. Not persisted — a fresh launch re-derives.
  const [openMap, setOpenMap] = useState<Record<string, boolean>>({});
  const isOpen = (tab: WorkspaceTab) =>
    openMap[tab.id] ?? tab.id === activeTabId;
  const toggleOpen = (tab: WorkspaceTab) =>
    setOpenMap((m) => ({ ...m, [tab.id]: !isOpen(tab) }));

  if (tabs.length === 0) {
    return (
      <div className="px-2 py-1 text-sm" style={{ color: "var(--th-fg-muted)" }}>
        No workspaces yet.
      </div>
    );
  }

  return (
    <ul className="flex flex-col gap-0.5 px-2 py-1">
      {tabs.map((tab) => (
        <WorkspaceRow
          key={tab.id}
          tab={tab}
          active={tab.id === activeTabId}
          open={isOpen(tab)}
          terminals={terminals}
          labels={labels}
          focusedId={focusedId}
          onToggle={() => toggleOpen(tab)}
          // Header click: switch to this workspace (no-op if already active).
          onActivate={() => setActiveTab(tab.id)}
          // Terminal click: switch to the owning workspace, then focus the tile.
          onSelectTerminal={(id) => {
            setActiveTab(tab.id);
            setFocus(id);
          }}
        />
      ))}
    </ul>
  );
}

/** One workspace: a header row (expand chevron + name + tile count) over its
 *  nested terminals when expanded. The header is split into two hit targets —
 *  the chevron toggles expand/collapse, the rest of the row switches to the
 *  workspace — so the two affordances don't fight. */
function WorkspaceRow({
  tab,
  active,
  open,
  terminals,
  labels,
  focusedId,
  onToggle,
  onActivate,
  onSelectTerminal,
}: {
  tab: WorkspaceTab;
  active: boolean;
  open: boolean;
  terminals: Record<TerminalId, TerminalInfo>;
  labels: Record<TerminalId, string>;
  focusedId: TerminalId | null;
  onToggle: () => void;
  onActivate: () => void;
  onSelectTerminal: (id: TerminalId) => void;
}) {
  const count = tab.order.length;
  return (
    <li>
      {/* Header row. Rounded + subtle hover to match ProjectsList rows; the
          ACTIVE workspace gets the same SUBTLE accent tint + thin accent left
          bar the focused project row uses (not a bright fill). */}
      <div
        className="flex w-full items-center gap-1 rounded-lg pr-2 transition-colors hover:bg-neutral-800/40"
        style={{
          color: "var(--th-fg)",
          ...(active
            ? {
                backgroundColor:
                  "color-mix(in srgb, var(--th-accent) 16%, transparent)",
                boxShadow: "inset 2px 0 0 0 var(--th-accent)",
              }
            : {}),
        }}
      >
        {/* Disclosure chevron — toggles expand/collapse only. */}
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          aria-label={open ? "Collapse workspace" : "Expand workspace"}
          title={open ? "Collapse" : "Expand"}
          className="flex h-7 w-5 shrink-0 items-center justify-center opacity-70 hover:opacity-100"
        >
          <ChevronIcon open={open} />
        </button>
        {/* Name + count — switches to this workspace. */}
        <button
          type="button"
          onClick={onActivate}
          aria-current={active ? "true" : undefined}
          className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-1.5 pr-1 text-left text-sm"
          title={`${tab.name} — ${count} project${count === 1 ? "" : "s"}`}
        >
          <span className="min-w-0 flex-1 truncate font-medium">{tab.name}</span>
          <CountBadge n={count} />
        </button>
      </div>

      {/* Nested terminals (only when expanded). Indented under the header so the
          hierarchy reads; each uses the same dot + derived-label row as Projects. */}
      {open &&
        (count === 0 ? (
          <div
            className="py-1 pl-7 pr-2 text-sm"
            style={{ color: "var(--th-fg-muted)" }}
          >
            No projects here yet.
          </div>
        ) : (
          <ul className="flex flex-col gap-0.5 py-0.5 pl-5">
            {tab.order.map((id) => (
              <TerminalRow
                key={id}
                id={id}
                info={terminals[id]}
                userLabel={labels[id]}
                active={id === focusedId}
                onClick={() => onSelectTerminal(id)}
              />
            ))}
          </ul>
        ))}
    </li>
  );
}

/** One nested terminal row: a themed lifecycle dot + the project's friendly name
 *  (with the short id faint beside it when the name isn't already the id) — the
 *  same look ProjectsList uses. The focused tile (of the active workspace) gets
 *  the subtle accent tint + left bar. */
function TerminalRow({
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
        className="flex w-full cursor-pointer items-center gap-2 rounded-lg py-1.5 pr-2 pl-2.5 text-left text-sm transition-colors hover:bg-neutral-800/40"
        style={{
          color: "var(--th-fg)",
          ...(active
            ? {
                backgroundColor:
                  "color-mix(in srgb, var(--th-accent) 16%, transparent)",
                boxShadow: "inset 2px 0 0 0 var(--th-accent)",
              }
            : {}),
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

/** A small count chip (mirrors the sidebar Section's CountBadge styling). */
function CountBadge({ n }: { n: number }) {
  return (
    <span
      className="shrink-0 rounded-full px-1.5 text-[10px] tabular-nums"
      style={{ backgroundColor: "var(--th-tile-bg)", color: "var(--th-fg-muted)" }}
    >
      {n}
    </span>
  );
}

/** Disclosure chevron — points right when collapsed, down when open (mirrors the
 *  sidebar's own ChevronIcon). */
function ChevronIcon({ open }: { open: boolean }) {
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
