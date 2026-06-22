# T-Hub

T-Hub is a **terminal-first command center for running and supervising many persistent coding-agent (Claude Code) sessions at once**. The V1 target is a single personal setup: Windows 11 + WSL2 Ubuntu + zsh, with an adapter-based core so other terminal agents can be added later.

## Status — feature-complete personal alpha (v0.1.67)

Well past the original "playable proof" nucleus: the terminal spine, the agent-supervision layer (the 0.5 plan), Codex support, and the full Windows/WSL integration all ship in the installed app.

- **Tauri 2 + React 18 + TypeScript + Tailwind** desktop shell with an xterm.js tile grid (Fit + WebGL + Search + Unicode 11), deterministic insertion/focus, and durable layout/workspace persistence.
- **Rust PTY ↔ tmux backend:** `portable-pty` (ConPTY on Windows) drives a `tmux -L t-hub` session per terminal — one PTY client per visible tile. Closing a tile **detaches** (the process survives); stop **kills** the session. `#[cfg(windows)]` reaches into WSL via `wsl.exe -e bash` (the `-e`/`--exec` is load-bearing — `wsl.exe -- bash` runs the user's *login* shell, e.g. zsh); `#[cfg(unix)]` attaches to tmux directly.
- **Agent supervision:** a `t-hub-agent` sidecar + Claude Code hooks feed a journal/statusline spine — context + cost readout, autocontinue, supervision tree. Hooks install consent-gated from **Settings → Hooks** and self-heal on startup.
- **Codex + Claude:** per-provider usage readouts, icons, and running-pulse activity in the sidebar.
- **MCP control channel:** the `t-hub-mcp` server forwards `tools/call` to the running app over a local control socket (read tools open; mutations gated).
- **~47 Tauri commands** across a dozen backend modules, plus a **side-by-side DEV build** (`com.t-hub.dev`, isolated `t-hub-dev` socket + `~/.t-hub-dev` state) installable alongside production — see [docs/DEV-BUILD.md](./docs/DEV-BUILD.md).

## Repository layout

```
src/                 React frontend (xterm tiles, auto-grid canvas, Zustand stores)
  ipc/types.ts       The IPC contract (commands + events) — single source of truth
  ipc/client.ts      Typed wrappers over Tauri invoke/listen
  components/         ~29 components (Terminal, Sidebar, UsageStrip, ThemeEditor, …)
  store/             Zustand stores (workspace, settings, activity, supervision, …)
src-tauri/           Rust/Tauri backend (~47 commands across these modules)
  src/commands.rs    0.1 terminal-nucleus commands (mirrors ipc/types.ts)
  src/commands_05.rs Agent-bridge / supervision / status / hooks commands
  src/tmux.rs        `tmux -L t-hub` wrappers (isolated socket; `wsl.exe -e bash`)
  src/pty.rs         portable-pty ↔ tmux-attach bridge
  src/agent/         core↔agent transport + journal spine
  src/claude/        hook install + startup self-heal (managed-marker reconcile)
  src/{codex,usage,recent,supervision,files,git,control,theme,diag}.rs   feature modules
  crates/            t-hub-agent (statusline/hook sidecar) · t-hub-mcp (MCP server) · t-hub-protocol
docs/                PLAN · HANDOFF · DEV-BUILD · MCP · SESSION_AWARENESS · AUDIT · …
PRD.md               Product Requirements Document v1.0
REVIEW.md            Technical review of the PRD + verified Claude Code facts
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

- **[docs/PLAN.md](./docs/PLAN.md)** — the original phased plan (0.5 → 2.0). Most of the 0.5 supervision track has since shipped; kept as the design-rationale record.
- **[PRD.md](./PRD.md)** — full product spec. **[REVIEW.md](./REVIEW.md)** — review + the verified Claude Code integration facts (hooks, sessions, statusline, SDK) the plan relies on.
