// Frontend bridge for the MCP control channel's Organization-tier actions.
//
// The backend control listener (`src-tauri/src/control.rs`) accepts + audits
// Organization commands, applies them to the SERVER's authoritative tab registry
// (headless-org), and forwards the accepted `{command, args}` - with the registry
// snapshot under `args.sync` - to the frontend by emitting a Tauri
// `control://apply` event (wired in `lib.rs`). This module subscribes to that
// event and applies each mutation by adopting the snapshot into the workspace
// store, so a control-driven placement/move/close lands even when its target tab
// is hidden - and WITHOUT switching the user's active tab or stealing focus
// (only `focus_tab`/`focus_session` move the view, by design).
//
// The reverse direction (the tab reporter below) up-syncs USER-originated layout
// changes, carrying the last registry revision this window applied (`baseSeq`);
// the core rejects a stale report and answers with the snapshot to adopt, so the
// old lost-update race (a UI report clobbering a headless move_tile) is closed.
//
// It is a SIDE-EFFECT module (no React render): it sets up its subscription on
// import. It is pulled into the bundle by a single <script type="module"> in
// index.html, mirroring how `themeBootstrap` is loaded — so it needs no hook in
// App.tsx / main.tsx. Importing it outside a webview must not throw, so the
// Tauri `listen` is imported lazily and guarded. In a SATELLITE window the whole
// bridge is inert: a satellite holds one tab, so applying global mutations or
// reporting its scoped tab list would corrupt the main window's layout/registry.
import { listen } from "@tauri-apps/api/event";
import { isSatelliteWindow, useWorkspace } from "../store/workspace";
import { useCaptain, type CaptainClaimRecord, type CrewRef } from "../store/captain";
import { notify } from "../lib/notify";
import { controlRequest } from "./controlClient";
import type { TabReport, TabReportResult } from "./types";

/** Exact Tauri event the backend control listener emits to apply a UI mutation. */
export const CONTROL_APPLY_EVENT = "control://apply";

/** The payload shape carried by `control://apply`: a forwarded command + its args. */
interface ControlApply {
  command: string;
  args: Record<string, unknown> | null;
}

/** Read a string field from a loose args object (tolerates absent/null args). */
function str(
  args: Record<string, unknown> | null | undefined,
  key: string,
): string | undefined {
  const v = args?.[key];
  return typeof v === "string" && v ? v : undefined;
}

function harness(value: unknown): "codex" | "claude" | undefined {
  return value === "codex" || value === "claude" ? value : undefined;
}

function harnessPermission(value: unknown): CrewRef["harnessPermission"] {
  return value === "bypassPermissions" || value === "acceptEdits" || value === "default"
    ? value
    : undefined;
}

function tHubCapability(value: unknown): CrewRef["tHubCapability"] {
  return value === "read" || value === "control" ? value : undefined;
}

// The last authoritative registry revision this window applied (from an apply's
// `sync` payload or a report response). Rides every report as `baseSeq`.
let lastSeq = 0;
// True while adopting a server snapshot into the store: the tab reporter must
// not echo that change back up (it would bump the revision for nothing and
// widen the stale-report window). Zustand notifies subscribers synchronously
// inside set(), so a plain flag is race-free here.
let adoptingRegistry = false;
let layoutSyncFailed = false;

function surfaceLayoutSyncFailure(error: unknown): void {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  console.error("workspace registry sync failed", error);
  if (layoutSyncFailed) return;
  layoutSyncFailed = true;
  notify(
    "error",
    "Workspace sync failed",
    "Your local layout is still available. Restart T-Hub to retry synchronization.",
  );
}

/**
 * Adopt an apply's authoritative registry snapshot (`args.sync`) into the
 * workspace store. Returns true if a snapshot was present. This is THE apply
 * path for organization mutations: the UI renders FROM the registry rather than
 * re-deriving the mutation (whose target may be hidden or unknown locally).
 */
function adoptSync(args: ControlApply["args"]): boolean {
  const sync = args?.sync;
  if (!sync || typeof sync !== "object") return false;
  const { seq, tabs } = sync as { seq?: unknown; tabs?: unknown };
  if (typeof seq !== "number" || !Array.isArray(tabs)) return false;
  if (!hasWorkWorkspace(tabs as TabReport[])) return true;
  lastSeq = seq;
  adoptAuthoritativeTabs(tabs as TabReport[]);
  return true;
}

