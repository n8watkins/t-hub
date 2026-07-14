// Lock the exact Claude/Codex commands, least-privilege capability defaults, and
// the confirmation boundary around a control-capable Captain Codex spawn.
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
vi.mock("../ipc/projects", () => ({
  listProjects: vi.fn().mockResolvedValue({ projects: [], count: 0, seq: 0 }),
  listPowderBoards: vi.fn().mockResolvedValue({
    connectionProfile: "default",
    boards: [],
    count: 0,
    totalCount: 0,
    hasMore: false,
  }),
  registerProject: vi.fn(),
  bindProjectPowder: vi.fn(),
  commissionCaptain: vi.fn(),
}));
import {
  SpawnMenu,
  PRESETS,
  CLAUDE_RESUME_CMD,
  CODEX_CMD,
  CODEX_RESUME_CMD,
} from "./SpawnMenu";

const byKey = (key: string) => PRESETS.find((p) => p.key === key);

describe("SpawnMenu presets", () => {
  it("keeps the exact Claude resume preset (no-regression guard)", () => {
    expect(CLAUDE_RESUME_CMD).toBe("claude --resume");
    const resume = byKey("resume");
    expect(resume).toBeDefined();
    expect(resume?.label).toBe("Resume Claude…");
    expect(resume?.options).toEqual({ startupCommand: "claude --resume" });
  });

  it("keeps the plain Shell preset with no startup command", () => {
    const shell = byKey("shell");
    expect(shell).toBeDefined();
    expect(shell?.options).toEqual({});
  });

  it("adds a fresh Codex preset", () => {
    expect(CODEX_CMD).toBe("codex");
    const codex = byKey("codex");
    expect(codex).toBeDefined();
    expect(codex?.label).toBe("Codex");
    expect(codex?.options).toEqual({ startupCommand: "codex" });
  });

  it("adds a Resume Codex picker preset symmetric with Claude", () => {
    expect(CODEX_RESUME_CMD).toBe("codex resume");
    const resumeCodex = byKey("resume-codex");
    expect(resumeCodex).toBeDefined();
    expect(resumeCodex?.label).toBe("Resume Codex…");
    expect(resumeCodex?.options).toEqual({ startupCommand: "codex resume" });
  });

  it("keeps ordinary Codex read-only and routes Captain through commissioning", () => {
    expect(byKey("codex")?.options.capability).toBeUndefined();
    expect(byKey("captain-codex")?.options).toEqual({});
    expect(byKey("captain-codex")?.commission).toBe(true);
  });

  it("has unique preset keys", () => {
    const keys = PRESETS.map((p) => p.key);
    expect(new Set(keys).size).toBe(keys.length);
  });
});

describe("SpawnMenu Captain commissioning", () => {
  it("spawns ordinary Codex immediately without elevation", () => {
    const onSpawn = vi.fn();
    const onClose = vi.fn();
    render(<SpawnMenu onSpawn={onSpawn} onClose={onClose} />);

    fireEvent.click(screen.getByRole("menuitem", { name: /Codex New terminal/i }));

    expect(onSpawn).toHaveBeenCalledWith(
      expect.objectContaining({ options: { startupCommand: "codex" } }),
    );
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("opens codebase-aware creation instead of emitting a generic spawn", async () => {
    const onSpawn = vi.fn();
    const onClose = vi.fn();
    render(<SpawnMenu onSpawn={onSpawn} onClose={onClose} />);

    fireEvent.click(screen.getByRole("menuitem", { name: /Captain Create a Codex/i }));
    expect(onSpawn).not.toHaveBeenCalled();
    expect(await screen.findByRole("dialog", { name: "Create Captain" })).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
  });
});
