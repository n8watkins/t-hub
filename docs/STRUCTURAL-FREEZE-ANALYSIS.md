# Structural Freeze / Interaction Performance Analysis

> ⚠️ **SUPERSEDED — see [`PERF-AND-DRAG-WORKLOG.md`](./PERF-AND-DRAG-WORKLOG.md) (§2/§4).**
> This doc's leading hypothesis (a compositor / win_snap / WebView2 child-HWND
> structural cause for the drag freeze) was **DISPROVEN**: win_snap-off (0.3.5),
> native frame (0.3.6), and `transparent:true` (0.3.7) all still froze. The actual
> root cause of the drag freeze AND the general sluggishness was a `claude -p /usage`
> **focus-storm** (CPU/WSL contention), fixed in 0.3.8. Kept for historical context only.

Date: 2026-06-27

## TL;DR

There are two different problems that have been called "freeze":

1. **Workload-dependent app freeze**: caused by backend WSL/process bursts and
   frontend terminal output/render storms. This is the family covered by
   `docs/PERF-AUDIT.md`; most high-impact fixes are already marked done there.
2. **Workload-independent interaction freeze**: drag/minimize/maximize/restore can
   feel bad even when the app is idle. That points away from agent/session workload
   and toward the Windows frameless-window + WebView2 + custom chrome path.
3. **Focus-return lag**: switching from another app back to T-Hub can lag for
   1-2 seconds. This one is different from idle drag: returning focus currently
   triggers several refresh paths at once, so app workload is a plausible primary
   cause for this symptom.

For the current "it feels frozen even when idle" complaint, do not start by
optimizing Claude/Codex output, WSL polling, or status events. Those can compound
busy-state jank, but they do not explain an empty app lagging while dragged.

## Evidence That This Is Not Primarily App Workload

From `docs/DRAG-LAG-INVESTIGATION.md`:

- Idle/new workspace still drags poorly with 0-2 terminals and no active session.
- Typing in a terminal is responsive.
- Prior event/IPC-flood fixes improved general responsiveness but did not fix idle
  drag.

That pattern is important:

- If app workload were the primary cause, typing and ordinary UI interaction would
  also degrade under the same conditions.
- If output/status traffic were the primary cause, an idle workspace should be
  smooth because there is little or no terminal/status traffic.
- If the issue happens specifically during window chrome operations, the drag or
  resize path itself is the suspect.

## Focus-Return Lag Is A Separate Bucket

New symptom:

- Switching from another app back to T-Hub can make T-Hub lag for about 1-2 seconds.

This should not be grouped with idle drag. App-switch/focus-return lag can be caused
by real app work because T-Hub intentionally refreshes several things when the
window regains focus.

Current focus-triggered work found in the frontend:

- `Tile.tsx:72-78` runs `gitInfo(cwd)` on mount and on every `window.focus`, once
  per mounted tile.
- `Canvas.tsx:180-194` runs `listTerminals()` on focus to refresh live cwd/title
  metadata.
- `UsageStrip.tsx:188-191` refreshes Claude usage on focus.
- `UsageStrip.tsx:258-261` refreshes Codex usage on focus.
- `RecentList.tsx:164-180` refreshes recent sessions on focus.

Even though several backend paths have been optimized since the original perf
audit, this can still produce a visible "return-to-app burst":

- many tile-level git refreshes can fire together;
- terminal-list refresh runs at the same time;
- usage/recent refreshes add more control-channel/backend work;
- React state updates and local cache writes can land while WebView2 is also
  repainting a just-focused window.

This makes focus-return lag a hybrid problem:

- the trigger is a native/window focus transition;
- the visible stall can plausibly come from app refresh work that starts on focus;
- it should be investigated separately from the idle drag modal-move-loop issue.

### Focus-Return Fix Direction

Recommended approach:

1. Add instrumentation around focus refreshes before changing behavior:
   log start/end/duration for `gitInfo`, `listTerminals`, `claudeUsage`,
   `codexUsage`, and `recentSessions` refreshes triggered by focus.
2. Replace many independent focus listeners with a single focus refresh scheduler.
3. Stagger or prioritize work:
   - immediate: visible terminal metadata needed for current screen;
   - delayed 250-500ms: git chips and recent sessions;
   - delayed or cache-first: usage refreshes.
4. Dedupe by key:
   - one `gitInfo` request per unique cwd, not per tile;
   - one recent/usage refresh per focus event;
   - skip refresh if the last successful result is still inside a short TTL.
5. Avoid focus refresh when the window only receives internal focus churn; use
   document/window visibility and last-focus timestamps to suppress duplicates.

Expected result:

- This can improve "switch back to T-Hub and it stalls for 1-2 seconds."
- It probably will not fix idle drag, because idle drag happens with no focus
  refresh burst.

## Likely Mechanism

### 1. Windows native move/resize path can park the app event loop

T-Hub is frameless and uses custom chrome. The current Windows path returns native
hit-test values such as `HTCAPTION` from `win_snap.rs`. On Windows, a non-client
caption drag enters the OS modal move loop. During that loop, the application is not
running its normal event loop in the same way it does during ordinary typing.

Expected symptom:

- Cursor moves ahead of the window.
- WebView content appears stale or frozen during the drag.
- The problem is present even when the app has no meaningful work.

### 2. WebView2 windowed hosting is a child-HWND composition problem

The WebView content is hosted as a child window. During parent-window move/resize,
Windows/DWM must keep the child WebView positioned and composed. Child-window
composition during parent moves is a known class of stale-frame/trailing-frame
behavior. This affects the whole rendered app surface, not just terminals.

Expected symptom:

