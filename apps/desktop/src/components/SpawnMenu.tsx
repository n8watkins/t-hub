import { Preset, PRESETS, SpawnSelection } from "./SpawnMenu.helpers";
// The spawn-preset popover for the "+" new-terminal affordance. It anchors a
// small themed menu with two presets (per the user's call — a fresh `claude` in
// ~ and a free-text "Custom" line were dropped as low-value clutter):
//   - Shell          → a plain login shell in ~ (today's default behavior)
//   - Resume Claude… → opens a NEW terminal running `claude --resume`, which shows
//                      Claude's interactive session PICKER in that terminal so the
//                      user chooses which past session to resume. It does NOT
//                      resume every session, and it always opens a fresh tile.
//
// Selecting a terminal preset calls back with typed spawn options, which Canvas
// threads into spawnTerminal(). Captain opens the project-aware commissioning
// workflow instead of creating and pinning a generic elevated terminal.
//
// Chrome matches the rest of T-Hub: it reads the `--th-*` theme tokens
// (surface/border/fg/accent/radius) so it tracks the active theme like the FAB
// and the ThemeEditor panels do.
import { useEffect, useState } from "react";
import { CaptainCommissionDialog } from "./CaptainCommissionDialog";

export interface SpawnMenuProps {
  /** Close the menu without spawning (Escape / backdrop click / after a pick). */
  onClose: () => void;
  /**
   * Spawn a read-only Crew terminal with the chosen typed options. Captain
   * creation uses the separate commissioning flow.
   */
  onSpawn: (selection: SpawnSelection) => void;
  /** A spawn is already in flight (#7) — disable the presets so a double-click
   *  can't stack duplicate spawns. */
  busy?: boolean;
}

export function SpawnMenu({ onClose, onSpawn, busy }: SpawnMenuProps) {
  const [commissionOpen, setCommissionOpen] = useState(false);
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

  const pick = (preset: Preset) => {
    // Busy gate (#7): a spawn is already in flight — ignore the pick so a
    // double-click can't stack a duplicate spawn.
    if (busy) return;
    if (preset.commission) {
      setCommissionOpen(true);
      return;
    }
    onSpawn({ options: preset.options, pinAsCaptain: preset.pinAsCaptain });
    onClose();
  };

  return (
    <>
      {/* Full-viewport backdrop: clicking outside the popover dismisses it. */}
      <div
        className="fixed inset-0 z-40"
        onPointerDown={onClose}
        aria-hidden={false}
      >
        <div
          role="menu"
          aria-label="New terminal preset"
          // Anchored above the "+" FAB. Stop events inside the menu from reaching
          // the full-viewport dismiss backdrop.
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-14 right-3 flex w-60 flex-col overflow-hidden rounded-lg border shadow-2xl"
          style={{
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
              onClick={() => pick(p)}
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
        </div>
      </div>
      <CaptainCommissionDialog
        open={commissionOpen}
        onCommissioned={() => {}}
        onClose={() => {
          setCommissionOpen(false);
          onClose();
        }}
      />
    </>
  );
}
