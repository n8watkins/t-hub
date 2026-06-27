# T-Hub Performance & Drag-Lag Worklog

Date: 2026-06-26 → 2026-06-27
Status: **living document** — the authoritative trail of what we tried, what we
ruled out, what we shipped, and what's left. Read this before re-investigating;
several "obvious" causes are already conclusively ruled out below.

Companion docs: `DRAG-LAG-INVESTIGATION.md` (drag deep-dive), `PERF-OPTIMIZATION-REVIEW.md`
(the 6-item responsiveness review), `FOCUS-RETURN-LAG-ANALYSIS.md` (app-switch lag),
`PERF-MAIN-UNCONFIRMED-PATCH.md` (the bg-terminal patch that landed).

---

## 1. Symptoms (as reported, refined over the session)

There are **three distinct problems**, repeatedly conflated. Keep them separate:

| # | Symptom | Nature |
|---|---|---|
| **A** | **Drag/resize/maximize/minimize lag → a 3-4s COMPLETE freeze, then the window jumps to the target.** Reproduces **idle / empty workspace (0-2 tiles)**, **size-independent**, on a **native frame** too. Typing is fine *except* it's frozen *during* the drag. | Structural / contention. NOT yet fixed. |
| **B** | **General sluggishness — "used to be faster"**, slow session creation. | Workload. Dominant cause found + fixed (see §4, usage storm). |
| **C** | **App-switch return lag** (Alt-Tab back → 1-2s stall). | Focus-refresh burst. Dominant offender fixed; rest backend-cached. |

---

## 2. Hypotheses tested → outcomes (the elimination trail)

| Hypothesis | Verdict | How we know |
|---|---|---|
| Event/IPC flood (status snapshots, double-emitter fan-out, terminal output) | **FIXED — real wins, but NOT problem A** | dedup at source + SocketEmitter-only + rAF/Rust coalescing (commits below). Problem A persists idle/empty where no events flow. |
| CSS `backdrop-filter`/blur compositing | **RULED OUT** | none exist in the codebase. |
| Indicator animations (`th-ind-pulse`/spin) forcing repaint | **RULED OUT for A** | only render on active sessions; A reproduces idle/empty. |
| Memory pressure / leaked live processes | **RULED OUT as A's cause (but a real leak)** | killed **~4 GB** of orphaned `claude` processes; drag freeze unchanged. WSL still held 7 GB (doesn't auto-return). 5 GB free on Windows. |
| WebView2 process bloat | **RULED OUT** | T-Hub owns ONE webview env (6 procs); the "26 procs" were other apps system-wide. |
| `win_snap` Win32 subclass (DwmExtendFrameIntoClientArea + per-message DwmDefWindowProc) | **CONCLUSIVELY RULED OUT for A** | v0.3.5 shipped it **disabled** by default; still froze. |
| WebView2 opaque redirection bitmap (`transparent:false`) | **RULED OUT for A** | v0.3.7 set `transparent:true` (→ tao adds `WS_EX_NOREDIRECTIONBITMAP`); still froze. (Kept — it's the documented resize-lag fix and doesn't hurt.) |
| Frameless/custom-titlebar path | **RULED OUT for A** | v0.3.6 native frame (`decorations:true`); still froze 3-4s on the **native** title bar. |
| GPU/software-rendering | **UNLIKELY** | native-frame **resize** went smooth → GPU composites fine. No `--disable-gpu` flag. |
| `claude -p /usage` focus storm (no backend cache, ~4s flaky, fired on EVERY focus) | **CONFIRMED as B's dominant cause → FIXED** | diag log: ~4s/cycle, **~65% failure**, bursty 4-7s clusters; only offender w/o a backend TTL cache. Fixed in v0.3.8 (`3285e54`). |
| **A (drag freeze) = CPU/WSL contention from the focus-spawn storm** | **LEADING, UNCONFIRMED** | freeze duration (~3-4s) ≈ one usage cycle (~4s). **Testable on v0.3.8.** Fallback: the OS modal-move-loop (would need a custom drag). |

### Diag-log correction (important, don't repeat the mistake)
The `~/.t-hub/diag.log` was **135 MB**, but **99.5% (~1.0 M lines) is an old `setSnapshot`
debug-log flood** — 26 snapshot UUIDs each logged tens of thousands of times — which
**stopped ~02:17** when the status-snapshot dedup (`d6d71ef`/`71a42ee`) landed. The file
size is a *logging artifact of an already-fixed issue*, NOT a subprocess storm. The real
`claude -p /usage` subprocess evidence is **~4,636 lines (~899 cycles / 27 h)** — small in
volume but dominant in **cost** (only multi-second + flaky + uncached). Don't cite the
135 MB as "usage storm."

---

