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
  type TabReport,
  type TabReportResult,
  type TerminalId,
  type TerminalInfo,
} from "./types";

export function spawnTerminal(opts: SpawnOptions = {}): Promise<TerminalInfo> {
  return invoke(Commands.spawnTerminal, { opts });
}

/** Up-sync the live workspace-tab layout to the core's AUTHORITATIVE tab
 *  registry (TASK C / #22, headless-org). Carries the active tab and the last
 *  registry revision this window applied (`baseSeq`); the core rejects a stale
 *  report (returning the snapshot to adopt) so a server-side mutation the UI
 *  has not applied yet is never clobbered. */
export function reportWorkspaceTabs(
  tabs: TabReport[],
  activeTabId?: string,
  baseSeq?: number,
): Promise<TabReportResult> {
  return invoke(Commands.reportWorkspaceTabs, { tabs, activeTabId, baseSeq });
}

/** (Re)attach to a terminal; resolves to base64 scrollback to seed xterm. */
export function attachTerminal(
  id: TerminalId,
  cols: number,
  rows: number,
): Promise<string> {
  return invoke(Commands.attachTerminal, { id, cols, rows });
}

/** Write human-origin + local terminal-management (non-automation-message) input
 *  to a terminal (comms-plane Phase 1). Human keystrokes/paste/drop plus the app's
 *  own repaint/path-insert writes. AUTOMATION-message input (fleet wake, auto-
 *  continue, rules engine) must go through {@link deliverAgentInput} instead. */
export function writeTerminal(id: TerminalId, data: string): Promise<void> {
  return invoke(Commands.writeTerminal, { id, data });
}

/** comms-plane Phase 1: deliver AUTOMATION input through the plane's primary path.
 *  `source` names the internal automation writer (the backend refuses an unknown
 *  source). Auto-continue and the rules engine call this instead of
 *  {@link writeTerminal}; the bytes are written immediately (no durability yet -
 *  that is Phase 2), but now funnelled + attributed. */
