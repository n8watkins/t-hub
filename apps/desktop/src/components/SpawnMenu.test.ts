// Codex Phase-1 D2: the spawn presets gain a fresh `codex` and a `codex resume`
// picker, symmetric with the Claude entries. These tests lock the exact preset
// commands - including a bypass-would-fail NO-REGRESSION guard on the existing
// `claude --resume` preset (per the plan test bar).
import { describe, expect, it } from "vitest";
import {
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
    expect(resume?.command).toBe("claude --resume");
  });

  it("keeps the plain Shell preset with no startup command", () => {
    const shell = byKey("shell");
    expect(shell).toBeDefined();
    expect(shell?.command).toBeUndefined();
  });

  it("adds a fresh Codex preset", () => {
    expect(CODEX_CMD).toBe("codex");
    const codex = byKey("codex");
    expect(codex).toBeDefined();
    expect(codex?.label).toBe("Codex");
    expect(codex?.command).toBe("codex");
  });

  it("adds a Resume Codex picker preset symmetric with Claude", () => {
    expect(CODEX_RESUME_CMD).toBe("codex resume");
    const resumeCodex = byKey("resume-codex");
    expect(resumeCodex).toBeDefined();
    expect(resumeCodex?.label).toBe("Resume Codex…");
    expect(resumeCodex?.command).toBe("codex resume");
  });

  it("has unique preset keys", () => {
    const keys = PRESETS.map((p) => p.key);
    expect(new Set(keys).size).toBe(keys.length);
  });
});
