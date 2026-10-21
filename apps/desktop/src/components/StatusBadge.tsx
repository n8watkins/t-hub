import { STATUS_META } from "./StatusBadge.helpers";
// A compact badge rendering the FR-012 SessionStatus (PLAN.md §D). The headline
// 0.5 status `waitingOnSubagents` gets its own distinct treatment so an
// orchestrator that's still supervising children reads differently from a
// completed one. Pure presentational; no IPC.
import type { SessionStatus } from "../ipc/model";
import { StatusIndicator } from "./StatusIndicator";
import { sessionStatusToVariant } from "./StatusIndicator.helpers";

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
