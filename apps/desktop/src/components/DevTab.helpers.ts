import {
  type DevServerSnapshot,
  type RunTarget,
  type RunTargetDiscovery
} from "../ipc/devserver";
import type { TerminalId } from "../ipc/types";

type DiscoveryState = "loading" | RunTargetDiscovery["state"];

export interface DevState {
  snapshot: DevServerSnapshot;
  lines: string[];
  url: string | null;
  targets: RunTarget[];
  discoveryState: DiscoveryState;
  discoveryMessage: string | null;
  selectedTargetId: string | null;
  subscribed: boolean;
}

export const states = new Map<TerminalId, DevState>();

export const listeners = new Map<TerminalId, Set<() => void>>();

export const unlisteners = new Map<TerminalId, () => void>();

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
