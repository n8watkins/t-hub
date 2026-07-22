import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HistoryEntry, HistoryListResult } from "../ipc/history";
import { useWorkspace } from "../store/workspace";

const mocks = vi.hoisted(() => ({
  historyList: vi.fn(),
  historyResume: vi.fn(),
  historyFocus: vi.fn(),
  controlEvents: new Map<string, (payload: unknown) => void>(),
}));

vi.mock("../ipc/history", async () => {
  const actual = await vi.importActual<typeof import("../ipc/history")>("../ipc/history");
  return {
    ...actual,
    historyList: mocks.historyList,
    historyResume: mocks.historyResume,
    historyFocus: mocks.historyFocus,
  };
});

vi.mock("../ipc/controlClient", () => ({
  onControlEvent: vi.fn((channel: string, callback: (payload: unknown) => void) => {
    mocks.controlEvents.set(channel, callback);
    return () => mocks.controlEvents.delete(channel);
  }),
  isRetryableControlError: (reason: unknown) =>
    typeof reason === "object" &&
    reason !== null &&
    "retryable" in reason &&
    reason.retryable === true,
}));

vi.mock("../lib/windowInteraction", () => ({
  runWhenIdle: (callback: () => void) => callback(),
}));

import { HistoryList } from "./HistoryList";

function entry(
  historyId: string,
  harness: "claude" | "codex",
  continuityState: HistoryEntry["continuityState"],
  label: string,
): HistoryEntry {
  const active = continuityState === "active";
  return {
    historyId,
    harness,
    provider: harness === "codex" ? "openai" : null,
    providerSessionId: `${historyId}-native`,
    conversationId: `${historyId}-native`,
    cwd: "/same/repository",
    projectId: null,
    projectName: null,
    captainId: null,
    role: null,
    workspaceId: null,
    worktreeId: null,
    branch: null,
    label,
    lastText: `${label} last message`,
    startedAt: "2026-07-20T00:00:00Z",
    lastSeenAt: "2026-07-20T00:01:00Z",
    continuityState,
    actions: {
      focus: { status: active ? "supported" : "unavailable", reason: null },
      resume: { status: active ? "unavailable" : "supported", reason: null },
      recover: { status: "unavailable", reason: null },
      archive: { status: "unavailable", reason: null },
      unarchive: { status: "unavailable", reason: null },
    },
  };
}

function result(entries: HistoryEntry[]): HistoryListResult {
  return {
    schemaVersion: 1,
    generatedAt: "2026-07-20T00:02:00Z",
    revision: entries.map((item) => item.historyId).join(":"),
    entries,
    count: entries.length,
    total: entries.length,
    truncated: false,
    sources: [
      { harness: "claude", status: "ready", reason: null },
      { harness: "codex", status: "ready", reason: null },
    ],
  };
}

beforeEach(() => {
  mocks.historyList.mockReset();
  mocks.historyResume.mockReset();
  mocks.historyFocus.mockReset();
  mocks.controlEvents.clear();
  localStorage.clear();
  useWorkspace.setState({ activeTabId: "workspace-one" });
});

