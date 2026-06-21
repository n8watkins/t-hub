// The sidebar — Workspaces + Projects navigation + Recent recall.
//
// In the product model the sidebar is three lists:
//   1. Workspaces — EVERY workspace tab as a collapsible row over the terminals
//      inside it (feat/workspaces-lifecycle). Clicking a row switches to that
//      workspace; clicking a nested terminal switches to it AND focuses it. The
//      active workspace is expanded + subtly highlighted.
//   2. Projects — the projects (terminals) open in the CURRENT (active) workspace
//      tab, named by directory. Clicking one reveals + focuses that tile.
//   3. Recent  — past Claude sessions you can RECALL: clicking re-spawns a
//      terminal in that session's directory and resumes it (`claude --resume`).
//
// Gone (vs. the old 0.5 supervision sidebar): the Workspaces list (tabs live in
// the titlebar strip), the Attention queue, the Claude supervision tree, and the
// global Files tree (Files is moving into each tile). The supervision/telemetry
// STORES still exist app-wide; this surface just no longer reads supervision.
//
// Kept working: the 3-state collapse (full / rail / hidden), the bottom-pinned
// WSL health strip, the secondary settings gear, and the public exports
// (SIDEBAR_RAIL_WIDTH, SidebarMode, the Sidebar props) App/Titlebar compile
// against. Telemetry is still read for the bottom WSL/host-metrics strip.
import { useAgentTelemetry } from "../store/telemetry";
import { useSettings } from "../store/settings";
import { useWorkspace, type WorkspaceTab } from "../store/workspace";
import { useTheme } from "../store/theme";
import { WslHealth, gib, usedFraction } from "./WslHealth";
import {
  UsageStrip,
  UsageInline,
  useClaudeUsage,
  CodexUsageStrip,
  CodexUsageInline,
  useCodexUsage,
} from "./UsageStrip";
import { WorkspacesList } from "./WorkspacesList";
import { RecentList } from "./RecentList";
import { ClaudeIcon } from "./ClaudeIcon";
import { CodexIcon } from "./CodexIcon";
import { ChevronIcon, CountBadge } from "./SidebarChrome";
import { usePersistedToggle } from "../hooks/usePersistedToggle";
import type { HostMetrics, ConnectionState } from "../ipc/protocol";
import type { TerminalId } from "../ipc/types";

// --- Sidebar header chrome -------------------------------------------------
// The window controls (minimize / maximize-restore / close) and the PRIMARY
// settings gear live in the TITLEBAR (see Titlebar.tsx) — always reachable
// regardless of the sidebar's collapse state. The sidebar header keeps only the
// T-Hub brand (a window-drag handle), the collapse button (the Ctrl/Cmd+B
// cycle), and a small SECONDARY settings gear for convenience.

/**
 * The sidebar's 3-state collapse mode (App owns + persists it; #1):
 *  - "full": the resizable full-width Projects + Recent surface.
 *  - "rail": a thin ~48px strip showing just iconic section markers + a compact
 *    workspace list, "barely showing" but still useful for switching tabs.
 *  - "hidden": not rendered at all (App skips <Sidebar> entirely).
 * The cycle full -> rail -> hidden -> full is driven by App's onToggleSidebar.
 */
export type SidebarMode = "full" | "rail" | "hidden";

/** Pixel width of the rail strip (kept in sync with App's RAIL width). */
export const SIDEBAR_RAIL_WIDTH = 48;

export interface SidebarProps {
  /** RECALL a past Claude session: spawn `claude --resume <id>` in `cwd`, add the
   *  tile to the active tab, and focus it. App wires this to the store's recall. */
  onRecall?: (sessionId: string, cwd: string) => void;
  /** Collapse mode (#1). "hidden" is handled by App (it skips render), so the
   *  component itself only ever sees "full" or "rail"; defaults to "full". */
  mode?: SidebarMode;
  /** Sidebar width in px (resizable, #2). Defaults to 256 (the old fixed w-64). */
  width?: number;
  /**
   * Cycle the sidebar collapse state (full -> rail -> hidden -> full). The
   * sidebar header's collapse button uses this so the chrome that now lives in
   * the sidebar can drive the same Ctrl/Cmd+B cycle App owns.
   */
  onToggleSidebar?: () => void;
}

