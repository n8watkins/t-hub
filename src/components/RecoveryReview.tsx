// RecoveryReview — a "rewind the workspace" panel (#recovery, Goal C).
//
// The workspace layout (tabs / tile order / sizes / focus / font) is mirrored to
// a durable SQLite copy on every change (see src-tauri/src/db.rs + the workspace
// store). That copy alone only ever holds the LATEST arrangement, so a crash, a
// botched drag, or a bad redeploy that overwrites it leaves the previous good
// layout unrecoverable. The backend therefore also keeps a short HISTORY ring of
// recent snapshots; this panel surfaces it:
//
//   - lists recent snapshots (timestamp + a "N tabs · M terminals" summary),
//   - lets the user PREVIEW a snapshot's tabs/terminals before committing, and
//   - RESTORES a chosen snapshot to the live workspace, behind a confirm step.
//
// It also surfaces ORPHANED tmux sessions — sessions the backend still has
// (list_terminals) that aren't placed in the CURRENT layout. These are read-only
// here (re-adoption happens automatically: the workspace store appends any
// unplaced live terminal onto the active tab on its next reconcile), so we just
// make them visible so the user knows nothing was lost.
//
// Styling matches the Settings modal (ThemeEditor): a scrim + centered themed
// panel, all `var(--th-*)` tokens, a `th-scroll` body. It is fully self-contained
// — its own open/close state lives here — and mounts via a "Recovery" button the
// Settings modal's General section renders (see ThemeEditor.tsx).
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  listSnapshots,
  getSnapshot,
  type SnapshotMeta,
} from "../ipc/persistence";
import { listTerminals } from "../ipc/client";
import {
  useWorkspace,
  deriveLabel,
  type WorkspaceTab,
} from "../store/workspace";
import type { TerminalId, TerminalInfo } from "../ipc/types";

// ---------------------------------------------------------------------------
// Snapshot layout shape (a structural subset of the store's PersistedLayout).
// We parse the snapshot JSON defensively here rather than importing the store's
// (un-exported) parser, so a malformed entry degrades to an empty preview rather
// than throwing. Only the fields the panel reads + re-applies are typed.
// ---------------------------------------------------------------------------
interface SnapshotLayout {
  tabs: WorkspaceTab[];
  activeTabId: string;
  focusedId: TerminalId | null;
  fontSize: number;
  labels: Record<TerminalId, string>;
  poppedOutTabs: WorkspaceTab[];
}

/** Coerce one parsed tab record into a clean {id,name,order} (drops junk). */
function coerceTab(t: unknown): WorkspaceTab | null {
  if (!t || typeof t !== "object") return null;
  const r = t as Partial<WorkspaceTab>;
  const order = Array.isArray(r.order)
    ? r.order.filter((x): x is TerminalId => typeof x === "string")
    : [];
  return {
    id: typeof r.id === "string" && r.id ? r.id : `tab-${Math.random().toString(36).slice(2)}`,
    name: typeof r.name === "string" && r.name ? r.name : "Workspace",
    order,
    sizes: r.sizes,
  };
}

function coerceTabs(value: unknown): WorkspaceTab[] {
  return Array.isArray(value)
    ? value.map(coerceTab).filter((t): t is WorkspaceTab => t !== null)
    : [];
}

/** Parse a snapshot JSON string into a layout, or null if unusable. */
function parseSnapshot(json: string | null): SnapshotLayout | null {
  if (!json) return null;
  try {
    const p = JSON.parse(json) as Record<string, unknown>;
    const tabs = coerceTabs(p.tabs);
    const poppedOutTabs = coerceTabs(p.poppedOutTabs);
    if (tabs.length === 0 && poppedOutTabs.length === 0) return null;
    const labels: Record<TerminalId, string> = {};
    if (p.labels && typeof p.labels === "object") {
      for (const [k, v] of Object.entries(p.labels as Record<string, unknown>)) {
        if (typeof v === "string") labels[k] = v;
      }
    }
    return {
      tabs,
      activeTabId: typeof p.activeTabId === "string" ? p.activeTabId : "",
      focusedId: typeof p.focusedId === "string" ? p.focusedId : null,
      fontSize: typeof p.fontSize === "number" ? p.fontSize : 13,
      labels,
      poppedOutTabs,
    };
  } catch {
    return null;
  }
}

/** All tabs a layout describes (visible + popped-out), in one flat list. */
function allTabs(layout: SnapshotLayout): WorkspaceTab[] {
  return [...layout.tabs, ...layout.poppedOutTabs];
}

/** Total terminal/tile count across every tab in a layout. */
function terminalCount(layout: SnapshotLayout): number {
  return allTabs(layout).reduce((n, t) => n + t.order.length, 0);
}