function tabReports(tabs: ReturnType<typeof useWorkspace.getState>["tabs"]): TabReport[] {
  return tabs.map((t) => ({
    schemaVersion: 1,
    id: t.id,
    name: t.name,
    kind: t.kind ?? (t.id === "captains-reserved" ? "captain" : "work"),
    tileIds: t.order,
  }));
}

function hasWorkWorkspace(tabs: TabReport[]): boolean {
  return tabs.some(
    (tab) => (tab.kind ?? (tab.id === "captains-reserved" ? "captain" : "work")) === "work",
  );
}

/** Apply only a usable authoritative snapshot. The server's legacy projection
 * can briefly contain Captain Workspace alone during startup recovery; adopting
 * that snapshot would erase the local work tab and make all visible terminals
 * disappear. Keep the local layout until a real Work Workspace is available. */
function adoptAuthoritativeTabs(tabs: TabReport[]): boolean {
  if (!hasWorkWorkspace(tabs)) return false;
  adoptingRegistry = true;
  try {
    useWorkspace.getState().adoptRegistry(tabs);
  } finally {
    adoptingRegistry = false;
  }
  return true;
}

/**
 * Hydrate the UI from the server registry before the first layout report.
 *
 * A Captain-only registry is an invalid recovered state: the UI always keeps
 * at least one work tab, and live terminals need a work tab to render. Keep a
 * locally persisted work layout in that case and repair the server from it.
 * This also lets setTerminals() surface pre-existing sessions while the first
 * control request is still in flight.
 */
export async function bootstrapWorkspaceTabs(): Promise<void> {
  try {
    const res = (await controlRequest("list_tabs")) as {
      seq?: unknown;
      activeTabId?: unknown;
      tabs?: unknown;
    };
    if (
      !res ||
      typeof res !== "object" ||
      typeof res.seq !== "number" ||
      !Array.isArray(res.tabs) ||
      res.tabs.length === 0
    ) {
      return;
    }

    const serverTabs = res.tabs as TabReport[];
    lastSeq = res.seq;
    const local = useWorkspace.getState();
    const localWork = local.tabs.filter((tab) => tab.id !== "captains-reserved");
    const serverWork = serverTabs.filter((tab) => tab.id !== "captains-reserved");
    if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
      console.warn("workspace registry bootstrap", {
        localTabs: local.tabs.length,
        serverTabs: serverTabs.length,
        localWorkspaces: localWork.length,
        serverWorkspaces: serverWork.length,
        liveTerminals: Object.keys(local.terminals).length,
      });
    }

    if (serverWork.length === 0) {
      // A Captain-only local layout is also unrecoverable: adopting it would
      // leave the Canvas unmounted, which prevents listTerminals() from ever
      // running and creates a deadlock where the live shells remain invisible.
      // Seed one ordinary workspace before repairing the server registry.
      let repairedLocal = local;
      if (localWork.length === 0) {
        useWorkspace.getState().addTab();
        repairedLocal = useWorkspace.getState();
      }
      const { reportWorkspaceTabs } = await import("./client");
      const repaired = await reportWorkspaceTabs(
        tabReports(repairedLocal.tabs),
        repairedLocal.activeTabId,
        res.seq,
      );
      if (typeof (repaired as { error?: unknown }).error === "string") {
        surfaceLayoutSyncFailure((repaired as { error: string }).error);
      }
      if (typeof repaired.seq === "number") lastSeq = repaired.seq;
      if (!repaired.error && repaired.stale && Array.isArray(repaired.tabs)) {
        adoptAuthoritativeTabs(repaired.tabs);
      }
      return;
    }

    adoptAuthoritativeTabs(serverTabs);
  } catch (error) {
    // The local layout remains usable if the control channel is unavailable.
    surfaceLayoutSyncFailure(error);
  }
}

// The last captains-registry revision this window adopted. Guards against a
// stale snapshot applying over a newer one (the boot fetch racing a live
// sync_captains forward). The server seq is persisted, so it is monotonic even
// across app restarts.
let lastCaptainsSeq = -1;
// True while the boot bootstrap is claiming legacy pins. Each claim forwards a
// PARTIAL sync_captains snapshot (only the ids claimed so far); adopting those
// mid-loop would transiently drop the not-yet-claimed pins from the store (and
// persist the shrunk list). We suppress those forwards and adopt only the final
// full snapshot the bootstrap fetches, so membership never flickers.
let captainsBootstrapping = false;

