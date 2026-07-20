import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HistoryEntry, HistoryListResult } from "../ipc/history";
import { useWorkspace } from "../store/workspace";

const mocks = vi.hoisted(() => ({
  historyList: vi.fn(),
  historyResume: vi.fn(),
  historyFocus: vi.fn(),
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
    expect(onCount).toHaveBeenLastCalledWith(2);
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

  it("retains one request ID across a failed resume retry", async () => {
    const closed = entry("history-codex", "codex", "resumable", "Retry Codex");
    mocks.historyList.mockResolvedValue(result([closed]));
    mocks.historyResume
      .mockRejectedValueOnce(new Error("ambiguous response"))
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
