// Codex Phase-1 D2: the spawn presets gain a fresh `codex` and a `codex resume`
// picker, symmetric with the Claude entries. These tests lock the exact preset
// commands - including a bypass-would-fail NO-REGRESSION guard on the existing
// `claude --resume` preset (per the plan test bar).
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createElement } from "react";
import {
  PRESETS,
  CLAUDE_RESUME_CMD,
  CODEX_CMD,
  CODEX_RESUME_CMD,
  SpawnMenu,
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

// item-3 §2.1.1: the audited control-capability opt-in. Inverted least-privilege
// is ratified, so a preset picked with the toggle OFF must NEVER pass a capability
// (defaults read); it passes `"control"` ONLY when the toggle is explicitly armed.
// These are bypass-would-fail guards on that inversion.
describe("SpawnMenu control-capability toggle", () => {
  afterEach(() => cleanup());

  function open() {
    const onSpawn = vi.fn();
    const onClose = vi.fn();
    render(
      createElement(SpawnMenu, { onSpawn, onClose, busy: false }),
    );
    return { onSpawn, onClose };
  }

  it("defaults to read: picking a preset passes NO capability (undefined)", () => {
    const { onSpawn } = open();
    fireEvent.click(screen.getByText("Shell"));
    expect(onSpawn).toHaveBeenCalledTimes(1);
    // (startupCommand, capability) — Shell has no command; capability undefined.
    expect(onSpawn).toHaveBeenCalledWith(undefined, undefined);
  });

  it("passes an undefined capability for a claude preset when not armed", () => {
    const { onSpawn } = open();
    fireEvent.click(screen.getByText("Resume Claude…"));
    expect(onSpawn).toHaveBeenCalledWith(CLAUDE_RESUME_CMD, undefined);
  });

  it("arms control: after toggling, picking a preset passes capability:'control'", () => {
    const { onSpawn } = open();
    const toggle = screen.getByRole("menuitemcheckbox", {
      name: "Spawn as control terminal",
    });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByText("Codex"));
    expect(onSpawn).toHaveBeenCalledWith(CODEX_CMD, "control");
  });

  it("re-toggling control back off returns to the read default", () => {
    const { onSpawn } = open();
    const toggle = screen.getByRole("menuitemcheckbox", {
      name: "Spawn as control terminal",
    });
    fireEvent.click(toggle); // on
    fireEvent.click(toggle); // off again
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(screen.getByText("Shell"));
    expect(onSpawn).toHaveBeenCalledWith(undefined, undefined);
  });

  it("the busy gate blocks a pick (no spawn stacked) but the toggle still arms", () => {
    const onSpawn = vi.fn();
    render(
      createElement(SpawnMenu, { onSpawn, onClose: vi.fn(), busy: true }),
    );
    fireEvent.click(screen.getByText("Shell"));
    expect(onSpawn).not.toHaveBeenCalled();
  });
});
