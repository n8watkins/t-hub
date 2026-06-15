// UsageStrip — the sidebar's bottom-pinned Claude USAGE readout.
//
// What the user cares about is how much of their plan they have LEFT — chiefly
// the WEEKLY (7-day) rate-limit window — not cost. So this leads with "Weekly
// left: N%" (and "5h left: N%"), derived from the statusline snapshots the status
// bridge records (useSupervision().snapshots: contextUsedPct, costUsd, and the
// fiveHour/sevenDay rate-limit windows with used_percentage + resets_at).
//
// Caveats: the rate_limits block is Claude.ai Pro/Max only AND only appears after
// the first API response of a session, so "left" reads "—" until then. Cost is
// kept as a small secondary line.
import { useSupervision } from "../store/supervision";
import type { StatusSnapshot, RateLimitWindow } from "../ipc/model";

interface WindowAgg {
  used: number | null; // highest used% across sessions (most constrained)
  resetsAt: number | null;
}

interface Agg {
  totalCost: number;
  haveCost: boolean;
  maxContext: number | null;
  weekly: WindowAgg;
  fiveHour: WindowAgg;
  sessions: number;
}

function foldWindow(agg: WindowAgg, w: RateLimitWindow | undefined): WindowAgg {
  if (!w || typeof w.usedPercentage !== "number") return agg;
  // Account-level windows should agree across sessions; take the max used (= the
  // least remaining) to be safe, and carry that window's reset time.
  if (agg.used == null || w.usedPercentage > agg.used) {
    return { used: w.usedPercentage, resetsAt: w.resetsAt ?? agg.resetsAt };
  }
  return agg;
}

function aggregate(snaps: Record<string, StatusSnapshot>): Agg {
  let totalCost = 0;
  let haveCost = false;
  let maxContext: number | null = null;
  let weekly: WindowAgg = { used: null, resetsAt: null };
  let fiveHour: WindowAgg = { used: null, resetsAt: null };
  const list = Object.values(snaps);
  for (const s of list) {
    if (typeof s.costUsd === "number") {
      totalCost += s.costUsd;
      haveCost = true;
    }
    if (typeof s.contextUsedPct === "number") {
      maxContext = Math.max(maxContext ?? 0, s.contextUsedPct);
    }
    weekly = foldWindow(weekly, s.sevenDay);
    fiveHour = foldWindow(fiveHour, s.fiveHour);
  }
  return { totalCost, haveCost, maxContext, weekly, fiveHour, sessions: list.length };
}

/** Color by REMAINING %: red when nearly out, amber when low, green otherwise. */
function leftColor(left: number): string {
  if (left <= 10) return "text-red-400";
  if (left <= 30) return "text-amber-400";
  return "text-emerald-400";
}

/** Compact "resets in 3d" / "in 4h" / "in 25m" from an epoch-seconds reset time. */
function resetsIn(epochSecs: number | null): string {
  if (!epochSecs) return "";
  const diff = epochSecs - Math.floor(Date.now() / 1000);
  if (diff <= 0) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

/** One "X left" row for a rate-limit window. */
function WindowRow({ label, w }: { label: string; w: WindowAgg }) {
  const left = w.used != null ? Math.max(0, Math.round(100 - w.used)) : null;
  const reset = resetsIn(w.resetsAt);
  return (
    <div className="flex items-center justify-between gap-2">
      <span>{label} left</span>
      <span className="flex items-center gap-1.5">
        {reset && (
          <span className="text-[10px] text-neutral-600" title="resets in">
            {reset}
          </span>
        )}
        <span className={left != null ? leftColor(left) : "text-neutral-500"}>
          {left != null ? `${left}%` : "—"}
        </span>
      </span>
    </div>
  );
}

export function UsageStrip() {
  const snapshots = useSupervision((s) => s.snapshots);
  const agg = aggregate(snapshots);

  if (agg.sessions === 0) {
    return (
      <div
        className="px-2 py-1 text-[11px] leading-snug"
        style={{ color: "var(--th-fg-muted)" }}
        title="Usage is fed by Claude Code's statusline. Install hooks + statusline from Settings > Hooks, then run a Claude turn."
      >
        Usage appears once a Claude session reports. If it stays blank, install the
        statusline in Settings &gt; Hooks.
      </div>
    );
  }

  const noLimits = agg.weekly.used == null && agg.fiveHour.used == null;

  return (
    <div className="flex flex-col gap-0.5 px-2 py-1 text-[11px] text-neutral-500">
      {/* Weekly first — the number the user actually watches. */}
      <WindowRow label="Weekly" w={agg.weekly} />
      <WindowRow label="5h" w={agg.fiveHour} />
      {noLimits && (
        <div className="text-[10px] leading-snug text-neutral-600">
          Rate-limit % is Pro/Max only, after the first turn.
        </div>
      )}
      {agg.maxContext != null && (
        <div className="flex items-center justify-between gap-2">
          <span>Context</span>
          <span className={pctColorUp(agg.maxContext)}>
            {Math.round(agg.maxContext)}%
          </span>
        </div>
      )}
      {agg.haveCost && (
        <div className="flex items-center justify-between gap-2 text-neutral-600">
          <span>Cost</span>
          <span>${agg.totalCost.toFixed(2)}</span>
        </div>
      )}
    </div>
  );
}

/** Color by USED % (context fills up): red high, amber medium. */
function pctColorUp(pct: number): string {
  if (pct >= 90) return "text-red-400";
  if (pct >= 75) return "text-amber-400";
  return "text-neutral-400";
}
