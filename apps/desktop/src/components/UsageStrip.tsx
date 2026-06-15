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

/** Fill color by REMAINING %: red nearly out, amber low, green healthy. */
function fillColor(left: number): string {
  if (left <= 10) return "var(--th-dot-error, #f87171)";
  if (left <= 30) return "var(--th-dot-starting, #fbbf24)";
  return "var(--th-dot-live, #34d399)";
}

/** One usage row: a label, a remaining-% number, and a horizontal BAR whose fill
 *  shows how much is USED (it fills up as you consume), colored by how much is
 *  LEFT. A reset hint sits under it. "—" when the value is unknown. */
function Row({
  label,
  usedPct,
  resets,
}: {
  label: string;
  usedPct: number | null;
  resets: string | null;
}) {
  const known = usedPct != null;
  const used = known ? Math.max(0, Math.min(100, usedPct)) : 0;
  const left = known ? Math.max(0, Math.round(100 - used)) : null;
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-neutral-400">{label}</span>
        <span className="tabular-nums" style={{ color: known ? fillColor(left!) : "var(--th-fg-muted)" }}>
          {left != null ? `${left}% left` : "—"}
        </span>
      </div>
      {/* Bar: track + fill (fill width = used%). */}
      <div
        className="h-1.5 w-full overflow-hidden rounded-full"
        style={{ backgroundColor: "color-mix(in srgb, var(--th-fg-muted) 25%, transparent)" }}
        title={left != null ? `${left}% left${resets ? ` · resets ${resets}` : ""}` : "unknown"}
      >
        <div
          className="h-full rounded-full transition-[width] duration-300"
          style={{ width: `${used}%`, backgroundColor: known ? fillColor(left!) : "transparent" }}
        />
      </div>
      {resets && (
        <span className="truncate text-[10px] text-neutral-600">resets {resets}</span>
      )}
    </div>
  );
}

/** Minimized usage (the collapsed view): just the weekly + session bars and the
 *  remaining %, side by side — no labels-as-rows, no reset hints. */
function CompactUsage({ usage }: { usage: ClaudeUsage }) {
  return (
    <div className="flex items-center gap-3 px-2 pb-1.5 pt-0.5 text-[10px] text-neutral-500">
      <MiniBar label="W" usedPct={usage.weekUsedPct} resets={usage.weekResets} />
      <MiniBar label="S" usedPct={usage.sessionUsedPct} resets={usage.sessionResets} />
    </div>
  );
}

function MiniBar({
  label,
  usedPct,
  resets,
}: {
  label: string;
  usedPct: number | null;
  resets: string | null;
}) {
  const known = usedPct != null;
  const used = known ? Math.max(0, Math.min(100, usedPct)) : 0;
  const left = known ? Math.max(0, Math.round(100 - used)) : null;
  return (
    <div
      className="flex min-w-0 flex-1 items-center gap-1.5"
      title={
        left != null
          ? `${label === "W" ? "Weekly" : "Session"}: ${left}% left${resets ? ` · resets ${resets}` : ""}`
          : "unknown"
      }
    >
      <span className="shrink-0 text-neutral-400">{label}</span>
      <div
        className="h-1 min-w-0 flex-1 overflow-hidden rounded-full"
        style={{ backgroundColor: "color-mix(in srgb, var(--th-fg-muted) 25%, transparent)" }}
      >
        <div
          className="h-full rounded-full"
          style={{ width: `${used}%`, backgroundColor: known ? fillColor(left!) : "transparent" }}
        />
      </div>
      <span
        className="shrink-0 tabular-nums"
        style={{ color: known ? fillColor(left!) : "var(--th-fg-muted)" }}
      >
        {left != null ? `${left}%` : "—"}
      </span>
    </div>
  );
}

/** Persist the last GOOD usage so the strip never flashes "unavailable" on a
 *  transient failed poll or right after launch — it shows the last-known weekly +
 *  5h until a fresh reading lands. */
const CACHE_KEY = "termhub.usage.v1";

function loadCachedUsage(): ClaudeUsage | null {
  if (typeof localStorage === "undefined") return null;
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const u = JSON.parse(raw) as ClaudeUsage;
    return u && u.ok ? u : null;
  } catch {
    return null;
  }
}

export function UsageStrip({ compact = false }: { compact?: boolean }) {
  // Seed from the cached last-good reading so usage is visible immediately on
  // launch and never blanks while a poll is in flight.
  const [usage, setUsage] = useState<ClaudeUsage | null>(loadCachedUsage);

  const refresh = useCallback(() => {
    void claudeUsage()
      .then((u) => {
        // Only ADOPT a good reading. A failed/unavailable poll must NOT wipe the
        // last-known values (the "usage keeps disappearing" fix) — we keep
        // showing the cached weekly + 5h until a fresh good reading replaces it.
        if (u && u.ok) {
          setUsage(u);
          try {
            localStorage.setItem(CACHE_KEY, JSON.stringify(u));
          } catch {
            /* ignore quota */
          }
        }
      })
      .catch(() => {
        /* transient — keep the last-known values */
      });
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

  // Only show the empty hint when we have NEVER had a reading (no cache, first
  // poll not yet good). Once we've had data, it stays put across failed polls.
  if (!usage || !usage.ok) {
    if (compact) return null; // collapsed view stays empty until a reading lands
    return (
      <div
        className="px-2 py-1 text-[11px] leading-snug"
        style={{ color: "var(--th-fg-muted)" }}
        title="Runs `claude -p /usage`. Make sure you're logged into Claude in WSL."
      >
        Usage loading… (ensure you're logged into Claude)
      </div>
    );
  }

  if (compact) return <CompactUsage usage={usage} />;

  return (
    <div className="flex flex-col gap-2 px-2 py-1.5 text-[11px] text-neutral-500">
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
