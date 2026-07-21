import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ShipWheel, X } from "lucide-react";
import {
  commissionCaptain,
  listProjects,
  registerProject,
  type RegisteredProject,
} from "../ipc/projects";
import { WslFolderPicker, type WslFolderSelection } from "./WslFolderPicker";

interface GitReviewInfo {
  remoteUrl?: string | null;
  defaultBranch?: string | null;
  branch?: string | null;
  headCommit?: string | null;
  dirtyCount?: number;
  isLinkedWorktree?: boolean;
}

interface CaptainCommissionDialogProps {
  open: boolean;
  onClose: () => void;
  onCommissioned: () => void;
}

type ProjectMode = "saved" | "existing" | "new";

export function CaptainCommissionDialog({
  open,
  onClose,
  onCommissioned,
}: CaptainCommissionDialogProps) {
  const [mode, setMode] = useState<ProjectMode>("existing");
  const [projects, setProjects] = useState<RegisteredProject[]>([]);
  const [wslHome, setWslHome] = useState("");
  const [wslHomeError, setWslHomeError] = useState<string | null>(null);
  const [projectId, setProjectId] = useState("");
  const [repoRoot, setRepoRoot] = useState("");
  const [projectName, setProjectName] = useState("");
  const [newParent, setNewParent] = useState("");
  const [newDisplayName, setNewDisplayName] = useState("");
  const [newDestinationLeaf, setNewDestinationLeaf] = useState("");
  const [folderSelection, setFolderSelection] = useState<WslFolderSelection | null>(null);
  const [metadataRetry, setMetadataRetry] = useState(0);
  const [assignment, setAssignment] = useState("");
  const [harness, setHarness] = useState<"codex" | "claude">("codex");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [registeredProject, setRegisteredProject] = useState<RegisteredProject | null>(null);
  const sourceChoiceOwned = useRef(false);
  const claimSourceChoice = useCallback(() => {
    sourceChoiceOwned.current = true;
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [busy, onClose, open]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setError(null);
    void listProjects()
      .then((catalog) => {
        if (cancelled) return;
        setProjects(catalog.projects);
        setWslHome(catalog.wslHome ?? "");
        setWslHomeError(catalog.wslHomeError ?? null);
        setNewParent((current) => current || catalog.wslHome || "/home");
        const first = catalog.projects[0];
        setProjectId((current) => current || first?.projectId || "");
        setRepoRoot((current) => current || catalog.wslHome || first?.rootPath || "/home");
        setMode((current) => {
          if (sourceChoiceOwned.current || current !== "existing") return current;
          return catalog.projects.length > 0 ? "saved" : "existing";
        });
      })
      .catch((cause) => {
        if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  const selected = useMemo(
    () => projects.find((project) => project.projectId === projectId),
    [projectId, projects],
  );

  const handleFolderMetadataChange = useCallback((selection: WslFolderSelection) => {
    setFolderSelection(selection);
  }, []);
  const retryFolderMetadata = useCallback(() => {
    setMetadataRetry((current) => current + 1);
  }, []);

  if (!open) return null;

  const submit = async () => {
    if (!assignment.trim()) {
      setError("Assignment is required.");
      return;
    }
    if (mode === "existing") {
      if (!projectName.trim()) {
        setError("Codebase name is required.");
        return;
      }
    }
    let newCodebaseDestination: string | null = null;
    if (mode === "new") {
      try {
        validateNewCodebaseDestination(newParent, newDestinationLeaf);
        if (!newDisplayName.trim()) throw new Error("Codebase name is required.");
        newCodebaseDestination = `${newParent.trim().replace(/\/+$/, "")}/${newDestinationLeaf.trim()}`;
      } catch (validationError) {
        setError(validationError instanceof Error ? validationError.message : String(validationError));
        return;
      }
    }
    setBusy(true);
    setError(null);
    let project: RegisteredProject;
    try {
      if (mode === "existing") {
        if (!repoRoot.trim()) throw new Error("WSL folder is required.");
        project = registeredProject?.rootPath === repoRoot.trim() && registeredProject.name === projectName.trim()
          ? registeredProject
          : await registerProject({ rootPath: repoRoot.trim(), name: projectName.trim() });
      } else if (mode === "new") {
        project = registeredProject?.rootPath === newCodebaseDestination && registeredProject.name === newDisplayName.trim()
          ? registeredProject
          : await registerProject({
              rootPath: newCodebaseDestination!,
              name: newDisplayName.trim(),
              createDirectory: true,
            });
      } else {
        if (!selected) throw new Error("Select a saved codebase.");
        project = selected;
      }
      if (mode !== "saved") setRegisteredProject(project);
    } catch (cause) {
      setError(formatRegistrationFailure(cause));
      setBusy(false);
      return;
    }
    try {
      await commissionCaptain({
        projectId: project.projectId,
        assignment: assignment.trim(),
        harness,
      });
      onCommissioned();
    } catch (cause) {
      setError(formatCommissionFailure(cause, project, mode !== "saved"));
      setBusy(false);
      return;
    } finally {
      setBusy(false);
    }
    onClose();
  };

  const inputClass =
    "h-9 w-full rounded border px-2 text-sm outline-none focus:ring-1";

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/55 p-4"
      role="presentation"
      onPointerDown={() => {
        if (!busy) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-busy={busy}
        aria-labelledby="create-captain-title"
        className="flex max-h-[calc(100vh-2rem)] w-full max-w-lg flex-col overflow-hidden rounded-lg border shadow-2xl"
        style={{ background: "var(--th-tile-bg)", borderColor: "var(--th-border)" }}
        onPointerDown={(event) => event.stopPropagation()}
      >
        <header
          className="flex h-12 items-center gap-2 border-b px-4"
          style={{ borderColor: "var(--th-border)" }}
        >
          <ShipWheel size={18} aria-hidden="true" />
          <h2
            id="create-captain-title"
            className="min-w-0 flex-1 text-sm font-semibold"
          >
            Create Captain
          </h2>
          <button
            type="button"
            className="flex h-8 w-8 items-center justify-center rounded hover:bg-white/10"
            onClick={() => {
              if (!busy) onClose();
            }}
            disabled={busy}
            aria-disabled={busy}
            aria-label="Close"
            title="Close"
          >
            <X size={17} />
          </button>
        </header>

        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
          <div
            role="group"
            aria-label="Codebase source"
            className="grid grid-cols-3 rounded border p-0.5"
            style={{ borderColor: "var(--th-border)" }}
          >
            {(["saved", "existing", "new"] as const).map((value) => (
              <button
                key={value}
                type="button"
                aria-pressed={mode === value}
                className="h-8 rounded text-xs font-medium"
                style={{
                  background: mode === value ? "var(--th-accent)" : "transparent",
                  color:
                    mode === value
                      ? "var(--th-accent-fg, var(--th-fg))"
                      : "var(--th-fg-muted)",
                }}
                onClick={() => {
                  claimSourceChoice();
                  setMode(value);
                  setRegisteredProject(null);
                  setError(null);
                }}
              >
                {value === "saved"
                  ? "Use saved codebase"
                  : value === "existing"
                    ? "Choose existing WSL folder"
                    : "Create new codebase"}
              </button>
            ))}
          </div>

          {mode === "saved" ? (
            <Field label="Saved codebase">
              <select
                aria-label="Saved codebase"
                value={projectId}
                onChange={(event) => {
                  claimSourceChoice();
                  setProjectId(event.target.value);
                }}
                className={inputClass}
                style={fieldStyle}
              >
                <option value="">Select codebase</option>
                {projects.map((project) => (
                  <option key={project.projectId} value={project.projectId}>
                    {project.name}
                  </option>
                ))}
              </select>
            </Field>
          ) : mode === "existing" ? (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="WSL folder" wide>
                <WslFolderPicker
                  path={repoRoot}
                  home={wslHome || undefined}
                  recentPaths={[...projects]
                    .sort((a, b) => b.updatedAt - a.updatedAt)
                    .map((project) => ({
                      label: project.name,
                      path: project.rootPath ?? project.repoRoot,
                    }))}
                  onPathChange={(path) => {
                    claimSourceChoice();
                    setRepoRoot(path);
                    setRegisteredProject(null);
                  }}
                  onFolderMetadataChange={handleFolderMetadataChange}
                  metadataRefreshToken={metadataRetry}
                />
                {wslHomeError && (
                  <p className="mt-1 text-xs text-amber-300">{wslHomeError}</p>
                )}
              </Field>
              <Field label="Codebase name">
                <input
                  aria-label="Codebase name"
                  value={projectName}
                  onChange={(event) => {
                    claimSourceChoice();
                    setProjectName(event.target.value);
                    setRegisteredProject(null);
                  }}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="Enter a codebase name"
                />
              </Field>
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="Parent WSL folder" wide>
                <WslFolderPicker
                  path={newParent}
                  home={wslHome || undefined}
                  recentPaths={[...projects]
                    .sort((a, b) => b.updatedAt - a.updatedAt)
                    .map((project) => ({
                      label: project.name,
                      path: project.rootPath ?? project.repoRoot,
                    }))}
                  onPathChange={(path) => {
                    claimSourceChoice();
                    setNewParent(path);
                    setRegisteredProject(null);
                  }}
                />
              </Field>
              <Field label="Codebase name">
                <input
                  aria-label="New codebase name"
                  value={newDisplayName}
                  onChange={(event) => {
                    claimSourceChoice();
                    setNewDisplayName(event.target.value);
                    setRegisteredProject(null);
                  }}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="my-project"
                />
              </Field>
              <Field label="Destination folder name">
                <input
                  aria-label="Destination folder name"
                  value={newDestinationLeaf}
                  onChange={(event) => {
                    claimSourceChoice();
                    setNewDestinationLeaf(event.target.value);
                    setRegisteredProject(null);
                  }}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="project-folder"
                />
              </Field>
              <Field label="Destination">
                <output className="block min-h-9 break-all rounded border px-2 py-2 font-mono text-xs" style={fieldStyle}>
                  {previewNewCodebaseDestination(newParent, newDestinationLeaf) || "Choose a parent and destination folder"}
                </output>
              </Field>
              <div className="rounded border border-blue-400/30 bg-blue-400/10 px-3 py-2 text-xs sm:col-span-2">
                <span className="block font-medium">Starting point: Empty codebase</span>
                <span style={{ color: "var(--th-fg-muted)" }}>
                  Template and clone starting points will be added later.
                </span>
              </div>
            </div>
          )}

          <Field label="Assignment">
            <textarea
              aria-label="Assignment"
              value={assignment}
              onChange={(event) => setAssignment(event.target.value)}
              className="min-h-20 w-full resize-y rounded border px-2 py-2 text-sm outline-none focus:ring-1"
              style={fieldStyle}
            />
          </Field>

          <Field label="Harness">
            <div
              className="grid grid-cols-2 rounded border p-0.5"
              style={{ borderColor: "var(--th-border)" }}
            >
              {(["codex", "claude"] as const).map((value) => (
                <button
                  key={value}
                  type="button"
                  aria-pressed={harness === value}
                  className="h-8 rounded text-xs font-medium capitalize"
                  style={{
                    background: harness === value ? "var(--th-accent)" : "transparent",
                    color:
                      harness === value
                        ? "var(--th-accent-fg, var(--th-fg))"
                        : "var(--th-fg-muted)",
                  }}
                  onClick={() => setHarness(value)}
                >
                  {value}
                </button>
              ))}
            </div>
          </Field>

          <ReviewSummary
            mode={mode}
            selected={selected}
            folderSelection={folderSelection}
            displayName={
              mode === "saved"
                ? selected?.name ?? ""
                : mode === "existing"
                  ? projectName
                  : newDisplayName
            }
            onRetryFolderMetadata={retryFolderMetadata}
            repoRoot={repoRoot}
            assignment={assignment}
            harness={harness}
            newParent={newParent}
            newDestinationLeaf={newDestinationLeaf}
          />

          {error && (
            <div role="alert" className="rounded border border-red-500/50 bg-red-500/10 px-3 py-2 text-xs text-red-300">
              {error}
            </div>
          )}
        </div>

        <footer
          className="flex items-center justify-end gap-2 border-t px-4 py-3"
          style={{ borderColor: "var(--th-border)" }}
        >
          <button
            type="button"
            className="h-9 px-3 text-sm"
            onClick={() => {
              if (!busy) onClose();
            }}
            disabled={busy}
          >
            Cancel
          </button>
          <button
            type="button"
            className="h-9 rounded px-4 text-sm font-medium disabled:opacity-50"
            style={{
              background: "var(--th-accent)",
              color: "var(--th-accent-fg, var(--th-fg))",
            }}
            onClick={() => void submit()}
            disabled={busy || (
              mode === "saved"
                ? !selected
                : mode === "existing"
                  ? !folderSelection || folderSelection.path !== repoRoot.trim() ||
                    !["valid-empty", "valid-populated"].includes(folderSelection.listingStatus) ||
                    folderSelection.metadataStatus === "checking"
                  : false
            )}
          >
            {busy ? "Creating..." : "Create Captain"}
          </button>
        </footer>
      </div>
    </div>
  );
}

function structuredFailure(cause: unknown): {
  message: string;
  kind?: string;
  details?: unknown;
} {
  if (cause && typeof cause === "object") {
    const value = cause as { message?: unknown; kind?: unknown; errorKind?: unknown; details?: unknown; errorDetails?: unknown };
    return {
      message: typeof value.message === "string" ? value.message : String(cause),
      kind: typeof value.kind === "string" ? value.kind : typeof value.errorKind === "string" ? value.errorKind : undefined,
      details: value.details ?? value.errorDetails,
    };
  }
  return { message: cause instanceof Error ? cause.message : String(cause) };
}

function failureEvidence(failure: ReturnType<typeof structuredFailure>): string {
  const kind = failure.kind ? ` [${failure.kind}]` : "";
  const details = failure.details === undefined ? "" : ` Details: ${JSON.stringify(failure.details)}`;
  return `${failure.message}${kind}${details}`;
}

function formatRegistrationFailure(cause: unknown): string {
  const failure = structuredFailure(cause);
  const normalized = `${failure.kind ?? ""} ${failure.message}`.toLowerCase();
  if (normalized.includes("unauthor") || normalized.includes("acl")) {
    return `Codebase registration is not authorized. ${failureEvidence(failure)}`;
  }
  if (normalized.includes("list") || normalized.includes("folder") || normalized.includes("path")) {
    return `Codebase registration could not validate the selected folder. ${failureEvidence(failure)}`;
  }
  return `Codebase registration failed. ${failureEvidence(failure)}`;
}

function formatCommissionFailure(cause: unknown, project: RegisteredProject, registered: boolean): string {
  const failure = structuredFailure(cause);
  const normalized = `${failure.kind ?? ""} ${failure.message}`.toLowerCase();
  const action = normalized.includes("capacity")
    ? "Captain capacity is unavailable; retry after capacity is released."
    : "Retry Captain creation to commission the existing codebase.";
  if (!registered) {
    return `Captain creation failed for existing codebase "${project.name}": ${failureEvidence(failure)} ${action}`;
  }
  const registeredAction = normalized.includes("capacity")
    ? "Captain capacity is unavailable; retry after capacity is released."
    : "Retry Captain creation to commission the registered Project.";
  return `Codebase "${project.name}" was registered as Project ${project.projectId}, but Captain creation failed: ${failureEvidence(failure)} ${registeredAction}`;
}

function previewNewCodebaseDestination(parent: string, name: string): string {
  const base = parent.trim().replace(/\/+$/, "");
  const leaf = name.trim();
  return base && leaf ? `${base}/${leaf}` : "";
}

export function validateNewCodebaseDestination(parent: string, name: string): string {
  const base = parent.trim().replace(/\/+$/, "");
  const leaf = name.trim();
  if (!base.startsWith("/") || base.startsWith("//") || base.includes("\\")) {
    throw new Error("Parent must be an absolute WSL path.");
  }
  if (!leaf) throw new Error("Destination folder name is required.");
  if (leaf === "." || leaf === ".." || /[\\/\u0000-\u001f\u007f]/.test(leaf)) {
    throw new Error("Destination folder name must be one safe folder name.");
  }
  return `${base}/${leaf}`;
}

function ReviewSummary({
  mode,
  selected,
  folderSelection,
  displayName,
  onRetryFolderMetadata,
  repoRoot,
  assignment,
  harness,
  newParent,
  newDestinationLeaf,
}: {
  mode: ProjectMode;
  selected?: RegisteredProject;
  folderSelection: WslFolderSelection | null;
  displayName: string;
  onRetryFolderMetadata: () => void;
  repoRoot: string;
  assignment: string;
  harness: "codex" | "claude";
  newParent: string;
  newDestinationLeaf: string;
}) {
  const source =
    mode === "saved"
      ? "Saved codebase"
      : mode === "existing"
        ? "Existing WSL codebase"
        : "New empty codebase";
  const location =
    mode === "saved"
      ? selected
        ? `${selected.name} · ${selected.rootPath}`
        : "Select a codebase"
      : mode === "existing"
        ? repoRoot.trim() || "Choose a WSL folder"
        : previewNewCodebaseDestination(newParent, newDestinationLeaf) || "Choose a parent and destination folder";

  return (
    <section
      aria-labelledby="captain-preflight-title"
      className="rounded border p-3"
      style={{ borderColor: "var(--th-border)", background: "var(--th-app-bg)" }}
    >
      <h3 id="captain-preflight-title" className="text-xs font-semibold">
        Review before creating
      </h3>
      <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
        <ReviewRow label="Source" value={source} />
        <ReviewRow label="Codebase name" value={displayName.trim() || "Required"} />
        <ReviewRow label="Codebase" value={location} />
        {mode === "existing" && (
          <FolderValidationSummary
            selection={folderSelection}
            rootPath={repoRoot}
          />
        )}
        {mode === "saved" && (
          <VersionControlSummary project={selected} />
        )}
        {mode === "existing" && (
          <VersionControlSummary
            selection={folderSelection?.path === repoRoot.trim() ? folderSelection : null}
            onRetry={onRetryFolderMetadata}
          />
        )}
        {mode === "new" && (
          <>
            <ReviewRow label="Filesystem changes" value={`Create ${location}`} />
            <ReviewRow label="External effects" value="No remote service calls" />
          </>
        )}
        <ReviewRow label="Assignment" value={assignment.trim() || "Required"} />
        <ReviewRow label="Harness" value={harness === "codex" ? "Codex" : "Claude"} />
        <ReviewRow label="Permissions" value="Unrestricted" />
      </dl>
    </section>
  );
}

function FolderValidationSummary({
  selection,
  rootPath,
}: {
  selection: WslFolderSelection | null;
  rootPath: string;
}) {
  if (!selection || selection.path !== rootPath.trim()) {
    return <ReviewRow label="Folder validation" value="Checking..." />;
  }
  if (selection.listingStatus === "loading") {
    return <ReviewRow label="Folder validation" value="Checking..." />;
  }
  if (selection.listingStatus === "stale") {
    return <ReviewRow label="Folder validation" value="Refreshing..." />;
  }
  if (selection.listingStatus === "error") {
    return <ReviewRow label="Folder validation" value={`Unavailable: ${selection.listingError ?? "directory listing failed"}`} />;
  }
  return (
    <ReviewRow
      label="Folder validation"
      value={selection.listingStatus === "valid-empty" ? "Valid empty folder" : "Valid folder"}
    />
  );
}

function VersionControlSummary({
  selection = null,
  project,
  onRetry,
}: {
  selection?: WslFolderSelection | null;
  project?: RegisteredProject;
  onRetry?: () => void;
}) {
  if (project) {
    if (project.vcsCapability === "none") {
      return <ReviewRow label="Version control" value="None" />;
    }
    return (
      <GitMetadataRows
        git={{
          remoteUrl: project.remoteUrl ?? null,
          defaultBranch: project.defaultBranch ?? null,
        }}
        gitMainRoot={project.gitMainRoot ?? null}
        worktreeCount={null}
        worktrees={null}
      />
    );
  }
  if (!selection || selection.metadataStatus === "checking") {
    return <ReviewRow label="Version control" value="Checking..." />;
  }
  if (selection.metadataStatus === "unavailable") {
    return (
      <>
        <ReviewRow label="Version control" value={`Unavailable: ${selection.metadataError ?? "unknown error"}`} />
        {onRetry && (
          <dd className="col-start-2">
            <button type="button" className="text-xs underline" onClick={onRetry}>
              Retry Version control check
            </button>
          </dd>
        )}
      </>
    );
  }
  if (!selection.git?.isRepo) {
    return <ReviewRow label="Version control" value="None" />;
  }
  return (
    <GitMetadataRows
      git={selection.git}
      gitMainRoot={selection.git.worktreeRoot}
      worktreeCount={selection.worktreeCount}
      worktrees={selection.worktrees}
    />
  );
}

function GitMetadataRows({
  git,
  gitMainRoot,
  worktreeCount,
  worktrees,
}: {
  git: GitReviewInfo;
  gitMainRoot: string | null;
  worktreeCount: number | null;
  worktrees: WslFolderSelection["worktrees"];
}) {
  const mainCount = worktrees?.filter((worktree) => !worktree.isLinked).length;
  const linkedCount = worktrees?.filter((worktree) => worktree.isLinked).length;
  const worktreeValue = worktreeCount === null
    ? "Unknown"
    : mainCount === undefined || linkedCount === undefined
      ? String(worktreeCount)
      : `${worktreeCount} (main ${mainCount}, linked ${linkedCount})`;
  return (
    <>
      <ReviewRow label="Version control" value="Git" />
      <ReviewRow label="Git main root" value={gitMainRoot || "Unknown"} />
      <ReviewRow label="Remote" value={git.remoteUrl || "None configured"} />
      <ReviewRow label="Default branch" value={git.defaultBranch || "Unknown"} />
      <ReviewRow label="Current branch" value={git.branch === undefined ? "Unknown" : git.branch || "Detached"} />
      <ReviewRow label="HEAD" value={git.headCommit || "Unknown"} />
      <ReviewRow label="Dirty entries" value={git.dirtyCount === undefined ? "Unknown" : String(git.dirtyCount)} />
      <ReviewRow label="Worktrees" value={worktreeValue} />
      <ReviewRow
        label="Selected worktree"
        value={git.isLinkedWorktree === undefined ? "Unknown" : git.isLinkedWorktree ? "Linked" : "Main"}
      />
    </>
  );
}

function ReviewRow({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt style={{ color: "var(--th-fg-muted)" }}>{label}</dt>
      <dd className="min-w-0 break-words">{value}</dd>
    </>
  );
}

const fieldStyle = {
  background: "var(--th-app-bg)",
  borderColor: "var(--th-border)",
  color: "var(--th-fg)",
};

function Field({
  label,
  children,
  wide = false,
}: {
  label: string;
  children: ReactNode;
  wide?: boolean;
}) {
  return (
    <div className={`block space-y-1 ${wide ? "sm:col-span-2" : ""}`}>
      <span className="text-xs font-medium" style={{ color: "var(--th-fg-muted)" }}>
        {label}
      </span>
      {children}
    </div>
  );
}
