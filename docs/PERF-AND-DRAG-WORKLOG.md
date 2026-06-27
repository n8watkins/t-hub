# T-Hub Performance & Drag-Lag — Master Worklog

Date: 2026-06-26 → 2026-06-27 · Branch: `main` · **Single source of truth.**

This doc **consolidates and supersedes** the scattered perf notes:
`DRAG-LAG-INVESTIGATION.md`, `PERF-OPTIMIZATION-REVIEW.md`,
`PERF-MAIN-UNCONFIRMED-PATCH.md` (all deleted), and `FOCUS-RETURN-LAG-ANALYSIS.md`
(lives in the `codex/client-review` worktree; its findings are folded in below).
The pre-existing `PERF-AUDIT.md` (broader/older history) is kept as-is.

---

## TL;DR — where we are now

- ✅ **The dominant cause is FIXED:** a `claude -p /usage` **focus-storm** was pegging
  WSL/CPU. App went from "almost unusable" to **"a lot better."**
- 🔬 **Residual being fixed:** a *busy terminal* still stuttered (DOM renderer) while
  the rest of the app stayed smooth → **GPU canvas renderer** (v0.3.9, under test).
- 📋 **Still to tackle:** see §6 — the reap-when-not-in-workspace directive (biggest
  remaining lever), 6 correctness findings, and a handful of medium/low perf items.

---

## 1. The three problems (kept separate — they were repeatedly conflated)

| | Symptom | Root cause | Status |
|---|---|---|---|
| **A** | Drag/resize/maximize/minimize → **3-4s hard freeze then jump**, idle/empty, size-independent, native-frame too | CPU/WSL starvation of the OS move loop by the usage subprocess storm | ✅ **Fixed** (v0.3.8) — user-verified |
| **B** | General sluggishness, "used to be faster", slow session creation | Same usage storm | ✅ **Fixed** (v0.3.8) |
| **C** | App-switch (Alt-Tab back) 1-2s stall | Focus-refresh burst; the only uncached offender was `claude -p /usage` | ✅ dominant offender fixed; rest backend-cached |
| **D** | A *busy* terminal stutters while the rest of the app is smooth | xterm **DOM renderer** can't keep up with Claude TUI full-screen repaints | ✅ CANVAS renderer (0.3.9) renders smoothly; window-op stale-frame fixed (0.3.10/0.3.11). **Residual:** lags when dragging *while* a focused session works → §6 |

---

## 2. The dominant confirmed root cause for A/B/C

This is the **dominant, user-verified** cause of the drag freeze (A), the general
sluggishness (B), and the app-switch stall (C). Residual focus lag (C) and the
busy-terminal renderer jank (D) still need post-fix verification — see §6 — but the
structural hypotheses (win_snap / frameless / redirection-bitmap, §4) are
**conclusively ruled out** and should not be re-opened.

The sidebar Usage strip read plan usage by running **`claude -p /usage`** — which
spawns a **full Claude CLI (Node) in WSL** (`wsl.exe → bash → script → $SHELL -ilc →
claude`, ~2s startup). Three things made it a wrecking ball:

1. It re-ran on **every window `focus` event, unthrottled** (`UsageStrip.tsx` —
   `addEventListener("focus", refresh)`). Focus fires constantly.
2. `claude -p /usage` is **flaky (~65% `ok=false`** — the numbers need a network
   round-trip and the process often prints only the intro line), so on failure it
   **retried twice → ~4s per cycle**.