export type PlaneWriteSource = "fleet-wake" | "auto-continue" | "rules-engine";
export function deliverAgentInput(
  id: TerminalId,
  data: string,
  source: PlaneWriteSource,
): Promise<void> {
  return invoke(Commands.deliverAgentInput, { id, data, source });
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

let inflightListTerminals: Promise<TerminalInfo[]> | null = null;

function isBoundedTmuxTimeout(error: unknown): boolean {
  return String(error).includes("command exceeded 10s timeout");
}

/**
 * List the tmux-backed terminals without multiplying cold-start WSL probes.
 *
 * Canvas intentionally has both a mount seed and a metadata refresh path, and
 * other surfaces may mount at the same time. Concurrent callers share one
 * request. A bounded tmux timeout receives one fresh attempt after the first
 * handler has returned; other errors remain visible without retry.
 */
export function listTerminals(): Promise<TerminalInfo[]> {
  if (inflightListTerminals) return inflightListTerminals;
  const request = (async () => {
    try {
      return await invoke<TerminalInfo[]>(Commands.listTerminals);
    } catch (error) {
      if (!isBoundedTmuxTimeout(error)) throw error;
      return invoke<TerminalInfo[]>(Commands.listTerminals);
    }
  })().finally(() => {
    if (inflightListTerminals === request) inflightListTerminals = null;
  });
  inflightListTerminals = request;
  return request;
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

type EventCallback<T> = (event: T) => void;

interface OrderedSubscriber<T> {
  callback: EventCallback<T>;
  order: number;
}

/** A per-channel fan-out hub: one real Tauri listener, keyed JS subscribers. */
class EventHub<T extends { id: TerminalId }> {
  private readonly globalSubs = new Map<EventCallback<T>, number>();
  private readonly keyedSubs = new Map<
    TerminalId,
    Map<EventCallback<T>, number>
  >();
  private backing: Promise<UnlistenFn> | null = null;
  private nextOrder = 0;

  constructor(private readonly event: string) {}

  /** Ensure the single backing Tauri listener is registered (idempotent). */
  private ensureBacking(): void {
    if (this.backing) return;
    // One listener for the whole app. Only global subscribers and subscribers
    // for this terminal are considered. Snapshotting before dispatch preserves
    // event semantics when a callback subscribes or unsubscribes re-entrantly.
    this.backing = listen<T>(this.event, (ev) => {
      const subscribers = this.subscribersFor(ev.payload.id);
      for (const { callback } of subscribers) {
        try {
          callback(ev.payload);
        } catch {
          // A throwing subscriber must not starve the others on this event.
        }
      }
    });
    // If registration ever fails (e.g. Tauri IPC not present), clear `backing`
    // so a later subscribe can retry, and swallow the rejection so it never
    // surfaces as an unhandled promise rejection.
    this.backing.catch(() => {
      this.backing = null;
    });
  }

  private subscribersFor(id: TerminalId): OrderedSubscriber<T>[] {
    const keyedSubscribers = this.keyedSubs.get(id);
    if (this.globalSubs.size === 0) {
      return Array.from(keyedSubscribers ?? [], ([callback, order]) => ({
        callback,
        order,
      }));
    }
    if (!keyedSubscribers || keyedSubscribers.size === 0) {
      return Array.from(this.globalSubs, ([callback, order]) => ({ callback, order }));
    }

    const subscribers: OrderedSubscriber<T>[] = [];
    for (const [callback, order] of this.globalSubs) {
      subscribers.push({ callback, order });
    }
    for (const [callback, order] of keyedSubscribers) {
      subscribers.push({ callback, order });
    }
    subscribers.sort((a, b) => a.order - b.order);
    return subscribers;
  }

  /** Register a global subscriber; returns a synchronous unsubscribe fn. */
  subscribe(cb: EventCallback<T>): UnlistenFn;
  /** Register a subscriber for one terminal; returns a synchronous unsubscribe fn. */
  subscribe(id: TerminalId, cb: EventCallback<T>): UnlistenFn;
  subscribe(idOrCb: TerminalId | EventCallback<T>, cb?: EventCallback<T>): UnlistenFn {
    const id = typeof idOrCb === "string" ? idOrCb : null;
    const callback = typeof idOrCb === "function" ? idOrCb : cb;
    if (!callback) throw new TypeError("terminal event subscriber is required");

    const subscribers = id === null ? this.globalSubs : this.subscribersForId(id);
    if (!subscribers.has(callback)) subscribers.set(callback, this.nextOrder++);
    this.ensureBacking();
    return () => {
      subscribers.delete(callback);
      if (
        id !== null &&
        subscribers.size === 0 &&
        this.keyedSubs.get(id) === subscribers
      ) {
        this.keyedSubs.delete(id);
      }
    };
  }

  private subscribersForId(id: TerminalId): Map<EventCallback<T>, number> {
    let subscribers = this.keyedSubs.get(id);
    if (!subscribers) {
      subscribers = new Map();
      this.keyedSubs.set(id, subscribers);
    }
    return subscribers;
  }
}

const outputHub = new EventHub<OutputEvent>(Events.output);
const stateHub = new EventHub<StateEvent>(Events.state);
const exitHub = new EventHub<ExitEvent>(Events.exit);

export function onOutput(cb: EventCallback<OutputEvent>): Promise<UnlistenFn>;
export function onOutput(
  id: TerminalId,
  cb: EventCallback<OutputEvent>,
): Promise<UnlistenFn>;
export function onOutput(
  idOrCb: TerminalId | EventCallback<OutputEvent>,
  cb?: EventCallback<OutputEvent>,
): Promise<UnlistenFn> {
  return Promise.resolve(
    typeof idOrCb === "string"
      ? outputHub.subscribe(idOrCb, cb!)
      : outputHub.subscribe(idOrCb),
  );
}

export function onState(cb: EventCallback<StateEvent>): Promise<UnlistenFn>;
export function onState(
  id: TerminalId,
  cb: EventCallback<StateEvent>,
): Promise<UnlistenFn>;
export function onState(
  idOrCb: TerminalId | EventCallback<StateEvent>,
  cb?: EventCallback<StateEvent>,
): Promise<UnlistenFn> {
  return Promise.resolve(
    typeof idOrCb === "string"
      ? stateHub.subscribe(idOrCb, cb!)
      : stateHub.subscribe(idOrCb),
  );
}

export function onExit(cb: EventCallback<ExitEvent>): Promise<UnlistenFn>;
export function onExit(
  id: TerminalId,
  cb: EventCallback<ExitEvent>,
): Promise<UnlistenFn>;
export function onExit(
  idOrCb: TerminalId | EventCallback<ExitEvent>,
  cb?: EventCallback<ExitEvent>,
): Promise<UnlistenFn> {
  return Promise.resolve(
    typeof idOrCb === "string"
      ? exitHub.subscribe(idOrCb, cb!)
      : exitHub.subscribe(idOrCb),
  );
}

/**
 * Decode base64 PTY output into bytes suitable for `xterm.write(Uint8Array)`.
 *
 * Hot path: this runs for EVERY output chunk of EVERY live terminal, so the
 * inner loop matters. `Uint8Array.from(atob(b64), c => c.charCodeAt(0))` lets
 * the engine size + fill the array in one native pass instead of a hand-rolled
 * `charCodeAt` loop with a bounds check per byte. `atob` still does the actual
 * base64 work; a malformed string throws there exactly as before, so callers
 * (which already wrap output handling in try/catch) see identical error
 * behavior — we just trade the JS loop for the engine's.
 */
export function decodeBase64(b64: string): Uint8Array {
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}
