import { ChevronRight, Folder, FolderOpen, Home, MoveUp } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { listDir } from "../ipc/files";
import { gitInfo, gitWorktreeList, type GitInfo, type WorktreeInfo } from "../ipc/git";
import type { DirEntry } from "../ipc/types";
import { normalizeWslPath, pickWslFolder } from "../ipc/wslFolderDialog";

interface WslFolderPickerProps {
  path: string;
  home?: string;
  recentPaths: Array<{ label: string; path: string }>;
  onPathChange: (path: string) => void;
  onFolderMetadataChange?: (selection: WslFolderSelection) => void;
  metadataRefreshToken?: number;
  listingRefreshToken?: number;
}

export interface WslFolderSelection {
  path: string;
  listingStatus: "loading" | "valid-empty" | "valid-populated" | "error" | "stale";
  listingPrior?: "empty" | "populated";
  listingError?: string;
  metadataStatus: "checking" | "ready" | "unavailable";
  metadataError?: string;
  git: GitInfo | null;
  worktreeCount: number | null;
  worktrees: WorktreeInfo[] | null;
}

type ListingState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "loaded-empty" }
  | { kind: "loaded-populated" }
  | { kind: "error"; message: string }
  | { kind: "stale"; prior: "empty" | "populated" };

export function WslFolderPicker({
  path,
  home,
  recentPaths,
  onPathChange,
  onFolderMetadataChange,
  metadataRefreshToken = 0,
  listingRefreshToken = 0,
}: WslFolderPickerProps) {
  const [manualPath, setManualPath] = useState(path);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [listing, setListing] = useState<ListingState>({ kind: "idle" });
  const [picking, setPicking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [localListingRefreshToken, setLocalListingRefreshToken] = useState(0);
  const requestGeneration = useRef(0);
  const metadataGeneration = useRef(0);
  const navigationGeneration = useRef(0);
  const selectionRef = useRef<WslFolderSelection | null>(null);

  useEffect(() => () => {
    navigationGeneration.current += 1;
    requestGeneration.current += 1;
    metadataGeneration.current += 1;
  }, []);

  useEffect(() => setManualPath(path), [path]);

  useEffect(() => {
    if (!path) return;
    const generation = ++requestGeneration.current;
    let cancelled = false;
    const priorKind = listing.kind === "loaded-empty"
      ? "empty"
      : listing.kind === "loaded-populated"
        ? "populated"
        : listing.kind === "stale"
          ? listing.prior
          : null;
    const nextListing = priorKind
      ? { kind: "stale" as const, prior: priorKind }
      : { kind: "loading" as const };
    const previousSelection = selectionRef.current?.path === path ? selectionRef.current : null;
    const initialSelection: WslFolderSelection = {
      path,
      listingStatus: nextListing.kind,
      listingPrior: nextListing.kind === "stale" ? nextListing.prior : undefined,
      metadataStatus: previousSelection?.metadataStatus ?? "checking",
      metadataError: previousSelection?.metadataError,
      git: previousSelection?.git ?? null,
      worktreeCount: previousSelection?.worktreeCount ?? null,
      worktrees: previousSelection?.worktrees ?? null,
    };
    selectionRef.current = initialSelection;
    setListing(() =>
      nextListing,
    );
    setError(null);
    onFolderMetadataChange?.(initialSelection);
    const updateSelection = (patch: Partial<WslFolderSelection>) => {
      if (selectionRef.current?.path !== path) return;
      const nextSelection = { ...selectionRef.current, ...patch };
      selectionRef.current = nextSelection;
      onFolderMetadataChange?.(nextSelection);
    };
    listDir(path)
      .then((nextEntries) => {
        if (cancelled || generation !== requestGeneration.current) return;
        const directories = nextEntries.filter((entry) => entry.isDir);
        setEntries(directories);
        setListing({ kind: directories.length === 0 ? "loaded-empty" : "loaded-populated" });
        updateSelection({
          listingStatus: directories.length === 0 ? "valid-empty" : "valid-populated",
          listingPrior: undefined,
          listingError: undefined,
        });
      })
      .catch((cause) => {
        if (cancelled || generation !== requestGeneration.current) return;
        const message = cause instanceof Error ? cause.message : String(cause);
        setListing({ kind: "error", message });
        setError(message);
        updateSelection({ listingStatus: "error", listingError: message });
      })
      .finally(() => {
        // The explicit listing state prevents an error from being rendered as
        // the successful empty-folder state.
    });
    return () => {
      cancelled = true;
    };
  }, [listingRefreshToken, localListingRefreshToken, onFolderMetadataChange, path]);

  useEffect(() => {
    if (!path || !onFolderMetadataChange) return;
    const generation = ++metadataGeneration.current;
    let cancelled = false;
    const updateSelection = (patch: Partial<WslFolderSelection>) => {
      if (selectionRef.current?.path !== path) return;
      const nextSelection = { ...selectionRef.current, ...patch };
      selectionRef.current = nextSelection;
      onFolderMetadataChange?.(nextSelection);
    };
    updateSelection({
      metadataStatus: "checking",
      metadataError: undefined,
      git: null,
      worktreeCount: null,
      worktrees: null,
    });
    void gitInfo(path)
      .then(async (git) => {
        if (!git.isRepo) return { git, worktreeCount: 0, worktrees: [] };
        if (cancelled || generation !== metadataGeneration.current) return null;
        const worktrees = await gitWorktreeList(path);
        if (cancelled || generation !== metadataGeneration.current) return null;
        return { git, worktreeCount: worktrees.length, worktrees };
      })
      .then((result) => {
        if (!result || cancelled || generation !== metadataGeneration.current) return;
        const { git, worktreeCount, worktrees } = result;
        updateSelection({
          metadataStatus: "ready",
          metadataError: undefined,
          git,
          worktreeCount,
          worktrees,
        });
      })
      .catch((cause) => {
        if (cancelled || generation !== metadataGeneration.current) return;
        const message = cause instanceof Error ? cause.message : String(cause);
        updateSelection({
          metadataStatus: "unavailable",
          metadataError: message,
          git: null,
          worktreeCount: null,
          worktrees: null,
        });
      });
    return () => {
      cancelled = true;
    };
  }, [onFolderMetadataChange, path, metadataRefreshToken]);

  const navigate = (nextPath: string) => {
    const generation = ++navigationGeneration.current;
    setPicking(false);
    setError(null);
    void normalizeWslPath(nextPath)
      .then((normalized) => {
        if (generation === navigationGeneration.current) onPathChange(normalized);
      })
      .catch((cause) => {
        if (generation !== navigationGeneration.current) return;
        setError(cause instanceof Error ? cause.message : String(cause));
      });
  };
  const parent = parentPath(path);
  const breadcrumbs = pathBreadcrumbs(path);
  const browseInExplorer = async () => {
    const generation = ++navigationGeneration.current;
    setPicking(true);
    setError(null);
    try {
      const selected = await pickWslFolder(path || home || "/");
      if (generation === navigationGeneration.current && selected) onPathChange(selected);
    } catch (cause) {
      if (generation !== navigationGeneration.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (generation === navigationGeneration.current) setPicking(false);
    }
  };

  return (
    <div
      className="space-y-2 rounded border p-2"
      style={{ borderColor: "var(--th-border)", background: "var(--th-app-bg)" }}
    >
      <div className="flex flex-wrap gap-1">
        <ShortcutButton
          label={picking ? "Opening Explorer..." : "Browse in Explorer"}
          onClick={() => void browseInExplorer()}
          icon={<FolderOpen size={12} />}
          disabled={picking}
        />
        {home && (
          <ShortcutButton label="Home" onClick={() => navigate(home)} icon={<Home size={12} />} />
        )}
        {recentPaths.slice(0, 3).map((recent) => (
          <ShortcutButton
            key={recent.path}
            label={recent.label}
            onClick={() => navigate(recent.path)}
            icon={<Folder size={12} />}
          />
        ))}
      </div>

      <nav aria-label="WSL folder breadcrumbs" className="flex min-w-0 items-center gap-0.5 overflow-x-auto">
        {breadcrumbs.map((crumb, index) => (
          <span key={crumb.path} className="flex items-center gap-0.5">
            {index > 0 && <ChevronRight size={12} aria-hidden="true" />}
            <button
              type="button"
              className="whitespace-nowrap rounded px-1.5 py-1 text-xs hover:bg-white/10"
              onClick={() => navigate(crumb.path)}
            >
              {crumb.label}
            </button>
          </span>
        ))}
      </nav>

      <div className="flex gap-1">
        <button
          type="button"
          aria-label="Parent folder"
          title="Parent folder"
          disabled={!parent}
          className="grid h-8 w-8 shrink-0 place-items-center rounded border disabled:opacity-40"
          style={{ borderColor: "var(--th-border)" }}
          onClick={() => parent && navigate(parent)}
        >
          <MoveUp size={14} aria-hidden="true" />
        </button>
        <input
          aria-label="Manual WSL path"
          value={manualPath}
          onChange={(event) => setManualPath(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") navigate(manualPath);
          }}
          className="h-8 min-w-0 flex-1 rounded border px-2 font-mono text-xs outline-none"
          style={{ background: "var(--th-tile-bg)", borderColor: "var(--th-border)" }}
        />
        <button
          type="button"
          className="h-8 rounded border px-3 text-xs font-medium"
          style={{ borderColor: "var(--th-border)" }}
          onClick={() => navigate(manualPath)}
        >
          Go
        </button>
      </div>

      <div className="max-h-40 overflow-y-auto rounded border" style={{ borderColor: "var(--th-border)" }}>
        {listing.kind === "idle" ? (
          <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            Choose a WSL folder.
          </p>
        ) : listing.kind === "loading" ? (
          <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            Loading folders...
          </p>
        ) : listing.kind === "error" ? (
          <div className="px-2 py-3 text-xs text-red-300">
            <p>Could not list this folder: {listing.message}</p>
            <button
              type="button"
              className="mt-1 underline"
              onClick={() => setLocalListingRefreshToken((current) => current + 1)}
            >
              Retry folder listing
            </button>
          </div>
        ) : listing.kind === "stale" ? (
          <>
            <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
              Refreshing this folder listing. Previous {listing.prior} results are stale.
              {listing.prior === "populated" && " Review only; select after refresh completes."}
            </p>
            {listing.prior === "populated" && entries.map((entry) => (
              <button
                key={entry.path}
                type="button"
                disabled
                aria-disabled="true"
                className="flex w-full items-center gap-2 border-b px-2 py-1.5 text-left text-xs opacity-60 last:border-b-0"
                style={{ borderColor: "var(--th-border)" }}
              >
                <Folder size={13} aria-hidden="true" />
                <span className="min-w-0 flex-1 truncate">{entry.name}</span>
              </button>
            ))}
          </>
        ) : listing.kind === "loaded-empty" ? (
          <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            This folder is empty.
          </p>
        ) : listing.kind === "loaded-populated" ? (
          entries.map((entry) => (
            <button
              key={entry.path}
              type="button"
              className="flex w-full items-center gap-2 border-b px-2 py-1.5 text-left text-xs last:border-b-0 hover:bg-white/10"
              style={{ borderColor: "var(--th-border)" }}
              onClick={() => navigate(entry.path)}
            >
              <Folder size={13} aria-hidden="true" />
              <span className="min-w-0 flex-1 truncate">{entry.name}</span>
            </button>
          ))
        ) : null}
      </div>

      {error && <p className="text-xs text-red-300" role="alert">{error}</p>}
    </div>
  );
}

function ShortcutButton({
  label,
  icon,
  onClick,
  disabled = false,
}: {
  label: string;
  icon: ReactNode;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className="flex h-7 items-center gap-1 rounded border px-2 text-xs"
      style={{ borderColor: "var(--th-border)" }}
      onClick={onClick}
      disabled={disabled}
    >
      {icon}
      {label}
    </button>
  );
}

export function normalizePosixPath(path: string): string | null {
  const trimmed = path.trim();
  if (!trimmed.startsWith("/") || trimmed.startsWith("//") || trimmed.includes("\\")) return null;
  const parts = trimmed.split("/").filter((part) => part && part !== ".");
  const normalized: string[] = [];
  for (const part of parts) {
    if (part === "..") return null;
    normalized.push(part);
  }
  return `/${normalized.join("/")}`;
}

export function parentPath(path: string): string | null {
  const normalized = normalizePosixPath(path);
  if (!normalized || normalized === "/") return null;
  const splitAt = normalized.lastIndexOf("/");
  return splitAt === 0 ? "/" : normalized.slice(0, splitAt);
}

export function pathBreadcrumbs(path: string): Array<{ label: string; path: string }> {
  const normalized = normalizePosixPath(path) ?? "/";
  const parts = normalized.split("/").filter(Boolean);
  return [
    { label: "/", path: "/" },
    ...parts.map((part, index) => ({
      label: part,
      path: `/${parts.slice(0, index + 1).join("/")}`,
    })),
  ];
}
