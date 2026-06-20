// The Codex client glyph: a scalloped blue→violet "flower" with a white `>_`
// terminal-prompt mark, traced from OpenAI's Codex logo. Unlike ClaudeIcon (a
// monochrome `currentColor` path), Codex's identity IS its blue/violet
// gradient, so the color is baked in; callers still control size/className/
// style/title so the API mirrors ClaudeIcon. Rendered inline (no raster asset)
// so it stays crisp at any size and needs no bundler asset plumbing.
import { useId } from "react";
import type { CSSProperties } from "react";

interface CodexIconProps {
  /** Square size (px number or any CSS length). Defaults to `1em` so it scales
   *  with the surrounding text, matching ClaudeIcon. */
  size?: number | string;
  className?: string;
  /** Inline styles (sizing/layout; the glyph's own colors are fixed). */
  style?: CSSProperties;
  /** Accessible label / tooltip. */
  title?: string;
}

// Eight petal centers evenly around the middle (every 45°) at radius 5.8 from
// (12,12). Overlapping circles whose union is the scalloped blob; one shared
// gradient (userSpaceOnUse, see below) spans the whole viewBox so every circle
// samples the SAME top→bottom ramp and the blob reads as one continuous shape
// with no internal seams.
const PETALS: ReadonlyArray<readonly [number, number]> = [
  [17.8, 12.0],
  [16.1, 7.9],
  [12.0, 6.2],
  [7.9, 7.9],
  [6.2, 12.0],
  [7.9, 16.1],
  [12.0, 17.8],
  [16.1, 16.1],
];

export function CodexIcon({
  size = "1em",
  className,
  style,
  title = "Codex",
}: CodexIconProps) {
  // Unique per instance so several Codex icons on one page don't collide on the
  // gradient id (a duplicate id can break the fill in some engines). Strip the
  // colons useId emits so the id is safe inside a `url(#...)` reference.
  const gradId = `codex-grad-${useId().replace(/:/g, "")}`;
  return (
    <svg
      role="img"
      viewBox="0 0 24 24"
      width={size}
      height={size}
      aria-label={title}
      className={className}
      style={style}
    >
      <title>{title}</title>
      <defs>
        {/* userSpaceOnUse so the one gradient is shared across all the petal
            circles (an objectBoundingBox gradient would restart per circle and
            seam the blob). Vertical: violet at the top, blue at the bottom. */}
        <linearGradient
          id={gradId}
          gradientUnits="userSpaceOnUse"
          x1="12"
          y1="2.5"
          x2="12"
          y2="21.5"
        >
          <stop offset="0" stopColor="#ab9cf5" />
          <stop offset="0.55" stopColor="#6d76f3" />
          <stop offset="1" stopColor="#3d5bf2" />
        </linearGradient>
      </defs>
      {/* Scalloped blob: 8 petals + a center disc, all sharing the gradient. */}
      <g fill={`url(#${gradId})`}>
        {PETALS.map(([cx, cy], i) => (
          <circle key={i} cx={cx} cy={cy} r={4.9} />
        ))}
        <circle cx={12} cy={12} r={6.4} />
      </g>
      {/* White `>_` prompt: a rounded chevron + an underscore, lower-right. */}
      <g
        fill="none"
        stroke="#ffffff"
        strokeWidth={2.1}
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <polyline points="8.3,8.2 11.6,12 8.3,15.8" />
        <line x1="13.2" y1="15.8" x2="17.4" y2="15.8" />
      </g>
    </svg>
  );
}
