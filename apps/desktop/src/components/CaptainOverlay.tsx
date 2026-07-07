// The CAPTAIN OVERLAY (captain-overlay + captain-list): ONE floating,
// draggable, resizable panel that summons the ACTIVE pinned captain above
// whatever workspace tab is active. Summoned by the `toggleCaptainOverlay`
// command (Ctrl+B C by default - pressing it again CYCLES pinned captains),
// the titlebar anchor dropdown, the header switcher chips, or the palette;
// Esc closes it and restores focus. Switching captains reuses the same panel
// geometry - only the terminal slot re-targets.
//
// RENDERING CONTRACT (why this mounts inside TerminalPoolLayer): the xterm for
// every terminal lives in the pool overlay (#20), whose `z-0` class makes it a
// stacking context. For the panel chrome to paint ABOVE other visible terminals
// while the captain's own xterm paints ABOVE the panel body, all three must
// share that one stacking context: the panel carries z-index 1 (over the
// z-auto pooled wrappers) and the pool gives the captain's wrapper z-index 2
// while the overlay is open. The panel's body is an empty placeholder
// registered via useTerminalSlot - the SAME takeover mechanism the fullscreen
// layer uses - so the single pooled TerminalView is simply repositioned into
// the overlay (no second xterm, no second attach, no tmux geometry fights) and
// released back to the tile placeholder on close.
//
// The panel deliberately has NO backdrop: everything outside it stays visible
// AND interactive (clicking another tile focuses it; the overlay stays up).
import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { Anchor } from "lucide-react";
import {
  useCaptain,
  CAPTAIN_MIN_WIDTH,
  CAPTAIN_MIN_HEIGHT,
} from "../store/captain";
import { useWorkspace, tabIdForTerminal } from "../store/workspace";
import { useSupervision, sessionStatusForTmux } from "../store/supervision";
import { sessionNameForTerminal } from "../store/sessionContext";
import { useActivity } from "../store/activity";
import { useTerminalSlot, requestPoolSync } from "./TerminalPool";
import { StatusIndicator, terminalVariant } from "./StatusIndicator";
import { repaintAllTerminals } from "../lib/repaint";

/** Margin kept between the panel and the canvas edges when clamping. */
const EDGE_PAD = 8;

/** Clamp panel geometry so it stays grabbable inside the canvas container. */
function clampGeometry(
  g: { x: number; y: number; width: number; height: number },
  parentW: number,
  parentH: number,
): { x: number; y: number; width: number; height: number } {
  const width = Math.min(
    Math.max(CAPTAIN_MIN_WIDTH, g.width),
    Math.max(CAPTAIN_MIN_WIDTH, parentW - EDGE_PAD * 2),
  );
  const height = Math.min(
    Math.max(CAPTAIN_MIN_HEIGHT, g.height),
    Math.max(CAPTAIN_MIN_HEIGHT, parentH - EDGE_PAD * 2),
  );
  const x = Math.min(Math.max(EDGE_PAD, g.x), Math.max(EDGE_PAD, parentW - width - EDGE_PAD));
  const y = Math.min(Math.max(EDGE_PAD, g.y), Math.max(EDGE_PAD, parentH - height - EDGE_PAD));
  return { x, y, width, height };
}

/** The last non-empty segment of a cwd (tolerant of either separator + trailing
 *  slashes) - the folder/worktree the session lives in. */
function cwdBasename(cwd: string | undefined): string {
  if (!cwd) return "";
  const parts = cwd
    .replace(/[/\\]+$/, "")
    .split(/[/\\]+/)
    .filter(Boolean);
  return parts[parts.length - 1] ?? "";
}

/** The STABLE captain identity, derivation order: the user's RENAME first, then
 *  the cwd basename (the folder/worktree the session lives in), then the
 *  WORKSPACE TAB name, then the short id. cwd beats the tab name because a tab
 *  is a GROUPING, not a per-captain identity: several unrelated captains can
 *  share one tab (e.g. an "appturnity" tab holding an appturnity session, the
 *  orchestrator, and the t-hub captain), and preferring the tab name collapsed
 *  them all to "appturnity". Their cwds are distinct, so cwd-first keeps them
 *  apart (orchestrator, t-hub-app, monorepo-app, appturnity). It DELIBERATELY
 *  never uses the volatile Claude-suggested session title (which shows junk like
 *  "task notification") - identity must be stable across turns. Pure function so
 *  callers pass already-subscribed values (the sidebar rows) or use the hook
 *  below (the overlay + deck). */