/** Format an epoch-SECONDS timestamp as a friendly local date/time. */
function formatTs(ts: number): string {
  try {
    return new Date(ts * 1000).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

// ---------------------------------------------------------------------------
// Public entry: a self-toggling modal. Rendered (returns null until open) by the
// Settings General section's "Recovery" button, which flips `open`.
// ---------------------------------------------------------------------------
export function RecoveryReview({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  // Esc closes the panel (only while open, so it doesn't swallow Esc globally).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;
  return <RecoveryPanel onClose={onClose} />;
}

// ---------------------------------------------------------------------------
// The panel (only mounted while open).
// ---------------------------------------------------------------------------
function RecoveryPanel({ onClose }: { onClose: () => void }) {
  const [snapshots, setSnapshots] = useState<SnapshotMeta[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [preview, setPreview] = useState<SnapshotLayout | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [restoreMsg, setRestoreMsg] = useState<string | null>(null);
  const [liveTerminals, setLiveTerminals] = useState<TerminalInfo[]>([]);

  // The live layout — to compute which sessions are orphaned (present in the
  // backend but not placed in any current tab).
  const tabs = useWorkspace((s) => s.tabs);
  const poppedOutTabs = useWorkspace((s) => s.poppedOutTabs);

  // Load the snapshot list + live terminals once on open.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [snaps, terms] = await Promise.all([
          listSnapshots(),
          listTerminals().catch(() => [] as TerminalInfo[]),
        ]);
        if (cancelled) return;
        setSnapshots(snaps);
        setLiveTerminals(terms);
        if (snaps.length > 0) setSelectedId(snaps[0].id);
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Load the preview layout whenever the selection changes.
  useEffect(() => {
    if (selectedId == null) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    setPreviewLoading(true);
    setConfirming(false);
    void (async () => {
      const json = await getSnapshot(selectedId).catch(() => null);
      if (cancelled) return;
      setPreview(parseSnapshot(json));
      setPreviewLoading(false);
    })();
    return () => {
      cancelled = true;
    };
  }, [selectedId]);

  // Orphaned sessions: live terminals the CURRENT layout doesn't place anywhere.
  const orphanIds = useMemo(() => {
    const placed = new Set<TerminalId>();
    for (const t of [...tabs, ...poppedOutTabs]) {
      for (const id of t.order) placed.add(id);
    }
    return liveTerminals.map((t) => t.id).filter((id) => !placed.has(id));
  }, [liveTerminals, tabs, poppedOutTabs]);

  const doRestore = useCallback(async () => {
    if (selectedId == null) return;
    const json = await getSnapshot(selectedId).catch(() => null);
    const layout = parseSnapshot(json);
    if (!layout) {
      setRestoreMsg("Could not load that snapshot — it may have aged out.");
      setConfirming(false);
      return;
    }
    // Apply via the store, then reconcile against live sessions. setState writes
    // the persisted layout fields; the subsequent setTerminals(...) prunes each
    // tab's order to ids that still exist, appends any orphaned live session onto
    // the active tab, and PERSISTS both the localStorage mirror and the durable
    // SQLite copy through the store's own save path — so the restored layout is
    // itself captured as the newest snapshot.
    const store = useWorkspace.getState();
    useWorkspace.setState({
      tabs: layout.tabs.length > 0 ? layout.tabs : layout.poppedOutTabs,
      activeTabId: layout.activeTabId,
      focusedId: layout.focusedId,
      fontSize: layout.fontSize,
      labels: layout.labels,
      // Re-adopt any popped-out tabs into the visible set on restore: a satellite
      // window from a prior session no longer exists, so leaving them popped would
      // render them nowhere. (Mirrors the store's own orphan-adoption on boot.)
      poppedOutTabs: [],
    });
    if (layout.poppedOutTabs.length > 0 && layout.tabs.length > 0) {
      useWorkspace.setState({
        tabs: [...layout.tabs, ...layout.poppedOutTabs],
      });
    }
    try {
      const live = await listTerminals();
      store.setTerminals(live);
    } catch {
      // No backend to reconcile against — the layout still applied + persisted on
      // the next store write; surface success regardless.
    }
    setConfirming(false);
    setRestoreMsg("Workspace restored.");
  }, [selectedId]);

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center p-6"
      onMouseDown={onClose}
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        className="flex h-[680px] max-h-[85vh] w-[860px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
      >
        {/* Header */}
        <div
          className="flex shrink-0 items-center justify-between border-b px-5 py-3.5"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div className="flex flex-col">
            <div className="text-base font-semibold">Recovery review</div>
            <div className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
              Restore a recent workspace layout from the durable history.
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="-mr-1 flex h-8 w-8 items-center justify-center rounded transition-colors hover:bg-neutral-700/40"
            title="Close (Esc)"
            aria-label="Close recovery review"
            style={{ color: "var(--th-fg-muted)" }}
          >
            <CloseIcon />
          </button>
        </div>

        {/* Body: snapshot list (left) + preview (right). */}
        <div className="flex min-h-0 flex-1">
          {/* Left: the snapshot history list. */}
          <div
            className="th-scroll flex w-72 shrink-0 flex-col overflow-y-auto border-r"
            style={{ borderColor: "var(--th-border)" }}
          >
            {error && (
              <div className="p-4 text-xs" style={{ color: "var(--th-dot-error, #f87171)" }}>
                {error}
              </div>
            )}
            {snapshots == null && !error && (
              <div className="p-4 text-xs" style={{ color: "var(--th-fg-muted)" }}>
                Loading history…
              </div>
            )}
            {snapshots != null && snapshots.length === 0 && (
              <div className="p-4 text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
                No snapshots yet. The history fills as you rearrange the workspace.
              </div>
            )}
            {snapshots?.map((s, i) => (
              <SnapshotRow
                key={s.id}
                meta={s}
                isLatest={i === 0}
                selected={s.id === selectedId}
                onSelect={() => {
                  setSelectedId(s.id);
                  setRestoreMsg(null);
                }}
              />
            ))}
          </div>

          {/* Right: preview of the selected snapshot + restore action. */}
          <div className="th-scroll min-h-0 flex-1 overflow-y-auto px-5 py-4">
            {selectedId == null ? (
              <Empty>Select a snapshot to preview it.</Empty>
            ) : previewLoading ? (
              <Empty>Loading preview…</Empty>
            ) : !preview ? (
              <Empty>This snapshot could not be read.</Empty>
            ) : (
              <PreviewPane
                layout={preview}
                labels={preview.labels}
                liveTerminals={liveTerminals}
              />
            )}

            {/* Orphaned sessions — read-only awareness. */}
            <OrphanSection orphanIds={orphanIds} liveTerminals={liveTerminals} />
          </div>
        </div>

        {/* Footer: restore + confirm. */}
        <div
          className="flex shrink-0 items-center justify-between gap-3 border-t px-5 py-3"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div className="min-w-0 flex-1 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            {restoreMsg ??
              "Restoring replaces the current tabs and tile arrangement. Live terminals are reconciled — none are killed."}
          </div>
          {confirming ? (
            <div className="flex shrink-0 items-center gap-2">
              <span className="text-xs" style={{ color: "var(--th-fg)" }}>
                Replace the current layout?
              </span>
              <Btn onClick={() => setConfirming(false)} title="Cancel restore">
                Cancel
              </Btn>
              <Btn onClick={() => void doRestore()} title="Confirm: restore this snapshot" emphasis>
                Confirm restore
              </Btn>
            </div>
          ) : (
            <Btn
              onClick={() => {
                setRestoreMsg(null);
                setConfirming(true);
              }}
              disabled={!preview}
              title={
                preview
                  ? "Restore the selected snapshot to the workspace"
                  : "Select a readable snapshot first"
              }
              emphasis
            >
              Restore…
            </Btn>
          )}
        </div>
      </div>
    </div>
  );
}

