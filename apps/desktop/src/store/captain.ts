// The captain store - the "summon the orchestrator" overlay (captain-overlay,
// captain-list phase 1).
//
// Terminals can be PINNED TO THE CAPTAIN OVERLAY. A pin is presentational only:
// it does not commission the terminal, grant control, or create a fleet claim. The
// captain overlay stays ONE floating, draggable, resizable panel that renders
// the ACTIVE captain ABOVE whatever workspace tab is active; multi-captain
// means fast switching inside that single panel, not simultaneous panels.
//
// Design notes:
//   - The overlay does NOT create a second attach. The pooled <TerminalView>
//     (TerminalPool #20) stays the single xterm/attach for the session; while
//     the overlay is open it simply OWNS the pool placeholder (the tile copy
//     yields via slotActive, exactly like the fullscreen double-render), so the
//     pooled terminal is repositioned into the overlay and released back to the
//     tile on close. One viewer at a time = no tmux geometry corruption.
//   - `captainIds` is kept in MRU order (index 0 = most recently summoned) and
//     the ACTIVE captain is always the front: explicit summons move-to-front,
//     cycling ROTATES the list (so repeated Ctrl+B C round-robins through every
//     pinned captain instead of ping-ponging between the two most recent).
//   - Persistence: localStorage owns overlay membership, geometry, and MRU order.
//     The SERVER registry independently owns commissioned Captain claims. A live
//     commissioned Captain is added to the overlay when its registry snapshot is
//     adopted, but removing a claim never removes an explicit visual pin. The
//     open/closed state deliberately does not persist (the app always starts
//     with the overlay closed). The v1 single-captain blob migrates to a
//     one-entry v2 list.
//   - Focus contract: opening moves keyboard focus to the captain terminal
//     (via the workspace store's setFocus, which the pooled TerminalView
//     follows); closing restores focus to the tile that had it before. Cycling
//     while summoned re-targets focus but does NOT touch the saved pre-summon
//     tile, so Esc always returns to where the user was before the summon.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";
import { loadPersisted, savePersisted } from "../lib/persist";
import { useWorkspace, registerCaptainRegistry } from "./workspace";

const PERSIST_KEY = "t-hub.captain.v2";

/** The reserved ship slug the server captains registry gives the Cortana
 *  singleton (mirrors `CORTANA_SLUG` in control.rs). The orchestrator mark is a
 *  claim on THIS slug with `role: "cortana"`, so releasing/claiming it addresses
 *  the singleton regardless of which terminal currently holds it. */
export const CORTANA_SLUG = "cortana";
/** The pre-list single-captain key (PR #9). Read-only now: migrated into the
 *  v2 list on first load, never written again, left in place so a rollback to
 *  an older build still finds its pin. */
const LEGACY_PERSIST_KEY = "t-hub.captain.v1";

/** Overlay size bounds (CSS px). Modest floor so xterm never refits absurdly
 *  small; no ceiling - the container clamp caps it to the canvas. */
export const CAPTAIN_MIN_WIDTH = 360;
export const CAPTAIN_MIN_HEIGHT = 220;
export const CAPTAIN_DEFAULT_WIDTH = 640;
export const CAPTAIN_DEFAULT_HEIGHT = 400;

interface PersistedCaptain {
  /** Pinned captains, MRU order (front = most recently summoned = active). */
  captainIds: TerminalId[];
  /** The terminal designated as the ORCHESTRATOR (its tile lives in the reserved
   *  Captains workspace tab; marked via a tile's right-click menu). null = none.
   *  Persisted so the designation survives a relaunch. */
  orchestratorId: TerminalId | null;
  /** Overlay top-left, relative to the canvas/pool container. null until the
   *  first open computes a default placement (bottom-right-ish). */
  x: number | null;
  y: number | null;
  width: number;
  height: number;
}

const num = (v: unknown): number | null =>
  typeof v === "number" && Number.isFinite(v) ? v : null;