export function stableCaptainIdentity(
  userLabel: string | undefined,
  workspaceName: string | undefined,
  cwd: string | undefined,
  terminalId: string,
): string {
  const named = (userLabel ?? "").trim();
  if (named) return named;
  const base = cwdBasename(cwd);
  if (base) return base;
  const ws = (workspaceName ?? "").trim();
  if (ws) return ws;
  return terminalId.slice(0, 8);
}

/** The user-facing display label for a captain terminal, resolved to the STABLE
 *  identity (rename -> workspace tab name -> cwd basename -> short id; NEVER the
 *  Claude title). Reads the raw user rename from `userLabels` (not the merged
 *  `labels`, which folds in Claude titles). Shared by the overlay switcher, the
 *  titlebar dropdown, the deck tiles, and the palette so a captain reads
 *  identically everywhere. */
export function useCaptainDisplayLabel(terminalId: string): string {
  const userLabel = useWorkspace((s) => s.userLabels[terminalId]);
  const cwd = useWorkspace((s) => s.terminals[terminalId]?.cwd);
  const workspaceName = useWorkspaceNameForTerminal(terminalId);
  return stableCaptainIdentity(userLabel, workspaceName, cwd, terminalId);
}

/** The NAME of the workspace tab holding `terminalId`'s tile, or undefined
 *  when the tile is gone (tab popped out to a satellite / terminal killed) -
 *  undefined doubles as the liveness affordance (dim + "tile not available").
 *  Shared by the titlebar dropdown rows and the sidebar captain rows so the
 *  workspace-context lookup cannot drift between the two surfaces. Built on
 *  tabIdForTerminal (memoized O(1) per tile) rather than a per-row scan. */
export function useWorkspaceNameForTerminal(
  terminalId: string,
): string | undefined {
  return useWorkspace((s) => {
    const tabId = tabIdForTerminal(s, terminalId);
    if (tabId === undefined) return undefined;
    return s.tabs.find((t) => t.id === tabId)?.name;
  });
}

/** The per-captain status dot: the same precise indicator the tiles/sidebar
 *  render (bound agent session status, falling back to output activity for a
 *  session-less shell). Shared by the overlay switcher + titlebar dropdown. */
export function CaptainStatusDot({
  terminalId,
  size = 8,
}: {
  terminalId: string;
  size?: number;
}) {
  const state = useWorkspace(
    (s) => s.terminals[terminalId]?.state ?? "starting",
  );
  const sessionStatus = useSupervision((s) =>
    sessionStatusForTmux(s, sessionNameForTerminal(terminalId)),
  );
  const outputActive = useActivity((s) => !!s.active[terminalId]);
  return (
    <StatusIndicator
      variant={terminalVariant(state, sessionStatus, outputActive)}
      size={size}
      className="shrink-0"
    />
  );
}

/**
 * The overlay host. Always mounted (inside TerminalPoolLayer); renders nothing
 * until the overlay is open with a live captain. Splitting the inner panel out
 * lets the host keep an unconditional effect (the WebView2 repaint nudge).
 */
export function CaptainOverlay() {
  const open = useCaptain((s) => s.open);
  const captainId = useCaptain((s) => s.activeCaptainId);

  // Toggling a floating surface over the WebGL terminals can leave WebView2 on
  // a stale blank frame (the "muted" bug) - force a repaint on every open/close
  // flip, mirroring Canvas's spawn-menu toggle and PreviewOverlay.
  useEffect(() => {
    repaintAllTerminals();
  }, [open]);

  // Safety net: if the captain's tile disappears while the overlay is up (tab
  // popped out to a satellite, tile removed by a path that skirts the workspace
  // cleanup), drop the overlay rather than float an empty frame. Kill paths go
  // through cleanupTileSideState -> forgetCaptain, which also unpins.
  const captainHasTile = useWorkspace(
    (s) =>
      captainId != null && s.tabs.some((t) => t.order.includes(captainId)),
  );
  useEffect(() => {
    if (open && !captainHasTile) useCaptain.getState().closeOverlay();
  }, [open, captainHasTile]);

  if (!open || !captainId || !captainHasTile) return null;
  return <CaptainPanel captainId={captainId} />;
}