/** One row in the snapshot history list. */
function SnapshotRow({
  meta,
  isLatest,
  selected,
  onSelect,
}: {
  meta: SnapshotMeta;
  isLatest: boolean;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className="flex flex-col gap-0.5 border-b px-4 py-3 text-left transition-colors hover:bg-neutral-700/20"
      aria-current={selected ? "true" : undefined}
      style={{
        borderColor: "var(--th-border)",
        backgroundColor: selected ? "var(--th-tile-bg)" : "transparent",
      }}
    >
      <div className="flex items-center gap-2">
        <span
          className="text-sm"
          style={{ color: "var(--th-fg)", fontWeight: selected ? 600 : 400 }}
        >
          {formatTs(meta.ts)}
        </span>
        {isLatest && (
          <span
            className="rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wide"
            style={{ backgroundColor: "var(--th-accent)", color: "var(--th-app-bg, #000)" }}
          >
            Latest
          </span>
        )}
      </div>
      <span className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
        {meta.summary}
      </span>
    </button>
  );
}

/** The preview of a selected snapshot: its tabs and the terminals in each. */
function PreviewPane({
  layout,
  labels,
  liveTerminals,
}: {
  layout: SnapshotLayout;
  labels: Record<TerminalId, string>;
  liveTerminals: TerminalInfo[];
}) {
  const liveById = useMemo(() => {
    const m: Record<TerminalId, TerminalInfo> = {};
    for (const t of liveTerminals) m[t.id] = t;
    return m;
  }, [liveTerminals]);

  const tabs = allTabs(layout);
  const total = terminalCount(layout);

  return (
    <div className="flex flex-col gap-3">
      <div className="text-xs font-semibold uppercase tracking-wide" style={{ color: "var(--th-fg)" }}>
        Preview — {tabs.length} {tabs.length === 1 ? "tab" : "tabs"} · {total}{" "}
        {total === 1 ? "terminal" : "terminals"}
      </div>
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className="rounded border"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div
            className="border-b px-3 py-2 text-sm"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
          >
            {tab.name}
            <span className="ml-2 text-xs" style={{ color: "var(--th-fg-muted)" }}>
              {tab.order.length} {tab.order.length === 1 ? "terminal" : "terminals"}
            </span>
          </div>
          {tab.order.length === 0 ? (
            <div className="px-3 py-2 text-xs" style={{ color: "var(--th-fg-muted)" }}>
              (empty)
            </div>
          ) : (
            <ul className="px-3 py-2">
              {tab.order.map((id) => {
                const info = liveById[id];
                const name = deriveLabel({
                  id,
                  label: labels[id],
                  title: info?.title,
                  cwd: info?.cwd,
                });
                const gone = !info;
                return (
                  <li
                    key={id}
                    className="flex items-center gap-2 py-0.5 text-sm"
                    style={{ color: "var(--th-fg)" }}
                  >
                    <span
                      className="h-1.5 w-1.5 shrink-0 rounded-full"
                      style={{
                        backgroundColor: gone
                          ? "var(--th-fg-muted)"
                          : "var(--th-dot-live, #4ade80)",
                      }}
                      title={gone ? "Not currently running" : "Running"}
                    />
                    <span className="truncate">{name}</span>
                    <span className="ml-auto shrink-0 font-mono text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
                      {id}
                      {gone ? " · gone" : ""}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      ))}
    </div>
  );
}

/**
 * Orphaned-session awareness: live sessions the CURRENT layout doesn't place in
 * any tab. Read-only — the workspace store auto-adopts any unplaced live session
 * onto the active tab on its next reconcile, so this is informational (nothing to
 * lose; here's where they'll land).
 */
function OrphanSection({
  orphanIds,
  liveTerminals,
}: {
  orphanIds: TerminalId[];
  liveTerminals: TerminalInfo[];
}) {
  const liveById = useMemo(() => {
    const m: Record<TerminalId, TerminalInfo> = {};
    for (const t of liveTerminals) m[t.id] = t;
    return m;
  }, [liveTerminals]);

  if (orphanIds.length === 0) return null;
  return (
    <div className="mt-5">
      <div className="text-xs font-semibold uppercase tracking-wide" style={{ color: "var(--th-fg)" }}>
        Orphaned sessions ({orphanIds.length})
      </div>
      <p className="mb-2 mt-1 text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        Running tmux sessions not placed in any current tab. They aren&apos;t lost:
        TermHub re-adopts them onto the active tab automatically. Listed here so you
        know they survived.
      </p>
      <ul
        className="rounded border px-3 py-2"
        style={{ borderColor: "var(--th-border)" }}
      >
        {orphanIds.map((id) => {
          const info = liveById[id];
          const name = deriveLabel({ id, title: info?.title, cwd: info?.cwd });
          return (
            <li
              key={id}
              className="flex items-center gap-2 py-0.5 text-sm"
              style={{ color: "var(--th-fg)" }}
            >
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ backgroundColor: "var(--th-dot-detached, #fbbf24)" }}
              />
              <span className="truncate">{name}</span>
              <span className="ml-auto shrink-0 font-mono text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
                {id}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-full items-center justify-center text-sm" style={{ color: "var(--th-fg-muted)" }}>
      {children}
    </div>
  );
}

function Btn({
  children,
  onClick,
  title,
  disabled,
  emphasis,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title?: string;
  disabled?: boolean;
  emphasis?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      disabled={disabled}
      className="shrink-0 rounded border px-3 py-1.5 text-sm transition-colors hover:bg-neutral-700/30 disabled:cursor-not-allowed disabled:opacity-50"
      style={{
        borderColor: emphasis ? "var(--th-accent)" : "var(--th-border)",
        color: emphasis ? "var(--th-accent)" : "var(--th-fg)",
      }}
    >
      {children}
    </button>
  );
}

/** Modal close X (matches the Settings modal). */
function CloseIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M18 6 6 18" />
      <path d="m6 6 12 12" />
    </svg>
  );
}