function coerceGeometry(p: {
  x?: unknown;
  y?: unknown;
  width?: unknown;
  height?: unknown;
}): Pick<PersistedCaptain, "x" | "y" | "width" | "height"> {
  return {
    x: num(p.x),
    y: num(p.y),
    width: Math.max(CAPTAIN_MIN_WIDTH, num(p.width) ?? CAPTAIN_DEFAULT_WIDTH),
    height: Math.max(CAPTAIN_MIN_HEIGHT, num(p.height) ?? CAPTAIN_DEFAULT_HEIGHT),
  };
}

/** A persisted terminal id: a non-empty string, else null. */
const coerceId = (v: unknown): TerminalId | null =>
  typeof v === "string" && v !== "" ? v : null;

/** Sanitize a v2 blob: `captainIds` must be a deduped list of non-empty ids. */
export function coercePersisted(raw: unknown): PersistedCaptain {
  const p = (raw ?? {}) as Partial<PersistedCaptain>;
  const ids = Array.isArray(p.captainIds)
    ? [...new Set(p.captainIds.filter((v): v is string => typeof v === "string" && v !== ""))]
    : [];
  return {
    captainIds: ids,
    orchestratorId: coerceId(p.orchestratorId),
    ...coerceGeometry(p),
  };
}

/** Convert a v1 single-captain blob (`{ captainId, x, y, width, height }`) into
 *  the v2 list shape: the one pin becomes a one-entry list, geometry carries
 *  over. Exported for the migration tests. */
export function migrateLegacyCaptain(raw: unknown): PersistedCaptain {
  const p = (raw ?? {}) as { captainId?: unknown } & Partial<PersistedCaptain>;
  const captainIds =
    typeof p.captainId === "string" && p.captainId ? [p.captainId] : [];
  return { captainIds, orchestratorId: null, ...coerceGeometry(p) };
}

function defaults(): PersistedCaptain {
  return {
    captainIds: [],
    orchestratorId: null,
    x: null,
    y: null,
    width: CAPTAIN_DEFAULT_WIDTH,
    height: CAPTAIN_DEFAULT_HEIGHT,
  };
}

/**
 * Load the persisted captain state: the v2 list when present and parseable,
 * else the v1 single pin migrated into a one-entry list (never losing an
 * existing pin), else empty defaults. A parseable v2 blob wins even when its
 * list is EMPTY (the user unpinned after migrating - the stale v1 pin must
 * not resurrect); only an absent or CORRUPT/unparseable v2 blob falls back to
 * the migrated v1 pin (pin preservation beats staleness on that edge).
 * Exported for the migration tests (the store computes this once at load).
 */
export function loadCaptainPersisted(): PersistedCaptain {
  return loadPersisted(
    PERSIST_KEY,
    loadPersisted(LEGACY_PERSIST_KEY, defaults(), migrateLegacyCaptain),
    coercePersisted,
  );
}

const initial = loadCaptainPersisted();

/** A crew member of a ship (item-2 §2.3). Crew now carry their own tile pointer +
 *  lifecycle state (was a bare tile-id string). */
export interface CrewRef {
  terminalId: string;
  claudeUuid?: string;
  provider?: "codex" | "claude";
  providerSessionId?: string;
  state?: { kind: "active" | "orphaned" | "removed"; since?: number };
}

/** One claim from the SERVER captains registry (item-2 identity re-key): the ship
 *  (the DURABLE primary key), the first-class role, the mutable terminal pointer
 *  (was `captainSessionId`; absent while the claim is orphaned/vacant), the workspace
 *  tabs it controls, and the crew it spawned. */
