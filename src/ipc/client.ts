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

// ---------------------------------------------------------------------------
// Shared, multiplexed terminal-event subscription (BUG 1: startup-freeze race).
//
// Previously every <TerminalView> registered its OWN broadcast listener for
// `terminal://output` (and state/exit). With ~16 terminals that meant 16 live
// Tauri listeners, each receiving EVERY terminal's output and filtering by id.
// On a cold relaunch all 16 mount and tear down listeners in a churn while the
// backend reader threads are already flooding OUTPUT events -- so emits landed
// on callback ids that the page had not yet registered or had just dropped,
// producing the thousands of "[TAURI] Couldn't find callback id N" warnings and
// a frozen, blank grid until a manual reload (by which point Rust was idle).
//
// The fix: register EXACTLY ONE backing Tauri listener per channel for the whole
// app, lazily and idempotently, and fan out to per-subscriber callbacks here in
// JS. A single listener means a single callback id per channel -- the orphan
// surface that the backend can emit into shrinks from O(N terminals) to O(1),
// and the listener is never torn down for the life of the page, so the backend
// never streams into a void during the mount churn. Per-terminal subscribe is
// now a synchronous map insert (still surfaced through a Promise<UnlistenFn> so
// callers are unchanged), and unsubscribe is a synchronous map delete.
// ---------------------------------------------------------------------------

/** A per-channel fan-out hub: one real Tauri listener, many JS subscribers. */
class EventHub<T> {
  private readonly subs = new Set<(e: T) => void>();
  private backing: Promise<UnlistenFn> | null = null;

  constructor(private readonly event: string) {}

  /** Ensure the single backing Tauri listener is registered (idempotent). */
  private ensureBacking(): void {
    if (this.backing) return;
    // One listener for the whole app; it dispatches to every subscriber. We
    // snapshot the subscriber set per event so a subscribe/unsubscribe during
    // dispatch can't disturb the in-progress iteration.
    this.backing = listen<T>(this.event, (ev) => {
      if (this.subs.size === 0) return;
      for (const cb of [...this.subs]) {
        try {
          cb(ev.payload);
        } catch {
          // A throwing subscriber must not starve the others on this event.
        }
      }
    });
  }

  /** Register a subscriber; returns an unsubscribe fn (synchronous removal). */
  subscribe(cb: (e: T) => void): UnlistenFn {
    this.subs.add(cb);
    this.ensureBacking();
    return () => {
      this.subs.delete(cb);
    };
  }
}

const outputHub = new EventHub<OutputEvent>(Events.output);
const stateHub = new EventHub<StateEvent>(Events.state);
const exitHub = new EventHub<ExitEvent>(Events.exit);

export function onOutput(cb: (e: OutputEvent) => void): Promise<UnlistenFn> {
  return Promise.resolve(outputHub.subscribe(cb));
}

export function onState(cb: (e: StateEvent) => void): Promise<UnlistenFn> {
  return Promise.resolve(stateHub.subscribe(cb));
}

export function onExit(cb: (e: ExitEvent) => void): Promise<UnlistenFn> {
  return Promise.resolve(exitHub.subscribe(cb));
}

/** Decode base64 PTY output into bytes suitable for `xterm.write(Uint8Array)`. */
export function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
