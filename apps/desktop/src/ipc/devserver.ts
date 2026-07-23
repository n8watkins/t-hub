// Thin compatibility wrappers over the shared Preview control service.
//
// Preview runs one managed typed target per durable Project or Fleet Workspace.
// Lifecycle calls use the same control adapter as CLI and MCP. The direct Tauri
// commands below are reachability-only helpers for the existing webview.

import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { controlRequest } from "./controlClient";
import type { TerminalId } from "./types";

/** Direct Tauri command names retained only for webview reachability. */
export const CommandsDevServer = {
  /** Host to substitute for a `localhost` preview URL (WSL2 fix). → string|null */
  previewHost: "preview_host",
  /** TCP-reachability probe for a host:port (precise preview errors). → bool */
  probeTcp: "probe_tcp",
} as const;

export type PackageManager = "pnpm" | "npm" | "yarn" | "bun";

export interface PackageScriptRunTarget {
  kind: "packageScript";
  id: string;
  script: string;
  label: string;
  packageManager: PackageManager;
  commandDisplay: string;
  recommended: boolean;
  discoveryFingerprint: string;
}

export interface StaticSiteRunTarget {
  kind: "staticSite";
  id: string;
  entrypoint: string;
  relativeRoot: string;
  label: string;
  commandDisplay: string;
  recommended: boolean;
  discoveryFingerprint: string;
}

export type RunTarget = PackageScriptRunTarget | StaticSiteRunTarget;

export interface PackageScriptTargetRef {
  kind: "packageScript";
  id: string;
  script?: string;
}

export interface StaticSiteTargetRef {
  kind: "staticSite";
  id: string;
}

export type RunTargetRef = PackageScriptTargetRef | StaticSiteTargetRef;

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
  previewUrl: string | null;
  output: string[];
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

interface PreviewScope {
  projectId: string;
  workspaceId?: string;
}

interface PreviewContext {
  rootPath: string;
  scope: PreviewScope;
}

interface PreviewTarget {
  id: string;
  label: string;
  kind:
    | { type: "packageScript"; packageManager: PackageManager; script: string }
    | { type: "staticSite"; entrypoint: string };
  relativeRoot: string;
  recommended: boolean;
}

interface PreviewDiscovery {
  discoveryFingerprint: string;
  targets: PreviewTarget[];
}

interface PreviewStatus {
  state: "starting" | "running" | "unreachable" | "stale" | "failed" | "stopped";
  targetId?: string;
  runId?: string;
  previewUrl?: string;
  reason?: string;
  output?: string[];
  observedAtMs: number;
}

interface CaptainsSnapshot {
  projects?: Array<{ projectId: string; rootPath?: string; repoRoot: string }>;
  workspaces?: Array<{
    id: string;
    kind: "captain" | "work";
    owner?: { projectId: string };
    tileIds: string[];
  }>;
  captains?: Array<{
    terminalId?: string;
    projectId?: string;
    crew?: Array<{ terminalId: string }>;
  }>;
}

const targets = new Map<TerminalId, RunTarget[]>();

async function previewContext(terminalId: TerminalId): Promise<PreviewContext> {
  const snapshot = (await controlRequest("list_captains")) as CaptainsSnapshot;
  const workspace = snapshot.workspaces?.find((candidate) =>
    candidate.tileIds.includes(terminalId),
  );
  const captain = snapshot.captains?.find(
    (candidate) =>
      candidate.terminalId === terminalId ||
      candidate.crew?.some((crew) => crew.terminalId === terminalId),
  );
  const projectId = workspace?.owner?.projectId ?? captain?.projectId;
  if (!projectId) {
    throw new Error("Preview requires a terminal owned by a registered Project");
  }
  const project = snapshot.projects?.find(
    (candidate) => candidate.projectId === projectId,
  );
  if (!project) {
    throw new Error(`Preview Project '${projectId}' is no longer registered`);
  }
  return {
    rootPath: project.rootPath ?? project.repoRoot,
    scope: {
      projectId,
      ...(workspace?.kind === "work" && workspace.owner?.projectId === projectId
        ? { workspaceId: workspace.id }
        : {}),
    },
  };
}

function legacyTarget(target: PreviewTarget, fingerprint: string): RunTarget {
  if (target.kind.type === "packageScript") {
    return {
      kind: "packageScript",
      id: target.id,
      script: target.kind.script,
      label: target.label,
      packageManager: target.kind.packageManager,
      commandDisplay: `${target.kind.packageManager} run ${target.kind.script}`,
      recommended: target.recommended,
      discoveryFingerprint: fingerprint,
    };
  }
  return {
    kind: "staticSite",
    id: target.id,
    entrypoint: target.kind.entrypoint,
    relativeRoot: target.relativeRoot || ".",
    label: target.label,
    commandDisplay: "Static site",
    recommended: target.recommended,
    discoveryFingerprint: fingerprint,
  };
}

export async function discoverRunTargets(
  terminalId: TerminalId,
  _cwd: string,
): Promise<RunTargetDiscovery> {
  const context = await previewContext(terminalId);
  const discovery = (await controlRequest("preview_discover", {
    rootPath: context.rootPath,
  })) as PreviewDiscovery;
  const discovered = discovery.targets.map((target) =>
    legacyTarget(target, discovery.discoveryFingerprint),
  );
  targets.set(terminalId, discovered);
  return {
    state: discovered.length > 0 ? "ready" : "notFound",
    targets: discovered,
    message: discovered.length > 0 ? null : "No managed Preview targets found",
  };
}

