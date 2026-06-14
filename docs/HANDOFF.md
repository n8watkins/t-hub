# TermHub — Session Handoff

**Last updated:** 2026-06-13 · **Branch:** `main` @ `240c646` (clean, pushed to `origin/main`)

> Read this whole file + [PLAN.md](./PLAN.md), [MCP.md](./MCP.md),
> [SESSION_AWARENESS.md](./SESSION_AWARENESS.md), and the repo `README.md`
> before doing anything. Do **not** re-ask the user anything answered here.

---

## 0. The one thing to do next

The user dumped a large batch of UX feedback (the "shell-v3" list in §5). A
worktree is already prepared: **`/home/natkins/n8builds/th-shell3`** on branch
**`feat/shell-v3`** (branched from `main` @ `240c646`, no commits yet). Start
there. The **#1 recurring bug: tile drag does not work** — the current
implementation uses HTML5 drag-and-drop, which dies over xterm's WebGL canvas
in WebView2. **Rebuild it with POINTER events** (`pointerdown`/`move`/`up` +
`document.elementFromPoint`), not HTML5 DnD.

---

## 1. What this is

**TermHub** — a terminal-first command center for running/supervising many
persistent Claude Code sessions at once. Target: **Windows 11 + WSL2 (Ubuntu
24.04) + zsh**. Tauri 2 (Rust) shell + React/TS/Tailwind frontend + xterm.js,
with a `portable-pty` (ConPTY) → `wsl.exe` → **`tmux -L termhub`** spine.

- **GitHub:** `github.com/n8watkins/termhub` (private; `gh` authed in WSL as `n8watkins`).
- **WSL repo (edit + commit here):** `/home/natkins/n8builds/tools` (ext4).
- **Windows build mirror (never edit):** `C:\Users\natha\termhub`.
- **Env:** WSL user `natkins`, Windows user `natha`. `/mnt/c` ↔ `C:\`.
- The product spec is `PRD.md`; the technical review is `REVIEW.md`; the
  forward plan is `docs/PLAN.md`.

## 2. Build / run / deploy (CRITICAL — the app only runs on Windows)

The app is a **Windows** binary (WebView2 + ConPTY + `wsl.exe`). It **cannot
run in WSL/Linux**. You (agents) **cannot launch the GUI** — the user is the
only one who sees it. So:

**To verify code (in WSL):**
```
pnpm typecheck                                                   # frontend
cargo check --manifest-path src-tauri/Cargo.toml --workspace     # backend (webkit IS installed here)
cargo test  --manifest-path src-tauri/Cargo.toml --workspace     # tests
```

**To deploy a change so the user sees it:**
1. Commit + push to `origin/main` from `/home/natkins/n8builds/tools`.
2. Clear stale sessions: `tmux -L termhub kill-server`.
3. Run the rebuild+relaunch script (force-syncs the Windows mirror to
   `origin/main`, `pnpm install`, `pnpm tauri build`, launches the exe):
   ```
   powershell.exe -NoProfile -ExecutionPolicy Bypass -File 'C:\Users\natha\Downloads\termhub_relaunch.ps1'
   ```
   It prints `BUILD_EXIT=0` + `LAUNCHED ...` on success. First build of a clean
   target is ~15-25 min; incremental ~1-3 min. (The script sets
   `CARGO_PROFILE_RELEASE_LTO=false` for speed.) **Filter the noisy PowerShell
   stderr** by piping through `grep -vE 'RemoteException|CategoryInfo|FullyQualified|At C:|^\s*\+|NotSpecified'`.
   The "RemoteException" lines are just git's progress text — not errors.

> Windows toolchain is fully present (Rust MSVC, VS Build Tools 2022, Node 22,
> pnpm 10, WebView2 149). The Windows clone is a **mirror**: the script does
> `git reset --hard origin/main`, so never edit it directly.

## 3. State — everything below is MERGED to `main` and builds green

This session built the 0.1 nucleus → a large v1 slice via **8 parallel agent
branches, all merged with (near) zero conflicts** thanks to strict disjoint
file ownership. Key commit anchors (newest first):

| Area | Commit(s) | Notes |
|---|---|---|
| Terminal palette → live xterm | `240c646` | theme recolors terminals too |
| Theming system | `4ca194b`/`15badb8` | tokens→CSS vars, `Ctrl/Cmd+,` editor, presets, get/set_theme + `theme://changed` |
| MCP server | `6f4b1f1`/`ef16c56` | `termhub-mcp` binary + app control listener, 13 tools, `.mcp.json`, e2e-tested |
| Fresh-prompt cascade fix | `ad523df` | fresh spawn = empty seed + frontend Ctrl-L |
| Shell v2 (Chrome top bar) | `65aad1b`/`b75c569` | persistent top bar, custom min/max/close, drag regions, tab fixes, **fixed-but-still-broken** tile drag |
| Files (index/search/reader) | `2245028` | backend + `FilePanel` (NOT mounted — see §6) |
| Session awareness (live sidebar) | `dd5ab95`/`fbf762d` | emit spine wired; needs runtime agent connection (§6) |
| Workspace tabs | `f14e00b` | tabs, per-tab persistence (localStorage `termhub.workspace.v2`, v1-migration) |
| Frameless window | `8701e6d` | `decorations:false` + (old) auto-hide titlebar |
| 0.5 personal-alpha base | `239cf61` | protocol crate, agent, supervision reducer, sidebar |
| Windows runtime fixes | `9469305`/`ba43a80`/`6303ec4`/`1fb8a8d`/`b7876d0` | wsl.exe-tmux routing, open-in-`~`, Cascadia Mono, copy/paste, no green dirs, no tmux status bar/mouse |
| 0.1 nucleus + global zoom | `e10eb79` and earlier | xterm tiles, PTY/tmux, auto-grid, Ctrl+/-/0 |

