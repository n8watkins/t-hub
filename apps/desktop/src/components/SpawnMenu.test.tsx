// Lock the exact Claude/Codex commands, least-privilege capability defaults, and
// the confirmation boundary around a control-capable Captain Codex spawn.
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
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

  it("keeps ordinary Codex read-only and makes Captain Codex explicitly control-capable", () => {
    expect(byKey("codex")?.options.capability).toBeUndefined();
    expect(byKey("captain-codex")?.options).toEqual({
      startupCommand: "codex",
      capability: "control",
    });
  });

  it("has unique preset keys", () => {
    const keys = PRESETS.map((p) => p.key);
    expect(new Set(keys).size).toBe(keys.length);
  });
});

describe("SpawnMenu control confirmation", () => {
  it("spawns ordinary Codex immediately without elevation", () => {
    const onSpawn = vi.fn();
    const onClose = vi.fn();
    render(<SpawnMenu onSpawn={onSpawn} onClose={onClose} />);

    fireEvent.click(screen.getByRole("menuitem", { name: /Codex New terminal/i }));

    expect(onSpawn).toHaveBeenCalledWith({ startupCommand: "codex" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("requires confirmation before emitting a control-capable spawn", () => {
    const onSpawn = vi.fn();
    const onClose = vi.fn();
    render(<SpawnMenu onSpawn={onSpawn} onClose={onClose} />);

    fireEvent.click(screen.getByRole("menuitem", { name: /Captain Codex/i }));
    expect(onSpawn).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog", { name: "Start Captain Codex?" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Start Captain Codex" }));
    expect(onSpawn).toHaveBeenCalledWith({
      startupCommand: "codex",
      capability: "control",
    });
    expect(onClose).toHaveBeenCalledOnce();
  });
});
