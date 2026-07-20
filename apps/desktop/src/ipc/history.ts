import { controlRequest, isRetryableControlError } from "./controlClient";

export type HistoryHarness = "claude" | "codex";
export type HistoryContinuityState =
  | "active"
  | "resumable"
  | "archived"
  | "recoveryRequired";
export type HistoryActionStatus =
  | "supported"
  | "unavailable"
  | "incompatible"
  | "unknown";

export interface HistoryActionCompatibility {
  status: HistoryActionStatus;
  reason: string | null;
}

export interface HistoryActions {
  focus: HistoryActionCompatibility;
  resume: HistoryActionCompatibility;
  recover: HistoryActionCompatibility;
  archive: HistoryActionCompatibility;
  unarchive: HistoryActionCompatibility;
}

export interface HistoryEntry {
  historyId: string;
  harness: HistoryHarness;
  provider: string | null;
  providerSessionId: string | null;
  conversationId: string;
  cwd: string;
  projectId: string | null;
  projectName: string | null;
  captainId: string | null;
  role: string | null;
  workspaceId: string | null;
  worktreeId: string | null;
  branch: string | null;
  label: string;
  lastText: string | null;
  startedAt: string | null;
  lastSeenAt: string;
  continuityState: HistoryContinuityState;
  actions: HistoryActions;
}

export interface HistorySource {
  harness: HistoryHarness;
  status: "ready" | "degraded" | "unavailable";
  reason: string | null;
}

export interface HistoryListResult {
  schemaVersion: 1;
  generatedAt: string;
  revision: string;
  entries: HistoryEntry[];
  count: number;
  total: number;
  truncated: boolean;
  sources: HistorySource[];
}

export interface HistoryResumeResult {
  accepted: "history_resume";
  requestId: string;
  historyId: string;
  harness: HistoryHarness;
  conversationId: string;
  terminalId: string;
  tabId: string | null;
  status: "active";
}

export interface HistoryFocusResult {
  accepted: "history_focus";
  historyId: string;
  terminalId: string;
  status: "focused";
  applied: boolean;
}

export function historyList(limit = 100): Promise<HistoryListResult> {
  return controlRequest("history_list", {
    limit,
    includeArchived: true,
  }) as Promise<HistoryListResult>;
}

export function historyResume(
  historyId: string,
  requestId: string,
  targetTabId?: string,
): Promise<HistoryResumeResult> {
  return controlRequest("history_resume", {
    historyId,
    requestId,
    ...(targetTabId ? { targetTabId } : {}),
  }) as Promise<HistoryResumeResult>;
}

export function historyFocus(historyId: string): Promise<HistoryFocusResult> {
  return controlRequest("history_focus", { historyId }) as Promise<HistoryFocusResult>;
}

export function invalidateHistoryCache(): Promise<boolean> {
  return controlRequest("invalidate_history_cache") as Promise<boolean>;
}

export function isAmbiguousHistoryFailure(reason: unknown): boolean {
  if (isRetryableControlError(reason)) return true;
  const message = reason instanceof Error ? reason.message : String(reason);
  return (
    message.startsWith("control_protocol:") ||
    message.startsWith("control_request:") ||
    message.startsWith("control_timeout:") ||
    message.startsWith("control_unavailable:") ||
    message.startsWith("history_resume_in_flight:") ||
    message.startsWith("history_recovery_required:") ||
    message.startsWith("history_persistence_failed:") ||
    message.includes("is already in flight")
  );
}
