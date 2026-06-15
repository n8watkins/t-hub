# T-Hub — Session Handoff

**Last updated:** 2026-06-15 · **Branch:** `main` (clean, pushed to `origin/main`) · **App version:** `0.1.52`

> Read this whole file first, plus `PRD.md` and `README.md`. Many decisions below
> are already made — **do not re-ask the user anything answered here.**

---

## 0. The one thing to know

This session ran **v0.1.47 → v0.1.52**, a long live-iteration burst driven by
**parallel Opus subagents on git worktrees** (the user's preferred workflow — see
§5). It reworked the **Recent list**, **Settings → Hooks**, the **Files** panel,
**Preview**, **tile chrome**, and added a **per-tile context meter** with a robust
tmux binding. All shipped + on `main`.

**OPEN / UNVERIFIED — START HERE (§4.1):** at the very end the user reported
**"can't type in the terminal"** on v0.1.52. I restarted the app on the existing
build (tmux sessions preserved) — likely a transient focus glitch. The
`Terminal.tsx` custom-key handler was inspected and is correct (plain keys
`return true` → typed normally). **If it persists**, the prime suspect is the
**split-ratio layout/z-index in `Tile.tsx` (commit `9eb5288`)**; revert that in
isolation and rebuild. **Ask the user whether typing works before building
anything else.**

**The single most valuable debugging tool:** the running Windows app writes a diag
log readable from WSL at **`/mnt/c/Users/natha/.termhub/diag.log`** (F12 devtools
also enabled in release). Clear it before a deploy with
`: > /mnt/c/Users/natha/.termhub/diag.log`.

---

## 1. What this is

**T-Hub** (codebase/repo identifiers stay `termhub`) — a local cockpit for running
& supervising many persistent Claude Code + shell sessions on **Windows 11 + WSL2
(Ubuntu-24.04) + zsh**. Tauri 2 (Rust, frameless WebView2) + React/TS/Tailwind +
xterm.js, over a `portable-pty` (ConPTY) → `wsl.exe` → **`tmux -L termhub`** spine.
Every xterm renders once into a persistent overlay pool (`TerminalPool.tsx`)
positioned over per-tile placeholders, so moving tiles never reloads a terminal.
TermHub names each terminal's tmux session **`th_<terminalId>`** (see `tmux.rs`/
`pty.rs`) — this id is now also how the context meter binds a tile to its session.

- **GitHub:** `github.com/n8watkins/t-hub` (PUBLIC, renamed from `termhub`).
- **WSL repo (edit + commit here):** `/home/natkins/n8builds/tools` (the local
  folder stays `tools`; the repo *is* the t-hub monorepo). `main` is integration.
- **Monorepo layout:** the Tauri app is at **`apps/desktop/`**; the marketing
  site at `apps/site/` (Next.js, npm, NOT in the pnpm workspace). The pnpm
  workspace manages only `apps/desktop`. Root `package.json` name = `t-hub`.
- **Windows build mirror (NEVER edit):** `C:\Users\natha\termhub` — deploy
  `git reset --hard`s it to `origin/main`.
- **Desktop shortcut:** `…\OneDrive\Desktop\T-Hub.lnk` now points at the current
  exe (`C:\Users\natha\termhub\apps\desktop\src-tauri\target\release\termhub.exe`).

## 2. Build / verify / deploy / DEBUG

You **cannot** run the Windows GUI; only the user sees it. Verify in WSL, deploy
to let the user see it.

**Verify (WSL):**
```
pnpm --filter termhub typecheck                       # frontend (run from repo root)
cd apps/desktop/src-tauri && cargo check -p termhub   # backend (Linux target only)
```
`cargo check` on Linux does NOT compile `#[cfg(windows)]` code (the `wsl.exe`
paths) — the Windows build at deploy is the real cross-check. Two lib tests
(`control::…live_send_read_close_roundtrip`, `tmux::…send_text_then_capture…`) are
**pre-existing environmental flakes** in this WSL worktree — ignore them.

