// UsageStrip — the sidebar's bottom-pinned Claude USAGE readout.
//
// The reworked sidebar dropped the old Usage view when it stopped reading the
// supervision store; this brings it back as a compact strip. It reads the
// per-session statusline snapshots the status bridge records
// (useSupervision().snapshots: contextUsedPct, costUsd, 5h/7d rate-limit
// windows) and aggregates them across all live Claude sessions:
//   - total cost  = sum of every session's costUsd
//   - context     = the MOST-full session's contextUsedPct
//   - rate limit  = the highest used% across any 5h/7d window (closest to a cap)
// Snapshots arrive only for sessions whose Claude statusline has reported, so an
// empty state ("No Claude usage yet") is normal until a session emits one.
import { useSupervision } from "../store/supervision";
import type { StatusSnapshot } from "../ipc/model";

interface Agg {
  totalCost: number;
  haveCost: boolean;
  maxContext: number | null;
  maxRate: number | null;
  sessions: number;
}

function aggregate(snaps: Record<string, StatusSnapshot>): Agg {
  let totalCost = 0;
  let haveCost = false;
  let maxContext: number | null = null;
  let maxRate: number | null = null;
  const list = Object.values(snaps);
  for (const s of list) {
    if (typeof s.costUsd === "number") {
      totalCost += s.costUsd;
      haveCost = true;
    }
    if (typeof s.contextUsedPct === "number") {
      maxContext = Math.max(maxContext ?? 0, s.contextUsedPct);
    }
    for (const w of [s.fiveHour, s.sevenDay]) {
      if (w && typeof w.usedPercentage === "number") {
        maxRate = Math.max(maxRate ?? 0, w.usedPercentage);
      }
    }
  }
  return { totalCost, haveCost, maxContext, maxRate, sessions: list.length };
}

/** Color a percentage: red near a cap, amber when high, muted otherwise. */
function pctColor(pct: number): string {
  if (pct >= 90) return "text-red-400";
  if (pct >= 75) return "text-amber-400";
  return "text-neutral-400";
}

export function UsageStrip() {
  const snapshots = useSupervision((s) => s.snapshots);
  const agg = aggregate(snapshots);

  if (agg.sessions === 0) {
    // No snapshots yet. This is normal until a Claude session's statusline
    // reports — but it's ALSO what you see if the statusline was never installed
    // into ~/.claude/settings.json (the data source). Surface the actionable
    // hint rather than a bare "no usage" so the user knows where to look.
    return (
      <div
        className="px-2 py-1 text-[11px] leading-snug"
        style={{ color: "var(--th-fg-muted)" }}
        title="Usage is fed by Claude Code's statusline. Install hooks + statusline from Settings > Hooks, then run a Claude turn."
      >
        Usage appears once a Claude session reports. If it stays blank, install
        the statusline in Settings &gt; Hooks.
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-0.5 px-2 py-1 text-[11px] text-neutral-500">
      <div className="flex items-center justify-between gap-2">
        <span>Cost</span>
        <span className="text-neutral-300" title="Sum of cost across live sessions">
          {agg.haveCost ? `$${agg.totalCost.toFixed(2)}` : "—"}
        </span>
      </div>
      <div className="flex items-center justify-between gap-2">
        <span>Context</span>
        <span
          className={agg.maxContext != null ? pctColor(agg.maxContext) : "text-neutral-400"}
          title="Highest context-window usage across sessions"
        >
          {agg.maxContext != null ? `${Math.round(agg.maxContext)}%` : "—"}
        </span>
      </div>
      <div className="flex items-center justify-between gap-2">
        <span>Rate limit</span>
        <span
          className={agg.maxRate != null ? pctColor(agg.maxRate) : "text-neutral-400"}
          title="Highest rate-limit window usage (5h / 7d) across sessions"
        >
          {agg.maxRate != null ? `${Math.round(agg.maxRate)}%` : "—"}
        </span>
      </div>
    </div>
  );
}