export async function devServerSnapshot(
  terminalId: TerminalId,
): Promise<DevServerSnapshot> {
  const context = await previewContext(terminalId);
  const status = (await controlRequest("preview_status", {
    scope: context.scope,
  })) as PreviewStatus;
  return legacySnapshot(terminalId, status);
}

export async function selectPreviewTarget(
  terminalId: TerminalId,
  targetId: string,
): Promise<DevServerSnapshot> {
  const context = await previewContext(terminalId);
  const target = targets
    .get(terminalId)
    ?.find((candidate) => candidate.id === targetId);
  if (!target) throw new Error("Selected Preview target is stale");
  const result = (await controlRequest("preview_select", {
    rootPath: context.rootPath,
    target: {
      scope: context.scope,
      targetId: target.id,
      discoveryFingerprint: target.discoveryFingerprint,
    },
    requestId: requestId(),
  })) as { status: PreviewStatus };
  return legacySnapshot(terminalId, result.status);
}

export async function refreshPreview(
  terminalId: TerminalId,
): Promise<DevServerSnapshot> {
  return scopedMutation(terminalId, "preview_refresh");
}

export async function openPreview(
  terminalId: TerminalId,
): Promise<DevServerSnapshot> {
  return scopedMutation(terminalId, "preview_open");
}

export async function restartPreview(
  terminalId: TerminalId,
): Promise<DevServerSnapshot> {
  const context = await previewContext(terminalId);
  const result = (await controlRequest("preview_restart", {
    rootPath: context.rootPath,
    scope: context.scope,
    requestId: requestId(),
  })) as { status: PreviewStatus };
  return legacySnapshot(terminalId, result.status);
}

async function scopedMutation(
  terminalId: TerminalId,
  command: "preview_refresh" | "preview_open",
): Promise<DevServerSnapshot> {
  const context = await previewContext(terminalId);
  const result = (await controlRequest(command, {
    scope: context.scope,
    requestId: requestId(),
  })) as { status: PreviewStatus };
  return legacySnapshot(terminalId, result.status);
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
  target: RunTargetRef,
): Promise<DevServerSnapshot> {
  return startCanonical(terminalId, cwd, target);
}

async function startCanonical(
  terminalId: TerminalId,
  _cwd: string,
  target: RunTargetRef,
): Promise<DevServerSnapshot> {
  const context = await previewContext(terminalId);
  const selected = targets
    .get(terminalId)
    ?.find((candidate) =>
      target.kind === "packageScript"
        ? candidate.kind === target.kind && candidate.id === target.id
        : candidate.kind === target.kind && candidate.id === target.id,
    );
  if (!selected) throw new Error("Selected Preview target is stale");
  const result = (await controlRequest("preview_start", {
    rootPath: context.rootPath,
    scope: context.scope,
    target: {
      scope: context.scope,
      targetId: selected.id,
      discoveryFingerprint: selected.discoveryFingerprint,
    },
    requestId: requestId(),
  })) as { status: PreviewStatus };
  return legacySnapshot(terminalId, result.status);
}

/** Stop the managed dev server for `terminalId` without touching a replacement. */
export async function stopDevServer(
  terminalId: TerminalId,
  runId?: string | null,
): Promise<DevServerSnapshot> {
  const context = await previewContext(terminalId);
  const result = (await controlRequest("preview_stop", {
    scope: context.scope,
    ...(runId ? { expectedRunId: runId } : {}),
    requestId: requestId(),
  })) as { status: PreviewStatus };
  return legacySnapshot(terminalId, result.status);
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
  _terminalId: TerminalId,
  _cb: (e: DevServerEvent) => void,
): Promise<UnlistenFn> {
  return Promise.resolve(() => {});
}

function requestId(): string {
  return (
    globalThis.crypto?.randomUUID?.() ??
    `preview-${Date.now()}-${Math.random().toString(16).slice(2)}`
  );
}

function legacySnapshot(
  terminalId: TerminalId,
  status: PreviewStatus,
): DevServerSnapshot {
  const target =
    targets.get(terminalId)?.find((candidate) => candidate.id === status.targetId) ??
    null;
  const state: RunnerState =
    status.state === "stopped"
      ? "idle"
      : status.state === "unreachable" || status.state === "stale"
        ? "failed"
        : status.state;
  return {
    terminalId,
    runId: status.runId ?? null,
    revision: status.observedAtMs,
    state,
    target,
    exitCode: null,
    reason: status.reason ?? null,
    previewUrl: status.previewUrl ?? null,
    output: (status.output ?? []).slice(-2000),
    observedAt: status.observedAtMs,
  };
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
 * The host to substitute for a loopback in a preview URL.
 * Managed Preview lifecycle responses already contain the backend-authoritative
 * reachable URL.
 * This compatibility lookup remains fresh for manually entered loopback URLs so
 * a WSL restart cannot leave the webview pinned to an obsolete interface address.
 */
export async function previewHost(): Promise<string | null> {
  try {
    return (await invoke<string | null>(CommandsDevServer.previewHost)) ?? null;
  } catch {
    // No Tauri backend (plain `vite`) or the command is missing: no rewrite.
    return null;
  }
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
