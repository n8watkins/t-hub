import type { SessionStatus } from "../ipc/model";

/** Per-status label + label-text color. The indicator VARIANT is NOT stored here:
 *  it's derived from the shared {@link sessionStatusToVariant} so every surface
 *  (tiles, sidebar rows, this badge, and the Settings legend) renders the same
 *  status with the same indicator - no per-component drift. */
interface StatusMeta {
  label: string;
  /** Tailwind text color for the label. */
  text: string;
}

export const STATUS_META: Record<SessionStatus, StatusMeta> = {
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

/** The human label for a status (for use outside the badge, e.g. the queue). */
export function statusLabel(status: SessionStatus): string {
  return (STATUS_META[status] ?? STATUS_META.unknown).label;
}
