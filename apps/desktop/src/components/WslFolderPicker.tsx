import { ChevronRight, Folder, FolderOpen, Home, MoveUp } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { listDir } from "../ipc/files";
import type { DirEntry } from "../ipc/types";
import { normalizeWslPath, pickWslFolder } from "../ipc/wslFolderDialog";

interface WslFolderPickerProps {
  path: string;
  home?: string;
  recentPaths: Array<{ label: string; path: string }>;
  onPathChange: (path: string) => void;
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
}: WslFolderPickerProps) {
  const [manualPath, setManualPath] = useState(path);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [listing, setListing] = useState<ListingState>({ kind: "idle" });
  const [picking, setPicking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestGeneration = useRef(0);

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
    setListing(() =>
      priorKind ? { kind: "stale", prior: priorKind } : { kind: "loading" },
    );
    setError(null);
    listDir(path)
      .then((nextEntries) => {
        if (cancelled || generation !== requestGeneration.current) return;
        const directories = nextEntries.filter((entry) => entry.isDir);
        setEntries(directories);
        setListing({ kind: directories.length === 0 ? "loaded-empty" : "loaded-populated" });
      })
      .catch((cause) => {
        if (cancelled || generation !== requestGeneration.current) return;
        const message = cause instanceof Error ? cause.message : String(cause);
        setListing({ kind: "error", message });
        setError(message);
      })
      .finally(() => {
        // The explicit listing state prevents an error from being rendered as
        // the successful empty-folder state.
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  const navigate = (nextPath: string) => {
    setError(null);
    void normalizeWslPath(nextPath)
      .then((normalized) => onPathChange(normalized))
      .catch((cause) => {
        setError(cause instanceof Error ? cause.message : String(cause));
      });
  };
  const parent = parentPath(path);
  const breadcrumbs = pathBreadcrumbs(path);
  const browseInExplorer = async () => {
    setPicking(true);
    setError(null);
    try {
      const selected = await pickWslFolder(path || home || "/");
      if (selected) onPathChange(selected);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setPicking(false);
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
          <p className="px-2 py-3 text-xs text-red-300">
            Could not list this folder: {listing.message}
          </p>
        ) : listing.kind === "stale" ? (
          <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            Refreshing this folder listing. Previous {listing.prior} results are stale.
          </p>
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
