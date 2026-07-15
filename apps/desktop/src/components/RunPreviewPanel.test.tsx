import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { useEffect, useState } from "react";
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
    const [url, setUrl] = useState(props.initialUrl);
    useEffect(() => {
      if (props.initialUrl) setUrl(props.initialUrl);
    }, [props.initialUrl]);
    previewProps.push({
      ...props,
      onNavigate: (next: string) => {
        setUrl(next);
        props.onNavigate?.(next);
      },
    });
    return <div data-testid="preview">{url ?? "empty"}</div>;
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

  it("does not persist a managed URL and clears its frame when the run stops", () => {
    usePanels.setState({
      devUrl: { "terminal-1": "http://127.0.0.1:43123/" },
      previewUrl: { "terminal-1": "http://127.0.0.1:43123/" },
    });
    render(<RunPreviewPanel terminalId="terminal-1" cwd="/static" />);

    act(() => {
      previewProps.at(-1)?.onNavigate?.("http://127.0.0.1:43123/");
    });
    expect(usePanels.getState().previewUrl["terminal-1"]).toBeNull();

    act(() => usePanels.getState().setDevUrl("terminal-1", null));

    expect(screen.getByTestId("preview").textContent).toBe("empty");
  });
});
