import { ChevronRight, Folder, GitBranch, Home, MoveUp } from "lucide-react";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import { listDir } from "../ipc/files";
import { gitInfo, type GitInfo } from "../ipc/git";
import type { DirEntry } from "../ipc/types";

interface WslFolderPickerProps {
  path: string;
  home?: string;
  recentPaths: Array<{ label: string; path: string }>;
  onPathChange: (path: string) => void;
}

export function WslFolderPicker({
  path,
  home,
  recentPaths,
  onPathChange,
}: WslFolderPickerProps) {
  const [manualPath, setManualPath] = useState(path);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [selectedGit, setSelectedGit] = useState<GitInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => setManualPath(path), [path]);

  useEffect(() => {
    if (!path) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setSelectedGit(null);
    listDir(path)
      .then((nextEntries) => {
        if (cancelled) return;
        setEntries(nextEntries.filter((entry) => entry.isDir));
      })
      .catch((cause) => {
        if (cancelled) return;
        setEntries([]);
        setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    void gitInfo(path)
      .then((nextGit) => {
        if (!cancelled) setSelectedGit(nextGit);
      })
      .catch(() => {
        // Git status is supplementary; folder navigation remains available.
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  const navigate = (nextPath: string) => {
    const normalized = normalizePosixPath(nextPath);
    if (normalized) onPathChange(normalized);
  };
  const parent = parentPath(path);
  const breadcrumbs = pathBreadcrumbs(path);

  return (
    <div
      className="space-y-2 rounded border p-2"
      style={{ borderColor: "var(--th-border)", background: "var(--th-app-bg)" }}
    >
      <div className="flex flex-wrap gap-1">
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
        {loading ? (
          <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            Loading folders...
          </p>
        ) : entries.length > 0 ? (
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
              {entry.isGitRepo && (
                <span className="flex items-center gap-1" style={{ color: "var(--th-fg-muted)" }}>
                  <GitBranch size={12} aria-hidden="true" />
                  Git
                </span>
              )}
            </button>
          ))
        ) : (
          <p className="px-2 py-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            No child folders.
          </p>
        )}
      </div>

      {selectedGit && (
        <p className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
          {selectedGit.isRepo
            ? `Git ${selectedGit.branch ?? "detached"}${selectedGit.dirtyCount ? ` · ${selectedGit.dirtyCount} changed` : " · clean"}`
            : "This folder is not a Git repository."}
        </p>
      )}
      {error && <p className="text-xs text-red-300">{error}</p>}
    </div>
  );
}

function ShortcutButton({
  label,
  icon,
  onClick,
}: {
  label: string;
  icon: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="flex h-7 items-center gap-1 rounded border px-2 text-xs"
      style={{ borderColor: "var(--th-border)" }}
      onClick={onClick}
    >
      {icon}
      {label}
    </button>
  );
}

export function normalizePosixPath(path: string): string | null {
  const trimmed = path.trim();
  if (!trimmed.startsWith("/")) return null;
  const parts = trimmed.split("/").filter((part) => part && part !== ".");
  const normalized: string[] = [];
  for (const part of parts) {
    if (part === "..") normalized.pop();
    else normalized.push(part);
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
