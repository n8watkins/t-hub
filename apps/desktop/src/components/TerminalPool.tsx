// Persistent terminal pool (#20 — seamless moves).
//
// Problem: TermHub keeps every tab mounted, but a terminal's xterm instance is
// created by <TerminalView> *inside* its grid cell. Moving a tile to another tab
// reparents that TerminalView (tab A's grid -> tab B's grid), which remounts and
// reattaches xterm — a visible reload/flash. Moving/resizing into a different
// slot makes xterm refit mid-relayout and the content squishes briefly.
//
// Fix: render each terminal's <TerminalView> EXACTLY ONCE, in a single
// absolutely-positioned pool layer that NEVER changes parent. The grid cells keep
// their header chrome and expose an empty, ref'd *placeholder* box where the
// terminal body should sit. A layout-sync effect positions each pooled terminal
// (position:absolute; left/top/width/height) on top of its current placeholder.
// Because the pooled TerminalView keeps a stable React key + parent across tab
// moves, reorders and resizes, xterm is never remounted/reattached — a move only
// repositions the existing instance. Terminal.tsx's own ResizeObserver refits it
// when its box changes size, so we never touch Terminal.tsx.
//
// Placeholders that belong to an inactive tab (or that don't exist this render,
// e.g. a terminal that has no tile yet) are kept mounted but hidden
// (visibility:hidden) and parked offscreen so they stay attached in the
// background, exactly like before.
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useWorkspace } from "../store/workspace";
import { usePanels } from "../store/panels";
import { TerminalView } from "./Terminal";
import type { TerminalId } from "../ipc/types";
// Diagnostics: tlog mirrors every pool show/park decision into a file the
// orchestrator can read from a RELEASE build (no devtools). Importing this here
// also self-installs the console/window diag hooks once at app startup (the pool
// mounts with the canvas) — see src/lib/diag.ts.
import { tlog } from "../lib/diag";

// Upper bound on the chained deferred re-syncs the pool will schedule while an
// active-tab terminal's placeholder rect is still degenerate. A transient
// mid-reorder reflow settles in 1-2 frames; this generous ceiling tolerates a
// slow multi-pass layout yet stops a PERMANENTLY-zero rect (pathological CSS)
// from spinning a re-sync every frame forever. Reset whenever a real measure
// lands (see deferredRetriesRef).
const MAX_DEFERRED_RETRIES = 10;

// Absolute ceiling on the first-paint re-arm chain (BUG 2). Before the first
// healthy active SHOW we keep the deferred budget topped up so the startup
// hydration window (no container / zero rects for a few frames) always resolves
// to a real measure and the muted flash can't persist. This caps that
// keep-trying at ~60 frames (~1s) so a genuinely pathological layout (active
// terminals whose placeholders never lay out at all) still stops spinning a
// re-sync every frame forever, rather than relying solely on firstPaintSettled.
const MAX_FIRST_PAINT_RETRIES = 60;

// ---------------------------------------------------------------------------
// Placeholder registry (context). Each Tile registers the empty body box it
// renders, keyed by terminal id; the pool reads the registry to know where to
// position each pooled terminal. A monotonic `version` bumps whenever the set of
// placeholders changes (mount/unmount) so the pool re-syncs immediately.
// ---------------------------------------------------------------------------

interface PoolRegistry {
  /** Register (or replace) the body placeholder element for a terminal id. */
  register: (id: TerminalId, el: HTMLElement) => void;
  /** Remove a terminal id's placeholder (on tile unmount). */
  unregister: (id: TerminalId, el: HTMLElement) => void;
  /** Ask the pool to re-measure now (e.g. after a layout-affecting change). */
  requestSync: () => void;
}

const PoolContext = createContext<PoolRegistry | null>(null);

/**
 * Register this element as terminal `id`'s body placeholder for the lifetime of
 * the calling component. The pool positions `id`'s pooled terminal over it.
 * No-op outside a <TerminalPoolProvider> (so Tile still works standalone).
 */
export function useTerminalSlot(id: TerminalId) {
  const reg = useContext(PoolContext);
  const ref = useRef<HTMLDivElement | null>(null);
  const setRef = useCallback(
    (el: HTMLDivElement | null) => {
      const prev = ref.current;
      if (prev === el) return;
      if (prev && reg) reg.unregister(id, prev);
      ref.current = el;
      if (el && reg) reg.register(id, el);
    },
    [id, reg],
  );
  return setRef;
}

