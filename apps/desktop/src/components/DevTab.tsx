// Managed-runner portion of the per-project Run and Preview surface.
//
// The backend discovers and validates typed package-script targets, constructs
// executable arguments, and owns lifecycle state. This component never accepts
// or sends arbitrary shell text.

import { useEffect, useRef, useState } from "react";
import { usePanels } from "../store/panels";
import {
  devServerSnapshot,
  discoverRunTargets,
  onDevServerEvent,
  startDevServer,
  stopDevServer,
  type DevServerEvent,
  type DevServerSnapshot,
  type RunTarget,
  type RunTargetDiscovery,
  type RunnerState,
} from "../ipc/devserver";
import type { TerminalId } from "../ipc/types";
import { stripAnsi } from "../lib/ansi";

export interface DevTabProps {
  terminalId: TerminalId;
  cwd: string;
}

const MAX_LINES = 2000;
const BUTTON_STYLE = {
  borderRadius: "var(--th-radius)",
  border: "1px solid var(--th-border)",
  background: "var(--th-tile-bg)",
  color: "var(--th-fg)",
} as const;
const ACCENT_BUTTON_STYLE = {
  ...BUTTON_STYLE,
  background: "var(--th-accent, var(--th-tile-bg))",
  color: "var(--th-accent-fg, var(--th-fg))",
} as const;

function detectUrl(line: string): string | null {
  const match = stripAnsi(line).match(
    /https?:\/\/(?:localhost|127\.0\.0\.1)(?::\d+)?(?:\/[^\s)]*)?/i,
  );
  return match ? match[0] : null;
}

type DiscoveryState = "loading" | RunTargetDiscovery["state"];

interface DevState {
  snapshot: DevServerSnapshot;
  lines: string[];
  url: string | null;
  targets: RunTarget[];
  discoveryState: DiscoveryState;
  discoveryMessage: string | null;
  selectedTargetId: string | null;
  subscribed: boolean;
}

function idleSnapshot(terminalId: TerminalId): DevServerSnapshot {
  return {
    terminalId,
    runId: null,
    revision: 0,
    state: "idle",
    target: null,
    exitCode: null,
    reason: null,
    observedAt: 0,
  };
}

function freshState(terminalId: TerminalId): DevState {
  return {
    snapshot: idleSnapshot(terminalId),
    lines: [],
    url: null,
    targets: [],
    discoveryState: "loading",
    discoveryMessage: null,
    selectedTargetId: null,
    subscribed: false,
  };
}

const states = new Map<TerminalId, DevState>();
const listeners = new Map<TerminalId, Set<() => void>>();
const unlisteners = new Map<TerminalId, () => void>();

function getState(id: TerminalId): DevState {
  let state = states.get(id);
  if (!state) {
    state = freshState(id);
    states.set(id, state);
  }
  return state;
}

function update(id: TerminalId, patch: Partial<DevState>): void {
  states.set(id, { ...getState(id), ...patch });
  const subscribers = listeners.get(id);
  if (subscribers) {
    for (const callback of [...subscribers]) callback();
  }
}

function appendLine(id: TerminalId, line: string): void {
  const current = getState(id).lines;
  const lines =
    current.length >= MAX_LINES
      ? current.slice(-(MAX_LINES - 1))
      : current.slice();
  lines.push(line);
  update(id, { lines });
}

function subscribe(id: TerminalId, callback: () => void): () => void {
  let subscribers = listeners.get(id);
  if (!subscribers) {
    subscribers = new Set();
    listeners.set(id, subscribers);
  }
  subscribers.add(callback);
  return () => subscribers?.delete(callback);
}

export function forgetDevState(id: TerminalId): void {
  const unlisten = unlisteners.get(id);
  if (unlisten) {
    try {
      unlisten();
    } catch {
      // The runtime may already have removed the listener.
    }
  }
  unlisteners.delete(id);
  states.delete(id);
  listeners.delete(id);
}

function chooseTarget(
  targets: RunTarget[],
  snapshot: DevServerSnapshot,
  previous: string | null,
): string | null {
  const candidates = [
    snapshot.target?.id,
    previous,
    targets.find((target) => target.recommended)?.id,
    targets[0]?.id,
  ];
  return (
    candidates.find(
      (candidate): candidate is string =>
        Boolean(candidate) && targets.some((target) => target.id === candidate),
    ) ?? null
  );
}

function applySnapshot(id: TerminalId, snapshot: DevServerSnapshot): void {
  const current = getState(id).snapshot;
  if (snapshot.revision < current.revision) return;
  update(id, { snapshot });
  if (snapshot.state !== "running") {
    usePanels.getState().setDevUrl(id, null);
  }
}

