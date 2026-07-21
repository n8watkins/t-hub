// Frontend transport shim for the control channel (server-split M1).
//
// The webview can't open a raw TCP socket, so the actual socket I/O lives in the
// Rust shell (`src-tauri/src/control_client.rs`). This module is the JS side of
// that seam:
//   - `controlRequest(command, args)` round-trips ONE command over the loopback
//     control socket via the thin `control_request` Tauri command, and
//   - `onControlEvent(channel, cb)` subscribes to the backend event stream that
//     the Rust forwarder reads off the socket and re-emits into the webview as a
//     single `control://event` envelope.
//
// On localhost this is the same wire M2 stretches to a remote server — only the
// Rust endpoint's address changes. Migrated `client`/`client05` wrappers call
// these instead of a direct in-process `invoke`/`listen`, one command/event at a
// time, so the un-migrated surface is untouched.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** The Tauri event the Rust forwarder re-emits each socket event frame on. */
const CONTROL_EVENT = "control://event";

/** Envelope carried by `control://event`: the original backend channel + payload. */
interface ControlEventEnvelope {
  channel: string;
  payload: unknown;
}

/**
 * Round-trip one control command over the loopback socket and resolve to its
 * `result` (rejects with the dispatcher's error string). The Rust `control_request`
 * command unwraps the `{ok,result|error}` frame into a `Result`, so this resolves
 * with the bare result value (or rejects).
 */
export function controlRequest(
  command: string,
  args: Record<string, unknown> = {},
): Promise<unknown> {
  return invoke("control_request", { command, args }).catch((reason: unknown) => {
    if (isControlRequestFailure(reason)) {
      throw new ControlRequestError(reason.message, reason.retryable, reason.kind, reason.details);
    }
    throw reason;
  });
}

interface ControlRequestFailure {
  message: string;
  retryable: boolean;
  kind?: string;
  details?: unknown;
}

export class ControlRequestError extends Error {
  readonly retryable: boolean;
  readonly kind?: string;
  readonly details?: unknown;

  constructor(message: string, retryable: boolean, kind?: string, details?: unknown) {
    super(message);
    this.name = "ControlRequestError";
    this.retryable = retryable;
    this.kind = kind;
    this.details = details;
  }
}

function isControlRequestFailure(reason: unknown): reason is ControlRequestFailure {
  return (
    typeof reason === "object" &&
    reason !== null &&
    "message" in reason &&
    typeof reason.message === "string" &&
    "retryable" in reason &&
    typeof reason.retryable === "boolean"
  );
}

export function isRetryableControlError(reason: unknown): boolean {
  return (
    (reason instanceof ControlRequestError && reason.retryable) ||
    (isControlRequestFailure(reason) && reason.retryable)
  );
}

// --- control event hub -----------------------------------------------------
// One backing `control://event` listener for the whole app; fan out by inner
// `channel` to per-channel subscribers (mirrors the EventHub pattern in
// ipc/client.ts). The Rust forwarder multiplexes every backend channel onto the
// single `control://event` envelope, so we demux it here.

type ChannelCallback = (payload: unknown) => void;

const channelSubs = new Map<string, Set<ChannelCallback>>();
let backing: Promise<UnlistenFn> | null = null;

/** Ensure the single backing `control://event` Tauri listener is registered. */
function ensureBacking(): void {
  if (backing) return;
  backing = listen<ControlEventEnvelope>(CONTROL_EVENT, (ev) => {
    const env = ev.payload;
    if (!env || typeof env.channel !== "string") return;
    const subs = channelSubs.get(env.channel);
    if (!subs || subs.size === 0) return;
    for (const cb of [...subs]) {
      try {
        cb(env.payload);
      } catch {
        // A throwing subscriber must not starve the others on this frame.
      }
    }
  });
  // If registration fails (e.g. not under Tauri), clear `backing` so a later
  // subscribe can retry, and swallow the rejection.
  backing.catch(() => {
    backing = null;
  });
}

/**
 * Subscribe to one backend event channel delivered over the control socket.
 * Returns a synchronous unsubscribe fn. Channel names are the raw backend
 * channels (e.g. `session://status`) — the same strings the Tauri `listen` path
 * used, so migrating a wrapper is a one-line source swap.
 */
export function onControlEvent(
  channel: string,
  cb: ChannelCallback,
): UnlistenFn {
  let subs = channelSubs.get(channel);
  if (!subs) {
    subs = new Set();
    channelSubs.set(channel, subs);
  }
  subs.add(cb);
  ensureBacking();
  return () => {
    subs?.delete(cb);
  };
}