// ---------------------------------------------------------------------------
// Provider + pool layer.
// ---------------------------------------------------------------------------

export interface TerminalPoolProps {
  children: React.ReactNode;
}

/**
 * Wraps the canvas body. Renders the children (the tab grids, which contain the
 * tile headers + placeholders) and, on top of them, the pooled terminal layer.
 * Both share this single `position:relative` box so a placeholder's rect maps
 * directly onto the absolute coordinates of its pooled terminal.
 */
export function TerminalPoolProvider({ children }: TerminalPoolProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  // id -> placeholder element. A plain ref (not state) so registration never
  // re-renders the provider; the pool re-syncs off the `version` bump instead.
  const slotsRef = useRef<Map<TerminalId, HTMLElement>>(new Map());
  const [version, setVersion] = useState(0);
  const bump = useCallback(() => setVersion((v) => v + 1), []);

  const registry = useMemo<PoolRegistry>(
    () => ({
      register: (id, el) => {
        slotsRef.current.set(id, el);
        bump();
      },
      unregister: (id, el) => {
        // Only delete if the current entry is still this element (guards against
        // a remount registering the new node before the old one unregisters).
        if (slotsRef.current.get(id) === el) {
          slotsRef.current.delete(id);
          bump();
        }
      },
      requestSync: bump,
    }),
    [bump],
  );

  return (
    <PoolContext.Provider value={registry}>
      <div ref={containerRef} className="relative h-full w-full">
        {children}
        <TerminalPoolLayer
          containerRef={containerRef}
          slotsRef={slotsRef}
          version={version}
        />
      </div>
    </PoolContext.Provider>
  );
}

interface PoolLayerProps {
  containerRef: React.RefObject<HTMLDivElement | null>;
  slotsRef: React.RefObject<Map<TerminalId, HTMLElement>>;
  /** Bumps whenever the placeholder set changes; re-runs the position sync. */
  version: number;
}

/**
 * The absolute overlay holding one pooled <TerminalView> per live terminal. The
 * wrappers are keyed by terminal id and never unmount while the terminal exists,
 * so xterm stays mounted/attached across tab moves and reorders. A layout-sync
 * effect positions each wrapper over its placeholder; wrappers with no visible
 * placeholder are hidden and parked.
 */
