# Window-Drag Lag Investigation

> Living document. The team updates the Hypotheses table, the Changes log, and the
> Experiments list as we learn more. Keep entries factual and cite `file:line` and
> commit hashes.

Last updated: 2026-06-26

---

## Problem statement

T-Hub's main window is **frameless** (`decorations: false`,
`apps/desktop/src-tauri/tauri.conf.json:21`) with a custom React titlebar
(`apps/desktop/src/components/Titlebar.tsx`). When the user grabs the top bar and
**drags the window around on Windows**, the drag is laggy — "almost unusable." The
window position trails the cursor and the rendered content stutters/freezes for the
duration of the drag.

This matters because window dragging is a constant, basic interaction. A janky drag
makes the whole app feel broken regardless of how good everything else is, and it's
been a long-standing complaint that several "freeze" fixes have *not* resolved.

---

## Symptom log

### 2026-06-26 — refined symptoms (KEY — collected directly from the user)

These three facts substantially change the diagnosis and are the anchor for the
hypotheses below:

1. **It lags even when the app is completely idle.** Empty/new workspace, 0–2
   terminal tiles, no Claude session running, nothing producing output — the drag
   is still laggy.
2. **Both** failure modes happen at once during a drag: the **window position
   trails behind the cursor** *and* the **content inside stutters/freezes**.
3. **Typing in a terminal is totally fine** — no lag anywhere else. *Only*
   window-dragging is bad.

#### Why these three facts matter (the load-bearing inference)

- "Lags even when idle/empty" means there is **almost nothing being emitted** over
  IPC during the laggy period. An idle app with no sessions produces effectively no
  `status://snapshot` / `terminal://output` / journal traffic. So an event/IPC
  flood **cannot** be the primary cause of the idle-drag lag.
- "Typing is fine" means the **main thread / event loop is healthy** in normal
  operation. A main thread starved by an event storm would also make keystroke echo
  laggy; it doesn't. So whatever happens during a drag is **specific to the drag**,
  not a background load that the drag merely exposes.

Together these point away from "background work starves the loop" and toward "the
drag itself enters a path that blocks repaint / trails the window."

### Earlier history

A long series of "freeze" reports preceded the refined symptoms. Those were mostly
**periodic** freezes (every ~5s) or sustained UI pinning under load — i.e. the
event-flood family, not the drag specifically. They were addressed by the commits in
the *Changes made so far* section. They were real wins, but the **drag** remained
laggy afterward, which is what prompted the refined symptom collection above.

---

## Timeline (from `git log` / `git show`)

All on the same machine; dates are author dates.

| When | Commit | What landed | Relevance to drag |
| --- | --- | --- | --- |
| 2026-06-13 15:00 | `f8b8770` | Scaffold 0.1 nucleus (Tauri 2 + React + xterm) | Baseline; window still decorated here. |
| 2026-06-13 **18:01** | `8701e6d` | **Frameless window** (`decorations:false`) + `Titlebar.tsx` with `data-tauri-drag-region` | First moment drag is driven by `data-tauri-drag-region`, NOT a native OS title bar. |
| 2026-06-13 23:11 | `ae32e59` | **win_snap.rs** first lands: Win32 **subclass** + `WM_NCHITTEST` returning HTCAPTION/HTMAXBUTTON/edge codes | Drag now driven by **native HTCAPTION** (OS modal move loop) over the titlebar, plus a per-message subclass proc. |
| 2026-06-13 23:52 | `98a8da0` | **`DwmExtendFrameIntoClientArea`** (1px top margin) + **`DwmDefWindowProc` first on every message** | Re-introduces a DWM-managed frame behind the client area; adds a DWM call on *every* window message. |
| 2026-06-13 …    | `33081cc` | max/restore toggle handled in the subclass (BUG 3) | Adds NCLBUTTON interception ahead of DWM. |
| 2026-06-22 →    | `e6821d3`,`1c96199` | Frontend-reported maximize-button rect | Adds the `set_maximize_button_rect` IPC + (later removed) per-move reporting. |
| 2026-06-26 18:00 | `71a42ee` | **Drag-freeze fix #1**: drop Titlebar `onMoved` IPC storm + dedup status snapshots | Targeted the IPC-storm theory. Helped, did NOT fix idle drag. |
| 2026-06-26 …    | `0afc609` | **Drag-freeze fix #2**: SocketEmitter alone (kill double-fanout) + coalesce terminal output | Targeted event volume. Helped, did NOT fix idle drag. |

