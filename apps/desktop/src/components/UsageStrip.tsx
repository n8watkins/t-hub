// UsageStrip — the sidebar's bottom-pinned Claude plan-usage readout.
//
// Data source: `claude -p /usage` (parsed in src-tauri/src/usage.rs), polled from
// here. This is far more reliable than the statusline rate_limits (which only
// exist on Pro/Max + after the first turn): `/usage` always prints the plan
// usage. We LEAD with what the user watches — WEEKLY remaining — then session,
// each as "left" (100 - used) with the reset hint Claude reports.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { claudeUsage, type ClaudeUsage } from "../ipc/usage";
import { codexUsage, type CodexRateWindow, type CodexUsage } from "../ipc/codex";
import { ClaudeIcon } from "./ClaudeIcon";
import { CodexIcon } from "./CodexIcon";
import { runWhenIdle } from "../lib/windowInteraction";
import { useSupervision } from "../store/supervision";
import type { RateLimitWindow, StatusSnapshot } from "../ipc/model";

/** Poll cadence: every 5 min (plus on mount) keeps the numbers fresh without
 *  spamming the (heavy) underlying commands. */
const POLL_MS = 5 * 60 * 1000;

/** Usage-strip FOCUS-refresh policy. The window `focus` event fires many times a
 *  minute (alt-tab, click away + back), and each one used to re-run the heavy/flaky
 *  `claude -p /usage` (full Claude CLI in WSL). That both stormed WSL/CPU AND —
 *  because the misses correlate with call RATE (under load it prints intro-only,
 *  ~40% `ok=false`; verified the command is reliable run individually) — LOWERED the
 *  success rate, so the strip went stale ("weekly + 5h not updating").
 *
 *  Policy: on focus, refresh ONLY when the data is STALE — i.e. no GOOD (parsed)
 *  read within `USAGE_FRESH_MS` — and never re-run more than once per
 *  `USAGE_RETRY_GAP_MS` (so a failing streak can't storm). Mount + the 5-min
 *  interval always force-refresh. Net: fresh data → zero requests; coming back to a
 *  stale strip → one well-spaced request that updates it. Gating on the last GOOD
 *  read (not the last run) is what fixes the staleness a failing run used to cause.
 *
 *  Both floors are ~1 MINUTE: a FAILED `claude -p /usage` must not re-poll every few
 *  seconds (the flaky CLI takes ~4s/call, and even off the main thread that's wasted
 *  WSL/CPU). So on focus, usage checks at most once a minute; the 5-min interval is
 *  the only faster-than-that path and it's deliberately sparse. */
const USAGE_FRESH_MS = 60 * 1000;
const USAGE_RETRY_GAP_MS = 60 * 1000;

/** `claude -p /usage` is now only the COLD-START FALLBACK for Claude (the live
 *  statusline is the primary source — see {@link useClaudeUsage}). It's the
 *  heavy/flaky CLI, so when it IS used, keep it sparse: a good read is fresh for an
 *  hour and the interval re-checks hourly (a failed read still retries within
 *  USAGE_RETRY_GAP_MS so a flaky first read recovers in ~a minute). */
const CLAUDE_FRESH_MS = 60 * 60 * 1000;
const CLAUDE_POLL_MS = 60 * 60 * 1000;

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

/** Build a {@link ClaudeUsage} from the freshest statusline snapshot that carries
 *  rate limits. Rate limits are ACCOUNT-WIDE, so any active session's snapshot
 *  reflects current usage — we take the most recently ingested one. Returns null
 *  until a session has reported rate limits (pre-first-turn / free tier), so the
 *  caller can fall back to `claude -p /usage`. `usedPercentage` maps to the strip's
 *  "used" (it shows "left" = 100 − used); reset epochs are formatted like Codex. */
