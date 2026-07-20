import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./Terminal", () => ({
  TerminalView: ({ terminalId }: { terminalId: string }) => (
    <div data-testid={`terminal-${terminalId}`} />
  ),
}));
vi.mock("./CaptainOverlay", () => ({ CaptainOverlay: () => null }));
vi.mock("./TileErrorBoundary", () => ({
  TileErrorBoundary: ({ children }: { children: React.ReactNode }) => children,
}));
vi.mock("../lib/diag", () => ({ tlog: () => {} }));

import { TerminalPoolProvider } from "./TerminalPool";
import { useCaptain } from "../store/captain";
import { usePanels } from "../store/panels";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { TERMINAL_COLD_AFTER_MS } from "../lib/terminalLifecycle";
import { resetTerminalResourcesForTests } from "../lib/terminalResources";

class ResizeObserverStub {
  observe(): void {}
  disconnect(): void {}
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", ResizeObserverStub);
  vi.stubGlobal("requestAnimationFrame", () => 1);
  vi.stubGlobal("cancelAnimationFrame", () => {});
  resetTerminalResourcesForTests();

  const tabs: WorkspaceTab[] = [
    { id: "active", name: "Active", order: ["hot"] },
    { id: "parked", name: "Parked", order: ["warm"] },
  ];
  useWorkspace.setState({
    tabs,
    activeTabId: "active",
    focusedId: "hot",
    terminals: {},
    poppedOutTabs: [],
  });
  usePanels.setState({
    tab: {},
    panelExpanded: {},
    fullscreenId: null,
  });
  useCaptain.setState({
    captainIds: [],
    activeCaptainId: null,
    open: false,
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("TerminalPool lifecycle", () => {
  it("keeps wrappers stable while warm terminals cool and rehydrate", () => {
    const { container } = render(
      <TerminalPoolProvider>
        <div />
      </TerminalPoolProvider>,
    );
    const parkedWrapper = container.querySelector(
      '[data-th-pool-tile="warm"]',
    );

    expect(container.querySelector('[data-testid="terminal-hot"]')).toBeTruthy();
    expect(container.querySelector('[data-testid="terminal-warm"]')).toBeTruthy();

    act(() => vi.advanceTimersByTime(TERMINAL_COLD_AFTER_MS));

    expect(container.querySelector('[data-testid="terminal-hot"]')).toBeTruthy();
    expect(container.querySelector('[data-testid="terminal-warm"]')).toBeNull();
    expect(container.querySelector('[data-th-pool-tile="warm"]')).toBe(
      parkedWrapper,
    );

    act(() => useWorkspace.setState({ activeTabId: "parked", focusedId: "warm" }));

    expect(container.querySelector('[data-testid="terminal-warm"]')).toBeTruthy();
    expect(container.querySelector('[data-th-pool-tile="warm"]')).toBe(
      parkedWrapper,
    );
    expect(container.querySelector('[data-testid="terminal-hot"]')).toBeTruthy();

    act(() => vi.advanceTimersByTime(TERMINAL_COLD_AFTER_MS));

    expect(container.querySelector('[data-testid="terminal-hot"]')).toBeNull();
    expect(container.querySelector('[data-testid="terminal-warm"]')).toBeTruthy();
  });
});
