# T-Hub — Session Handoff

**Last updated:** 2026-06-15 · **Branch:** `main` (clean, pushed to `origin/main`) · **App version:** `0.1.47`

> Read this whole file first, plus `docs/AUDIT.md` (strategic audit), `PRD.md`,
> and `README.md`. Many decisions below are already made — **do not re-ask the
> user anything answered here.**

---

## 0. The one thing to know

This was a long live-iteration session (v0.1.18 → **v0.1.47**) that restructured
the repo into a **monorepo**, built the **per-tile workbench** (Files/Preview/Dev
+ git + usage), shipped an **auto-updater**, and fixed a pile of WSL-boundary
bugs. Everything below is deployed and (mostly) user-verified. The remaining work
is polish + a couple of small features (§4).

**The single most valuable debugging tool:** the running Windows app writes a
diagnostic log you can read from WSL at **`/mnt/c/Users/natha/.termhub/diag.log`**
(F12 devtools also enabled in release). Use it instead of guessing — every hard
bug this session was cracked by reading it. Clear it before a deploy with
`: > /mnt/c/Users/natha/.termhub/diag.log`.

---

## 1. What this is

**T-Hub** (codebase/repo identifiers stay `termhub`) — a local cockpit for running
& supervising many persistent Claude Code + shell sessions on **Windows 11 + WSL2
(Ubuntu-24.04) + zsh**. Tauri 2 (Rust, frameless WebView2) + React/TS/Tailwind +
xterm.js, over a `portable-pty` (ConPTY) → `wsl.exe` → **`tmux -L termhub`** spine.
Every xterm renders once into a persistent overlay pool (`TerminalPool.tsx`)
positioned over per-tile placeholders, so moving tiles never reloads a terminal.

- **GitHub:** `github.com/n8watkins/t-hub` (PUBLIC, renamed from `termhub`).
- **WSL repo (edit + commit here):** `/home/natkins/n8builds/tools` (the local
  folder stays `tools`; the repo *is* the t-hub monorepo). `main` is integration.
- **Monorepo layout:** the Tauri app is at **`apps/desktop/`**; the marketing
  site at `apps/site/` (Next.js, npm, NOT in the pnpm workspace). The pnpm
  workspace manages only `apps/desktop`. Root `package.json` name = `t-hub`.
- **Windows build mirror (NEVER edit):** `C:\Users\natha\termhub` — deploy
  `git reset --hard`s it to `origin/main`.

## 2. Build / verify / deploy / DEBUG

You **cannot** run the Windows GUI; only the user sees it. Verify in WSL, deploy
to let the user see it.

