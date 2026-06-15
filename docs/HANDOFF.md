# TermHub — Session Handoff

**Last updated:** 2026-06-14 · **Branch:** `main` @ `5fb33c2` (clean, pushed to `origin/main`) · **App version:** `0.1.5`

> Read this whole file first, plus [PLAN.md](./PLAN.md), [MCP.md](./MCP.md),
> [SESSION_AWARENESS.md](./SESSION_AWARENESS.md), and the repo `README.md`.
> Do **not** re-ask the user anything answered here — many decisions below are
> already made.

---

## 0. The one thing to do next

The shell, terminals, theming, tabs, drag, multi-window, MCP, SQLite, hooks,
file tree, lifecycle controls, and the persistent-pool muted bug are all **done
and deployed** (v0.1.5). What remains is a short punch-list of UX fixes from the
user's live testing (§5). Start with **#1: clicking a session/terminal in the
sidebar should switch to and focus that terminal** — today the Sessions-list
click calls `setSelectedSession`, whose state is **never read**, so it does
nothing. The user expects it to reveal the terminal.

The single most important tool you now have: **a file-based diagnostic log the
running Windows app writes to, which you can read from WSL** (§2). Use it
instead of guessing — it is how every hard bug this session got fixed.

---

## 1. What this is

**TermHub** — a terminal-first cockpit for running/supervising many persistent
Claude Code sessions. Target: **Windows 11 + WSL2 (Ubuntu-24.04) + zsh**. Tauri
2 (Rust, frameless WebView2) + React/TS/Tailwind + xterm.js, with a
`portable-pty` (ConPTY) → `wsl.exe` → **`tmux -L termhub`** spine. Every xterm is
rendered once into a persistent overlay pool (`TerminalPool.tsx`) positioned
over placeholder cells, so moving tiles never reloads a terminal.

- **GitHub:** `github.com/n8watkins/t-hub` (private; `gh` authed in WSL as `n8watkins`).
- **WSL repo (edit + commit here):** `/home/natkins/n8builds/tools` (ext4). `main` is the integration branch.
- **Windows build mirror (NEVER edit):** `C:\Users\natha\termhub` — the deploy script `git reset --hard`s it to `origin/main`.
- **Env:** WSL user `natkins`, Windows user `natha`. `/mnt/c` ↔ `C:\`.
- Spec: `PRD.md`; technical review: `REVIEW.md`; forward plan: `docs/PLAN.md`.

## 2. Build / verify / deploy / DEBUG (CRITICAL)

The app is a **Windows** binary; it **cannot run in WSL**, and **you cannot
launch the GUI** — the user is the only one who sees it.

**Verify code (in WSL):**
```
npx tsc --noEmit                              # frontend (run from repo root; node_modules is symlinked from th-shell3 if missing)
cd src-tauri && cargo check -p termhub        # backend — Linux target only
```
> `cargo check` on Linux does NOT compile `#[cfg(windows)]` code (the `wsl.exe`
> path translation in `files.rs`, the WSL-home resolver in `claude/install.rs`,
> `win_snap.rs`, etc.). Those are only truly compiled by the **Windows build at
> deploy**. Keep windows-gated code self-contained and correct; the deploy is
> the real cross-check.

**Deploy (so the user sees a change):**
```
./scripts/bump-version.sh                     # STANDING RULE: bump version EVERY deploy (patch). Prints new version.
cd src-tauri && cargo check -p termhub        # sync Cargo.lock to the bumped version
git add -A && git commit ...                  # ASCII-only message + Co-Authored-By trailer (§7)
git push origin main
: > /mnt/c/Users/natha/.termhub/diag.log       # OPTIONAL: clear the diag log for a clean post-deploy read
powershell.exe -NoProfile -ExecutionPolicy Bypass -File 'C:\Users\natha\Downloads\termhub_relaunch.ps1'
```
The relaunch script (run it `run_in_background: true`; it takes minutes):
`Stop-Process termhub` → `cd %USERPROFILE%\termhub` → `git fetch + reset --hard
origin/main` → `pnpm install` → `pnpm tauri build --bundles nsis` → launches the
exe. Prints `BUILD_EXIT=0` + `LAUNCHED ...` on success (grep `/tmp/...out` for
those). **It kills `termhub.exe` itself** (close-to-tray keeps a stale instance
alive — this is why the kill matters). **Do NOT `tmux -L termhub kill-server`** —
that destroys the user's running Claude sessions; the default deploy must
preserve them.

