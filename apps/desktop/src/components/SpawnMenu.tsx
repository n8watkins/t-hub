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
import { useEffect } from "react";

/** `claude --resume` lets Claude show its interactive session picker. (`--continue`
 *  / `-c` would silently resume the MOST RECENT session instead; the picker is the
 *  intended "Resume" UX, so we default to `--resume`.) */
const CLAUDE_RESUME_CMD = "claude --resume";

export interface SpawnMenuProps {
  /** Close the menu without spawning (Escape / backdrop click / after a pick). */
  onClose: () => void;
  /**
   * Spawn a terminal with the chosen preset. `startupCommand` is the command to
   * run in the new pane, or `undefined` for the plain "Shell" preset (today's
   * behavior). Canvas owns the actual spawnTerminal() IPC call + tile insertion.
   */
  onSpawn: (startupCommand?: string) => void;
}

interface Preset {
  key: string;
  label: string;
  /** One-line hint shown under the label. */
  hint: string;
  /** The startup command, or undefined for a plain shell. */
  command?: string;
}

const PRESETS: Preset[] = [
  { key: "shell", label: "Shell", hint: "New login shell in ~", command: undefined },
  {
    key: "resume",
    label: "Resume Claude…",
    // Make it unambiguous: a NEW terminal opens, showing Claude's session picker
    // there so the user chooses which session to resume (it doesn't resume all).
    hint: "New terminal → pick a session to resume",
    command: CLAUDE_RESUME_CMD,
  },
];

export function SpawnMenu({ onClose, onSpawn }: SpawnMenuProps) {
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
    onSpawn(command);
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
            className="flex flex-col items-start gap-0.5 px-3 py-2 text-left transition-colors hover:bg-neutral-700/30"
          >
            <span className="text-sm" style={{ color: "var(--th-fg)" }}>
              {p.label}
            </span>
            <span className="text-xs" style={{ color: "var(--th-fg-muted)" }}>
              {p.hint}
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}