/** Loosely validate + adopt one captains snapshot `{seq, captains}` into the
 *  captain store (records missing arrays coerce to empty - the store renders
 *  from whatever the server sent). Returns true when adopted. Exported for the
 *  reconciliation tests (the seq guard + the A1 empty guard). */
export function adoptCaptainsSnapshot(sync: unknown): boolean {
  if (!sync || typeof sync !== "object") return false;
  const { seq, captains } = sync as { seq?: unknown; captains?: unknown };
  if (typeof seq !== "number" || !Array.isArray(captains)) return false;
  if (seq < lastCaptainsSeq) return false;
  lastCaptainsSeq = seq;
  const records: CaptainClaimRecord[] = [];
  for (const c of captains) {
    const r = c as (Partial<CaptainClaimRecord> & { captainSessionId?: unknown }) | null;
    if (!r) continue;
    // Item-2 re-key: the terminal pointer is `terminalId` (was `captainSessionId`);
    // accept the legacy field too for a mixed on-disk/wire window. A claim with NO
    // live terminal (orphaned/vacant) has no pin to render, so it is skipped here -
    // the store keys by terminal, and the server retains the record for re-adoption.
    const terminalId =
      typeof r.terminalId === "string" && r.terminalId
        ? r.terminalId
        : typeof r.captainSessionId === "string" && r.captainSessionId
          ? r.captainSessionId
          : undefined;
    if (terminalId === undefined) continue;
    // Crew deserializes from BOTH the legacy `string[]` and the modern `CrewRef[]`.
    const crew = Array.isArray(r.crew)
      ? r.crew.flatMap((t): CrewRef[] => {
          if (typeof t === "string") return [{ terminalId: t }];
          if (t && typeof t === "object" && typeof (t as CrewRef).terminalId === "string") {
            const raw = t as unknown as Record<string, unknown>;
            return [
              {
                terminalId: raw.terminalId as string,
                claudeUuid: str(raw, "claudeUuid"),
                provider: harness(raw.provider),
                providerSessionId: str(raw, "providerSessionId"),
                harness: harness(raw.harness),
                harnessPermission: harnessPermission(raw.harnessPermission),
                tHubCapability: tHubCapability(raw.tHubCapability),
                conversationId: str(raw, "conversationId"),
                resumePoint: str(raw, "resumePoint"),
                workspaceTabId: str(raw, "workspaceTabId"),
                delegatedRole:
                  raw.delegatedRole === "shipAdmin" || raw.delegatedRole === "fleetAdmin"
                    ? raw.delegatedRole
                    : undefined,
                delegatedGrantGeneration:
                  typeof raw.delegatedGrantGeneration === "number"
                    ? raw.delegatedGrantGeneration
                    : undefined,
                state: raw.state as CrewRef["state"],
              },
            ];
          }
          return [];
        })
      : [];
    records.push({
      terminalId,
      shipSlug: typeof r.shipSlug === "string" ? r.shipSlug : "",
      assignmentId:
        typeof r.assignmentId === "string" ? r.assignmentId : undefined,
      displayName: typeof r.displayName === "string" ? r.displayName : undefined,
      role: r.role === "cortana" ? "cortana" : "captain",
      claudeUuid: typeof r.claudeUuid === "string" ? r.claudeUuid : undefined,
      provider: harness(r.provider),
      providerSessionId:
        typeof r.providerSessionId === "string" ? r.providerSessionId : undefined,
      projectId: typeof r.projectId === "string" ? r.projectId : undefined,
      assignment: typeof r.assignment === "string" ? r.assignment : undefined,
      harness: harness(r.harness),
      conversationId:
        typeof r.conversationId === "string" ? r.conversationId : undefined,
      resumePoint: typeof r.resumePoint === "string" ? r.resumePoint : undefined,
      workspaceTabIds: Array.isArray(r.workspaceTabIds)
        ? r.workspaceTabIds.filter((t): t is string => typeof t === "string")
        : [],
      crew,
      state: r.state,
    });
  }
  useCaptain.getState().adoptCaptainsRegistry(records);
  return true;
}

/** Adopt a `sync_captains` apply's `args.sync` snapshot. Suppressed while the
 *  boot bootstrap is running (it adopts only its own final full snapshot). */
function adoptCaptainsSync(args: ControlApply["args"]): boolean {
  if (captainsBootstrapping) return false;
  return adoptCaptainsSnapshot(args?.sync);
}