describe("HistoryList", () => {
  it("renders distinct same-cwd Claude and Codex rows with correct lifecycle actions", async () => {
    const closed = entry("history-codex", "codex", "resumable", "Closed Codex");
    const active = entry("history-claude", "claude", "active", "Active Claude");
    mocks.historyList.mockResolvedValue(result([closed, active]));
    mocks.historyResume.mockResolvedValue({ status: "active" });
    mocks.historyFocus.mockResolvedValue({ status: "focused" });
    localStorage.setItem("th.recent.cache.v1", "legacy");
    localStorage.setItem("th.recent.hidden.v2", "legacy");

    const onCount = vi.fn();
    const { container } = render(<HistoryList onCount={onCount} />);

    expect(await screen.findByText("Closed Codex")).toBeTruthy();
    expect(screen.getByText("Active Claude")).toBeTruthy();
    expect(container.querySelectorAll("[data-history-id]")).toHaveLength(2);
    expect(
      container
        .querySelector('[data-history-id="history-codex"]')
        ?.getAttribute("data-continuity-state"),
    ).toBe("resumable");
    expect(
      container
        .querySelector('[data-history-id="history-claude"]')
        ?.getAttribute("data-continuity-state"),
    ).toBe("active");
    await waitFor(() => expect(onCount).toHaveBeenLastCalledWith(2));
    expect(localStorage.getItem("th.recent.cache.v1")).toBeNull();
    expect(localStorage.getItem("th.recent.hidden.v2")).toBeNull();

    fireEvent.click(
      screen.getByRole("button", { name: "Resume conversation: Closed Codex" }),
    );
    await waitFor(() => expect(mocks.historyResume).toHaveBeenCalledOnce());
    expect(mocks.historyResume).toHaveBeenCalledWith(
      "history-codex",
      expect.any(String),
      "workspace-one",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Focus conversation: Active Claude" }),
    );
    await waitFor(() =>
      expect(mocks.historyFocus).toHaveBeenCalledWith("history-claude"),
    );
  });

  it("retains one request ID across an ambiguous resume retry", async () => {
    const closed = entry("history-codex", "codex", "resumable", "Retry Codex");
    mocks.historyList.mockResolvedValue(result([closed]));
    mocks.historyResume
      .mockRejectedValueOnce(new Error("control_timeout: ambiguous response"))
      .mockResolvedValueOnce({ status: "active" });

    render(<HistoryList />);
    const button = await screen.findByRole("button", {
      name: "Resume conversation: Retry Codex",
    });
    fireEvent.click(button);
    expect(await screen.findByText("Action failed. Retry safely.")).toBeTruthy();
    fireEvent.click(button);

    await waitFor(() => expect(mocks.historyResume).toHaveBeenCalledTimes(2));
    expect(mocks.historyResume.mock.calls[0][1]).toBe(
      mocks.historyResume.mock.calls[1][1],
    );
  });

  it("retains one request ID across durable backend recovery", async () => {
    const closed = entry("history-recovery", "codex", "resumable", "Recover Codex");
    mocks.historyList.mockResolvedValue(result([closed]));
    mocks.historyResume
      .mockRejectedValueOnce(
        new Error("history_recovery_required: reserved terminal liveness is unknown"),
      )
      .mockResolvedValueOnce({ status: "active" });

    render(<HistoryList />);
    const button = await screen.findByRole("button", {
      name: "Resume conversation: Recover Codex",
    });
    fireEvent.click(button);
    expect(await screen.findByText("Action failed. Retry safely.")).toBeTruthy();
    fireEvent.click(button);

    await waitFor(() => expect(mocks.historyResume).toHaveBeenCalledTimes(2));
    expect(mocks.historyResume.mock.calls[0][1]).toBe(
      mocks.historyResume.mock.calls[1][1],
    );
  });

  it("retains one request ID for a structurally retryable placement failure", async () => {
    const closed = entry("history-placement", "codex", "resumable", "Place Codex");
    mocks.historyList.mockResolvedValue(result([closed]));
    const retryable = Object.assign(
      new Error("history_resume_failed: spawned terminal has no Workspace placement"),
      { retryable: true },
    );
    mocks.historyResume.mockRejectedValueOnce(retryable).mockResolvedValueOnce({ status: "active" });

    render(<HistoryList />);
    const button = await screen.findByRole("button", {
      name: "Resume conversation: Place Codex",
    });
    fireEvent.click(button);
    expect(await screen.findByText("Action failed. Retry safely.")).toBeTruthy();
    fireEvent.click(button);

    await waitFor(() => expect(mocks.historyResume).toHaveBeenCalledTimes(2));
    expect(mocks.historyResume.mock.calls[0][1]).toBe(
      mocks.historyResume.mock.calls[1][1],
    );
  });

  it("rotates the request ID after an authoritative resume refusal", async () => {
    const closed = entry("history-codex", "codex", "resumable", "Rotate Codex");
    mocks.historyList.mockResolvedValue(result([closed]));
    mocks.historyResume
      .mockRejectedValueOnce(new Error("history_unavailable: authoritative refusal"))
      .mockResolvedValueOnce({ status: "active" });

    render(<HistoryList />);
    const button = await screen.findByRole("button", {
      name: "Resume conversation: Rotate Codex",
    });
    fireEvent.click(button);
    expect(await screen.findByText("Action failed. Retry safely.")).toBeTruthy();
    fireEvent.click(button);

    await waitFor(() => expect(mocks.historyResume).toHaveBeenCalledTimes(2));
    expect(mocks.historyResume.mock.calls[0][1]).not.toBe(
      mocks.historyResume.mock.calls[1][1],
    );
  });

  it("does not duplicate a resume on a rapid double click", async () => {
    const closed = entry("history-codex", "codex", "resumable", "One Codex");
    mocks.historyList.mockResolvedValue(result([closed]));
    let finishResume: (() => void) | undefined;
    mocks.historyResume.mockImplementation(
      () =>
        new Promise((resolve) => {
          finishResume = () => resolve({ status: "active" });
        }),
    );

    render(<HistoryList />);
    const button = await screen.findByRole("button", {
      name: "Resume conversation: One Codex",
    });
    fireEvent.click(button);
    fireEvent.click(button);

    expect(mocks.historyResume).toHaveBeenCalledOnce();
    await act(async () => {
      finishResume?.();
      await Promise.resolve();
    });
  });

  it("replaces an active row with one resumable row after a close refresh", async () => {
    const active = entry("history-codex", "codex", "active", "Transition Codex");
    const closed = entry("history-codex", "codex", "resumable", "Transition Codex");
    mocks.historyList
      .mockResolvedValueOnce(result([active]))
      .mockResolvedValueOnce({ ...result([closed]), revision: "closed" });

    const { container } = render(<HistoryList />);
    expect(
      await screen.findByRole("button", {
        name: "Focus conversation: Transition Codex",
      }),
    ).toBeTruthy();

    window.dispatchEvent(new Event("t-hub:history-changed"));

    expect(
      await screen.findByRole("button", {
        name: "Resume conversation: Transition Codex",
      }),
    ).toBeTruthy();
    expect(container.querySelectorAll('[data-history-id="history-codex"]')).toHaveLength(1);
  });

  it("refreshes when the backend broadcasts a History change", async () => {
    const first = entry("history-first", "codex", "resumable", "First Codex");
    const second = entry("history-second", "claude", "resumable", "Second Claude");
    mocks.historyList
      .mockResolvedValueOnce(result([first]))
      .mockResolvedValueOnce(result([second]));

    render(<HistoryList />);
    expect(await screen.findByText("First Codex")).toBeTruthy();
    const notify = mocks.controlEvents.get("history://changed");
    expect(notify).toBeTruthy();
    act(() => notify?.({ reason: "terminal-closed" }));

    expect(await screen.findByText("Second Claude")).toBeTruthy();
    expect(screen.queryByText("First Codex")).toBeNull();
  });

  it("refreshes on SessionEnd without polling ordinary completed turns", async () => {
    const row = entry("history-session", "claude", "active", "Session Claude");
    mocks.historyList.mockResolvedValue(result([row]));

    render(<HistoryList />);
    expect(await screen.findByText("Session Claude")).toBeTruthy();
    const notify = mocks.controlEvents.get("session://status");
    expect(notify).toBeTruthy();

    act(() => notify?.({ sessionId: "native-id", status: "completed" }));
    await act(async () => Promise.resolve());
    expect(mocks.historyList).toHaveBeenCalledOnce();

    const notifyJournal = mocks.controlEvents.get("agent://journal");
    expect(notifyJournal).toBeTruthy();
    act(() =>
      notifyJournal?.({
        entry: {
          seq: 1,
          timestamp_ms: 1,
          source: "hook",
          entity_id: "native-id",
          event_type: "sessionEnd",
          payload: {},
        },
      }),
    );
    await waitFor(() => expect(mocks.historyList).toHaveBeenCalledTimes(2));
  });

  it("keeps healthy entries visible when one provider source is degraded", async () => {
    const healthy = entry("history-claude", "claude", "resumable", "Healthy Claude");
    const partial = result([healthy]);
    partial.sources[1] = {
      harness: "codex",
      status: "degraded",
      reason: "Skipped malformed rollout",
    };
    partial.truncated = true;
    mocks.historyList.mockResolvedValue(partial);

    render(<HistoryList />);

    expect(await screen.findByText("Healthy Claude")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("codex");
  });
});
