// The sidebar's "Workspaces" list — the ONLY navigation surface now.
//
// In the product model a WORKSPACE is a top tab; a terminal lives inside one.
// (The old separate "Projects" section is gone — workspaces are the unit.) This
// shows EVERY workspace as a collapsible row over its terminals, the single place
// to switch to any workspace / any terminal, rename a workspace, and close a
// terminal.
//
// Behavior:
//   - Click a workspace row     -> switch to it (setActiveTab). Active workspace
//     gets a SUBTLE accent tint + thin accent left bar (not a bright fill).
//   - Double-click the name     -> rename the workspace inline (renameTab).
//   - Chevron                   -> expand/collapse its terminals (active expanded
//     by default; local UI state, not persisted).
//   - Click a terminal          -> switch to its workspace AND focus it.
//   - X on a terminal           -> kill it (deleteTerminal) — close what you're
//     working on right from here.
//
// Reads the workspace store directly (no props), so App needs no extra wiring.
import { useEffect, useRef, useState } from "react";
import { useWorkspace, deriveLabel } from "../store/workspace";
import type { WorkspaceTab } from "../store/workspace";
import type { TerminalId, TerminalInfo, TerminalState } from "../ipc/types";

/** Lifecycle-dot color per terminal state — the SAME `--th-dot-*` palette Tile
 *  uses, so a terminal reads identically wherever it appears. */
const DOT_VAR: Record<TerminalState, string> = {
  starting: "var(--th-dot-starting)",
  live: "var(--th-dot-live)",
  detached: "var(--th-dot-detached)",
  exited: "var(--th-dot-exited)",
  error: "var(--th-dot-error)",
};

export function WorkspacesList() {
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const terminals = useWorkspace((s) => s.terminals);
  const labels = useWorkspace((s) => s.labels);
  const focusedId = useWorkspace((s) => s.focusedId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);
  const setFocus = useWorkspace((s) => s.setFocus);
  const renameTab = useWorkspace((s) => s.renameTab);
  const deleteTerminal = useWorkspace((s) => s.deleteTerminal);

  // Local expand/collapse, keyed by tab id. Undefined = default (open iff active).
  const [openMap, setOpenMap] = useState<Record<string, boolean>>({});
  const isOpen = (tab: WorkspaceTab) => openMap[tab.id] ?? tab.id === activeTabId;
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
          onActivate={() => setActiveTab(tab.id)}
          onRename={(name) => renameTab(tab.id, name)}
          onSelectTerminal={(id) => {
            setActiveTab(tab.id);
            setFocus(id);
          }}
          onCloseTerminal={(id) => deleteTerminal(id)}
        />
      ))}
    </ul>
  );
}

function WorkspaceRow({
  tab,
  active,
  open,
  terminals,
  labels,
  focusedId,
  onToggle,
  onActivate,
  onRename,
  onSelectTerminal,
  onCloseTerminal,
}: {
  tab: WorkspaceTab;
  active: boolean;
  open: boolean;
  terminals: Record<TerminalId, TerminalInfo>;
  labels: Record<TerminalId, string>;
  focusedId: TerminalId | null;
  onToggle: () => void;
  onActivate: () => void;
  onRename: (name: string) => void;
  onSelectTerminal: (id: TerminalId) => void;
  onCloseTerminal: (id: TerminalId) => void;
}) {
  const count = tab.order.length;
  // Inline rename state: double-click the name to edit; Enter/blur commits,
  // Esc cancels. Seeded from the current name each time editing starts.
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(tab.name);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const startEdit = () => {
    setDraft(tab.name);
    setEditing(true);
  };
  const commit = () => {
    const name = draft.trim();
    if (name && name !== tab.name) onRename(name);
    setEditing(false);
  };

  return (
    <li>
      <div
        className="flex w-full items-center gap-1 rounded-lg pr-1 transition-colors hover:bg-neutral-800/40"
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

        {editing ? (
          <input
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit();
              else if (e.key === "Escape") setEditing(false);
            }}
            spellCheck={false}
            className="min-w-0 flex-1 rounded bg-transparent px-1 py-1 text-sm outline-none"
            style={{
              color: "var(--th-fg)",
              border: "1px solid var(--th-focus-ring, var(--th-accent))",
            }}
          />
        ) : (
          <button
            type="button"
            onClick={onActivate}
            onDoubleClick={startEdit}
            aria-current={active ? "true" : undefined}
            className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-1.5 pr-1 text-left text-sm"
            title={`${tab.name} — ${count} terminal${count === 1 ? "" : "s"} · double-click to rename`}
          >
            <span className="min-w-0 flex-1 truncate font-medium">{tab.name}</span>
            <CountBadge n={count} />
          </button>
        )}
      </div>

      {/* Smooth expand/collapse via a 0fr↔1fr grid row (rows stay mounted). */}
      <div
        className="grid"
        style={{
          gridTemplateRows: open ? "1fr" : "0fr",
          transition: "grid-template-rows 200ms ease",
        }}
      >
        <div style={{ overflow: "hidden", minHeight: 0 }}>
          {count === 0 ? (
            <div
              className="py-1 pl-7 pr-2 text-sm"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Nothing open here yet.
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
                  onClose={() => onCloseTerminal(id)}
                />
              ))}
            </ul>
          )}
        </div>
      </div>
    </li>
  );
}

/** One nested terminal row: a themed lifecycle dot + the friendly "what's
 *  running" label (e.g. "claude · tools" from deriveLabel — the command + dir,
 *  NOT the opaque session id, which isn't useful here), and an X to close it. The
 *  focused tile (of the active workspace) gets the subtle accent tint + left bar. */
function TerminalRow({
  id,
  info,
  userLabel,
  active,
  onClick,
  onClose,
}: {
  id: TerminalId;
  info?: TerminalInfo;
  userLabel?: string;
  active: boolean;
  onClick: () => void;
  onClose: () => void;
}) {
  const state: TerminalState = info?.state ?? "starting";
  const label = deriveLabel({
    id,
    label: userLabel,
    title: info?.title,
    cwd: info?.cwd,
  });
  const cwd = info?.cwd ?? "";
  return (
    <li
      className="group flex items-center gap-2 rounded-lg pr-1 transition-colors hover:bg-neutral-800/40"
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
      <button
        type="button"
        onClick={onClick}
        aria-current={active ? "true" : undefined}
        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-1.5 pl-2.5 text-left text-sm"
        title={cwd ? `${label} — ${cwd} (${state})` : `${label} — ${state}`}
      >
        <span
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: DOT_VAR[state] }}
          aria-hidden
        />
        <span className="min-w-0 flex-1 truncate">{label}</span>
      </button>
      {/* Close (kill) this terminal — "close what we're working on from the
          workspace". Reveals on row hover so it doesn't clutter the list. */}
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
        className="shrink-0 rounded px-1 leading-none opacity-0 transition-opacity hover:bg-neutral-700/40 group-hover:opacity-100"
        style={{ color: "var(--th-fg-muted)" }}
        title="Close this terminal (kills its session)"
        aria-label="Close terminal"
      >
        ×
      </button>
    </li>
  );
}

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
