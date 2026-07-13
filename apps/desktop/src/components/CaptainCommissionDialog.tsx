import { useEffect, useMemo, useState, type ReactNode } from "react";
import { ShipWheel, X } from "lucide-react";
import {
  bindProjectPowder,
  commissionCaptain,
  listProjects,
  registerProject,
  type RegisteredProject,
} from "../ipc/projects";

interface CaptainCommissionDialogProps {
  open: boolean;
  onClose: () => void;
  onCommissioned: () => void;
}

type ProjectMode = "registered" | "register";

export function CaptainCommissionDialog({
  open,
  onClose,
  onCommissioned,
}: CaptainCommissionDialogProps) {
  const [mode, setMode] = useState<ProjectMode>("registered");
  const [projects, setProjects] = useState<RegisteredProject[]>([]);
  const [projectId, setProjectId] = useState("");
  const [repoRoot, setRepoRoot] = useState("");
  const [projectName, setProjectName] = useState("");
  const [powderRepository, setPowderRepository] = useState("");
  const [connectionProfile, setConnectionProfile] = useState("production");
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
        const first = catalog.projects[0];
        setProjectId((current) => current || first?.projectId || "");
        if (catalog.projects.length === 0) setMode("register");
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
    setConnectionProfile(selected.powder?.connectionProfile ?? "production");
  }, [selected]);

  if (!open) return null;

  const submit = async () => {
    if (!assignment.trim()) {
      setError("Assignment is required.");
      return;
    }
    if (!powderRepository.trim()) {
      setError("Powder repository is required.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      let project: RegisteredProject;
      if (mode === "register") {
        if (!repoRoot.trim()) throw new Error("Repository path is required.");
        project = await registerProject({
          repoRoot: repoRoot.trim(),
          name: projectName.trim() || undefined,
          powderRepository: powderRepository.trim(),
          powderConnectionProfile: connectionProfile.trim() || "default",
        });
      } else {
        if (!selected) throw new Error("Select a registered project.");
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
        aria-labelledby="commission-captain-title"
        className="flex max-h-[calc(100vh-2rem)] w-full max-w-lg flex-col overflow-hidden rounded-lg border shadow-2xl"
        style={{ background: "var(--th-tile-bg)", borderColor: "var(--th-border)" }}
        onPointerDown={(event) => event.stopPropagation()}
      >
        <header
          className="flex h-12 items-center gap-2 border-b px-4"
          style={{ borderColor: "var(--th-border)" }}
        >
          <ShipWheel size={18} aria-hidden="true" />
          <h2 id="commission-captain-title" className="min-w-0 flex-1 text-sm font-semibold">
            Commission Captain
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
            className="grid grid-cols-2 rounded border p-0.5"
            style={{ borderColor: "var(--th-border)" }}
          >
            {(["registered", "register"] as const).map((value) => (
              <button
                key={value}
                type="button"
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
                {value === "registered" ? "Registered project" : "Register repository"}
              </button>
            ))}
          </div>

          {mode === "registered" ? (
            <Field label="Project">
              <select
                aria-label="Project"
                value={projectId}
                onChange={(event) => setProjectId(event.target.value)}
                className={inputClass}
                style={fieldStyle}
              >
                <option value="">Select project</option>
                {projects.map((project) => (
                  <option key={project.projectId} value={project.projectId}>
                    {project.name}
                  </option>
                ))}
              </select>
            </Field>
          ) : (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="Repository path" wide>
                <input
                  aria-label="Repository path"
                  value={repoRoot}
                  onChange={(event) => setRepoRoot(event.target.value)}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="/home/user/project"
                />
              </Field>
              <Field label="Project name">
                <input
                  aria-label="Project name"
                  value={projectName}
                  onChange={(event) => setProjectName(event.target.value)}
                  className={inputClass}
                  style={fieldStyle}
                  placeholder="Derived from repository"
                />
              </Field>
            </div>
          )}

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field label="Powder repository">
              <input
                aria-label="Powder repository"
                value={powderRepository}
                onChange={(event) => setPowderRepository(event.target.value)}
                className={inputClass}
                style={fieldStyle}
              />
            </Field>
            <Field label="Connection profile">
              <input
                aria-label="Connection profile"
                value={connectionProfile}
                onChange={(event) => setConnectionProfile(event.target.value)}
                className={inputClass}
                style={fieldStyle}
              />
            </Field>
          </div>

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
            {busy ? "Commissioning..." : "Commission Captain"}
          </button>
        </footer>
      </div>
    </div>
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