export function Sidebar({
  onRecall,
  mode = "full",
  width = 256,
  onToggleSidebar,
}: SidebarProps) {
  // Active workspace tab + its live terminals drive the Projects list; the
  // titlebar owns tab switching, so the sidebar only ever reads the ACTIVE tab.
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  const setActiveTab = useWorkspace((s) => s.setActiveTab);

  // Rail mode: a thin, iconic strip. Hooks must run unconditionally, so the
  // selectors above always run; we branch on render here.
  if (mode === "rail") {
    return (
      <SidebarRail
        width={width}
        tabs={tabs}
        activeTabId={activeTabId}
        setActiveTab={setActiveTab}
        onToggleSidebar={onToggleSidebar}
      />
    );
  }

  return (
    <SidebarFull
      width={width}
      onRecall={onRecall}
      onToggleSidebar={onToggleSidebar}
    />
  );
}

interface FullProps {
  width: number;
  onRecall?: (sessionId: string, cwd: string) => void;
  onToggleSidebar?: () => void;
}

function SidebarFull({ width, onRecall, onToggleSidebar }: FullProps) {
  const { metrics, agent } = useAgentTelemetry();

  // Only the tab list is needed here now (the Workspaces section count); the
  // Workspaces list itself reads everything else from the store directly.
  const tabs = useWorkspace((s) => s.tabs);

  return (
    <aside
      className="flex h-full shrink-0 flex-col overflow-hidden border-r"
      style={{
        width,
        backgroundColor: "var(--th-sidebar-bg)",
        borderColor: "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
      {/* The sidebar's top chrome (brand + collapse + settings) now lives in the
          titlebar's LEFT cluster (see Titlebar.tsx LeftChrome), so the sidebar
          starts straight at its content and reclaims that vertical space. */}

      {/* Body: two stacked sections — Workspaces (EVERY tab + its terminals; the
          one navigation surface now) and Recent (recallable past Claude
          sessions). Each grows and scrolls internally; the whole body scrolls as
          a safety net on a short window. The old separate "Projects" section is
          gone — a workspace's terminals live under it in Workspaces. */}
      <div className="th-scroll flex min-h-0 flex-1 flex-col overflow-y-auto">
        {/* Workspaces — every workspace tab as a collapsible row over its
            terminals: switch workspace/terminal, rename, close. Self-contained
            (reads the store directly). */}
        <Section title="Workspaces" count={tabs.length} className="border-b">
          <WorkspacesList />
        </Section>

        {/* Recent — past Claude sessions to resume. Collapsible, and capped to
            half the viewport with its own scroll so a long history can't swallow
            the whole sidebar; the list scrolls inside this region, not the page. */}
        <Section
          title="Recent"
          className="border-b"
          collapsible
          storageKey="t-hub.sidebar.recent.open"
          bodyClassName="th-scroll overflow-y-auto"
          bodyStyle={{ maxHeight: "38vh" }}
        >
          <RecentList onRecall={(id, cwd) => onRecall?.(id, cwd)} />
        </Section>
      </div>

      {/* Pinned to the very bottom: the Claude USAGE readout (cost/context/rate
          limits, from the supervision snapshots) then the WSL/host-metrics health
          strip. Both live outside the scroll body so they stay bottom-left
          regardless of how long the lists above grow. */}
      <UsageSection />
      <BottomStatus metrics={metrics} connection={agent?.connection} />
    </aside>
  );
}

/**
 * Bottom-pinned WSL health strip (#wsl-bottom): a thin always-visible bar with a
 * collapse chevron and the "WSL" label, then the WslHealth body (host/distro
 * metrics + the agent connection state). Pinned to the sidebar's bottom-left,
 * independent of the lists above. Open/collapsed persists to localStorage.
 *
 * When COLLAPSED the bar still carries a one-line summary (RAM used/total + the
 * 1-minute load average) so the strip stays useful without expanding — it reads
 * the same `metrics` the expanded body does. Expanded shows the full WslHealth.
 *
 * The old Usage view (Claude context/cost aggregated across supervised sessions)
 * was dropped here: the sidebar no longer reads the supervision store. WSL health
 * is host telemetry and stays.
 */
function BottomStatus({
  metrics,
  connection,
}: {
  metrics: HostMetrics | null;
  connection?: ConnectionState;
}) {
  const [open, persistOpen] = usePersistedToggle("t-hub.sidebar.bottom.open");

  return (
    <div className="shrink-0 border-t" style={{ borderColor: "var(--th-border)" }}>
      <div className="flex items-stretch">
        <button
          type="button"
          onClick={() => persistOpen(!open)}
          className="flex h-7 w-6 shrink-0 items-center justify-center opacity-70 hover:opacity-100"
          aria-expanded={open}
          title={open ? "Collapse" : "Expand"}
        >
          <ChevronIcon open={open} />
        </button>
        <span
          className="flex items-center px-1 py-1 text-xs font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg-muted)" }}
        >
          WSL
        </span>
        {/* Collapsed: keep a live RAM + load readout in the bar itself. */}
        {!open && <WslMiniSummary metrics={metrics} />}
      </div>
      {open && (
        // FIXED height so the strip never jumps; the view scrolls within it.
        <div
          className="th-scroll overflow-y-auto border-t"
          style={{ borderColor: "var(--th-border)", height: 116 }}
        >
          <WslHealth metrics={metrics} connection={connection} />
        </div>
      )}
    </div>
  );
}

