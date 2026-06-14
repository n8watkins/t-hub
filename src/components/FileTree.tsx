// FileTree — a self-contained, mountable file-tree sidebar surface for one
// project root (PRD §6.8 reading, §9.7 indexing; FR-014/015/016/017).
//
// It owns all its state and talks to the backend only through the typed
// wrappers in ../ipc/files. It does NOT touch the workspace store, the canvas,
// Sidebar, or App, so the integrator can drop it anywhere (a sidebar tab, a
// split, a popped-out window). Mounting is the host's job — see the prop notes
// below; this component never wires itself in.
//
// What it does:
//   - Indexes `root` (and re-indexes when `root` changes), surfacing a compact
//     status (indexing / N files / error) in a small header.
//   - A debounced fuzzy filename search box (search_files) whose ranked results
//     replace the tree while the box is non-empty.
//   - A shallow, lazy-expanding directory tree (list_dir on first expand only;
//     folder expansion is UI state, not a rescan — PRD §9.7).
//   - Click-to-open: every file row calls `onOpenFile(absolutePath)`. The host
//     routes that to a reader (e.g. <FilePanel initialFile=… />, a canvas tab,
//     or a popped-out window). If no `onOpenFile` is given, the tree manages an
//     internal selection and (when `embedReader` is set) renders the FilePanel
//     reader inline so it works as a complete, drop-in Files panel by itself.
//
// Theming: every surface reads `var(--th-*)` tokens (live-customizable, no
// reload) and scroll regions use the on-brand `th-scroll` class, matching the
// rest of the chrome.

import {
  type CSSProperties,
  type ReactNode,
  useCallback,
  useEffect,
  useState,
} from "react";
import { indexProject, listDir, searchFiles } from "../ipc/files";
import type { DirEntry, FileHit } from "../ipc/types";
import { FilePanel } from "./FilePanel";

export interface FileTreeProps {
  /**
   * Project/worktree root to index and browse (an absolute path; a WSL path
   * like `/home/you/proj` is fine — the backend routes it to the host). When it
   * changes the tree re-indexes and resets navigation. With no root it renders a
   * gentle empty state.
   */
  root?: string;
  /**
   * Called with the absolute path of a file when the user clicks it. The host
   * routes this to a reader (a <FilePanel initialFile=… />, a canvas tab, a
   * popped-out window, …). If omitted, the tree tracks selection internally and
   * — when `embedReader` is true — renders the FilePanel reader inline.
   */
  onOpenFile?: (absPath: string) => void;
  /**
   * When true (and `onOpenFile` is NOT provided), render the FilePanel reader to
   * the right of the tree so this component is a complete Files panel on its own.
   * Ignored when `onOpenFile` is provided (the host owns the reader then).
   */
  embedReader?: boolean;
  /** The currently-open file (absolute path), to highlight its row. Optional. */
  activePath?: string | null;
  /** Max results for a search query (default 50). */
  searchLimit?: number;
  className?: string;
}

type IndexState =
  | { status: "idle" }
  | { status: "indexing" }
  | { status: "ready"; count: number; root: string }
  | { status: "error"; message: string };

export function FileTree({
  root,
  onOpenFile,
  embedReader = false,
  activePath,
  searchLimit = 50,
  className,
}: FileTreeProps) {
  const [indexState, setIndexState] = useState<IndexState>({ status: "idle" });
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<FileHit[]>([]);
  const [searching, setSearching] = useState(false);
  // Internal selection only used when the host doesn't own opening (no
  // onOpenFile). Lets the embedded reader work as a drop-in panel.
  const [internalPath, setInternalPath] = useState<string | null>(null);

  // Resolved, normalized root reported by the indexer (matches the abs paths
  // list_dir returns), falling back to the prop before indexing completes.
  const indexedRoot =
    indexState.status === "ready" ? indexState.root : (root ?? "");
  const selectedPath = activePath ?? (onOpenFile ? null : internalPath);

  // (Re)index whenever the root changes. -----------------------------------
  useEffect(() => {
    let cancelled = false;
    if (!root) {
      setIndexState({ status: "idle" });
      setHits([]);
      setQuery("");
      setInternalPath(null);
      return;
    }
    setIndexState({ status: "indexing" });
    setHits([]);
    setQuery("");
    setInternalPath(null);
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
  }, [root]);

  // Open a file: hand it to the host, or track it internally for the embedded
  // reader. -----------------------------------------------------------------
  const open = useCallback(
    (absPath: string) => {
      if (onOpenFile) onOpenFile(absPath);
      else setInternalPath(absPath);
    },
    [onOpenFile],
  );

  // Debounced fuzzy search. -------------------------------------------------
  useEffect(() => {
    if (!root || indexState.status !== "ready") {
      setHits([]);
      setSearching(false);
      return;
    }
    if (!query.trim()) {
      setHits([]);
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const handle = window.setTimeout(() => {
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
      window.clearTimeout(handle);
    };
  }, [query, root, indexState.status, searchLimit]);

  if (!root) {
    return (
      <Shell className={className}>
        <div
          className="flex h-full items-center justify-center px-6 text-center text-sm"
          style={{ color: "var(--th-fg-muted)" }}
        >
          No project selected. Pick a terminal/worktree to browse its files.
        </div>
      </Shell>
    );
  }

  const tree = (
    <div className="flex h-full min-h-0 w-full flex-col">
      <Header root={root} indexState={indexState} />

      {/* Search box. */}
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
          onFocus={(e) => {
            e.currentTarget.style.borderColor = "var(--th-focus-ring)";
          }}
          onBlur={(e) => {
            e.currentTarget.style.borderColor = "var(--th-border)";
          }}
        />
      </div>

      {/* Body: search results when querying, else the lazy tree. */}
      <div className="th-scroll min-h-0 flex-1 overflow-y-auto">
        {query.trim() ? (
          <SearchResults
            hits={hits}
            searching={searching}
            activePath={selectedPath}
            root={indexedRoot}
            onOpen={open}
          />
        ) : (
          <TreeRoot
            root={indexedRoot}
            activePath={selectedPath}
            onOpenFile={open}
          />
        )}
      </div>
    </div>
  );

  // Embedded-reader mode: tree on the left, FilePanel reader on the right.
  // Only used when the host hasn't claimed opening via onOpenFile.
  if (embedReader && !onOpenFile) {
    return (
      <Shell className={className}>
        <div className="flex min-h-0 flex-1">
          <div
            className="flex w-72 shrink-0 flex-col border-r"
            style={{ borderColor: "var(--th-border)" }}
          >
            {tree}
          </div>
          <div className="min-h-0 min-w-0 flex-1">
            {/* FilePanel with an explicit initialFile acts as a pure reader; we
             * give it the SAME root so its own indexing is a cache hit. The
             * key forces a fresh reader load when the selection changes. */}
            <FilePanel
              key={internalPath ?? "none"}
              root={root}
              initialFile={internalPath ?? undefined}
            />
          </div>
        </div>
      </Shell>
    );
  }

  return <Shell className={className}>{tree}</Shell>;
}

// --- Shell + header --------------------------------------------------------

function Shell({
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
  indexState: IndexState;
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
      <div
        className="shrink-0 text-[11px]"
        style={{ color: "var(--th-fg-muted)" }}
      >
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
            <Row active={active} onClick={() => onOpen(abs)} title={hit.relPath}>
              <span className="flex items-baseline gap-2">
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
              </span>
            </Row>
          </li>
        );
      })}
    </ul>
  );
}

