import { useSyncExternalStore } from "react";

export type TerminalTemperature = "hot" | "warm" | "cold";

interface TerminalResources {
  temperature: TerminalTemperature;
  xterm: boolean;
  canvas: boolean;
  pty: boolean;
}

export interface TerminalResourceSnapshot {
  total: number;
  hot: number;
  warm: number;
  cold: number;
  xterms: number;
  canvases: number;
  ptys: number;
}

const DEFAULT_RESOURCES: TerminalResources = {
  temperature: "cold",
  xterm: false,
  canvas: false,
  pty: false,
};

const EMPTY_SNAPSHOT: TerminalResourceSnapshot = {
  total: 0,
  hot: 0,
  warm: 0,
  cold: 0,
  xterms: 0,
  canvases: 0,
  ptys: 0,
};

const resources = new Map<string, TerminalResources>();
const listeners = new Set<() => void>();
let snapshot = EMPTY_SNAPSHOT;

function rebuildSnapshot(): void {
  const next: TerminalResourceSnapshot = { ...EMPTY_SNAPSHOT };
  for (const resource of resources.values()) {
    next.total += 1;
    next[resource.temperature] += 1;
    if (resource.xterm) next.xterms += 1;
    if (resource.canvas) next.canvases += 1;
    if (resource.pty) next.ptys += 1;
  }
  snapshot = next;
  for (const listener of listeners) listener();
}

export function updateTerminalResources(
  terminalId: string,
  patch: Partial<TerminalResources>,
): void {
  const current = resources.get(terminalId) ?? DEFAULT_RESOURCES;
  const next = { ...current, ...patch };
  if (
    current.temperature === next.temperature &&
    current.xterm === next.xterm &&
    current.canvas === next.canvas &&
    current.pty === next.pty &&
    resources.has(terminalId)
  ) {
    return;
  }
  resources.set(terminalId, next);
  rebuildSnapshot();
}

export function removeTerminalResources(terminalId: string): void {
  if (!resources.delete(terminalId)) return;
  rebuildSnapshot();
}

export function getTerminalResourceSnapshot(): TerminalResourceSnapshot {
  return snapshot;
}

export function subscribeTerminalResources(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useTerminalResourceSnapshot(): TerminalResourceSnapshot {
  return useSyncExternalStore(
    subscribeTerminalResources,
    getTerminalResourceSnapshot,
    getTerminalResourceSnapshot,
  );
}

export function resetTerminalResourcesForTests(): void {
  resources.clear();
  rebuildSnapshot();
}
