import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { controlRequest } from "../ipc/controlClient";
import { gitInfo, type GitInfo } from "../ipc/git";
import { parseDispatchClaims } from "../lib/dispatchClaims";

interface StartAgentDialogProps {
  open: boolean;
  captainSessionId: string;
  directory: string;
  onClose: () => void;
  onStarted: () => void;
}

export function StartAgentDialog({
  open,
  captainSessionId,
  directory,
  onClose,
  onStarted,
}: StartAgentDialogProps) {
  const [assignment, setAssignment] = useState("");
  const [harness, setHarness] = useState<"codex" | "claude">("codex");
  const [requestId, setRequestId] = useState(() => crypto.randomUUID());
  const [laneId, setLaneId] = useState(() => `agent:${requestId}`);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checkout, setCheckout] = useState<GitInfo | null>(null);
  const [baselineError, setBaselineError] = useState<string | null>(null);
  const [visibleProductBug, setVisibleProductBug] = useState(false);
  const [dependencies, setDependencies] = useState("");
  const [mutableFiles, setMutableFiles] = useState("");
  const [mutableSchemas, setMutableSchemas] = useState("");
  const [mutableInterfaces, setMutableInterfaces] = useState("");
  const [integrationContracts, setIntegrationContracts] = useState("");

  useEffect(() => {
    if (!open) return;
    let active = true;
    setCheckout(null);
    setBaselineError(null);
    void gitInfo(directory)
      .then((info) => {
        if (!active) return;
        if (!info.isRepo || !info.headCommit) {
          setBaselineError("The directory does not have a resolvable Git commit.");
          return;
        }
        if (info.dirtyCount > 0) {
          setBaselineError(
            "The checkout has uncommitted work. Preserve it and dispatch from a clean worktree.",
          );
          return;
        }
        setCheckout(info);
      })
      .catch((cause) => {
        if (active) setBaselineError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      active = false;
    };
  }, [directory, open]);

  if (!open) return null;

  const submit = async () => {
    const trimmed = assignment.trim();
    if (!trimmed) {
      setError("Assignment is required.");
      return;
    }
    if (!checkout?.headCommit) {
      setError(baselineError ?? "The exact source commit is still being resolved.");
      return;
    }
    let claims;
    try {
      claims = parseDispatchClaims({
        laneId,
        dependencies,
        mutableFiles,
        mutableSchemas,
        mutableInterfaces,
        integrationContracts,
      });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await controlRequest("start_agent", {
        requestId,
        captainSessionId,
        assignment: trimmed,
        directory,
        harness,
        sourceCommit: checkout.headCommit,
        visibleProductBug,
        ...claims,
      });
      setAssignment("");
      const nextRequestId = crypto.randomUUID();
      setRequestId(nextRequestId);
      setLaneId(`agent:${nextRequestId}`);
      setDependencies("");
      setMutableFiles("");
      setMutableSchemas("");
      setMutableInterfaces("");
      setIntegrationContracts("");
      onStarted();
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/55 p-4"
      role="presentation"
      onPointerDown={onClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="start-agent-title"
        className="max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-lg border p-4 shadow-2xl"
        style={{ background: "var(--th-tile-bg)", borderColor: "var(--th-border)" }}
        onPointerDown={(event) => event.stopPropagation()}
      >
        <header className="mb-4 flex items-center gap-2">
          <h2 id="start-agent-title" className="min-w-0 flex-1 text-sm font-semibold">
            Start agent
          </h2>
          <button type="button" onClick={onClose} aria-label="Close" title="Close">
            <X size={17} />
          </button>
        </header>
        <label className="mb-3 block text-xs">
          Assignment
          <textarea
            autoFocus
            value={assignment}
            onChange={(event) => setAssignment(event.target.value)}
            rows={4}
            className="mt-1 w-full rounded border bg-transparent p-2 text-sm outline-none focus:ring-1"
            style={{ borderColor: "var(--th-border)" }}
            placeholder="Describe the work this agent should do"
          />
        </label>
        <div className="mb-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
          Directory: <span className="font-mono">{directory}</span>
        </div>
        <div className="mb-3 text-xs" style={{ color: "var(--th-fg-muted)" }}>
          Source baseline:{" "}
          <span className="font-mono">
            {checkout?.headCommit?.slice(0, 12) ?? (baselineError ? "Unavailable" : "Resolving...")}
          </span>
        </div>
        {baselineError && (
          <p role="alert" className="mb-3 text-xs text-red-400">
            {baselineError}
          </p>
        )}
        <fieldset className="mb-4 rounded border p-3" style={{ borderColor: "var(--th-border)" }}>
          <legend className="px-1 text-xs font-semibold">Lane ownership</legend>
          <label className="mb-3 block text-xs">
            Lane ID
            <input
              value={laneId}
              onChange={(event) => setLaneId(event.target.value)}
              aria-label="Lane ID"
              aria-describedby="lane-id-help"
              className="mt-1 h-9 w-full rounded border bg-transparent px-2 font-mono text-xs outline-none focus:ring-1"
              style={{ borderColor: "var(--th-border)" }}
            />
            <span id="lane-id-help" className="mt-1 block" style={{ color: "var(--th-fg-muted)" }}>
              Stable identity used by dependencies and collision checks.
            </span>
          </label>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="block text-xs">
              Dependencies
              <textarea
                value={dependencies}
                onChange={(event) => setDependencies(event.target.value)}
                aria-label="Dependencies"
                aria-describedby="dependencies-help"
                rows={3}
                className="mt-1 w-full rounded border bg-transparent p-2 font-mono text-xs outline-none focus:ring-1"
                style={{ borderColor: "var(--th-border)" }}
                placeholder="lane.backend"
              />
              <span id="dependencies-help" className="mt-1 block" style={{ color: "var(--th-fg-muted)" }}>
                Completed lane IDs, one per line. Leave empty only when independent.
              </span>
            </label>
            <label className="block text-xs">
              Mutable files or directories
              <textarea
                value={mutableFiles}
                onChange={(event) => setMutableFiles(event.target.value)}
                aria-label="Mutable files or directories"
                aria-describedby="mutable-files-help"
                rows={3}
                className="mt-1 w-full rounded border bg-transparent p-2 font-mono text-xs outline-none focus:ring-1"
                style={{ borderColor: "var(--th-border)" }}
                placeholder="apps/desktop/src"
              />
              <span id="mutable-files-help" className="mt-1 block" style={{ color: "var(--th-fg-muted)" }}>
                Repository-relative paths or directory prefixes, one per line. Globs are not accepted.
              </span>
            </label>
            <label className="block text-xs">
              Mutable schemas
              <textarea
                value={mutableSchemas}
                onChange={(event) => setMutableSchemas(event.target.value)}
                aria-label="Mutable schemas"
                rows={2}
                className="mt-1 w-full rounded border bg-transparent p-2 font-mono text-xs outline-none focus:ring-1"
                style={{ borderColor: "var(--th-border)" }}
                placeholder="captains-v18"
              />
            </label>
            <label className="block text-xs">
              Mutable interfaces
              <textarea
                value={mutableInterfaces}
                onChange={(event) => setMutableInterfaces(event.target.value)}
                aria-label="Mutable interfaces"
                rows={2}
                className="mt-1 w-full rounded border bg-transparent p-2 font-mono text-xs outline-none focus:ring-1"
                style={{ borderColor: "var(--th-border)" }}
                placeholder="control.dispatch"
              />
            </label>
          </div>
          <label className="mt-3 block text-xs">
            Integration contracts
            <textarea
              value={integrationContracts}
              onChange={(event) => setIntegrationContracts(event.target.value)}
              aria-label="Integration contracts"
              aria-describedby="integration-contracts-help"
              rows={2}
              className="mt-1 w-full rounded border bg-transparent p-2 font-mono text-xs outline-none focus:ring-1"
              style={{ borderColor: "var(--th-border)" }}
              placeholder="contract-id | integration-owner | lane.backend, lane.frontend"
            />
            <span
              id="integration-contracts-help"
              className="mt-1 block"
              style={{ color: "var(--th-fg-muted)" }}
            >
              Optional. One ordered contract per line using the format shown above.
            </span>
          </label>
        </fieldset>
        <label className="mb-4 block text-xs">
          Harness
          <select
            value={harness}
            onChange={(event) => setHarness(event.target.value as "codex" | "claude")}
            className="mt-1 h-9 w-full rounded border bg-transparent px-2 text-sm outline-none"
            style={{ borderColor: "var(--th-border)" }}
          >
            <option value="codex">Codex</option>
            <option value="claude">Claude</option>
          </select>
        </label>
        <label className="mb-4 flex items-start gap-2 text-xs">
          <input
            type="checkbox"
            checked={visibleProductBug}
            onChange={(event) => setVisibleProductBug(event.target.checked)}
            className="mt-0.5"
          />
          <span>
            Visible product bug
            <span className="mt-0.5 block" style={{ color: "var(--th-fg-muted)" }}>
              Requires packaged GUI end-to-end acceptance evidence before completion.
            </span>
          </span>
        </label>
        {error && (
          <p role="alert" className="mb-3 text-xs text-red-400">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <button type="button" onClick={onClose} disabled={busy} className="rounded px-3 py-2 text-xs">
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void submit()}
            disabled={busy || !checkout?.headCommit || baselineError !== null}
            className="rounded px-3 py-2 text-xs font-semibold disabled:opacity-50"
            style={{ background: "var(--th-accent)", color: "var(--th-accent-fg, var(--th-fg))" }}
          >
            {busy ? "Starting..." : "Start agent"}
          </button>
        </div>
      </div>
    </div>
  );
}
