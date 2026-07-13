# T-Hub

T-Hub is a **terminal-first command center for running and supervising many persistent coding-agent (Claude Code) sessions at once**. The V1 target is a single personal setup: Windows 11 + WSL2 Ubuntu + zsh, with an adapter-based core so other terminal agents can be added later.

## Status — released personal alpha (v0.3.58)

Well past the original "playable proof" nucleus: the terminal spine, the agent-supervision layer (the 0.5 plan), Codex support, and the full Windows/WSL integration all ship in the installed app. **v0.3.58 is released** — a signed Windows installer ships from a GitHub Release with `latest.json` auto-update wired in (Tauri updater plugin).

- **Tauri 2 + React 18 + TypeScript + Tailwind** desktop shell with an xterm.js tile grid (Fit + WebGL + Search + Unicode 11), deterministic insertion/focus, and durable layout/workspace persistence.
- **Rust PTY ↔ tmux backend:** `portable-pty` (ConPTY on Windows) drives a `tmux -L t-hub` session per terminal — one PTY client per visible tile. Closing a tile **detaches** (the process survives); stop **kills** the session. `#[cfg(windows)]` reaches into WSL via `wsl.exe -e bash` (the `-e`/`--exec` is load-bearing — `wsl.exe -- bash` runs the user's *login* shell, e.g. zsh); `#[cfg(unix)]` attaches to tmux directly.
- **Agent supervision:** a `t-hub-agent` sidecar + Claude Code hooks feed a journal/statusline spine — context + cost readout, autocontinue, supervision tree. Hooks install consent-gated from **Settings → Hooks** and self-heal on startup.
- **Git worktree workflow:** `Ctrl+B w` creates a worktree tab from a branch name (with a repo picker when the focused tile isn't in a repo), landing it as a sibling `<repo>-worktrees/<branch>`; `Ctrl+B c` opens a plain tab and `Ctrl+B l` lists/re-opens existing worktrees.
- **Rebindable hybrid keymap:** direct hotkeys + a tmux-style `Ctrl+B` prefix tier + a `Ctrl+K` fuzzy command palette — all bindings are user-editable from Settings and persist.
- **Event→action rules engine:** user-configurable rules fire when a supervised session's FR-012 status transitions (optionally from a specific prior status) and run one action — notify, type text, spawn, restart, or run a command in the session.
- **Notifications:** synthesized WebAudio chimes for attention/done/error plus OS toast notifications (via the Tauri notification plugin), both gated by Settings toggles.
- **Native session-restore:** orphaned Claude sessions surviving an app/host restart are listed in the **Recovery** panel and brought back via `claude --resume <id>` into a fresh tile.
- **Tray recovery actions:** light, no-restart recovery from the system tray — **Reload window** (re-renders the React UI without touching tmux/agent) and **Reconnect agent bridge** (safe disconnect → reconnect off the UI thread, preserving the journal cursor).
- **Codex + Claude:** per-provider usage readouts, icons, and running-pulse activity in the sidebar.
- **MCP control channel:** the `t-hub-mcp` server forwards `tools/call` to the running app over a local control socket. The catalog is **~30 tools** — read tools (`get_status`, `list_terminals`, `read_terminal`, `supervision_tree`, `wait_for_status`, `scribe_status`, …) open, organization tools (`new_tab`/`focus_tab`/`rename_tab`, `create_worktree`/`remove_worktree`, `claim_captain`/`watch_fleet`, …) audited, and process-changing tools (`spawn_terminal`, `send_text`/`send_keys`, `close_terminal`, …) confirmation-gated.
- **~58 Tauri commands** across ~a dozen backend modules, plus a **side-by-side DEV build** (`com.t-hub.dev`, isolated `t-hub-dev` socket + `~/.t-hub-dev` state) installable alongside production — see [docs/DEV-BUILD.md](./docs/DEV-BUILD.md).
- **Tests:** Rust unit + MCP e2e suites on the backend, plus a **vitest** frontend harness (jsdom + RTL) — 300+ tests across stores, components, and pure-function libs (`pnpm test`).

## Repository layout

A **pnpm monorepo** (`pnpm-workspace.yaml`). The desktop app is the workspace package; `apps/site` is the npm-based marketing site, kept out of the pnpm workspace for now.

```
apps/
  desktop/                     The Tauri desktop app (the pnpm workspace package)
    src/                       React frontend (xterm tiles, auto-grid canvas, Zustand stores)
      ipc/types.ts             The IPC contract (commands + events) — single source of truth
      ipc/client.ts            Typed wrappers over Tauri invoke/listen
      components/              45 components (Terminal, Sidebar, UsageStrip, ThemeEditor,
                               CommandPalette, WorktreePrompt, WorktreesList, RecoveryReview, …)
      store/                  Zustand stores (workspace, settings, activity, supervision,
                               keybindings, rules, fileOpen, sessionContext, …)
      lib/                    Side-effect mounts + helpers (commands · chord · keymapExecutor ·
                               prefixKeyHandler · notify · rulesMount · worktreeTarget · recentRepos · …)
    src-tauri/                 Rust/Tauri backend (~58 commands across these modules)
      src/commands.rs          0.1 terminal-nucleus commands (mirrors ipc/types.ts)
      src/commands_05.rs       Agent-bridge / supervision / status / hooks commands
      src/tmux.rs              `tmux -L t-hub` wrappers (isolated socket; `wsl.exe -e bash`)
      src/pty.rs               portable-pty ↔ tmux-attach bridge
      src/git.rs               git info/commit + worktree list/add/remove commands
      src/agent/               core↔agent transport + journal spine
      src/claude/              hook install + startup self-heal (managed-marker reconcile)
      src/{codex,usage,recent,supervision,files,db,devserver,control,theme,diag,…}.rs   feature modules
      crates/                  t-hub-agent (statusline/hook sidecar) · t-hub-mcp (MCP server) · t-hub-protocol
  site/                        Marketing site (npm-based; its own package-lock.json)
docs/                          PLAN · HANDOFF · DEV-BUILD · MCP · WORKTREE-WORKFLOW · SESSION_AWARENESS · AUDIT · …
PRD.md                         Product Requirements Document v1.0
REVIEW.md                      Technical review of the PRD + verified Claude Code facts
pnpm-workspace.yaml            Workspace manifest (lists apps/desktop)
```

## Build & run

**Prerequisites (all platforms):** [Node](https://nodejs.org) ≥ 20 + [pnpm](https://pnpm.io), the [Rust toolchain](https://rustup.rs), and `tmux` available in the target WSL distro (`sudo apt install tmux`). Then `pnpm install`.

### Windows 11 (primary target)
1. Install the Rust **MSVC** toolchain, the [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-studio-build-tools/), and the WebView2 runtime (preinstalled on Win11).
2. Ensure your WSL distro has `tmux` (the app reaches in via `wsl.exe`).
3. `pnpm install` then `pnpm tauri dev` (or `pnpm tauri build` for an installer).

### Inside WSL2 via WSLg (Linux dev build)
Uses the `cfg(unix)` path (attaches to tmux directly — no `wsl.exe`). Install the Tauri Linux system deps, e.g. on Ubuntu:
```
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```
Then `pnpm tauri dev` (a window opens through WSLg).

### Frontend only (no Rust)
`pnpm dev` (Vite dev server) or `pnpm build`. The terminal backend commands are unavailable without the Tauri host, but the UI shell renders.

## Roadmap & docs

- **[docs/PRODUCTION-READINESS.md](./docs/PRODUCTION-READINESS.md)** - the active stabilization program, CI target, security workstreams, and measurable Alpha/Beta/Stable release gates.
- **[docs/PLAN.md](./docs/PLAN.md)** — the original phased plan (0.5 → 2.0). Most of the 0.5 supervision track has since shipped; kept as the design-rationale record.
- **[PRD.md](./PRD.md)** — full product spec. **[REVIEW.md](./REVIEW.md)** — review + the verified Claude Code integration facts (hooks, sessions, statusline, SDK) the plan relies on.
