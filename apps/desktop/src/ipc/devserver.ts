// Typed wrappers over the Dev-server IPC surface (feat/dev-runner).
//
// Run and Preview runs one managed package script per project, scoped to that
// project's directory. These wrappers invoke the discovery and lifecycle commands and
// `listen` on the per-terminal output channel. Kept separate from ./client (0.1
// nucleus) and ./files so the dev-runner contract lives in one place. Mirrors
// `src-tauri/src/devserver.rs` (its `DevServerEvent` uses `rename_all =
// "camelCase"`); keep this in lockstep with that file.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { TerminalId } from "./types";

/** Tauri command names for the managed dev runner (used with `invoke`). */
export const CommandsDevServer = {
  /** Discover typed root-package targets for a project. */
  discoverRunTargets: "discover_run_targets",
  /** Start (or restart) the selected typed target. */
  startDevServer: "start_dev_server",
  /** Stop one exact run, or the active run when omitted. */
  stopDevServer: "stop_dev_server",
  /** Read backend-authoritative lifecycle state. */
  devServerSnapshot: "dev_server_snapshot",
  /** Host to substitute for a `localhost` preview URL (WSL2 fix). → string|null */
  previewHost: "preview_host",
  /** TCP-reachability probe for a host:port (precise preview errors). → bool */
  probeTcp: "probe_tcp",
} as const;

export type PackageManager = "pnpm" | "npm" | "yarn" | "bun";

export interface RunTarget {
  kind: "packageScript";
  id: string;
  script: string;
  label: string;
  packageManager: PackageManager;
  commandDisplay: string;
  recommended: boolean;
}

export interface PackageScriptTargetRef {
  kind: "packageScript";
  script: string;
}

export interface RunTargetDiscovery {
  state: "ready" | "notFound" | "unreadable" | "invalid";
  targets: RunTarget[];
  message: string | null;
}

export type RunnerState =
  | "idle"
  | "starting"
  | "running"
  | "stopping"
  | "exited"
  | "failed";

export interface DevServerSnapshot {
  terminalId: TerminalId;
  runId: string | null;
  revision: number;
  state: RunnerState;
  target: RunTarget | null;
  exitCode: number | null;
  reason: string | null;
  observedAt: number;
}

/**
 * One event from a managed dev server, streamed on `devserver://<terminalId>`.
 * Mirrors `DevServerEvent` in `src-tauri/src/devserver.rs`.
 */
export interface DevServerEvent {
  /** The terminal/project id this event belongs to. */
  id: TerminalId;
  /** Generation that owns this event. */
  runId: string;
  /** Monotonic backend revision for stale-event rejection. */
  revision: number;
  /**
   * `"line"` — a captured stdout/stderr output line (in `line`).
   * `"started"` means the child process spawned and the runner becomes active.
   * `"exited"` — the child ended on its own; `line` is a human-readable summary.
   */
  kind: "line" | "started" | "exited";
  /** The output line, or a lifecycle summary, with no trailing newline. */
  line: string;
}

export function discoverRunTargets(cwd: string): Promise<RunTargetDiscovery> {
  return invoke(CommandsDevServer.discoverRunTargets, { cwd });
}

export function devServerSnapshot(
  terminalId: TerminalId,
): Promise<DevServerSnapshot> {
  return invoke(CommandsDevServer.devServerSnapshot, { terminalId });
}

/**
 * Build the per-terminal dev-server event channel name. The backend emits on
 * exactly this string (`devserver://<id>`); kept here so the frontend never
 * hard-codes the format in two places.
 */
export function devServerChannel(terminalId: TerminalId): string {
  return `devserver://${terminalId}`;
}

/**
 * Start (or restart) the selected typed target inside `cwd`. The backend validates
 * the target again and replaces any active run for this terminal. Output arrives
 * via {@link onDevServerEvent}.
 */
export function startDevServer(
  terminalId: TerminalId,
  cwd: string,
  target: PackageScriptTargetRef,
): Promise<DevServerSnapshot> {
  return invoke(CommandsDevServer.startDevServer, { terminalId, cwd, target });
}