**Key takeaway from the timeline:** the laggy-drag behavior could plausibly date to
`8701e6d` (frameless + `data-tauri-drag-region`, 18:01) OR to `ae32e59`/`98a8da0`
(the Win32 subclass + DWM frame, 23:11–23:52). They landed the *same day*, so commit
chronology alone can't separate them — only an A/B experiment can (see *Next
experiments*). The frameless config is the **earliest** candidate and is present in
*every* configuration we ship, so it is the common denominator.

---

## How dragging actually works here (mechanism)

This is the substrate every hypothesis sits on.

1. **Drag is OS-native via HTCAPTION.** Over the titlebar, `win_snap`'s `hit_test`
   returns `HTCAPTION` (`apps/desktop/src-tauri/src/win_snap.rs:397`). The
   `data-tauri-drag-region` attributes (`Titlebar.tsx:250`, `381`, `343`) are
   "belt-and-braces" on top of that (see the comment at `win_snap.rs:385`). On
   Windows, an HTCAPTION press makes the OS enter its **modal move loop**
   (`DefWindowProc` for `WM_NCLBUTTONDOWN`/HTCAPTION runs a nested
   `GetMessage`/`SetWindowPos` loop that does not return until mouse-up).
2. **The modal move loop parks the app's event loop.** While that nested loop runs,
   tao/wry's normal `RunLoop`/event pump is effectively suspended — the app's main
   thread is inside Windows' move loop, not tao's. This is the canonical Win32
   behavior (documented in SDL #1059, Chromium's HTCAPTION change). The frontend's
   own code already learned this: the comment at `Titlebar.tsx:89–96` describes "a
   Windows window-drag fires move events tens of times/sec while the main thread is
   parked in the OS modal move loop."
3. **The WebView2 content is a CHILD HWND (Windowed hosting).** Tauri/wry on Windows
   default to WebView2 **Windowed hosting**: the web content lives in a child `HWND`
   parented to the main window (Microsoft docs: *Windowed vs. Visual hosting*). We do
   **not** set `COREWEBVIEW2_FORCED_HOSTING_MODE` and the window is **not**
   `transparent` (no transparency/visual-hosting override anywhere in
   `src-tauri/`), so this is the plain windowed/child-HWND model. During a parent
   move, the child HWND has to be repositioned and repainted by the OS/compositor;
   child-window repaint during a parent modal-move is a well-known source of trailing
   / stale-frame lag.
4. **`DwmDefWindowProc` runs first on EVERY message** (`win_snap.rs:272`) and the
   DWM frame is extended (`win_snap.rs:208–222`, re-extended on activate/DPI at
   `:281`). This adds DWM work into the per-message path and gives the window a
   DWM-composited frame surface.

Versions in play (`apps/desktop/src-tauri/Cargo.lock`): `tauri 2.11.2`,
`wry 0.55.1`, `tao 0.35.3`.

---

## Hypotheses

| # | Hypothesis | Evidence for | Evidence against | Status |
| --- | --- | --- | --- | --- |
| H1 | **Event/IPC flood starves the main thread**, so the drag (which also pumps move events) backs up behind a busy loop. | Historically the app DID flood (`d6d71ef`, `6ccc9fe`, `0afc609`); fixing it improved general responsiveness. | **Idle+empty still lags** → almost nothing is being emitted during the laggy period. **Typing is fine** → the loop is not starved in normal use. An empty app cannot produce the flood this needs. | **Largely RULED OUT** as the *primary* drag cause. May still *compound* the lag when busy, but it is not what makes an idle drag janky. |
| H2 | **OS modal move loop blocks tao/wry's event loop**, so the webview can't repaint and Tauri can't service the window while the OS drags it. | Canonical Win32 behavior (SDL #1059, Chromium HTCAPTION). The codebase already documents the "main thread parked in the OS modal move loop" (`Titlebar.tsx:89`). Explains BOTH symptoms (position trails + content freezes) and is **present even when idle** (it's structural, not load-based). Matches "only dragging is bad." | None yet. This is the move loop's normal effect; the question is how much it contributes vs. H3/H4. | **LEADING** |
| H3 | **WebView2 Windowed-hosting child-HWND repaint lag** during the parent move — the child webview HWND can't keep up compositing/repositioning while the parent is dragged. | Tauri/wry default = Windowed hosting (child HWND), confirmed: no `transparent`, no `COREWEBVIEW2_FORCED_HOSTING_MODE` override. Known wry symptom "webview frozen until mouse moves" (wry #616). Explains the **content stutter** specifically, **independent of app load** (so it survives idle). DOM-rendered xterm (no GPU context, `Terminal.tsx:3–17`) means each repaint is CPU/DOM work, which a move loop can stall. | Hard to separate from H2 without disabling one. | **LEADING** |
| H4 | **win_snap subclass + DWM frame extension overhead** — `DwmDefWindowProc` first on every message (`win_snap.rs:272`) and `DwmExtendFrameIntoClientArea` (`:219`) add compositor/per-message cost that worsens drag repaint. | win_snap is the only *T-Hub-specific* (non-vanilla-Tauri) thing in the drag path; it touches DWM on every message and gives the window a DWM frame surface. WS_EX_COMPOSITED-style "excessive paint" issues are documented (WebView2Feedback #1096). Could turn an ordinary move-loop stall into a visibly worse one. | The frameless `data-tauri-drag-region` window (`8701e6d`) existed ~5h **before** win_snap (`ae32e59`); if the lag predates win_snap, this is not the root (but it could be an additive cost). `DwmDefWindowProc` per message is cheap in isolation. | **OPEN** (needs the A/B in Experiment 1) |
| H5 | **`data-tauri-drag-region` JS-driven drag path** (`startDragging` round-trip) rather than native HTCAPTION is the laggy one. | This path requires a JS event → IPC → `start_dragging` and is known to be janky (tauri #9445, #11345). | In T-Hub the **native HTCAPTION** from `win_snap` (`win_snap.rs:397`) takes over the drag at the OS level over the whole titlebar row; the JS path is described as belt-and-braces (`win_snap.rs:385`). So for builds *with* win_snap, the native path should dominate. Relevant mainly to a **pre-win_snap** baseline or builds where the subclass failed to install. | **UNLIKELY** (for current builds; still worth confirming via Experiment 1's vanilla comparison) |
| H6 | **Per-move IPC storm** from `useReportMaxButtonRect` subscribing to `onMoved` (old code): `scaleFactor()` + `getBoundingClientRect()` + `invoke(set_maximize_button_rect)` per move event, piling up against the blocked thread. | Was real and is exactly the "froze the window on drag" mechanism described in `71a42ee`. | **Already fixed** in `71a42ee`: `onMoved` subscription dropped, scale cached, report rAF-coalesced (`Titlebar.tsx:170–176`, `:89–96`). Drag still lags after the fix → not the (whole) root. | **FIXED, but drag persists** → not the remaining root cause. |
| H7 | **Per-event maximize-state / status IPC** (duplicate trackers, double event fan-out). | Real overhead; fixed in `0afc609` (single `useWindowMaximized`, SocketEmitter alone). | Same as H1: idle+empty produces ~none of this; drag still lags. | **RULED OUT** as primary (overlaps H1). |
| H8 | **GPU/driver compositing path** (transparency, vsync, swapchain) makes the move-loop repaint expensive. | DOM xterm renderer is CPU/DOM; window is opaque (no transparency). | Window is **not** `transparent`; no GPU context for terminals (`Terminal.tsx:3–17`). Less likely to be the lever than H2/H3. Could still interact with the DWM frame (H4). | **UNLIKELY / OPEN** (cheap to test by toggling DWM frame in Experiment 3) |

### Bottom line on the hypotheses

- The **event/IPC-flood family (H1, H6, H7)** is **largely ruled out as the primary
  cause of the drag lag** by the idle+empty + typing-is-fine evidence. Those fixes
  were correct and improved general responsiveness, but they could not have fixed an
  idle drag because an idle app emits almost nothing.
- The **leading causes are structural and load-independent**: the **OS modal move
  loop blocking tao/wry's loop (H2)** and **WebView2 windowed-hosting child-HWND
  repaint lag during the parent move (H3)**, with **win_snap's DWM frame / per-message
  subclass (H4)** as the most likely T-Hub-specific *additive* factor to isolate first.

---

## Changes made so far

None of these fixed the **drag**. They each targeted the event-flood family and did
improve general responsiveness / periodic freezes.

| Commit | What it changed | Hypothesis it targeted | Observed outcome |
| --- | --- | --- | --- |
| `6ccc9fe` | Frontend `setSnapshot` dedup — skip store update + diag write on no-op statusline resends. | H1 (event flood / re-render storm) | Stopped the re-render + disk-IO storm; ballooning diag logs fixed. Drag unaffected. |
| `d6d71ef` | Backend dedups `status://snapshot` **emits** at the source (`agent/mod.rs`), only on meaningful change. | H1 (event flood at the source) | Cut ~200 events/sec sustained when busy. Drag (esp. idle) unaffected. |
| `ce6bc95` | Throttle per-tile git poll (5s→30s) + Canvas refresh (5s→15s); USERPROFILE fallback for Windows theme dir. | Periodic 5s freeze (wsl.exe spawn storm) | Killed the every-5s freeze. Not a drag fix. |
| `3bb186d` | Stop `/usage` transcript litter bloating the recent-sessions scan. | Periodic UI freeze (catalog walk) | Removed a cold-poll freeze. Not a drag fix. |
| `71a42ee` | **Drag-freeze fix #1**: drop Titlebar `onMoved` subscription (per-move `scaleFactor()` + `invoke`); rAF-coalesce the rect report; dedup status-snapshot fan-out. | H6 (per-move IPC storm) + H1 | Removed a genuine per-move storm. **Drag still lags when idle/empty** → not the root. |
| `0afc609` | **Drag-freeze fix #2**: install `SocketEmitter` alone (kill TeeEmitter double-fanout); coalesce `terminal://output` (8ms/256KB batches, adaptive read); collapse duplicate maximized-state trackers. | H1/H7 (event volume) | Halved bridge-event volume; batched PTY output. Helps under load. **Drag still lags when idle/empty.** |
| `2324b0b` | Show app version in the top-left brand (build stamp). | — (diagnostic only) | Lets us identify which build is running while testing. |

---

## Leading hypotheses (after the idle+empty evidence)

1. **H2 — OS modal move loop blocks tao/wry's event loop.** Structural, present even
   when idle, explains *both* symptoms (window trails + content freezes), and matches
   "only dragging is bad." This is the strongest single explanation.
2. **H3 — WebView2 Windowed-hosting child-HWND repaint lag** during the parent move.
   Explains the content stutter independently of app load; confirmed we run plain
   Windowed hosting (child HWND, opaque window, no override).
3. **H4 — win_snap's DWM frame extension + per-message `DwmDefWindowProc`** as the
   most likely T-Hub-specific *additive* cost. This is the one thing we control that
   a vanilla Tauri frameless app does NOT have, so it is the highest-value thing to
   A/B first to learn whether our code makes a baseline-bad situation worse.

These three are not mutually exclusive — the realistic model is "the OS move loop
parks the loop (H2), the child webview can't repaint cleanly during it (H3), and
win_snap's DWM/subclass work (H4) adds to the per-frame cost." The experiments below
are designed to attribute the share.

---

## Next experiments to isolate the cause (PRIORITIZED)

Run on the real Windows target with an **idle, empty workspace** (the refined-symptom
condition) so background load can't muddy results. Compare the *same* drag each time.

### 1. (HIGHEST VALUE) A/B with `win_snap::install` disabled

Temporarily skip the `win_snap::install(&main)` call in
`apps/desktop/src-tauri/src/lib.rs:400` (or early-return from `imp::install`) and run
an idle drag.

- **If the drag becomes smooth** → the **win_snap subclass + DWM frame (H4)** is the
  dominant cost. Next, bisect *within* win_snap (Experiment 3).
- **If the drag is unchanged** → win_snap is **not** the root; the lag is the
  underlying frameless/move-loop/child-HWND behavior (H2+H3). Proceed to Experiment 2.

This single test cleanly separates "our custom Win32 code" from "the platform
substrate," which is the biggest open question (H4 status is OPEN precisely because
the frameless window predates win_snap).

### 2. Vanilla Tauri frameless comparison (isolates H2/H3 from T-Hub)

Build a throwaway minimal Tauri 2 app: `decorations:false`, a `data-tauri-drag-region`
bar, a **single blank page** (no React app, no terminals), **no win_snap**. Drag it on
the same machine.

- **If it ALSO lags** → the lag is inherent to **Tauri/wry frameless dragging on
  Windows** (H2 modal move loop and/or H3 child-HWND repaint). Confirms the root is
  upstream/structural, and the fix space is "mitigate the move loop" not "fix our
  code." (Pin the same `tauri 2.11.2` / `wry 0.55.1` / `tao 0.35.3` to be exact.)
- **If it is smooth** → something T-Hub-specific is the cause; combined with
  Experiment 1's result, attribute it to win_snap (if #1 fixed it) or to our DOM
  content cost (proceed to Experiment 4).

### 3. Toggle `DwmExtendFrameIntoClientArea` off (sub-bisect of H4)

With win_snap otherwise installed, no-op `extend_frame`
(`apps/desktop/src-tauri/src/win_snap.rs:208`) — skip the `DwmExtendFrameIntoClientArea`
call and the re-extend on `WM_ACTIVATE`/`WM_DPICHANGED` (`:281`). Keep the
`WM_NCHITTEST` hit-test.

- **If smooth** → the **DWM frame extension** specifically is the lever (it gives the
  window a DWM-composited frame surface that the move loop has to recomposite). The
  fix is to find a Snap-Layouts approach that doesn't keep a live extended frame, or
  to accept losing the flyout for a smooth drag.
- **If still laggy** → the DWM frame is not it; suspect the per-message
  `DwmDefWindowProc` (`:272`) — try gating it to only the messages that need it
  (caption-button hover) instead of every message — or conclude H4 is not the lever
  and the cost is H2/H3.

### 4. Single empty webview / blank-page test inside T-Hub (isolates content cost)

Point the main window at a blank route (no terminals, no sidebar, just the titlebar)
and drag.

- **If smooth (but the real app is not)** → the **content repaint cost** under the
  move loop matters (H3 amplified by DOM-rendered content). Mitigations: pause /
  `visibility:hidden` heavy subtrees during a drag, or reduce per-frame DOM work.
- **If still laggy** → content is not the lever; reinforces H2/H4 (the chrome/frame
  itself).

### 5. Manual drag via `startDragging` with a plain client window (isolates H5)

As a control, disable the native HTCAPTION (return `HTCLIENT` from `hit_test` over the
titlebar) and rely purely on `data-tauri-drag-region` → `startDragging`. Compare.

- **If the JS path is *worse*** → confirms native HTCAPTION is the better path and H5
  is not our problem (expected). If it's the *same*, both converge on the OS move loop
  (H2) regardless of who initiates it.

### 6. (If H2/H3 confirmed) Evaluate mitigations, not "fixes"

If Experiments 1–2 show the root is the structural move loop + child-HWND repaint
(H2/H3) rather than our code, the realistic levers are mitigations:

- **Custom drag without the OS modal move loop**: handle `WM_NCLBUTTONDOWN`/HTCAPTION
  ourselves and move the window via `SetWindowPos` on `WM_MOUSEMOVE` (with a capture),
  *not* `DefWindowProc`, so tao/wry's loop keeps pumping and the webview keeps
  repainting. (This is the standard escape from the modal move loop; cost is
  re-implementing drag + snap interaction.)
- **Window-to-Visual or Visual hosting** for WebView2
  (`COREWEBVIEW2_FORCED_HOSTING_MODE`) to change the child-HWND compositing model —
  research whether wry 0.55 supports/benefits from it; this is exploratory.
- Confirm whether a newer `wry`/`tao`/WebView2 runtime improves the child-HWND
  repaint-during-move behavior (check wry changelog past 0.55.1 for move-loop /
  repaint fixes).

---

## Appendix — file/line index for the drag path

- Frameless config: `apps/desktop/src-tauri/tauri.conf.json:21` (`"decorations": false`)
- `win_snap::install` call site: `apps/desktop/src-tauri/src/lib.rs:399-402`
- Subclass proc (DwmDefWindowProc first, every message):
  `apps/desktop/src-tauri/src/win_snap.rs:229-303` (DWM call at `:272`)
- DWM frame extension: `apps/desktop/src-tauri/src/win_snap.rs:208-222`; re-extend on
  activate/DPI at `:281`
- HTCAPTION over the titlebar: `apps/desktop/src-tauri/src/win_snap.rs:397`
- `data-tauri-drag-region` regions: `apps/desktop/src/components/Titlebar.tsx:250`,
  `:343`, `:381` (and Sidebar.tsx:596/618/643)
- Old per-move IPC storm (now removed) + the move-loop description:
  `apps/desktop/src/components/Titlebar.tsx:89-96`, current report path `:126-176`
- DOM xterm renderer rationale (no WebGL context):
  `apps/desktop/src/components/Terminal.tsx:3-17`
- Versions: `apps/desktop/src-tauri/Cargo.lock` — `tauri 2.11.2`, `wry 0.55.1`,
  `tao 0.35.3`

## References (known upstream issues)

- Microsoft — *Windowed vs. Visual hosting of WebView2* (child-HWND windowed-hosting
  model): https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting
- wry #616 — "Webview frozen until mouse moves on Windows":
  https://github.com/tauri-apps/wry/issues/616
- tauri #9445 — "Weird drag behavior on windows":
  https://github.com/tauri-apps/tauri/issues/9445
- tauri #11345 — repositioning broken on Win11 with decorations:false:
  https://github.com/tauri-apps/tauri/issues/11345
- SDL #1059 — "main thread is blocked when user resizes or moves a window" (the modal
  move loop): https://github.com/libsdl-org/SDL/issues/1059
- WebView2Feedback #1096 — excessive WM_PAINT with composited styles:
  https://github.com/MicrosoftEdge/WebView2Feedback/issues/1096