function selectStatuslineUsage(
  snapshots: Record<string, StatusSnapshot>,
): ClaudeUsage | null {
  // Rate limits are account-wide, but a single snapshot can be PARTIAL (one window
  // populated, the other null — the Rust `RateLimitWindow` fields are optional). So
  // pick the freshest NON-NULL value for EACH window INDEPENDENTLY — a partial newest
  // snapshot must not blank the other number (same class as the Codex partial fix).
  let five: RateLimitWindow | null = null;
  let fiveAt = -1;
  let seven: RateLimitWindow | null = null;
  let sevenAt = -1;
  for (const snap of Object.values(snapshots)) {
    if (!snap.rateLimitsPresent) continue;
    if (snap.fiveHour?.usedPercentage != null && snap.ingestedAtMs > fiveAt) {
      five = snap.fiveHour;
      fiveAt = snap.ingestedAtMs;
    }
    if (snap.sevenDay?.usedPercentage != null && snap.ingestedAtMs > sevenAt) {
      seven = snap.sevenDay;
      sevenAt = snap.ingestedAtMs;
    }
  }
  if (five == null && seven == null) return null;
  return {
    sessionUsedPct: five?.usedPercentage ?? null,
    sessionResets: fmtReset(five?.resetsAt),
    weekUsedPct: seven?.usedPercentage ?? null,
    weekResets: fmtReset(seven?.resetsAt),
    weekSonnetUsedPct: null,
    ok: true,
  };
}

/** Field-equality for the statusline-derived usage, so we return a STABLE object
 *  ref while the displayed numbers are unchanged (snapshots churn every turn). */
function sameClaudeUsage(a: ClaudeUsage | null, b: ClaudeUsage | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.ok === b.ok &&
    a.sessionUsedPct === b.sessionUsedPct &&
    a.weekUsedPct === b.weekUsedPct &&
    a.sessionResets === b.sessionResets &&
    a.weekResets === b.weekResets
  );
}

/**
 * Claude plan usage for the sidebar. **Statusline-first:** the PRIMARY source is the
 * live statusline `rate_limits` (5h + weekly) that Claude Code already streams per
 * turn for any active session — FREE, non-blocking, ACCOUNT-WIDE (one reading no
 * matter how many terminals), so there's no `claude -p /usage` spawn in the common
 * case. `claude -p /usage` is only the COLD-START FALLBACK: it runs when no session
 * has reported rate limits yet (pre-first-turn / free tier), sparsely (hourly).
 *
 * One poller, owned by Sidebar.tsx — NOT per-terminal. Seeded from the last-good
 * cache so usage shows immediately on launch and never blanks on a failed poll.
 */
