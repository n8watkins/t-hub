// A compact badge rendering the FR-012 SessionStatus (PLAN.md §D). The headline
// 0.5 status `waitingOnSubagents` gets its own distinct treatment so an
// orchestrator that's still supervising children reads differently from a
// completed one. Pure presentational; no IPC.
import type { SessionStatus } from "../ipc/model";

/** Dot color + label + (optional) tooltip per status. */
interface StatusMeta {
  dot: string;
  label: string;
  /** Tailwind text color for the label. */
  text: string;
}

const STATUS_META: Record<SessionStatus, StatusMeta> = {
  working: { dot: "bg-emerald-500", label: "Working", text: "text-emerald-300" },
  waitingOnSubagents: {
    // Amber + pulse: actively supervising, not done.
    dot: "bg-amber-400 animate-pulse",
    label: "Waiting on subagents",
    text: "text-amber-300",
  },
  needsQuestion: { dot: "bg-sky-400", label: "Needs answer", text: "text-sky-300" },
  needsPermission: {
    dot: "bg-violet-400",
    label: "Needs permission",
    text: "text-violet-300",
  },
  completed: { dot: "bg-neutral-400", label: "Completed", text: "text-neutral-300" },
  failed: { dot: "bg-red-500", label: "Failed", text: "text-red-300" },
  rateLimited: { dot: "bg-orange-500", label: "Rate-limited", text: "text-orange-300" },
  detached: { dot: "bg-neutral-500", label: "Detached", text: "text-neutral-400" },
  restoring: { dot: "bg-amber-500", label: "Restoring", text: "text-amber-300" },
  expired: { dot: "bg-neutral-600", label: "Expired", text: "text-neutral-500" },
  unknown: { dot: "bg-neutral-700", label: "Unknown", text: "text-neutral-500" },
};

export interface StatusBadgeProps {
  status: SessionStatus;
  /** When true, render only the dot (for dense rows). */
  dotOnly?: boolean;
  className?: string;
}

export function StatusBadge({ status, dotOnly, className }: StatusBadgeProps) {
  const meta = STATUS_META[status] ?? STATUS_META.unknown;
  if (dotOnly) {
    return (
      <span
        className={`inline-block h-2 w-2 shrink-0 rounded-full ${meta.dot} ${className ?? ""}`}
        title={meta.label}
        aria-label={meta.label}
      />
    );
  }
  return (
    <span
      className={`inline-flex items-center gap-1.5 text-xs ${className ?? ""}`}
      title={meta.label}
    >
      <span className={`h-2 w-2 shrink-0 rounded-full ${meta.dot}`} aria-hidden />
      <span className={meta.text}>{meta.label}</span>
    </span>
  );
}

/** The human label for a status (for use outside the badge, e.g. the queue). */
export function statusLabel(status: SessionStatus): string {
  return (STATUS_META[status] ?? STATUS_META.unknown).label;
}