export interface CaptainClaimRecord {
  shipSlug: string;
  role?: "cortana" | "captain";
  claudeUuid?: string;
  provider?: "codex" | "claude";
  providerSessionId?: string;
  projectId?: string;
  assignment?: string;
  harness?: "codex" | "claude";
  conversationId?: string;
  resumePoint?: string;
  /** The mutable terminal pointer. Absent for an orphaned/vacant claim (no live
   *  tile to pin); the wire adapter only surfaces claims that HAVE one. */
  terminalId?: string;
  workspaceTabIds: string[];
  crew: CrewRef[];
  state?: { kind: "active" | "orphaned" | "vacant"; since?: number };
}

/** Count of in-flight Cortana releases. Kept for transfer sequencing diagnostics. */
let pendingReleases = 0;
export function captainReleasesInFlight(): number {
  return pendingReleases;
}

/** Make the "Mark as Cortana" affordance REAL: claim/transfer the Cortana
 *  singleton in the SERVER captains registry (the crown's source of truth), with
 *  most-recent-wins semantics per the general.
 *
 *  The server refuses to seize the reserved slug off a LIVE incumbent (one
 *  captain per ship - it only auto-transfers on unambiguous death), so a clean
 *  transfer is release-then-claim: free the `cortana` slug off whoever holds it,
 *  THEN claim it for the newly-marked tile. Best-effort like every other server
 *  captaincy mutation - outside Tauri or with the control channel down the
 *  optimistic local mark stands and the next adopted snapshot reconciles.
 *
 *  The release phase is counted into `pendingReleases` (like {@link serverCaptaincy})
 *  so a transient empty snapshot mid-transfer can't trip the A1 guard into wiping
 *  local designations; the count is balanced exactly once via `uncount`. */
function serverClaimCortana(id: TerminalId): void {
  pendingReleases += 1;
  let counted = true;
  const uncount = () => {
    if (counted) {
      counted = false;
      pendingReleases -= 1;
    }
  };
  void import("../ipc/controlClient")
    .then(async (m) => {
      // Free the singleton off any prior holder. An absent/unknown slug is a
      // strict server error - swallowed, the claim below is the load-bearing half.
      await m.controlRequest("release_captain", { shipSlug: CORTANA_SLUG }).catch(() => {});
      uncount();
      await m.controlRequest("claim_captain", { captainSessionId: id, role: "cortana" });
    })
    .catch(() => {
      // Control channel unavailable (tests / dev browser) or the claim raced a
      // server mutation - the optimistic local mark stands and reconciles.
    })
    .finally(uncount);
}

/** Clear the Cortana singleton server-side (the "Unmark Cortana" affordance / a
 *  killed orchestrator tile): release the reserved slug by ship, so it drops the
 *  holder even when the local `orchestratorId` has gone stale. Best-effort. */
function serverReleaseCortana(): void {
  pendingReleases += 1;
  let counted = true;
  const uncount = () => {
    if (counted) {
      counted = false;
      pendingReleases -= 1;
    }
  };
  void import("../ipc/controlClient")
    .then((m) => m.controlRequest("release_captain", { shipSlug: CORTANA_SLUG }))
    .catch(() => {})
    .finally(uncount);
}

/** The tile focused before the overlay opened, restored on close. Module-level
 *  (not store state): it's transient plumbing, never rendered or persisted. */
let prevFocusedId: TerminalId | null = null;

/** True when `id` currently has a tile in some (non-popped-out) workspace tab -
 *  the pool only renders those, so the overlay can only show those. */
function terminalHasTile(id: TerminalId): boolean {
  return useWorkspace.getState().tabs.some((t) => t.order.includes(id));
}