export function useClaudeUsage(): ClaudeUsage | null {
  // PRIMARY: live statusline rate-limits, ref-stabilized so the strip only
  // re-renders when a displayed number actually changes (snapshots churn per turn).
  const snapshots = useSupervision((s) => s.snapshots);
  const slRef = useRef<ClaudeUsage | null>(null);
  const statusline = useMemo(() => {
    const next = selectStatuslineUsage(snapshots);
    if (sameClaudeUsage(slRef.current, next)) return slRef.current;
    slRef.current = next;
    return next;
  }, [snapshots]);

  // FALLBACK: claude -p /usage — only while the statusline has NO rate-limit data.
  const [polled, setPolled] = useState<ClaudeUsage | null>(loadCachedUsage);
  const lastRunRef = useRef(0);
  const lastGoodRef = useRef(0);
  // Set true once the statusline takes over (cold-start effect cleanup), so an
  // in-flight `claude -p /usage` started before the statusline landed is DROPPED
  // instead of overwriting the fresher statusline values / cache with a late read.
  const ignoreRef = useRef(false);

  const refresh = useCallback((force = false) => {
    const now = Date.now();
    if (!force) {
      const fresh = now - lastGoodRef.current < CLAUDE_FRESH_MS;
      const ranRecently = now - lastRunRef.current < USAGE_RETRY_GAP_MS;
      if (fresh || ranRecently) return; // fresh, or we just tried — skip
    }
    lastRunRef.current = now;
    void claudeUsage()
      .then((u) => {
        if (ignoreRef.current) return; // statusline landed first — drop late /usage
        // Only ADOPT a good reading; a failed poll keeps the last-known values.
        if (u && u.ok) {
          lastGoodRef.current = Date.now();
          setPolled(u);
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

  // Only spawn `claude -p /usage` while the statusline has nothing yet (cold start).
  const haveStatusline = !!statusline;
  useEffect(() => {
    if (haveStatusline) return; // statusline covers it — no CLI spawn
    ignoreRef.current = false; // (re)arm: we're the active source again
    refresh(true);
    const id = window.setInterval(() => refresh(true), CLAUDE_POLL_MS);
    let cancelIdle: (() => void) | undefined;
    const onFocus = () => {
      cancelIdle?.();
      cancelIdle = runWhenIdle(() => refresh());
    };
    window.addEventListener("focus", onFocus);
    return () => {
      ignoreRef.current = true; // statusline is taking over — drop in-flight /usage
      cancelIdle?.();
      window.clearInterval(id);
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh, haveStatusline]);

  // Persist the live statusline reading so a later fallback / restart shows it
  // immediately instead of blanking until the first /usage poll.
  useEffect(() => {
    if (!statusline) return;
    try {
      localStorage.setItem(CACHE_KEY, JSON.stringify(statusline));
    } catch {
      /* ignore quota */
    }
  }, [statusline]);

  return statusline ?? polled;
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

export function loadCachedCodexUsage(): CodexUsage | null {
  if (typeof localStorage === "undefined") return null;
  try {
    const raw = localStorage.getItem(CODEX_CACHE_KEY);
    if (!raw) return null;
    const u = JSON.parse(raw) as CodexUsage;
    return u && u.ok ? normalizeCodexWindows(u) : null;
  } catch {
    return null;
  }
}

function mergeCodexWindow(
  previous: CodexRateWindow | null,
  incoming: CodexRateWindow | null,
): CodexRateWindow | null {
  if (!incoming) return previous;
  if (!previous) return incoming;
  return {
    usedPercent: incoming.usedPercent ?? previous.usedPercent,
    windowMinutes: incoming.windowMinutes ?? previous.windowMinutes,
    resetsAt: incoming.resetsAt ?? previous.resetsAt,
  };
}

function normalizeCodexWindows(usage: CodexUsage): CodexUsage {
  const slots = [usage.primary, usage.secondary] as const;
  let fiveHour = slots.find((window) => window?.windowMinutes === 300) ?? null;
  let weekly = slots.find((window) => window?.windowMinutes === 10_080) ?? null;
  if (!fiveHour && !weekly) return usage;

  // A partial provider window can omit its duration while still carrying a fresh
  // percentage. Preserve it in its original provider slot when that semantic
  // slot is not already occupied, then use the remaining slot as a fallback.
  // This keeps a partial primary reading beside a recognized weekly secondary.
  for (const [index, window] of slots.entries()) {
    if (
      !window ||
      window.windowMinutes === 300 ||
      window.windowMinutes === 10_080
    ) {
      continue;
    }
    if (index === 0 && !fiveHour) fiveHour = window;
    else if (index === 1 && !weekly) weekly = window;
    else if (!fiveHour) fiveHour = window;
    else if (!weekly) weekly = window;
  }
  return {
    ...usage,
    primary: fiveHour,
    secondary: weekly,
  };
}

/** Merge a successful but partial provider snapshot into the last-known reading.
 * Codex can emit one rate-limit window without the other, so replacing the whole
 * cache would make the omitted weekly or session value disappear until a later
 * rollout happens to include it again. */
export function mergeCodexUsage(
  previous: CodexUsage | null,
  incoming: CodexUsage,
): CodexUsage | null {
  if (!incoming.ok) return previous;
  const normalizedIncoming = normalizeCodexWindows(incoming);
  if (!normalizedIncoming.primary && !normalizedIncoming.secondary) return previous;
  if (!previous?.ok) return normalizedIncoming;
  const normalizedPrevious = normalizeCodexWindows(previous);
  return {
    primary: mergeCodexWindow(normalizedPrevious.primary, normalizedIncoming.primary),
    secondary: mergeCodexWindow(
      normalizedPrevious.secondary,
      normalizedIncoming.secondary,
    ),
    planType: normalizedIncoming.planType ?? normalizedPrevious.planType,
    contextTokens:
      normalizedIncoming.contextTokens ?? normalizedPrevious.contextTokens,
    contextWindow:
      normalizedIncoming.contextWindow ?? normalizedPrevious.contextWindow,
    ok: true,
  };
}

export function isUsableCodexUsage(usage: CodexUsage): boolean {
  return mergeCodexUsage(null, usage) != null;
}

/** Locally roll a window past its reset boundary. Each window carries an absolute
 *  `resetsAt`; once that has passed, the window has rolled over to a FRESH one
 *  (0% used) — so we can show "available again" WITHOUT a new poll (esp. the 5h
 *  session). While still in-window the reading is returned unchanged. */
function advanceWindow(
  w: CodexRateWindow | null,
  nowSec: number,
): CodexRateWindow | null {
  if (!w) return w;
  const { resetsAt, windowMinutes } = w;
  if (resetsAt == null || windowMinutes == null || windowMinutes <= 0) return w;
  if (nowSec < resetsAt) return w; // still inside the current window — unchanged
  // One or more windows elapsed since the reading → a fresh window, 0% used.
  const span = windowMinutes * 60;
  let next = resetsAt;
  while (nowSec >= next) next += span;
  return { ...w, usedPercent: 0, resetsAt: next };
}

/** Apply {@link advanceWindow} to both Codex windows for display, so a stale
 *  last-known reading still shows correct "available" windows between polls. */
export function advanceCodexUsage(
  u: CodexUsage | null,
  nowMs: number,
): CodexUsage | null {
  if (!u || !u.ok) return u;
  const nowSec = Math.floor(nowMs / 1000);
  const primary = advanceWindow(u.primary, nowSec);
  const secondary = advanceWindow(u.secondary, nowSec);
  // `advanceWindow` returns the SAME ref while a window is still in-window, so if
  // neither rolled past its reset, return the SAME object — the 60s nowMs tick
  // must not churn a new allocation (and downstream renders) for no real change.
  if (primary === u.primary && secondary === u.secondary) return u;
  return { ...u, primary, secondary };
}

/** Poll Codex usage (same cadence + last-good caching as {@link useClaudeUsage}).
 *  Returns null until the first good reading; never blanks on a failed poll. The
 *  returned value is time-ADVANCED: windows past their reset show as fresh without
 *  waiting for the next poll (see {@link advanceCodexUsage}).
 *
 *  `enabled` GATES polling to "Codex is in use" (a Codex tile is open — see
 *  `useHasCodexSession`): when false we never spawn the WSL read, but still return
 *  the cached, time-advanced last-known value so the strip persists what it had. */
export function useCodexUsage(enabled = true): CodexUsage | null {
  const [usage, setUsage] = useState<CodexUsage | null>(loadCachedCodexUsage);
  const usageRef = useRef(usage);
  const lastRunRef = useRef(0);
  const lastGoodRef = useRef(0);
  const inFlightRef = useRef(false);

  const refresh = useCallback((force = false) => {
    if (inFlightRef.current) return;
    const now = Date.now();
    if (!force) {
      const fresh = now - lastGoodRef.current < USAGE_FRESH_MS;
      const ranRecently = now - lastRunRef.current < USAGE_RETRY_GAP_MS;
      if (fresh || ranRecently) return;
    }
    lastRunRef.current = now;
    inFlightRef.current = true;
    void codexUsage()
      .then((u) => {
        if (u && isUsableCodexUsage(u)) {
          const current = advanceCodexUsage(usageRef.current, Date.now());
          usageRef.current = current;
          const merged = mergeCodexUsage(current, u);
          if (!merged) return;
          usageRef.current = merged;
          lastGoodRef.current = Date.now();
          setUsage(merged);
          try {
            localStorage.setItem(CODEX_CACHE_KEY, JSON.stringify(merged));
          } catch {
            /* ignore quota */
          }
        }
      })
      .catch(() => {
        /* transient — keep the last-known values */
      })
      .finally(() => {
        inFlightRef.current = false;
      });
  }, []);

  useEffect(() => {
    if (!enabled) return; // only poll while a Codex session is in use
    refresh(true);
    const id = window.setInterval(() => refresh(true), POLL_MS);
    // Stale-gated, and deferred past an active window drag (see useClaudeUsage).
    let cancelIdle: (() => void) | undefined;
    const onFocus = () => {
      cancelIdle?.();
      cancelIdle = runWhenIdle(() => refresh());
    };
    window.addEventListener("focus", onFocus);
    return () => {
      cancelIdle?.(); // drop a deferred refresh so it can't fire after unmount
      window.clearInterval(id);
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh, enabled]);

  // Re-render once a minute so a window that has rolled past its reset shows as
  // "available again" without needing a fresh poll. Cheap: one state write/min.
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNowMs(Date.now()), 60 * 1000);
    return () => window.clearInterval(id);
  }, []);

  return useMemo(() => advanceCodexUsage(usage, nowMs), [usage, nowMs]);
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
