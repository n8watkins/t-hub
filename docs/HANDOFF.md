# T-Hub — Session Handoff

**Last updated:** 2026-06-27 · **Branch:** `main` · **App version:** `0.3.11` (local builds; perf/drag overhaul — not pushed, not released)

> **Zero-context handoff.** Read this file in full, plus any doc it links that's
> relevant to your task. Every decision below is already made — **do not re-ask
> the user anything answered here.** Working dir: `/home/natkins/projects/tools/t-hub/t-hub-app`.

---

## 1. What this is

**T-Hub** — a Tauri 2 desktop "command center" for running and supervising many Claude Code / Codex agent sessions at once (a local cockpit for persistent agent + shell terminals).

- **Stack:** Rust backend + React/TypeScript/Tailwind frontend with xterm.js terminals.
- **Repo:** a **pnpm monorepo**. `apps/desktop` = the app (Tauri); `apps/site` = the marketing site. Work happens in `apps/desktop`.
- **How it reaches the agents:** on **Windows** it drives WSL via `wsl.exe -e bash` running **`tmux -L t-hub`**; under `#[cfg(unix)]` it attaches tmux **directly** (this is the Linux/WSLg dev variant). Each terminal's tmux session is `th_<terminalId[..8]>`.
- **Backend surface:** ~50 `#[tauri::command]`s + an **MCP server** (`t-hub-mcp`, 22 tools) + a **loopback control channel** (`control.rs`, a `127.0.0.1` TCP NDJSON server with a per-launch token). **The control channel is now the spine of the server-split** (§2).
- Codebase/repo identifiers deliberately stay `t-hub` (lowercase); the user-facing name is "T-Hub".

---

## 2. State

### Latest session (2026-06-27): PERFORMANCE & FREEZE OVERHAUL (v0.3.12→v0.3.18) — see [`PERF-AND-DRAG-WORKLOG.md`](./PERF-AND-DRAG-WORKLOG.md)

**[`docs/PERF-AND-DRAG-WORKLOG.md`](./PERF-AND-DRAG-WORKLOG.md) is the single source of
truth** for everything below (fixes shipped, hypotheses ruled out, full prioritized
backlog). Read it in full before doing any perf/drag work — do NOT re-chase the
ruled-out causes (win_snap, transparent/redirection-bitmap, frameless, memory, event
flood). Versions 0.3.2→0.3.11 are **local Windows builds only — NOT committed-to-a-tag,
NOT pushed, NOT released** (commits are on local `main`).

