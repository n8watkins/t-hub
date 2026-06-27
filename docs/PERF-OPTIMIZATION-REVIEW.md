# Performance Optimization Review

Date: 2026-06-27

## Scope

This is a review-only optimization outline for T-Hub desktop responsiveness. It
does not claim to solve the Windows drag root cause by itself. The drag issue still
appears to have a structural Windows/WebView2/titlebar component, as described in
`docs/DRAG-LAG-INVESTIGATION.md`.

The goal here is to reduce avoidable app work around the interactions the user
called out: dragging, minimize/maximize/restore, and switching pages/panels/tabs.

## Highest-Value Updates To Consider

### 1. Throttle or pause background terminal rendering

Evidence:

- `TerminalPool` renders one `TerminalView` per pooled id and keeps it mounted
  with `visible={true}`.
- Parked terminals are hidden/offscreen, but still attached and eligible to process
  output.
- `Terminal.tsx` writes every flush into xterm and runs URL scanning/activity bumps
  in the same path.

Recommendation:

Keep the persistent pool, but make output scheduling visibility-aware:

- active visible terminal surface: rAF flush;
- inactive tab / hidden panel / non-focused fullscreen background: slower timer;
- hidden/minimized document: much slower timer or no DOM writes until visible;
- bounded byte backlog with an immediate flush threshold.

This is the accidental patch recorded in `docs/PERF-MAIN-UNCONFIRMED-PATCH.md`.
It should be reviewed in a separate worktree before landing.

Expected impact:

High for page switches, maximize/restore under terminal output, and general JS
main-thread headroom.

Risk:

Medium. Output ordering must remain exact, background URL detection should still
work, and noisy terminals must not build unbounded memory.

### 2. Coalesce resize/maximize IPC and layout work

Evidence:

- `windowMaximized.ts` tracks maximize state via `onResized` and `isMaximized()`.
- `Titlebar.tsx` reports maximize-button rect on resize with rAF coalescing, which
  is good, but the maximized-state tracker can still be made more burst-safe.
- `TerminalPool` also measures container/slot rects through a `ResizeObserver`
  and schedules `sync()` on rAF.

Recommendation:

- Keep one maximized-state subscription, but collapse resize bursts into one
  in-flight `isMaximized()` call plus one pending follow-up.
- Audit whether `TerminalPool` needs to observe every slot all the time. A large
  grid means one resize can schedule repeated full-pool rect reads.
- Consider only observing active-tab placeholders, then force a one-shot sync on
  tab switch/fullscreen/panel changes.

Expected impact:

Medium for maximize/restore and window resize. It will not fully solve OS modal
drag lag, but it removes avoidable work during the same interaction family.

Risk:

Low to medium. The main risk is stale geometry after a tile move or panel toggle.

### 3. Debounce durable workspace persistence

Evidence:

- `workspace.ts` calls `persist()` after many UI-only state changes:
  focus changes, tab switches, tile moves, tab resize ratios, zoom, tab rename,
  add/remove, pop in/out, etc.
- `persist()` serializes layout synchronously to localStorage and then sends the
  same JSON to backend persistence.
- Some high-frequency interactions have settled persistence, but others still
  persist immediately.

Recommendation:

Split persistence into tiers:

- immediate persist for destructive/session lifecycle changes;
- short debounce for focus/tab selection/drag/drop layout updates;
- explicit flush on window blur/close where supported.

Expected impact:

Medium. This reduces synchronous localStorage writes and backend persistence calls
during navigation and layout interactions.

Risk:

Low to medium. The main risk is losing the last small focus/layout update if the
process crashes inside the debounce window.

### 4. Centralize git-info polling by cwd

Evidence:

- `Tile.tsx` polls git info per tile every 30 seconds and on focus.
- `FilePanel.tsx` separately calls `gitInfo(root)` for the panel root.
- Backend caching helps, but each frontend caller still schedules its own refresh
  and state update.

Recommendation:

Create a small frontend git-info cache/store keyed by cwd:

- subscribers share a single in-flight request per cwd;
- a focus event triggers one refresh per unique cwd, not per tile;
- panels and tile headers read the same cached result;
- mutation paths can invalidate a cwd explicitly.

Expected impact:

Medium in workspaces with many tiles in the same repo/worktree.

Risk:

Low. Must preserve explicit invalidation after commits/worktree operations.

### 5. Reduce terminal repaint broadcasts

Evidence:

- Opening/closing overlays calls `repaintAllTerminals()`.
- Each terminal listens and schedules a full `term.refresh(0, rows - 1)`.
- This protects against stale/blank WebView2 DOM-rendered terminal frames, but it
  is expensive with many terminals.

Recommendation:

Make repaint broadcasts foreground-aware:

- repaint visible foreground terminals immediately;
- mark background terminals dirty and repaint on foreground transition;
- keep a manual refresh path for recovery.

Expected impact:

Medium for panel/page overlays and spawn menu interactions.

Risk:

Medium. The existing repaint-all behavior appears to be compensating for real
WebView2 stale-frame bugs, so this needs runtime visual verification.

### 6. Make file search cancellation real where possible

Evidence:

- `FilePanel.tsx` and `FileTree.tsx` debounce search and ignore stale results
  after cancellation.
- The backend request still runs once sent.

Recommendation:

For large repos, add a request id or cancellable search path so stale searches can
be ignored earlier server-side, or increase debounce/adapt it based on repo size.

Expected impact:

Low to medium. More important for huge repos and fast typing than for window chrome.

Risk:

Low if implemented as request-id stale suppression; medium if adding true backend
cancellation.

## Drag-Specific Note

Performance hardening helps the app feel less overloaded, but the idle drag symptom
is probably not primarily caused by app event floods. The current investigation
still points toward the Windows frameless/native move-loop/WebView2 child-window
path. For drag itself, the next meaningful experiments remain:

- A/B `win_snap::install` disabled.
- Test a minimal frameless Tauri app on the same machine.
- If confirmed, prototype a custom pointer-driven move path that avoids the OS
  modal move loop, knowing it will not solve minimize/maximize by itself.

## Suggested Worktree Policy

Any implementation should happen outside `main`, for example in a separate worktree
or branch dedicated to performance experiments. Each optimization should land as a
small, separately testable patch with before/after notes.

