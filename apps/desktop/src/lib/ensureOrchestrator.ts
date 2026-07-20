// Frontend helpers for the durable Cortana singleton. The backend owns runtime
// discovery, identity preservation, duplicate handling, and recovery. The
// frontend supplies one stable startup operation id and validates the result
// before it represents Cortana as healthy.
//
// `~/.t-hub/orchestrator` is the canonical orchestrator home. The legacy display
// fallback recognizes that home by cwd suffix without resolving the absolute
// WSL or Windows home path in the frontend.

/** The canonical orchestrator home, as a path suffix. */
export const ORCHESTRATOR_CWD_SUFFIX = ".t-hub/orchestrator";

/** The user-facing NAME of the orchestrator agent. The orchestrator's cwd
 *  basename is "orchestrator", so the derived stable identity would read
 *  "orchestrator" - a bland, technical label. Wherever the DESIGNATED
 *  orchestrator is rendered to the user (the sidebar Agents row, the overlay
 *  switcher chip) we substitute this name instead. This is a display-only
 *  concern: the store's `orchestratorId` / `ensureOrchestrator` adopt logic is
 *  untouched, and a captain's own derived identity logic never sees it. */
export const ORCHESTRATOR_DISPLAY_NAME = "Cortana";

/** A stable identifier retained for parser and monitor contract fixtures. */
export const CORTANA_RECONCILE_OPERATION_ID = "t-hub.desktop.cortana.startup.v1";

/**
 * Backend reconciliation is cheap bounded control-plane work. This interval is
 * frequent enough to recover a dead harness without asking the model for status
 * and slow enough to avoid turning supervision into a polling workload.
 */
export const CORTANA_RECONCILE_INTERVAL_MS = 30_000;