export interface CaptainState {
  /** Pinned captains in MRU order (front = most recently summoned). Persisted. */
  captainIds: TerminalId[];
  /** The SERVER captains registry records, keyed by captain session id (phase 2:
   *  workspaceTabIds + crew for the sidebar and context-aware summon). Empty for
   *  a fresh optimistic pin until the next sync_captains snapshot lands. */
  claims: Record<TerminalId, CaptainClaimRecord>;
  /** The captain the overlay shows / the next summon target. Invariant:
   *  `captainIds[0] ?? null` - kept explicit so every consumer reads one field. */
  activeCaptainId: TerminalId | null;
  /** Whether the overlay is up. Always starts false (not persisted). */
  open: boolean;
  /** Whether the titlebar anchor's captain dropdown is up (not persisted).
   *  Lives here (not component state) so lib/escOverlays can dismiss it from
   *  the single Esc dispatch point without a second listener. */
  anchorMenuOpen: boolean;
  /** The terminal designated as the ORCHESTRATOR - the fleet's top agent, placed
   *  in the reserved Captains workspace tab. null = none designated. Persisted. */
  orchestratorId: TerminalId | null;
  /** Overlay geometry, relative to the canvas container. x/y null = not yet
   *  placed (the overlay computes + commits a default on first open). */
  x: number | null;
  y: number | null;
  width: number;
  height: number;

  /** Pin a captain (ADDITIVE - other pins stay). No-op when already pinned. */
  pinCaptain: (id: TerminalId) => void;
  /** Unpin one captain. Unpinning the ACTIVE captain while summoned closes the
   *  overlay (never show an unpinned/killed session); the next MRU pin becomes
   *  active. Other pins are untouched. */
  unpinCaptain: (id: TerminalId) => void;
  /** Tile-menu / palette toggle: pin this terminal, or unpin it if pinned. */
  toggleCaptain: (id: TerminalId) => void;
  /** Adopt a SERVER captains-registry snapshot. The registry is authoritative for
   *  claims, not visual pins. Newly commissioned Captains append to the overlay,
   *  while explicit local pins remain until the user unpins them. */
  adoptCaptainsRegistry: (records: CaptainClaimRecord[]) => void;
  /** Summon a SPECIFIC pinned captain (switcher chip, titlebar dropdown,
   *  palette entry): it becomes active (MRU front), the overlay opens if
   *  closed, and keyboard focus moves to it. No-op if unpinned or tile-less. */
  summonCaptain: (id: TerminalId) => void;
  /** Open the overlay CONTEXT-AWARE (phase 2): the captain whose registry claim
   *  OWNS the active workspace tab wins (MRU order among multiple owners), then
   *  the MRU fallback - the first pinned captain with a live tile. Summoning
   *  from an unclaimed tab is therefore plain MRU. No-op when none qualifies. */
  openOverlay: () => void;
  /** Close the overlay and restore focus to the previously focused tile. */
  closeOverlay: () => void;
  /** While summoned: switch to the next pinned captain with a live tile, in
   *  MRU order, by ROTATING the list (round-robin, no ping-pong). No-op when
   *  closed or no other captain qualifies. */
  cycleCaptain: () => void;
  /** The registered Ctrl+B C command: closed -> summon the active captain;
   *  already summoned -> CYCLE to the next captain (Esc dismisses). */
  toggleOverlay: () => void;
  /** Show/hide the titlebar anchor dropdown. */
  setAnchorMenu: (open: boolean) => void;
  /** Designate (or clear, with null) the orchestrator terminal. Persisted. */
  setOrchestratorId: (id: TerminalId | null) => void;
  /** Commit dragged/resized geometry (persisted). */
  setGeometry: (g: { x: number; y: number; width: number; height: number }) => void;
}