**THE HEADLINE (v0.3.17, watchdog-confirmed):** the original *always-present, sporadic,
"super-laggy" hard freeze* — the one that ghosted the T-Hub icon in Alt-Tab (Windows
hung-window: UI thread not pumping for ~5s) — was **`control_request` running on the
MAIN THREAD.** It's the loopback transport for `recent`/`git`/`usage`/`codex`/`files`,
and it was a **synchronous** `#[tauri::command]`; Tauri runs sync commands on the main
UI thread, so a slow backend op (a flaky ~4s `claude -p /usage`, a stalling
`\\wsl.localhost\` read) blocked the whole window for its full duration. **Fix: `async`
+ `tauri::async_runtime::spawn_blocking`** (drop the `State` borrow before the await).
After it: **zero** `rust-main` blocks across 326 lines of active use (was 2–4s every
~minute). Found by building a **Rust main-thread watchdog** (`hangwatch.rs`,
`run_on_main_thread` probe + emit counter) once a JS detector proved the block was
host-side, not renderer-side (emit-flood was *ruled out* — blocks had only ~20–54 emits).

**Other wins this session (all on `main`, local Windows builds, NOT pushed/tagged):**
- **v0.3.14** cold first-drag freeze (focus storm) — Option A `lib/windowInteraction.ts`
  defers focus work during a drag (`runWhenIdle`/`isInteracting`). *User-verified.*
- **v0.3.13** Codex usage — read the LIVE session rollout (`~/.codex/sessions/**`,
  timestamp-selected) not the 6-day-stale `logs_*.sqlite`; polls only when a Codex tile
  is open. *User-verified.*
- **v0.3.18** Claude usage — **statusline-first**: live `rate_limits` from the per-turn
  statusline (free, account-wide, NOT per-terminal); `claude -p /usage` only the
  cold-start fallback (one check on load, then never while a session runs). *Verified:
  `/usage` dropped from every-few-min to once-on-load.*
- v0.3.12 usage freshness (no blank/revert); v0.3.9 GPU canvas renderer; v0.3.10/11
  stale-frame repaint; killed ~4 GB orphaned `claude`; consolidated perf docs.
- **Diagnostics shipped & STILL ARMED in release:** `hangDetector.ts` (JS) +
  `hangwatch.rs` (Rust watchdog) → `{"t":"hang",...}` in `~/.t-hub/diag.log`.
  ⚠️ Tier-2 decides keep/gate/remove now the freeze is fixed.

**EXECUTION PLAN — next context (tiered; full per-item detail + file:line in worklog §6).**
⚠️ A recent-work REVIEW workflow is in flight (scanning v0.3.12–v0.3.18 for missed polish
+ OTHER sync-command-on-main-thread freeze sources like `control_request` was). FOLD its
confirmed findings into Tier 1/2 before executing — they may add items or reprioritize.

**Tier 1 — parallel WORKTREE batch** (10 items, all FRONTEND, low-risk, zero conflict with
the backend hang/emit work; all triaged "ideal for a worktree"). Several share
`Terminal.tsx`/`repaintMount.ts`, so do them SEQUENTIALLY in ONE worktree, then one build:
  1. **maximize doesn't re-FIT terminals** (user-reported) — `repaintMount.ts onResized`
     broadcasts `refresh()` not `fit()`; call `refreshTerminal()` (which fits) on the settle.
  2. **#6** delete-confirm fires while typing — add editable-target guard in
     `useLifecycleKeybinds.tsx` (export+reuse `isEditableTarget` from `Canvas.tsx`).
  3. **#5** chord rebind leaves a stale shadow binding — in `store/keybindings.ts setBinding`/
     `setPrefixedBinding`, delete the chord from any other command first.
  4. **#3** per-window automation duplicates — `if (isSatellite()) return;` in
     `autoContinueMount.ts`/`rulesMount.ts` installers (dup spawns/continues in satellites).
  5. **#4** satellite boots a BLANK window — `workspace.ts setTerminals` satellite branch
     never repopulates an empty order; recover from the live terminal list.
  6. **#7** double-click stacks duplicate spawns — per-action pending guard (Set in
     `workspace.ts` recall + local `spawning` in `Canvas.tsx`/`SpawnMenu.tsx`/`RecentList.tsx`).
  7. **hidden-tab output queue UNBOUNDED** (memory) — cap/collapse stale `pending[]` for a
     parked terminal in `Terminal.tsx` (~line 164/889).
  8. **foreground-aware repaint** — `Terminal.tsx:1103` `onRepaintAll` should early-return
     when `!foregroundRef.current` (don't refresh bg terminals on an overlay toggle).
  9. **file-search stale-result cancellation** — request-id guard in `FilePanel.tsx` +
     `FileTree.tsx` search effects (`searchFiles` has no ordering guarantee).
  10. *(small, med-risk)* **drag commits React state only on pointerup** — `Canvas.tsx`
      onPointerMove drives flexGrow/CSS imperatively, commit `setRows/Cols` on release.

**Tier 2 — perf polish on `main`** (the residual stutters the user still feels, now that the
big freeze is gone):
  - **Option B** — de-storm the focus handlers so **alt-tab in/out** stops hitching (focus
    work has no drag to defer against): run on idle-callback + coalesce the N `gitInfo`/IPC
    into one + drop RecentList's `JSON.stringify` diff. (Option A already deferred them
    during drags; B makes them non-blocking when they DO run.)
  - **A1 emit-coalesce during interaction** — `AtomicBool window_interacting` set from
    window move/resize; while true widen the terminal-output coalesce window (8ms→~100ms) +
    `MAX_BATCH_BYTES` in `remote_pty.rs` (the "freeze while a terminal actively works" case).
  - **Pinpoint the "staggered" sub-500ms stutter** — drop the `hangwatch.rs` `STALL_MS`
    threshold to ~200ms, reproduce, read what it names, fix that.
  - Drop the consumer-less `agent://journal` emit.
  - **Review-surfaced (recent-work review, all LOW/confirmed):**
    - `spawn_blocking` the async commands that block in-body (worker-pool starvation,
      not a main freeze): **`tmux_scroll`/`tmux_exit_scroll`** (fire on scroll —
      highest value), **`list_dir`/`read_text_file`** (UNC reads), then `git_commit`/
      `git_worktree_*` for consistency. Mirror `git_info`'s `spawn_blocking` pattern.
    - **`diag_log`/`diag_clear` are sync `#[tauri::command]`s** (file I/O on the main
      thread when invoked from JS) + the diag.log **reopens per line and never
      rotates** (unbounded growth; ~20 always-on callers). Make `diag_log` async +
      a buffered/rotated background writer (one fd, size cap). *(Doing this also lets
      the still-armed hang detectors log fully off-thread.)*
    - Codex **Windows DB-fallback drops `plan_type`** (python slices from the
      `rate_limits` offset; `plan_type` sits before it) — rollout path is fine.
    - Polish: `advanceCodexUsage` reallocates every 60s tick even when nothing rolled
      over; in-flight cold-start `/usage` promise isn't cancelled when the statusline
      arrives; `useHasCodexSession` recomputes O(N) with an extra `getState()` per
      terminal; `runWhenIdle`'s deferred `onSettle` can fire a refresh on an unmounted
      component (RecentList/UsageStrip). All benign; tidy when nearby.
  - DONE in v0.3.19 (this review pass): fixed the Rust watchdog's ironic main-thread
    `diag_log` (now logs off-thread via a channel); statusline usage backfills each
    window independently (a partial snapshot no longer blanks the other); both hang
    detectors kept ON by default (user wants ongoing monitoring).

**Tier 3 — architectural (careful — keep the lifecycle contract in worklog §6):**
  - **Reap-on-leave-workspace** — workspace CLOSE/DELETE currently ORPHANS sessions (the
    original ~4.5 GB leak): record to Recent, then SIGKILL the process tree. MUST preserve
    on workspace SWITCH and pop-out-to-satellite. App/window-close is an OPEN decision.

**Tier 4 — verify / housekeeping:** canvas-renderer acceptance matrix (worklog §6 checklist) ·
`t-hub-agent` statusline self-throttle (SEPARATE binary/build) · `React.memo` hot
sidebar/tile components.

**Local Windows build (this session's verified flow):** see [`PERF-AND-DRAG-WORKLOG.md`](./PERF-AND-DRAG-WORKLOG.md) §7 + the Claude Code memory note `local-windows-build.md`
(external memory, not a repo file). Summary: an isolated Windows clone at
`C:\Users\natha\projects\Tools\t-hub\t-hub-app` (`git clone` the WSL repo → `pnpm install`
→ set `createUpdaterArtifacts:false` + `targets:["nsis"]` → `pnpm tauri build`); the lag
can't be reproduced from WSL (no display), so every build is hand-tested on Windows.

### Baseline: v0.2.0 SHIPPED
`v0.2.0` is tagged (`9ee6b75`) and the GitHub Release is published — signed `T-Hub_0.2.0_x64-setup.exe` + `latest.json`, so **auto-update is LIVE**. That release shipped the herdr-parity wave (worktree workflow, rebindable keymap, rules engine, OS toasts, session-restore, a perf overhaul) plus post-release cheap-wins (dedup refactor, light tray recovery, vitest). Details in [`ROADMAP-PLAN.md`](./ROADMAP-PLAN.md).

> ⚠️ The v0.2.0 **Windows** build was **not** runtime-tested — only the Linux/WSLg dev variant. If Windows surprises, **fix-forward with v0.2.1**, don't block.

### This session: THE SERVER-SPLIT (item ⑥) is largely shipped — all on `main`, ahead of the v0.2.0 release

The keystone roadmap item. The bet: pull T-Hub's "brain" out of the desktop GUI and onto the **loopback control socket**, so the same wire that serves localhost today can later reach a remote host — any device gets the full cockpit. The webview's JS can't open a raw TCP socket, so the socket I/O lives in the Rust shell (the thin-client seam). **Progress vs the §6 M1→M4 plan in [`SERVER-SPLIT-AND-ROADMAP.md`](./SERVER-SPLIT-AND-ROADMAP.md):**

- **M1 — Decouple locally — ✅ SHIPPED.** GUI↔backend traffic for server-owned state crosses the socket: a command request/response transport (`control_request`), the event stream forwarded into the webview (`spawn_event_forwarder` → re-emits `control://event`), and a `TeeEmitter` so channels migrate **one at a time** (events go to BOTH the webview and the socket until a channel is fully flipped). Commits: `bca229d` (first slice — one read command + the event stream), `090225e` (widen — the 0.5 supervision/status surface).
- **M2 — Tiles over the wire — ✅ core SHIPPED.**
  - **M2a:** a tile's PTY (`tmux attach`) streams over the socket. Server emits `{out}`/`{exit}` frames and reads `{write}`/`{resize}`; client side is `RemotePty`/`RemotePtyManager`. Commits: `935c1a0` (server half), `72d5fa1` (client flip), `73cfe99`/`d08615a` (review fixes — bounded per-subscriber write, unified `tmux_target`).
  - **M2b:** persistent server key (`~/.t-hub/server-key`, stable identity across restarts), **opt-in** network bind, and an `is_allowed_peer` gate (loopback + Tailscale CGNAT `100.64.0.0/10` + `fd7a:115c::/32`, with IPv4-mapped-IPv6 normalization). Thin-client mode via `T_HUB_REMOTE_ADDR` / `T_HUB_REMOTE_TOKEN`. Commits: `c07d3ec` (core), `47cb906` (hardening — conn cap, constant-time token compare, reconnect backoff), `040dc8b` (security-review fixes — dual-stack peer norm + forwarder backoff).
- **M3 — Overlay server-side — ✅ SHIPPED.** Each source moved to a shape-identical control command + a sync core (so the SAME code serves the in-process and socket paths) + a frontend `controlRequest` flip. `recent_sessions` (`eeeb8b0`), `claude_usage`/`codex_usage`/`git_info` (`c45b9fb`), `host_metrics` (`bdd71e3` — bridge-first, Linux-only local-`/proc` fallback so Windows never zeros), and the file **index** — `index_project` + `search_files` (`9dbbc39`). **Deliberately deferred to M4:** the file BROWSER/READER/EDITOR (`list_dir`/`read_text_file`/`write_text_file`) — arbitrary-path read+write over a network-bindable channel needs peer-gating/path-scoping first.
- **M4 — Multi-client + hardening — ☐ not started.**

**§8 pre-M1 decisions (settled, do not re-litigate):** (a) **shared-vs-per-client split** — server owns sessions/agents/status/scrollback/cost/supervision; client owns layout/focus/theme/keymap (those never cross the wire). (b) **No network bind until auth-beyond-loopback** — satisfied by M2b's `is_allowed_peer` tailnet gate + persistent key; the bind is **opt-in**, default stays loopback-only.

**Also this session (UI batch, pre-split):** tab strip removed (brand = tray icon, settings moved top-right); sidebar reshape (`+` in Workspaces header, expand-all default, last-active time, bigger buttons, portal'd color picker, collapsed-rail stats); a shared ring+center **status indicator** (working = spinner, done = true solid green `#22c55e`, idle = green ring) with a live legend in Settings; Win11 Snap-Layouts maximize-button rect tracking; **notifications + sounds now default OFF** (opt-in); titlebar × hides-to-tray (default) vs quits, as a setting.

**Verification (re-run green at the tip of `main`, `9dbbc39`):** `cargo build` clean · **190 Rust lib tests** pass · `tsc` clean · **53 vitest** pass. Each server-split commit was also verified **live** against the running app via control-socket probes, and reviewed by sub-agents (security-critical surfaces — peer gate, token compare, write bounding — all came back clean; every real finding was acted on).

**Push state:** all server-split + M3 commits through `9dbbc39` are on `origin/main`; this handoff's doc commit follows (the session has been pushing throughout).

---

## 3. Next steps (ordered)

**M3 is now complete** (all overlay/index reads on the socket). The remaining server-split **tail** is below. Read [`SERVER-SPLIT-AND-ROADMAP.md`](./SERVER-SPLIT-AND-ROADMAP.md) §6 (the M1→M4 detail + the per-milestone status table) before starting.

1. **File BROWSER/READER/EDITOR remoting (the deferred Files chunk — security-sensitive, M4-gated).** `index_project` + `search_files` are on the socket; `list_dir` / `read_text_file` / `write_text_file` (frontend `apps/desktop/src/ipc/files.ts`) are NOT — they read+WRITE **arbitrary paths**, which over the network-bindable (post-M2b) control channel is a real security expansion. There's already a `files::control_read_text` core ready, but DON'T just dispatch it: first add path-scoping (restrict to known project roots) and confirm the `is_allowed_peer` gate is the right boundary for filesystem writes. **Acceptance:** a thin client browses + opens + saves remote files; a peer cannot read/write outside the allowed roots. Until then the file panel's tree/reader/editor stays local (works fine on one machine).
2. **M2b deferred hardening** (task #18 — before trusting the bind beyond a single-user tailnet): per-client auth for `attach_pty` (today the PTY attach trusts the connection), a server read/idle timeout, protocol versioning, and reconnect re-sync. These are listed in the §6 M2 row.
3. **Real two-device Tailscale test + a `variant=dev` Windows build** (task #19): the M2b bind/gate path has only been exercised locally — drive a second physical device over the tailnet against a dev build. (This one needs the **user** — it's a two-machine test.)
4. **M4 — multi-client + hardening:** named-session namespacing, per-client view vs shared state, PTY resize-ownership arbitration, auth beyond the loopback token, split client/server logs.

**Older parked work (still valid, lower priority):** broader vitest coverage (component/RTL); the **heavy** tray actions (restart-tmux, full-WSL-shutdown — add only when needed); WS-9 nits (`sanitizeBranchToDir` collision + remote-branch `-b`-vs-DWIM — see [`WORKTREE-WORKFLOW.md`](./WORKTREE-WORKFLOW.md)); the parked differentiators (budget governor, worktree fleet launcher, MCP supervision event stream).

---

## 4. Conventions & gotchas (hard-won)

**Server-split migration pattern (use this for the M3 tail):**
- A migrated read = **(1)** a shape-identical command arm in `control.rs`'s `dispatch`, backed by **(2)** a **sync core** extracted from the existing Tauri command (so the in-process and socket paths share one implementation — see `recent::recent_sessions_cached`, `usage::claude_usage_blocking`, `git::git_info_cached` for the shape), then **(3)** flip the frontend `ipc/<x>.ts` wrapper from `invoke` to `controlRequest` (and `listen` → `onControlEvent` for events). The response JSON must be **byte-identical** to the old `invoke` result — it's a transport swap, not a redesign.
- **Verify each flip LIVE**, not just by build: socket probes against the running app (the build + tsc pass even when the wire is wrong). There are **no GUI-automation tools** in this env (xdotool/ydotool/wtype all absent) and the **MCP tools hit the prod instance + drive the control channel, not the webview attach path** — so to exercise a webview path, forward a real frontend action (e.g. via `create_worktree`) and watch the socket.
- **`control.rs` stays Tauri-free** (it's the server half); `control_client.rs` is the Tauri-aware client half (the `#[tauri::command]` + `AppHandle` event re-emit). They meet only at the NDJSON wire + the shared `EventFanout`.
- **Thin-client mode:** `T_HUB_REMOTE_ADDR` + `T_HUB_REMOTE_TOKEN` point the GUI's control_request/event-forwarder/RemotePty at a remote server instead of local loopback. Unset = local loopback (the default).

**Build / release**
- **Version lives in 3 files:** `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, `apps/desktop/src-tauri/Cargo.toml`. Bump all three, run `cargo build` to sync `Cargo.lock`, then commit.
- A pushed `v*` tag **or** `gh workflow run release.yml -f variant=prod` builds the **PROD** Windows installer + GitHub Release + `latest.json`. **CI builds from the REMOTE — push first.** `variant=dev` builds a side-by-side "T-Hub Dev" (`com.t-hub.dev`, isolated socket) that can't disturb live sessions. Variant model: [`DEV-BUILD.md`](./DEV-BUILD.md).

**Local dev run**
- `pnpm -C apps/desktop tauri dev` builds the **Linux/WSLg** variant (attaches tmux directly; **no `wsl.exe`, no WebView2**). Good for cross-platform logic; does NOT cover the Windows path.
- **Isolate a test instance** so it can't touch live sessions: `T_HUB_TMUX_SOCKET=t-hub-localtest T_HUB_CONTROL_FILE=~/.t-hub-localtest/control.json T_HUB_DIAG_FILE=~/.t-hub-localtest/diag.log`.
- An **inherited** `T_HUB_*` env var WINS over per-user resolution — a prod app launched from a polluted WSL shell silently runs on the wrong socket. The startup marker `t-hub: started vX (diag -> …)` reveals the resolved path; if unexpected, relaunch from a clean shell.

**Verify commands**
- Rust: `cd apps/desktop/src-tauri && cargo build` · `cargo test --lib` (190 tests).
- Frontend: `cd apps/desktop && pnpm typecheck` · `pnpm test` (53 vitest).

**Traps**
- **`tauri.conf.json` must NOT have a `plugins.notification` block** — even empty `{}` PANICS at startup ("invalid type: map, expected unit"). Passes `cargo build` + `tsc`, crashes only at runtime — **always run the app, not just build it.**
- **`host_metrics` / any `/proc` read over the control channel** — the daemon's local `/proc` is the **Windows host** (zeros) on the current in-GUI topology; the WSL agent bridge is the real source. `host_metrics` already handles this (bridge-first, Linux-only local fallback — see `control.rs::host_metrics`); apply the same bridge-first pattern to any future `/proc`-derived read, never a naive local read.
- **Never `pkill -f "<pattern>"` matching your own bash command** — kills your shell (exit 144). Use `pkill -x` or PIDs.
- **`apps/desktop/bin/claude`** is an untracked local symlink into `node_modules` (a claude-code install) — a dev artifact, **not ours to commit**. Leave it untracked.

**Commits**
- Conventional Commits. Trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Commit after each logical change; **push only when the user asks** (this session was actively pushing). `main` is pushed directly. **Subagent caveat:** subagents sometimes commit mid-batch despite instructions — verify commit boundaries didn't cross-contaminate.

---

## 5. File map (key entry points)

**Server-split — backend (`apps/desktop/src-tauri/src/`)**
- `control.rs` — the server half: NDJSON dispatch table, `EventFanout` (subscriber registry), `SUBSCRIBE_COMMAND` + `ATTACH_PTY_COMMAND`, `serve_pty_attach`, `persistent_key`/`key_path`, `resolve_remote_bind`/`tailscale_ip4`/`is_allowed_peer`, `MAX_CONNS`/`ConnGuard`, `ct_token_eq`. **The M3-tail dispatch arms go here.**
- `control_client.rs` — the Tauri-aware client half: `control_request` command, `SocketEmitter`/`TeeEmitter`, `spawn_event_forwarder` (backoff reconnect → re-emits `control://event`), `ControlEndpoint`, `install()` (honors `T_HUB_REMOTE_ADDR/TOKEN`).
- `remote_pty.rs` — `RemotePty`/`RemotePtyManager`: connect/handshake/reader thread re-emits `terminal://output|exit|state`; write/resize on a cloned stream; `fresh` set for fresh-spawn scrollback.
- `pty.rs` — `stream_attach_to_sink` + `PtyStreamHandle` (the socket-streaming attach); in-process variant kept `#[allow(dead_code)]` as the M2a revert path.
- `commands.rs` — terminal commands rewired onto `RemotePtyManager`; `tmux_target` delegates to `tmux::target_for_id`.
- `recent.rs` / `usage.rs` / `codex.rs` / `git.rs` — each has a **sync core** (`*_cached` / `*_blocking`) shared by the in-process and socket paths — **the template for migrating `host_metrics` + files.**

**Server-split — frontend (`apps/desktop/src/ipc/`)**
- `controlClient.ts` — `controlRequest()` + `onControlEvent()`: the frontend transport over the socket.
- `recent.ts` / `usage.ts` / `codex.ts` / `git.ts` — flipped to `controlRequest` (reference these for the pattern).
- `client05.ts` — `hostMetrics()` flipped (the rest of the 0.5 surface stays on `invoke`); `files.ts` — `indexProject`/`searchFiles` flipped, but `listDir`/`readTextFile`/`writeTextFile` still on `invoke` (the deferred M4-gated Files-remoting chunk).
- `controlBridge.ts` — the original one-way org-mutation bridge (the prototype M1 generalized).

**Status UI (`apps/desktop/src/components/`)** — `StatusIndicator.tsx` (variants + `sessionStatusToVariant`/`terminalVariant` helpers), `StatusBadge.tsx`, `WorkspacesList.tsx`, `Sidebar.tsx`, `Titlebar.tsx`, `ThemeEditor.tsx` (status legend + closeToTray).

**Roadmap / design docs (`docs/`)** — link, don't duplicate:
- [`SERVER-SPLIT-AND-ROADMAP.md`](./SERVER-SPLIT-AND-ROADMAP.md) — **read for the M3 tail / M4** (item ⑥; §6 M1→M4 + the status table; §8 decisions).
- [`ROADMAP-PLAN.md`](./ROADMAP-PLAN.md) — the herdr-parity wave + shipped status.
- [`PERF-AUDIT.md`](./PERF-AUDIT.md) · [`WORKTREE-WORKFLOW.md`](./WORKTREE-WORKFLOW.md) · [`SMOKE-TEST.md`](./SMOKE-TEST.md) · [`HERDR-PARITY.md`](./HERDR-PARITY.md) · [`DEV-BUILD.md`](./DEV-BUILD.md).
