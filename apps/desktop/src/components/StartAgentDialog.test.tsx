import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { controlRequest } from "../ipc/controlClient";
import { gitInfo } from "../ipc/git";
import { StartAgentDialog } from "./StartAgentDialog";

vi.mock("../ipc/controlClient", () => ({ controlRequest: vi.fn() }));
vi.mock("../ipc/git", () => ({ gitInfo: vi.fn() }));

const SOURCE_COMMIT = "a".repeat(40);

describe("StartAgentDialog", () => {
  beforeEach(() => {
    vi.mocked(controlRequest).mockReset().mockResolvedValue({});
    vi.mocked(gitInfo).mockReset().mockResolvedValue({
      isRepo: true,
      branch: "feature/ui",
      worktreeRoot: "/repo/worktrees/ui",
      isLinkedWorktree: true,
      dirtyCount: 0,
      headCommit: SOURCE_COMMIT,
    });
  });

  it("dispatches normalized logical claims with the exact checkout commit", async () => {
    render(
      <StartAgentDialog
        open
        captainSessionId="captain-1"
        directory="/repo/worktrees/ui"
        onClose={vi.fn()}
        onStarted={vi.fn()}
      />,
    );
    await screen.findByText(SOURCE_COMMIT.slice(0, 12));

    fireEvent.change(screen.getByLabelText("Assignment"), {
      target: { value: "Implement the dispatch UI" },
    });
    fireEvent.change(screen.getByLabelText("Lane ID"), { target: { value: "lane.ui" } });
    fireEvent.change(screen.getByLabelText("Dependencies"), {
      target: { value: "lane.backend" },
    });
    fireEvent.change(screen.getByLabelText("Mutable files or directories"), {
      target: { value: "apps\\desktop\\src\napps/desktop/src" },
    });
    fireEvent.change(screen.getByLabelText("Mutable schemas"), {
      target: { value: "captains-v18" },
    });
    fireEvent.change(screen.getByLabelText("Mutable interfaces"), {
      target: { value: "control.dispatch" },
    });
    fireEvent.change(screen.getByLabelText("Integration contracts"), {
      target: { value: "ui-order | integrator.1 | lane.backend, lane.ui" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start agent" }));

    await waitFor(() => expect(controlRequest).toHaveBeenCalledTimes(1));
    expect(controlRequest).toHaveBeenCalledWith(
      "start_agent",
      expect.objectContaining({
        captainSessionId: "captain-1",
        directory: "/repo/worktrees/ui",
        sourceCommit: SOURCE_COMMIT,
        laneId: "lane.ui",
        dependencies: ["lane.backend"],
        mutableFiles: ["apps/desktop/src"],
        mutableSchemas: ["captains-v18"],
        mutableInterfaces: ["control.dispatch"],
        integrationContracts: [
          {
            contractId: "ui-order",
            integrationOwner: "integrator.1",
            orderedLaneIds: ["lane.backend", "lane.ui"],
          },
        ],
      }),
    );
  });

  it("rejects absolute pseudo-file claims before dispatch", async () => {
    render(
      <StartAgentDialog
        open
        captainSessionId="captain-1"
        directory="/repo/worktrees/ui"
        onClose={vi.fn()}
        onStarted={vi.fn()}
      />,
    );
    await screen.findByText(SOURCE_COMMIT.slice(0, 12));
    fireEvent.change(screen.getByLabelText("Assignment"), { target: { value: "Implement UI" } });
    fireEvent.change(screen.getByLabelText("Mutable files or directories"), {
      target: { value: "/repo/worktrees/ui" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start agent" }));

    expect((await screen.findByRole("alert")).textContent).toContain(
      "relative to the repository root",
    );
    expect(controlRequest).not.toHaveBeenCalled();
  });
});
