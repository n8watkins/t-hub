import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { usePanels } from "../store/panels";
import { RunPreviewPanel } from "./RunPreviewPanel";

const previewProps: Array<{
  initialUrl?: string;
  onNavigate?: (url: string) => void;
}> = [];

vi.mock("./DevTab", () => ({
  DevTab: ({ terminalId, cwd }: { terminalId: string; cwd: string }) => (
    <div data-testid="runner">{terminalId}:{cwd}</div>
  ),
}));

vi.mock("./WebPreview", () => ({
  WebPreview: (props: {
    initialUrl?: string;
    onNavigate?: (url: string) => void;
  }) => {
    previewProps.push(props);
    return <div data-testid="preview">{props.initialUrl ?? "empty"}</div>;
  },
}));

beforeEach(() => {
  previewProps.length = 0;
  usePanels.setState({ devUrl: {}, previewUrl: {} });
});

describe("RunPreviewPanel", () => {
  it("keeps the managed runner and its preview in one surface", () => {
    usePanels.setState({
      devUrl: { "terminal-1": "http://localhost:5173" },
      previewUrl: { "terminal-1": "http://localhost:3000" },
    });

    render(<RunPreviewPanel terminalId="terminal-1" cwd="/repo/t-hub" />);

    expect(screen.getByRole("region", { name: "Run and Preview" })).toBeTruthy();
    expect(screen.getByTestId("runner").textContent).toBe("terminal-1:/repo/t-hub");
    expect(screen.getByTestId("preview").textContent).toBe("http://localhost:5173");
    expect(previewProps.at(-1)).not.toHaveProperty("detectedUrls");
  });

  it("persists a manual preview URL without treating terminal output as input", () => {
    render(<RunPreviewPanel terminalId="terminal-1" cwd="/repo/t-hub" />);

    act(() => {
      previewProps.at(-1)?.onNavigate?.("http://localhost:4173");
    });

    expect(usePanels.getState().previewUrl["terminal-1"]).toBe(
      "http://localhost:4173",
    );
  });
});
