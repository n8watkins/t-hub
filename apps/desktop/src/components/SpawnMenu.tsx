// The spawn-preset popover for the "+" new-terminal affordance. It anchors a
// small themed menu with two presets (per the user's call — a fresh `claude` in
// ~ and a free-text "Custom" line were dropped as low-value clutter):
//   - Shell          → a plain login shell in ~ (today's default behavior)
//   - Resume Claude… → opens a NEW terminal running `claude --resume`, which shows
//                      Claude's interactive session PICKER in that terminal so the
//                      user chooses which past session to resume. It does NOT
//                      resume every session, and it always opens a fresh tile.
//
// Selecting a preset calls back with an optional `startupCommand` string (None
// for Shell), which Canvas threads into spawnTerminal({ startupCommand }) → the
// backend runs it inside a login shell the pane execs back into (so quitting the
// command drops to a live shell instead of closing the tile).
//
// Chrome matches the rest of T-Hub: it reads the `--th-*` theme tokens
// (surface/border/fg/accent/radius) so it tracks the active theme like the FAB
// and the ThemeEditor panels do.
import { useEffect, useState } from "react";
import { ShieldCheck } from "lucide-react";

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

export interface SpawnMenuProps {
  /** Close the menu without spawning (Escape / backdrop click / after a pick). */
  onClose: () => void;
  /**
   * Spawn a terminal with the chosen preset. `startupCommand` is the command to
   * run in the new pane, or `undefined` for the plain "Shell" preset (today's
   * behavior). `capability` is the requested tier — `"control"` ONLY when the
   * user explicitly flipped the (default-off) control toggle, `undefined` (read)
   * otherwise. Canvas owns the actual spawnTerminal() IPC call + tile insertion.
   */
  onSpawn: (startupCommand?: string, capability?: "control") => void;
  /** A spawn is already in flight (#7) — disable the presets so a double-click
   *  can't stack duplicate spawns. */
  busy?: boolean;
}

interface Preset {
  key: string;
  label: string;
  /** One-line hint shown under the label. */
  hint: string;
  /** The startup command, or undefined for a plain shell. */
  command?: string;
}

export const PRESETS: Preset[] = [
  { key: "shell", label: "Shell", hint: "New login shell in ~", command: undefined },
  {
    key: "resume",
    label: "Resume Claude…",
    // Make it unambiguous: a NEW terminal opens, showing Claude's session picker
    // there so the user chooses which session to resume (it doesn't resume all).
    hint: "New terminal → pick a session to resume",
    command: CLAUDE_RESUME_CMD,
  },
  {
    key: "codex",
    label: "Codex",
    hint: "New terminal → fresh Codex session",
    command: CODEX_CMD,
  },
  {
    key: "resume-codex",
    label: "Resume Codex…",
    // Symmetric with Resume Claude: opens Codex's own session picker in a new tile.
    hint: "New terminal → pick a Codex session to resume",
    command: CODEX_RESUME_CMD,
  },
];

export function SpawnMenu({ onClose, onSpawn, busy }: SpawnMenuProps) {
  // The audited control-capability opt-in. DEFAULT OFF (inverted least-privilege
  // is ratified — control is the deliberate, explicit choice, never the default).
  // When on, EVERY preset picked from this menu spawns with `capability:"control"`,
  // which the backend injects the full control token for and records in the audit
  // log. Local to the menu's lifetime: it resets to read every time the menu opens.
  const [control, setControl] = useState(false);

  // Escape closes the menu.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const pick = (command?: string) => {
    // Busy gate (#7): a spawn is already in flight — ignore the pick so a
    // double-click can't stack a duplicate spawn.
    if (busy) return;
    // Control is opt-in: pass `"control"` ONLY when the toggle is on, so an
    // ordinary pick always defaults to the read tier (never elevates by accident).
    onSpawn(command, control ? "control" : undefined);
    onClose();
  };

  return (
    // Full-viewport backdrop: a click anywhere outside the popover dismisses it.
    // Transparent (not dimmed) so it reads as a lightweight menu, not a modal.
    <div
      className="fixed inset-0 z-40"
      onPointerDown={onClose}
      aria-hidden={false}
    >
      <div
        role="menu"
        aria-label="New terminal preset"
        // Anchored just above the "+" FAB (which sits at bottom-3 right-3, h-9).
        // Stop pointer/click propagation so interacting with the menu doesn't hit
        // the backdrop dismiss above.
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
        className="absolute bottom-14 right-3 flex w-60 flex-col overflow-hidden rounded-lg border shadow-2xl"
        style={{
          // Solid surface so the menu never bleeds the terminal through
          // (--th-header-bg carries alpha in some themes).
          backgroundColor: "var(--th-tile-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
          borderRadius: "var(--th-radius)",
        }}
      >
        <div
          className="border-b px-3 py-2 text-xs font-medium uppercase tracking-wide"
          style={{
            borderColor: "var(--th-border)",
            color: "var(--th-fg-muted)",
          }}
        >
          New terminal
        </div>

        {PRESETS.map((p) => (
          <button
            key={p.key}
            type="button"
            role="menuitem"
            onClick={() => pick(p.command)}
            disabled={busy}
            className="flex flex-col items-start gap-0.5 px-3 py-2 text-left transition-colors hover:bg-neutral-700/30 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <span className="text-sm" style={{ color: "var(--th-fg)" }}>
              {p.label}
            </span>
            <span className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
              {p.hint}
            </span>
          </button>
        ))}

        {/* Audited control-capability opt-in. Visually SET APART from the presets
            (its own bordered footer + accent chrome when armed) because it grants
            the full control token — a privilege elevation, not a preset. Default
            off; the next preset picked while it is armed spawns a control terminal
            (e.g. a hand-spawned captain). The one-click "Create Orchestrator" is
            the fully-provisioned path; this is the general control-spawn primitive. */}
        <button
          type="button"
          role="menuitemcheckbox"
          aria-checked={control}
          aria-label="Spawn as control terminal"
          onClick={() => setControl((v) => !v)}
          title="Grant this terminal the full control token (spawn / type / kill the fleet). Audited. Default is a read-only work terminal."
          className="flex items-center gap-2 border-t px-3 py-2 text-left transition-colors hover:bg-neutral-700/30"
          style={{
            borderColor: "var(--th-border)",
            // Tint the whole row on the accent when armed so the elevation is
            // impossible to miss before a preset is picked.
            backgroundColor: control ? "var(--th-accent-soft, rgba(250,204,21,0.12))" : undefined,
          }}
        >
          <span
            className="flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border"
            style={{
              borderColor: control ? "var(--th-accent)" : "var(--th-border)",
              backgroundColor: control ? "var(--th-accent)" : "transparent",
              color: control ? "var(--th-tile-bg)" : "var(--th-fg-muted)",
            }}
          >
            {control && <ShieldCheck size={12} aria-hidden />}
          </span>
          <span className="flex flex-col">
            <span
              className="text-sm"
              style={{ color: control ? "var(--th-accent)" : "var(--th-fg)" }}
            >
              Control terminal
            </span>
            <span className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
              {control
                ? "Armed — the next preset spawns a control terminal (audited)"
                : "Elevate: grants the full control token (audited)"}
            </span>
          </span>
        </button>
      </div>
    </div>
  );
}