function TerminalPoolLayer({ containerRef, slotsRef, version }: PoolLayerProps) {
  // Every terminal id that has a tile somewhere (across ALL tabs), de-duped.
  // We render the union of tab orders rather than the live `terminals` map so a
  // wrapper exists exactly for tiles the user placed (and survives tab switches).
  const tabs = useWorkspace((s) => s.tabs);
  const activeTabId = useWorkspace((s) => s.activeTabId);
  // The focused tile id. A header click sets ONLY this (setFocus) — it does NOT
  // change `tabs`/`activeTabId`, so the position layout-effect below would never
  // re-run on focus. We subscribe to it here purely to drive a rAF re-sync after
  // a focus/selection change (see the focus effect), so positions always settle.
  const focusedId = useWorkspace((s) => s.focusedId);

  // Per-tile panel state (usePanels) now gates terminal visibility too: a pooled
  // terminal is SHOWN only when its tile's active tab is "terminal" (the Files/
  // Preview/Dev surfaces render in the tile body instead and must not be covered
  // by the xterm), and only when it isn't eclipsed by ANOTHER tile's fullscreen.
  // We subscribe to BOTH the tab map and fullscreenId so switching a tile's tab
  // or toggling fullscreen re-renders this layer, which re-runs the position/
  // visibility sync via the layout-effect below (panelTab/fullscreenId are in
  // its dep list). The whole `tab` record object is a fresh reference whenever
  // any tile's tab changes (see panels.setTab), so this is a sufficient trigger.
  const panelTab = usePanels((s) => s.tab);
  const fullscreenId = usePanels((s) => s.fullscreenId);
  // Also re-sync when any tile's panel is expanded/collapsed: that flips whether
  // the terminal half exists, so its xterm must show (split) or park (expanded).
  const panelExpanded = usePanels((s) => s.panelExpanded);

  // THE FIX (mutedbug): poolIds must be a STABLE DOM order that does NOT change
  // when a tile is reordered or moved between tabs. Positioning is absolute
  // (each wrapper is placed by its `transform`), so the DOM order of the
  // wrappers is irrelevant to layout. But React keys the wrappers off this list:
  // if the list reorders, React physically MOVES the wrapper <div> (and its
  // <canvas>) in the DOM. WebView2 blanks a WebGL/canvas element the instant it
  // is detached/re-inserted during such a move, so every reorder muted the grid.
  //
  // We therefore keep ids in their first-seen ("established") pool order forever:
  // reconcile each render by keeping the current order filtered to ids that are
  // still present, then appending any newly-seen ids at the end. An id is never
  // moved once placed, so a reorder only changes each wrapper's transform.
  const poolOrderRef = useRef<TerminalId[]>([]);
  const poolIds = useMemo(() => {
    const present = new Set<TerminalId>();
    for (const t of tabs) for (const id of t.order) present.add(id);
    // 1) keep established order, dropping ids whose tiles are gone.
    const next = poolOrderRef.current.filter((id) => present.has(id));
    // 2) append any newly-seen ids at the END (never reorder an existing id).
    const known = new Set(next);
    for (const id of present) if (!known.has(id)) next.push(id);
    poolOrderRef.current = next;
    return next;
  }, [tabs]);

  // Stable wrapper element refs so the sync can write inline position styles
  // imperatively (no React re-render per pointer-move while resizing/dragging).
  const wrapRefs = useRef<Map<TerminalId, HTMLDivElement>>(new Map());

  // Last transform we wrote per wrapper, so the sync can tell when a VISIBLE
  // terminal actually moved (a same-tab reorder repositions it while it stays
  // on-screen, so the Terminal's IntersectionObserver never fires). When a
  // visible terminal's transform changes we dispatch a "th-pool-moved" event on
  // its wrapper; Terminal.tsx listens for it and forces a repaint, so the new
  // position never shows a stale/blank WebGL frame. Belt-and-suspenders on top
  // of the stable poolIds order above.
  const lastTransformRef = useRef<Map<TerminalId, string>>(new Map());

  // THE MUTED-BUG FIX (last-good rect cache). Per terminal id we remember the
  // last NON-degenerate placeholder geometry we measured while it was on the
  // active tab: the on-screen offset (relative to the container base) plus its
  // width/height. During a reorder, `moveTile` drops manual sizes and the grid
  // reflows; a sync that lands mid-reflow can read a transient ZERO rect (or a
  // zero container base). Previously that parked the ACTIVE-tab terminal
  // (visibility:hidden + offscreen) and it STAYED parked -- a focus click does
  // not re-run sync, the store's `tabs`/`activeTabId` don't change -- so the
  // whole grid read blank until a tab switch re-synced. Now an active-tab
  // terminal is NEVER hidden for a transient/zero rect: we re-apply its
  // last-good geometry and schedule a deferred re-sync that lands the correct
  // position once the reflow settles.
  const lastGoodRectRef = useRef<
    Map<TerminalId, { offsetX: number; offsetY: number; width: number; height: number }>
  >(new Map());

  // Derive, per render, which tab each terminal lives in — so the sync knows
  // whether its placeholder belongs to the *active* (displayed) tab. A terminal
  // on an inactive tab has a placeholder in the DOM (every tab stays mounted),
  // but that placeholder has a zero-area rect (its tab is display:none), so we
  // additionally gate on tab activity to avoid stacking it at 0,0.
  const tabOfId = useMemo(() => {
    const m = new Map<TerminalId, string>();
    for (const t of tabs) for (const id of t.order) m.set(id, t.id);
    return m;
  }, [tabs]);

  // A deferred re-sync scheduled on the next animation frame. A sync that lands
  // mid-reflow (transient zero rect / zero container base) is always followed by
  // one that reads settled geometry, so any active-tab terminal pinned to its
  // last-good rect snaps to the correct position once layout settles. Coalesced:
  // many triggers in a frame schedule at most one follow-up.
  const deferredRafRef = useRef(0);
  // Bounds the deferred-rAF retry chain so a PERMANENTLY-degenerate active rect
  // (pathological CSS, not a transient reflow) can't spin a re-sync every frame
  // forever. Reset to 0 whenever any healthy SHOW lands (real layout settled);
  // a reorder/tab-switch's `useLayoutEffect` always starts a fresh chain anyway.
  const deferredRetriesRef = useRef(0);

  // Always-current pointer to the latest `sync`, so the abort paths (and the
  // very-first-paint settle) can re-arm a deferred re-sync without forming a
  // useCallback dependency cycle with `sync` itself. Set after `sync` is built.
  const syncRef = useRef<(trigger: string) => void>(() => {});

  // Whether the pool has ever landed a healthy SHOW (a real, non-degenerate
  // active-tab rect). Until it has, the FIRST layout passes can read no
  // container / zero rects (the placeholder set + persisted tabs are still
  // hydrating), during which the wrappers sit at their initial offscreen park
  // (translate(-100000px)) — so the active grid cells flash their muted
  // background for the ~frame(s) before the first real measure lands. We use
  // this to keep re-arming a next-frame re-sync through that window (instead of
  // bailing on an abort and waiting for an external trigger), so the active
  // terminals snap on as soon as their rects exist — collapsing the flash.
  const firstPaintSettledRef = useRef(false);
  // Counts first-paint re-arms so the keep-trying window is itself bounded (see
  // MAX_FIRST_PAINT_RETRIES) and can't spin forever on a pathological layout.
  const firstPaintRetriesRef = useRef(0);

  // Schedule (or re-arm) the deferred next-frame re-sync. Coalesced to one
  // pending rAF. Bounded by the retry budget so a permanently-degenerate layout
  // can't spin forever; before the first healthy paint we keep the budget topped
  // up (within MAX_FIRST_PAINT_RETRIES) so the initial hydration window — which
  // can span several frames — always resolves to a real measure.
  const scheduleDeferredSync = useCallback((reason: string) => {
    // During the very first paint window, don't let the budget run dry: each
    // healthy SHOW resets it to 0 anyway, so this only matters while rects are
    // still degenerate (no visible terminal yet) — exactly when we must keep
    // trying so the muted flash can't persist. Reset BEFORE the exhaustion guard
    // so a slow multi-frame hydration never stalls before the first real measure,
    // but stop once the absolute first-paint ceiling is hit so a layout that
    // never lays out at all doesn't re-arm a frame forever.
    if (!firstPaintSettledRef.current) {
      if (firstPaintRetriesRef.current < MAX_FIRST_PAINT_RETRIES) {
        firstPaintRetriesRef.current += 1;
        deferredRetriesRef.current = 0;
      }
    }
    if (deferredRetriesRef.current >= MAX_DEFERRED_RETRIES) return;
    deferredRetriesRef.current += 1;
    if (deferredRafRef.current) cancelAnimationFrame(deferredRafRef.current);
    deferredRafRef.current = requestAnimationFrame(() => {
      deferredRafRef.current = 0;
      syncRef.current(reason);
    });
  }, []);

  // Position one wrapper as a VISIBLE active terminal: write transform/size and,
  // if it moved on-screen (a same-tab reorder/swap that the IntersectionObserver
  // can't catch), fire "th-pool-moved" so Terminal.tsx repaints rather than
  // showing a stale frame. Shared by the healthy-show and last-good-hold paths.
  const applyVisible = useCallback(
    (
      wrap: HTMLDivElement,
      transform: string,
      width: number,
      height: number,
      id: TerminalId,
    ) => {
      wrap.style.visibility = "visible";
      wrap.style.pointerEvents = "";
      wrap.style.transform = transform;
      wrap.style.width = `${width}px`;
      wrap.style.height = `${height}px`;
      const prevTransform = lastTransformRef.current.get(id);
      if (
        prevTransform !== undefined &&
        prevTransform !== transform &&
        prevTransform !== "translate(-100000px, 0px)"
      ) {
        wrap.dispatchEvent(new CustomEvent("th-pool-moved"));
      }
      lastTransformRef.current.set(id, transform);
    },
    [],
  );

  // Position every pooled terminal over its placeholder. Runs after layout
  // (useLayoutEffect) so we read settled rects and paint with no flash.
  // `trigger` tags the call site for the diag instrumentation (tlog -> file) so
  // we can SEE, on the user's machine, which path drove each show/park decision.
  //
  // THE INVARIANT (mutedbug fix), now scoped to the per-tile panel: a terminal
  // that is ELIGIBLE to show is NEVER hidden/parked by sync() for a transient/
  // zero rect — it is held visible at the best geometry we have (a freshly-
  // measured real rect, else its last-good rect) and a deferred re-sync lands
  // the settled position once layout settles. This is what stops the active grid
  // flashing blank after a reorder/focus mid-reflow.
  //
  // A terminal is ELIGIBLE (shouldShow) when ALL hold:
  //   1. its tile is on the ACTIVE workspace tab (inactive tabs are display:none
  //      so their placeholders have zero-area rects — never stack them at 0,0);
  //   2. its tile's panel tab is "terminal" (on Files/Preview/Dev the tile body
  //      renders that surface and the xterm must be parked so it doesn't cover
  //      it); and
  //   3. it is not eclipsed by ANOTHER tile's fullscreen. When some tile is
  //      fullscreen, ONLY that tile is visible (Canvas renders its placeholder in
  //      a full-window layer over everything), so every other terminal parks.
  // Terminals that are NOT eligible are parked offscreen + hidden, exactly like
  // an inactive-tab terminal always was.
  const sync = useCallback(
    (trigger: string) => {
      // Latest panel state. Read here (not via closure) so the predicate is
      // always current even when sync runs from a deferred rAF; the layout-effect
      // still re-runs sync when panelTab/fullscreenId change (they're in its dep
      // list), so a tab/fullscreen toggle always triggers a fresh sync.
      const panels = usePanels.getState();
      const fsId = panels.fullscreenId;
      const expandedMap = panels.panelExpanded;
      const tabMap = panels.tab;
      // Eligibility. On a NON-terminal tab the panel is EXPANDED by DEFAULT (fills
      // the tile) -> the terminal is PARKED (no overlay covering the panel). Only
      // when the user switches that tile to the SPLIT (panelExpanded === false)
      // does the terminal show, in its half over the placeholder. On the Terminal
      // tab it always shows. Then the usual gates: a fullscreen tile shows only
      // itself; otherwise only active-workspace-tab terminals show.
      const shouldShow = (id: TerminalId): boolean => {
        const onNonTerminal = (tabMap[id] ?? "terminal") !== "terminal";
        if (onNonTerminal && (expandedMap[id] ?? true)) return false;
        if (fsId != null) return id === fsId;
        return tabOfId.get(id) === activeTabId;
      };
      const container = containerRef.current;
      if (!container) {
        // BUG 2 (startup flicker): the first layout-effect can run before the
        // provider's container ref has attached. Don't just bail (that left the
        // wrappers parked offscreen until the next EXTERNAL trigger — a ~frame
        // gap during which the active grid cells flashed their muted
        // background). Re-arm a next-frame re-sync so we position the active
        // terminals the instant the container exists.
        tlog("pool", `sync(${trigger}) aborted: no container; re-arming re-sync`);
        scheduleDeferredSync("post-no-container");
        return;
      }
      const base = container.getBoundingClientRect();
      const slots = slotsRef.current;
      if (!slots) {
        tlog("pool", `sync(${trigger}) aborted: no slots map; re-arming re-sync`);
        scheduleDeferredSync("post-no-slots");
        return;
      }
      // A degenerate container base means the whole canvas is mid-reflow (e.g.
      // the grid is between layout passes after a reorder dropped manual sizes).
      // Reading per-placeholder rects against a zero base would compute garbage
      // offsets, so DON'T trust them: hold active terminals at last-good and
      // schedule a deferred re-sync that lands once the base settles.
      const baseDegenerate = base.width <= 0 || base.height <= 0;
      if (baseDegenerate) {
        tlog(
          "pool",
          `sync(${trigger}): container base DEGENERATE (w=${Math.round(
            base.width,
          )} h=${Math.round(base.height)}); holding active terminals at last-good ` +
            `+ scheduling deferred re-sync`,
        );
      }

      let needDeferred = baseDegenerate;

      for (const id of wrapRefs.current.keys()) {
        const wrap = wrapRefs.current.get(id);
        if (!wrap) continue;
        const slot = slots.get(id);
        const show = shouldShow(id);
        const rect = slot?.getBoundingClientRect();
        const rectOk =
          !!rect && rect.width > 0 && rect.height > 0 && !baseDegenerate;
        const rectStr = rect
          ? `${Math.round(rect.width)}x${Math.round(rect.height)}`
          : "none";

        // ===== ELIGIBLE INVARIANT: show, never park for a transient rect. =====
        if (show) {
          // Best geometry: a healthy fresh rect wins; otherwise hold last-good.
          if (slot && rectOk && rect) {
            const offsetX = rect.left - base.left;
            const offsetY = rect.top - base.top;
            lastGoodRectRef.current.set(id, {
              offsetX,
              offsetY,
              width: rect.width,
              height: rect.height,
            });
            const transform = `translate(${offsetX}px, ${offsetY}px)`;
            tlog(
              "pool",
              `sync(${trigger}) SHOW ${id} (eligible): rect ${rectStr} @ (${Math.round(
                offsetX,
              )},${Math.round(offsetY)}) base ${Math.round(
                base.width,
              )}x${Math.round(base.height)} activeTab=${activeTabId} fs=${fsId ?? "none"}`,
            );
            applyVisible(wrap, transform, rect.width, rect.height, id);
            // A real measure landed -> layout has settled for this id; reset the
            // deferred retry budget so future transient reflows get a fresh chain.
            deferredRetriesRef.current = 0;
            // First healthy eligible SHOW: the startup hydration window is over,
            // so the muted-flash guard can stop topping up the deferred budget.
            firstPaintSettledRef.current = true;
            continue;
          }

          // Transient/zero rect or degenerate base: HOLD at last-good (stay
          // visible) and schedule a deferred re-sync to land the settled
          // position. Per the invariant we do NOT park an eligible terminal.
          const lastGood = lastGoodRectRef.current.get(id);
          if (lastGood) {
            tlog(
              "pool",
              `sync(${trigger}) HOLD ${id} (eligible): degenerate rect=${rectStr} ` +
                `baseDegenerate=${baseDegenerate}; pinning to last-good ` +
                `${Math.round(lastGood.width)}x${Math.round(
                  lastGood.height,
                )} @ (${Math.round(lastGood.offsetX)},${Math.round(
                  lastGood.offsetY,
                )}) activeTab=${activeTabId}; scheduling re-sync`,
            );
            const transform = `translate(${lastGood.offsetX}px, ${lastGood.offsetY}px)`;
            applyVisible(wrap, transform, lastGood.width, lastGood.height, id);
            needDeferred = true;
            continue;
          }

          // No rect AND no last-good yet (truly never measured: e.g. just
          // mounted before its first layout). Keep it VISIBLE (invariant) but it
          // has no geometry to place — leave whatever transform it has and let
          // the deferred re-sync land a real position next frame. We deliberately
          // do NOT park it offscreen, since an eligible terminal must never blank.
          tlog(
            "pool",
            `sync(${trigger}) WAIT ${id} (eligible): no rect (${rectStr}) and no ` +
              `last-good yet; keeping visible, scheduling re-sync activeTab=${activeTabId}`,
          );
          wrap.style.visibility = "visible";
          wrap.style.pointerEvents = "";
          needDeferred = true;
          continue;
        }

        // ===== NOT ELIGIBLE: park offscreen + hidden. =====
        // Inactive tab, a non-terminal panel tab (Files/Preview/Dev showing in
        // the tile body), or eclipsed by another tile's fullscreen. Keep mounted
        // but invisible + inert, parked offscreen so it never overlaps the active
        // grid (or the in-tile surface). Park at the last known size so a hidden
        // terminal's xterm isn't forced to 0x0 (which would refit to 0 cols).
        tlog(
          "pool",
          `sync(${trigger}) PARK ${id} (not-eligible): rect=${rectStr} ` +
            `expanded=${expandedMap[id] ?? false} activeTab=${activeTabId} fs=${fsId ?? "none"}`,
        );
        wrap.style.visibility = "hidden";
        wrap.style.pointerEvents = "none";
        if (rect && rect.width > 0 && rect.height > 0) {
          wrap.style.width = `${rect.width}px`;
          wrap.style.height = `${rect.height}px`;
        }
        wrap.style.transform = "translate(-100000px, 0px)";
        lastTransformRef.current.set(id, "translate(-100000px, 0px)");
      }

      // Schedule the deferred re-sync OUTSIDE the loop so a single follow-up
      // covers every held active terminal this pass. Bounded so a permanently-
      // degenerate rect can't loop forever; the bound is generous (10 frames) so
      // a slow multi-pass reflow still resolves. The helper also keeps the budget
      // topped up through the first-paint hydration window (BUG 2) so the muted
      // flash can't persist while rects are still settling.
      if (needDeferred) {
        if (
          deferredRetriesRef.current < MAX_DEFERRED_RETRIES ||
          !firstPaintSettledRef.current
        ) {
          scheduleDeferredSync("deferred-rAF");
        } else {
          tlog(
            "pool",
            `sync(${trigger}): deferred re-sync budget exhausted ` +
              `(${MAX_DEFERRED_RETRIES} frames); active terminals held VISIBLE at ` +
              `last-good. A real resize/tab-switch will re-sync.`,
          );
        }
      }
    },
    // panelTab + fullscreenId are dependencies even though sync reads the LIVE
    // store via getState(): including them regenerates `sync` when a tile's tab
    // or the fullscreen target changes, which cascades to the position layout-
    // effect (it depends on `sync`) so visibility re-syncs immediately on a
    // tab/fullscreen toggle. tabOfId/activeTabId stay for the workspace-tab gate.
    [
      containerRef,
      slotsRef,
      tabOfId,
      activeTabId,
      panelTab,
      panelExpanded,
      fullscreenId,
      applyVisible,
      scheduleDeferredSync,
    ],
  );

  // Keep `syncRef` pointing at the latest `sync` so the deferred scheduler and
  // abort paths always invoke the current closure (no useCallback dep cycle).
  syncRef.current = sync;

  // Re-sync on every dependency that can move a placeholder OR change a
  // terminal's visibility: the placeholder set (version), the active workspace
  // tab, the tabs array (order/sizes changes all produce a new `tabs` reference
  // via the store), AND the per-tile panel state (a tile's panel tab or the
  // fullscreen target — both gate `shouldShow`). useLayoutEffect lands the
  // position/visibility before paint so a tab switch / move / panel-tab switch /
  // fullscreen toggle shows no transient mis-place or flash. (`sync` already
  // depends on panelTab/fullscreenId so it changes identity on those, but we
  // also list them here so the intent is explicit and robust.)
  //
  // A same-tab REORDER (moveTile) produces a new `tabs` reference (it drops
  // manual sizes), so this fires and re-measures -- confirming the reorder path
  // re-syncs. But the synchronous layout-effect read can still catch the grid
  // mid-reflow (transient zero rects), so we ALSO schedule a next-frame
  // re-measure tagged `layout-rAF`. The hardened sync holds active terminals at
  // their last-good rect on the transient pass; this follow-up lands the settled
  // position. (sync() already self-schedules a deferred re-sync if it had to
  // hold/park anything, so this is a belt-and-suspenders second frame that
  // always runs after a tabs/active-tab change.)
  useLayoutEffect(() => {
    sync("layout-effect");
    const raf = requestAnimationFrame(() => sync("layout-rAF"));
    return () => cancelAnimationFrame(raf);
  }, [sync, version, tabs, activeTabId, panelTab, panelExpanded, fullscreenId]);

  // Re-sync after a FOCUS/SELECTION change (mutedbug fix). Clicking a tile header
  // calls setFocus, which mutates ONLY `focusedId` — not `tabs`/`activeTabId` —
  // so the layout-effect above never fires for it. A focus click can still
  // coincide with a transient reflow (the `focused` style toggles the tile's
  // box-shadow/border-color; harmless to layout, but a stray ResizeObserver pass
  // mid-interaction historically read a zero rect). Belt-and-suspenders: whenever
  // the focused tile changes, log it and schedule a rAF re-sync so any held
  // active terminal lands its settled position. The active-tab invariant in
  // sync() already guarantees it was never parked; this just re-settles geometry.
  useEffect(() => {
    if (focusedId == null) return;
    tlog(
      "focus",
      `focusedId -> ${focusedId} (activeTab=${activeTabId}); scheduling rAF re-sync`,
    );
    const raf = requestAnimationFrame(() => sync("focus-rAF"));
    return () => cancelAnimationFrame(raf);
  }, [focusedId, activeTabId, sync]);

  // Keep terminals glued to their placeholders as the window/container resizes
  // or the flex grid reflows (gutter drags resize cells without changing `tabs`
  // until pointer-up). A ResizeObserver on the container catches container-size
  // changes; we also observe each registered placeholder so a single cell's
  // resize (e.g. a column gutter drag) repositions just-in-time. rAF-coalesced
  // so a burst of observer callbacks does one measure per frame.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let raf = 0;
    const schedule = (trigger: string) => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        sync(trigger);
      });
    };
    const ro = new ResizeObserver(() => schedule("resize-observer"));
    ro.observe(container);
    const slots = slotsRef.current;
    if (slots) for (const el of slots.values()) ro.observe(el);
    const onWindowResize = () => schedule("window-resize");
    window.addEventListener("resize", onWindowResize);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener("resize", onWindowResize);
    };
    // Re-establish observers when the placeholder set changes (version) so newly
    // mounted cells are observed and removed ones are dropped.
  }, [containerRef, slotsRef, sync, version]);

  // Cancel any pending deferred re-sync on unmount so a stray rAF can't fire
  // sync() into a torn-down layer.
  useEffect(() => {
    return () => {
      if (deferredRafRef.current) {
        cancelAnimationFrame(deferredRafRef.current);
        deferredRafRef.current = 0;
      }
    };
  }, []);

  return (
    <div
      // Pool overlay: spans the canvas body. It paints ABOVE the grid cells'
      // backgrounds (those are non-positioned, so this positioned layer wins) but
      // BELOW the gutters/intersection handles, which carry z-10/z-20 — so their
      // full negative-margin hit zones still win the pointer at cell edges. `z-0`
      // (a positioned auto-context layer) is what lands the overlay between the
      // two. The container is click-through (pointer-events:none) so headers and
      // gutters stay grabbable; each pooled terminal re-enables pointer events on
      // itself so it's interactive.
      //
      // FULLSCREEN: when a tile is fullscreen, Canvas renders that tile in a
      // full-window `fixed z-40` layer with an empty body placeholder; the pool
      // positions the fullscreen terminal over it (negative offsets relative to
      // the container reach up to the viewport origin). For the xterm to paint
      // OVER the fullscreen tile's body we lift the whole overlay above that
      // layer (z-50) while fullscreen is active — every OTHER terminal is parked
      // offscreen/hidden so only the fullscreen one is visible, and the overlay
      // stays pointer-events:none except over the terminal itself, so the
      // fullscreen header/tab bar (below the terminal body, in the z-40 layer)
      // stays clickable.
      className={
        "pointer-events-none absolute inset-0 " +
        (fullscreenId != null ? "z-50" : "z-0")
      }
      aria-hidden={false}
    >
      {poolIds.map((id) => (
        <div
          key={id}
          // data-th-pool-tile lets index.css make the wrapper pointer-inert
          // during a tile drag, so elementFromPoint falls through to the grid
          // placeholder (data-tile-id) underneath for drop resolution.
          data-th-pool-tile={id}
          // Clicking INTO a terminal (its body, to type) must focus that tile —
          // otherwise focusedId only tracked header clicks, so the Files tree
          // (which roots at the focused terminal's cwd) never followed when you
          // clicked between terminals. Capture phase so it wins before xterm.
          onPointerDownCapture={() => useWorkspace.getState().setFocus(id)}
          ref={(el) => {
            if (el) {
              wrapRefs.current.set(id, el);
              // THE MUTED-GRID FIX: only seed the offscreen park on a wrapper's
              // FIRST attach (no transform yet). This inline ref is recreated
              // every render, so React re-invokes it on EVERY pool re-render —
              // and unconditionally resetting the transform here yanked ALL
              // already-positioned terminals offscreen. When that render did NOT
              // also change tabs/version/activeTabId (e.g. opening/closing the "+"
              // spawn menu re-renders Canvas only), the positioning layout-effect
              // never re-ran, so the whole grid stayed parked offscreen = muted
              // until an unrelated re-sync. Guarding on "no transform yet" keeps an
              // existing wrapper at its current position across re-renders; a brand
              // new wrapper still gets the initial offscreen park before first sync.
              if (!el.style.transform) {
                el.style.transform = "translate(-100000px, 0px)";
              }
            } else {
              wrapRefs.current.delete(id);
            }
          }}
          className="pointer-events-auto absolute left-0 top-0 overflow-hidden"
          style={{ visibility: "hidden" }}
        >
          {/* Rendered ONCE, here, for this terminal's whole lifetime. Stable key
              + stable parent => xterm is never remounted on a tab move/reorder.
              visible is always true (pool keeps every terminal attached). */}
          <TerminalView terminalId={id} visible={true} />
        </div>
      ))}
    </div>
  );
}
