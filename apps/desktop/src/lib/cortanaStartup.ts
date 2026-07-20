export interface CortanaReconcileResult {
  healthy: boolean;
  terminalId: string | null;
  degradedReason?: string | null;
}

const AMBIGUOUS_FAILURE_PREFIXES = [
  "control_protocol:",
  "control_request:",
  "control_timeout:",
  "control_unavailable:",
];

export function newCortanaRecoveryId(): string {
  try {
    if (typeof crypto !== "undefined" && crypto.randomUUID) {
      return crypto.randomUUID();
    }
  } catch {
    // Fall through to the collision-resistant local fallback.
  }
  return `cortana_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

export function cortanaFailureMessage(error: unknown): string {
  const fallback = "Cortana startup could not be completed";
  const raw = error instanceof Error ? error.message : typeof error === "string" ? error : fallback;
  const normalized = raw.replace(/\s+/g, " ").trim() || fallback;
  return normalized.length > 240 ? `${normalized.slice(0, 237)}...` : normalized;
}

export function isAmbiguousCortanaFailure(error: unknown): boolean {
  const message = cortanaFailureMessage(error);
  return (
    AMBIGUOUS_FAILURE_PREFIXES.some((prefix) => message.startsWith(prefix)) ||
    message.includes("is already in flight")
  );
}
