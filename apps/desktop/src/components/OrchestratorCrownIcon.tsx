// The SPECIAL orchestrator glyph: a crown, marking Cortana as the entity that
// commands the fleet (visually distinct from the captains' anchor while the
// status dot / context stay at full parity). Inline SVG in the app's icon
// idiom (viewBox 24, currentColor fill) so its color tracks the accent in
// both themes. Its own module (the ClaudeIcon/CodexIcon convention) because
// two chrome surfaces render it: the sidebar's orchestrator row
// (CaptainsList) and the orchestrator's own tile header (Tile).
export function OrchestratorCrownIcon({ size = 13 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      className="pointer-events-none shrink-0"
      aria-hidden
    >
      {/* A five-point crown: outer points + a base band. */}
      <path d="M3 8l3.5 3L12 5l5.5 6L21 8l-1.8 9.2a1 1 0 0 1-.98.8H5.78a1 1 0 0 1-.98-.8L3 8z" />
    </svg>
  );
}
