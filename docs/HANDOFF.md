# T-Hub — Session Handoff

**Last updated:** 2026-06-20 · **Branch:** `main` (clean, pushed to `origin/main`, `HEAD = 417c8d1`) · **App version:** `0.1.54`

> Read this whole file first, plus `PRD.md` and `README.md`. Most decisions below
> are already made — **do not re-ask the user anything answered here.**

---

## 0. The one thing to know

This was a long live-iteration session. It (1) **merged 3 parallel lanes** (A tile-identity, B sidebar-strips, C terminal-input) to `main`, (2) ran **post-merge code reviews** and fixed the findings, (3) worked through a **big user-walkthrough backlog**, (4) built **Codex support** (usage readout, icon, auto-continue), and (5) **rebranded the user-facing name to "T-Hub"**. Everything is committed + pushed.

**REPO MOVED mid-session:** the tree is now at **`/home/natkins/projects/tools/t-hub/`** (was `…/n8builds/tools/t-hub`). Main repo: `…/projects/tools/t-hub/t-hub-app`; active worktree (where work happened): `…/projects/tools/t-hub/wt-terminal-input` on branch **`fix/post-merge-review`** (it tracks + pushes to `origin/main`). The worktree's git link broke on the move and was fixed with `git worktree repair <worktree-path>` from the main repo. See [[t-hub-monorepo-structure]] memory.

