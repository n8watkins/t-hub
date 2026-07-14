import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { useCaptain } from "./captain";
import {
  clientForTerminal,
  useClientForTerminal,
  useHasCodexSession,
} from "./clientType";
import { useWorkspace } from "./workspace";

beforeEach(() => {
  useWorkspace.setState({
    terminals: {
      agent001: {
        id: "agent001",
        tmuxSession: "th_agent001",
        cwd: "/repo",
        title: "node",
        state: "live",
      },
    },
    labels: { agent001: "Claude review" },
    userLabels: {},
    claudeTitles: { agent001: "Claude review" },
  });
  useCaptain.setState({ claims: {} });
});

describe("authoritative terminal client identity", () => {
  it("classifies node-hosted Codex ahead of stale Claude metadata", () => {
    useCaptain.setState({
      claims: {
        agent001: {
          terminalId: "agent001",
          shipSlug: "ship-agent001",
          provider: "codex",
          harness: "codex",
          workspaceTabIds: [],
          crew: [],
        },
      },
    });

    expect(clientForTerminal("agent001")).toBe("codex");
  });

  it("resolves cross-harness Crew identity by terminal id", () => {
    useCaptain.setState({
      claims: {
        captain1: {
          terminalId: "captain1",
          shipSlug: "ship-captain1",
          provider: "claude",
          harness: "claude",
          workspaceTabIds: [],
          crew: [
            { terminalId: "agent001", provider: "codex", harness: "codex" },
          ],
        },
      },
    });

    expect(clientForTerminal("agent001")).toBe("codex");
  });

  it("preserves heuristic fallback for an unregistered terminal", () => {
    expect(clientForTerminal("agent001")).toBe("claude");
  });

  it("rerenders client and aggregate Codex state when only registry identity changes", () => {
    const client = renderHook(() => useClientForTerminal("agent001"));
    const aggregate = renderHook(() => useHasCodexSession());
    expect(client.result.current).toBe("claude");
    expect(aggregate.result.current).toBe(false);

    act(() => {
      useCaptain.setState({
        claims: {
          agent001: {
            terminalId: "agent001",
            shipSlug: "ship-agent001",
            provider: "codex",
            harness: "codex",
            workspaceTabIds: [],
            crew: [],
          },
        },
      });
    });

    expect(client.result.current).toBe("codex");
    expect(aggregate.result.current).toBe(true);
  });
});
