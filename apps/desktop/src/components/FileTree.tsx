// FileTree — a self-contained, mountable file-tree sidebar surface for one
// project root (PRD §6.8 reading, §9.7 indexing; FR-014/015/016/017).
//
// It owns all its state and talks to the backend only through the typed
// wrappers in ../ipc/files. It does NOT touch the workspace store, the canvas,
// Sidebar, or App, so the integrator can drop it anywhere (a sidebar tab, a
// split, a popped-out window). Mounting is the host's job — see the prop notes
// below; this component never wires itself in.
//
// What it does (VS Code sidebar feel — instant, navigable, search-on-top):
//   - A **search box at the very top**. Empty → the folder tree shows; non-empty
//     → ranked fuzzy filename results (search_files) replace it.
//   - A **real, navigable folder tree shown IMMEDIATELY** from list_dir: the
//     root's children render as soon as the first shallow list returns (folders
//     first, then files). Clicking a folder expands/collapses it with its own
//     lazy, cached list_dir — never a rescan (PRD §9.7). The tree is usable the
//     instant the root's first list_dir returns and is NEVER blocked on the
//     project index.
//   - Indexing runs **in the background, only to power search**. We surface it as
//     a tiny, non-blocking count/spinner next to the search box — never a
//     blocking overlay over the tree.
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
  useRef,
  useState,
} from "react";
import { listDir, searchFiles } from "../ipc/files";
import { tlog } from "../lib/diag";
import type { DirEntry, FileHit } from "../ipc/types";
import { FilePanel } from "./FilePanel";
import { PreviewOverlay } from "./PreviewOverlay";
import { WebPreview } from "./WebPreview";

export interface FileTreeProps {
  /**
   * Project/worktree root to browse (an absolute path; a WSL path like
   * `/home/you/proj` is fine — the backend routes it to the host). When it
   * changes the tree resets navigation and re-indexes in the background. With no
   * root it renders a gentle empty state.
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
   * Ignored when `onOpenFile` is provided (the host owns the reader then), or
   * when `previewInOverlay` is on (the overlay takes over opening instead).
   */
  embedReader?: boolean;
  /**
   * When true (the default, and only when `onOpenFile` is NOT provided), clicking
   * a file opens it in a large, centered <PreviewOverlay> over the whole app
   * (like the Settings modal) using the FilePanel reader — rather than the inline
   * `embedReader` split. This also surfaces a small "Web preview" affordance in
   * the search bar that opens a <WebPreview> in the same overlay. Set false to
   * keep the old inline-reader behavior. Ignored when `onOpenFile` is provided.
   */
  previewInOverlay?: boolean;
  /** The currently-open file (absolute path), to highlight its row. Optional. */
  activePath?: string | null;
  /** Max results for a search query (default 50). */
  searchLimit?: number;
  className?: string;
}

/** Background index state — only ever surfaced as a tiny, non-blocking hint
 * next to the search box. The tree never waits on this. */
type IndexState =
  | { status: "idle" }
  | { status: "indexing" }
  | { status: "ready"; count: number; root: string }
  | { status: "error"; message: string };

/** What the self-contained preview overlay is currently showing (when the host
 *  doesn't own opening and `previewInOverlay` is on). `null` = overlay closed. */
type Preview =
  | { kind: "file"; path: string }
  | { kind: "web" }
  | null;

