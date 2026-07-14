import { useEffect, useMemo, useState, type ReactNode } from "react";
import { ShipWheel, X } from "lucide-react";
import {
  bindProjectPowder,
  commissionCaptain,
  listProjects,
  registerProject,
  type RegisteredProject,
} from "../ipc/projects";
import { WslFolderPicker } from "./WslFolderPicker";

interface CaptainCommissionDialogProps {
  open: boolean;
  onClose: () => void;
  onCommissioned: () => void;
}

type ProjectMode = "saved" | "existing";

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
  const [projectId, setProjectId] = useState("");
  const [repoRoot, setRepoRoot] = useState("");
  const [projectName, setProjectName] = useState("");
  const [powderRepository, setPowderRepository] = useState("");
  const [connectionProfile, setConnectionProfile] = useState("default");
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
    if (!selected) return;
    setPowderRepository(selected.powder?.repository ?? "");
    setConnectionProfile(selected.powder?.connectionProfile ?? "default");
  }, [selected]);

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
    setBusy(true);
    setError(null);
    try {
      let project: RegisteredProject;
      if (mode === "existing") {
        if (!repoRoot.trim()) throw new Error("WSL folder is required.");
        project = await registerProject({
          repoRoot: repoRoot.trim(),
          name: projectName.trim() || undefined,
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
            className="grid grid-cols-2 rounded border p-0.5"
            style={{ borderColor: "var(--th-border)" }}
          >
            {(["saved", "existing"] as const).map((value) => (
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
                  setError(null);
                }}
              >
                {value === "saved"
                  ? "Use saved codebase"
                  : "Choose existing WSL folder"}
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
          ) : (
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
          )}

          <Field label="Powder board">
            <input
              aria-label="Powder board"
              value={powderRepository}
              onChange={(event) => setPowderRepository(event.target.value)}
              className={inputClass}
              style={fieldStyle}
            />
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
            disabled={busy}
          >
            {busy ? "Creating..." : "Create Captain"}
          </button>
        </footer>
      </div>
    </div>
  );
}

function ReviewSummary({
  mode,
  selected,
  repoRoot,
  powderRepository,
  connectionProfile,
  assignment,
  harness,
}: {
  mode: ProjectMode;
  selected?: RegisteredProject;
  repoRoot: string;
  powderRepository: string;
  connectionProfile: string;
  assignment: string;
  harness: "codex" | "claude";
}) {
  const source = mode === "saved" ? "Saved codebase" : "Existing WSL codebase";
  const location =
    mode === "saved"
      ? selected
        ? `${selected.name} · ${selected.repoRoot}`
        : "Select a codebase"
      : repoRoot.trim() || "Choose a WSL folder";

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
        <ReviewRow
          label="Powder"
          value={`${powderRepository.trim() || "Not selected"} via ${connectionProfile.trim() || "Not selected"}`}
        />
        <ReviewRow label="Assignment" value={assignment.trim() || "Required"} />
        <ReviewRow label="Harness" value={harness === "codex" ? "Codex" : "Claude"} />
        <ReviewRow label="Permissions" value="Harness default" />
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