/**
 * Apply one forwarded Organization-tier command to the workspace store. Best-effort
 * and total: an unknown command or missing/mismatched args is a no-op (the store
 * methods themselves also no-op on unknown ids), so a malformed forward never
 * throws inside the event listener.
 */
export function applyControl(command: string, args: ControlApply["args"]): void {
  const ws = useWorkspace.getState();

  switch (command) {
    case "move_tile": {
      // Server-registry move: adopt the snapshot (applies into a hidden tab,
      // no focus change). A `targetId` form is the legacy within-tab reorder.
      const terminalId = str(args, "terminalId") ?? str(args, "id");
      const targetId = str(args, "targetId") ?? str(args, "targetTerminalId");
      if (terminalId && targetId) {
        ws.moveTile(terminalId, targetId);
        return;
      }
      if (adoptSync(args)) return;
      // Legacy core (no snapshot): best-effort direct move.
      const tabId = str(args, "tabId");
      if (terminalId && tabId) ws.moveTileToTab(terminalId, tabId);
      return;
    }

    case "rename_tab": {
      if (adoptSync(args)) return;
      const tabId = str(args, "tabId") ?? str(args, "id");
      const name = str(args, "name");
      if (tabId && name) ws.renameTab(tabId, name);
      return;
    }

    case "new_tab": {
      // Headless-org: the tab already exists in the registry snapshot - adopt it
      // WITHOUT activating (agents stage tabs in the background; focus_tab is the
      // explicit way to switch). Legacy fallback adopts by id (which activates).
      if (adoptSync(args)) return;
      const id = str(args, "id");
      const name = str(args, "name");
      if (id) {
        ws.adoptTab(id, name ?? "Workspace");
      } else {
        const local = ws.addTab();
        if (name) useWorkspace.getState().renameTab(local, name);
      }
      return;
    }

    case "close_tab":
    case "sync_tabs": {
      // close_tab: the tab left the registry; sync_tabs: a bare snapshot push
      // (e.g. close_terminal dropped a tile). Adopting handles both.
      adoptSync(args);
      return;
    }

    case "sync_captains": {
      // Captain-chat phase 2: every captains-registry mutation (claim/release/
      // crew/prune, whoever originated it) forwards the authoritative snapshot;
      // the captain store renders FROM it, exactly like tabs render from sync.
      adoptCaptainsSync(args);
      return;
    }

    case "spawn_terminal": {
      // Headless-org: the SERVER already spawned the session (args.id) and placed
      // it in the registry; register the live terminal and adopt the snapshot -
      // no focus change, no view switch. The tile's xterm attaches on mount like
      // any adopted session. Legacy fallback (no id): spawn client-side.
      const id = str(args, "id");
      const cwd = str(args, "cwd");
      const name = str(args, "name");
      if (id) {
        ws.adoptTerminal({
          id,
          tmuxSession: str(args, "tmuxSession") ?? `th_${id}`,
          cwd: cwd ?? "",
          // Mirror commands::resolve_title (name || shell || "terminal"); the
          // ~5s meta poll refreshes it live afterwards.
          title: name ?? str(args, "shell") ?? "terminal",
          state: "live",
        });
        adoptSync(args);
        return;
      }
      const shell = str(args, "shell");
      const startupCommand = str(args, "startupCommand");
      void ws
        .spawnWorkspaceTerminal({ cwd, name, shell, startupCommand })
        .catch((e) => console.error("spawn_terminal failed", e));
      return;
    }

    case "focus_tab": {
      // MCP schema: { tabId } -> activate that workspace tab (the explicit,
      // intentional view switch).
      const tabId = str(args, "tabId") ?? str(args, "id");
      if (tabId && ws.tabs.some((t) => t.id === tabId)) ws.setActiveTab(tabId);
      return;
    }

    case "add_worktree_workspace": {
      // WS-4 / headless-org: the backend created the git worktree AND spawned the
      // worktree terminal (args.terminalId), placing it in the named tab in the
      // registry. Adopt both - no activation, no focus steal. Legacy fallback
      // (no terminalId): run the store's client-side create→tab→spawn helper.
      const worktreePath = str(args, "worktreePath") ?? str(args, "worktree_path");
      if (!worktreePath) return;
      const terminalId = str(args, "terminalId") ?? str(args, "terminal_id");
      if (terminalId) {
        ws.adoptTerminal({
          id: terminalId,
          tmuxSession: `th_${terminalId}`,
          cwd: worktreePath,
          title:
            str(args, "tabName") ??
            str(args, "branch") ??
            worktreePath.split("/").filter(Boolean).pop() ??
            "Worktree",
          state: "live",
        });
        adoptSync(args);
        return;
      }
      const repoRoot = str(args, "repoRoot") ?? str(args, "repo_root") ?? "";
      const branch = str(args, "branch");
      const tabName = str(args, "tabName") ?? str(args, "tab_name");
      const tabId = str(args, "tabId") ?? str(args, "tab_id");
      void ws
        .addWorktreeWorkspace(repoRoot, worktreePath, branch, {
          tabName,
          tabId,
          alreadyCreated: true,
        })
        .catch((e) => console.error("add_worktree_workspace failed", e));
      return;
    }

    case "remove_worktree_workspace": {
      // Compatibility handler for older control servers. Current servers fail
      // closed before forwarding this command until the unified worktree status
      // service can authorize removal.
      const worktreePath = str(args, "worktreePath") ?? str(args, "worktree_path");
      if (!worktreePath) return;
      const repoRoot = str(args, "repoRoot") ?? str(args, "repo_root") ?? "";
      const force = args?.force === true;
      void ws
        .removeWorktreeWorkspace(repoRoot, worktreePath, force)
        .catch((e) => console.error("remove_worktree_workspace failed", e));
      return;
    }

    case "focus_session": {
      // MCP schema: { sessionId } -> switch to the session's tab and focus its
      // tile. The id may name a terminal/tile id, the owning tab's id, or a tab
      // index; we map best-effort. A terminal id activates its owning tab first,
      // then focuses the tile, so focus lands on a visible canvas.
      const id =
        str(args, "sessionId") ??
        str(args, "terminalId") ??
        str(args, "tabId") ??
        str(args, "id");
      if (!id) return;

      const { tabs } = ws;
      // 1) Treat `id` as a terminal/tile id: focus it (activating its tab).
      const owningTab = tabs.find((t) => t.order.includes(id));
      if (owningTab) {
        if (owningTab.id !== ws.activeTabId) ws.setActiveTab(owningTab.id);
        ws.setFocus(id);
        return;
      }
      // 2) Treat `id` as a tab id: activate that tab.
      if (tabs.some((t) => t.id === id)) {
        ws.setActiveTab(id);
        return;
      }
      // 3) Unknown id: best-effort no-op (the session may live in another window).
      return;
    }

    default:
      // Not an Organization-tier mutation we apply here; ignore.
      return;
  }
}