**Two dev surfaces — know the difference:**
- **WSLg/Linux dev instance** (`TERMHUB_TMUX_SOCKET=termhub-dev pnpm tauri dev` from `apps/desktop`): fast hot-reload, but **cannot** exercise Windows-only features (OS file-drop, clipboard-image) — it's webkitgtk, not WebView2. Great for UI/logic; misleading for those two features.
- **Windows installer** (`T-Hub_0.1.54_x64-setup.exe`, in `C:\Users\natha\Downloads\`): the REAL app (WebView2). This is what to install to test file-drop / image-paste / true titlebar.

**Best debug tool:** the running app writes a diag log readable from WSL at **`/home/natkins/.termhub/diag.log`** (dev instance) and `/mnt/c/Users/natha/.termhub/diag.log` (Windows). Grep tags: `codex`, `autocontinue`, `usage`, `pool`, `resize`.

---

## 1. What this is

**T-Hub** (codebase/repo identifiers deliberately stay `termhub`) — a local cockpit for many persistent Claude Code + Codex + shell sessions on **Windows 11 + WSL2 (Ubuntu-24.04) + zsh**. Tauri 2 (Rust, frameless WebView2) + React/TS/Tailwind + xterm.js, over `portable-pty` (ConPTY) → `wsl.exe` → **`tmux -L termhub`** (dev: `termhub-dev`). Each xterm renders once into a persistent overlay pool (`TerminalPool.tsx`) positioned over per-tile placeholders. Each terminal's tmux session is **`th_<terminalId>`**.

---

## 2. State — done this session (all on `main`)

Merged lane work + fixes (oldest→newest): `e40c82d`…`99f3517` (lanes A/B/C core + their review fixes), then:
- `41920b7` harden cross-lane drag/drop + shared helpers · `a62a102` reorder threshold + moveTab off-by-one + search-respects-dotfiles
- `4651418` **Preview**: re-attach Dev runner (Dev tab → devUrl → Preview) + de-circle Claude icon · `7952653` **hide dotfiles** (tree+search) · `c8a0547` **sidebar** color cascade + per-terminal color circle + reorder + activity pulse · `d044f66` **Recents** enriched
- `e7fa13b` drag-anywhere + grid→sidebar move + branch poll + context-meter gated-to-Claude + usage retry · `e1ca2af` review fixes (devUrl cleanup, gated git poll, color-picker, usage doc)
- `214930b`/`9b989cb` **auto-continue on usage reset** (per-terminal opt-in via tile ⋯ menu; agent-aware — Claude via statusline snapshots, Codex via the codex usage poll; injects `settings.autoContinueText`, default "continue", at the reset)
- `1930d8f`/`8ab42e6` **Codex usage** in the sidebar — reads the LIVE `~/.codex/logs_*.sqlite` (rollouts stopped June 10); extracts the embedded `rate_limits` block (`reset_at`/`resets_at`). **VERIFIED live**: a 2:16pm codex run logged 5h=1%/wk=0% and the sidebar updated on the next poll.
- `6c6a29f`/`4de50f7` **Codex icon** = user PNG (`src/assets/codex.png`), white halo stripped to transparent; **sidebar rows lead with the agent icon** · `e77f42c` **detect node→codex**: `tmux.rs::pane_info` resolves runtime-wrapped agents via the pane pid's child `/proc/<kid>/cmdline` (Codex ships as `node …/codex`)
- `49d0f4f` **theme contrast**: `color-scheme` per theme (fixes white-on-white native `<select>` dropdowns), solid menu bgs, AA muted text · `540636e` settings nav: drop "App"/"Theme" group headers
- `417c8d1` **brand → "T-Hub"** (window title, productName, satellite title, tray). Identifiers (`com.termhub.dev`, crate, socket, MCP) unchanged.

**Verified:** `pnpm --filter termhub typecheck` + `cargo check -p termhub` green throughout (only pre-existing `TerminalState` dead-code warning). Codex usage end-to-end verified live. Drag/sidebar/contrast verified by review + types, NOT runtime-clicked.

---

## 3. Next steps (ordered)

1. **Test file-drop + image-paste on the Windows build** — install `T-Hub_0.1.54_x64-setup.exe`. Drag a file from Explorer onto a terminal (should type its `/mnt/c/…` path) and Ctrl+V a screenshot (should insert a temp PNG path). Code: `src/lib/dropPaste.ts`, `src-tauri/src/dropin.rs`. WSLg dev CANNOT test these.
2. **"Move some info to the bottom"** — BLOCKED on the user: needs which items → where. Don't guess.
3. **PR-view per workspace** — a `gh pr list` panel for the workspace's repos/worktrees (title, checks, click-to-open). User likes it; awaiting go-ahead.
4. **Detached EDITABLE Files window** — pending from before this session (see [[t-hub-ui-batch-jun2026]] memory).
5. **Perf/cleanup nits (low):** per-row supervision lookup O(rows×snapshots) (partly done — `sessionIdByTmux` index added); duplicated terminal-row vs workspace-row drag choreography; Recents `cwdWorktree` is a path heuristic that can disagree with the real branch.
6. **Validate auto-continue against a real limit** — it's untested end-to-end (can't force a rate-limit). Watch the `autocontinue` diag tag when a limit naturally hits.

---

## 4. Conventions & gotchas

- **Commit + push cadence:** commit after each logical change; push to `origin/main`. Trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Work happens on the `fix/post-merge-review` worktree which pushes to `main` via `git push origin HEAD:main` (rebase onto `origin/main` first).
- **Gates:** `pnpm --filter termhub typecheck` (from worktree root) + `cargo check -p termhub` (from `apps/desktop/src-tauri`). The lone `TerminalState` dead-code warning is pre-existing.
- **Windows build:** `gh workflow run release.yml --ref main` triggers a **`workflow_dispatch`** build = a downloadable **artifact** (NOT a public release; release/`latest.json` steps are gated on `v*` tags). Download with `gh run download <id> -n termhub-installers -D <dir>`. Installer name follows `productName` → now `T-Hub_<ver>_x64-setup.exe`.
- **Brand rename caveat:** `productName` is now `T-Hub`, so the installer/install-path changed → the old "TermHub" install will NOT auto-upgrade in place (one-time manual install of T-Hub). Reversible by setting `productName` back. Technical ids stay `termhub` ON PURPOSE.
- **Codex detection:** Codex runs as a `node` process (`@openai/codex/bin/codex.js`), so `pane_current_command` = `node`. `tmux.rs::pane_info` now resolves it via `/proc/<child>/cmdline`. Claude runs as `claude` directly.
- **Codex usage source:** `~/.codex/logs_*.sqlite` (live). The old `~/.codex/sessions/**/*.jsonl` rollouts STOPPED June 10 — don't read those. Cloud/web Codex writes NO local file, so only in-terminal Codex CLI usage is visible.
- **MCP `list_terminals` is a red herring for cmd/cwd:** the control-channel handler (`control.rs`) hardcodes `title=tmux_session`, `cwd=""` — it does NOT call `pane_info`. The real UI uses the Tauri `commands::list_terminals` which does. Don't diagnose "empty command" from the MCP path.
- **Don't run the installer for the user** — they install manually.

---

## 5. File map (for the next steps)

- `apps/desktop/src/lib/dropPaste.ts` — file-drop → PTY path insert (C1) + path translation; `src-tauri/src/dropin.rs` — clipboard image → temp PNG (C2).
- `apps/desktop/src-tauri/src/codex.rs` — Codex usage (reads `~/.codex/logs_*.sqlite`); `src/ipc/codex.ts` + `src/components/UsageStrip.tsx` (`useCodexUsage`/`CodexUsageStrip`).
- `apps/desktop/src/lib/autoContinueMount.ts` — auto-continue controller; `src/store/autoContinue.ts` — per-terminal opt-in.
- `apps/desktop/src-tauri/src/tmux.rs` — `pane_info` (node→codex resolution); `src/store/clientType.ts` — claude/codex classification.
- `apps/desktop/src/components/WorkspacesList.tsx` — sidebar rows (agent icon, color, reorder, activity); `Tile.tsx` — tile chrome + ⋯ menu (auto-continue toggle); `ThemeEditor.tsx` — settings nav.
- `apps/desktop/src/store/theme.ts` (+ `index.css`) — themes + `color-scheme`; `src/assets/codex.png` — the Codex icon.
- `apps/desktop/src-tauri/tauri.conf.json` — `productName`/title (brand) + version.

See also memories: [[t-hub-postmerge-feedback]] (the live backlog tracker), [[t-hub-monorepo-structure]] (paths/move), [[termhub-deploy-flow]], [[dev-instance-no-auto-deploy]].