export function FileTree({
  root,
  onOpenFile,
  embedReader = false,
  previewInOverlay = true,
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
  // The self-contained preview overlay's current content (file or web), or null
  // when closed. Only used when this component owns opening (no onOpenFile) and
  // `previewInOverlay` is on. Mounted once at the bottom of the tree's Shell.
  const [preview, setPreview] = useState<Preview>(null);
  // Whether THIS instance routes opens through the centered overlay. The host
  // owning opens (onOpenFile) always wins; otherwise the overlay is the default.
  const useOverlay = !onOpenFile && previewInOverlay;

  // Normalized root reported by the background indexer (matches the abs paths
  // search_files relative paths join onto). Falls back to the prop until the
  // index lands — the tree itself uses `root` directly via list_dir, so it does
  // not depend on this.
  const indexedRoot =
    indexState.status === "ready" ? indexState.root : (root ?? "");
  // Highlight the row of whatever's currently open: the host's activePath, else
  // the file showing in the overlay, else the inline reader's selection.
  const previewPath = preview?.kind === "file" ? preview.path : null;
  const selectedPath =
    activePath ?? (onOpenFile ? null : (previewPath ?? internalPath));

  // Reset transient UI when the root changes. We deliberately do NOT index here.
  // The tree renders straight from shallow list_dir calls (instant, per-folder);
  // a full-tree index only exists to power SEARCH, and walking a big root (e.g.
  // all of ~/n8builds) on every mount made the panel feel like it was "indexing
  // everything at once". So indexing is now lazy — it runs on the FIRST search
  // (searchFiles builds the index on demand server-side; see the search effect),
  // not on mount. Browsing folders never triggers it.
  useEffect(() => {
    setQuery("");
    setHits([]);
    setSearching(false);
    setInternalPath(null);
    setPreview(null);
    setIndexState({ status: "idle" });
  }, [root]);

  // Open a file: hand it to the host (onOpenFile wins), else open it in the
  // centered preview overlay (the default), else track it internally for the
  // inline embedded reader. ------------------------------------------------
  const open = useCallback(
    (absPath: string) => {
      if (onOpenFile) onOpenFile(absPath);
      else if (previewInOverlay) setPreview({ kind: "file", path: absPath });
      else setInternalPath(absPath);
    },
    [onOpenFile, previewInOverlay],
  );

  // Debounced fuzzy search. The backend indexes on demand, so search works even
  // before the background index lands — we don't gate on indexState here.
  useEffect(() => {
    if (!root || !query.trim()) {
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
  }, [query, root, searchLimit]);

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
      {/* Search box ABOVE the tree. The tiny index hint lives inside it so it
          never blocks or overlays the tree. The "Web preview" affordance only
          appears when this instance owns the centered overlay. */}
      <SearchBar
        query={query}
        onQuery={setQuery}
        indexState={indexState}
        onWebPreview={useOverlay ? () => setPreview({ kind: "web" }) : undefined}
      />

      {/* Body: search results when querying, else the instant folder tree. */}
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
            root={root}
            activePath={selectedPath}
            onOpenFile={open}
          />
        )}
      </div>
    </div>
  );

  // The self-contained preview overlay, mounted once and shared by every return
  // path below. It floats over the whole app (like Settings) and is the default
  // way files open when this component owns opening (no onOpenFile). Closing
  // (Esc / backdrop / ✕) clears `preview`.
  const overlay = useOverlay ? (
    <PreviewOverlay
      open={preview !== null}
      onClose={() => setPreview(null)}
      title={
        preview?.kind === "file"
          ? basenameOf(preview.path)
          : preview?.kind === "web"
            ? "Web preview"
            : ""
      }
      subtitle={preview?.kind === "file" ? preview.path : undefined}
    >
      {preview?.kind === "file" ? (
        // FilePanel as a PURE reader (readerOnly) — the overlay header already
        // shows the path; this just renders the file body (it caps large files
        // and rejects binary, surfacing a friendly error). Keyed by path so a
        // new selection loads fresh.
        <FilePanel
          key={preview.path}
          root={root}
          initialFile={preview.path}
          readerOnly
          className="h-full"
        />
      ) : preview?.kind === "web" ? (
        <WebPreview />
      ) : null}
    </PreviewOverlay>
  ) : null;

  // Inline embedded-reader mode: tree on the left, FilePanel reader on the
  // right. Only used when the host hasn't claimed opening AND the overlay is
  // disabled (previewInOverlay=false) — otherwise the overlay is the reader.
  if (embedReader && !onOpenFile && !useOverlay) {
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
            <FilePanel
              key={internalPath ?? "none"}
              root={root}
              initialFile={internalPath ?? undefined}
              readerOnly
            />
          </div>
        </div>
      </Shell>
    );
  }

  return (
    <Shell className={className}>
      {tree}
      {overlay}
    </Shell>
  );
}

