// FilePanel — the self-contained Files feature surface (PRD §6.8 reading, §9.7
// indexing; FR-014/015/016/017): a fuzzy search box with ranked results, a
// shallow lazy-expanding file tree, and a Markdown/plain-text reader.
//
// Self-contained on purpose: it owns all its state and talks to the backend
// only through the typed wrappers in ../ipc/files. It does NOT touch the
// workspace store, the canvas, or any 0.1/0.5 component, so it can be mounted
// anywhere (sidebar tab, split, separate window) by the integrator. See the
// mount note in the LEAD report — App.tsx wiring is intentionally left to the
// canvas/integration track.
//
// Props let the host drive the root (e.g. the selected terminal's worktree, per
// PRD §6.8.1). With no root it shows a gentle empty state.

import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import {
  indexProject,
  listDir,
  readTextFile,
  searchFiles,
  writeTextFile,
} from "../ipc/files";
import type { DirEntry, FileContents, FileHit } from "../ipc/types";
import { Markdown } from "./Markdown";

const MARKDOWN_EXTS = new Set(["md", "markdown", "mdx", "mdown", "markdn"]);

export interface FilePanelProps {
  /**
   * Project/worktree root to index and browse. When it changes (e.g. the user
   * selects a different terminal), the panel re-indexes and resets navigation.
   * If omitted, the panel renders an empty state prompting for a root.
   */
  root?: string;
  /** Optional starting absolute path to open in the reader. */
  initialFile?: string;
  /** Optional max results for a search query (default 50). */
  searchLimit?: number;
  /**
   * When true, render ONLY the reader for `initialFile` — no header, no search
   * box, no internal file tree. Used when a host (e.g. FileTree in embedReader
   * mode) already owns navigation and just needs FilePanel as a pure reader,
   * avoiding a duplicate search box + tree. Requires `initialFile`.
   */
  readerOnly?: boolean;
  className?: string;
}

/** The active right-pane content. */
type ReaderState =
  | { status: "empty" }
  | { status: "loading"; path: string }
  | { status: "error"; path: string; message: string }
  | { status: "ready"; contents: FileContents; mode: ReaderMode };

type ReaderMode = "rendered" | "source";

