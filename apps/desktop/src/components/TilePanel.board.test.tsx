import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";

const boardProps: Array<{ terminalId: string; cwd: string }> = [];

vi.mock("./BoardPanel", () => ({
  BoardPanel: (props: { terminalId: string; cwd: string }) => {
    boardProps.push(props);
    return <div data-testid="board-panel">Project Board</div>;
  },
}));
vi.mock("./WebPreview", () => ({ WebPreview: () => null }));
vi.mock("./FilePanel", () => ({ FilePanel: () => null }));
vi.mock("./DevTab", () => ({ DevTab: () => null }));

import { TilePanel } from "./TilePanel";

beforeEach(() => {
  boardProps.length = 0;
});

describe("TilePanel board tab", () => {
  it("uses the focused terminal and cwd for authoritative Project resolution", () => {
    render(
      <TilePanel
        terminalId="terminal-t-hub"
        cwd="/home/tester/projects/t-hub"
        tab="board"
      />,
    );

    expect(screen.getByTestId("board-panel").textContent).toBe("Project Board");
    expect(boardProps.at(-1)).toEqual({
      terminalId: "terminal-t-hub",
      cwd: "/home/tester/projects/t-hub",
    });
  });
});
