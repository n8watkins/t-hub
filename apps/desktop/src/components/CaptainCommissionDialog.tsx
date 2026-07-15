import { useEffect, useMemo, useState, type ReactNode } from "react";
import { ShipWheel, X } from "lucide-react";
import {
  bindProjectPowder,
  commissionCaptain,
  listPowderBoards,
  listProjects,
  registerProject,
  type PowderBoard,
  type RegisteredProject,
} from "../ipc/projects";
import {
  gitInfo,
  gitWorktreeList,
  type GitInfo,
  type WorktreeInfo,
} from "../ipc/git";
import { WslFolderPicker } from "./WslFolderPicker";

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
  const [mode, setMode] = useState<ProjectMode>("saved");
  const [projects, setProjects] = useState<RegisteredProject[]>([]);
  const [powderProfiles, setPowderProfiles] = useState<string[]>([]);
  const [powderProfilesError, setPowderProfilesError] = useState<string | null>(
    null,
  );
  const [wslHome, setWslHome] = useState("");
  const [wslHomeError, setWslHomeError] = useState<string | null>(null);
  const [folderGit, setFolderGit] = useState<GitInfo | null>(null);
  const [folderWorktrees, setFolderWorktrees] = useState<WorktreeInfo[]>([]);
  const [initializeGit, setInitializeGit] = useState(false);
  const [projectId, setProjectId] = useState("");
  const [repoRoot, setRepoRoot] = useState("");
  const [projectName, setProjectName] = useState("");
  const [newParent, setNewParent] = useState("");
  const [newName, setNewName] = useState("");
  const [powderRepository, setPowderRepository] = useState("");
  const [connectionProfile, setConnectionProfile] = useState("");
  const [powderBoards, setPowderBoards] = useState<PowderBoard[]>([]);
  const [powderBoardsLoading, setPowderBoardsLoading] = useState(false);
  const [powderBoardsError, setPowderBoardsError] = useState<string | null>(null);
  const [powderBoardsRetry, setPowderBoardsRetry] = useState(0);
  const [assignment, setAssignment] = useState("");
  const [harness, setHarness] = useState<"codex" | "claude">("codex");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setError(null);
    void listProjects()
      .then((catalog) => {
        if (cancelled) return;
        setProjects(catalog.projects);
        setPowderProfiles(catalog.powderProfiles ?? []);
        setPowderProfilesError(catalog.powderProfilesError ?? null);
        setWslHome(catalog.wslHome ?? "");
        setWslHomeError(catalog.wslHomeError ?? null);
        setNewParent((current) => current || catalog.wslHome || "/home");
        const first = catalog.projects[0];
        setProjectId((current) => current || first?.projectId || "");
        setRepoRoot((current) => current || catalog.wslHome || first?.repoRoot || "/home");
        if (catalog.projects.length === 0) setMode("existing");
        if (catalog.powderProfiles?.length === 1) {
          setConnectionProfile(catalog.powderProfiles[0]);
        } else if ((catalog.powderProfiles?.length ?? 0) > 1) {
          setConnectionProfile((current) =>
            catalog.powderProfiles?.includes(current) ? current : "",
          );
        } else if (!catalog.powderProfilesError) {
          setConnectionProfile((current) => current || "default");
        }
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

  useEffect(() => {
    if (mode !== "saved" || !selected) return;
    setPowderRepository(selected.powder?.repository ?? "");
    setConnectionProfile(selected.powder?.connectionProfile ?? "default");
  }, [mode, selected]);

  useEffect(() => {
    if (!open) return;
    const profile = connectionProfile.trim();
    if (!profile) {
      setPowderBoards([]);
      setPowderBoardsLoading(false);
      setPowderBoardsError(null);
      return;
    }
    let cancelled = false;
    setPowderBoards([]);
    setPowderBoardsLoading(true);
    setPowderBoardsError(null);
    void loadAllPowderBoards(profile)
      .then((boards) => {
        if (!cancelled) setPowderBoards(boards);
      })
      .catch((cause) => {
        if (!cancelled) {
          setPowderBoardsError(cause instanceof Error ? cause.message : String(cause));
        }
      })
      .finally(() => {
        if (!cancelled) setPowderBoardsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionProfile, open, powderBoardsRetry]);

  useEffect(() => {
    if (powderBoardsLoading || powderBoardsError) return;
    const savedBinding =
      mode === "saved" &&
      selected?.powder?.connectionProfile === connectionProfile
        ? selected.powder.repository
        : "";
    setPowderRepository((current) => {
      if (powderBoards.some((board) => board.name === current)) return current;
      if (savedBinding) return savedBinding;
      if (powderBoards.length === 1) return powderBoards[0].name;
      return "";
    });
  }, [connectionProfile, mode, powderBoards, powderBoardsError, powderBoardsLoading, selected]);

  useEffect(() => {
    if (mode !== "existing" || !repoRoot) return;
    let cancelled = false;
    setFolderGit(null);
    setFolderWorktrees([]);
    setInitializeGit(false);
    void gitInfo(repoRoot)
      .then((info) => {
        if (!cancelled) setFolderGit(info);
      })
      .catch(() => undefined);
    void gitWorktreeList(repoRoot)
      .then((worktrees) => {
        if (!cancelled) setFolderWorktrees(worktrees);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [mode, repoRoot]);

  if (!open) return null;

  const submit = async () => {
    if (!assignment.trim()) {
      setError("Assignment is required.");
      return;
    }
    if (!powderRepository.trim()) {
      setError("Powder board is required.");
      return;
    }
    if (!connectionProfile.trim()) {
      setError("Select a Powder connection profile under Advanced.");
      return;
    }
    if (mode === "existing") {
      if (folderGit === null) {
        setError("Wait for Git inspection to finish before creating the Captain.");
        return;
      }
      if (!folderGit.isRepo && !initializeGit) {
        setError("Initialize Git explicitly or choose a folder that is already a Git repository.");
        return;
      }
    }
    let newCodebaseDestination: string | null = null;
    if (mode === "new") {
      try {
        newCodebaseDestination = validateNewCodebaseDestination(newParent, newName);
      } catch (validationError) {
        setError(validationError instanceof Error ? validationError.message : String(validationError));
        return;
      }
    }
    setBusy(true);
    setError(null);
    try {
      let project: RegisteredProject;
      if (mode === "existing") {
        if (!repoRoot.trim()) throw new Error("WSL folder is required.");
        if (!folderGit) throw new Error("Git inspection is incomplete.");
        project = await registerProject({
          repoRoot: repoRoot.trim(),
          name: projectName.trim() || undefined,
          ...(folderGit.isRepo ? {} : { initializeGit: true }),
          powderRepository: powderRepository.trim(),
          powderConnectionProfile: connectionProfile.trim() || "default",
        });
      } else if (mode === "new") {
        project = await registerProject({
          repoRoot: newCodebaseDestination!,
          name: newName.trim(),
          createDirectory: true,
          initializeGit: true,
          powderRepository: powderRepository.trim(),
          powderConnectionProfile: connectionProfile.trim() || "default",
        });
      } else {
        if (!selected) throw new Error("Select a saved codebase.");
        project = selected;
        if (
          !selected.powder ||
          selected.powder.repository !== powderRepository.trim() ||
          selected.powder.connectionProfile !== connectionProfile.trim()
        ) {
          project = await bindProjectPowder({
            projectId: selected.projectId,
            repository: powderRepository.trim(),
            connectionProfile: connectionProfile.trim() || "default",
          });
        }
      }
      await commissionCaptain({
        projectId: project.projectId,
        assignment: assignment.trim(),
        harness,
      });
      onCommissioned();
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  const inputClass =
    "h-9 w-full rounded border px-2 text-sm outline-none focus:ring-1";

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/55 p-4"
      role="presentation"
      onPointerDown={onClose}
    >
      <div
        role="dialog"
        aria-modal="true"
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
            onClick={onClose}
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
                  setMode(value);
                  if (value !== "saved") setPowderRepository("");
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
                onChange={(event) => setProjectId(event.target.value)}
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
                      path: project.repoRoot,
                    }))}
                  onPathChange={setRepoRoot}
                />
                {wslHomeError && (
                  <p className="mt-1 text-xs text-amber-300">{wslHomeError}</p>
                )}
              </Field>
              <Field label="Codebase name">
                <input
                  aria-label="Codebase name"
                  value={projectName}
                  onChange={(event) => setProjectName(event.target.value)}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="Derived from folder"
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
                      path: project.repoRoot,
                    }))}
                  onPathChange={setNewParent}
                />
              </Field>
              <Field label="Codebase name">
                <input
                  aria-label="New codebase name"
                  value={newName}
                  onChange={(event) => setNewName(event.target.value)}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="my-project"
                />
              </Field>
              <Field label="Destination">
                <output className="block min-h-9 break-all rounded border px-2 py-2 font-mono text-xs" style={fieldStyle}>
                  {previewNewCodebaseDestination(newParent, newName) || "Choose a parent and name"}
                </output>
              </Field>
              <div className="rounded border border-blue-400/30 bg-blue-400/10 px-3 py-2 text-xs sm:col-span-2">
                <span className="block font-medium">Starting point: Empty Git repository</span>
                <span style={{ color: "var(--th-fg-muted)" }}>
                  Template and clone starting points will be added later.
                </span>
              </div>
            </div>
          )}

          {mode === "existing" && folderGit && !folderGit.isRepo && (
            <label
              className="flex items-start gap-2 rounded border border-amber-400/40 bg-amber-400/10 px-3 py-2 text-xs"
            >
              <input
                type="checkbox"
                aria-label="Initialize Git repository"
                checked={initializeGit}
                onChange={(event) => setInitializeGit(event.target.checked)}
                className="mt-0.5"
              />
              <span>
                <span className="block font-medium">Initialize Git repository</span>
                <span style={{ color: "var(--th-fg-muted)" }}>
                  Creates only a .git directory in this existing folder and uses main as the default branch.
                </span>
              </span>
            </label>
          )}

          <Field label="Powder board">
            <select
              aria-label="Powder board"
              value={powderRepository}
              onChange={(event) => setPowderRepository(event.target.value)}
              className={inputClass}
              style={fieldStyle}
              disabled={powderBoardsLoading || !!powderBoardsError || !connectionProfile.trim()}
            >
              {powderBoardsLoading ? (
                <option value="">Loading Powder boards...</option>
              ) : powderBoardsError ? (
                <option value="">Powder boards unavailable</option>
              ) : powderBoards.length === 0 && !powderRepository ? (
                <option value="">No Powder boards found for this profile</option>
              ) : (
                <>
                  {powderBoards.length !== 1 && <option value="">Select Powder board</option>}
                  {mode === "saved" &&
                    powderRepository &&
                    !powderBoards.some((board) => board.name === powderRepository) && (
                      <option value={powderRepository}>
                        {powderRepository} (current binding)
                      </option>
                    )}
                  {powderBoards.map((board) => (
                    <option key={board.name} value={board.name}>
                      {board.name} ({board.tier}, {board.cardCount} cards)
                    </option>
                  ))}
                </>
              )}
            </select>
            {powderBoardsError && (
              <div role="alert" className="mt-1 flex items-center justify-between gap-2 text-xs text-amber-300">
                <span>Could not load Powder boards: {powderBoardsError}</span>
                <button
                  type="button"
                  className="rounded border px-2 py-1"
                  style={{ borderColor: "var(--th-border)" }}
                  onClick={() => setPowderBoardsRetry((value) => value + 1)}
                >
                  Retry
                </button>
              </div>
            )}
          </Field>

          <details
            className="rounded border px-3 py-2"
            style={{ borderColor: "var(--th-border)" }}
          >
            <summary
              className="cursor-pointer text-xs font-medium"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Advanced
            </summary>
            <div className="mt-3 space-y-2">
              <Field label="Powder connection profile">
                {powderProfiles.length > 0 ? (
                  <select
                    aria-label="Powder connection profile"
                    value={connectionProfile}
                    onChange={(event) => setConnectionProfile(event.target.value)}
                    className={inputClass}
                    style={fieldStyle}
                  >
                    {!connectionProfile && <option value="">Select profile</option>}
                    {powderProfiles.map((profile) => (
                      <option key={profile} value={profile}>
                        {profile}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    aria-label="Powder connection profile"
                    value={connectionProfile}
                    onChange={(event) => setConnectionProfile(event.target.value)}
                    className={inputClass}
                    style={fieldStyle}
                  />
                )}
              </Field>
              {powderProfilesError && (
                <p className="text-xs text-amber-300">{powderProfilesError}</p>
              )}
            </div>
          </details>

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
            repoRoot={repoRoot}
            powderRepository={powderRepository}
            connectionProfile={connectionProfile}
            assignment={assignment}
            harness={harness}
            folderGit={folderGit}
            folderWorktrees={folderWorktrees}
            initializeGit={initializeGit}
            newParent={newParent}
            newName={newName}
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
          <button type="button" className="h-9 px-3 text-sm" onClick={onClose} disabled={busy}>
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
            disabled={busy || powderBoardsLoading || !!powderBoardsError || !powderRepository.trim()}
          >
            {busy ? "Creating..." : "Create Captain"}
          </button>
        </footer>
      </div>
    </div>
  );
}

async function loadAllPowderBoards(connectionProfile: string): Promise<PowderBoard[]> {
  const boards: PowderBoard[] = [];
  let offset = 0;
  for (;;) {
    const page = await listPowderBoards({ connectionProfile, offset, limit: 500 });
    if (page.connectionProfile !== connectionProfile) {
      throw new Error("Powder returned boards for a different connection profile.");
    }
    boards.push(...page.boards);
    if (!page.hasMore) return boards;
    if (page.nextOffset === undefined || page.nextOffset <= offset) {
      throw new Error("Powder board pagination did not advance.");
    }
    offset = page.nextOffset;
  }
}

function previewNewCodebaseDestination(parent: string, name: string): string {
  const base = parent.trim().replace(/\/+$/, "");
  const leaf = name.trim();
  return base && leaf ? `${base}/${leaf}` : "";
}

function validateNewCodebaseDestination(parent: string, name: string): string {
  const base = parent.trim().replace(/\/+$/, "");
  const leaf = name.trim();
  if (!base.startsWith("/") || base.startsWith("//") || base.includes("\\")) {
    throw new Error("Parent must be an absolute WSL path.");
  }
  if (!leaf) throw new Error("Codebase name is required.");
  if (leaf === "." || leaf === ".." || /[\\/\u0000-\u001f\u007f]/.test(leaf)) {
    throw new Error("Codebase name must be one safe folder name.");
  }
  return `${base}/${leaf}`;
}

function ReviewSummary({
  mode,
  selected,
  repoRoot,
  powderRepository,
  connectionProfile,
  assignment,
  harness,
  folderGit,
  folderWorktrees,
  initializeGit,
  newParent,
  newName,
}: {
  mode: ProjectMode;
  selected?: RegisteredProject;
  repoRoot: string;
  powderRepository: string;
  connectionProfile: string;
  assignment: string;
  harness: "codex" | "claude";
  folderGit: GitInfo | null;
  folderWorktrees: WorktreeInfo[];
  initializeGit: boolean;
  newParent: string;
  newName: string;
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
        ? `${selected.name} · ${selected.repoRoot}`
        : "Select a codebase"
      : mode === "existing"
        ? repoRoot.trim() || "Choose a WSL folder"
        : previewNewCodebaseDestination(newParent, newName) || "Choose a parent and name";

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
        <ReviewRow label="Codebase" value={location} />
        {mode === "existing" && folderGit && (
          <>
            <ReviewRow
              label="Git"
              value={
                folderGit.isRepo
                  ? `${folderGit.branch ?? "Detached HEAD"} · ${folderGit.dirtyCount ? `${folderGit.dirtyCount} changed` : "clean"} · ${folderGit.isLinkedWorktree ? "linked worktree" : "main worktree"}`
                  : initializeGit
                    ? "Initialize with main as the default branch"
                    : "Not a Git repository - initialization not authorized"
              }
            />
            {folderGit.isRepo && (
              <>
                <ReviewRow
                  label="Remote"
                  value={folderGit.remoteUrl || "No origin remote"}
                />
                <ReviewRow
                  label="Default branch"
                  value={folderGit.defaultBranch || "Not advertised by origin"}
                />
                <ReviewRow
                  label="HEAD"
                  value={folderGit.headCommit?.slice(0, 12) || "Unknown"}
                />
                <ReviewRow
                  label="Worktrees"
                  value={`${folderWorktrees.length || 1} detected`}
                />
              </>
            )}
          </>
        )}
        {mode === "new" && (
          <>
            <ReviewRow label="Git" value="Initialize with main as the default branch" />
            <ReviewRow label="Filesystem changes" value={`Create ${location}`} />
            <ReviewRow
              label="External effects"
              value="Use an existing Powder board; no remote or board will be created"
            />
          </>
        )}
        <ReviewRow
          label="Powder"
          value={`${powderRepository.trim() || "Not selected"} via ${connectionProfile.trim() || "Not selected"}`}
        />
        <ReviewRow label="Assignment" value={assignment.trim() || "Required"} />
        <ReviewRow label="Harness" value={harness === "codex" ? "Codex" : "Claude"} />
        <ReviewRow label="Permissions" value="Unrestricted" />
      </dl>
    </section>
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
