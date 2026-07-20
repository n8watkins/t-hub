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

export interface CortanaRecoveryOperation {
  currentId: () => string;
  authoritativeResult: () => void;
  failure: (error: unknown) => void;
}

/**
 * Keep one request identity across an ambiguous response leg, then advance after
 * every authoritative result so a later health check is not served stale cache.
 */
export function createCortanaRecoveryOperation(
  mintId: () => string = newCortanaRecoveryId,
): CortanaRecoveryOperation {
  let operationId = mintId();
  const rotate = () => {
    operationId = mintId();
  };
  return {
    currentId: () => operationId,
    authoritativeResult: rotate,
    failure(error) {
      if (!isAmbiguousCortanaFailure(error)) rotate();
    },
  };
}

export function cortanaFailureMessage(error: unknown): string {
  const fallback = "Cortana startup could not be completed";
  const raw = error instanceof Error ? error.message : typeof error === "string" ? error : fallback;
  const normalized = raw.replace(/\s+/g, " ").trim() || fallback;
  return normalized.length > 240 ? `${normalized.slice(0, 237)}...` : normalized;
}

export function isAmbiguousCortanaFailure(error: unknown): boolean {
  const explicitlyRetryable =
    typeof error === "object" &&
    error !== null &&
    "retryable" in error &&
    error.retryable === true;
  const message = cortanaFailureMessage(error);
  return (
    explicitlyRetryable ||
    AMBIGUOUS_FAILURE_PREFIXES.some((prefix) => message.startsWith(prefix)) ||
    message.includes("is already in flight")
  );
}
