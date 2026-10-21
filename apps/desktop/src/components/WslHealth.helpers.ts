

/** KiB → human GiB string. Exported so the sidebar's collapsed WSL summary
 *  (Sidebar.tsx BottomStatus) formats memory identically to the expanded view. */
export function gib(kib: number): string {
  return (kib / (1024 * 1024)).toFixed(1);
}

/** Memory-used fraction (0..1) from total/available. Exported for the collapsed
 *  summary so its warning thresholds match this expanded view exactly. */
export function usedFraction(total: number, available: number): number {
  if (total <= 0) return 0;
  return Math.min(1, Math.max(0, (total - available) / total));
}