**DEBUG (the key capability — you can read the running app's logs):**
- The app appends runtime logs to **`C:\Users\natha\.termhub\diag.log`**, which
  you read from WSL at **`/mnt/c/Users/natha/.termhub/diag.log`**.
- `tlog(tag, ...)` in `src/lib/diag.ts` writes compact JSON lines; `console.warn`/
  `error` + window `error`/`unhandledrejection` are mirrored in. Backend command
  `diag_log`/`diag_clear` in `src-tauri/src/diag.rs`.
- **F12 devtools are enabled in the release build** (the `devtools` feature on
  the `tauri` dep in `src-tauri/Cargo.toml`).
- Tags in use: `pool` (every show/park decision — id, rect, SHOW/PARK + reason,
  activeTab), `files` (every `list_dir` call + OK count / ERROR), `attach`
  (subscribe/attach/flush/teardown), `focus`, plus `error`/`warn`.
- Useful greps: `grep -E 'PARK [0-9a-f]+ \(active\)'` = the muted-bug regression
  signature (must be **0**); note `(inactive)` and `activeTab=` both contain the
  substring "active", so a naive `grep active` false-matches.

## 3. State — DONE this session (all merged to `main`, deployed, version-tagged)

Built as **two waves of parallel Opus agents on disjoint git worktrees** (the
orchestrator merges + verifies + deploys), then several hotfix deploys.

**Wave 1 (`ee5e097`, ~v0.1.0):** native-speed file tree; **the muted/blank-pool
bug REAL fix** (active-tab terminals are never parked + header-click now triggers
a pool re-sync — root cause was `setFocus` changing only `focusedId`, which the
pool layout-effect didn't depend on); the diagnostic logger + F12; friendly
session labels; spawn presets (Claude/Shell/Resume/Custom); notification sounds;
6 new MCP tools (`read_terminal`/`capture_pane`, `send_text`, `send_keys`,
`close_terminal`, `new_tab`, `focus_tab`).

**Wave 2 (`4fee9e9`, v0.1.2):** hooks install to the real **WSL** `~/.claude`;
Claude-derived terminal titles; file/web **preview overlay**; **recovery-review**
screen (SQLite snapshot history); **terminal lifecycle** (X = detach/keep
session, trash = confirmed delete, `Ctrl+Shift+W` = fast-delete); **startup-race
fix** (subscribe before attach + one shared `EventHub` per channel — killed a
16k-line orphaned-callback storm, verified 0 after); muted-flicker re-arm;
Win11 max/restore toggle.

**Hotfixes:**
- `dea410a` **v0.1.3** — hooks resolve the real absolute `termhub-agent` path
  (`~/.local/bin`, via login-shell `command -v`); file-tree `tlog` instrumentation.
- `0e2b53f` **v0.1.4** — file tree was rendering at **0px height** (only `grow`
  section, squeezed by the tall default-open Hooks panel); gave it `min-h-[180px]`
  and collapsed Hooks by default. `list_dir` itself was already returning 16
  entries — the data was fine, there was no room to draw it.
- `5fb33c2` **v0.1.5** — tmux **`mouse on`** (global) so the wheel scrolls inside
  Claude/full-screen apps instead of sending arrows (selection now = Shift+drag);
  `tmux::pane_info()` reads per-session foreground command + live cwd, so
  `list_terminals` labels tiles `claude · tools` / `zsh · …` and the Files tree
  follows the focused terminal's real cwd.

**Verified working** (user-confirmed or log-confirmed): muted bug gone (move-tile
→ click no longer blanks; `PARK (active)` = 0); cold-start no longer freezes
(0 callback storms, typing works); file tree shows 16 entries / has height;
hooks install to WSL and Sessions populate; `mouse on` is set (`show-options -g
mouse → on`); label data flows from tmux.

**Live config touched (with backup):** `~/.claude/settings.json` had 15 hook
entries pointing at a stale `/usr/bin/termhub-agent`; corrected to
`/home/natkins/.local/bin/termhub-agent` (backup at `…settings.json.termhub-bak`).

## 4. State — IN FLIGHT / known cosmetic, NOT yet fixed

- **Startup content flash** ("muted bug back"): on launch the terminals are
  positioned correctly (pool log clean) but xterm hasn't painted the seeded
  scrollback for ~a moment, then it does. Self-healing, cosmetic. The pool's
  first-paint guard handles position, not content paint.
- **Win11 Snap-Layouts hover flyout:** max/restore *toggles* now, but hovering
  the maximize button does NOT show the Windows 11 snap-arrangement flyout. The
  click is intercepted in `win_snap.rs` (HTMAXBUTTON → `WM_SYSCOMMAND`); the
  DWM hover path for the flyout still needs work.
- **Label accuracy:** labels now derive from `pane_current_command` + cwd
  (accurate); the earlier Claude-prompt-derived titles (`agent://title`, matched
  by cwd) can be ambiguous when terminals share a cwd.

## 5. NEXT STEPS (the user's open punch-list, ordered)

1. **Click a session/terminal in the sidebar → switch to + focus that terminal.**
   Today `onSelectSession` → `App.tsx` `setSelectedSession`, and `selectedSession`
   is **never read** (dead state) — so nothing happens. Wire it to find the tab
   containing that terminal and call `setActiveTab` + `setFocus`. Note the
   "Sessions" accordion = Claude **supervision** nodes (session ids from hooks,
   correlated to a terminal by cwd); the user's **terminals** are under
   "Workspaces" (`TerminalRow`). Decide which the click should target (likely:
   make BOTH the Workspaces terminal rows and the Sessions rows reveal/focus the
   matching tile). Acceptance: clicking a row switches to its tab and lights up
   that tile.
2. **Startup content-flash polish** — force an xterm repaint the instant the pool
   places each terminal on first paint (`TerminalPool.tsx` fires `th-pool-moved`;
   `Terminal.tsx` listens). Acceptance: no visible blank-then-fill on cold start.
3. **Win11 Snap-Layouts hover flyout** — make hovering the maximize button show
   the snap flyout (`win_snap.rs` DWM hover handling). Windows-only; verify via
   the deploy build. Acceptance: hover shows the snap grid.
4. **Label accuracy** — confirm `claude · tools` / `zsh · …` reads right after
   0.1.5; refine `deriveLabel` (`store/workspace.ts`) / the cwd correlation if
   the user reports wrong tiles labeled.
5. **Notification sounds** are now firing (hooks work). User can mute in
   Settings → General → Notifications; soften/tune if they ask.
6. **Desktop notification toasts** — sounds work; the OS toast needs the Tauri
   notification plugin added (`@tauri-apps/plugin-notification` + Rust
   `tauri-plugin-notification` init + `notification:default` capability).

## 5b. Questions asked this session → the user's answers (DECIDED — do NOT re-ask)

- **Q: Scroll — flip tmux `mouse on` so the wheel scrolls in Claude (trade-off:
  text selection then needs Shift+drag)?** → **YES, do it.** ("I gotta be able to
  scroll in a terminal session with Claude.") Shipped in v0.1.5; Shift+drag for
  selection is accepted.
- **Q: When you click the maximize/restore button, what happens — (a) nothing,
  (b) maximizes but covers taskbar, (c) maximizes but won't restore?** → "It works
  but only basically; **on hover it should display all the different ways it could
  be formatted**." = the toggle works; the real ask is the **Windows 11
  Snap-Layouts hover flyout** (§4, §5.3).
- **Q: What does the muted bug look like, and what triggers it?** → "Inside the
  terminals, just like before." First answered "on startup, then settles," then
  found the trigger is **clicking a Session row**. BUT the pool log shows
  `PARK (active)` = 0 and the session-click handler is dead state — so the old
  pool muted bug is genuinely gone; what's left is the transient **startup
  content paint** (§4) that coincided. Do not chase a pool regression.
- **Q (implied): should clicking a session do something?** → "I figured clicking
  the session would open that Claude Code instance in the terminal… it doesn't,
  but it should, or there should be a way to do that." → that's §5.1.
- **Icon:** the user pasted a microphone "scribe" PNG, then said **ignore it** —
  it was the wrong asset. No final icon yet; leave the Tauri default (§6).

## 6. Conventions & gotchas (hard-won)

- **Commits:** ASCII-only messages (no em-dashes — they break PowerShell/`.ps1`
  parsing). End every commit with
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Commit after each
  logical change.
- **Version:** run `./scripts/bump-version.sh` before EVERY deploy (bumps patch
  across `package.json`, `tauri.conf.json`, `Cargo.toml`; About in Settings shows
  it via `getVersion()`). The user explicitly wants a fresh version each update.
- **Parallel-agent orchestration (the user STRONGLY prefers this for batches):**
  one git worktree + branch per agent off `main`
  (`git worktree add -b feat/x /home/natkins/n8builds/th-x main`), symlink
  `node_modules` from `th-shell3`, strict **disjoint file ownership**, agents run
  `npx tsc --noEmit` but NOT cargo (orchestrator compiles Rust once at
  integration), agents commit on their branch. Merge sequentially; the only
  recurring conflict hotspots are `workspace.ts` (keep label-region vs
  actions-region edits separate), `Sidebar.tsx` (Files-section vs
  Workspaces/Sessions regions), and `lib.rs`/`Cargo.toml` (command registration /
  feature flags — orchestrator reconciles). Clean up worktrees after merge
  (`git worktree remove --force` + `git branch -D`).
- **tmux `#{...}` format gotcha:** over `wsl.exe`, a bare `#{...}` argv word is
  eaten as a shell comment (this broke `list-sessions -F '#{session_name}'` →
  "-F expects an argument", which silently broke the whole live terminal list /
  cwd / labels). Fixes: either avoid `-F` (parse default output, as
  `list_sessions` now does) OR wrap the tmux call in `bash -lc '<script with the
  format SINGLE-QUOTED>'` (as `pane_info()` does — single quotes make `#`
  literal). See `src-tauri/src/tmux.rs`.
- **WSL path translation:** file commands run Windows-side; `files.rs`
  `to_host_path` maps `/home/...` → `\\wsl.localhost\<distro>\...` UNC.
  `normalize()` must NOT `canonicalize()` a WSL UNC path (it rewrites it to the
  `\\?\UNC\` verbatim form that `unc_to_posix` didn't recognize → silently fell
  back to the slow std::fs UNC read). The fast path shells `list_dir`/index
  natively inside WSL (`find` / `rg`).
- **tmux is preserved across deploys.** Do not kill the server. New terminals
  open in `~`. `mouse on` is global now (selection = Shift+drag).
- **Frameless window:** `tauri.conf.json` `decorations:false`; window perms in
  `capabilities/default.json` (incl. `core:window:allow-toggle-maximize`).
- **Icon:** the user has NOT supplied a final app icon (they said to ignore the
  microphone "scribe" icon). Taskbar icon is still the Tauri default. Do not
  swap it until they hand over a real asset.
- **Memory:** the orchestrator keeps auto-memory at
  `~/.claude/projects/-home-natkins-n8builds-tools/memory/` (deploy flow, diag
  log, parallel-agent pattern).

## 7. File map (current, for the next steps)

- `src/App.tsx` — shell root; `onSelectSession={setSelectedSession}` (the dead
  wiring to fix in §5.1); mounts `LifecycleKeybinds`.
- `src/components/Sidebar.tsx` — accordion (Workspaces / Files / Sessions /
  Hooks); `filesRootFor()` (Files root follows focused terminal cwd); Files
  section has `min-h-[180px]`; Hooks `defaultOpen={false}`.
- `src/components/TerminalPool.tsx` — the pool; `sync()`; active-never-parked
  invariant + first-paint re-arm; `tlog('pool', …)`.
- `src/components/Terminal.tsx` — xterm wrapper; subscribe-before-attach +
  buffer/flush; `th-pool-moved`/IntersectionObserver repaint. Integrator-owned.
- `src/ipc/client.ts` — the shared `EventHub` (one listener per channel,
  fans out); `writeTerminal` (input), attach/seed.
- `src/store/workspace.ts` — tabs/order/focus, `labels`/`deriveLabel`,
  `detachTile`/`deleteTerminal`, `agent://title` subscription, SQLite mirror.
- `src/components/{SpawnMenu,ConfirmDialog,PreviewOverlay,WebPreview,RecoveryReview,HookInstallPanel,FileTree,FilePanel,Tile,Titlebar}.tsx`.
- `src/lib/{diag,notify,notifyMount}.ts`, `src/lib/useLifecycleKeybinds.tsx`.
- `src-tauri/src/tmux.rs` — `list_sessions` (no `-F`), `pane_info()` (foreground
  cmd + cwd), `new_session` (mouse on), capture/send helpers.
- `src-tauri/src/commands.rs` — `list_terminals` (uses `pane_info` for title+cwd),
  spawn/attach.
- `src-tauri/src/{diag,db,control,win_snap,files}.rs`, `claude/install.rs`
  (WSL settings path + `resolve_agent_bin`), `commands_05.rs`, `agent/`,
  `crates/{termhub-protocol,termhub-agent,termhub-mcp}`.

## 8. Worktrees

`main` is the integration branch (this session worked directly on `main` for
hotfixes and merged agent branches into it). Stale leftover worktrees from older
sessions may linger (`feat/files`, `feat/mcp`, `feat/shell-v2`, `feat/tabs`,
`feat/theming`, `feat/session-awareness`, `feat/0.5-personal-alpha`,
`feat/shell-v3`) — removable with `git worktree remove`. This session's wave
worktrees were already cleaned up.