function handleEvent(id: TerminalId, event: DevServerEvent): void {
  const current = getState(id);
  if (
    event.kind === "started" &&
    current.snapshot.state === "starting" &&
    current.snapshot.runId === null &&
    event.revision > current.snapshot.revision
  ) {
    update(id, {
      snapshot: {
        ...current.snapshot,
        runId: event.runId,
        revision: event.revision,
        state: "running",
      },
    });
    return;
  }
  if (event.runId !== current.snapshot.runId) return;
  if (event.revision <= current.snapshot.revision) return;

  if (event.kind === "line") {
    update(id, {
      snapshot: { ...current.snapshot, revision: event.revision },
    });
    appendLine(id, event.line);
    const found = detectUrl(event.line);
    if (found && found !== getState(id).url) {
      update(id, { url: found });
      usePanels.getState().setDevUrl(id, found);
    }
    return;
  }

  if (event.kind === "exited") {
    if (event.line) appendLine(id, `[t-hub] ${event.line}`);
    void devServerSnapshot(id).then((snapshot) => applySnapshot(id, snapshot));
  }
}

export function DevTab({ terminalId, cwd }: DevTabProps) {
  const [, forceRender] = useState(0);
  const [discoveryNonce, setDiscoveryNonce] = useState(0);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(
    () => subscribe(terminalId, () => forceRender((value) => value + 1)),
    [terminalId],
  );

  const state = getState(terminalId);
  const { snapshot, lines, url } = state;

  useEffect(() => {
    if (getState(terminalId).subscribed) return;
    update(terminalId, { subscribed: true });
    let disposed = false;
    void onDevServerEvent(terminalId, (event) =>
      handleEvent(terminalId, event),
    ).then((unlisten) => {
      if (disposed) {
        unlisten();
        update(terminalId, { subscribed: false });
      } else unlisteners.set(terminalId, unlisten);
    });
    return () => {
      disposed = true;
    };
  }, [terminalId]);

  useEffect(() => {
    let cancelled = false;
    update(terminalId, {
      discoveryState: "loading",
      discoveryMessage: null,
    });
    void Promise.all([
      discoverRunTargets(cwd),
      devServerSnapshot(terminalId),
    ])
      .then(([discovery, authoritative]) => {
        if (cancelled) return;
        const current = getState(terminalId);
        update(terminalId, {
          snapshot: authoritative,
          targets: discovery.targets,
          discoveryState: discovery.state,
          discoveryMessage: discovery.message,
          selectedTargetId: chooseTarget(
            discovery.targets,
            authoritative,
            current.selectedTargetId,
          ),
        });
      })
      .catch((error) => {
        if (cancelled) return;
        update(terminalId, {
          discoveryState: "unreadable",
          discoveryMessage: String(error),
          targets: [],
          selectedTargetId: null,
        });
      });
    return () => {
      cancelled = true;
    };
  }, [terminalId, cwd, discoveryNonce]);

  useEffect(() => {
    const element = logRef.current;
    if (!element) return;
    const nearBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight < 80;
    if (nearBottom) element.scrollTop = element.scrollHeight;
  }, [lines]);

  const selectedTarget = state.targets.find(
    (target) => target.id === state.selectedTargetId,
  );
  const busy = ["starting", "running", "stopping"].includes(snapshot.state);

  const onRun = () => {
    const current = getState(terminalId);
    const target = current.targets.find(
      (candidate) => candidate.id === current.selectedTargetId,
    );
    if (!target) return;
    update(terminalId, {
      snapshot: {
        ...current.snapshot,
        runId: null,
        state: "starting",
        target,
        reason: null,
        exitCode: null,
      },
      lines: [],
      url: null,
    });
    usePanels.getState().setDevUrl(terminalId, null);
    void startDevServer(terminalId, cwd, {
      kind: "packageScript",
      script: target.script,
    })
      .then((authoritative) => applySnapshot(terminalId, authoritative))
      .catch((error) => {
        const failed = getState(terminalId).snapshot;
        appendLine(terminalId, `[t-hub] failed to start: ${String(error)}`);
        update(terminalId, {
          snapshot: {
            ...failed,
            state: "failed",
            reason: String(error),
          },
        });
      });
  };

  const onStop = () => {
    const current = getState(terminalId).snapshot;
    if (!current.runId) return;
    update(terminalId, {
      snapshot: { ...current, state: "stopping" },
    });
    void stopDevServer(terminalId, current.runId)
      .then((authoritative) => applySnapshot(terminalId, authoritative))
      .catch((error) => {
        appendLine(terminalId, `[t-hub] failed to stop: ${String(error)}`);
        void devServerSnapshot(terminalId).then((authoritative) =>
          applySnapshot(terminalId, authoritative),
        );
      });
  };

  return (
    <div
      className="flex h-full min-h-0 flex-col"
      style={{ color: "var(--th-fg)" }}
    >
      <div
        className="flex shrink-0 items-center gap-2 border-b px-3 py-2"
        style={{ borderColor: "var(--th-border)" }}
      >
        <select
          aria-label="Run target"
          value={state.selectedTargetId ?? ""}
          onChange={(event) =>
            update(terminalId, { selectedTargetId: event.target.value || null })
          }
          disabled={state.discoveryState !== "ready" || busy || state.targets.length === 0}
          className="min-w-0 flex-1 px-2.5 py-1.5 font-mono text-sm disabled:opacity-60"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
        >
          {state.discoveryState === "loading" ? (
            <option value="">Loading run targets...</option>
          ) : state.targets.length === 0 ? (
            <option value="">No package scripts found</option>
          ) : (
            state.targets.map((target) => (
              <option key={target.id} value={target.id}>
                {target.label} - {target.commandDisplay}
              </option>
            ))
          )}
        </select>
        {busy ? (
          <button
            type="button"
            onClick={onStop}
            disabled={!snapshot.runId || snapshot.state === "stopping"}
            className="shrink-0 px-3 py-1.5 text-sm font-medium disabled:opacity-60"
            style={BUTTON_STYLE}
            title="Stop the active run"
          >
            {snapshot.state === "stopping" ? "Stopping..." : "Stop"}
          </button>
        ) : (
          <button
            type="button"
            onClick={onRun}
            disabled={!selectedTarget}
            className="shrink-0 px-3 py-1.5 text-sm font-medium disabled:opacity-60"
            style={ACCENT_BUTTON_STYLE}
            title="Run the selected package script"
          >
            Run
          </button>
        )}
      </div>

      {state.discoveryState !== "loading" &&
      state.discoveryState !== "ready" ? (
        <div
          role="alert"
          className="flex shrink-0 items-center justify-between gap-3 border-b px-3 py-2 text-xs"
          style={{ borderColor: "var(--th-border)", color: "var(--th-error)" }}
        >
          <span>{state.discoveryMessage ?? "Run targets are unavailable."}</span>
          <button
            type="button"
            onClick={() => setDiscoveryNonce((value) => value + 1)}
            className="shrink-0 underline"
          >
            Retry
          </button>
        </div>
      ) : null}

      <div
        className="flex shrink-0 items-center gap-2 border-b px-3 py-1.5 text-xs"
        style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
      >
        <StatusDot status={snapshot.state} />
        <span>{snapshot.state}</span>
        {selectedTarget ? (
          <>
            <span aria-hidden>·</span>
            <code>{selectedTarget.commandDisplay}</code>
          </>
        ) : null}
        {url ? (
          <>
            <span aria-hidden>·</span>
            <button
              type="button"
              onClick={() => usePanels.getState().setDevUrl(terminalId, url)}
              className="min-w-0 truncate font-mono hover:underline"
              style={{ color: "var(--th-fg)" }}
              title={`Detected ${url}`}
            >
              {url}
            </button>
          </>
        ) : snapshot.state === "running" ? (
          <>
            <span aria-hidden>·</span>
            <span>waiting for a localhost URL...</span>
          </>
        ) : null}
        {snapshot.reason ? (
          <span role="status" className="min-w-0 truncate" title={snapshot.reason}>
            {snapshot.reason}
          </span>
        ) : null}
      </div>

      <div
        ref={logRef}
        className="min-h-0 flex-1 overflow-auto px-3 py-2 font-mono text-xs leading-relaxed"
        style={{ background: "var(--th-tile-bg)" }}
      >
        {lines.length === 0 ? (
          <div style={{ color: "var(--th-fg-muted)" }}>
            {snapshot.state === "starting"
              ? "Starting..."
              : state.discoveryState === "ready" && state.targets.length === 0
                ? "No root package scripts are available."
                : `Select a package script to run in ${cwd || "this project"}.`}
          </div>
        ) : (
          lines.map((line, index) => (
            <div key={index} className="whitespace-pre-wrap break-words">
              {line || " "}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

function StatusDot({ status }: { status: RunnerState }) {
  const color =
    status === "running"
      ? "var(--th-ok, #3fb950)"
      : status === "failed" || status === "exited"
        ? "var(--th-error, #f85149)"
        : "var(--th-fg-muted)";
  return (
    <span
      aria-hidden
      className="inline-block h-2 w-2 shrink-0 rounded-full"
      style={{ background: color }}
    />
  );
}
