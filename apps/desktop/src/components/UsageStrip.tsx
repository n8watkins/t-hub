// UsageStrip — the sidebar's bottom-pinned Claude plan-usage readout.
//
// Data source: `claude -p /usage` (parsed in src-tauri/src/usage.rs), polled from
// here. This is far more reliable than the statusline rate_limits (which only
// exist on Pro/Max + after the first turn): `/usage` always prints the plan
// usage. We LEAD with what the user watches — WEEKLY remaining — then session,
// each as "left" (100 - used) with the reset hint Claude reports.
import { useCallback, useEffect, useState } from "react";
import { claudeUsage, type ClaudeUsage } from "../ipc/usage";
import { codexUsage, type CodexUsage } from "../ipc/codex";
import { ClaudeIcon } from "./ClaudeIcon";
import { CodexIcon } from "./CodexIcon";

/** Poll cadence: /usage is a quick local Claude command; every 5 min (plus on
 *  mount + window focus) keeps the numbers fresh without spamming it. */
const POLL_MS = 5 * 60 * 1000;

/** Fill color by REMAINING %: red nearly out, amber low, green healthy. */
function fillColor(left: number): string {
  if (left <= 10) return "var(--th-dot-error, #f87171)";
  if (left <= 30) return "var(--th-dot-starting, #fbbf24)";
  return "var(--th-dot-live, #34d399)";
}

/**
 * The SINGLE source of truth for turning a raw `usedPct` into what the readouts
 * show: whether the value is `known`, the clamped `used` fill %, the rounded
 * `left` (remaining) %, and the `color` (by remaining, muted when unknown). Both
 * the expanded {@link Row} and the collapsed {@link InlinePct} consume this so
 * the two can never drift on rounding/"remaining vs used" convention.
 */
function usageStat(usedPct: number | null): {
  known: boolean;
  used: number;
  left: number | null;
  color: string;
} {
  const known = usedPct != null;
  const used = known ? Math.max(0, Math.min(100, usedPct)) : 0;
  const left = known ? Math.max(0, Math.round(100 - used)) : null;
  const color = known ? fillColor(left!) : "var(--th-fg-muted)";
  return { known, used, left, color };
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
  const { known, used, left, color } = usageStat(usedPct);
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-neutral-400">{label}</span>
        <span className="tabular-nums" style={{ color }}>
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
          style={{ width: `${used}%`, backgroundColor: known ? color : "transparent" }}
        />
      </div>
      {resets && (
        <span className="truncate text-[10px] text-neutral-600">resets {resets}</span>
      )}
    </div>
  );
}

/**
 * The collapsed Usage view: a terse, right-aligned readout of the key REMAINING
 * percentages — weekly (the number that matters) then the 5-hour session — meant
 * to sit inline in the bottom bar's header next to the "Usage" label (Sidebar.tsx
 * UsageSection). Numbers only (no bars/resets); colored by how much is LEFT.
 * Renders nothing until a good reading lands so the bar isn't a lone "—".
 */
export function UsageInline({ usage }: { usage: ClaudeUsage | null }) {
  if (!usage || !usage.ok) return null;
  return (
    <span className="ml-auto flex items-center gap-2 pr-2 text-[10px] tabular-nums">
      <ClaudeIcon
        size={11}
        className="shrink-0"
        style={{ color: "#D97757" }}
        title="Claude usage"
      />
      <InlinePct label="wk" usedPct={usage.weekUsedPct} resets={usage.weekResets} />
      <InlinePct
        label="5h"
        usedPct={usage.sessionUsedPct}
        resets={usage.sessionResets}
      />
    </span>
  );
}

/** One inline "label NN%" remaining readout for {@link UsageInline}. */
function InlinePct({
  label,
  usedPct,
  resets,
}: {
  label: string;
  usedPct: number | null;
  resets: string | null;
}) {
  const { left, color } = usageStat(usedPct);
  return (
    <span
      className="flex items-center gap-1"
      title={
        left != null
          ? `${label === "wk" ? "Weekly" : "5-hour session"}: ${left}% left${resets ? ` · resets ${resets}` : ""}`
          : "unknown"
      }
    >
      <span style={{ color: "var(--th-fg-muted)" }}>{label}</span>
      <span style={{ color }}>
        {left != null ? `${left}%` : "—"}
      </span>
    </span>
  );
}

/** Persist the last GOOD usage so the strip never flashes "unavailable" on a
 *  transient failed poll or right after launch — it shows the last-known weekly +
 *  5h until a fresh reading lands. */
