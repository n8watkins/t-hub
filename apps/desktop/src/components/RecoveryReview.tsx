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
// Finally (WS-6, native session-restore) it lists RESUMABLE orphaned Claude
// sessions — recorded per-tile bindings whose tmux session is GONE (the app /
// backend / host restarted) but whose transcript still EXISTS — each with a
// Restore button that re-spawns the conversation via `claude --resume <id>` in its
// original cwd (list_orphaned_sessions + the store's recall action).
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
  listOrphanedSessions,
  type OrphanedSession,
} from "../ipc/sessions";
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
  // WS-6: resumable orphaned Claude sessions (recorded tile bindings whose tmux
  // session is gone but whose transcript survives). null = still loading.
  const [orphanSessions, setOrphanSessions] = useState<OrphanedSession[] | null>(
    null,
  );

  // The live layout — to compute which sessions are orphaned (present in the
  // backend but not placed in any current tab).
  const tabs = useWorkspace((s) => s.tabs);
  const poppedOutTabs = useWorkspace((s) => s.poppedOutTabs);
  // Re-spawn + resume a past Claude session into the active tab (the SAME store
  // path the sidebar's Recent recall uses — one way a resumed tile is created).
  const recall = useWorkspace((s) => s.recall);

  // Load the snapshot list + live terminals + resumable orphans once on open.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [snaps, terms, orphans] = await Promise.all([
          listSnapshots(),
          listTerminals().catch(() => [] as TerminalInfo[]),
          listOrphanedSessions().catch(() => [] as OrphanedSession[]),
        ]);
        if (cancelled) return;
        setSnapshots(snaps);
        setLiveTerminals(terms);
        setOrphanSessions(orphans);
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

            {/* Resumable orphaned Claude sessions (WS-6) — Restore brings them
                back via `claude --resume` after an app/backend/host restart. */}
            <ResumableSessionsSection
              sessions={orphanSessions}
              liveTerminals={liveTerminals}
              // Restore is an EXPLICIT "resume THIS session" action whose copy
              // promises `claude --resume`, so force the resume regardless of the
              // passive global `resumeStartsClaude` default.
              onRestore={(s) => recall(s.sessionId, s.cwd, { forceResume: true })}
            />

            {/* Orphaned LIVE sessions — read-only awareness. */}
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

/** Format an epoch-SECONDS timestamp as a short relative "Nd/Nh/Nm ago" string. */
function relativeAgo(ts: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (secs < 60) return "just now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/**
 * Resumable orphaned Claude sessions (WS-6, native session-restore). Each row is a
 * tile we recorded whose tmux session is GONE (the app/backend/host restarted) but
 * whose transcript still EXISTS, so `claude --resume <sessionId>` in its original
 * cwd brings the conversation back. The backend already excludes any session whose
 * tmux session is still live, so everything here is genuinely orphaned.
 *
 * Restore reuses the workspace store's `recall` action (the same spawn path the
 * sidebar's Recent recall uses): it spawns a terminal rooted at `cwd` running
 * `claude --resume <sessionId>`, places the tile in the active tab, and focuses it.
 * Double-resume guard: once a row is restored (or if a live terminal is already
 * running in that cwd) it shows "Restored" and disables, so a resume can't be
 * fired twice for the same session from this panel.
 */
function ResumableSessionsSection({
  sessions,
  liveTerminals,
  onRestore,
}: {
  sessions: OrphanedSession[] | null;
  liveTerminals: TerminalInfo[];
  onRestore: (s: OrphanedSession) => Promise<TerminalId | null>;
}) {
  // Sessions restored from THIS panel (so a second click is a no-op + clearly
  // labeled). Keyed by sessionId.
  const [restored, setRestored] = useState<Set<string>>(() => new Set());
  const [busy, setBusy] = useState<string | null>(null);

  // A live terminal already running in a session's cwd is a strong signal that
  // it may already be placed/resumed — guard against a duplicate resume.
  const liveCwds = useMemo(() => {
    const s = new Set<string>();
    for (const t of liveTerminals) if (t.cwd) s.add(t.cwd.replace(/\/+$/, ""));
    return s;
  }, [liveTerminals]);

  const doRestore = useCallback(
    async (s: OrphanedSession) => {
      setBusy(s.sessionId);
      try {
        await onRestore(s);
        setRestored((prev) => new Set(prev).add(s.sessionId));
      } finally {
        setBusy(null);
      }
    },
    [onRestore],
  );

  // Still loading, or genuinely nothing to restore — render nothing.
  if (sessions == null || sessions.length === 0) return null;

  return (
    <div className="mt-5">
      <div className="text-xs font-semibold uppercase tracking-wide" style={{ color: "var(--th-fg)" }}>
        Resumable sessions ({sessions.length})
      </div>
      <p className="mb-2 mt-1 text-xs leading-snug" style={{ color: "var(--th-fg-muted)" }}>
        Claude sessions whose terminal didn&apos;t survive the last restart. Restore
        re-opens each in its original directory via{" "}
        <span className="font-mono">claude --resume</span>.
      </p>
      <ul className="rounded border" style={{ borderColor: "var(--th-border)" }}>
        {sessions.map((s) => {
          const alreadyLive = liveCwds.has(s.cwd.replace(/\/+$/, ""));
          const isRestored = restored.has(s.sessionId);
          const isBusy = busy === s.sessionId;
          const guarded = isRestored || alreadyLive;
          return (
            <li
              key={s.sessionId}
              className="flex items-center gap-2 border-b px-3 py-1.5 text-sm last:border-b-0"
              style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
            >
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ backgroundColor: "var(--th-dot-detached, #fbbf24)" }}
                title="Orphaned — resumable"
              />
              <div className="flex min-w-0 flex-col">
                <span className="truncate">{s.label}</span>
                <span className="truncate font-mono text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
                  {s.cwd} · {relativeAgo(s.lastSeen)}
                </span>
              </div>
              <div className="ml-auto shrink-0">
                {isRestored ? (
                  <span className="text-xs" style={{ color: "var(--th-dot-live, #4ade80)" }}>
                    Restored
                  </span>
                ) : (
                  <Btn
                    onClick={() => void doRestore(s)}
                    disabled={isBusy || guarded}
                    title={
                      alreadyLive
                        ? "A terminal is already running in this directory"
                        : `Resume: claude --resume in ${s.cwd}`
                    }
                    emphasis
                  >
                    {isBusy ? "Restoring…" : alreadyLive ? "Already open" : "Restore"}
                  </Btn>
                )}
              </div>
            </li>
          );
        })}
      </ul>
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
        T-Hub re-adopts them onto the active tab automatically. Listed here so you
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
