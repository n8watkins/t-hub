import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { WebPreview } from "./WebPreview";
import { reachablePreviewUrl } from "../ipc/devserver";
import { popOutPreview } from "../store/preview";

vi.mock("../ipc/devserver", () => ({
  reachablePreviewUrl: vi.fn((url: string) => Promise.resolve(url)),
  probePreviewReachable: vi.fn(() => Promise.resolve(true)),
}));

vi.mock("@tauri-apps/plugin-shell", () => ({
  open: vi.fn(() => Promise.resolve()),
}));

vi.mock("../store/preview", () => ({
  popOutPreview: vi.fn(() => Promise.resolve()),
}));

beforeEach(() => {
  vi.clearAllMocks();
});

describe("WebPreview navigation boundary", () => {
  it("does not create a frame without a managed or explicit URL", () => {
    render(<WebPreview />);

    expect(screen.queryByTitle("Web preview")).toBeNull();
    expect(screen.getByText("Nothing to preview yet")).toBeTruthy();
    expect(reachablePreviewUrl).not.toHaveBeenCalled();
  });

  it("loads a URL only after explicit navigation", async () => {
    const onNavigate = vi.fn();
    render(<WebPreview onNavigate={onNavigate} />);

    fireEvent.change(screen.getByPlaceholderText("http://localhost:3000"), {
      target: { value: "http://127.0.0.1:9223/json/list" },
    });
    fireEvent.click(screen.getByTitle("Load this URL"));

    await waitFor(() => {
      expect(screen.getByTitle("Web preview").getAttribute("src")).toBe(
        "http://127.0.0.1:9223/json/list",
      );
    });
    expect(onNavigate).toHaveBeenCalledWith(
      "http://127.0.0.1:9223/json/list",
    );
  });

  it("adopts a URL reported by the managed runner", async () => {
    render(
      <WebPreview
        initialUrl="http://localhost:5173"
        initialUrlProvenance="managed"
      />,
    );

    await waitFor(() => {
      expect(screen.getByTitle("Web preview").getAttribute("src")).toBe(
        "http://localhost:5173",
      );
    });
    expect(reachablePreviewUrl).not.toHaveBeenCalled();
  });

  it("retains compatibility resolution for a manual initial URL", async () => {
    render(
      <WebPreview
        initialUrl="http://localhost:4173"
        initialUrlProvenance="manual"
      />,
    );

    await waitFor(() => {
      expect(screen.getByTitle("Web preview").getAttribute("src")).toBe(
        "http://localhost:4173",
      );
    });
    expect(reachablePreviewUrl).toHaveBeenCalledWith("http://localhost:4173");
  });

  it("opens a managed URL externally without rewriting backend authority", async () => {
    render(
      <WebPreview
        initialUrl="http://172.23.64.3:5173/app"
        initialUrlProvenance="managed"
      />,
    );

    await waitFor(() => {
      expect(screen.getByTitle("Web preview")).toBeTruthy();
    });
    vi.mocked(reachablePreviewUrl).mockClear();
    fireEvent.click(screen.getByLabelText("Open externally in browser"));

    await waitFor(() => {
      expect(shellOpen).toHaveBeenCalledWith("http://172.23.64.3:5173/app");
    });
    expect(reachablePreviewUrl).not.toHaveBeenCalled();
  });

  it("pops out a managed URL without rewriting backend authority", async () => {
    render(
      <WebPreview
        initialUrl="http://172.23.64.3:4173/app"
        initialUrlProvenance="managed"
      />,
    );

    await waitFor(() => {
      expect(screen.getByTitle("Web preview")).toBeTruthy();
    });
    vi.mocked(reachablePreviewUrl).mockClear();
    fireEvent.click(screen.getByLabelText("Pop out preview"));

    await waitFor(() => {
      expect(popOutPreview).toHaveBeenCalledWith(
        "http://172.23.64.3:4173/app",
      );
    });
    expect(reachablePreviewUrl).not.toHaveBeenCalled();
  });

  it("rewrites a manual URL before external browser and pop-out actions", async () => {
    vi.mocked(reachablePreviewUrl).mockResolvedValue(
      "http://172.23.64.2:3000/app",
    );
    render(
      <WebPreview
        initialUrl="http://localhost:3000/app"
        initialUrlProvenance="manual"
      />,
    );

    await waitFor(() => {
      expect(screen.getByTitle("Web preview").getAttribute("src")).toBe(
        "http://172.23.64.2:3000/app",
      );
    });
    vi.mocked(reachablePreviewUrl).mockClear();
    fireEvent.click(screen.getByLabelText("Open externally in browser"));
    fireEvent.click(screen.getByLabelText("Pop out preview"));

    await waitFor(() => {
      expect(shellOpen).toHaveBeenCalledWith("http://172.23.64.2:3000/app");
      expect(popOutPreview).toHaveBeenCalledWith(
        "http://172.23.64.2:3000/app",
      );
    });
    expect(reachablePreviewUrl).toHaveBeenCalledTimes(2);
    expect(reachablePreviewUrl).toHaveBeenNthCalledWith(
      1,
      "http://localhost:3000/app",
    );
    expect(reachablePreviewUrl).toHaveBeenNthCalledWith(
      2,
      "http://localhost:3000/app",
    );
  });
});