**Verified:** all merges compile (`typecheck` + `cargo check --workspace`);
agent test suites pass in their worktrees (session-awareness 76, MCP 37, files
13, theme 4). **Not verified:** the Windows GUI — only the user can confirm
visuals/interactions.

## 4. Live UX state the user has tested (and reactions)

Working: terminals spawn (WSL zsh via tmux, open in `~`, Cascadia Mono, no
green dirs, no scrollbar, arrow cursor), `Ctrl+C/V` copy-paste, global zoom,
tabs (switch with no terminal reload), gutters, the theme editor (`Ctrl+,`).
Cascade ("bunch of cmds on spawn") — fixed in `ad523df`, user not re-confirmed.

## 5. NEXT STEPS — `feat/shell-v3` (the user's latest feedback, verbatim intent)

Do these in `/home/natkins/n8builds/th-shell3`. They all touch the shell
(`Canvas.tsx`, `Tile.tsx`, `Titlebar.tsx`, `App.tsx`, `store/workspace.ts`,
`Sidebar.tsx`, `index.css`) — theming is already merged, so build on the
themed components and consume the `--th-*` CSS vars / `useTheme` store.

1. **Tile drag is STILL broken — top priority.** Rebuild with **pointer events
   + `elementFromPoint`** (HTML5 DnD fails over the WebGL canvas). Must support:
   drag a tile onto **any** other tile to **swap/reposition in any direction
   incl. diagonal**; and **drag a tile onto a workspace tab to move it to that
   tab**.
2. **Resizable sidebar** — let the user drag the sidebar's right edge to resize
   its width (clamp to a sane range).
3. **Visible Settings button** — a settings entry (gear) somewhere in the top
   bar that opens the theme/settings editor (today it's only `Ctrl/Cmd+,`).
4. **Wider workspace tabs** — increase each tab's width. (User retracted the
   "tabs further right" idea — ignore that part.)
5. **Status dots are confusing** — the green circles next to terminals/tabs
   read as "selected?". Either explain them (they're terminal **lifecycle
   state**: starting/live/detached/exited/error — see `DOT_CLASS` in `Tile.tsx`)
   or de-emphasize. The user wants **selection** to just be a **subtle
   theme-accent "lit up"**, not a hard ring/circle.
6. **Rename TermHub → "T-Hub"** in the **top-left** of the title bar (the user
   prefers "T-Hub"). (Product name change; window title can stay or change too.)
7. **Window controls visibility:**
   - **Not maximized →** min/maximize/close **always visible** (never hide).
   - **Maximized →** controls **auto-hide after ~2s**; hovering the top edge
     **persists them ~3s** then hides again.
8. **Titlebar reveal should PUSH content down (layout shift), not overlay** —
   like the content getting shoved down when you touch the top; only on
   touching the very top; **make this toggleable in Settings**.