export const useCaptain = create<CaptainState>((set, get) => {
  const persist = () => {
    const s = get();
    savePersisted(PERSIST_KEY, {
      captainIds: s.captainIds,
      orchestratorId: s.orchestratorId,
      x: s.x,
      y: s.y,
      width: s.width,
      height: s.height,
    } satisfies PersistedCaptain);
  };

  /** Set the list + derived active in one shot, keeping the invariant. */
  const commitIds = (captainIds: TerminalId[]) => {
    set({ captainIds, activeCaptainId: captainIds[0] ?? null });
    persist();
  };

  /** Apply an orchestrator designation LOCALLY only (state + persist + tile
   *  placement), WITHOUT driving the server. The public `setOrchestratorId`
   *  wraps this and then mutates the server registry; `adoptCaptainsRegistry`
   *  reuses it to converge on a server-declared Cortana without re-driving the
   *  server (which would loop: adopt -> claim -> snapshot -> adopt). Returns true
   *  when it actually changed. */
  const applyOrchestrator = (id: TerminalId | null): boolean => {
    if (get().orchestratorId === id) return false;
    const prev = get().orchestratorId;
    set({ orchestratorId: id });
    persist();
    // Placement: a newly-designated orchestrator's tile moves into the reserved
    // Captains tab (it is an agent, not a work tile). A cleared orchestrator's
    // tile returns to a work tab UNLESS it is still a captain.
    if (id) useWorkspace.getState().moveTileToCaptainsTab(id);
    if (prev && prev !== id && !get().captainIds.includes(prev)) {
      useWorkspace.getState().moveTileToWorkTab(prev);
    }
    return true;
  };

  return {
    captainIds: initial.captainIds,
    claims: {},
    activeCaptainId: initial.captainIds[0] ?? null,
    open: false,
    anchorMenuOpen: false,
    orchestratorId: initial.orchestratorId,
    x: initial.x,
    y: initial.y,
    width: initial.width,
    height: initial.height,

    pinCaptain: (id) => {
      const ids = get().captainIds;
      if (ids.includes(id)) return;
      // New pins land at the END (least recently summoned): pinning is a
      // designation, not a summon, so it never steals the active slot.
      commitIds([...ids, id]);
      // A visual pin deliberately leaves authority, capability, and workspace
      // placement unchanged. Full commissioning is a separate control operation.
    },

    unpinCaptain: (id) => {
      const s = get();
      if (!s.captainIds.includes(id)) return;
      // Never leave the overlay showing a session that just lost its pin
      // (kill paths land here via forgetCaptain) - close FIRST so the focus
      // restore runs while the pre-summon tile is still resolvable.
      if (id === s.activeCaptainId && s.open) s.closeOverlay();
      commitIds(s.captainIds.filter((c) => c !== id));
      // Last pin gone: drop the anchor dropdown too - the anchor button's
      // click is gated on count > 0, so an orphaned empty popover would have
      // no dismiss affordance left (Esc aside).
      if (get().captainIds.length === 0) get().setAnchorMenu(false);
      // Unpinning only changes the overlay. It never releases a commissioned
      // Captain or moves the terminal between workspaces.
    },

    adoptCaptainsRegistry: (records) => {
      const s = get();
      const claims: Record<TerminalId, CaptainClaimRecord> = {};
      let serverCortanaId: TerminalId | null = null;
      for (const r of records) {
        if (!r.terminalId) continue;
        // The Cortana singleton is tracked via `orchestratorId`, NOT the captain
        // pin list - keep the two designations distinct (mirroring the server's
        // FleetRole::Cortana vs Captain split), so a mark never also silently
        // pins the tile as a summonable captain.
        if (r.role === "cortana") {
          serverCortanaId = r.terminalId;
          continue;
        }
        claims[r.terminalId] = r;
      }
      const activeIds = Object.keys(claims);
      const added = activeIds.filter((id) => !s.captainIds.includes(id));
      const next = [...s.captainIds, ...added];
      set({ claims });
      const unchanged =
        next.length === s.captainIds.length &&
        next.every((id, i) => id === s.captainIds[i]);
      if (!unchanged) commitIds(next);
      if (next.length === 0) get().setAnchorMenu(false);
      // Cortana reconciliation: the SERVER cortana claim is authoritative for who
      // wears the crown, so adopt a server-declared holder when it differs from
      // local. This converges an optimistic mark and picks up a mark made from
      // another surface or restored from the registry at boot. Local-only (no
      // server re-drive) via applyOrchestrator. We deliberately do NOT clear a
      // local designation merely because THIS snapshot omitted cortana: a
      // release-then-claim transfer forwards an intermediate no-cortana snapshot,
      // and clearing on it would flap the crown off then back on - a dead
      // orchestrator is still cleared by the terminal-existence check below.
      if (serverCortanaId && serverCortanaId !== get().orchestratorId) {
        applyOrchestrator(serverCortanaId);
      }
      // Reconcile a STALE orchestrator: after a relaunch where the designated
      // session did not return, its id dangles (the strip shows a raw id, the
      // input stays disabled). Clear it if the terminal is no longer present.
      // Guarded on a non-empty terminals map so a not-yet-loaded workspace at
      // boot never false-clears a valid designation. Reads the CURRENT
      // designation (post cortana-reconciliation) so a freshly-adopted server
      // Cortana is validated against live terminals, not the stale pre-adopt id.
      const orch = get().orchestratorId;
      if (orch != null) {
        const terminals = useWorkspace.getState().terminals;
        if (
          Object.keys(terminals).length > 0 &&
          terminals[orch] === undefined
        ) {
          get().setOrchestratorId(null);
        }
      }
    },

    toggleCaptain: (id) => {
      if (get().captainIds.includes(id)) get().unpinCaptain(id);
      else get().pinCaptain(id);
    },

    summonCaptain: (id) => {
      const s = get();
      // Any summon path retires the anchor dropdown - a chord/palette summon
      // while it is open must not leave its full-window click-away backdrop
      // up to swallow the next pointerdown.
      s.setAnchorMenu(false);
      if (!s.captainIds.includes(id) || !terminalHasTile(id)) return;
      const ws = useWorkspace.getState();
      // Most recently summoned wins: move to the MRU front (skip the write
      // when it already leads - re-summoning the active captain must not
      // persist/re-render for nothing).
      if (s.captainIds[0] !== id) {
        commitIds([id, ...s.captainIds.filter((c) => c !== id)]);
      }
      if (!get().open) {
        prevFocusedId = ws.focusedId;
        set({ open: true });
      }
      // Keyboard goes to the captain: the pooled TerminalView focuses its xterm
      // when it becomes the focused tile (Terminal.tsx focus effect).
      ws.setFocus(id);
    },

    openOverlay: () => {
      const { captainIds, claims, open, summonCaptain, setAnchorMenu } = get();
      // Defensive mirror of summonCaptain's dropdown retire: even a no-op
      // open (no live pin) must not strand the backdrop.
      setAnchorMenu(false);
      if (open) return;
      // Context-aware summon (phase 2): a chord pressed ON a claimed workspace
      // summons the captain OWNING that workspace (per the server registry's
      // workspaceTabIds), MRU order breaking a tie between multiple owners.
      // An unclaimed tab (or an owner whose tile is gone) falls back to plain
      // MRU: the first captain whose tile is still live. A pin whose tab
      // popped out to a satellite is skipped, not dropped - it can be summoned
      // again when the tab returns. No live pin: nothing to summon (the
      // titlebar anchor tooltip explains how to pin one).
      const activeTabId = useWorkspace.getState().activeTabId;
      const owner = activeTabId
        ? captainIds.find(
            (id) =>
              terminalHasTile(id) &&
              (claims[id]?.workspaceTabIds.includes(activeTabId) ?? false),
          )
        : undefined;
      const target = owner ?? captainIds.find((id) => terminalHasTile(id));
      if (target) summonCaptain(target);
    },

    closeOverlay: () => {
      if (!get().open) return;
      set({ open: false });
      // Return focus to the tile that had it before the summon. The saved id
      // can be STALE - that tile may have been closed while the overlay was
      // open - so validate it against the live workspace first and fall back
      // to the active tab's first tile, so focus never stays parked on the
      // (now hidden) captain or on a dead id.
      const ws = useWorkspace.getState();
      const prev = prevFocusedId;
      prevFocusedId = null;
      if (prev && terminalHasTile(prev)) {
        ws.setFocus(prev);
        return;
      }
      const active = ws.tabs.find((t) => t.id === ws.activeTabId);
      const first = active?.order[0];
      if (first) ws.setFocus(first);
    },

    cycleCaptain: () => {
      const s = get();
      if (!s.open) return;
      const ids = s.captainIds;
      const i = Math.max(0, ids.indexOf(s.activeCaptainId ?? ""));
      // Next pinned captain AFTER the active one (wrapping) with a live tile.
      for (let k = 1; k < ids.length; k++) {
        const j = (i + k) % ids.length;
        if (!terminalHasTile(ids[j])) continue;
        // ROTATE so the target becomes the front while the cyclic order is
        // preserved - repeated cycles visit every captain (round-robin)
        // instead of ping-ponging between the two most recent.
        commitIds([...ids.slice(j), ...ids.slice(0, j)]);
        useWorkspace.getState().setFocus(ids[j]);
        return;
      }
      // Solo captain (or no other live one): stay summoned - Esc dismisses.
    },

    toggleOverlay: () => {
      if (get().open) get().cycleCaptain();
      else get().openOverlay();
    },

    setAnchorMenu: (open) => {
      if (get().anchorMenuOpen !== open) set({ anchorMenuOpen: open });
    },

    setOrchestratorId: (id) => {
      if (!applyOrchestrator(id)) return;
      // Make the mark REAL (per the general): the orchestrator IS the Cortana
      // singleton in the server captains registry, so a mark claims/transfers
      // that role server-side (most-recent-wins) and clearing it releases the
      // slug. The local state above is the optimistic mirror; the crown then
      // renders from the adopted server snapshot (adoptCaptainsRegistry).
      if (id) serverClaimCortana(id);
      else serverReleaseCortana();
    },

    setGeometry: (g) => {
      set({
        x: g.x,
        y: g.y,
        width: Math.max(CAPTAIN_MIN_WIDTH, g.width),
        height: Math.max(CAPTAIN_MIN_HEIGHT, g.height),
      });
      persist();
    },
  };
});

