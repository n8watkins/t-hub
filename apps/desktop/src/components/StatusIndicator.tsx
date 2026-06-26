// A single, shared status indicator: a RING (bordered circle) with a SOLID
// CENTER of a contrasting color. It replaces the old flat/blinky "dots" with one
// legible shape whose state reads at a glance even at ~8-12px:
//
//   working   — animated pulsing RING + solid center (accent/emerald): actively
//               working. The pulse is a halo ring (th-ind-pulse keyframe, see
//               index.css) so the shape itself never jumps size.
//   attention — pulsing AMBER ring (needs input / blocked / needs permission).
//   done      — SOLID filled, calm green, no animation (a finished, quiet turn).
//   error     — SOLID filled red, no animation.
//   idle      — distinct + muted: a HOLLOW outline ring, dim, hollow center.
//
// Pure presentational: no store access. Callers map their own data to a variant
// (see StatusBadge / Tile / SupervisionTree). The colors come from the theme
// tokens (--th-*) so it tracks the active theme; the pulse keyframes live in the
// APPENDED block at the end of src/index.css.

import type { SessionStatus } from "../ipc/model";
import type { TerminalState } from "../ipc/types";

export type StatusVariant = "working" | "idle" | "done" | "attention" | "error";

/**
 * Map a Claude/Codex session's reducer status (FR-012) to an indicator variant.
 * This is the PRECISE signal — distinct states for actively working vs. asking a
 * question / needing permission vs. idle — so it never conflates them the way a
 * raw output-activity pulse does.
 */
export function sessionStatusToVariant(status: SessionStatus): StatusVariant {
  switch (status) {
    case "working":
    case "waitingOnSubagents":
      return "working";
    case "needsQuestion":
    case "needsPermission":
    case "rateLimited":
      return "attention";
    case "failed":
      return "error";
    case "completed":
      return "done";
    // detached / restoring / expired / unknown → present but quiet.
    default:
      return "idle";
  }
}

/**
 * The indicator variant for ONE terminal row/tile, or `null` for a BLANK state
 * (an empty/quiet terminal — no agent bound and nothing running).
 *
 * Priority: a lifecycle error wins; then, if an agent session is bound (Claude
 * has supervision + statusline), trust its precise reducer status — crucially we
 * do NOT fall back to the noisy output-activity proxy here, so an idle Claude TUI
 * that keeps redrawing no longer false-pulses as "working". Only a SESSION-LESS
 * terminal (a plain shell, or Codex before/without a status snapshot) uses output
 * activity: it pulses while a command is actively producing output, and is blank
 * when quiet.
 */
export function terminalVariant(
  state: TerminalState,
  sessionStatus: SessionStatus | undefined,
  outputActive: boolean,
): StatusVariant | null {
  if (state === "error") return "error";
  if (sessionStatus !== undefined) return sessionStatusToVariant(sessionStatus);
  if (outputActive) return "working";
  return null;
}

/** How a variant draws:
 *  - `spinner` — a rotating arc ("thinking"); the working state.
 *  - `solid`   — a true filled disc (done / error).
 *  - `ring`    — a hollow colored outline (idle).
 *  - `pulse`   — a ring + solid center with a pulsing halo (attention). */
type IndicatorKind = "spinner" | "solid" | "ring" | "pulse";

/** Per-variant visual spec: the main `color` (theme token or semantic hex) + how
 *  it draws (`kind`). */
interface VariantSpec {
  color: string;
  kind: IndicatorKind;
  label: string;
}

/** A true, vivid green for the finished/idle states (done = solid, idle = ring). */
const TRUE_GREEN = "#22c55e";

const VARIANTS: Record<StatusVariant, VariantSpec> = {
  // Actively working: an accent SPINNER — reads as "thinking".
  working: { color: "var(--th-accent)", kind: "spinner", label: "Working" },
  // Needs the user: amber ring + center, pulsing so it draws the eye.
  attention: { color: "#f59e0b", kind: "pulse", label: "Needs attention" },
  // Finished, calm: a TRUE solid green disc, no animation.
  done: { color: TRUE_GREEN, kind: "solid", label: "Done" },
  // Error: a solid red disc, no animation.
  error: { color: "#ef4444", kind: "solid", label: "Error" },
  // Idle: a hollow GREEN ring — present but nothing happening.
  idle: { color: TRUE_GREEN, kind: "ring", label: "Idle" },
};

export interface StatusIndicatorProps {
  /** The state to render, or `null` for a BLANK indicator — an empty slot that
   *  still reserves `size` so the row/header layout never shifts. */
  variant: StatusVariant | null;
  /** Outer diameter in px (default 10 — reads clearly in the 8-12px range). */
  size?: number;
  /** Override the title/aria-label (else the variant's default label). */
  title?: string;
  className?: string;
}

export function StatusIndicator({
  variant,
  size = 10,
  title,
  className,
}: StatusIndicatorProps) {
  // Blank state: reserve the slot (so rows stay aligned) but draw nothing.
  if (variant === null) {
    return (
      <span
        className={className}
        style={{ width: size, height: size, display: "inline-block" }}
        aria-hidden
      />
    );
  }
  const spec = VARIANTS[variant] ?? VARIANTS.idle;
  const label = title ?? spec.label;
  const common = { role: "img" as const, "aria-label": label, title: label };
  const base = { width: size, height: size, borderRadius: 9999 };
  // Border/arc thickness scales with the indicator so it stays crisp at any size.
  const bw = Math.max(1.5, Math.round(size * 0.16 * 100) / 100);

  // Working: a rotating arc — one colored side of a faint ring, spun ("thinking").
  if (spec.kind === "spinner") {
    return (
      <span
        className={`th-ind-spin ${className ?? ""}`}
        style={{
          ...base,
          border: `${bw}px solid color-mix(in srgb, ${spec.color} 22%, transparent)`,
          borderTopColor: spec.color,
        }}
        {...common}
      />
    );
  }

  // Done / error: a true solid filled disc.
  if (spec.kind === "solid") {
    return (
      <span
        className={className}
        style={{ ...base, display: "inline-block", backgroundColor: spec.color }}
        {...common}
      />
    );
  }

  // Idle: a hollow colored ring (just the border).
  if (spec.kind === "ring") {
    return (
      <span
        className={className}
        style={{
          ...base,
          display: "inline-block",
          boxSizing: "border-box",
          border: `${bw}px solid ${spec.color}`,
        }}
        {...common}
      />
    );
  }

  // Attention: a ring + solid center with a pulsing halo (the eye-catcher).
  const ring = Math.max(1.25, Math.round(size * 0.18 * 100) / 100);
  const center = Math.max(2, Math.round(size * 0.44 * 100) / 100);
  return (
    <span
      className={`th-ind th-ind-pulse ${className ?? ""}`}
      style={{
        // The pulse keyframe reads --th-ind-pulse-color to draw a halo of the color.
        ["--th-ind-pulse-color" as string]: spec.color,
        width: size,
        height: size,
        border: `${ring}px solid ${spec.color}`,
      }}
      {...common}
    >
      {/* The SOLID CENTER — a contrasting fill inside the ring. */}
      <span
        className="th-ind-center"
        style={{ width: center, height: center, backgroundColor: spec.color }}
      />
    </span>
  );
}
