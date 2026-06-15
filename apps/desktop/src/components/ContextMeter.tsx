// ContextMeter — a compact per-tile readout of how FULL a Claude session's
// context window is (feat/context-meter).
//
// Each terminal tile runs `wsl → tmux → claude`; this sits in the tile header
// and shows that session's `context_window` fullness as a thin bar + a "78%"
// label. The value comes from the statusline snapshot matched to the tile by
// cwd (store/sessionContext.ts). It is BEST-EFFORT: when no Claude session is
// matched (`usedPct == null`) it renders nothing, so a plain shell tile or a
// session that hasn't emitted a statusline yet shows no meter at all.
//
// Colors are themed (var(--th-*)) and follow the tile chrome's low-key style.
// Unlike the sidebar UsageStrip (which shows plan usage LEFT), this shows
// context USED — higher is closer to the limit — so the color ramps toward the
// warning hue as the bar fills (amber past ~75%, red past ~90%).

/** Warn thresholds (USED %). Below the first is the calm/healthy hue; past them
 *  the meter shifts to amber then red as the window approaches full. */
const AMBER_AT = 75;
const RED_AT = 90;

/** Fill color by USED %: calm accent while there's headroom, amber as it tightens,
 *  red when nearly full. */
function fillColor(used: number): string {
  if (used >= RED_AT) return "var(--th-dot-error, #f87171)";
  if (used >= AMBER_AT) return "var(--th-dot-starting, #fbbf24)";
  return "var(--th-dot-live, #34d399)";
}

export interface ContextMeterProps {
  /** Context-window fullness 0..=100, or null when no session is matched (then
   *  the component renders nothing). */
  usedPct: number | null;
}

/**
 * Compact context-window meter: a short track whose fill width = used %, plus a
 * tabular "NN%" label. Renders `null` when `usedPct` is null so the tile header
 * is unchanged for unmatched tiles (graceful degradation per the task).
 */
export function ContextMeter({ usedPct }: ContextMeterProps) {
  if (usedPct == null) return null;
  // Clamp into range; round for the label (the bar uses the precise value).
  const used = Math.max(0, Math.min(100, usedPct));
  const rounded = Math.round(used);
  const color = fillColor(used);
  return (
    <span
      // Stop the header's pointer-drag from starting on the meter, and keep it
      // from shrinking away when the header is tight on space.
      onPointerDown={(e) => e.stopPropagation()}
      className="flex shrink-0 items-center gap-1"
      title={`Claude context window: ${rounded}% full`}
      aria-label={`Context window ${rounded} percent full`}
    >
      {/* Track + fill. Width is the used %. */}
      <span
        className="block h-1 w-8 overflow-hidden rounded-full"
        style={{
          backgroundColor: "color-mix(in srgb, var(--th-fg-muted) 25%, transparent)",
        }}
      >
        <span
          className="block h-full rounded-full transition-[width] duration-300"
          style={{ width: `${used}%`, backgroundColor: color }}
        />
      </span>
      <span
        className="tabular-nums text-[0.85em] leading-none"
        style={{ color: "var(--th-fg-muted)" }}
      >
        {rounded}%
      </span>
    </span>
  );
}