/**
 * Subscribe to `control://apply` and apply each forwarded command. Returns a
 * promise of the Tauri unlisten fn (unused at the module top level — the
 * subscription lives for the lifetime of the window). Guarded so importing this
 * module outside a Tauri webview (e.g. a test/SSR context) is a safe no-op, and
 * inert in a satellite window (see the module header).
 */
export function startControlBridge(): void {
  if (typeof window === "undefined") return;
  if (isSatelliteWindow()) return;
  void listen<ControlApply>(CONTROL_APPLY_EVENT, (ev) => {
    const payload = ev.payload;
    if (!payload || typeof payload.command !== "string") return;
    try {
      applyControl(payload.command, payload.args ?? null);
    } catch {
      // Never let a malformed forward crash the listener.
    }
  }).catch(() => {
    // `listen` rejects when not running under Tauri — safe to ignore.
  });

  startTabReporter();
  startCaptainsBootstrap();
}

/**
 * Seed the captain store from the SERVER captains registry at boot (captain-chat
 * phase 2), and migrate any pre-phase-2 localStorage pins by CLAIMING them
 * server-side - an existing pin is never lost. Only pins whose tile is still
 * live are claimed (a stale pin for a dead terminal must not create a garbage
 * server claim; it simply drops when the adopted snapshot omits it). Failures
 * (not under Tauri, control channel down) are swallowed: the store keeps its
 * locally-persisted designations and the next sync_captains reconciles.
 */
/**
 * The bootstrap body (exported for tests): fetch the server registry, claim any
 * live-tile local pins the server does not yet know, then adopt the final
 * snapshot. `captainsBootstrapping` is held true for the whole run so mid-loop
 * partial `sync_captains` forwards are suppressed (see {@link adoptCaptainsSync}).
 */