const CACHE_KEY = "t-hub.usage.v1";

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

/**
 * Poll `claude -p /usage` and return the latest good reading (or null before the
 * first one). Seeded from the last-good cached reading so usage is visible
 * immediately on launch and never blanks while a poll is in flight; a
 * failed/unavailable poll keeps the last-known values rather than wiping them.
 *
 * Extracted as a hook so the SINGLE poller can feed both the expanded strip and
 * the collapsed inline summary without double-polling (Sidebar.tsx owns it).
 */
export function useClaudeUsage(): ClaudeUsage | null {
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

  return usage;
}

// ============================ Codex usage ===================================
// Codex has no `/usage` command; src-tauri/src/codex.rs reads its newest session
// rollout for the same rate-limit windows (primary ≈ 5h, secondary ≈ weekly).
// We reuse Row / InlinePct / usageStat above; the only difference is Codex
// reports `resetsAt` as a Unix epoch, so we format it to a short local string.

/** Format a Unix-epoch reset time to a short local "Jun 20, 9:00 PM" hint. */
function fmtReset(epoch: number | null | undefined): string | null {
  if (epoch == null) return null;
  try {
    return new Date(epoch * 1000).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  } catch {
    return null;
  }
}

const CODEX_CACHE_KEY = "t-hub.codexUsage.v1";

function loadCachedCodexUsage(): CodexUsage | null {
  if (typeof localStorage === "undefined") return null;
  try {
    const raw = localStorage.getItem(CODEX_CACHE_KEY);
    if (!raw) return null;
    const u = JSON.parse(raw) as CodexUsage;
    return u && u.ok ? u : null;
  } catch {
    return null;
  }
}

/** Poll Codex usage (same cadence + last-good caching as {@link useClaudeUsage}).
 *  Returns null until the first good reading; never blanks on a failed poll. */
export function useCodexUsage(): CodexUsage | null {
  const [usage, setUsage] = useState<CodexUsage | null>(loadCachedCodexUsage);

  const refresh = useCallback(() => {
    void codexUsage()
      .then((u) => {
        if (u && u.ok) {
          setUsage(u);
          try {
            localStorage.setItem(CODEX_CACHE_KEY, JSON.stringify(u));
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

  return usage;
}

/** Expanded Codex rows (weekly = secondary window, session = primary). Renders
 *  nothing when there's no Codex usage, so non-Codex users see no empty block. */
export function CodexUsageStrip({ usage }: { usage: CodexUsage | null }) {
  if (!usage || !usage.ok) return null;
  return (
    <div className="flex flex-col gap-2 px-2 py-1.5 text-[11px] text-neutral-500">
      <Row
        label="Weekly"
        usedPct={usage.secondary?.usedPercent ?? null}
        resets={fmtReset(usage.secondary?.resetsAt)}
      />
      <Row
        label="Session"
        usedPct={usage.primary?.usedPercent ?? null}
        resets={fmtReset(usage.primary?.resetsAt)}
      />
    </div>
  );
}

/** Collapsed Codex inline (weekly + 5h remaining). No `ml-auto` — it sits right
 *  after the Claude inline group in the section header. */
export function CodexUsageInline({ usage }: { usage: CodexUsage | null }) {
  if (!usage || !usage.ok) return null;
  return (
    <span className="flex items-center gap-2 pr-2 text-[10px] tabular-nums">
      <CodexIcon size={11} className="shrink-0" title="Codex usage" />
      <InlinePct
        label="wk"
        usedPct={usage.secondary?.usedPercent ?? null}
        resets={fmtReset(usage.secondary?.resetsAt)}
      />
      <InlinePct
        label="5h"
        usedPct={usage.primary?.usedPercent ?? null}
        resets={fmtReset(usage.primary?.resetsAt)}
      />
    </span>
  );
}

/**
 * The expanded Usage view: the full weekly + 5-hour session rows (bars + reset
 * hints). Presentational — the caller supplies `usage` from {@link useClaudeUsage}
 * (so it shares the collapsed summary's single poller). Shows a login hint until
 * the first good reading lands.
 */
export function UsageStrip({ usage }: { usage: ClaudeUsage | null }) {
  // Only show the empty hint when we have NEVER had a reading (no cache, first
  // poll not yet good). Once we've had data, it stays put across failed polls.
  if (!usage || !usage.ok) {
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
