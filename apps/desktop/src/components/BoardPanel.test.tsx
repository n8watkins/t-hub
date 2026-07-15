import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { BoardPanel } from "./BoardPanel";
import { projectBoardSnapshot } from "../ipc/projects";
import { open } from "@tauri-apps/plugin-shell";

vi.mock("../ipc/projects", () => ({ projectBoardSnapshot: vi.fn() }));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));

const readySnapshot = {
  schemaVersion: 1 as const,
  status: "ready" as const,
  resolution: "captain" as const,
  project: {
    projectId: "project-1",
    name: "T-Hub",
    repoRoot: "/repo/t-hub",
  },
  binding: { repository: "t-hub", connectionProfile: "production" },
  board: {
    repository: {
      name: "t-hub",
      aliases: [],
      tier: "active",
      cardCount: 1,
    },
    cards: [
      {
        id: "t-hub-1",
        title: "Repair Board",
        status: "running",
        priority: "p1",
        estimate: "m",
        labels: ["desktop"],
        claim: { agent: "crew-one", expiresAt: 2000000000 },
        updatedAt: 123,
      },
    ],
    totalCount: 1,
    hasMore: false,
    refreshedAt: 123,
  },
  external: {
    url: "https://powder.example.test/board",
    repositoryFilterApplied: false as const,
  },
};

beforeEach(() => {
  vi.clearAllMocks();
});

describe("BoardPanel", () => {
  it("renders only the backend-scoped Project cards", async () => {
    vi.mocked(projectBoardSnapshot).mockResolvedValue(readySnapshot);

    render(<BoardPanel terminalId="terminal-1" cwd="/repo/t-hub" />);

    expect(screen.getByRole("status").textContent).toContain("Loading");
    expect(await screen.findByText("Repair Board")).toBeTruthy();
    expect(screen.getByRole("region", { name: "Project Board" })).toBeTruthy();
    expect(screen.getByText("t-hub-1")).toBeTruthy();
    expect(screen.getByText("Claimed by crew-one")).toBeTruthy();
    expect(projectBoardSnapshot).toHaveBeenCalledWith({
      terminalId: "terminal-1",
      cwd: "/repo/t-hub",
      limit: 1000,
    });
    expect(document.querySelector("iframe")).toBeNull();
  });

  it("shows an honest unbound state without a manual URL", async () => {
    vi.mocked(projectBoardSnapshot).mockResolvedValue({
      schemaVersion: 1,
      status: "unbound",
      resolution: "cwd",
      project: readySnapshot.project,
      problem: {
        code: "powder_not_bound",
        message: "This Project does not have a Powder board binding.",
        retryable: false,
      },
    });

    render(<BoardPanel terminalId="terminal-1" cwd="/repo/t-hub" />);

    expect(await screen.findByText("No Powder board is bound")).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByText("Open full Powder board")).toBeNull();
  });

  it("retries a transient failure and opens only the full board externally", async () => {
    vi.mocked(projectBoardSnapshot)
      .mockResolvedValueOnce({
        schemaVersion: 1,
        status: "unreachable",
        resolution: "captain",
        project: readySnapshot.project,
        binding: readySnapshot.binding,
        external: readySnapshot.external,
        problem: {
          code: "powder_unreachable",
          message: "The Powder service is unreachable.",
          retryable: true,
        },
      })
      .mockResolvedValueOnce(readySnapshot);
    vi.mocked(open).mockResolvedValue(undefined);

    render(<BoardPanel terminalId="terminal-1" cwd="/repo/t-hub" />);

    fireEvent.click(await screen.findByRole("button", { name: "Open full Powder board" }));
    expect(open).toHaveBeenCalledWith("https://powder.example.test/board");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(projectBoardSnapshot).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("Repair Board")).toBeTruthy();
  });

  it("discards a stale response after the focused terminal changes", async () => {
    let resolveFirst: ((value: typeof readySnapshot) => void) | undefined;
    vi.mocked(projectBoardSnapshot)
      .mockImplementationOnce(
        () => new Promise((resolve) => {
          resolveFirst = resolve;
        }),
      )
      .mockResolvedValueOnce({
        ...readySnapshot,
        board: {
          ...readySnapshot.board,
          cards: [{ ...readySnapshot.board.cards[0], id: "new-2", title: "New terminal card" }],
        },
      });
    const { rerender } = render(
      <BoardPanel terminalId="old-terminal" cwd="/repo/old" />,
    );
    rerender(<BoardPanel terminalId="new-terminal" cwd="/repo/new" />);
    expect(await screen.findByText("New terminal card")).toBeTruthy();
    resolveFirst?.(readySnapshot);
    await Promise.resolve();
    expect(screen.queryByText("Repair Board")).toBeNull();
  });
});
