# T-Hub — Session Handoff

**Last updated:** 2026-06-22 · **Branch:** `main` (clean, in sync with `origin/main`) · **App version:** `0.1.67`

> Read this whole file first, plus `PRD.md` and `README.md`. Most decisions below
> are already made — **do not re-ask the user anything answered here.**

---

## 0. The one thing to know

This was a long live-iteration session. It (1) **merged 3 parallel lanes** (A tile-identity, B sidebar-strips, C terminal-input) to `main`, (2) ran **post-merge code reviews** and fixed the findings, (3) worked through a **big user-walkthrough backlog**, (4) built **Codex support** (usage readout, icon, auto-continue), and (5) **rebranded the user-facing name to "T-Hub"**. Everything is committed + pushed.

**REPO LOCATION + LAYOUT:** the tree is at **`/home/natkins/projects/tools/t-hub/`** (was `…/n8builds/tools/t-hub` earlier in the session). It's now a **SINGLE repo** — work directly in **`/home/natkins/projects/tools/t-hub/t-hub-app`** on **`main`**. The parallel-lane worktrees (incl. `wt-terminal-input`) and their branches (`fix/post-merge-review` etc.) were **removed after merge** (cleanup 2026-06-20) — `git worktree list` should show only `t-hub-app [main]`, `git branch` only `main`. See [[t-hub-monorepo-structure]] memory.

