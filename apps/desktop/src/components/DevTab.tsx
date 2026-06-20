// DevTab — the per-project "Dev" view: a managed `npm run dev` runner.
//
// OWNED by the Dev-runner agent (feat/dev-runner). The per-tile panel mounts
// <DevTab terminalId cwd/> and must not edit this file; keep these props stable.
//
// What it does:
//   - Runs the project's dev server, scoped to `cwd`, as a managed child process
//     in the backend (WSL on Windows, directly on unix). The command is editable
//     and defaults to the detected package manager (`pnpm dev` when a
//     pnpm-lock.yaml exists in cwd, else `npm run dev`).
//   - Streams the server's combined stdout+stderr into a scrolling output pane.
//   - Sniffs each line for the localhost URL the server prints
//     (`http://localhost:PORT` / `http://127.0.0.1:PORT`) and publishes it via
//     `usePanels.getState().setDevUrl(terminalId, url)` so the Preview tab loads
//     it automatically.
//   - Run/Stop button + a status row (idle / running / exited + detected URL).
//
// State is kept PER terminalId in a module-level store (below) so switching tabs
// or remounting the tile doesn't lose the log / running state, and so the backend
// process (which outlives a remount) stays in sync with what we render.

import { useCallback, useEffect, useRef, useState } from "react";
import { usePanels } from "../store/panels";
import {
  onDevServerEvent,
  startDevServer,
  stopDevServer,
  type DevServerEvent,
} from "../ipc/devserver";
import { listDir } from "../ipc/files";
import type { TerminalId } from "../ipc/types";
import { stripAnsi } from "../lib/ansi";

export interface DevTabProps {
  /** The project/terminal this dev runner belongs to. */
  terminalId: TerminalId;
  /** The project's working directory (where the dev server runs). */
  cwd: string;
}

/** Lifecycle status of the managed dev server for a tile. */
type RunStatus = "idle" | "running" | "exited";

/** How many output lines we retain per tile (a rolling window so a chatty dev
 *  server can't grow the log unbounded). */
const MAX_LINES = 2000;

/**
 * Detect the localhost URL a dev server prints. Matches `http://localhost:PORT`
 * and `http://127.0.0.1:PORT` (the two forms the task calls out), with an
 * optional path. Vite/Next/CRA all print one of these in their startup banner.
 * Returns the first match in `line`, or null.
 */
function detectUrl(line: string): string | null {
  // Strip ANSI first — dev servers colorize their startup banner, so the raw
  // line carries escape codes that would otherwise be captured into the URL.
  const m = stripAnsi(line).match(
    /https?:\/\/(?:localhost|127\.0\.0\.1)(?::\d+)?(?:\/[^\s)]*)?/i,
  );
  return m ? m[0] : null;
}

// ---------------------------------------------------------------------------
// Per-terminal state store.
//
// The backend dev-server process is keyed by terminalId and survives a DevTab
// unmount (e.g. switching to the Terminal tab and back). So the UI state that
// must track it — the log buffer, the run status, the editable command, the
// detected URL — also lives OUTSIDE the component, in this module-level map. The
// component subscribes to its slice and re-renders on change. This mirrors the
// "process outlives the view" model the rest of TermHub uses (tmux sessions).
// ---------------------------------------------------------------------------

interface DevState {
  status: RunStatus;
  /** Rolling output log (newest appended; capped at MAX_LINES). */
  lines: string[];
  /** The detected dev-server URL, mirrored into usePanels for the Preview tab. */
  url: string | null;
  /** The editable command. `undefined` until we've resolved the default. */
  command: string | undefined;
  /** Whether we're currently subscribed to this terminal's backend channel. */
  subscribed: boolean;
}

function freshState(): DevState {
  return {
    status: "idle",
    lines: [],
    url: null,
    command: undefined,
    subscribed: false,
  };
}

const states = new Map<TerminalId, DevState>();
/** Per-terminal subscriber callbacks (the mounted DevTab's re-render trigger). */
const listeners = new Map<TerminalId, Set<() => void>>();

function getState(id: TerminalId): DevState {
  let s = states.get(id);
  if (!s) {
    s = freshState();
    states.set(id, s);
  }
  return s;
}

/** Mutate a terminal's state and notify its subscribers to re-render. */
function update(id: TerminalId, patch: Partial<DevState>): void {
  const next = { ...getState(id), ...patch };
  states.set(id, next);
  const subs = listeners.get(id);
  if (subs) for (const cb of [...subs]) cb();
}

function appendLine(id: TerminalId, line: string): void {
  const s = getState(id);
  const lines = s.lines.length >= MAX_LINES ? s.lines.slice(-(MAX_LINES - 1)) : s.lines.slice();
  lines.push(line);
  update(id, { lines });
}

