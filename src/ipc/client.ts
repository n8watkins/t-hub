// Typed wrappers over the Tauri IPC surface. Frontend modules should import
// from here rather than calling `invoke`/`listen` directly, so the command and
// event contract lives in exactly one place (./types).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  Commands,
  Events,
  type ExitEvent,
  type OutputEvent,
  type SpawnOptions,
  type StateEvent,
  type TerminalId,
  type TerminalInfo,
} from "./types";

export function spawnTerminal(opts: SpawnOptions = {}): Promise<TerminalInfo> {
  return invoke(Commands.spawnTerminal, { opts });
}

/** (Re)attach to a terminal; resolves to base64 scrollback to seed xterm. */
export function attachTerminal(
  id: TerminalId,
  cols: number,
  rows: number,
): Promise<string> {
  return invoke(Commands.attachTerminal, { id, cols, rows });
}

export function writeTerminal(id: TerminalId, data: string): Promise<void> {
  return invoke(Commands.writeTerminal, { id, data });
}

export function resizeTerminal(
  id: TerminalId,
  cols: number,
  rows: number,
): Promise<void> {
  return invoke(Commands.resizeTerminal, { id, cols, rows });
}

export function closeTerminal(id: TerminalId): Promise<void> {
  return invoke(Commands.closeTerminal, { id });
}

export function killTerminal(id: TerminalId): Promise<void> {
  return invoke(Commands.killTerminal, { id });
}

export function listTerminals(): Promise<TerminalInfo[]> {
  return invoke(Commands.listTerminals);
}

export function onOutput(cb: (e: OutputEvent) => void): Promise<UnlistenFn> {
  return listen<OutputEvent>(Events.output, (ev) => cb(ev.payload));
}

export function onState(cb: (e: StateEvent) => void): Promise<UnlistenFn> {
  return listen<StateEvent>(Events.state, (ev) => cb(ev.payload));
}

export function onExit(cb: (e: ExitEvent) => void): Promise<UnlistenFn> {
  return listen<ExitEvent>(Events.exit, (ev) => cb(ev.payload));
}

/** Decode base64 PTY output into bytes suitable for `xterm.write(Uint8Array)`. */
export function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
