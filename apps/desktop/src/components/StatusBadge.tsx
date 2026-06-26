// A compact badge rendering the FR-012 SessionStatus (PLAN.md §D). The headline
// 0.5 status `waitingOnSubagents` gets its own distinct treatment so an
// orchestrator that's still supervising children reads differently from a
// completed one. Pure presentational; no IPC.
import type { SessionStatus } from "../ipc/model";
import { StatusIndicator, type StatusVariant } from "./StatusIndicator";

/** Indicator variant + label + label text color per status. The visual is now
 *  the shared ring+center {@link StatusIndicator}; `variant` picks its state. */
interface StatusMeta {
  variant: StatusVariant;
  label: string;
  /** Tailwind text color for the label. */
  text: string;
}

// FR-012 11-state SessionStatus → the shared 5 indicator variants:
//   working                            → working   (pulsing accent ring)
//   completed                          → done      (solid green)
//   needsQuestion/needsPermission/
//     waitingOnSubagents               → attention (pulsing amber ring)
//   failed (+ rateLimited)             → error     (solid red)
//   detached/restoring/expired/unknown → idle      (muted hollow ring)
// rateLimited maps to `error` (it's a hard block on progress); restoring keeps
// the amber-ish "in flight" read via `attention`.
const STATUS_META: Record<SessionStatus, StatusMeta> = {
  working: { variant: "working", label: "Working", text: "text-emerald-300" },
  waitingOnSubagents: {
    variant: "attention",
    label: "Waiting on subagents",
    text: "text-amber-300",
  },
  needsQuestion: { variant: "attention", label: "Needs answer", text: "text-sky-300" },
  needsPermission: {
    variant: "attention",
    label: "Needs permission",
    text: "text-violet-300",
  },
  completed: { variant: "done", label: "Completed", text: "text-neutral-300" },
  failed: { variant: "error", label: "Failed", text: "text-red-300" },
  rateLimited: { variant: "error", label: "Rate-limited", text: "text-orange-300" },
  detached: { variant: "idle", label: "Detached", text: "text-neutral-400" },
  restoring: { variant: "attention", label: "Restoring", text: "text-amber-300" },
  expired: { variant: "idle", label: "Expired", text: "text-neutral-500" },
  unknown: { variant: "idle", label: "Unknown", text: "text-neutral-500" },
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
      <StatusIndicator
        variant={meta.variant}
        size={9}
        title={meta.label}
        className={className}
      />
    );
  }
  return (
    <span
      className={`inline-flex items-center gap-1.5 text-xs ${className ?? ""}`}
      title={meta.label}
    >
      <StatusIndicator variant={meta.variant} size={9} title={meta.label} />
      <span className={meta.text}>{meta.label}</span>
    </span>
  );
}

/** The human label for a status (for use outside the badge, e.g. the queue). */
export function statusLabel(status: SessionStatus): string {
  return (STATUS_META[status] ?? STATUS_META.unknown).label;
}