export function FilePanel({
  root,
  initialFile,
  searchLimit = 50,
  readerOnly = false,
  className,
}: FilePanelProps) {
  const [indexState, setIndexState] = useState<
    | { status: "idle" }
    | { status: "indexing" }
    | { status: "ready"; count: number; root: string }
    | { status: "error"; message: string }
  >({ status: "idle" });

  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<FileHit[]>([]);
  const [searching, setSearching] = useState(false);
  const [reader, setReader] = useState<ReaderState>({ status: "empty" });
  const [activePath, setActivePath] = useState<string | null>(null);

  // --- Indexing: (re)index whenever the root changes. --------------------
  // Skipped entirely in reader-only mode — the host owns navigation/search, so
  // this instance is a pure reader and never builds its own index.
  useEffect(() => {
    let cancelled = false;
    if (readerOnly) return;
    if (!root) {
      setIndexState({ status: "idle" });
      setHits([]);
      setQuery("");
      setReader({ status: "empty" });
      setActivePath(null);
      return;
    }
    setIndexState({ status: "indexing" });
    setReader({ status: "empty" });
    setActivePath(null);
    setQuery("");
    setHits([]);
    indexProject(root)
      .then((summary) => {
        if (cancelled) return;
        setIndexState({
          status: "ready",
          count: summary.count,
          root: summary.root,
        });
      })
      .catch((e) => {
        if (cancelled) return;
        setIndexState({ status: "error", message: String(e) });
      });
    return () => {
      cancelled = true;
    };
  }, [root, readerOnly]);

  // --- Open a file into the reader. --------------------------------------
  const openFile = useCallback((path: string) => {
    setActivePath(path);
    setReader({ status: "loading", path });
    readTextFile(path)
      .then((contents) => {
        const isMd = MARKDOWN_EXTS.has(contents.ext);
        setReader({
          status: "ready",
          contents,
          mode: isMd ? "rendered" : "source",
        });
      })
      .catch((e) => {
        setReader({ status: "error", path, message: String(e) });
      });
  }, []);

  // After the editor saves, reflect the new text (and byte size) in the open
  // reader so the view + the next edit's baseline match what's now on disk.
  const onSaved = useCallback((path: string, text: string) => {
    setReader((r) =>
      r.status === "ready" && r.contents.path === path
        ? { ...r, contents: { ...r.contents, text, size: new Blob([text]).size } }
        : r,
    );
  }, []);

  // Open the initial file. In reader-only mode there is no index, so open it
  // immediately; otherwise wait until the index is ready (so the surrounding
  // search/tree are live by the time the reader fills).
  const openedInitial = useRef(false);
  useEffect(() => {
    if (
      !openedInitial.current &&
      initialFile &&
      (readerOnly || indexState.status === "ready")
    ) {
      openedInitial.current = true;
      openFile(initialFile);
    }
  }, [initialFile, indexState.status, openFile, readerOnly]);

  // --- Debounced fuzzy search. -------------------------------------------
  useEffect(() => {
    if (!root || indexState.status !== "ready") {
      setHits([]);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const handle = setTimeout(() => {
      searchFiles(root, query, searchLimit)
        .then((res) => {
          if (!cancelled) setHits(res);
        })
        .catch(() => {
          if (!cancelled) setHits([]);
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, 90);
    return () => {
      cancelled = true;
      clearTimeout(handle);
    };
  }, [query, root, indexState.status, searchLimit]);

  // Reader-only: just the reader pane (no header/search/tree). The host owns
  // navigation; this is a pure file viewer for `initialFile`.
  if (readerOnly) {
    return (
      <PanelShell className={className}>
        <Reader
          reader={reader}
          onSetMode={(mode) =>
            setReader((r) => (r.status === "ready" ? { ...r, mode } : r))
          }
          onSaved={onSaved}
        />
      </PanelShell>
    );
  }

  if (!root) {
    return (
      <PanelShell className={className}>
        <div
          className="flex h-full items-center justify-center px-6 text-center text-sm"
          style={{ color: "var(--th-fg-muted)" }}
        >
          No project selected. Select a terminal/worktree to browse its files.
        </div>
      </PanelShell>
    );
  }

  return (
    <PanelShell className={className}>
      {/* Header: root + index status. */}
      <Header root={root} indexState={indexState} />

      <div className="flex min-h-0 flex-1">
        {/* Left rail: search + results, or the tree when no query. */}
        <div
          className="flex w-72 shrink-0 flex-col border-r"
          style={{ borderColor: "var(--th-border)" }}
        >
          <div
            className="border-b p-2"
            style={{ borderColor: "var(--th-border)" }}
          >
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Fuzzy search files…"
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
              className="w-full px-2.5 py-1.5 text-sm focus:outline-none"
              style={{
                borderRadius: "var(--th-radius)",
                border: "1px solid var(--th-border)",
                background: "var(--th-tile-bg)",
                color: "var(--th-fg)",
              }}
            />
          </div>
          <div className="th-scroll min-h-0 flex-1 overflow-y-auto">
            {query.trim() ? (
              <SearchResults
                hits={hits}
                searching={searching}
                activePath={activePath}
                root={indexState.status === "ready" ? indexState.root : root}
                onOpen={openFile}
              />
            ) : (
              <FileTree
                root={indexState.status === "ready" ? indexState.root : root}
                activePath={activePath}
                onOpenFile={openFile}
              />
            )}
          </div>
        </div>

        {/* Right pane: the reader. */}
        <div className="min-h-0 min-w-0 flex-1">
          <Reader
            reader={reader}
            onSetMode={(mode) =>
              setReader((r) => (r.status === "ready" ? { ...r, mode } : r))
            }
            onSaved={onSaved}
          />
        </div>
      </div>
    </PanelShell>
  );
}

// --- Shell + header --------------------------------------------------------

function PanelShell({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={"flex h-full min-h-0 w-full flex-col " + (className ?? "")}
      style={{ background: "var(--th-sidebar-bg)", color: "var(--th-fg)" }}
    >
      {children}
    </div>
  );
}

function Header({
  root,
  indexState,
}: {
  root: string;
  indexState:
    | { status: "idle" }
    | { status: "indexing" }
    | { status: "ready"; count: number; root: string }
    | { status: "error"; message: string };
}) {
  const label = basename(root) || root;
  return (
    <div
      className="flex items-center justify-between gap-3 border-b px-3 py-2"
      style={{ borderColor: "var(--th-border)" }}
    >
      <div className="min-w-0">
        <div
          className="truncate text-sm font-medium"
          style={{ color: "var(--th-fg)" }}
          title={root}
        >
          {label}
        </div>
        <div
          className="truncate text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
          title={root}
        >
          {root}
        </div>
      </div>
      <div className="shrink-0 text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
        {indexState.status === "indexing" && (
          <span style={{ color: "var(--th-dot-starting)" }}>indexing…</span>
        )}
        {indexState.status === "ready" && (
          <span title="files indexed">{indexState.count} files</span>
        )}
        {indexState.status === "error" && (
          <span style={{ color: "var(--th-dot-error)" }} title={indexState.message}>
            index error
          </span>
        )}
      </div>
    </div>
  );
}

// --- Search results --------------------------------------------------------

function SearchResults({
  hits,
  searching,
  activePath,
  root,
  onOpen,
}: {
  hits: FileHit[];
  searching: boolean;
  activePath: string | null;
  root: string;
  onOpen: (absPath: string) => void;
}) {
  if (!searching && hits.length === 0) {
    return (
      <div className="px-3 py-2 text-xs" style={{ color: "var(--th-fg-muted)" }}>
        No matching files.
      </div>
    );
  }
  return (
    <ul className="py-1">
      {hits.map((hit) => {
        const abs = joinPath(root, hit.relPath);
        const dir = hit.relPath.includes("/")
          ? hit.relPath.slice(0, hit.relPath.lastIndexOf("/"))
          : "";
        const active = abs === activePath;
        return (
          <li key={hit.relPath}>
            <button
              type="button"
              onClick={() => onOpen(abs)}
              className="flex w-full items-baseline gap-2 px-3 py-1 text-left text-sm"
              style={{ background: active ? "var(--th-tile-bg)" : "transparent" }}
              onMouseEnter={(e) => {
                if (!active) e.currentTarget.style.background = "var(--th-tile-bg)";
              }}
              onMouseLeave={(e) => {
                if (!active) e.currentTarget.style.background = "transparent";
              }}
              title={hit.relPath}
            >
              <span className="truncate" style={{ color: "var(--th-fg)" }}>
                {hit.isKeyFile && (
                  <span className="mr-1" style={{ color: "var(--th-accent)" }}>
                    ★
                  </span>
                )}
                {hit.basename}
              </span>
              {dir && (
                <span
                  className="min-w-0 flex-1 truncate text-[11px]"
                  style={{ color: "var(--th-fg-muted)" }}
                >
                  {dir}
                </span>
              )}
            </button>
          </li>
        );
      })}
    </ul>
  );
}

// --- File tree (shallow, lazy-expanding) -----------------------------------

function FileTree({
  root,
  activePath,
  onOpenFile,
}: {
  root: string;
  activePath: string | null;
  onOpenFile: (absPath: string) => void;
}) {
  return (
    <div className="py-1">
      <TreeDir
        path={root}
        name={basename(root) || root}
        depth={0}
        defaultOpen
        activePath={activePath}
        onOpenFile={onOpenFile}
      />
    </div>
  );
}

function TreeDir({
  path,
  name,
  depth,
  defaultOpen,
  activePath,
  onOpenFile,
}: {
  path: string;
  name: string;
  depth: number;
  defaultOpen?: boolean;
  activePath: string | null;
  onOpenFile: (absPath: string) => void;
}) {
  const [open, setOpen] = useState(!!defaultOpen);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Lazily load children the first time the dir is opened (PRD §9.7: folder
  // expansion is UI state + a shallow list, not a full rescan).
  useEffect(() => {
    if (!open || entries !== null || loading) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    listDir(path)
      .then((res) => {
        if (!cancelled) setEntries(res);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, path, entries, loading]);

  const indent = { paddingLeft: `${depth * 12 + 8}px` };

  return (
    <div>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        style={indent}
        className="flex w-full items-center gap-1.5 py-1 pr-2 text-left text-sm"
        onMouseEnter={(e) => (e.currentTarget.style.background = "var(--th-tile-bg)")}
        onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
        title={path}
      >
        <span className="w-3 shrink-0 text-[10px]" style={{ color: "var(--th-fg-muted)" }}>
          {open ? "▾" : "▸"}
        </span>
        <span className="truncate" style={{ color: "var(--th-fg)" }}>
          {name}
        </span>
      </button>
      {open && (
        <div>
          {loading && (
            <div
              style={{ paddingLeft: `${(depth + 1) * 12 + 14}px`, color: "var(--th-fg-muted)" }}
              className="py-0.5 text-[11px]"
            >
              loading…
            </div>
          )}
          {error && (
            <div
              style={{ paddingLeft: `${(depth + 1) * 12 + 14}px`, color: "var(--th-dot-error)" }}
              className="py-0.5 text-[11px]"
              title={error}
            >
              error
            </div>
          )}
          {entries?.map((entry) =>
            entry.isDir ? (
              <TreeDir
                key={entry.path}
                path={entry.path}
                name={entry.name}
                depth={depth + 1}
                activePath={activePath}
                onOpenFile={onOpenFile}
              />
            ) : (
              <TreeFile
                key={entry.path}
                entry={entry}
                depth={depth + 1}
                active={entry.path === activePath}
                onOpen={onOpenFile}
              />
            ),
          )}
        </div>
      )}
    </div>
  );
}

function TreeFile({
  entry,
  depth,
  active,
  onOpen,
}: {
  entry: DirEntry;
  depth: number;
  active: boolean;
  onOpen: (absPath: string) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onOpen(entry.path)}
      style={{
        paddingLeft: `${depth * 12 + 22}px`,
        background: active ? "var(--th-tile-bg)" : "transparent",
        color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
      }}
      className="flex w-full items-center py-1 pr-2 text-left text-sm"
      onMouseEnter={(e) => {
        if (!active) e.currentTarget.style.background = "var(--th-tile-bg)";
      }}
      onMouseLeave={(e) => {
        if (!active) e.currentTarget.style.background = "transparent";
      }}
      title={entry.path}
    >
      <span className="truncate">{entry.name}</span>
    </button>
  );
}

// --- Reader ----------------------------------------------------------------

function Reader({
  reader,
  onSetMode,
  onSaved,
}: {
  reader: ReaderState;
  onSetMode: (mode: ReaderMode) => void;
  /** Called after a successful save so the host can refresh the open contents. */
  onSaved?: (path: string, text: string) => void;
}) {
  // Edit state lives here (hooks must precede the early returns below). Editing
  // swaps the read-only <pre>/markdown view for a textarea; Save writes back via
  // writeTextFile. Reset whenever the shown file changes.
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const shownPath =
    reader.status === "ready"
      ? reader.contents.path
      : reader.status === "loading" || reader.status === "error"
        ? reader.path
        : null;
  useEffect(() => {
    setEditing(false);
    setSaving(false);
    setSaveError(null);
  }, [shownPath]);

  if (reader.status === "empty") {
    return (
      <div
        className="flex h-full items-center justify-center px-6 text-center text-sm"
        style={{ color: "var(--th-fg-muted)" }}
      >
        Select a file to read.
      </div>
    );
  }
  if (reader.status === "loading") {
    return (
      <div
        className="flex h-full items-center justify-center text-sm"
        style={{ color: "var(--th-fg-muted)" }}
      >
        loading {basename(reader.path)}…
      </div>
    );
  }
  if (reader.status === "error") {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-1 px-6 text-center">
        <div className="text-sm" style={{ color: "var(--th-dot-error)" }}>
          Could not open file
        </div>
        <div
          className="max-w-md break-words text-xs"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {reader.message}
        </div>
      </div>
    );
  }

  const { contents, mode } = reader;
  const isMd = MARKDOWN_EXTS.has(contents.ext);
  // A truncated (capped) read must NOT be editable — saving would write back the
  // partial buffer and lose the rest of the file.
  const canEdit = !contents.truncated;
  const dirty = editing && draft !== contents.text;

  const startEdit = () => {
    setDraft(contents.text);
    setSaveError(null);
    setEditing(true);
  };
  const cancelEdit = () => {
    setEditing(false);
    setSaveError(null);
  };
  const save = () => {
    setSaving(true);
    setSaveError(null);
    writeTextFile(contents.path, draft)
      .then(() => {
        setSaving(false);
        setEditing(false);
        onSaved?.(contents.path, draft);
      })
      .catch((e) => {
        setSaving(false);
        setSaveError(String(e));
      });
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Reader toolbar: filename, size/dirty, and edit/save controls. */}
      <div
        className="flex items-center justify-between gap-3 border-b px-3 py-1.5"
        style={{ borderColor: "var(--th-border)" }}
      >
        <div
          className="min-w-0 truncate text-xs"
          style={{ color: "var(--th-fg)" }}
          title={contents.path}
        >
          {basename(contents.path)}
          {contents.truncated && (
            <span
              className="ml-2"
              style={{ color: "var(--th-dot-starting)" }}
              title="file exceeded the read cap; editing is disabled to avoid saving a partial file"
            >
              (truncated)
            </span>
          )}
          {dirty && (
            <span
              className="ml-2"
              style={{ color: "var(--th-dot-starting)" }}
              title="unsaved changes"
            >
              ●
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!editing && (
            <span className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
              {formatBytes(contents.size)}
            </span>
          )}
          {!editing && isMd && (
            <div
              className="flex overflow-hidden text-[11px]"
              style={{
                borderRadius: "var(--th-radius)",
                border: "1px solid var(--th-border)",
              }}
            >
              <ToggleButton
                active={mode === "rendered"}
                onClick={() => onSetMode("rendered")}
                label="Rendered"
              />
              <ToggleButton
                active={mode === "source"}
                onClick={() => onSetMode("source")}
                label="Source"
              />
            </div>
          )}
          {!editing && canEdit && <EditBtn onClick={startEdit} label="Edit" />}
          {editing && (
            <>
              <EditBtn
                onClick={save}
                label={saving ? "Saving…" : "Save"}
                disabled={saving || !dirty}
                primary
              />
              <EditBtn onClick={cancelEdit} label="Cancel" disabled={saving} />
            </>
          )}
        </div>
      </div>

      {saveError && (
        <div
          className="border-b px-3 py-1 text-[11px]"
          style={{ borderColor: "var(--th-border)", color: "var(--th-dot-error)" }}
          title={saveError}
        >
          Save failed: {saveError}
        </div>
      )}

      {/* Body: an editable textarea while editing, else the read-only view. */}
      <div className="th-scroll min-h-0 flex-1 overflow-auto">
        {editing ? (
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            spellCheck={false}
            autoCorrect="off"
            autoCapitalize="off"
            className="h-full w-full resize-none bg-transparent px-4 py-3 font-mono text-[12.5px] leading-relaxed outline-none"
            style={{ color: "var(--th-fg)" }}
            onKeyDown={(e) => {
              // Ctrl/Cmd+S saves without leaving the editor.
              if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
                e.preventDefault();
                if (dirty && !saving) save();
              }
            }}
          />
        ) : isMd && mode === "rendered" ? (
          <div className="px-5 py-4">
            <Markdown source={contents.text} />
          </div>
        ) : (
          <pre
            className="px-4 py-3 font-mono text-[12.5px] leading-relaxed"
            style={{ color: "var(--th-fg)" }}
          >
            <code>{contents.text}</code>
          </pre>
        )}
      </div>
    </div>
  );
}

/** A small toolbar button for the editor (Edit / Save / Cancel). */
function EditBtn({
  onClick,
  label,
  disabled,
  primary,
}: {
  onClick: () => void;
  label: string;
  disabled?: boolean;
  primary?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="rounded border px-2 py-0.5 text-[11px] transition-colors hover:bg-neutral-700/30 disabled:cursor-not-allowed disabled:opacity-40"
      style={{
        borderColor: primary ? "var(--th-accent)" : "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
      {label}
    </button>
  );
}

function ToggleButton({
  active,
  onClick,
  label,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="px-2 py-0.5"
      style={{
        background: active ? "var(--th-tile-bg)" : "transparent",
        color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
      }}
    >
      {label}
    </button>
  );
}

// --- Path helpers (string-based; paths may be WSL/posix absolute) ----------

/** Final path component (handles both `/` and `\`). */
function basename(p: string): string {
  const norm = p.replace(/\\/g, "/").replace(/\/+$/, "");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

/** Join an absolute root with a `/`-separated relative path. */
function joinPath(root: string, rel: string): string {
  const base = root.replace(/\/+$/, "");
  return rel ? `${base}/${rel}` : base;
}

/** Human-readable byte size. */
function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