export async function bootstrapCaptains(): Promise<void> {
  captainsBootstrapping = true;
  try {
    const res = (await controlRequest("list_captains")) as {
      seq?: number;
      captains?: CaptainClaimRecord[];
    };
    const server = new Set(
      (res.captains ?? [])
        .map((c) => c.terminalId ?? (c as { captainSessionId?: string }).captainSessionId)
        .filter((id): id is string => typeof id === "string"),
    );
    const ws = useWorkspace.getState();
    const missing = useCaptain
      .getState()
      .captainIds.filter(
        (id) => !server.has(id) && ws.tabs.some((t) => t.order.includes(id)),
      );
    for (const id of missing) {
      try {
        await controlRequest("claim_captain", { captainSessionId: id });
      } catch (e) {
        console.warn("captains bootstrap: claim failed", id, e);
      }
    }
    // Adopt the final registry state. Each claim above also forwarded a
    // sync_captains apply (suppressed while bootstrapping); re-fetching (only
    // when we claimed) makes the final adopt deterministic rather than relying
    // on event ordering.
    const finalRes = missing.length
      ? ((await controlRequest("list_captains")) as typeof res)
      : res;
    adoptCaptainsSnapshot(finalRes);
  } catch {
    // Not under Tauri or the control channel is down - locally persisted
    // designations stand until a sync_captains forward arrives.
  } finally {
    captainsBootstrapping = false;
  }
}

function startCaptainsBootstrap(): void {
  void bootstrapCaptains();
}

/** Test-only: reset the captains reconciliation singletons between cases (this
 *  is a side-effect module whose state persists across a test file's imports). */
export function __resetCaptainsReconcileForTest(): void {
  lastCaptainsSeq = -1;
  captainsBootstrapping = false;
}

/** Test-only: force the bootstrapping flag, to prove `sync_captains` forwards are
 *  suppressed while a bootstrap is in flight. */
export function __setCaptainsBootstrappingForTest(v: boolean): void {
  captainsBootstrapping = v;
}

/**
 * Up-sync USER-originated workspace-tab changes to the core's AUTHORITATIVE tab
 * registry (TASK C / #22, headless-org) whenever the layout or active tab
 * changes. Reports are SERIALIZED (at most one in flight; trailing changes
 * coalesce into one follow-up) and carry `baseSeq`; on a stale rejection the
 * returned authoritative snapshot is adopted - the rare concurrent local change
 * loses to the server, by design. Failures (e.g. not under Tauri) are swallowed.
 */
function startTabReporter(): void {
  let inFlight = false;
  let pending = false;
  let bootstrapping = true;

  const report = (): void => {
    if (adoptingRegistry) return; // never echo a server-applied snapshot back up
    if (bootstrapping) {
      pending = true;
      return;
    }
    if (inFlight) {
      pending = true;
      return;
    }
    inFlight = true;
    const { tabs, activeTabId } = useWorkspace.getState();
    const payload = tabReports(tabs);
    void import("./client")
      .then((m) => m.reportWorkspaceTabs(payload, activeTabId, lastSeq))
      .then((res: TabReportResult | void) => {
        if (res?.error) {
          throw new Error(res.error);
        }
        layoutSyncFailed = false;
        if (res && typeof res.seq === "number") {
          lastSeq = res.seq;
          if (res.stale && Array.isArray(res.tabs)) {
            // A server mutation raced this report: converge on the registry.
            adoptAuthoritativeTabs(res.tabs);
          }
        }
      })
      .catch((error) => {
        // Keep the local layout usable, but make a real Tauri failure visible.
        surfaceLayoutSyncFailure(error);
      })
      .finally(() => {
        inFlight = false;
        if (pending) {
          pending = false;
          report();
        }
      });
  };
  // Fire on every tab-layout change (identity of the `tabs` array changes on any
  // add/remove/rename/reorder/move) and on active-tab switches (the registry
  // mirrors the active tab for default spawn placement + focus proofs).
  useWorkspace.subscribe((state, prev) => {
    if (state.tabs !== prev.tabs || state.activeTabId !== prev.activeTabId) {
      report();
    }
  });
  // Read the authoritative registry before the first report so a cold webview
  // cannot overwrite server-side workspaces with its boot-time local snapshot.
  void bootstrapWorkspaceTabs().finally(() => {
    bootstrapping = false;
    report();
  });
}

// Run the subscription on import (side-effect module, mirroring themeBootstrap).
startControlBridge();