3. Net: a near-constant storm of heavy Claude subprocess spawns pegging WSL/CPU —
   **even when the app looked idle** (the poll doesn't need tiles).

**Why it looked like a graphics bug:** a drag enters the OS modal move loop, which
needs steady CPU to track the cursor. Starved by a ~4s usage cycle, it **froze
~3-4s then jumped** — perfectly mimicking a compositor/window bug, which is why the
earlier reports (and we) chased WebView2/frameless/win_snap/transparent for so long.

**The breakthrough was reading the diag log** (`<win-home>/.t-hub/diag.log`) — i.e.
looking at what the app was *doing*, not theorizing about rendering.
> Diag-log caveat: the file was 135 MB but **99.5% is an old `setSnapshot` logging
> flood** (already fixed by `d6d71ef`/`71a42ee`), NOT the usage storm. Real usage
> evidence = ~4,636 lines confirming ~4s/cycle, ~65% failure, bursty 4-7s clusters.

---

## 3. What worked — fixes shipped

| Ver | Commit | Change | Effect |
|---|---|---|---|
| 0.3.2 | `71a42ee` | Drop Titlebar `onMoved` per-move IPC; dedup status-snapshot fan-out | event flood |
| 0.3.3 | `0afc609` | SocketEmitter-only (kill TeeEmitter double-fanout); Rust terminal-output coalescing; dedup maximized hooks | event volume halved |
| 0.3.4 | `2324b0b` | App-version stamp in titlebar | build identity |
| 0.3.5 | `964636d` | *Diag:* `win_snap` off by default | ruled win_snap OUT |
| 0.3.6 | *(clone)* | *Diag:* `decorations:true` native frame | ruled frameless path OUT |
| 0.3.7 | `49aec86` | `transparent:true` (+ opaque bg); restore win_snap; (swept in the bg-terminal throttle) | ruled redirection-bitmap OUT |
| **0.3.8** | **`3285e54`** | **Throttle usage+codex focus refresh to 60s** | **✅ Fixed the dominant offender for A + B + C** (user-verified) |
| 0.3.9 | `93deab7` | **xterm CANVAS (GPU) renderer** + `@xterm/addon-canvas` | ✅ fix D — busy terminal renders smoothly (user-verified); avoids the WebGL blank-grid |
| 0.3.10 | `a0ba809` | Force terminal repaint on window-state change (`repaintMount.ts`) | ✅ canvas stale-frame after maximize/minimize ("scroll to reload") fixed |
| 0.3.11 | `1ca1482` | Tighter post-window-op repaint (leading-frame rAF + 50ms trailing) | snappier terminal refocus after maximize/minimize |
| 0.3.12 | `0891d10` | Usage strip refreshes on focus only when STALE (gate on last-GOOD read) | ✅ Claude weekly/5h stop blanking/reverting (user-verified) |
| 0.3.13 | `3807c29` | **Codex usage from the LIVE session rollout** (not the stale `logs_*.sqlite`); timestamp-selected; frontend window time-advance | ✅ Codex weekly/session correct + match `/status` (user-verified) |
| **0.3.14** | *(pending)* | **Cold-first-drag fix "Option A": suppress focus-triggered work during a window drag** (`windowInteraction.ts` `runWhenIdle` + `isInteracting`; wrap all 6 focus handlers) **+ gate Codex polling on an open Codex tile** | first-drag-after-unfocus freeze (focus storm) — awaiting user verify |
| — | `49aec86` (swept) | Background-terminal output throttling (fg rAF vs bg 250/1000ms/512KiB); windowMaximized rAF+in-flight guard | C/B |

One-time: **killed ~4 GB of orphaned `claude` processes** (they survive SIGTERM →
needed SIGKILL) — leaked by the workspace-close path (see §6).

---

## 4. Hypotheses RULED OUT (don't re-chase these)

events/IPC flood · CSS blur (none) · indicator animations (only on active sessions) ·
memory pressure (freed ~4 GB, no change) · webview process bloat (T-Hub owns 1 env) ·
**win_snap** (0.3.5 off, still froze) · **WebView2 redirection bitmap** (0.3.7
`transparent`, still froze) · **frameless path** (0.3.6 native frame, still froze) ·
**GPU/software-render as the cause of the A/B/C window-interaction freeze** (native
resize was smooth → graphics wasn't starving the move loop; the usage storm was).
All of A/B/C traced to the **usage storm**. This does **not** rule out terminal
rendering for **D** (busy-terminal stutter), which IS the **DOM renderer** — the
canvas renderer (§3, v0.3.9) addresses D, a separate axis from the A/B/C freeze.

---

## 5. Architecture note (we are NOT thread-limited)

- Tauri's Rust backend already runs a **multi-threaded tokio runtime + unlimited
  `spawn_blocking`**; terminals run as **separate WSL processes**. Not capped.
- The real constraint is the **single WebView2 UI/JS/render thread** — *identical to
  VS Code* (both are Chromium). You can't add threads to it.
- VS Code stays smooth by **offloading off that thread** (extension host, language
  servers, search = separate processes) and **GPU terminal rendering (WebGL)** — not
  by adding UI threads.
- Our two gaps vs VS Code, both fixable, neither architectural: (a) we render
  terminals on the **DOM** (the v0.3.9 canvas renderer closes this), and (b) we had
  the usage-storm bug (fixed). Web Workers could offload *computation* (ANSI/URL
  parsing) but **not** DOM/rendering.

---

## 6. What's LEFT to tackle (prioritized)

**Cold-first-drag = FOCUS STORM (distinct from the A1 emit residual below)**
- [x] **First drag "out of the blue" freezes; the second is smooth.** **ROOT CAUSE
      (workflow, adversarially verified):** clicking the title bar of an UNFOCUSED
      window fires a `focus` transition, and ~8 focus handlers fire at once on the
      single UI thread right as the OS drag loop starts — repaint EVERY terminal
      (dominant), plus `gitInfo` per tile, `listTerminals`, `recentSessions` (+ a
      JSON diff), claude/codex usage IPC. The 2nd drag has no focus transition → no
      storm. **Option A SHIPPED (v0.3.14):** `windowInteraction.ts` exposes
      `isInteracting()` (set on pointerdown + Tauri `onMoved`/`onResized`, cleared
      ~250ms after) and `runWhenIdle(fn)` (defer one frame; if interacting, defer to
      a `th-window-settled` event). All 6 focus handlers (`repaintMount`, `Tile`,
      `RecentList`, `Canvas`, `UsageStrip` ×2) now wrap their refresh in
      `runWhenIdle`, so the storm runs AFTER the drag settles, never during.
- [ ] **Option B (de-storm focus handlers) — FOLLOW-UP, not yet done.** Option A only
      DEFERS the storm past a DRAG; the storm itself is still heavy, so a focus
      transition with **no drag still hitches** — most notably **ALT-TAB IN/OUT of the
      app** (user-reported on v0.3.13: "freeze that affects the ability to tab in and
      out"). Alt-tab is keyboard focus → no pointerdown, no `onMoved` → `isInteracting()`
      is false → `runWhenIdle` runs the storm after one frame, so the hitch remains.
      Option B fixes this by making the handlers non-blocking even when they run:
      (a) run focus work on an idle callback / after-paint instead of synchronously;
      (b) **coalesce duplicate IPC** — one batched `gitInfo` for all tiles instead of N,
      a single shared usage poll; (c) drop `RecentList`'s main-thread
      `JSON.stringify(prev)===JSON.stringify(list)` diff. Bigger change, more files,
      more regression surface — do it as a focused pass once Option A is verified.
      *(User explicitly asked to keep B queued here.)*

**Now / verify**
- [x] **Codex usage stale/reverting.** **DONE (v0.3.13).** Was scraped from
      `~/.codex/logs_*.sqlite` (only written on certain events → 6 days stale when a
      session was idle/out-of-credits, overwriting the good cached value). Now read
      from the LIVE session rollout (`~/.codex/sessions/**/rollout-*.jsonl`), the same
      data `/status` shows, selecting by event timestamp; frontend time-advances each
      window past its reset and polls only while a Codex tile is open
      (`useHasCodexSession`). Verified against the live rollout (session 0% left /
      weekly 70% left) and user-confirmed.
- [ ] **Drag/maximize lags when a FOCUSED Claude session is actively working** (the
      current residual — idle is fine; what's left after 0.3.8/0.3.9). **ROOT CAUSE
      (subagent, evidence-based — corrects an earlier wrong guess of a statusline
      spawn-storm):** the statusline is throttled to `refreshInterval:5` ≈ 0.2/sec/
      session (verified against the live 19 MB journal, peak 5/sec) — NOT the cause.
      The cause is **terminal output `app.emit(terminal://output)` marshaling onto the
      Windows MAIN/UI thread** (`remote_pty.rs:329`): a freshly-sent prompt triggers an
      output burst (echo + spinner + token stream + TUI repaints) → ~125 emits/sec per
      streaming terminal *even after* the 8ms/256KB coalesce + frontend rAF batch, and
      during a drag the OS modal move/size loop owns that same main thread → the emits
      drain slowly → the drag stutters.
      **FIX (Tauri app — normal rebuild, NO t-hub-agent rebuild):** set an
      `AtomicBool window_interacting` from window move/resize events (lib.rs setup);
      while true, WIDEN the terminal-output coalesce window (8ms → ~100ms) + raise
      `MAX_BATCH_BYTES` in `remote_pty.rs` `emit_batch`/`reader_loop`, so output
      accumulates and flushes in a few large emits AFTER release instead of ~125/sec
      during the drag. Optionally coalesce `control://event` emits (`control_client.rs:245`)
      the same way. *Principle: stop marshaling work onto the main UI thread while
      Windows owns it for the drag.*
- [ ] *Secondary (separate builds/scope):* statusline self-throttle in `t-hub-agent`
      (skip the journal append+fsync + the per-render `tmux display` spawn if <~2s
      since last) → shrinks the 19 MB journal — **t-hub-agent builds separately**.
      Memoize hot sidebar/tile components (`React.memo` on `TerminalRow`/`Tile`/
      `SupervisionTree`). Drop the no-frontend-consumer `agent://journal` emit.
- [ ] **Verify v0.3.9 canvas renderer.** Acceptance for the whole matrix: the busy
      terminal is **smoother** than the DOM build AND there is **NO blank/stale-frame
      regression** (the regression to watch — the old WebGL blank-grid). If any case
      blanks, revert to DOM or scope canvas to foreground tiles. v0.3.10 already
      force-repaints terminals on window-state change (canvas stale-frame fix), so
      these cases also re-validate that path. Cases:
  - [ ] 1 busy TUI (Claude full-screen repaint) + the rest idle.
  - [ ] 6-12 tile grid, mixed busy/idle.
  - [ ] Tab-switch back to a tab whose hidden terminals emitted output while parked.
  - [ ] Open / close the spawn-preset menu.
  - [ ] Open / close the Preview and Settings overlays.
  - [ ] Maximize/fullscreen a tile, then restore.
  - [ ] Resize the grid gutters.
  - [ ] Pop out a workspace to a satellite, then return.
  - [ ] Windows minimize, then restore.
  - [ ] Alt-Tab away from T-Hub, then back.

**High value — architectural (the directive: "nothing runs unless in a workspace")**
- [ ] **Reap-on-leave-workspace.** ⚠️ **"Leave workspace" here means workspace
      CLOSE / DELETE — NOT normal switching.** Switching to another workspace must
      **preserve** every session; popping a workspace out to a satellite window must
      **preserve** too. Only close/delete reaps. Today tile-close kills the session,
      but **workspace-close orphans it** (`WorkspacesList → closeTab` ignores returned
      ids → leaked tmux + claude; this caused the ~4.5 GB). Fix: on workspace
      close/delete, **first record the binding to Recent (recall metadata)**, then kill
      the tmux/PTY **and reap the process tree (SIGKILL — they ignore SIGTERM)**.
      Recall stays available via Recent → `claude --resume <id>` (transcript on disk;
      WS-6 `db.rs`/`list_orphaned_sessions`). This targets the orphan leak **without**
      making navigation destructive. *Subsumes much of perf-rec #1/#5.*

> **Session-lifecycle semantics (the product contract — keep this stable; future
> perf work must NOT accidentally change it):**
>
> | Action | Sessions | Recall |
> |---|---|---|
> | **Tile close** | kill that one session | yes, via Recent |
> | **Workspace close/delete** | kill all of its sessions | yes, via Recent |
> | **Workspace switch** | **preserve** (kill nothing) | n/a (still live) |
> | **Pop-out to satellite** | **preserve** (kill nothing) | n/a (still live) |
> | **App / window close** | ⚠️ **OPEN DECISION** — explicitly decide preserve-vs-reap | TBD |
>
> The only paths that kill are tile close and workspace close/delete; both record
> recall metadata to Recent *before* the kill. App/window close is deliberately left
> undecided here — flag it before touching that path.

**Correctness findings (all CONFIRMED by review, not yet fixed)**
- [ ] #1 close kills instead of detaches — *reconciled by the reap directive* (kill on
      leave + recall via Recent is the intended model).
- [ ] #2 workspace-close orphans sessions (the leak — same fix as reap above).
- [ ] #3 per-window automation duplicates — `autoContinueMount`/`rulesMount` run in
      EVERY window incl. satellites → double spawns/continues. Gate on `!isSatellite()`.
- [ ] #4 satellite fallback can boot a blank popped-out window.
- [ ] #5 rebinding a chord doesn't remove it from other commands → old binding shadows.
- [ ] #6 Ctrl/Cmd+Shift+W delete-confirm fires while typing in an input (no editable guard).
- [ ] #7 **Recent-resume + spawn-menu have NO busy/pending gate** → a double-click stacks
      duplicate tmux+claude spawns (`RecentList.tsx:352-355`, `Canvas.tsx:216-230`,
      `workspace.ts:967-989`). On-theme with "stop unneeded spawns" — disable the control
      until the spawn settles. *(from the codex `PERF-AUDIT.md` follow-up F6/Fix#6)*

**Perf — medium (from the optimization review)**
- [ ] Debounce durable **workspace persistence** (tiers: immediate for lifecycle,
      debounce for focus/tab/layout).
- [ ] **Foreground-aware repaint broadcasts** (don't `repaintAllTerminals` →
      `term.refresh` on every terminal for an overlay toggle).
- [ ] **File-search cancellation** (request-id stale suppression for huge repos).
- [ ] **Drag commits state only on `pointerup`.** During a grid/sidebar drag, drive CSS
      variables imperatively (+ an epsilon no-op guard) and skip the React
      `setRows`/`setCols`/`setSidebarWidth` + persistence writes until release
      (`Canvas.tsx:621-707`/`724-801`, `App.tsx:243-247`). *(codex `PERF-AUDIT.md` follow-up F1/F2/Fix#2)*

**Perf — low (already mitigated by backend TTL caches; do only if instrumentation shows jank)**
- [ ] Throttle the **recent / git / listTerminals** focus refreshes. These *non-usage*
      focus refreshes still fire on every focus, but are **backend-cached** (git ~3.5s
      `git.rs:57`, recent 15s `recent.rs:47`), so they're **low priority** — revisit
      only if post-v0.3.8 instrumentation shows a **remaining focus-return stall** (the
      `claude -p /usage` storm, the one uncached offender, is already fixed). The
      proposed centralized `focusRefresh.ts` scheduler is **over-engineered** — skip it.
- [ ] Centralize git polling by cwd (frontend) — low priority (backend-cached).

**Usage strip freshness (regression from the 0.3.8 throttle — user-reported "weekly + 5h not updating")**
- [x] **Claude usage (weekly + session) lags / shows stale.** **DONE (v0.3.12).**
      **LIVE EVIDENCE (ran the exact command 3×, 3/3 success):** `claude -p /usage` is
      **RELIABLE when called individually** — it prints session/week %s fine. The ~40%
      `ok=false` in the diag **correlates with HIGH CALL RATE** (the pre-throttle focus
      storm / clustered calls): under load the usage round-trip doesn't land before the
      process prints intro-only → likely **light rate-limiting / a timing race**, NOT a
      parse-format or auth bug. So **fewer requests directly improves the success rate** —
      the 0.3.8 throttle already helps; lengthening the cadence (Option 2) helps more.
      Separately, the *freshness* regression was that the throttle gated on the last
      *run* (success OR fail), so a failing call reset the gate → stale strip.
      **FIX SHIPPED (both hooks):** `UsageStrip.tsx` now tracks **`lastGoodRef`** (set
      only on `u.ok`) in addition to `lastRunRef`. The focus gate skips when the data is
      fresh (`<USAGE_FRESH_MS` = 60s since a SUCCESS) **or** we just tried
      (`<USAGE_RETRY_GAP_MS` = 15s since any run, so a failing streak can't storm). A
      failed poll no longer blanks the strip (cached last-good values persist) and no
      longer blocks the next refresh past the short retry gap. Applies to **both**
      `useClaudeUsage` and `useCodexUsage`. "Fix the flakiness to one-shot" is NOT
      achievable; "make fewer, well-spaced requests + gate on last-good" is, and is now in.
- [ ] **Codex 5-hour + weekly may be stale/mis-parsed.** `codex_usage(wsl)` returns
      `ok=true` reliably (reads the newest codex session rollout, `codex.rs`), but the
      diag doesn't log the parsed windows. Verify it reads the **newest** rollout and
      parses BOTH the ~5h (primary) and weekly (secondary) windows correctly — the
      "5-hour not updating" report points here, not at the throttle.
- [ ] Decide whether `transparent:true` stays (it's the documented resize-redirection
      fix and is harmless) — currently kept.

---

## 7. Build / test loop

Local native MSVC build from an isolated Windows clone
(`C:\Users\natha\projects\Tools\t-hub\t-hub-app`): `git clone` the WSL repo →
`pnpm install` → edit `tauri.conf.json` for test (`createUpdaterArtifacts:false`,
`targets:["nsis"]`) → `pnpm tauri build` → install `T-Hub_<ver>_x64-setup.exe`.
Drag/resize/render lag can't be reproduced from WSL (no display), so every version
is hand-tested on Windows. (Full recipe lives in the author's external Claude Code
memory note `local-windows-build.md` — a local agent-memory note, **not** a file in
this repo.)
