import type { TerminalTemperature } from "./terminalResources";

export const TERMINAL_COLD_AFTER_MS = 30_000;

export class TerminalLifecycleController {
  private readonly known = new Set<string>();
  private readonly cold = new Set<string>();
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(
    private readonly onChange: () => void,
    private readonly coldAfterMs = TERMINAL_COLD_AFTER_MS,
  ) {}

  reconcile(ids: Iterable<string>, hotIds: ReadonlySet<string>): void {
    const current = new Set(ids);
    for (const id of this.known) {
      if (current.has(id)) continue;
      this.clearTimer(id);
      this.cold.delete(id);
    }

    for (const id of current) {
      if (hotIds.has(id)) {
        this.clearTimer(id);
        this.cold.delete(id);
        continue;
      }
      if (this.cold.has(id) || this.timers.has(id)) continue;
      const timer = setTimeout(() => {
        this.timers.delete(id);
        if (!this.known.has(id)) return;
        this.cold.add(id);
        this.onChange();
      }, this.coldAfterMs);
      this.timers.set(id, timer);
    }
    this.known.clear();
    for (const id of current) this.known.add(id);
  }

  temperature(id: string, hot: boolean): TerminalTemperature {
    if (hot) return "hot";
    return this.cold.has(id) ? "cold" : "warm";
  }

  dispose(): void {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
    this.known.clear();
    this.cold.clear();
  }

  private clearTimer(id: string): void {
    const timer = this.timers.get(id);
    if (timer === undefined) return;
    clearTimeout(timer);
    this.timers.delete(id);
  }
}

const pendingDetaches = new Map<string, Promise<void>>();

export function beginTerminalDetach(
  terminalId: string,
  detach: () => Promise<void>,
): Promise<void> {
  const previous = pendingDetaches.get(terminalId) ?? Promise.resolve();
  let current: Promise<void>;
  current = previous
    .catch(() => undefined)
    .then(detach)
    .finally(() => {
      if (pendingDetaches.get(terminalId) === current) {
        pendingDetaches.delete(terminalId);
      }
    });
  pendingDetaches.set(terminalId, current);
  return current;
}

export async function waitForTerminalDetach(terminalId: string): Promise<void> {
  await pendingDetaches.get(terminalId)?.catch(() => undefined);
}

export function resetTerminalDetachmentsForTests(): void {
  pendingDetaches.clear();
}
