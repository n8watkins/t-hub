// Typed wrappers over the Dev-server IPC surface (feat/dev-runner).
//
// The Dev tab runs ONE managed `npm run dev`-style process per project, scoped to
// that project's directory. These wrappers `invoke` the two Tauri commands and
// `listen` on the per-terminal output channel. Kept separate from ./client (0.1
// nucleus) and ./files so the dev-runner contract lives in one place. Mirrors
// `src-tauri/src/devserver.rs` (its `DevServerEvent` uses `rename_all =
// "camelCase"`); keep this in lockstep with that file.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { TerminalId } from "./types";

/** Tauri command names for the managed dev runner (used with `invoke`). */
export const CommandsDevServer = {
  /** Start (or restart) the dev server for a terminal/project. → void */
  startDevServer: "start_dev_server",
  /** Stop the dev server for a terminal/project (idempotent). → void */
  stopDevServer: "stop_dev_server",
} as const;

/**
 * One event from a managed dev server, streamed on `devserver://<terminalId>`.
 * Mirrors `DevServerEvent` in `src-tauri/src/devserver.rs`.
 */
export interface DevServerEvent {
  /** The terminal/project id this event belongs to. */
  id: TerminalId;
  /**
   * `"line"` — a captured stdout/stderr output line (in `line`).
   * `"started"` — the child process spawned (Dev tab flips to "running").
   * `"exited"` — the child ended on its own; `line` is a human-readable summary.
   */
  kind: "line" | "started" | "exited";
  /** The output line, or a lifecycle summary, with no trailing newline. */
  line: string;
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
 * Start (or restart) the managed dev server for `terminalId`, running `command`
 * inside `cwd`. Any dev server already running for this id is replaced. Output
 * arrives via {@link onDevServerEvent}.
 */
export function startDevServer(
  terminalId: TerminalId,
  cwd: string,
  command: string,
): Promise<void> {
  return invoke(CommandsDevServer.startDevServer, { terminalId, cwd, command });
}

/** Stop the managed dev server for `terminalId` (idempotent — safe if none). */
export function stopDevServer(terminalId: TerminalId): Promise<void> {
  return invoke(CommandsDevServer.stopDevServer, { terminalId });
}

/**
 * Subscribe to a terminal's dev-server output/lifecycle events. Returns a promise
 * resolving to an unlisten fn; call it on unmount to tear the listener down.
 *
 * Unlike the multiplexed terminal-output hub (one app-wide listener fanned out in
 * ./client), each Dev tab uses its OWN channel (`devserver://<id>`), so a plain
 * per-terminal `listen` is the right shape: there is exactly one Dev tab per id,
 * and the listener's lifetime matches that tab's mount.
 */
export function onDevServerEvent(
  terminalId: TerminalId,
  cb: (e: DevServerEvent) => void,
): Promise<UnlistenFn> {
  return listen<DevServerEvent>(devServerChannel(terminalId), (ev) =>
    cb(ev.payload),
  );
}