/**
 * The collapsed WSL bar's inline readout: RAM used/total (GiB) + the 1-minute
 * load average, right-aligned in the bar. Uses the SAME thresholds as the
 * expanded WslHealth (gib/usedFraction shared from there) so the colors agree.
 * Renders nothing until the first metrics snapshot lands.
 */
function WslMiniSummary({ metrics }: { metrics: HostMetrics | null }) {
  if (!metrics) return null;
  const memUsed = usedFraction(metrics.mem_total_kib, metrics.mem_available_kib);
  const memColor =
    memUsed >= 0.9 ? "text-red-400" : memUsed >= 0.75 ? "text-amber-400" : undefined;
  const load1 = metrics.load_avg?.[0] ?? 0;
  // Load relative to core count: >=1.0 per core is saturation (matches WslHealth).
  const loadWarn = metrics.cpu_count > 0 && load1 / metrics.cpu_count >= 1.0;
  return (
    <span
      className="ml-auto flex items-center gap-2 pr-2 text-[10px] tabular-nums"
      style={{ color: "var(--th-fg-muted)" }}
    >
      <span className={memColor} title={`RAM ${(memUsed * 100).toFixed(0)}% used`}>
        {gib(metrics.mem_total_kib - metrics.mem_available_kib)}/
        {gib(metrics.mem_total_kib)}G
      </span>
      <span
        className={loadWarn ? "text-amber-400" : undefined}
        title={`Load ${metrics.load_avg.map((l) => l.toFixed(2)).join(" ")} · ${metrics.cpu_count} cores`}
      >
        {load1.toFixed(2)}
      </span>
    </span>
  );
}

/**
 * Bottom-pinned Claude USAGE: a collapse chevron + "Usage" label. Expanded shows
 * the full weekly/session rows; collapsed keeps the key REMAINING percentages
 * (weekly + 5-hour) inline in the bar itself, so the strip stays useful without
 * expanding. A single poller (useClaudeUsage) feeds both states.
 * Open/collapsed persists to localStorage.
 */
