// Compact WSL health display (PLAN.md Workstream H — utility area). Renders a
// HostMetrics snapshot (RAM/swap/CPU load/process count/distro) with a warning
// tint when memory is tight. Low-priority chrome; presentational only. The
// metrics come from the agent via client05.hostMetrics() (polled by a parent).
import type { HostMetrics } from "../ipc/protocol";
import type { ConnectionState } from "../ipc/protocol";
import { useTerminalResourceSnapshot } from "../lib/terminalResources";

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

export interface WslHealthProps {
  /** Latest metrics, or null before the first successful poll. */
  metrics: HostMetrics | null;
  /** Current agent connection state (drives the "agent offline" hint). */
  connection?: ConnectionState;
}

export function WslHealth({ metrics, connection }: WslHealthProps) {
  const terminals = useTerminalResourceSnapshot();
  if (!metrics) {
    const offline = connection && connection !== "live";
    return (
      <div className="px-2 py-1 text-[11px] text-neutral-600">
        {offline ? `agent ${connection}` : "WSL metrics pending…"}
      </div>
    );
  }

  const memUsed = usedFraction(metrics.mem_total_kib, metrics.mem_available_kib);
  const memWarn = memUsed >= 0.9;
  const memCaution = memUsed >= 0.75;
  const load1 = metrics.load_avg?.[0] ?? 0;
  // Load relative to core count: >1.0 per core is saturation.
  const loadWarn = metrics.cpu_count > 0 && load1 / metrics.cpu_count >= 1.0;

  const memColor = memWarn
    ? "text-red-400"
    : memCaution
      ? "text-amber-400"
      : "text-neutral-400";

  return (
    <div className="flex flex-col gap-0.5 px-2 py-1 text-[11px] text-neutral-500">
      {metrics.distro && (
        <div className="truncate text-neutral-400" title={metrics.distro}>
          {metrics.distro}
        </div>
      )}
      <div className="flex items-center justify-between gap-2">
        <span>RAM</span>
        <span className={memColor} title={`${(memUsed * 100).toFixed(0)}% used`}>
          {gib(metrics.mem_total_kib - metrics.mem_available_kib)}/
          {gib(metrics.mem_total_kib)} GiB
        </span>
      </div>
      {metrics.swap_total_kib > 0 && (
        <div className="flex items-center justify-between gap-2">
          <span>Swap</span>
          <span>
            {gib(metrics.swap_total_kib - metrics.swap_free_kib)}/
            {gib(metrics.swap_total_kib)} GiB
          </span>
        </div>
      )}
      <div className="flex items-center justify-between gap-2">
        <span>Load</span>
        <span className={loadWarn ? "text-amber-400" : undefined}>
          {metrics.load_avg.map((l) => l.toFixed(2)).join(" ")}
          <span className="text-neutral-600"> · {metrics.cpu_count}c</span>
        </span>
      </div>
      <div className="flex items-center justify-between gap-2">
        <span>Procs</span>
        <span>{metrics.process_count}</span>
      </div>
      <div
        className="flex items-center justify-between gap-2"
        title={`${terminals.xterms} xterm, ${terminals.canvases} canvas, ${terminals.ptys} PTY`}
      >
        <span>Terms</span>
        <span>
          {terminals.hot} hot · {terminals.warm} warm · {terminals.cold} cold
        </span>
      </div>
    </div>
  );
}