/** Final path component (handles both `/` and `\`), for the overlay title. */
function basenameOf(p: string): string {
  const norm = p.replace(/\\/g, "/").replace(/\/+$/, "");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

// --- Shell -----------------------------------------------------------------

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

// --- Search bar (with the non-blocking index hint) -------------------------

function SearchBar({
  query,
  onQuery,
  indexState,
  onWebPreview,
}: {
  query: string;
  onQuery: (q: string) => void;
  indexState: IndexState;
  /** When provided, render a small "Web preview" affordance that opens the
   *  WebPreview surface in the shared overlay. Omitted = no button. */
  onWebPreview?: () => void;
}) {
  return (
    <div
      className="flex items-center gap-1.5 border-b px-2 py-1"
      style={{ borderColor: "var(--th-border)" }}
    >
      <div className="relative min-w-0 flex-1">
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder="Search files…"
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          className="w-full px-2 py-1 pr-7 text-xs focus:outline-none"
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
        {query && (
          <button
            type="button"
            onClick={() => onQuery("")}
            title="Clear search"
            aria-label="Clear search"
            className="absolute inset-y-0 right-1.5 my-auto flex h-5 w-5 items-center justify-center text-xs"
            style={{ color: "var(--th-fg-muted)" }}
          >
            ✕
          </button>
        )}
      </div>
      {/* Tiny, non-blocking index hint — purely informational, never gates the
          tree. */}
      <IndexHint indexState={indexState} />
      {/* "Web preview" affordance — opens a URL/iframe surface in the shared
          overlay. Only present when this instance owns the overlay. */}
      {onWebPreview && (
        <button
          type="button"
          onClick={onWebPreview}
          title="Preview a webpage (e.g. a local dev server)"
          aria-label="Web preview"
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded transition-colors hover:bg-neutral-700/40"
          style={{ color: "var(--th-fg-muted)" }}
        >
          <GlobeIcon />
        </button>
      )}
    </div>
  );
}

/** A small globe glyph for the Web-preview affordance. */
function GlobeIcon() {
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
      className="pointer-events-none"
      aria-hidden
    >
      <circle cx="12" cy="12" r="10" />
      <path d="M2 12h20" />
      <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
    </svg>
  );
}