## 3. What is firmly established about Problem A (the drag freeze)
- Idle/empty, size-independent, **native-frame too**, both window-trails AND content-freeze, **3-4s hard freeze then jump**.
- Ruled out: events, CSS, animations, memory, webview-bloat, win_snap, redirection bitmap, frameless path, GPU.
- A 3-4s *hard freeze* (not frame jank) ⇒ the main thread / system is **blocked or starved** for ~3-4s when the move loop runs — consistent with the ~4s `wsl.exe`→Claude usage cycle (and the other still-unthrottled focus spawns) saturating WSL/CPU.
- **Next test:** measure drag on v0.3.8 (usage now throttled). If the freeze shrinks → contention confirmed; finish throttling the other focus spawns. If unchanged → it's the OS modal-move-loop; prototype a custom `SetWindowPos` drag that doesn't enter the modal loop (won't help maximize/minimize).

---

## 4. Fixes shipped (commit · version · effect)

| Version | Commit | What | Targets |
|---|---|---|---|
| 0.3.2 | `71a42ee` | Drop Titlebar `onMoved` per-move IPC storm; dedup status-snapshot fan-out at source | event flood; first drag attempt |
| 0.3.3 | `0afc609` | SocketEmitter-only (kill TeeEmitter double-fanout); Rust-side terminal-output coalescing; dedup the two maximized hooks | event volume halved |
| 0.3.4 | `2324b0b` | App version stamp in the top-left titlebar (build identity) | diagnostics |
| 0.3.5 | `964636d` | **Diagnostic:** `win_snap::install` skipped by default (`T_HUB_DISABLE/ENABLE_WIN_SNAP`) | A/B — ruled win_snap OUT |
| 0.3.6 | *(clone-only)* | **Diagnostic:** `decorations:true` native frame | A/B — ruled frameless path OUT |
| 0.3.7 | `49aec86` | `transparent:true` (+ opaque html/#root); restore win_snap default-on; (also swept in the bg-terminal throttle patch) | A/B — ruled redirection bitmap OUT |
| 0.3.8 | `3285e54` | **Throttle `claude -p /usage` + codex focus refresh to 60s** (mount + 5-min interval still force) | **B (dominant), candidate for A** |
| — | `49aec86` (swept) | Background-terminal output throttling (foreground rAF vs background 250ms/1000ms/512KiB) + windowMaximized rAF+in-flight guard | C/B (PERF-OPTIMIZATION-REVIEW #1, #2-half) |

One-time action (not a commit): **killed ~4 GB of orphaned `claude` processes** (they
survive SIGTERM; needed SIGKILL) — these were leaked by the workspace-close path (see §6).

---

## 5. Architecture clarification: "going back" does NOT need RAM
A kept-alive session = a live `claude` (Node) process in WSL (~400-570 MB each; we saw
~10 = ~4.5 GB, several idle 24-35 h). It does **not** need to stay resident: Claude
persists transcripts to disk, T-Hub records the tile→session binding (WS-6 `db.rs` /
`list_orphaned_sessions`), and Recent recall = `claude --resume <id>`. **Directive (user):
"nothing should be actively running unless it's in a workspace; recall via Recent."**
This is the right model and the biggest single RAM/CPU lever — see §6.

---

## 6. Outstanding / NOT yet addressed

**High-value, architectural:**
- [ ] **Reap-on-leave-workspace** (the directive). Today: tile-close kills the session, but **workspace-close orphans it** (`WorkspacesList → closeTab` ignores returned ids → leaked tmux + claude). Fix: on leave-workspace, record to Recent + kill the tmux/PTY + reap the process tree (SIGKILL needed). Subsumes much of PERF-OPTIMIZATION-REVIEW #1/#5.

**From PERF-OPTIMIZATION-REVIEW.md (6 items):**
- [x] #1 background terminal throttle — landed (`49aec86`). *Subsumed-by-directive: it lazily renders backgrounded sessions that, per the directive, shouldn't be running at all.*
- [x] #2 coalesce maximize IPC — already done (`windowMaximized` in-flight/pending guard). The "audit TerminalPool slot observer" half = low value; **skip** (would risk the slot-rect hardening for ~no win).
- [ ] #3 debounce workspace persistence (tiers) — not done.
- [ ] #4 centralize git polling by cwd — **low priority**: backend already has a 3.5s TTL cache (`git.rs:57`) that absorbs the focus burst; the frontend store rewrite is largely redundant.
- [ ] #5 foreground-aware repaint broadcasts — not done (subsumed by directive for out-of-workspace).
- [ ] #6 real file-search cancellation — not done (low impact except huge repos).

**From FOCUS-RETURN-LAG-ANALYSIS.md:**
- [x] usage + codex focus throttle — **done** (`3285e54`, the dominant offender).
- [ ] recent focus refresh — low value (backend 15s TTL `recent.rs:47` already absorbs).
- [ ] git focus refresh — low value (backend 3.5s TTL).
- [ ] `listTerminals` focus — minor min-interval.
- [ ] **The proposed centralized `focusRefresh.ts` scheduler = OVER-ENGINEERED** per review: backend TTL caches already collapse git/recent/codex bursts; the 15-line usage throttle captured ~all the win. Treat the scheduler as optional.

**Correctness findings (separate client-review, all CONFIRMED, not yet fixed):**
- [ ] #1 close kills instead of detaches — reconciled by the directive (kill on leave-workspace + recall via Recent is the intended model).
- [ ] #2 workspace-close orphans sessions (the leak — see reap above).
- [ ] #3 per-window automation duplicates (autoContinue/rules run in every window incl. satellites) — gate on `!isSatellite()`.
- [ ] #4 satellite fallback can boot a blank window.
- [ ] #5 rebinding allows a duplicate chord that shadows the new binding.
- [ ] #6 Ctrl/Cmd+Shift+W delete-confirm fires while typing in an input.

**Open question:**
- [ ] Why does `claude -p /usage` fail ~65% of the time (`ok=false`)? (Likely the wsl.exe shell-resolution issue — see the memory note about `wsl.exe -- bash` running zsh; pass `-e`.) Fixing the *failure* removes the 2nd retry attempt (~halves each cycle's cost).

---

## 7. Local build/test loop (how these were produced)
Local Windows MSVC build, isolated clone at `C:\Users\natha\projects\Tools\t-hub\t-hub-app`
(`git clone` of the WSL repo → `pnpm install` → `pnpm tauri build`); test config disables
`createUpdaterArtifacts` + uses `targets:["nsis"]`. See memory `local-windows-build.md`.
Each version above shipped as a `T-Hub_<ver>_x64-setup.exe` and was hand-tested on Windows
(drag/resize/maximize), since the lag can't be reproduced from WSL (no display).
