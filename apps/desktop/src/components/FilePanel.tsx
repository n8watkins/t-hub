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
import { FileTypeIcon, FolderTypeIcon } from "../lib/fileIcons";
import {
  indexProject,
  listDir,
  readTextFile,
  searchFiles,
  writeTextFile,
} from "../ipc/files";
import { type GitInfo, gitCommit, gitInfo } from "../ipc/git";
import type { DirEntry, FileContents, FileHit } from "../ipc/types";
import { Markdown } from "./Markdown";
import { tlog } from "../lib/diag";
import { useSettings } from "../store/settings";

const MARKDOWN_EXTS = new Set(["md", "markdown", "mdx", "mdown", "markdn"]);

// --- "Show ignored" persistence (localStorage, not the settings store) ------
// A local browsing preference, deliberately kept out of store/settings.ts. Reads
// are defensive (SSR/private-mode/quota): any failure falls back to OFF.
const SHOW_IGNORED_KEY = "t-hub.files.showIgnored";

function readShowIgnored(): boolean {
  try {
    return localStorage.getItem(SHOW_IGNORED_KEY) === "1";
  } catch {
    return false;
  }
}

function writeShowIgnored(v: boolean): void {
  try {
    localStorage.setItem(SHOW_IGNORED_KEY, v ? "1" : "0");
  } catch {
    // ignore (private mode / quota): the in-memory state still drives the UI.
  }
}

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
  /**
   * Compact (narrow) layout: instead of the wide side-by-side tree+reader rail,
   * STACK them — show the tree full-width, and when a file is opened show the
   * reader full-width with a "back to files" control. Used in the per-tile split
   * (a half-tile is too narrow for side-by-side). Defaults to false (the roomy
   * side-by-side layout, e.g. the expanded panel).
   */
  compact?: boolean;
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
  compact = false,
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

  // --- Reload + collapsible search state ---------------------------------
  // `reloadKey` is bumped by the header's Reload control; it's threaded into the
  // tree's React `key` so the WHOLE tree remounts and re-lists every open dir
  // from scratch (answers the user's "should I reload it?" — yes, here's the
  // button). `searchOpen` keeps the fuzzy-search box SECONDARY: collapsed by
  // default so browsing is the primary, bulletproof path and the search box can
  // never sit between the user and the tree. Search is index-only/lazy, so the
  // tree never depends on it either way. (`reload`/`closeSearch` are defined
  // below, after `refreshGit`, to avoid a TDZ on it.)
  const [reloadKey, setReloadKey] = useState(0);
  const [searchOpen, setSearchOpen] = useState(false);

  // --- "Show ignored" toggle (persisted locally) -------------------------
  // OFF (default): the backend's directory-only gitignore rule — ignored DIRS
  // (node_modules, dist, …) hidden, ignored FILES (.env, …) always shown. ON:
  // list everything, ignored dirs included (only `.git` stays hidden). Persisted
  // to localStorage (NOT the settings store, by design — this is a local browsing
  // preference). Threaded into `listDir` AND into the tree's remount key so a
  // flip re-lists every open folder with the new flag.
  const [showIgnored, setShowIgnored] = useState<boolean>(readShowIgnored);
  const toggleShowIgnored = useCallback(() => {
    setShowIgnored((v) => {
      const next = !v;
      writeShowIgnored(next);
      return next;
    });
  }, []);

  // --- Git awareness (feat/git-panel): branch/worktree + commit. ----------
  // `git` is the latest GitInfo for the panel's root; `null` until first load /
  // when there's no root. `refreshGit` re-queries (root change + post-commit so
  // the dirty count resets). Reader-only mode skips git entirely (no header).
  const [git, setGit] = useState<GitInfo | null>(null);
  const refreshGit = useCallback(() => {
    if (!root) {
      setGit(null);
      return;
    }
    const target = root;
    gitInfo(target)
      .then((info) => {
        // Ignore a stale response if the root changed while in flight.
        if (target === root) setGit(info);
      })
      .catch(() => {
        if (target === root) setGit(null);
      });
  }, [root]);
  useEffect(() => {
    if (readerOnly) return;
    refreshGit();
  }, [refreshGit, readerOnly]);

  // Reload the tree: remount it (re-list every open dir), drop the stale search
  // index, and refresh git. Defined here so the `refreshGit` dep is initialized.
  const reload = useCallback(() => {
    tlog("files", `reload requested root=${root ?? "(none)"}`);
    setIndexState({ status: "idle" });
    setReloadKey((k) => k + 1);
    refreshGit();
  }, [root, refreshGit]);
  // Closing the search box clears the query so we fall back to the tree cleanly.
  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setQuery("");
  }, []);

  // --- Reset navigation when the root changes. ---------------------------
  // NOTE: we deliberately do NOT index the whole project on mount anymore. The
  // file TREE is lazy (shallow `listDir` per folder, ~0.1s each), so browsing is
  // instant; the fuzzy-search index (a full project walk) is built ON DEMAND the
  // first time the user actually types a query (see the search effect). Indexing
  // up front was pure latency for the common case of "just open a file".
  useEffect(() => {
    if (readerOnly) return;
    // DIAG: surface the root the panel actually received — an empty/undefined
    // root means the tile passed no cwd (the "No project selected" empty state),
    // which reads as "files not loading".
    tlog("files", `FilePanel root=${root ?? "(none)"} compact=${compact}`);
    setIndexState({ status: "idle" });
    setHits([]);
    setQuery("");
    setSearchOpen(false);
    setReader({ status: "empty" });
    setActivePath(null);
  }, [root, readerOnly, compact]);

  // --- Open a file into the reader. --------------------------------------
  const openFile = useCallback((path: string) => {
    setActivePath(path);
    setReader({ status: "loading", path });
    tlog("files", `read -> ${path}`);
    readTextFile(path)
      .then((contents) => {
        tlog("files", `read OK ${path}: ${contents.size}B`);
        const isMd = MARKDOWN_EXTS.has(contents.ext);
        setReader({
          status: "ready",
          contents,
          mode: isMd ? "rendered" : "source",
        });
      })
      .catch((e) => {
        // DIAG: capture the exact path + error for "cannot find file" reports so
        // we can see whether it's a path-translation / distro / stale-entry issue.
        tlog("files", `read ERROR ${path}: ${String(e)}`);
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
    if (!openedInitial.current && initialFile) {
      openedInitial.current = true;
      openFile(initialFile);
    }
  }, [initialFile, openFile]);

  // --- Debounced fuzzy search (LAZY index). ------------------------------
  // No query -> show the tree, do nothing (and never index). On the first query
  // we build the index on demand (cheap, ~0.1s), then search; subsequent queries
  // reuse it. So browsing never pays the index cost — only an actual search does.
  useEffect(() => {
    const q = query.trim();
    if (!root || !q) {
      setHits([]);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const handle = setTimeout(async () => {
      try {
        if (indexState.status !== "ready") {
          setIndexState({ status: "indexing" });
          const summary = await indexProject(root);
          if (cancelled) return;
          setIndexState({
            status: "ready",
            count: summary.count,
            root: summary.root,
          });
        }
        const res = await searchFiles(root, q, searchLimit);
        if (!cancelled) setHits(res);
      } catch (e) {
        if (!cancelled) {
          setHits([]);
          setIndexState({ status: "error", message: String(e) });
        }
      } finally {
        if (!cancelled) setSearching(false);
      }
    }, 120);
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

  // Compact (narrow split): STACK tree/reader instead of side-by-side. Show the
  // reader full-width when a file is open (with a back-to-files control), else the
  // tree full-width with a SECONDARY, collapsible search. Fits a half-tile where
  // the 288px rail can't.
  //
  // Height chain (the prime "blank tree" suspect — a broken chain renders the
  // tree at 0px): PanelShell is `h-full min-h-0 flex-col`; this branch's content
  // wrapper is `min-h-0 flex-1 flex-col`; the header/search rows are `shrink-0`
  // (they must NOT eat the tree's space or be squeezed to nothing); the tree's
  // own viewport is `min-h-0 flex-1 overflow-y-auto`. Every flex parent has
  // `min-h-0` so a tall tree scrolls instead of collapsing the column.
  if (compact) {
    const fileOpen = reader.status !== "empty";
    return (
      <PanelShell className={className}>
        <Header
          root={root}
          indexState={indexState}
          git={git}
          onCommitted={refreshGit}
          onReload={reload}
          showIgnored={showIgnored}
          onToggleShowIgnored={toggleShowIgnored}
        />
        {fileOpen ? (
          <div className="flex min-h-0 flex-1 flex-col">
            <button
              type="button"
              onClick={() => {
                setReader({ status: "empty" });
                setActivePath(null);
              }}
              className="flex shrink-0 items-center gap-1 border-b px-3 py-1.5 text-left text-xs hover:bg-neutral-800/40"
              style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
            >
              ← Files
            </button>
            <div className="min-h-0 flex-1">
              <Reader
                reader={reader}
                onSetMode={(mode) =>
                  setReader((r) => (r.status === "ready" ? { ...r, mode } : r))
                }
                onSaved={onSaved}
              />
            </div>
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col">
            <SearchBox query={query} onQuery={setQuery} searching={searching} />
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
                  key={`${reloadKey}:${showIgnored ? 1 : 0}`}
                  root={root}
                  activePath={activePath}
                  showIgnored={showIgnored}
                  onOpenFile={openFile}
                />
              )}
            </div>
          </div>
        )}
      </PanelShell>
    );
  }

  return (
    <PanelShell className={className}>
      {/* Header: root + index status + reload + git branch/worktree + commit. */}
      <Header
        root={root}
        indexState={indexState}
        git={git}
        onCommitted={refreshGit}
        onReload={reload}
        showIgnored={showIgnored}
        onToggleShowIgnored={toggleShowIgnored}
      />

      <div className="flex min-h-0 flex-1">
        {/* Left rail: search box + the tree (or ranked results while searching). */}
        <div
          className="flex w-72 shrink-0 flex-col border-r"
          style={{ borderColor: "var(--th-border)" }}
        >
          <SearchBox query={query} onQuery={setQuery} searching={searching} />
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
                key={`${reloadKey}:${showIgnored ? 1 : 0}`}
                root={root}
                activePath={activePath}
                showIgnored={showIgnored}
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
  git,
  onCommitted,
  onReload,
  showIgnored,
  onToggleShowIgnored,
}: {
  root: string;
  indexState:
    | { status: "idle" }
    | { status: "indexing" }
    | { status: "ready"; count: number; root: string }
    | { status: "error"; message: string };
  /** Latest git facts for `root`, or null (no root / not a repo / loading). */
  git: GitInfo | null;
  /** Called after a successful commit so the host can refresh git info. */
  onCommitted: () => void;
  /** Re-list the tree (remount it) + refresh git. Wired to the Reload control. */
  onReload: () => void;
  /** Whether the tree currently shows gitignored dirs too (the toggle's state). */
  showIgnored: boolean;
  /** Flip the "Show ignored" preference (re-lists the tree with the new flag). */
  onToggleShowIgnored: () => void;
}) {
  const label = basename(root) || root;
  // Dotfile visibility lives in the settings store (persisted; default hidden).
  // The toggle flips it for the whole app — the tree re-filters live.
  const hideDotfiles = useSettings((s) => s.hideDotfiles);
  const setHideDotfiles = useSettings((s) => s.setHideDotfiles);
  return (
    <div className="border-b" style={{ borderColor: "var(--th-border)" }}>
      <div className="flex items-center justify-between gap-3 px-3 py-2">
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
            {shortPath(root)}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2 text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
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
          {/* Show hidden: dotfiles (.git, .cargo, .claude, …) are hidden by
              default to keep the tree quiet; flip to reveal them. Persisted in the
              settings store; the tree re-filters live (no reload needed). */}
          <button
            type="button"
            onClick={() => setHideDotfiles(!hideDotfiles)}
            aria-pressed={!hideDotfiles}
            className="shrink-0 rounded border px-1.5 py-0.5 leading-none transition-colors hover:bg-neutral-700/30"
            style={{
              borderColor: !hideDotfiles ? "var(--th-accent)" : "var(--th-border)",
              color: !hideDotfiles ? "var(--th-fg)" : "var(--th-fg-muted)",
              background: !hideDotfiles ? "var(--th-tile-bg)" : "transparent",
            }}
            title={
              hideDotfiles
                ? "Hiding dotfiles (.git, .cargo, .claude, …). Click to show them."
                : "Showing dotfiles (.git, .cargo, .claude, …). Click to hide them."
            }
          >
            {hideDotfiles ? "Show hidden" : "Hide hidden"}
          </button>
          {/* Show ignored: OFF = directory-only gitignore (ignored dirs like
              node_modules hidden, .env & other ignored FILES shown); ON = show
              everything including ignored dirs. Persisted to localStorage. */}
          <button
            type="button"
            onClick={onToggleShowIgnored}
            aria-pressed={showIgnored}
            className="shrink-0 rounded border px-1.5 py-0.5 leading-none transition-colors hover:bg-neutral-700/30"
            style={{
              borderColor: showIgnored ? "var(--th-accent)" : "var(--th-border)",
              color: showIgnored ? "var(--th-fg)" : "var(--th-fg-muted)",
              background: showIgnored ? "var(--th-tile-bg)" : "transparent",
            }}
            title={
              showIgnored
                ? "Showing ignored files & dirs (node_modules, build output, …). Click to hide ignored dirs."
                : "Hiding ignored dirs (node_modules, …). Ignored files like .env still show. Click to show everything."
            }
          >
            {showIgnored ? "Hide ignored" : "Show ignored"}
          </button>
          {/* Reload: re-list the whole tree (re-fetch list_dir for open dirs)
              and refresh git. The answer to "should I reload it?" — one click. */}
          <button
            type="button"
            onClick={onReload}
            className="shrink-0 rounded border px-1.5 py-0.5 leading-none transition-colors hover:bg-neutral-700/30"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
            title="Reload the file tree"
            aria-label="Reload files"
          >
            ↻ Reload
          </button>
        </div>
      </div>
      {/* feat/git-panel: branch/worktree row + inline commit form. Renders only
          when `root` is inside a git repo; otherwise stays hidden. */}
      <GitBar root={root} git={git} onCommitted={onCommitted} />
    </div>
  );
}

// --- Git bar (feat/git-panel) ----------------------------------------------

/**
 * The branch + worktree display plus a "Commit…" affordance, shown under the
 * FilePanel header. Hidden entirely when `root` isn't a git repo (git is null or
 * `isRepo: false`). The commit form stages all changes (`git add -A`) and commits
 * a message, then calls `onCommitted` so the host re-queries git (dirty resets).
 */
function GitBar({
  root,
  git,
  onCommitted,
}: {
  root: string;
  git: GitInfo | null;
  onCommitted: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [committing, setCommitting] = useState(false);
  const [result, setResult] = useState<
    { kind: "ok"; hash: string } | { kind: "error"; message: string } | null
  >(null);

  // Not a repo (or still loading / no root): render nothing.
  if (!git || !git.isRepo) return null;

  const commit = () => {
    const msg = message.trim();
    if (!msg || committing) return;
    setCommitting(true);
    setResult(null);
    gitCommit(root, msg)
      .then((hash) => {
        setCommitting(false);
        setResult({ kind: "ok", hash });
        setMessage("");
        setOpen(false);
        onCommitted(); // refresh git info (dirty count resets)
      })
      .catch((e) => {
        setCommitting(false);
        setResult({ kind: "error", message: String(e) });
      });
  };

  return (
    <div className="px-3 pb-2">
      <div className="flex items-center justify-between gap-3">
        <div
          className="flex min-w-0 items-center gap-2 text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
        >
          {/* Branch name. */}
          <span
            className="flex min-w-0 items-center gap-1"
            title={
              git.worktreeRoot
                ? `worktree: ${git.worktreeRoot}`
                : "current branch"
            }
          >
            <span style={{ color: "var(--th-fg-muted)" }}>⎇</span>
            <span className="truncate" style={{ color: "var(--th-fg)" }}>
              {git.branch ?? "detached"}
            </span>
          </span>
          {/* Linked-worktree badge. */}
          {git.isLinkedWorktree && (
            <span
              className="rounded px-1 py-0.5 text-[10px]"
              style={{
                border: "1px solid var(--th-border)",
                color: "var(--th-fg-muted)",
              }}
              title={
                git.worktreeRoot
                  ? `linked worktree at ${git.worktreeRoot}`
                  : "linked worktree"
              }
            >
              worktree
            </span>
          )}
          {/* Dirty file count. */}
          <span
            title={
              git.dirtyCount === 0
                ? "working tree clean"
                : `${git.dirtyCount} changed file(s)`
            }
            style={{
              color:
                git.dirtyCount > 0 ? "var(--th-dot-starting)" : "var(--th-fg-muted)",
            }}
          >
            {git.dirtyCount === 0
              ? "clean"
              : `${git.dirtyCount} change${git.dirtyCount === 1 ? "" : "s"}`}
          </span>
        </div>
        {!open && (
          <button
            type="button"
            onClick={() => {
              setOpen(true);
              setResult(null);
            }}
            className="shrink-0 rounded border px-2 py-0.5 text-[11px] transition-colors hover:bg-neutral-700/30"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
          >
            Commit…
          </button>
        )}
      </div>

      {/* Inline commit form: a message field + Commit / Cancel. */}
      {open && (
        <div className="mt-2 flex items-center gap-2">
          <input
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            placeholder="Commit message…"
            spellCheck={false}
            autoFocus
            className="min-w-0 flex-1 px-2 py-1 text-[12px] focus:outline-none"
            style={{
              borderRadius: "var(--th-radius)",
              border: "1px solid var(--th-border)",
              background: "var(--th-tile-bg)",
              color: "var(--th-fg)",
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commit();
              } else if (e.key === "Escape") {
                e.preventDefault();
                setOpen(false);
              }
            }}
          />
          <button
            type="button"
            onClick={commit}
            disabled={committing || !message.trim()}
            className="shrink-0 rounded border px-2 py-1 text-[11px] transition-colors hover:bg-neutral-700/30 disabled:cursor-not-allowed disabled:opacity-40"
            style={{ borderColor: "var(--th-accent)", color: "var(--th-fg)" }}
          >
            {committing ? "Committing…" : "Commit"}
          </button>
          <button
            type="button"
            onClick={() => setOpen(false)}
            disabled={committing}
            className="shrink-0 rounded border px-2 py-1 text-[11px] transition-colors hover:bg-neutral-700/30 disabled:cursor-not-allowed disabled:opacity-40"
            style={{ borderColor: "var(--th-border)", color: "var(--th-fg)" }}
          >
            Cancel
          </button>
        </div>
      )}

      {/* Inline success/failure feedback. */}
      {result?.kind === "ok" && (
        <div className="mt-1 text-[11px]" style={{ color: "var(--th-dot-running)" }}>
          Committed {result.hash}
        </div>
      )}
      {result?.kind === "error" && (
        <div
          className="mt-1 break-words text-[11px]"
          style={{ color: "var(--th-dot-error)" }}
          title={result.message}
        >
          {result.message}
        </div>
      )}
    </div>
  );
}

// --- Search bar (secondary, collapsible) -----------------------------------

/**
 * The fuzzy-search affordance, deliberately SECONDARY to browsing. Collapsed it
 * is a slim "Search files…" button; opening it reveals the input (auto-focused)
 * with a × to close. Browsing the tree is the primary path and never depends on
 * this — search is a lazy, index-only overlay — so a confused user can always
 * just close it and keep clicking folders. The closed row is `shrink-0` so it
 * never steals height from the tree below it.
 */
function SearchBar({
  open,
  query,
  onQuery,
  onOpen,
  onClose,
  compact,
}: {
  open: boolean;
  query: string;
  onQuery: (q: string) => void;
  onOpen: () => void;
  onClose: () => void;
  /** Tighter copy/padding for the narrow stacked layout. */
  compact?: boolean;
}) {
  if (!open) {
    return (
      <div className="shrink-0 border-b p-2" style={{ borderColor: "var(--th-border)" }}>
        <button
          type="button"
          onClick={onOpen}
          className="flex w-full items-center gap-2 rounded px-2.5 py-1.5 text-left text-sm transition-colors hover:bg-neutral-700/20"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "transparent",
            color: "var(--th-fg-muted)",
          }}
          title="Fuzzy-search files (browsing the tree works without this)"
        >
          <span aria-hidden>⌕</span>
          <span>Search files…</span>
        </button>
      </div>
    );
  }
  return (
    <div className="shrink-0 border-b p-2" style={{ borderColor: "var(--th-border)" }}>
      <div className="flex items-center gap-1.5">
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder={compact ? "Search files…" : "Fuzzy search files…"}
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          autoFocus
          className="min-w-0 flex-1 px-2.5 py-1.5 text-sm focus:outline-none"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
          onKeyDown={(e) => {
            // Escape closes search and drops back to the tree.
            if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
        />
        <button
          type="button"
          onClick={onClose}
          className="shrink-0 rounded px-1.5 py-1 leading-none transition-colors hover:bg-neutral-700/30"
          style={{ color: "var(--th-fg-muted)" }}
          title="Close search (back to browsing)"
          aria-label="Close search"
        >
          ×
        </button>
      </div>
    </div>
  );
}

// --- Search results --------------------------------------------------------

/** Always-visible search input above the tree. Typing filters via fuzzy search
 *  (the index builds lazily on the first query); clearing it falls back to the
 *  tree. A small "…" shows while a search is in flight. */
function SearchBox({
  query,
  onQuery,
  searching,
}: {
  query: string;
  onQuery: (q: string) => void;
  searching: boolean;
}) {
  return (
    <div className="border-b p-2" style={{ borderColor: "var(--th-border)" }}>
      <div className="relative">
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder="Search files…"
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          className="w-full px-2.5 py-1.5 pr-7 text-sm focus:outline-none"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
        />
        {query && (
          <button
            type="button"
            onClick={() => onQuery("")}
            className="absolute right-1.5 top-1/2 -translate-y-1/2 rounded px-1 leading-none hover:bg-neutral-700/40"
            style={{ color: "var(--th-fg-muted)" }}
            title="Clear search"
            aria-label="Clear search"
          >
            {searching ? "…" : "×"}
          </button>
        )}
      </div>
    </div>
  );
}

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
  // Respect "hide dotfiles" in search too (same as the tree): drop hits with any
  // dot-segment in their path so the toggle quiets dotfile noise everywhere.
  const hideDotfiles = useSettings((s) => s.hideDotfiles);
  const shown = hideDotfiles
    ? hits.filter((h) => !h.relPath.split("/").some((s) => s.startsWith(".")))
    : hits;
  if (!searching && shown.length === 0) {
    return (
      <div className="px-3 py-2 text-xs" style={{ color: "var(--th-fg-muted)" }}>
        No matching files.
      </div>
    );
  }
  return (
    <ul className="py-1">
      {shown.map((hit) => {
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
  showIgnored,
  onOpenFile,
}: {
  root: string;
  activePath: string | null;
  /** Forwarded to every `listDir`: when true, ignored dirs (node_modules, …)
   *  are listed too. The whole tree remounts when this flips (its `key` includes
   *  it), so each TreeDir re-lists with the new flag — no per-effect dep needed. */
  showIgnored: boolean;
  onOpenFile: (absPath: string) => void;
}) {
  // DIAG: the "blank tree" reports hinge on whether this container actually has
  // pixels — a collapsed flex/min-h-0 chain renders the tree at 0 height and it
  // reads as "files not loading". Measure our own rendered box (and the parent's,
  // which is the scroll viewport) once mounted + on root change so the
  // orchestrator can confirm from the diag file that the tree got real height.
  const wrapRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    // Defer one frame so layout has settled before we read offsetHeight.
    const id = requestAnimationFrame(() => {
      const parent = el.parentElement;
      tlog(
        "files",
        `FileTree mounted root=${root} treeH=${el.offsetHeight} viewportH=${parent?.offsetHeight ?? "?"}`,
      );
    });
    return () => cancelAnimationFrame(id);
  }, [root]);

  return (
    <div ref={wrapRef} className="py-1">
      <TreeDir
        // Key on the root so changing the project (new terminal/worktree) fully
        // remounts the tree — otherwise the same TreeDir instance keeps the old
        // root's cached `entries` and never re-lists (stale-tree bug).
        key={root}
        path={root}
        name={basename(root) || root}
        depth={0}
        defaultOpen
        // The root participates in single-child collapse-through so a project
        // whose root holds exactly one folder shows its files immediately.
        autoExpandSingle
        activePath={activePath}
        showIgnored={showIgnored}
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
  autoExpandSingle,
  activePath,
  showIgnored,
  onOpenFile,
}: {
  path: string;
  name: string;
  depth: number;
  defaultOpen?: boolean;
  /**
   * When true and this dir's ONLY child is itself a single directory, auto-open
   * that child so a "root with one folder" (e.g. site-forge -> source-engine)
   * doesn't read as empty/confusing — we collapse-through single-child chains so
   * the user lands on real files immediately. Propagated to each auto-opened
   * child so the whole chain unfolds. The user can still collapse any level.
   */
  autoExpandSingle?: boolean;
  activePath: string | null;
  /** Forwarded to `listDir` (and to child dirs): show ignored dirs too. The tree
   *  remounts when this flips, so it's constant per mount — NOT an effect dep. */
  showIgnored: boolean;
  onOpenFile: (absPath: string) => void;
}) {
  const [open, setOpen] = useState(!!defaultOpen);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Hide dotfiles (".*") when the setting is on (persisted; default on). Applied
  // to the rendered entries below, so it takes effect at EVERY level of the tree
  // and re-filters live when toggled (the cached `entries` are untouched).
  const hideDotfiles = useSettings((s) => s.hideDotfiles);

  // Lazily load children the first time the dir is opened (PRD §9.7: folder
  // expansion is UI state + a shallow list, not a full rescan).
  //
  // DEPS ARE [open, path] ONLY — NOT entries/loading (and NOT showIgnored).
  // Including `loading` made `setLoading(true)` below re-run this effect, whose
  // cleanup set cancelled=true on the in-flight listDir; the guard then blocked a
  // refetch, so `loading` got stuck true forever and the folder showed "loading…"
  // with the entries never applied (the bug that left source-engine perpetually
  // loading). `showIgnored` is intentionally omitted too: flipping it REMOUNTS the
  // whole tree (its key includes the flag), so each TreeDir is fresh and re-lists
  // with the new value on first open — adding it as a dep is both unnecessary and
  // would risk the same self-cancel. With [open, path] the effect only re-fires
  // when the folder is opened or its path changes.
  useEffect(() => {
    if (!open || entries !== null) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    listDir(path, showIgnored)
      .then((res) => {
        if (!cancelled) {
          tlog("files", `TreeDir ${path} -> ${res.length} entries (applied)`);
          setEntries(res);
        }
      })
      .catch((e) => {
        tlog("files", `TreeDir ${path} ERR ${String(e)}`);
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, path]);

  // The entries we actually render: drop dotfiles when the setting is on. Done
  // here (not in the listDir cache) so a toggle re-filters live and the filter
  // holds at every level of the tree.
  const visibleEntries = entries
    ? hideDotfiles
      ? entries.filter((e) => !e.name.startsWith("."))
      : entries
    : null;

  // Collapse-through single-child chains: when this dir is open, loaded, and its
  // sole entry is a directory, auto-open it. The child renders with the same
  // `autoExpandSingle` flag, so a deep single-folder chain (foo/bar/baz/...)
  // unfolds in one pass and the reader-side tree shows real files at once. Only
  // fires while the user hasn't manually touched this node. Based on the VISIBLE
  // entries so a folder whose lone non-dotfile child is a dir still collapses
  // through.
  const onlyChildDir =
    visibleEntries && visibleEntries.length === 1 && visibleEntries[0].isDir
      ? visibleEntries[0]
      : null;

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
        <FolderTypeIcon name={name} open={open} />
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
          {visibleEntries && visibleEntries.length === 0 && !loading && !error && (
            <div
              style={{ paddingLeft: `${(depth + 1) * 12 + 14}px`, color: "var(--th-fg-muted)" }}
              className="py-0.5 text-[11px]"
            >
              empty folder
            </div>
          )}
          {visibleEntries?.map((entry) =>
            entry.isDir ? (
              <TreeDir
                key={entry.path}
                path={entry.path}
                name={entry.name}
                depth={depth + 1}
                // Auto-open this child when it's the lone entry of a node that is
                // itself unfolding a single-child chain — so site-forge ->
                // source-engine -> ... lands on real files without clicking.
                defaultOpen={!!autoExpandSingle && entry.path === onlyChildDir?.path}
                autoExpandSingle={autoExpandSingle}
                activePath={activePath}
                showIgnored={showIgnored}
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
        // Same base indent as a sibling folder row (depth*12 + 8) so the file's
        // type icon lines up under the folder icons; the chevron-width spacer
        // below stands in for the (absent) expander.
        paddingLeft: `${depth * 12 + 8}px`,
        background: active ? "var(--th-tile-bg)" : "transparent",
        color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
      }}
      className="flex w-full items-center gap-1.5 py-1 pr-2 text-left text-sm"
      onMouseEnter={(e) => {
        if (!active) e.currentTarget.style.background = "var(--th-tile-bg)";
      }}
      onMouseLeave={(e) => {
        if (!active) e.currentTarget.style.background = "transparent";
      }}
      title={entry.path}
    >
      {/* Chevron-width spacer (matches the folder row's expander) so file icons
          sit in the same column as folder icons. */}
      <span className="w-3 shrink-0" aria-hidden />
      <FileTypeIcon name={entry.name} />
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

/** Shorten a path for display: collapse `/home/<user>` to `~` so the header
 *  shows `~/appturnity/site-forge` instead of `/home/natkins/appturnity/...`.
 *  Full path stays in the title tooltip. */
function shortPath(p: string): string {
  const m = p.match(/^\/home\/[^/]+(\/.*)?$/);
  if (m) return "~" + (m[1] ?? "");
  return p;
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