function IndexHint({ indexState }: { indexState: IndexState }) {
  if (indexState.status === "indexing") {
    return (
      <span
        className="shrink-0 text-[11px]"
        style={{ color: "var(--th-dot-starting)" }}
        title="Building the search index in the background"
      >
        indexing…
      </span>
    );
  }
  if (indexState.status === "ready") {
    return (
      <span
        className="shrink-0 text-[11px] tabular-nums"
        style={{ color: "var(--th-fg-muted)" }}
        title="Files available to search"
      >
        {indexState.count}
      </span>
    );
  }
  if (indexState.status === "error") {
    return (
      <span
        className="shrink-0 text-[11px]"
        style={{ color: "var(--th-dot-error)" }}
        title={indexState.message}
      >
        index error
      </span>
    );
  }
  return null;
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
            <Row
              active={active}
              onClick={() => onOpen(abs)}
              title={hit.relPath}
              style={{ paddingLeft: "10px" }}
            >
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
//
// The root is rendered as an always-open container whose children come straight
// from list_dir — so the tree appears the instant that first shallow list
// returns, with zero dependence on the project index. Each folder owns its own
// lazy, cached list_dir; expanding/collapsing is pure UI state (PRD §9.7).

function TreeRoot({
  root,
  activePath,
  onOpenFile,
}: {
  root: string;
  activePath: string | null;
  onOpenFile: (absPath: string) => void;
}) {
  // Load the root's children immediately on mount (and whenever the root
  // changes) — this is the "instant tree". Keyed by `root` so a new root
  // refetches from scratch.
  const children = useDirEntries(root);

  return (
    <div className="py-1">
      <DirChildren
        state={children}
        depth={0}
        activePath={activePath}
        onOpenFile={onOpenFile}
      />
    </div>
  );
}

/** Internal hook: lazily (and once) list a directory's shallow children,
 * caching the result. `enabled` gates the fetch so collapsed folders don't
 * list until first opened. */
type DirEntriesState =
  | { status: "loading" }
  | { status: "ready"; entries: DirEntry[] }
  | { status: "error"; message: string };

function useDirEntries(path: string, enabled = true): DirEntriesState | null {
  const [state, setState] = useState<DirEntriesState | null>(null);
  // Which path the current `state` describes. State (not a ref) so the returned
  // value re-renders when it changes; deliberately NOT an effect dependency
  // (see the effect note below).
  const [statePath, setStatePath] = useState<string | null>(null);
  // Paths we already have a SETTLED (ready/error) result for, so a
  // collapse → re-expand of the SAME folder is free (PRD §9.7) without
  // re-listing. A ref so reading it never re-triggers the effect.
  const settledRef = useRef<Set<string>>(new Set());

  // STUCK-LOADING FIX: depend ONLY on [path, enabled]. The previous version also
  // listed `state` and `loadedPath`; because the effect itself calls
  // setState({loading}) + setLoadedPath(path), those deps changed the instant
  // the effect ran, which RE-RAN the effect — its cleanup set `cancelled = true`
  // on the in-flight listDir, and the cache guard then returned early without
  // re-fetching. The listDir result was silently dropped and the tree stuck on
  // "loading…" forever (the backend logged "list_dir OK N entries" but the UI
  // never updated — exactly the symptom in the diag log).
  useEffect(() => {
    if (!enabled) return;
    // Skip only when we already have a settled result AND `state` already
    // describes this path; otherwise (path changed, or never loaded) fetch.
    if (settledRef.current.has(path) && statePath === path) return;
    let cancelled = false;
    setStatePath(path);
    setState({ status: "loading" });
    tlog("files", `list_dir -> ${path}`);
    listDir(path)
      .then((entries) => {
        tlog("files", `list_dir OK ${path}: ${entries.length} entries`);
        if (cancelled) return;
        settledRef.current.add(path);
        setState({ status: "ready", entries });
      })
      .catch((e) => {
        tlog("files", `list_dir ERROR ${path}: ${String(e)}`);
        if (cancelled) return;
        settledRef.current.add(path);
        setState({ status: "error", message: String(e) });
      });
    return () => {
      cancelled = true;
    };
    // statePath is read for the cache guard but intentionally omitted from the
    // deps — it is SET inside this effect, so depending on it reintroduces the
    // self-cancelling loop described above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, enabled]);

  // While a brand-new path is loading but `state` still holds the prior path's
  // entries, hide the stale data.
  return statePath === path ? state : null;
}

/** Render the body of a directory: loading/error/empty hints, then its dirs and
 * files. Shared by the root container and every expanded folder. */
function DirChildren({
  state,
  depth,
  activePath,
  onOpenFile,
}: {
  state: DirEntriesState | null;
  depth: number;
  activePath: string | null;
  onOpenFile: (absPath: string) => void;
}) {
  if (state === null || state.status === "loading") {
    return <Hint depth={depth}>loading…</Hint>;
  }
  if (state.status === "error") {
    return (
      <Hint depth={depth} color="var(--th-dot-error)" title={state.message}>
        error
      </Hint>
    );
  }
  if (state.entries.length === 0) {
    return <Hint depth={depth}>empty</Hint>;
  }
  return (
    <>
      {state.entries.map((entry) =>
        entry.isDir ? (
          <TreeDir
            key={entry.path}
            path={entry.path}
            name={entry.name}
            depth={depth}
            activePath={activePath}
            onOpenFile={onOpenFile}
          />
        ) : (
          <TreeFile
            key={entry.path}
            entry={entry}
            depth={depth}
            active={entry.path === activePath}
            onOpen={onOpenFile}
          />
        ),
      )}
    </>
  );
}

function TreeDir({
  path,
  name,
  depth,
  activePath,
  onOpenFile,
}: {
  path: string;
  name: string;
  depth: number;
  activePath: string | null;
  onOpenFile: (absPath: string) => void;
}) {
  const [open, setOpen] = useState(false);
  // Only list children once the folder has been opened (lazy + cached). Once
  // loaded the result is kept across collapse/re-expand — collapsing is pure UI
  // state, never a rescan (PRD §9.7).
  const children = useDirEntries(path, open);

  return (
    <div>
      <Row
        onClick={() => setOpen((v) => !v)}
        title={path}
        style={{ paddingLeft: `${depth * 12 + 8}px` }}
      >
        <span className="flex items-center gap-1.5">
          <Chevron open={open} />
          <FolderIcon open={open} />
          <span
            className="truncate"
            style={{ color: "var(--th-fg)", fontWeight: 500 }}
          >
            {name}
          </span>
        </span>
      </Row>
      {open && (
        <DirChildren
          state={children}
          depth={depth + 1}
          activePath={activePath}
          onOpenFile={onOpenFile}
        />
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
      style={{ paddingLeft: `${depth * 12 + 8}px` }}
    >
      <span className="flex items-center gap-1.5">
        {/* Spacer matching the folder chevron's width so a file's type icon
            lines up directly under its sibling folders' icons (shared column). */}
        <span className="w-3 shrink-0" aria-hidden />
        <FileIcon name={entry.name} />
        <span
          className="truncate"
          style={{ color: active ? "var(--th-fg)" : "var(--th-fg-muted)" }}
        >
          {entry.name}
        </span>
      </span>
    </Row>
  );
}

// --- Small themed primitives ----------------------------------------------

/** Expand/collapse chevron for a folder row. */
function Chevron({ open }: { open: boolean }) {
  return (
    <span
      className="w-3 shrink-0 text-center text-[10px] leading-none"
      style={{ color: "var(--th-fg-muted)" }}
      aria-hidden
    >
      {open ? "▾" : "▸"}
    </span>
  );
}

// --- Row icons (inline SVG; no icon dependency) ----------------------------
//
// Small (13px) themed glyphs so folders vs files are unmistakable at a glance,
// plus a light extension hint that tints the file glyph by category (code /
// web / data / docs / image …). All stroke/fill via `currentColor` so the
// surrounding `color` drives them — kept on `var(--th-*)` tokens. Duplicated in
// FilePanel.tsx (its compact tree) on purpose: these two tree components are
// independent, self-contained surfaces and the file-ownership boundary keeps a
// shared icon module out of scope.

/** Folder glyph — a subtly different shape when open vs closed so an expanded
 *  folder reads as "open". Tinted with the accent so folders pop over files. */
function FolderIcon({ open }: { open: boolean }) {
  return (
    <span
      className="flex w-3.5 shrink-0 items-center justify-center"
      style={{ color: "var(--th-accent)" }}
      aria-hidden
    >
      <svg width="13" height="13" viewBox="0 0 16 16" fill="none">
        {open ? (
          // Open folder: back flap + an angled front face.
          <>
            <path
              d="M1.5 4.2c0-.5.4-.9.9-.9h3.1l1.2 1.2h6.9c.5 0 .9.4.9.9v1.1H2.6c-.5 0-1 .35-1.1.85V4.2z"
              fill="currentColor"
              opacity="0.45"
            />
            <path
              d="M2.5 7.4h12l-1.3 4.4c-.1.4-.5.7-.9.7H2.4c-.5 0-.9-.45-.85-.95l.6-3.45c.05-.4.4-.7.35-.7z"
              fill="currentColor"
            />
          </>
        ) : (
          // Closed folder: a tab + body.
          <path
            d="M1.5 4.2c0-.5.4-.9.9-.9h3.1l1.2 1.2h6.9c.5 0 .9.4.9.9v6c0 .5-.4.9-.9.9H2.4c-.5 0-.9-.4-.9-.9V4.2z"
            fill="currentColor"
          />
        )}
      </svg>
    </span>
  );
}

/** File glyph: a page outline (with a folded corner) tinted by the file's
 *  category. The tint is the only "type hint" — the shape stays consistent so
 *  rows read as a tidy column. */
function FileIcon({ name }: { name: string }) {
  const color = fileIconColor(name);
  return (
    <span
      className="flex w-3.5 shrink-0 items-center justify-center"
      style={{ color }}
      aria-hidden
    >
      <svg width="13" height="13" viewBox="0 0 16 16" fill="none">
        <path
          d="M4 1.5h5.2L13 5.3V14a.5.5 0 0 1-.5.5h-8A.5.5 0 0 1 4 14V2a.5.5 0 0 1 .5-.5z"
          fill="currentColor"
          opacity="0.18"
        />
        <path
          d="M4 1.5h5.2L13 5.3V14a.5.5 0 0 1-.5.5h-8A.5.5 0 0 1 4 14V2a.5.5 0 0 1 .5-.5z"
          stroke="currentColor"
          strokeWidth="1"
          strokeLinejoin="round"
        />
        <path
          d="M9 1.7V5.3h3.4"
          stroke="currentColor"
          strokeWidth="1"
          strokeLinejoin="round"
          fill="none"
        />
      </svg>
    </span>
  );
}

/** Map a filename to a category tint for its [`FileIcon`]. Themed token first
 *  (key/doc files lean on `--th-accent`); a few common code/web/data/image
 *  families get a distinct but muted hue so the tree has light type cues without
 *  becoming a rainbow. Unknown types fall back to the muted foreground. */
function fileIconColor(name: string): string {
  const lower = name.toLowerCase();
  // High-signal project files share the accent (they're the ones you reach for).
  if (
    lower === "package.json" ||
    lower === "cargo.toml" ||
    lower === "tsconfig.json" ||
    lower === "tauri.conf.json" ||
    lower === "dockerfile" ||
    lower === "makefile" ||
    lower.startsWith("readme") ||
    lower.startsWith("license") ||
    lower.startsWith("licence") ||
    lower.startsWith("changelog")
  ) {
    return "var(--th-accent)";
  }
  const dot = lower.lastIndexOf(".");
  const ext = dot >= 0 ? lower.slice(dot + 1) : "";
  return EXT_TINT[ext] ?? "var(--th-fg-muted)";
}

/** Extension → tint. Muted, category-grouped hues; intentionally small. */
const EXT_TINT: Record<string, string> = {
  // TypeScript / JS family.
  ts: "#4f9cf0",
  tsx: "#4f9cf0",
  js: "#e2b93d",
  jsx: "#e2b93d",
  mjs: "#e2b93d",
  cjs: "#e2b93d",
  // Rust / systems.
  rs: "#d08770",
  go: "#4dc4d6",
  // Web markup / styles.
  html: "#e06c4f",
  css: "#56a3e0",
  scss: "#cf649a",
  // Data / config.
  json: "#d6a84d",
  toml: "#9aa0a6",
  yaml: "#9aa0a6",
  yml: "#9aa0a6",
  // Docs.
  md: "#7fb37f",
  mdx: "#7fb37f",
  markdown: "#7fb37f",
  txt: "#9aa0a6",
  // Python.
  py: "#5a9fd4",
  // Images (still shown in the tree even if the reader rejects them).
  png: "#b48ead",
  jpg: "#b48ead",
  jpeg: "#b48ead",
  gif: "#b48ead",
  svg: "#b48ead",
  webp: "#b48ead",
};

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
      className="block w-full py-0.5 pr-2 text-left text-[13px] leading-5"
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

/** Join an absolute root with a `/`-separated relative path. Preserves the
 * root's own separator style so abs paths match what list_dir returns. */
function joinPath(root: string, rel: string): string {
  if (!rel) return root;
  const sep = root.includes("\\") && !root.includes("/") ? "\\" : "/";
  const base = root.replace(/[\\/]+$/, "");
  return `${base}${sep}${sep === "\\" ? rel.replace(/\//g, "\\") : rel}`;
}
