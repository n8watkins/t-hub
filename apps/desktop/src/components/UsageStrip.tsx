// UsageStrip — the sidebar's bottom-pinned Claude plan-usage readout.
//
// Data source: `claude -p /usage` (parsed in src-tauri/src/usage.rs), polled from
// here. This is far more reliable than the statusline rate_limits (which only
// exist on Pro/Max + after the first turn): `/usage` always prints the plan
// usage. We LEAD with what the user watches — WEEKLY remaining — then session,
// each as "left" (100 - used) with the reset hint Claude reports.
import { useCallback, useEffect, useState } from "react";
import { claudeUsage, type ClaudeUsage } from "../ipc/usage";

/** Poll cadence: /usage is a quick local Claude command; every 5 min (plus on
 *  mount + window focus) keeps the numbers fresh without spamming it. */
const POLL_MS = 5 * 60 * 1000;

/** Color by REMAINING %: red nearly out, amber low, green healthy. */
function leftColor(left: number): string {
  if (left <= 10) return "text-red-400";
  if (left <= 30) return "text-amber-400";
  return "text-emerald-400";
}

/** One "X left: N%" row from a used-percentage + reset hint. */
function Row({
  label,
  usedPct,
  resets,
}: {
  label: string;
  usedPct: number | null;
  resets: string | null;
}) {
  const left = usedPct != null ? Math.max(0, Math.round(100 - usedPct)) : null;
  return (
    <div className="flex items-center justify-between gap-2">
      <span>{label} left</span>
      <span className="flex items-center gap-1.5">
        {resets && (
          <span className="truncate text-[10px] text-neutral-600" title={`resets ${resets}`}>
            {resets}
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
  const [usage, setUsage] = useState<ClaudeUsage | null>(null);
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(() => {
    void claudeUsage()
      .then((u) => {
        setUsage(u);
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
  }, []);

  useEffect(() => {
    refresh();
    const id = window.setInterval(refresh, POLL_MS);
    window.addEventListener("focus", refresh);
    return () => {
      window.clearInterval(id);
      window.removeEventListener("focus", refresh);
    };
  }, [refresh]);

  if (!loaded) {
    return (
      <div className="px-2 py-1 text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
        Loading usage…
      </div>
    );
  }

  if (!usage || !usage.ok) {
    return (
      <div
        className="px-2 py-1 text-[11px] leading-snug"
        style={{ color: "var(--th-fg-muted)" }}
        title="Runs `claude -p /usage`. Make sure you're logged into Claude in WSL."
      >
        Usage unavailable. Ensure you're logged into Claude.
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-0.5 px-2 py-1 text-[11px] text-neutral-500">
      {/* Weekly first — the number that actually matters. */}
      <Row label="Weekly" usedPct={usage.weekUsedPct} resets={usage.weekResets} />
      <Row
        label="Session"
        usedPct={usage.sessionUsedPct}
        resets={usage.sessionResets}
      />
    </div>
  );
}
