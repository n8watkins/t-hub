// The Codex client glyph — the user-provided Codex logo asset (a blue→violet
// scalloped blob with a white `>_` prompt). Rendered as an <img> so it's exactly
// the supplied artwork; the API (size/className/style/title) mirrors ClaudeIcon
// so the two are drop-in interchangeable in the tile header / Recent rows.
//
// Vite resolves the asset URL via `new URL(..., import.meta.url)` (no ambient
// PNG-module declaration needed) and fingerprints it into the bundle.
import type { CSSProperties } from "react";

const CODEX_PNG = new URL("../assets/codex.png", import.meta.url).href;

interface CodexIconProps {
  /** Square size (px number or any CSS length). Defaults to `1em` so it scales
   *  with the surrounding text, matching ClaudeIcon. */
  size?: number | string;
  className?: string;
  /** Inline styles (sizing/layout; the artwork's own colors are fixed). */
  style?: CSSProperties;
  /** Accessible label / tooltip. */
  title?: string;
}

export function CodexIcon({
  size = "1em",
  className,
  style,
  title = "Codex",
}: CodexIconProps) {
  const dim = typeof size === "number" ? `${size}px` : size;
  return (
    <img
      src={CODEX_PNG}
      alt={title}
      title={title}
      className={className}
      draggable={false}
      style={{
        width: dim,
        height: dim,
        objectFit: "contain",
        display: "inline-block",
        ...style,
      }}
    />
  );
}
