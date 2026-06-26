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

export type StatusVariant = "working" | "idle" | "done" | "attention" | "error";

/** Per-variant visual spec. `ring` is the border color, `center` the inner dot
 *  color, `pulse` toggles the halo animation, `hollow` makes the center empty
 *  (idle). All colors are CSS color strings (theme tokens where it tracks the
 *  theme, literal hex for the semantic amber/red that aren't themed). */
interface VariantSpec {
  ring: string;
  center: string;
  pulse: boolean;
  hollow: boolean;
  label: string;
}

const VARIANTS: Record<StatusVariant, VariantSpec> = {
  // Actively working: accent ring + a bright solid center, pulsing halo.
  working: {
    ring: "var(--th-accent)",
    center: "var(--th-accent)",
    pulse: true,
    hollow: false,
    label: "Working",
  },
  // Needs the user: amber ring + amber center, pulsing so it draws the eye.
  attention: {
    ring: "#f59e0b",
    center: "#f59e0b",
    pulse: true,
    hollow: false,
    label: "Needs attention",
  },
  // Finished, calm: solid green fill, no animation.
  done: {
    ring: "#10b981",
    center: "#10b981",
    pulse: false,
    hollow: false,
    label: "Done",
  },
  // Error: solid red, no animation.
  error: {
    ring: "#ef4444",
    center: "#ef4444",
    pulse: false,
    hollow: false,
    label: "Error",
  },
  // Idle: muted, hollow outline ring — clearly a "nothing happening" state.
  idle: {
    ring: "var(--th-fg-muted)",
    center: "transparent",
    pulse: false,
    hollow: true,
    label: "Idle",
  },
};

export interface StatusIndicatorProps {
  variant: StatusVariant;
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
  const spec = VARIANTS[variant] ?? VARIANTS.idle;
  const label = title ?? spec.label;
  // Ring thickness + center size scale with the indicator so it stays crisp at
  // any size. The ring is ~18% of the diameter (min 1.25px); the solid center is
  // ~44% so the contrasting fill is unmistakable inside the ring.
  const ring = Math.max(1.25, Math.round(size * 0.18 * 100) / 100);
  const center = Math.max(2, Math.round(size * 0.44 * 100) / 100);
  return (
    <span
      className={`th-ind${spec.pulse ? " th-ind-pulse" : ""} ${className ?? ""}`}
      style={{
        // The pulse keyframe reads --th-ind-pulse-color to draw a halo of the
        // ring color (set per-variant; ignored when not pulsing).
        ["--th-ind-pulse-color" as string]: spec.ring,
        width: size,
        height: size,
        // The RING: a colored border with a transparent interior.
        border: `${ring}px solid ${spec.ring}`,
        // Muted/idle reads dimmer; active/done/error are full strength.
        opacity: spec.hollow ? 0.6 : 1,
      }}
      role="img"
      aria-label={label}
      title={label}
    >
      {/* The SOLID CENTER — a contrasting fill inside the ring (empty for idle). */}
      <span
        className="th-ind-center"
        style={{
          width: center,
          height: center,
          backgroundColor: spec.center,
        }}
      />
    </span>
  );
}