**Deploy (so the user sees a change):**
```
cd apps/desktop && ./scripts/bump-version.sh          # STANDING RULE: bump EVERY deploy
( cd src-tauri && cargo check -p termhub )            # sync Cargo.lock to the bump
cd /home/natkins/n8builds/tools && git add -A && git commit ...   # ASCII-only msg + Co-Authored-By trailer
git push origin main
: > /mnt/c/Users/natha/.termhub/diag.log              # optional: clean diag read
powershell.exe -NoProfile -ExecutionPolicy Bypass -File 'C:\Users\natha\Downloads\termhub_relaunch.ps1'
```
Run the relaunch `run_in_background: true`; filter its noise with
`grep -vE 'RemoteException|CategoryInfo|FullyQualified|At C:|^\s*\+|NotSpecified'`.
Success prints `BUILD_EXIT=0` + `LAUNCHED ...`. It kills `termhub.exe` first
(close-to-tray leaves a stale instance), hard-syncs the mirror to `origin/main`,
`pnpm install`, **`cd apps\desktop` + `pnpm tauri build`**, launches. Do **NOT**
`tmux -L termhub kill-server` (destroys the user's running Claude sessions).

**Quick app restart (no rebuild):**
`powershell.exe -NoProfile -Command 'Stop-Process -Name termhub -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 1200; Start-Process -FilePath "C:\Users\natha\termhub\apps\desktop\src-tauri\target\release\termhub.exe"'`
(tmux sessions survive). Note the SINGLE outer quotes — see §5 (zsh `$` trap).

**Updater signing:** `createUpdaterArtifacts: true`, so the build MUST sign.
relaunch.ps1 loads the minisign key from `%USERPROFILE%\.tauri\termhub-updater.key`
(+ `.password`). Keys also in WSL `~/.tauri/`; GitHub repo secrets set for CI. Real
releases: tag `vX.Y.Z` + push → `.github/workflows/release.yml` signs/publishes.

**`termhub-agent` is NOT built by the deploy** (it's the WSL-side hook +
`--statusline` binary). When `crates/termhub-agent/**` changes, rebuild + reinstall
manually (the app deploy only builds the GUI):
```
cd apps/desktop/src-tauri && cargo build -p termhub-agent --release
cp target/release/termhub-agent ~/.local/bin/termhub-agent.new && mv -f ~/.local/bin/termhub-agent.new ~/.local/bin/termhub-agent
```
(mv-over because the file is busy while the app runs). The statusLine/hook *command
path* is unchanged, so NO hook reinstall is needed — the next tick uses the new
binary. Done this session for the context-meter `$TMUX_PANE` change.

## 3. State — DONE this session (all on `main`, pushed, deployed)

Releases, newest first (the merge commits per feature are in `git log`):

- **v0.1.52** (`78749d6`):
  - **Files — `.env` visible** (`2e0f91f` backend, `18844ce` frontend): the file
    TREE now applies `.gitignore` to **directories only**, so ignored *files*
    (`.env`, `.env.*`) always show while ignored *dirs* (node_modules/dist/build)
    stay hidden. Added a **"Show ignored"** header toggle (localStorage
    `termhub.files.showIgnored`). Replaced the weak inline SVGs with **lucide-react**
    file-type icons (new dep).
  - **Robust context-meter binding** (`8cc6280`): replaced the fragile cwd match
    with a **tmux-session** match — the statusline hook reads `$TMUX_PANE` →
    `#{session_name}`; a tile looks itself up by `th_${terminalId}` (cwd kept as
    fallback). Required the **termhub-agent rebuild** (done, see §2).
  - **Draggable split ratio** (`9eb5288`): drag the terminal|panel divider; clamped
    0.25–0.75, persisted per tile (localStorage `termhub.panels.splitRatio.v1`).
    ⚠️ Suspect for the §4.1 "can't type" report.
- **v0.1.51** (`db06f2d`): **Hooks "View raw JSON"** button (`36ccd4d`). **Tile
  chrome** (`fdb97a5`): PageUp/PageDown scroll the xterm viewport; session id
  removed from the tile header; tab bar centered. **Files** (`1b37bb0`): tree
  respects `.gitignore` + folder/file-type icons. **Preview** (`4fba34e`): fixed
  the WSL2 mirrored-networking connection bug (bind `0.0.0.0` + rewrite
  `localhost`→reachable WSL IP + TCP probe), unbounded multiple previews, pop-out
  preview as a top-level Tauri `WebviewWindow`.
- **v0.1.50** (`b4aa3e3`): **Settings → Hooks** reorganized into outcome-category
  cards (Attention / Sessions / Supervision / Worktrees) with the
  **notification-sound toggle surfaced in the Attention card**; install/consent/
  Apply/Uninstall unchanged. First per-tile context meter (cwd-based — superseded
  by 0.1.52's robust binding).
- **v0.1.49** (`cc08e87`): **Recent row redesign** — one row per project: folder
  name over the session's most-recent message text (read from the transcript
  TAIL); hover-revealed **→** resume and **×** hide; Resume button + session
  dropdown removed. **× = hide** (localStorage `th.recent.hidden.v1`; does NOT
  delete the transcript; the project resurfaces on a newer session).
- **v0.1.48** (`6d2d4e9`): **Recent capped by PROJECT, not raw session.** It kept
  the newest 150 *sessions* then grouped, so one chatty folder (plain `claude` from
  `$HOME` = 143 of the user's newest 150) starved out every other project. Now
  buckets per project folder, keeps the newest N projects (`PER_PROJECT_LIMIT=1`
  since 0.1.49 — one row per project).

**Parallel orchestration ran twice** (v0.1.50/.51 = 4 agents, v0.1.52 = 2 agents):
manual worktrees, disjoint file ownership, **zero merge conflicts**.

## 4. NEXT STEPS (ordered)

1. **VERIFY the "can't type in terminal" report (v0.1.52).** I restarted the app
   (no rebuild). If still broken: `Terminal.tsx`'s custom-key handler is fine
   (returns `true` for plain keys), so suspect the **split-ratio layout/z-index**
   in `Tile.tsx` (`9eb5288`) — the divider is `relative z-10`, the centered tab bar
   is `absolute z-10`; verify nothing overlays/steals focus from the pooled xterm.
   Fastest safe fix: `git revert 9eb5288` + rebuild, then reintroduce the split
   more carefully. **Confirm with the user first.**
2. **Clickable links in the terminal** (user request, 2026-06-15). Add
   `@xterm/addon-web-links` to the xterm in `Terminal.tsx` (the app already uses
   `@xterm/addon-{fit,search,webgl,unicode11}`); open matched URLs externally via
   the Tauri shell plugin (`@tauri-apps/plugin-shell` is already a dep — used by
   the preview "Open externally"). Acceptance: a URL printed in a terminal is
   clickable and opens in the default browser.
3. **Claude's localhost URLs → Preview** (user request, 2026-06-15). When a
   `localhost:<port>` / `127.0.0.1:<port>` URL appears in terminal output, surface
   it as an available Preview target. Preview already knows how to make a WSL URL
   reachable (`reachablePreviewUrl()` in `ipc/devserver.ts`) and `devserver.rs`
   already detects a managed dev server's URL — the gap is DETECTING ad-hoc
   localhost URLs from arbitrary terminal output (scan PTY output per terminal,
   reuse the web-links URL matcher from #2), then list them in the Preview tab /
   `store/preview.ts` so the user can open one. Pairs naturally with #2.
4. **Confirm the context meter shows.** Robust binding deployed + agent rebuilt; it
   appears on a tile running an active Claude session (its `NN%` from the
   statusline). Data flows (diag showed sessions at 16–68% ctx); just needs on-
   screen confirmation.
5. **Kill-confirm: detect a live Claude turn — NOW UNLOCKED** by the tmux↔session
   binding (`store/sessionContext.ts` exposes `sessionNameForTerminal`/
   `th_<terminalId>`). `Tile.tsx` `busy` only checks a running dev server today;
   fold in the session's `working`/`needsQuestion` status so killing a mid-turn
   tile warns.
6. **`filesRootDir` setting** — user wants Files to start at a configurable root
   (default `/home/natkins`). Stubbed in `store/settings.ts` (DEFAULTS only); wire
   through `PersistedSettings`/`loadPersisted`/`persistAll`/`SettingsState`, add a
   "Files" group input in `ThemeEditor.tsx`, have `Tile.tsx` pass
   `root = filesRootDir.trim() || cwd` to `TilePanel`.
7. **Visual pass** — user wants the sidebar/chrome "bigger, rounder, more breathing
   room." Partial.
8. **Desktop notification toasts** — sounds work; OS toasts need
   `@tauri-apps/plugin-notification` wired (`lib/notify.ts` already tries it).
9. **Recent load latency** — `recent.rs` reads transcripts over the UNC share, now
   a 32KB PREFIX *and* a 32KB SUFFIX per surviving row. Could go lazy-per-folder.

## 5. Conventions & gotchas (hard-won)

- **`wsl.exe` from the GUI process mangles a trailing bash arg** — `bash -lc
  <script> termhub <path>` (`$1`) arrives EMPTY in the GUI-spawned process (works
  from an interactive WSL shell — do NOT test it that way). Use `wsl.exe --cd
  <path>` or read over the `\\wsl.localhost\<distro>\` UNC share with std::fs.
- **`zsh -lc` skips `~/.zshrc`** (login but non-interactive), where the user's PATH
  (`~/.npm-global/bin` → `claude`) lives. Use **`$SHELL -ilc`** for any command
  needing the user's tools (why Resume + `/usage` use it).
- **zsh expands `$vars` inside a WSL→`powershell.exe -Command "…"` DOUBLE-quoted
  string** before PowerShell sees them (broke the shortcut command — `+ $p` →
  `+ ` → parse error). Use **SINGLE** quotes for the outer `-Command '…'` (and
  PowerShell double-quotes for strings inside) so zsh passes it literally.
- **`claude -p /usage` is TTY-dependent** — piped it prints only an intro line; run
  it under `script -qec '<cmd>' /dev/null`. It's FREE (0 tokens).
- **The Agent tool's `isolation:"worktree"` is BROKEN here** ("WorktreeCreate hook
  … no worktree path"). For parallel batches: create worktrees MANUALLY
  (`git worktree add -b feat/<x> /home/natkins/n8builds/th-<x> main`), launch
  BACKGROUND agents with the ABSOLUTE worktree path baked into each prompt (every
  file path under it; run `CI=1 pnpm install` at the worktree root first — fast,
  pnpm store shared; a fresh worktree recompiles Rust). Strict **disjoint file
  ownership**; shared hot-spots = `lib.rs`, `panels.ts`, `Tile.tsx`, `App.tsx`,
  `index.css`. Merge sequentially (`git merge --no-ff`, conflict-free when
  disjoint), verify the merged tree, bump+deploy. Clean up:
  `git worktree remove --force` + `git branch -d`.
- **WSL2 preview networking (mirrored mode):** dev servers default to `127.0.0.1`
  = WSL loopback, unreachable from the Windows WebView. Shipped fix
  (`devserver.rs`/`ipc/devserver.ts`/`WebPreview.tsx`): export `HOST=0.0.0.0`, and
  `reachablePreviewUrl()` rewrites `localhost`/`127.0.0.1`/`0.0.0.0` → the WSL
  interface IP (`preview_host`). Pop-out windows load top-level (no iframe X-Frame
  limits); labels `th-pop-preview-*` match the existing `th-pop-*` capability glob.
- **Claude transcript data** lives in WSL `~/.claude/projects/<enc>/<id>.jsonl`
  (one per session). Claude auto-prunes at **`cleanupPeriodDays` = 30** (default,
  user hasn't overridden) — that's why ~800 MB / ~1150 files only spans ~3 weeks.
  T-Hub only READS them (32KB prefix + suffix); T-Hub's own data is ~7.5 MB.
- **React effect lesson:** a tree load effect must NOT depend on the state it sets
  (deps `[open, path]` only) or it cancels its own in-flight fetch → stuck
  "loading…". The Files "Show ignored" toggle is part of the tree's React `key`, so
  flipping it remounts (keeps deps constant per mount).
- **Commits:** ASCII-only messages (no em-dashes — they break the `.ps1`). End with
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Bump version EVERY
  deploy. **`pnpm install`** sometimes needs `--no-frozen-lockfile` (after a dep
  add) or `CI=1` (to auto-confirm the modules-purge prompt).
- **Feature knobs added this session:** Recent `PER_PROJECT_LIMIT=1`, hidden set
  `th.recent.hidden.v1`. Files `termhub.files.showIgnored`. Split
  `termhub.panels.splitRatio.v1`. Context meter: `StatusSnapshot.tmux_pane/
  tmux_session` + `sessionContext.ts` `bySession` (`th_<id>`) with `byCwd` fallback.
  `lucide-react` is now a dependency.

## 6. File map (for the next steps)

- `apps/desktop/src/components/Tile.tsx` — tile chrome + tab bar (centered) + ⤢/× ;
  body renders terminal placeholder / `PanelPane`; **draggable split** (divider,
  `splitRatio`); renders `<ContextMeter>`. Prime suspect for §4.1.
- `apps/desktop/src/components/Terminal.tsx` — xterm wrapper;
  `attachCustomKeyEventHandler` (PageUp/Down scroll + Ctrl+C/V + zoom). §4.1.
- `apps/desktop/src/components/TerminalPool.tsx` — the xterm overlay pool;
  `shouldShow` gates visibility (placeholder rect / active tab).
- `apps/desktop/src/components/RecentList.tsx` + `ipc/recent.ts` +
  `src-tauri/src/recent.rs` — Recent (per-project rows, hover →/×, hide set;
  per-project cap; prefix+suffix transcript reads).
- `apps/desktop/src/components/ContextMeter.tsx` + `store/sessionContext.ts` —
  per-tile context % keyed by tmux session (`th_<id>`)/cwd, fed by
  `status://snapshot`.
- `apps/desktop/src-tauri/src/claude/status.rs` — `StatusSnapshot` (carries
  `tmux_pane`/`tmux_session`); `crates/termhub-agent/src/hook.rs` — `--statusline`
  ingest (stamps `$TMUX_PANE`→session); `ipc/client05.ts` — `StatusSnapshotWire`.
- `apps/desktop/src/components/HookInstallPanel.tsx` — Hooks category cards + "View
  raw JSON"; `components/ThemeEditor.tsx` — Settings shell + Hooks section +
  General→Hooks pointer; `src-tauri/src/claude/hooks.rs` — hook/statusLine fragment.
- `apps/desktop/src/components/{WebPreview,PreviewOverlay,DevTab}.tsx` +
  `store/preview.ts` + `src-tauri/src/devserver.rs` + `ipc/devserver.ts` — preview
  (WSL-reachable URL, multiple, pop-out window).
- `apps/desktop/src/components/{FilePanel,FileTree}.tsx` + `src-tauri/src/files.rs`
  — gitignore-aware tree (directories only → `.env` shows) + lucide icons +
  Show-ignored toggle.
- `apps/desktop/src/store/{panels,workspace,settings,supervision,theme}.ts`,
  `src-tauri/src/lib.rs` (command + plugin registration).
- `.github/workflows/release.yml` — signed release pipeline (tag `vX.Y.Z`).