function subscribe(id: TerminalId, cb: () => void): () => void {
  let subs = listeners.get(id);
  if (!subs) {
    subs = new Set();
    listeners.set(id, subs);
  }
  subs.add(cb);
  return () => {
    subs?.delete(cb);
  };
}

// ---------------------------------------------------------------------------
// Default-command detection: prefer pnpm when a pnpm-lock.yaml is present in the
// project's cwd, else fall back to npm. Resolved once per terminal (cached on the
// state's `command` field, which the user can then freely edit).
// ---------------------------------------------------------------------------

async function resolveDefaultCommand(cwd: string): Promise<string> {
  if (!cwd) return "npm run dev";
  try {
    const entries = await listDir(cwd);
    const hasPnpm = entries.some((e) => e.name === "pnpm-lock.yaml");
    return hasPnpm ? "pnpm dev" : "npm run dev";
  } catch {
    // Listing failed (e.g. cwd not yet reachable) — a safe default the user can
    // edit. We don't block the UI on this.
    return "npm run dev";
  }
}

// ---------------------------------------------------------------------------
// The component.
// ---------------------------------------------------------------------------

export function DevTab({ terminalId, cwd }: DevTabProps) {
  // Force re-render when this terminal's slice changes.
  const [, force] = useState(0);
  useEffect(
    () => subscribe(terminalId, () => force((n) => n + 1)),
    [terminalId],
  );

  const state = getState(terminalId);
  const { status, lines, url, command } = state;

  // The editable command field. Mirrors `state.command` but is a controlled
  // input; we seed it from the resolved default the first time.
  const [draft, setDraft] = useState<string>(command ?? "");
  const logRef = useRef<HTMLDivElement>(null);

  // Resolve the default command once per terminal (when not yet set). We don't
  // overwrite a command the user already edited.
  useEffect(() => {
    if (state.command !== undefined) {
      // Keep the local draft in sync if the stored command changed elsewhere.
      setDraft(state.command);
      return;
    }
    let cancelled = false;
    void resolveDefaultCommand(cwd).then((def) => {
      if (cancelled) return;
      // Don't clobber a value the user typed while we were resolving.
      if (getState(terminalId).command === undefined) {
        update(terminalId, { command: def });
        setDraft(def);
      }
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalId, cwd]);

  // Subscribe to the backend dev-server channel for this terminal. We attach the
  // listener ONCE per terminal id (tracked on the state's `subscribed` flag) so a
  // remount doesn't double-subscribe; the listener lives for the app session,
  // which is fine since there's exactly one Dev tab per terminal.
  useEffect(() => {
    if (state.subscribed) return;
    update(terminalId, { subscribed: true });
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void onDevServerEvent(terminalId, (e: DevServerEvent) => {
      handleEvent(terminalId, e);
    }).then((un) => {
      if (disposed) {
        un();
      } else {
        unlisten = un;
      }
    });
    return () => {
      // We intentionally do NOT unsubscribe on unmount: the process outlives the
      // view, and re-subscribing on every tab switch would risk missing lines in
      // the gap. The listener is torn down only if the tile is truly gone, which
      // the panel handles via usePanels.forget; here we just stop the pending
      // attach if it hasn't resolved yet.
      disposed = true;
      void unlisten; // keep the live listener; see comment above.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalId]);

  // Auto-scroll the log to the bottom as lines arrive (only if already near the
  // bottom, so a user scrolled up to read history isn't yanked back down).
  useEffect(() => {
    const el = logRef.current;
    if (!el) return;
    const nearBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const onRun = useCallback(() => {
    const cmd = (draft || command || "npm run dev").trim();
    if (!cmd) return;
    // Persist the command + reset the log for a fresh run; flip to running
    // optimistically (the backend also emits a "started" event).
    update(terminalId, {
      command: cmd,
      status: "running",
      lines: [],
      url: null,
    });
    // Clear any stale detected URL for the Preview tab until the new run prints one.
    usePanels.getState().setDevUrl(terminalId, null);
    void startDevServer(terminalId, cwd, cmd).catch((err) => {
      appendLine(terminalId, `[termhub] failed to start: ${String(err)}`);
      update(terminalId, { status: "exited" });
    });
  }, [draft, command, terminalId, cwd]);

  const onStop = useCallback(() => {
    void stopDevServer(terminalId).catch(() => {
      /* idempotent on the backend; nothing to surface */
    });
    update(terminalId, { status: "idle" });
    // Clear the published URL: the server is gone, so Preview must stop loading
    // it and the tile's busy-gate (devUrl present => "looks busy") must release.
    usePanels.getState().setDevUrl(terminalId, null);
  }, [terminalId]);

  const running = status === "running";

  return (
    <div
      className="flex h-full min-h-0 flex-col"
      style={{ color: "var(--th-fg)" }}
    >
      {/* Control row: command field + Run/Stop. */}
      <div
        className="flex shrink-0 items-center gap-2 border-b px-3 py-2"
        style={{ borderColor: "var(--th-border)" }}
      >
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !running) onRun();
          }}
          placeholder="npm run dev"
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          autoComplete="off"
          disabled={running}
          className="min-w-0 flex-1 px-2.5 py-1.5 font-mono text-sm focus:outline-none disabled:opacity-60"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
          onFocus={(e) => {
            e.currentTarget.style.borderColor = "var(--th-focus-ring)";
          }}
          onBlur={(e) => {
            e.currentTarget.style.borderColor = "var(--th-border)";
          }}
          title="The dev-server command to run in this project"
        />
        {running ? (
          <button
            type="button"
            onClick={onStop}
            className="shrink-0 px-3 py-1.5 text-sm font-medium"
            style={{
              borderRadius: "var(--th-radius)",
              border: "1px solid var(--th-border)",
              background: "var(--th-tile-bg)",
              color: "var(--th-fg)",
            }}
            title="Stop the dev server"
          >
            Stop
          </button>
        ) : (
          <button
            type="button"
            onClick={onRun}
            className="shrink-0 px-3 py-1.5 text-sm font-medium"
            style={{
              borderRadius: "var(--th-radius)",
              border: "1px solid var(--th-border)",
              background: "var(--th-accent, var(--th-tile-bg))",
              color: "var(--th-accent-fg, var(--th-fg))",
            }}
            title="Run the dev server"
          >
            Run
          </button>
        )}
      </div>

      {/* Status row: lifecycle + the detected URL (click to force-feed Preview). */}
      <div
        className="flex shrink-0 items-center gap-2 border-b px-3 py-1.5 text-xs"
        style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
      >
        <StatusDot status={status} />
        <span>
          {status === "running"
            ? "running"
            : status === "exited"
              ? "exited"
              : "idle"}
        </span>
        {url ? (
          <>
            <span aria-hidden>·</span>
            <button
              type="button"
              onClick={() => usePanels.getState().setDevUrl(terminalId, url)}
              className="min-w-0 truncate font-mono hover:underline"
              style={{ color: "var(--th-fg)" }}
              title={`Detected ${url} — feeding it to Preview`}
            >
              {url}
            </button>
          </>
        ) : running ? (
          <>
            <span aria-hidden>·</span>
            <span>waiting for a localhost URL…</span>
          </>
        ) : null}
      </div>

      {/* Output pane: the streamed dev-server log. */}
      <div
        ref={logRef}
        className="min-h-0 flex-1 overflow-auto px-3 py-2 font-mono text-xs leading-relaxed"
        style={{ background: "var(--th-tile-bg)" }}
      >
        {lines.length === 0 ? (
          <div style={{ color: "var(--th-fg-muted)" }}>
            {status === "running"
              ? "Starting…"
              : `Press Run to start the dev server in ${cwd || "this project"}.`}
          </div>
        ) : (
          lines.map((line, i) => (
            <div key={i} className="whitespace-pre-wrap break-words">
              {line || " "}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

/** A small colored status dot mirroring the terminal status convention. */
function StatusDot({ status }: { status: RunStatus }) {
  const color =
    status === "running"
      ? "var(--th-ok, #3fb950)"
      : status === "exited"
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

// ---------------------------------------------------------------------------
// Event handling (module-level so it runs regardless of which DevTab instance is
// mounted, and so URL detection updates usePanels even mid-remount).
// ---------------------------------------------------------------------------

function handleEvent(id: TerminalId, e: DevServerEvent): void {
  switch (e.kind) {
    case "started":
      update(id, { status: "running" });
      break;
    case "exited":
      if (e.line) appendLine(id, `[termhub] ${e.line}`);
      update(id, { status: "exited" });
      // The server died, so drop its URL: Preview stops loading a dead address
      // and the tile's busy-gate (devUrl present) releases.
      usePanels.getState().setDevUrl(id, null);
      break;
    case "line":
    default: {
      appendLine(id, e.line);
      // Sniff for the localhost URL and feed Preview the first time we see one
      // (or whenever it changes — a server can re-bind to a new port).
      const found = detectUrl(e.line);
      if (found && found !== getState(id).url) {
        update(id, { url: found });
        usePanels.getState().setDevUrl(id, found);
      }
      break;
    }
  }
}