9. **Settings panel** (ties to #3) — a real settings surface (theme editor is
   the start; add the toggles like #8).
10. **Claude-hooks panel is confusing** (`HookInstallPanel` in the sidebar) —
    clarify copy: it installs Claude Code hooks **globally** in
    `~/.claude/settings.json` (affects **all** Claude sessions, not the focused
    terminal). Say so in the panel.
11. **Drag terminal ↔ workspace** (covered by #1's drag-to-tab).
12. **Drag-reorder workspace tabs** — `shell-v2` added `moveTab`; verify it
    actually works (likely same HTML5-DnD problem → pointer-ify it).
13. **(BIG / future) Multi-window:** pull a workspace **out into a new window**,
    and drag workspaces **between windows**. This is a real architecture effort
    (Tauri multi-window + cross-window DnD) — scope/plan it separately; PRD-level.

## 6. Deferred (owned by integrator, not the agents) — see TaskList #21/#22

- **Sidebar shows no live data (#21).** The emit spine is merged, but the
  Windows app spawns **non-login** `wsl.exe -- termhub-agent`, which won't find
  `termhub-agent` (installed at `~/.local/bin`, only on the interactive PATH).
  Fix: make it reachable (install to `/usr/local/bin` w/ sudo, or use an
  absolute path / `TERMHUB_AGENT_BIN`). Also: the sidebar only shows anything
  when a **real Claude Code session is running** (hooks fire → journal → emit).
  See `docs/SESSION_AWARENESS.md`.
- **FilePanel not mounted (#22).** `src/components/FilePanel.tsx` is built but
  unmounted; mount it (toggle / sidebar tab) and route its index through WSL
  paths (the Rust index runs Windows-side; pass a `\\wsl.localhost\...` or
  WSL-agent path). Backend commands exist (`index_project`, `search_files`,
  `list_dir`, `read_text_file`).
- **MCP stubs:** `list_tabs` returns empty (no tab read-API yet); the UI-mutating
  tools (`focus_session`/`move_tile`/`rename_tab`) return `applied:false`
  (need frontend-facing commands). See `docs/MCP.md`.

## 7. Conventions & gotchas (hard-won this session)

- **Commit trailer (required):** end every commit with
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Commit after each
  logical change; push when sane. Use the `cmX.txt` heredoc pattern for multi-
  line messages (PowerShell/zsh hate em-dashes — keep commit + **`.ps1` files
  ASCII-only**; a `—` in a script breaks PowerShell parsing).
- **Parallel agents pattern that worked:** give each agent its **own git
  worktree + branch**, **strict disjoint file ownership**, "don't touch
  package.json/Cargo.toml unless adding a dep", "don't commit shared files",
  push the branch; the integrator merges sequentially. This produced ~zero
  merge conflicts across 8 branches. Shared merge points to expect: `lib.rs`
  (handler/registration appends), `src/ipc/types.ts` (appends), `App.tsx`.
- **The cascade ("bunch of cmds on spawn")** was fit/resize timing, not the
  shell. `Terminal.tsx`: the first `fit()` is deferred to a double-`rAF` and
  attach runs inside it; fresh spawns are **not seeded** (`commands.rs`
  `attach_terminal` returns empty for `has_live`), and the frontend sends one
  `Ctrl-L` to draw a single clean prompt. Don't reintroduce a synchronous fit
  or a fresh-spawn capture.
- **tmux is inside WSL.** On Windows, ALL `tmux` control commands AND the attach
  are routed through `wsl.exe` (`tmux.rs` `tmux()` is `#[cfg(windows)]` ->
  `wsl.exe --cd ~ -- tmux -L termhub …`). New terminals open in `~` (WSL home).
  TermHub sets `status off` + `mouse off` per session (so xterm owns selection →
  `Ctrl+C` copies; no tmux right-click menu).
- **Frameless window:** `tauri.conf.json` `decorations:false`. Window perms are
  in `capabilities/default.json` (minimize/maximize/unmaximize/close/start-dragging).
  Drag regions use `data-tauri-drag-region`.
- **The user's `~/.zshrc` was edited** (one additive line: `LS_COLORS` `ow`/`tw`
  → blue so `/mnt/c` dirs aren't green). Don't undo it.
- **Theming = CSS vars.** `:root` defaults live in `src/index.css`; the active
  theme overwrites `--th-*` vars from `src/store/theme.ts` (`useTheme` store);
  `ThemeEditor` mounts via `src/themeBootstrap.tsx` (its own React root, no
  App.tsx hook) + a `<script>` in `index.html`. Consume vars via Tailwind
  arbitrary values, e.g. `bg-[var(--th-app-bg)]`.

## 8. File map (for the shell-v3 work)

- `src/App.tsx` — top-level shell (Titlebar + sidebar + canvas column).
- `src/components/Titlebar.tsx` — the persistent top bar (tab strip + window controls + drag regions).
- `src/components/Canvas.tsx` — tab rendering (all tabs mounted, `display:none` inactive), grid, gutters, FAB.
- `src/components/Tile.tsx` — tile chrome, status dot (`DOT_CLASS`), the drag source/target (HTML5 — **pointer-ify**).
- `src/components/Terminal.tsx` — xterm wrapper (fit/attach/cascade/zoom/copy-paste/palette). Integrator-owned.
- `src/store/workspace.ts` — tabs/order/focus/fontSize + `moveTile`/`moveTab`/`addTab`/`cycleTab` + persistence.
- `src/store/theme.ts` + `src/components/ThemeEditor.tsx` + `src/index.css` — theming.
- `src/components/Sidebar.tsx` + `HookInstallPanel.tsx` — supervision sidebar + hooks UI.
- `src-tauri/src/{commands,pty,tmux,commands_05,theme,control,files}.rs`, `agent/`, `crates/{termhub-protocol,termhub-agent,termhub-mcp}`.

## 9. Worktrees / branches

`main` is the integration branch. Already-merged feature branches still have
worktrees (can be removed with `git worktree remove`): `feat/0.5-personal-alpha`,
`feat/tabs`, `feat/session-awareness`, `feat/files`, `feat/shell-v2`, `feat/mcp`,
`feat/theming`. **Active:** `feat/shell-v3` @ `/home/natkins/n8builds/th-shell3`
(empty, start here).
