import type { SpawnOptions } from "../ipc/types";

/** `claude --resume` lets Claude show its interactive session picker. (`--continue`
 *  / `-c` would silently resume the MOST RECENT session instead; the picker is the
 *  intended "Resume" UX, so we default to `--resume`.) */
export const CLAUDE_RESUME_CMD = "claude --resume";

/** A fresh interactive Codex session (Codex Phase-1 D2). Mirrors the Claude
 *  launch; the crew/headless `codex exec` pipeline is a separate producer path. */
export const CODEX_CMD = "codex";

/** `codex resume` opens Codex's own interactive session picker in a new terminal,
 *  symmetric with `claude --resume` (verified on the pinned Codex 0.142.5). */
export const CODEX_RESUME_CMD = "codex resume";

export interface SpawnSelection {
  options: SpawnOptions;
  pinAsCaptain?: boolean;
}

export interface Preset extends SpawnSelection {
  key: string;
  label: string;
  /** One-line hint shown under the label. */
  hint: string;
  commission?: boolean;
}

export const PRESETS: Preset[] = [
  { key: "shell", label: "Shell", hint: "New login shell in ~", options: {} },
  {
    key: "resume",
    label: "Resume Claude…",
    // Make it unambiguous: a NEW terminal opens, showing Claude's session picker
    // there so the user chooses which session to resume (it doesn't resume all).
    hint: "New terminal → pick a session to resume",
    options: { startupCommand: CLAUDE_RESUME_CMD, providerIntent: "claude" },
  },
  {
    key: "codex",
    label: "Codex",
    hint: "New terminal → fresh Codex session",
    options: { startupCommand: CODEX_CMD, providerIntent: "codex" },
  },
  {
    key: "captain-codex",
    label: "Captain",
    hint: "Create a Codex or Claude Captain for a codebase",
    options: {},
    commission: true,
  },
  {
    key: "resume-codex",
    label: "Resume Codex…",
    // Symmetric with Resume Claude: opens Codex's own session picker in a new tile.
    hint: "New terminal → pick a Codex session to resume",
    options: { startupCommand: CODEX_RESUME_CMD, providerIntent: "codex" },
  },
];