/** The sidebar AGENT order: the orchestrator FIRST (top of the fleet
 *  hierarchy), then the pinned captains, deduped (the orchestrator may itself be
 *  a pinned captain). This is the single source of "which agents, in what order"
 *  the sidebar hierarchy renders. */
export function agentOrder(
  s: Pick<CaptainState, "orchestratorId" | "captainIds">,
): TerminalId[] {
  const out: TerminalId[] = [];
  if (s.orchestratorId) out.push(s.orchestratorId);
  for (const id of s.captainIds) if (!out.includes(id)) out.push(id);
  return out;
}

// Give the workspace store a synchronous read of the agent id set so its
// adoptRegistry can keep an externally-claimed captain's tile alive through a
// server tab sync even when the server does not report that tile as a live
// work-tab tile. captain.ts already imports the workspace store, so registering
// here (rather than the workspace store importing us) avoids a static import
// cycle - the same reason forgetCaptain is invoked via a dynamic import there.
registerCaptainRegistry(() => agentOrder(useCaptain.getState()));

/**
 * Lifecycle cleanup: when a terminal is killed/removed, unpin it if it was a
 * captain (and drop the overlay if it was the SUMMONED one) so no designation
 * ever points at a dead id. Called from workspace.ts's cleanupTileSideState via
 * dynamic import (matching the DevTab/devserver pattern there - no static cycle
 * with the workspace store).
 */
export function forgetCaptain(id: TerminalId): void {
  useCaptain.getState().unpinCaptain(id);
  // The orchestrator can be ANY tile (not only a pinned captain), so clear the
  // designation here too when its terminal dies - a dead id must never remain
  // the orchestrator target (persisted).
  if (useCaptain.getState().orchestratorId === id) {
    useCaptain.getState().setOrchestratorId(null);
  }
}