function UsageSection() {
  const [open, persistOpen] = usePersistedToggle("t-hub.sidebar.usage.open");
  // One poller each drives both the collapsed inline summary and the full strip,
  // for Claude and (when present) Codex.
  const usage = useClaudeUsage();
  const codex = useCodexUsage();
  const hasCodex = !!codex?.ok;
  return (
    <div className="shrink-0 border-t" style={{ borderColor: "var(--th-border)" }}>
      <div className="flex items-stretch">
        <button
          type="button"
          onClick={() => persistOpen(!open)}
          className="flex h-7 w-6 shrink-0 items-center justify-center opacity-70 hover:opacity-100"
          aria-expanded={open}
          title={open ? "Collapse" : "Expand"}
        >
          <ChevronIcon open={open} />
        </button>
        <span
          className="flex items-center px-1 py-1 text-xs font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg-muted)" }}
        >
          Usage
        </span>
        {/* Collapsed: key percentages inline — Claude (ml-auto pushes it right),
            then Codex right after it when present. */}
        {!open && (
          <>
            <UsageInline usage={usage} />
            {hasCodex && <CodexUsageInline usage={codex} />}
          </>
        )}
      </div>
      {/* Expanded: the full weekly/session rows. With Codex present, label each
          provider so the two readouts are unambiguous. */}
      {open && (
        <>
          {hasCodex && <ProviderLabel name="Claude" />}
          <UsageStrip usage={usage} />
          {hasCodex && (
            <>
              <ProviderLabel name="Codex" />
              <CodexUsageStrip usage={codex} />
            </>
          )}
        </>
      )}
    </div>
  );
}

/** A small provider sub-header shown above each usage block when both Claude and
 *  Codex usage are present, so the weekly/session rows are unambiguous. */
function ProviderLabel({ name }: { name: string }) {
  const Icon = name === "Codex" ? CodexIcon : ClaudeIcon;
  return (
    <div
      className="flex items-center gap-1.5 px-2 pt-1 text-[10px] font-semibold uppercase tracking-wide"
      style={{ color: "var(--th-fg-muted)" }}
    >
      <Icon
        size={11}
        className="shrink-0"
        style={name === "Claude" ? { color: "#D97757" } : undefined}
        title={name}
      />
      {name}
    </div>
  );
}

/**
 * Rail mode (#1): a thin ~48px iconic strip — "barely showing" but still useful.
 * It stacks one square per workspace tab (its initial + a tiny tile count) so the
 * user can still switch tabs, then a small column of section glyphs (Projects /
 * Recent) as a hint of what the full sidebar holds.
 */
function SidebarRail({
  width,
  tabs,
  activeTabId,
  setActiveTab,
  onToggleSidebar,
}: {
  width: number;
  tabs: WorkspaceTab[];
  activeTabId: string;
  setActiveTab: (id: string) => void;
  onToggleSidebar?: () => void;
}) {
  // Per-workspace color identity (feat/workspace-colors) — tints the rail squares.
  const railColors = useTheme((s) => s.workspaceColors);
  return (
    <aside
      className="flex h-full shrink-0 flex-col items-center gap-1 border-r"
      style={{
        width,
        backgroundColor: "var(--th-sidebar-bg)",
        borderColor: "var(--th-border)",
        color: "var(--th-fg)",
      }}
    >
      {/* Compact header for the rail: the brand mark (also a drag handle) stacked
          over the collapse button. The window controls live in the titlebar. */}
      <SidebarRailHeader onToggleSidebar={onToggleSidebar} />
      <div className="flex flex-col items-center gap-1 pt-1">
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          const count = tab.order.length;
          const initial = (tab.name.trim()[0] ?? "?").toUpperCase();
          // The workspace color identity: fills the active square, and tints an
          // inactive square's border so each workspace is recognizable in the rail.
          const color = railColors[tab.id];
          return (
            <button
              key={tab.id}
              type="button"
              onClick={() => setActiveTab(tab.id)}
              title={`${tab.name} — ${count} project${count === 1 ? "" : "s"}`}
              aria-current={active ? "true" : undefined}
              className="relative flex h-8 w-8 items-center justify-center rounded text-xs font-semibold hover:opacity-90"
              style={{
                backgroundColor: active
                  ? color ?? "var(--th-accent)"
                  : "transparent",
                color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
                border: active
                  ? undefined
                  : `1px solid ${color ?? "var(--th-border)"}`,
              }}
            >
              {initial}
              {count > 0 && (
                <span
                  className="absolute -right-0.5 -top-0.5 min-w-[12px] rounded-full px-0.5 text-center text-[8px] leading-[12px]"
                  style={{
                    backgroundColor: "var(--th-border)",
                    color: "var(--th-fg)",
                  }}
                >
                  {count}
                </span>
              )}
            </button>
          );
        })}
        {tabs.length === 0 && (
          <div
            className="text-xs"
            style={{ color: "var(--th-fg-muted)" }}
            title="No workspaces"
          >
            —
          </div>
        )}
      </div>
      {/* Section hints: glyphs standing in for the full sidebar's two lists. */}
      <div
        className="mt-auto flex flex-col items-center gap-1 px-1 pb-2 pt-2 text-sm"
        style={{ color: "var(--th-fg-muted)" }}
        aria-hidden
      >
        <span title="Projects">▦</span>
        <span title="Recent">↺</span>
        <span title="WSL">◷</span>
      </div>
    </aside>
  );
}