// --- Lazy directory tree ---------------------------------------------------

function TreeRoot({
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

  // Lazily list children the first time the dir opens (PRD §9.7: shallow list,
  // not a rescan). Re-listing on re-open is avoided by caching `entries`.
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

  const indent: CSSProperties = { paddingLeft: `${depth * 12 + 8}px` };

  return (
    <div>
      <Row
        onClick={() => setOpen((v) => !v)}
        title={path}
        style={indent}
      >
        <span className="flex items-center gap-1.5">
          <span
            className="w-3 shrink-0 text-[10px]"
            style={{ color: "var(--th-fg-muted)" }}
          >
            {open ? "▾" : "▸"}
          </span>
          <span className="truncate" style={{ color: "var(--th-fg)" }}>
            {name}
          </span>
        </span>
      </Row>
      {open && (
        <div>
          {loading && (
            <Hint depth={depth + 1}>loading…</Hint>
          )}
          {error && (
            <Hint depth={depth + 1} color="var(--th-dot-error)" title={error}>
              error
            </Hint>
          )}
          {entries?.length === 0 && !loading && (
            <Hint depth={depth + 1}>empty</Hint>
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
    <Row
      active={active}
      onClick={() => onOpen(entry.path)}
      title={entry.path}
      style={{ paddingLeft: `${depth * 12 + 22}px` }}
    >
      <span
        className="truncate"
        style={{ color: active ? "var(--th-fg)" : "var(--th-fg-muted)" }}
      >
        {entry.name}
      </span>
    </Row>
  );
}

// --- Small themed primitives ----------------------------------------------

/** A clickable, hover-highlighting, full-width row used across the tree +
 * search results. Active rows get the tile background; hover uses it too. */
function Row({
  children,
  onClick,
  title,
  active,
  style,
}: {
  children: ReactNode;
  onClick: () => void;
  title?: string;
  active?: boolean;
  style?: CSSProperties;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className="block w-full py-1 pr-2 text-left text-sm"
      style={{
        background: active ? "var(--th-tile-bg)" : "transparent",
        ...style,
      }}
      onMouseEnter={(e) => {
        if (!active) e.currentTarget.style.background = "var(--th-tile-bg)";
      }}
      onMouseLeave={(e) => {
        if (!active) e.currentTarget.style.background = "transparent";
      }}
    >
      {children}
    </button>
  );
}

function Hint({
  children,
  depth,
  color,
  title,
}: {
  children: ReactNode;
  depth: number;
  color?: string;
  title?: string;
}) {
  return (
    <div
      style={{
        paddingLeft: `${depth * 12 + 14}px`,
        color: color ?? "var(--th-fg-muted)",
      }}
      className="py-0.5 text-[11px]"
      title={title}
    >
      {children}
    </div>
  );
}

// --- Path helpers (string-based; paths may be WSL/posix or Windows abs) -----

/** Final path component (handles both `/` and `\`). */
function basename(p: string): string {
  const norm = p.replace(/\\/g, "/").replace(/\/+$/, "");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

/** Join an absolute root with a `/`-separated relative path. Preserves the
 * root's own separator style so abs paths match what list_dir returns. */
function joinPath(root: string, rel: string): string {
  if (!rel) return root;
  const sep = root.includes("\\") && !root.includes("/") ? "\\" : "/";
  const base = root.replace(/[\\/]+$/, "");
  return `${base}${sep}${sep === "\\" ? rel.replace(/\//g, "\\") : rel}`;
}