**Verify (WSL):**
```
pnpm --filter termhub typecheck                       # frontend (run from repo root)
cd apps/desktop/src-tauri && cargo check -p termhub   # backend (Linux target only)
```
`cargo check` on Linux does NOT compile `#[cfg(windows)]` code (the `wsl.exe`
paths) — the Windows build at deploy is the real cross-check.

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
(close-to-tray leaves a stale instance), hard-syncs the mirror, `pnpm install`,
**`cd apps\desktop` + `pnpm tauri build`**, launches. Do **NOT**
`tmux -L termhub kill-server` (destroys the user's running Claude sessions).

**Updater signing:** `createUpdaterArtifacts: true`, so the build MUST sign.
relaunch.ps1 loads the minisign key from `%USERPROFILE%\.tauri\termhub-updater.key`
(+ `.password`) into `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)`. Keys also in WSL
`~/.tauri/`; GitHub repo secrets set for CI. Real releases: tag `vX.Y.Z` + push →
`.github/workflows/release.yml` builds/signs/publishes `latest.json`.

**`termhub-agent` is NOT built by the deploy** (it's a workspace member that runs
inside WSL — hooks + the `--statusline` usage feed). When agent code changes,
rebuild + reinstall it manually:
```
cd apps/desktop/src-tauri && cargo build -p termhub-agent --release
cp target/release/termhub-agent ~/.local/bin/termhub-agent.new && mv -f ~/.local/bin/termhub-agent.new ~/.local/bin/termhub-agent
```
(mv-over because the file is "busy" while the app runs). Then the user must
**re-install hooks from Settings → Hooks** (writes hooks + statusLine into
`~/.claude/settings.json`).

## 3. State — DONE this session (all on `main`, deployed)

- **Monorepo restructure** → `apps/desktop` + `apps/site`; deploy + bump paths
  moved; 9 stale worktrees pruned; rollback tag `pre-monorepo-reorg`.
- **Per-tile workbench:** each tile has a **Terminal · Files · Preview · Dev** tab
  bar + ⤢ fullscreen + ×. Opening a non-terminal tab EXPANDS the panel to fill
  the tile (terminal parked); the **⇿** toggle switches to a terminal+panel SPLIT.
- **Sidebar:** "Projects" removed; **Workspaces** is the nav (collapsible, lists
  each workspace's terminals; click to switch/focus; **double-click a workspace
  name to rename**; per-terminal **×** to close). **Recent** = global, grouped by
  folder, with a darker **Resume** button + a **session dropdown**; labels use the
  session's first real prompt. Brand + collapse + settings live in the **titlebar
  left cluster**. **Usage** strip pinned at the bottom (see below).
- **Lifecycle:** tile **× = KILL** the tmux session (confirm only if a dev server
  is running on it). No more detach/trash duality.
- **Recall** (`claude --resume <id>`) works and **focuses the xterm**. Startup
  command runs via `$SHELL -ilc` (interactive login) so `claude`'s PATH resolves.
- **Git in Files** (`GitBar`): branch + linked-worktree badge + dirty count +
  **Commit…** (stage all + commit). Plus edit/save in the reader.
- **Dev runner** (Dev tab): managed `npm run dev`, detects the localhost URL,
  feeds the Preview tab.
- **Auto-updater:** Tauri updater plugin + Settings → Updates (version, check/
  install, auto-check/auto-install toggles); release workflow signs + publishes on
  `vX.Y.Z` tags. Repo is public so installs can fetch `latest.json`.
- **Usage strip:** sourced from **`claude -p /usage`** run under a **pty via
  `script -qec ... /dev/null`** (it's TTY-dependent; FREE — 0 tokens/cost). Shows
  **Weekly** + **Session** as **bars** (fill = used, colored by remaining) with
  reset hints; last-good reading cached to localStorage (never blanks).
- **Terminal copy/paste:** Tauri **clipboard-manager** plugin (navigator.clipboard
  is silently blocked in WebView2). **Shift+drag to select** (tmux mouse-mode
  captures plain drag), then **Ctrl+C / Ctrl+V**. tmux's own right-click menu
  (split/kill/zoom) is DISABLED (`unbind -n MouseDown3Pane` in `ensure_mouse_on`).
- **Startup notification-sound burst** fixed (warmup window in `lib/notify.ts`).
- **Files load reliably** — see the React gotcha in §5.

**Verified working (user-confirmed):** files load + navigate; Shift+drag copy/
paste; usage bars show real weekly/session; sidebar workspaces; resume opens Claude.

## 4. NEXT STEPS (open, ordered)

1. **`filesRootDir` setting** — the user wants Files to start at a configurable
   root (default their home `/home/natkins`), not always the project cwd. The
   field is STUBBED in `apps/desktop/src/store/settings.ts` (DEFAULTS only) but
   NOT wired: add it to `PersistedSettings`/`loadPersisted`/`persistAll`/
   `SettingsState`/the store body (follow the `resumeStartsClaude` pattern), add a
   text input in `ThemeEditor.tsx` (a "Files" group), and have `Tile.tsx` pass
   `root = filesRootDir.trim() || cwd` to `TilePanel`. Acceptance: setting it to
   `/home/natkins` roots every Files panel there.
2. **Right-click in the terminal** — currently does nothing (custom menu removed;
   tmux menu disabled). User is undecided. Likely: a minimal Copy/Paste only, or
   leave as-is. CONFIRM with the user before building.
3. **Visual pass** — the user wants the sidebar/chrome "bigger, rounder, more
   breathing room." Partial (rounded rows, 40px tile header). Not a full sweep.
4. **Kill-confirm: detect a live Claude turn** — `Tile.tsx` `busy` only checks a
   running dev server (`usePanels.devUrl[id]`), not an in-flight Claude turn,
   because there's no reliable frontend terminal-id → Claude-session-id mapping.
   If you build that mapping, fold `working`/`needsQuestion`/etc. into `busy`.
5. **Desktop notification toasts** — sounds work; OS toasts need
   `@tauri-apps/plugin-notification` wired (`lib/notify.ts` already tries it).
6. **Recent load latency** — `recent.rs` reads transcripts over the UNC share
   (~2.5s for ~200 files); could go lazy-per-folder.
7. **Stale `T-Hub.lnk`** — the desktop shortcut still points at the OLD exe path;
   new path is `C:\Users\natha\termhub\apps\desktop\src-tauri\target\release\
   termhub.exe`. relaunch.ps1 launches directly so deploy works; only the manual
   shortcut is stale (user to fix).

## 5. Conventions & gotchas (hard-won)

- **`wsl.exe` from the GUI process mangles a trailing bash arg** — passing a path
  as `bash -lc <script> termhub <path>` (`$1`) makes `$1` arrive EMPTY in the
  GUI-spawned process (works fine from an interactive WSL shell — do NOT test it
  that way). Use `wsl.exe --cd <path>` instead (or read over the
  `\\wsl.localhost\<distro>\` UNC share with std::fs). This broke Recent AND the
  Files tree; both fixed.
- **`zsh -lc` skips `~/.zshrc`** (login but non-interactive), where the user's
  PATH (`~/.npm-global/bin` → `claude`) lives. Use **`$SHELL -ilc`** (interactive)
  for any command needing the user's tools. This is why Resume + `/usage` use it.
- **`claude -p /usage` is TTY-dependent** — piped it prints only an intro line;
  run it under `script -qec '<cmd>' /dev/null` to get the numbers. It's FREE.
- **React effect lesson (the files "loading…" marathon):** `TreeDir`'s load effect
  had `loading`/`entries` in its deps, so `setLoading(true)` re-ran the effect,
  whose cleanup cancelled the in-flight fetch → stuck "loading…". A load effect
  must NOT depend on the state it sets. Deps are `[open, path]` only now.
- **Commits:** ASCII-only messages (no em-dashes — they break the `.ps1`). End with
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Bump version EVERY deploy.
- **Parallel agents:** the user likes orchestrated parallel Opus subagents on
  worktrees off `main` with DISJOINT file ownership; the orchestrator merges +
  verifies + deploys. Shared hot-spots: `lib.rs` (command/plugin registration),
  `workspace.ts`, `Tile.tsx`, `TerminalPool.tsx`, `main.tsx`. Scaffold shared
  contracts first.
- **pnpm install** sometimes needs `--no-frozen-lockfile` (after a dep add) or
  `CI=1` (to auto-confirm the modules-purge prompt).

## 6. File map (for the next steps)

- `apps/desktop/src/components/FilePanel.tsx` — Files panel: tree (`TreeDir`),
  search box, reader/editor, `GitBar`, compact (stacked) vs roomy layouts, the
  `compact` prop, `shortPath()`. The load-effect fix is in `TreeDir`.
- `apps/desktop/src/components/Tile.tsx` — tile chrome + tab bar + ⤢/× ; body
  renders the terminal placeholder / `PanelPane` (split vs expanded). `shortenHomePath`.
- `apps/desktop/src/components/TerminalPool.tsx` — the xterm overlay pool;
  `shouldShow` gates visibility (parked when panel expanded / inactive tab).
- `apps/desktop/src/components/TilePanel.tsx` — body switcher (Files/Preview/Dev).
- `apps/desktop/src/components/Terminal.tsx` — xterm wrapper; Ctrl+C/V via the
  clipboard plugin (`clipboardWrite/Read`); `onContextMenu` preventDefault.
- `apps/desktop/src/components/{WorkspacesList,RecentList,UsageStrip,Sidebar,Titlebar}.tsx`.
- `apps/desktop/src/store/{panels,workspace,settings,supervision,theme}.ts`.
- `apps/desktop/src-tauri/src/{files,recent,git,usage,devserver,tmux,commands}.rs`,
  `claude/{install,hooks,status}.rs`, `lib.rs` (command + plugin registration).
- `.github/workflows/release.yml` — signed release pipeline (tag `vX.Y.Z`).