// ===========================================================================
// Sidebar chrome header — the brand + collapse control. FULL mode: a single
// 32px row with the T-Hub brand on the left and the collapse button + a small
// secondary settings gear on the right. RAIL mode: a compact stacked version.
// The window controls + the PRIMARY settings gear live in the titlebar (see
// Titlebar.tsx) and are intentionally NOT duplicated here.
// ===========================================================================

/**
 * The full-mode sidebar header: a 32px row matching the titlebar height. The
 * left holds the brand (a window-drag handle); the right holds the collapse
 * button (cycles full -> rail -> hidden, the Ctrl/Cmd+B action) plus a small
 * secondary settings gear. The empty middle is also a drag handle so the window
 * can still be moved by grabbing the header.
 */
function SidebarHeader({ onToggleSidebar }: { onToggleSidebar?: () => void }) {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  return (
    <div
      className="flex h-8 shrink-0 items-stretch border-b"
      style={{ borderColor: "var(--th-border)" }}
    >
      <SidebarBrand />
      {/* Draggable filler so the header itself moves the window. */}
      <div data-tauri-drag-region className="min-w-0 flex-1" aria-hidden />
      {onToggleSidebar && <CollapseButton onClick={onToggleSidebar} />}
      <SidebarSettingsButton onClick={toggleSettings} />
    </div>
  );
}

/**
 * The rail-mode header: the brand mark over the collapse button, stacked
 * vertically so they fit the thin (~48px) strip. The brand square doubles as a
 * window-drag handle; the collapse button expands the rail back to full (or on
 * to hidden). The window controls live in the titlebar, not here.
 */
function SidebarRailHeader({ onToggleSidebar }: { onToggleSidebar?: () => void }) {
  return (
    <div
      className="flex w-full flex-col items-center gap-1 border-b pb-1.5 pt-1.5"
      style={{ borderColor: "var(--th-border)" }}
    >
      {/* Only the brand mark is a drag handle; the control button must NOT be
          inside a drag region or a click would start a window drag instead. */}
      <span
        data-tauri-drag-region
        className="inline-block h-3 w-3 rounded-[2px]"
        style={{ backgroundColor: "var(--th-accent)" }}
        title="T-Hub"
        aria-hidden
      />
      {onToggleSidebar && (
        <button
          type="button"
          onClick={onToggleSidebar}
          aria-label="Expand sidebar"
          title="Expand sidebar"
          className="flex h-6 w-6 items-center justify-center rounded text-neutral-300 transition-colors hover:bg-neutral-700"
        >
          <SidebarToggleIcon />
        </button>
      )}
    </div>
  );
}

/** "T-Hub" wordmark with a small accent glyph; a window-drag handle. */
function SidebarBrand() {
  return (
    <div
      data-tauri-drag-region
      className="flex shrink-0 select-none items-center gap-1.5 pl-2.5 pr-2"
    >
      <span
        className="inline-block h-2.5 w-2.5 rounded-[2px]"
        style={{ backgroundColor: "var(--th-accent)" }}
        aria-hidden
      />
      <span
        className="text-xs font-semibold tracking-tight"
        style={{ color: "var(--th-fg)" }}
      >
        T-Hub
      </span>
    </div>
  );
}

