import type { TerminalTemperature } from "./terminalResources";

// How long a terminal stays WARM (mounted + attached to tmux, output flushed on
// the throttled background path) after it leaves the foreground before it goes
// COLD (xterm disposed, detached from tmux). Returning to a WARM terminal is
// instant; returning to a COLD one remounts and replays the tmux capture — a
// visible reload/flash.
//
// This was 30s, which made routine tab-switching feel like the terminals were
// "constantly refreshing": revisit any tab more than 30s after you last looked
// at it and it reloaded. A terminal held warm is cheap (an idle tmux attach +
// a background-throttled renderer), so we keep it warm generously and only cold
// out the genuinely-abandoned ones. 5 minutes comfortably covers real
// switching cadence while still eventually freeing terminals you've walked away
// from. (If the warm set ever needs a hard memory ceiling for users with many
// tabs, bound it by count on top of this timer rather than shortening it.)
export const TERMINAL_COLD_AFTER_MS = 300_000;
export const TERMINAL_MAX_WARM = 12;

export class TerminalLifecycleController {
  private readonly known = new Set<string>();
  private readonly cold = new Set<string>();
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly parkedOrder = new Map<string, number>();
  private sequence = 0;

  constructor(
    private readonly onChange: () => void,
    private readonly coldAfterMs = TERMINAL_COLD_AFTER_MS,
    private readonly maxWarm = TERMINAL_MAX_WARM,
  ) {}

  reconcile(ids: Iterable<string>, hotIds: ReadonlySet<string>): void {
    const current = new Set(ids);
    for (const id of this.known) {
      if (current.has(id)) continue;
      this.clearTimer(id);
      this.cold.delete(id);
      this.parkedOrder.delete(id);
    }

    for (const id of current) {
      if (hotIds.has(id)) {
        this.clearTimer(id);
        this.cold.delete(id);
        this.parkedOrder.delete(id);
        continue;
      }
      if (!this.parkedOrder.has(id)) this.parkedOrder.set(id, this.sequence++);
      if (this.cold.has(id) || this.timers.has(id)) continue;
      const timer = setTimeout(() => {
        this.timers.delete(id);
        if (!this.known.has(id)) return;
        this.cold.add(id);
        this.onChange();
      }, this.coldAfterMs);
      this.timers.set(id, timer);
    }
    const warm = [...current].filter(
      (id) => !hotIds.has(id) && !this.cold.has(id),
    );
    const demoted = warm
      .sort(
        (a, b) =>
          (this.parkedOrder.get(a) ?? 0) - (this.parkedOrder.get(b) ?? 0),
      )
      .slice(0, Math.max(0, warm.length - this.maxWarm));
    for (const id of demoted) {
      this.clearTimer(id);
      this.cold.add(id);
    }
    this.known.clear();
    for (const id of current) this.known.add(id);
    // reconcile runs from TerminalPool's effect, after React has already rendered
    // the previous temperatures. Force one follow-up render when the count cap
    // immediately demotes warm terminals so their TerminalViews unmount and their
    // RemotePty detach hooks run now, rather than waiting for an unrelated render.
    if (demoted.length > 0) this.onChange();
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
    this.parkedOrder.clear();
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
  const current: Promise<void> = previous
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
