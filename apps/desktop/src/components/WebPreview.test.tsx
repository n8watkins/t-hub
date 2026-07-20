import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { WebPreview } from "./WebPreview";
import { reachablePreviewUrl } from "../ipc/devserver";

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
    render(<WebPreview initialUrl="http://localhost:5173" />);

    await waitFor(() => {
      expect(screen.getByTitle("Web preview").getAttribute("src")).toBe(
        "http://localhost:5173",
      );
    });
    expect(reachablePreviewUrl).toHaveBeenCalledWith("http://localhost:5173");
  });
});