function CaptainPanel({ captainId }: { captainId: string }) {
  // The captain's xterm is positioned over this placeholder by the pool. While
  // the panel is mounted the tile's own placeholder yields (slotActive=false in
  // TabGrid/Canvas), so this registration can't race it.
  const slotRef = useTerminalSlot(captainId);
  const panelRef = useRef<HTMLDivElement | null>(null);

  const captainIds = useCaptain((s) => s.captainIds);

  const x = useCaptain((s) => s.x);
  const y = useCaptain((s) => s.y);
  const width = useCaptain((s) => s.width);
  const height = useCaptain((s) => s.height);

  // First-open placement + re-open clamping. With no persisted position yet,
  // default to the bottom-right of the canvas (clear of the sidebar, near the
  // FAB corner but padded). Committed to the store so it persists.
  useLayoutEffect(() => {
    const el = panelRef.current;
    const parent = el?.parentElement; // the pool layer, inset-0 of the canvas
    if (!el || !parent) return;
    const pw = parent.clientWidth;
    const ph = parent.clientHeight;
    if (pw <= 0 || ph <= 0) return; // mid-reflow; keep whatever we have
    const s = useCaptain.getState();
    const start = {
      x: s.x ?? pw - s.width - 24,
      y: s.y ?? ph - s.height - 56,
      width: s.width,
      height: s.height,
    };
    const clamped = clampGeometry(start, pw, ph);
    if (
      clamped.x !== s.x ||
      clamped.y !== s.y ||
      clamped.width !== s.width ||
      clamped.height !== s.height
    ) {
      s.setGeometry(clamped);
    }
    // Run once per mount (per overlay open): the store then drives geometry.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // NOTE: Esc handling deliberately does NOT live here. Canvas owns the one
  // window-capture Escape listener and routes it through lib/escOverlays, so
  // the overlay-vs-fullscreen precedence (and the Shift+Esc literal-Esc
  // passthrough to the captain) is a single explicit order instead of two
  // competing capture listeners racing on registration order.

  // Focus nudge on open: setFocus(captainId) (done by openOverlay) re-focuses
  // the pooled xterm via Terminal.tsx's focus effect, but that effect only
  // re-runs when focusedId/focusedRegion CHANGE - if the captain was already
  // the focused tile, nothing fires. Poke the xterm's hidden input sink
  // directly (same class Canvas's keymap special-cases) once layout settles.
  useEffect(() => {
    const raf = requestAnimationFrame(() => {
      const sink = document.querySelector<HTMLTextAreaElement>(
        `[data-th-pool-tile="${captainId}"] .xterm-helper-textarea`,
      );
      sink?.focus();
    });
    return () => cancelAnimationFrame(raf);
  }, [captainId]);

  // --- Drag (header) + resize (corner handle), pointer-based like every other
  // T-Hub drag. During the gesture we write left/top/width/height straight onto
  // the panel DOM (no React churn) and ask the pool for an imperative re-sync
  // (rAF-coalesced) so the captain's xterm glides with the frame; pointer-up
  // commits the final geometry to the store (which persists it).
  const gestureRef = useRef<{
    kind: "move" | "resize";
    startX: number;
    startY: number;
    baseX: number;
    baseY: number;
    baseW: number;
    baseH: number;
  } | null>(null);
  const syncRafRef = useRef(0);
  const scheduleSync = useCallback(() => {
    if (syncRafRef.current) return;
    syncRafRef.current = requestAnimationFrame(() => {
      syncRafRef.current = 0;
      requestPoolSync("captain-gesture");
    });
  }, []);

  const beginGesture = (kind: "move" | "resize") => (e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const el = panelRef.current;
    if (!el) return;
    const s = useCaptain.getState();
    gestureRef.current = {
      kind,
      startX: e.clientX,
      startY: e.clientY,
      baseX: s.x ?? el.offsetLeft,
      baseY: s.y ?? el.offsetTop,
      baseW: s.width,
      baseH: s.height,
    };
    const handle = e.currentTarget as HTMLElement;
    try {
      handle.setPointerCapture(e.pointerId);
    } catch {
      /* best-effort; window listeners still track */
    }
    document.body.style.userSelect = "none";
    document.body.style.cursor = kind === "move" ? "grabbing" : "nwse-resize";

    const applied = { x: 0, y: 0, width: 0, height: 0 };
    const apply = (clientX: number, clientY: number) => {
      const g = gestureRef.current;
      const parent = el.parentElement;
      if (!g || !parent) return;
      const dx = clientX - g.startX;
      const dy = clientY - g.startY;
      const raw =
        g.kind === "move"
          ? { x: g.baseX + dx, y: g.baseY + dy, width: g.baseW, height: g.baseH }
          : { x: g.baseX, y: g.baseY, width: g.baseW + dx, height: g.baseH + dy };
      const c = clampGeometry(raw, parent.clientWidth, parent.clientHeight);
      applied.x = c.x;
      applied.y = c.y;
      applied.width = c.width;
      applied.height = c.height;
      el.style.left = `${c.x}px`;
      el.style.top = `${c.y}px`;
      el.style.width = `${c.width}px`;
      el.style.height = `${c.height}px`;
      scheduleSync();
    };

    const onMove = (ev: PointerEvent) => apply(ev.clientX, ev.clientY);
    const onUp = () => {
      cleanup();
      if (applied.width > 0) useCaptain.getState().setGeometry(applied);
      // Land the settled position exactly (the store write re-renders with the
      // same values; one more sync snaps the xterm if the last rAF was stale).
      requestPoolSync("captain-gesture-end");
    };
    const cleanup = () => {
      gestureRef.current = null;
      window.removeEventListener("pointermove", onMove, true);
      window.removeEventListener("pointerup", onUp, true);
      window.removeEventListener("pointercancel", onUp, true);
      document.body.style.removeProperty("user-select");
      document.body.style.removeProperty("cursor");
      try {
        handle.releasePointerCapture(e.pointerId);
      } catch {
        /* already released */
      }
    };
    window.addEventListener("pointermove", onMove, true);
    window.addEventListener("pointerup", onUp, true);
    window.addEventListener("pointercancel", onUp, true);
  };

  return (
    <div
      ref={panelRef}
      data-captain-overlay=""
      // pointer-events-auto: the pool layer is click-through; the panel isn't.
      // z-index 1 puts the chrome above every z-auto pooled wrapper; the pool
      // lifts the captain's own wrapper to 2 so its xterm paints over the body.
      className="pointer-events-auto absolute flex flex-col overflow-hidden rounded-lg border shadow-2xl"
      style={{
        left: x ?? undefined,
        top: y ?? undefined,
        width,
        height,
        zIndex: 1,
        backgroundColor: "var(--th-tile-bg)",
        borderColor: "color-mix(in srgb, var(--th-accent) 55%, var(--th-border))",
        boxShadow:
          "0 0 0 1px color-mix(in srgb, var(--th-accent) 35%, transparent), 0 12px 40px -8px rgba(0,0,0,0.7)",
      }}
      // Clicking anywhere on the panel keeps/returns focus on the captain.
      onPointerDown={() => useWorkspace.getState().setFocus(captainId)}
    >
      {/* Header: drag handle + identity + close. */}
      <div
        onPointerDown={beginGesture("move")}
        className="th-captain-drag-handle flex h-7 shrink-0 cursor-grab touch-none select-none items-center gap-2 border-b px-2 active:cursor-grabbing"
        style={{
          backgroundColor:
            "color-mix(in srgb, var(--th-accent) 12%, var(--th-header-bg))",
          borderColor: "var(--th-border)",
          fontSize: "var(--th-font-size)",
        }}
        title="Captain - drag to move · Esc dismisses · Shift+Esc sends Esc to the captain (interrupt)"
      >
        <Anchor
          size="1em"
          className="shrink-0"
          style={{ color: "var(--th-accent)" }}
          aria-hidden
        />
        {/* Captain switcher (captain-list): one chip per pinned captain, MRU
            order, status dot + name; clicking switches the SAME panel to that
            captain (geometry untouched). The active captain reads highlighted. */}
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-hidden">
          {captainIds.map((id) => (
            <CaptainSwitcherChip
              key={id}
              terminalId={id}
              active={id === captainId}
            />
          ))}
        </div>
        <span
          className="ml-auto shrink-0 text-[10px]"
          style={{ color: "var(--th-fg-muted)" }}
          title="Esc dismisses the overlay · Shift+Esc sends a literal Esc to the captain (interrupt) · Ctrl+B C cycles pinned captains"
        >
          {captainIds.length > 1
            ? "Ctrl+B C cycle · Esc dismiss"
            : "Esc dismiss · Shift+Esc interrupt"}
        </span>
        <button
          type="button"
          aria-label="Close captain overlay"
          title="Close (Esc)"
          className="flex h-5 w-5 shrink-0 items-center justify-center rounded hover:bg-neutral-700"
          style={{ color: "var(--th-fg-muted)" }}
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => useCaptain.getState().closeOverlay()}
        >
          ×
        </button>
      </div>

      {/* Body: the empty placeholder the pool positions the captain xterm over. */}
      <div ref={slotRef} className="min-h-0 flex-1 overflow-hidden" />

      {/* Footer strip: hosts the resize grip. It must live OUTSIDE the body
          placeholder - the captain's xterm wrapper is a sibling painted ABOVE
          this whole panel (pool z-index 2 vs the panel's 1) and it covers the
          placeholder rect exactly, so a corner handle overlapping the body
          would be unreachable. A thin strip below the placeholder stays clear. */}
      <div
        className="flex h-3 shrink-0 items-center justify-end border-t"
        style={{
          backgroundColor: "var(--th-header-bg)",
          borderColor: "var(--th-border)",
        }}
      >
        <div
          onPointerDown={beginGesture("resize")}
          role="separator"
          aria-label="Resize captain overlay"
          title="Drag to resize"
          className="th-captain-resize-handle flex h-3 w-6 cursor-nwse-resize touch-none items-center justify-center"
        >
          <svg
            width="14"
            height="8"
            viewBox="0 0 14 8"
            aria-hidden
            className="pointer-events-none"
            style={{ color: "var(--th-fg-muted)" }}
          >
            <path
              d="M12 1 5 8 M12 5 9 8"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              fill="none"
            />
          </svg>
        </div>
      </div>
    </div>
  );
}

/** One switcher chip: status dot + truncated name. Clicking summons that
 *  captain into the same panel (summonCaptain: MRU front + focus). A pin whose
 *  tile is gone (tab popped out to a satellite) renders dimmed - summoning it
 *  is a store-level no-op until the tile returns. */
function CaptainSwitcherChip({
  terminalId,
  active,
}: {
  terminalId: string;
  active: boolean;
}) {
  const label = useCaptainDisplayLabel(terminalId);
  const hasTile = useWorkspace((s) =>
    s.tabs.some((t) => t.order.includes(terminalId)),
  );
  return (
    <button
      type="button"
      // Chips live on the drag handle: swallow pointerdown so a click never
      // starts a panel drag (mirrors the close button).
      onPointerDown={(e) => e.stopPropagation()}
      onClick={() => useCaptain.getState().summonCaptain(terminalId)}
      title={
        !hasTile
          ? `${label} - tile not available (tab popped out?)`
          : active
            ? `Captain - ${label} (shown)`
            : `Switch to captain - ${label}`
      }
      aria-pressed={active}
      className="flex min-w-0 shrink items-center gap-1 rounded px-1.5 py-0.5 text-xs"
      style={{
        backgroundColor: active
          ? "color-mix(in srgb, var(--th-accent) 25%, transparent)"
          : "transparent",
        color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
        fontWeight: active ? 600 : 400,
        opacity: hasTile ? 1 : 0.5,
      }}
    >
      <CaptainStatusDot terminalId={terminalId} size={8} />
      <span className="max-w-[9rem] truncate">{label}</span>
    </button>
  );
}