/** Stop the managed dev server for `terminalId` without touching a replacement. */
export function stopDevServer(
  terminalId: TerminalId,
  runId?: string | null,
): Promise<DevServerSnapshot> {
  return invoke(CommandsDevServer.stopDevServer, { terminalId, runId });
}

/**
 * Subscribe to a terminal's dev-server output/lifecycle events. Returns a promise
 * resolving to an unlisten fn; call it on unmount to tear the listener down.
 *
 * Unlike the multiplexed terminal-output hub (one app-wide listener fanned out in
 * ./client), each managed runner uses its own channel (`devserver://<id>`), so a
 * plain per-terminal `listen` is the right shape.
 */
export function onDevServerEvent(
  terminalId: TerminalId,
  cb: (e: DevServerEvent) => void,
): Promise<UnlistenFn> {
  return listen<DevServerEvent>(devServerChannel(terminalId), (ev) =>
    cb(ev.payload),
  );
}

// ---------------------------------------------------------------------------
// Preview reachability (the WSL2 localhost fix).
//
// The dev server runs INSIDE WSL; the preview iframe is a WINDOWS process. A
// `localhost`/`127.0.0.1` URL from the server's banner may reach WSL directly in
// mirrored mode or through WSL's NAT relay. Probe that route first. Only when it
// is unreachable does `preview_host` provide the WSL interface IP to substitute.
// `probe_tcp` runs on the Windows side, matching the WebView's network boundary.
// ---------------------------------------------------------------------------

/** Hosts that name a loopback the WSL-side server may have bound — these are the
 *  ones we rewrite to a Windows-reachable host. */
const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "::1"]);

/**
 * The host to substitute for a loopback in a preview URL, resolved once and
 * cached (the value is stable for a WSL session and the lookup spawns a process
 * backend-side). Resolves to `null` on unix / when no rewrite is needed, or when
 * the backend isn't present (plain browser dev) — callers then keep the URL.
 */
let previewHostPromise: Promise<string | null> | null = null;
export function previewHost(): Promise<string | null> {
  if (!previewHostPromise) {
    previewHostPromise = (async () => {
      try {
        return (await invoke<string | null>(CommandsDevServer.previewHost)) ?? null;
      } catch {
        // No Tauri backend (plain `vite`) or the command is missing: no rewrite.
        return null;
      }
    })();
  }
  return previewHostPromise;
}

/**
 * Resolve a `localhost`/`127.0.0.1`/`0.0.0.0` URL for the Windows-side preview.
 * An already-reachable loopback URL passes through unchanged. Otherwise the WSL
 * interface host from {@link previewHost} is substituted. Non-loopback hosts and
 * parse failures pass through unchanged. The port/path/query are preserved.
 */
export async function reachablePreviewUrl(raw: string): Promise<string> {
  if (!raw) return raw;
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return raw; // not a full URL (caller normalizes first); leave it be
  }
  if (!LOOPBACK_HOSTS.has(u.hostname.toLowerCase())) return raw;
  if ((await probePreviewReachable(raw, 500)) === true) return raw;
  const host = await previewHost();
  if (!host) return raw; // unix / no backend / lookup failed — localhost is fine
  u.hostname = host;
  return u.toString();
}

/**
 * Probe whether `url`'s host:port accepts a TCP connection (the connection the
 * iframe would make). Returns true if reachable, false if refused/timed out, and
 * null if we can't tell (bad URL, or no backend to probe with). `timeoutMs`
 * defaults to a snappy 1.5s.
 */
export async function probePreviewReachable(
  url: string,
  timeoutMs = 1500,
): Promise<boolean | null> {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return null;
  }
  const port = u.port ? Number(u.port) : u.protocol === "https:" ? 443 : 80;
  if (!Number.isFinite(port) || port <= 0 || port > 65535) return null;
  try {
    return await invoke<boolean>(CommandsDevServer.probeTcp, {
      host: u.hostname,
      port,
      timeoutMs,
    });
  } catch {
    return null; // no backend / command missing — can't probe
  }
}
