// A compact badge rendering the FR-012 SessionStatus (PLAN.md §D). The headline
// 0.5 status `waitingOnSubagents` gets its own distinct treatment so an
// orchestrator that's still supervising children reads differently from a
// completed one. Pure presentational; no IPC.
import type { SessionStatus } from "../ipc/model";
import { StatusIndicator, sessionStatusToVariant } from "./StatusIndicator";

/** Per-status label + label-text color. The indicator VARIANT is NOT stored here:
 *  it's derived from the shared {@link sessionStatusToVariant} so every surface
 *  (tiles, sidebar rows, this badge, and the Settings legend) renders the same
 *  status with the same indicator — no per-component drift. */
interface StatusMeta {
  label: string;
  /** Tailwind text color for the label. */
  text: string;
}

const STATUS_META: Record<SessionStatus, StatusMeta> = {
  working: { label: "Working", text: "text-emerald-300" },
  waitingOnSubagents: { label: "Waiting on subagents", text: "text-amber-300" },
  needsQuestion: { label: "Needs answer", text: "text-sky-300" },
  needsPermission: { label: "Needs permission", text: "text-violet-300" },
  completed: { label: "Completed", text: "text-neutral-300" },
  failed: { label: "Failed", text: "text-red-300" },
  rateLimited: { label: "Rate-limited", text: "text-orange-300" },
  detached: { label: "Detached", text: "text-neutral-400" },
  restoring: { label: "Restoring", text: "text-amber-300" },
  expired: { label: "Expired", text: "text-neutral-500" },
  unknown: { label: "Unknown", text: "text-neutral-500" },
};

export interface StatusBadgeProps {
  status: SessionStatus;
  /** When true, render only the dot (for dense rows). */
  dotOnly?: boolean;
  className?: string;
}

export function StatusBadge({ status, dotOnly, className }: StatusBadgeProps) {
  const meta = STATUS_META[status] ?? STATUS_META.unknown;
  const variant = sessionStatusToVariant(status);
  if (dotOnly) {
    return (
      <StatusIndicator
        variant={variant}
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
      <StatusIndicator variant={variant} size={9} title={meta.label} />
      <span className={meta.text}>{meta.label}</span>
    </span>
  );
}

/** The human label for a status (for use outside the badge, e.g. the queue). */
export function statusLabel(status: SessionStatus): string {
  return (STATUS_META[status] ?? STATUS_META.unknown).label;
}