export interface CortanaReconcileResult {
  operationId: string;
  action: "keep" | "adopt" | "recover" | "create" | "degraded";
  healthy: boolean;
  terminalId: string | null;
  identityId: string | null;
  generation: number;
  degradedReason: string | null;
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

/** Fail closed on a malformed reconciliation response. */
export function parseCortanaReconcileResult(value: unknown): CortanaReconcileResult {
  if (!value || typeof value !== "object") {
    throw new Error("Cortana reconciliation returned no result.");
  }
  const result = value as Record<string, unknown>;
  const actions = new Set(["keep", "adopt", "recover", "create", "degraded"]);
  if (
    !isNonEmptyString(result.operationId) ||
    !actions.has(String(result.action)) ||
    typeof result.healthy !== "boolean" ||
    (result.terminalId !== null && !isNonEmptyString(result.terminalId)) ||
    (result.identityId !== null && !isNonEmptyString(result.identityId)) ||
    typeof result.generation !== "number" ||
    !Number.isSafeInteger(result.generation) ||
    result.generation < 0 ||
    (result.degradedReason !== null && typeof result.degradedReason !== "string")
  ) {
    throw new Error("Cortana reconciliation returned malformed identity or recovery evidence.");
  }
  const parsed = result as unknown as CortanaReconcileResult;
  if (parsed.healthy && (!parsed.terminalId || !parsed.identityId || parsed.generation < 1)) {
    throw new Error("Cortana reconciliation claimed health without a durable live identity.");
  }
  if (!parsed.healthy && parsed.action !== "degraded") {
    throw new Error("Cortana reconciliation returned an inconsistent degraded state.");
  }
  return parsed;
}

/** Normalize a cwd for suffix comparison: unify separators, drop trailing
 *  slashes, lowercase (Windows paths are case-insensitive; WSL paths here are
 *  lowercase for `.t-hub` anyway). */
function normalizeCwd(cwd: string): string {
  return cwd.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

/** True when `cwd` is the orchestrator home (`.../.t-hub/orchestrator`). */
export function isOrchestratorCwd(cwd: string | undefined | null): boolean {
  if (!cwd) return false;
  const n = normalizeCwd(cwd);
  return n === ORCHESTRATOR_CWD_SUFFIX || n.endsWith("/" + ORCHESTRATOR_CWD_SUFFIX);
}

/** A live terminal, as much as this logic needs. */
export interface OrchestratorTerminal {
  cwd?: string;
  state?: string;
}

export interface CortanaReconciliationMonitorOptions {
  reconcile: () => Promise<unknown>;
  onResult: (result: CortanaReconcileResult) => void;
  onError: (error: unknown) => void;
  intervalMs?: number;
}

export interface CortanaReconciliationMonitor {
  start: () => void;
  requestNow: () => void;
  observeTerminals: (terminals: Record<string, OrchestratorTerminal>) => void;
  stop: () => void;
}

/**
 * Keep backend-owned Cortana reconciliation active for the desktop lifetime.
 *
 * The monitor reacts immediately when the UI observes its authoritative terminal
 * exit, and also performs a bounded periodic backend check so a dead harness in
 * an otherwise-live terminal is recovered. Only one request may be in flight.
 * Timer or liveness signals received during a request collapse into one trailing
 * reconciliation. No model prompt, token, transcript, or provider API is polled.
 */
export function createCortanaReconciliationMonitor({
  reconcile,
  onResult,
  onError,
  intervalMs = CORTANA_RECONCILE_INTERVAL_MS,
}: CortanaReconciliationMonitorOptions): CortanaReconciliationMonitor {
  if (!Number.isFinite(intervalMs) || intervalMs < 1_000) {
    throw new Error("Cortana reconciliation interval must be at least one second.");
  }

  let started = false;
  let stopped = false;
  let inFlight = false;
  let trailingRun = false;
  let timer: ReturnType<typeof globalThis.setInterval> | undefined;
  let terminalId: string | null = null;
  let terminalWasObserved = false;

  const run = () => {
    if (stopped) return;
    if (inFlight) {
      trailingRun = true;
      return;
    }
    inFlight = true;
    void reconcile()
      .then((value) => {
        if (stopped) return;
        const result = parseCortanaReconcileResult(value);
        if (terminalId !== result.terminalId) terminalWasObserved = false;
        terminalId = result.terminalId;
        onResult(result);
      })
      .catch((error) => {
        if (!stopped) onError(error);
      })
      .finally(() => {
        inFlight = false;
        if (!stopped && trailingRun) {
          trailingRun = false;
          run();
        }
      });
  };

  return {
    start() {
      if (started || stopped) return;
      started = true;
      run();
      timer = globalThis.setInterval(run, intervalMs);
    },
    requestNow() {
      if (!started || stopped) return;
      run();
    },
    observeTerminals(terminals) {
      if (stopped || !terminalId) return;
      const terminal = terminals[terminalId];
      if (terminal?.state === "live" || terminal?.state === "detached") {
        terminalWasObserved = true;
        return;
      }
      if (terminal?.state === "exited" || terminal?.state === "error") {
        run();
        return;
      }
      if (!terminal && terminalWasObserved) run();
    },
    stop() {
      if (stopped) return;
      stopped = true;
      trailingRun = false;
      if (timer !== undefined) globalThis.clearInterval(timer);
      timer = undefined;
    },
  };
}

/**
 * Resolve an already-live legacy terminal for display fallback only. Runtime
 * creation and recovery always belong to backend reconciliation. Precedence:
 *   1. the persisted orchestrator, if it is still a live terminal - keep it;
 *   2. else a live terminal whose cwd is the orchestrator home - adopt it;
 *   3. else null - leave it for the backend result to populate.
 * Returns the id to DESIGNATE, or null to make no change. Pure + idempotent:
 * a second call with the same inputs designates the same id.
 */
export function resolveOrchestrator(
  orchestratorId: string | null,
  terminals: Record<string, OrchestratorTerminal>,
): string | null {
  if (orchestratorId && terminals[orchestratorId]) return orchestratorId;
  const match = Object.entries(terminals).find(([, t]) =>
    isOrchestratorCwd(t.cwd),
  );
  return match ? match[0] : null;
}