**Two dev surfaces — know the difference:**
- **WSLg/Linux dev instance** (`T_HUB_TMUX_SOCKET=t-hub-dev pnpm tauri dev` from `apps/desktop`): fast hot-reload, but **cannot** exercise Windows-only features (OS file-drop, clipboard-image) — it's webkitgtk, not WebView2. Great for UI/logic; misleading for those two features.
- **Windows installer** (`T-Hub_0.1.67_x64-setup.exe`, in `C:\Users\natha\Downloads\`): the REAL app (WebView2). This is what to install to test file-drop / image-paste / true titlebar.
- **T-Hub Dev** (side-by-side Windows sandbox): a SEPARATE installable app (`com.t-hub.dev`, isolated `t-hub-dev` socket + `~/.t-hub-dev` state) that coexists with production T-Hub and can't disturb its live sessions. Build via `gh workflow run release.yml --ref main -f variant=dev`. **See [docs/DEV-BUILD.md](DEV-BUILD.md)** for the full prod-vs-dev model, what's isolated/shared, and the complete `t-hub`-identifier inventory.

**Best debug tool:** the running app writes a diag log to `<home>/.t-hub/diag.log`, resolved per-user at startup — `%USERPROFILE%` on Windows (read from WSL at `/mnt/c/Users/natha/.t-hub/diag.log`), `$HOME` on unix. Overridable via `$T_HUB_DIAG_FILE` (the side-by-side DEV build points it at `~/.t-hub-dev/diag.log`). **Gotcha:** an *inherited* `T_HUB_DIAG_FILE`/`T_HUB_TMUX_SOCKET`/`T_HUB_CONTROL_FILE` WINS — so a prod app launched from a WSL session a dev-isolated app spawned will silently run on the dev socket + log to the DEV path. The always-fires startup marker `t-hub: started vX (diag -> …)` reveals the resolved path; if it points at `.t-hub-dev`, the env is polluted (close the dev app / relaunch from a clean shell). Grep tags: `codex`, `autocontinue`, `usage`, `pool`, `resize`, `reconcile`, `recent`.

---

## 1. What this is

**T-Hub** (codebase/repo identifiers deliberately stay `t-hub`) — a local cockpit for many persistent Claude Code + Codex + shell sessions on **Windows 11 + WSL2 (Ubuntu-24.04) + zsh**. Tauri 2 (Rust, frameless WebView2) + React/TS/Tailwind + xterm.js, over `portable-pty` (ConPTY) → `wsl.exe` → **`tmux -L t-hub`** (dev: `t-hub-dev`). Each xterm renders once into a persistent overlay pool (`TerminalPool.tsx`) positioned over per-tile placeholders. Each terminal's tmux session is **`th_<terminalId>`**.

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
- `417c8d1` **brand → "T-Hub"** (window title, productName, satellite title, tray). (Internal ids were renamed later — see the full `t-hub` rename in §4.)

**Verified:** `pnpm --filter t-hub-desktop typecheck` + `cargo check -p t-hub` green throughout (only pre-existing `TerminalState` dead-code warning). Codex usage end-to-end verified live. Drag/sidebar/contrast verified by review + types, NOT runtime-clicked.

---

## 3. Next steps (ordered)

> **Note (since this handoff):** the herdr-parity backlog that followed this session has **shipped** — Wave 0 + Wave 1 (OS toasts, the `Ctrl+B` prefix + `Ctrl+K` palette keymap, the `git_worktree_add/list/remove` primitive, `wait_for_status`, the `store/rules.ts` rules engine, and session-restore via `tile_sessions` + the Recovery panel) plus **WS-9** (the worktree-centric workflow). See [ROADMAP-PLAN.md](./ROADMAP-PLAN.md) for the shipped state. **The real next project is the remote / server-split (⑥)** — extract a headless `t-hub-server` in WSL and make the GUI a thin client over Tailscale. See [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) and the *Later / someday* section of ROADMAP-PLAN.

1. **⑥ Remote / SSH (server split) — the next big project.** Per [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md): M1 decouple locally (`RemoteTarget` routing the WSL/tmux/git hops) → M2 PTY over the wire → M3 overlay panels server-side → M4 multi-client. It refactors the same backend seams everything else touches, so it runs *after* the parity work (now done).
2. **Runtime smoke-test the shipped parity work in the live Windows app** — all of Wave 0/1 + WS-9 was verified at compile + unit-test level, not runtime-clicked. Drive a session to `NeedsPermission` (toast), exercise `Ctrl+B w`/`c`/`l` (worktree tabs), and kill+relaunch with live agent tiles (offered restore).
3. **Test file-drop + image-paste on the Windows build** — install `T-Hub_0.1.67_x64-setup.exe`. Drag a file from Explorer onto a terminal (should type its `/mnt/c/…` path) and Ctrl+V a screenshot (should insert a temp PNG path). Code: `src/lib/dropPaste.ts`, `src-tauri/src/dropin.rs`. WSLg dev CANNOT test these.
4. **Perf/cleanup nits (low):** per-row supervision lookup O(rows×snapshots) (partly done — `sessionIdByTmux` index added); duplicated terminal-row vs workspace-row drag choreography; Recents `cwdWorktree` is a path heuristic that can disagree with the real branch.
5. **Validate auto-continue against a real limit** — it's untested end-to-end (can't force a rate-limit). Watch the `autocontinue` diag tag when a limit naturally hits. (Now a migrated rule in the rules engine.)

---

## 4. Conventions & gotchas

- **Commit + push cadence:** work on `main` in `t-hub-app`; commit after each logical change (trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`) and `git push origin main`.
- **Gates:** `pnpm --filter t-hub-desktop typecheck` (from the repo root, `t-hub-app`) + `cargo check -p t-hub` (from `apps/desktop/src-tauri`). The lone `TerminalState` dead-code warning is pre-existing.
- **Windows build:** `gh workflow run release.yml --ref main` triggers a **`workflow_dispatch`** build = a downloadable **artifact** (NOT a public release; release/`latest.json` steps are gated on `v*` tags). Download with `gh run download <id> -n t-hub-installers -D <dir>`. Installer name follows `productName` → now `T-Hub_<ver>_x64-setup.exe`.
- **Full `t-hub` rename (rollback tag `pre-thub-rename`):** the project is now **100% `t-hub`** (crate names, bundle ids, tmux socket, MCP server, `~/.t-hub` config dir, `T_HUB_*` env hooks, the `__t_hub_managed__` marker — all renamed). The prod **bundle id changed to `com.t-hub.app`**, so installing the renamed prod app is a **FRESH install** (any prior install stays until you uninstall it) and live tmux sessions restart on the new `t-hub` socket. Naming map + canonical ids: [docs/DEV-BUILD.md](DEV-BUILD.md).
- **Two installable variants:** prod `T-Hub` (`com.t-hub.app`) and sandbox `T-Hub Dev` (`com.t-hub.dev`) coexist; each replaces only its own prior install. Variant is a Cargo feature (`devbuild`) + the `tauri.dev.conf.json` overlay; CI selects it with the `variant` dispatch input. Details: [docs/DEV-BUILD.md](DEV-BUILD.md).
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

See also memories: [[t-hub-postmerge-feedback]] (the live backlog tracker), [[t-hub-monorepo-structure]] (paths/move), [[t-hub-deploy-flow]], [[dev-instance-no-auto-deploy]].