/** Collapse button — cycles the sidebar (full -> rail -> hidden). */
function CollapseButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Collapse sidebar"
      title="Collapse sidebar (Ctrl/Cmd+B)"
      className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
    >
      <SidebarToggleIcon />
    </button>
  );
}

/** Settings gear — opens the settings/theme surface (also Ctrl/Cmd+,). */
function SidebarSettingsButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      aria-label="Settings"
      title="Settings (Ctrl/Cmd+,)"
      onClick={onClick}
      className="flex h-8 w-9 items-center justify-center text-neutral-300 transition-colors hover:bg-neutral-700"
    >
      <GearIcon />
    </button>
  );
}

// --- Shared chrome icons (sized to sit in the 32px header) -----------------

/** Settings gear. */
function GearIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none"
      aria-hidden
    >
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

/** Sidebar collapse/expand glyph (a panel with a divider). */
function SidebarToggleIcon() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="pointer-events-none"
      aria-hidden
    >
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <line x1="9" y1="4" x2="9" y2="20" />
    </svg>
  );
}

// ===========================================================================
// Sections. Each of the sidebar's two lists (Projects, Recent) is a simple
// titled block: an uppercase header (with an optional count chip) over its body.
// Unlike the old supervision sidebar these are NOT collapsible accordions —
// there are only two, and both are primary, so they're always shown.
// ===========================================================================

/** A titled sidebar section: an uppercase header (chevron-free) with an optional
 *  count chip, over its body. The outer `className` carries the border styling. */
function Section({
  title,
  count,
  className,
  children,
  collapsible = false,
  storageKey,
  bodyClassName,
  bodyStyle,
}: {
  title: string;
  count?: number;
  className?: string;
  children: React.ReactNode;
  /** When true, the header becomes a chevron toggle that shows/hides the body. */
  collapsible?: boolean;
  /** localStorage key to persist the collapsed state across launches. */
  storageKey?: string;
  /** Optional wrapper around the body — e.g. a height-capped, internally
   *  scrolling region so a long list can't consume the whole sidebar. */
  bodyClassName?: string;
  bodyStyle?: React.CSSProperties;
}) {
  const [open, setOpen] = usePersistedToggle(storageKey);
  const isOpen = collapsible ? open : true;
  const toggle = () => setOpen(!open);

  return (
    <section
      className={["flex flex-col", className ?? ""].join(" ")}
      style={{ borderColor: "var(--th-border)" }}
    >
      {collapsible ? (
        <button
          type="button"
          onClick={toggle}
          aria-expanded={isOpen}
          title={isOpen ? "Collapse" : "Expand"}
          className="flex w-full items-center gap-1 px-2 pt-2 pb-1 text-xs font-semibold uppercase tracking-wide opacity-80 hover:opacity-100"
          style={{ color: "var(--th-fg-muted)" }}
        >
          <ChevronIcon open={isOpen} />
          <span className="min-w-0 flex-1 truncate text-left">{title}</span>
          {count != null && <CountBadge n={count} />}
        </button>
      ) : (
        <div
          className="flex w-full items-center gap-1 px-2 pt-2 pb-1 text-xs font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg-muted)" }}
        >
          <span className="min-w-0 flex-1 truncate">{title}</span>
          {count != null && <CountBadge n={count} />}
        </div>
      )}
      {/* Animate open/close by transitioning the grid row 0fr↔1fr; the body
          stays mounted and the inner wrapper clips it as it collapses. */}
      <div
        className="grid"
        style={{
          gridTemplateRows: isOpen ? "1fr" : "0fr",
          transition: "grid-template-rows 200ms ease",
        }}
      >
        <div style={{ overflow: "hidden", minHeight: 0 }}>
          {bodyClassName || bodyStyle ? (
            <div className={bodyClassName} style={bodyStyle}>
              {children}
            </div>
          ) : (
            children
          )}
        </div>
      </div>
    </section>
  );
}