- The app content stutters during drag/maximize/restore even when React is idle.
- The rest of Windows remains responsive.
- The problem looks like "T-Hub froze", but the app may simply not be presenting
  fresh WebView frames during the OS operation.

### 3. `win_snap` may add T-Hub-specific cost

The structural platform path may be bad by itself, but T-Hub also adds custom Win32
logic for Windows 11 Snap Layouts:

- subclassed hit testing;
- `DwmDefWindowProc` participation;
- `DwmExtendFrameIntoClientArea`.

This may be cheap in isolation, but it is in the exact interaction path where the
user sees lag. It is the first T-Hub-specific thing to isolate.

### 4. Minimize/maximize/restore are related but not identical

Drag uses the modal move loop. Maximize/restore/minimize are not the same gesture,
but they still stress the same layer:

- native window state changes;
- WebView child-window resize/recomposition;
- React/layout/terminal pool geometry sync;
- xterm resize/repaint paths.

So a custom pointer-drag implementation could improve drag while leaving
maximize/minimize unchanged. Maximize/minimize likely need separate reduction of
resize/composition churn or a change to the window/chrome strategy.

## What Not To Conclude

Do not conclude "performance fixes are useless." They still matter under real load.

But for idle drag/window-state jank:

- background terminal throttling helps only if hidden terminals are producing output;
- git/list-terminal/usage polling helps periodic freezes, not idle drag;
- status dedup helps event storms, not a no-event modal move loop.

The right mental model is:

- **App workload fixes** improve busy-state responsiveness.
- **Structural chrome/WebView fixes** are needed for idle drag/window-state jank.
- **Focus orchestration fixes** are needed for app-switch-return lag.

## Decision Tree For Next Experiments

Run these in a separate worktree or branch, on the real Windows target, with an
idle/empty workspace first.

### Experiment 1: Disable `win_snap`

Temporarily skip `win_snap::install(&main)` or early-return from the Windows
installer.

Result interpretation:

- Smooth: `win_snap`/DWM subclass path is the dominant T-Hub-specific cause.
- Still bad: the platform substrate is bad even without T-Hub's snap integration.

This is the highest-value first experiment.

### Experiment 2: Disable only DWM frame extension

Keep hit testing, but no-op `DwmExtendFrameIntoClientArea` and re-extension on
activate/DPI.

Result interpretation:

- Smooth: DWM frame extension is likely the expensive part.
- Still bad: suspect the non-client move loop, child-HWND WebView composition, or
  `DwmDefWindowProc`/hit-test behavior.

### Experiment 3: Minimal frameless Tauri comparison

Create a tiny Tauri 2 app with:

- `decorations: false`;
- a simple drag region;
- no React app complexity;
- no `win_snap`;
- same machine and comparable Tauri/wry/tao versions if possible.

Result interpretation:

- Also bad: upstream/structural frameless WebView2 behavior.
- Smooth: T-Hub-specific chrome, DWM, layout, or content composition is adding the
  visible lag.

### Experiment 4: Blank T-Hub content route

Keep T-Hub's real window/chrome, but render a blank body/no terminal pool.

Result interpretation:

- Smooth: T-Hub content/layout/repaint cost is amplifying the structural path.
- Still bad: the window/chrome/WebView shell is enough to reproduce it.

### Experiment 5: Native decorations as a control

Run a build with native window decorations enabled.

Result interpretation:

- Smooth: frameless custom chrome is the core tradeoff.
- Still bad: broader WebView2/window composition issue.

This may sacrifice custom titlebar and Snap Layout behavior, so treat it as a
diagnostic, not a product fix.

## Possible Fix Directions After Attribution

### If `win_snap` is the lever

Try, in order:

- avoid live DWM frame extension;
- restrict `DwmDefWindowProc` calls to the messages that actually need it;
- return `HTCAPTION` only where necessary;
- accept losing or feature-flagging native Snap Layouts if the cost is too high.

### If frameless + native move loop is the lever

Prototype a custom pointer-driven move loop for drag only:

- pointerdown in titlebar starts custom move tracking;
- app drives window position itself;
- avoid native `HTCAPTION` modal move loop where possible.

Tradeoff:

- This can improve drag.
- It does not automatically fix maximize/minimize/restore.
- It must preserve resize, Snap Layouts, double-click maximize, keyboard/window
  behavior, and monitor/DPI correctness.

### If WebView child-HWND composition is the lever

Explore windowing/hosting mitigations:

- test whether a different WebView hosting mode is possible in the Tauri/wry stack;
- test transparent/visual-hosting-related knobs only as controlled experiments;
- reduce full-surface DOM repaint/resize work during window-state transitions;
- hide/defer heavyweight terminal surfaces during transition and repaint after.

This path is higher risk because it can affect rendering, input, transparency, and
GPU behavior.

### If content/layout amplifies the issue

Then the app optimization review applies:

- throttle background terminal output;
- make repaint broadcasts foreground-aware;
- reduce TerminalPool slot observation scope;
- debounce persistence and low-priority polls during transition windows.

Those are useful, but they should be treated as amplification fixes, not the root
idle-drag fix.

## Recommended Next Step

There are now two first experiments, depending on which symptom is being tested.

For idle drag/window-state jank, do **Experiment 1** in a separate worktree:
disable `win_snap` and test idle drag, then record the result with exact
build/version and date. That one test splits the problem into either:

- "our Win32 Snap/Layout code is the expensive path", or
- "the base frameless WebView2 platform path is already bad."

For app-switch-return lag, instrument and centralize the focus refresh burst first.
That symptom has clear focus-triggered app work and should not wait on the drag
experiments.
