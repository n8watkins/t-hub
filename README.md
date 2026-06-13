# TermHub

TermHub is a **terminal-first command center for running and supervising many persistent coding-agent (Claude Code) sessions at once**. The V1 target is a single personal setup: Windows 11 + WSL2 Ubuntu + zsh, with an adapter-based core so other terminal agents can be added later.

## Status — 0.1 "playable proof" nucleus (scaffolded)

The terminal nucleus is in place and compiles end to end:

- **Tauri 2 + React 18 + TypeScript + Tailwind** desktop shell.
- **xterm.js** terminal tiles (Fit + WebGL + Search + Unicode 11) in a responsive auto-grid with deterministic insertion, focus, and layout persistence.
- **Rust PTY ↔ tmux backend:** `portable-pty` (ConPTY on Windows) drives a `tmux -L termhub` session per terminal — one PTY client per visible tile. Closing a tile **detaches** (the process survives); stop **kills** the session.
- **Platform-abstracted attach:** `#[cfg(windows)]` spawns `wsl.exe … tmux attach`; `#[cfg(unix)]` attaches to tmux directly (so the nucleus is exercisable inside WSL today).

**Verified:** `pnpm typecheck` + `vite build` pass; `cargo check` passes in-tree against real Tauri 2.11 (Linux + `x86_64-pc-windows-msvc`); the spawn → stream → resize → **detach-survives** → reattach-with-scrollback → kill cycle was runtime-tested against real `tmux`.

## Repository layout

```
src/                 React frontend (xterm tiles, auto-grid canvas, Zustand store)
  ipc/types.ts       The IPC contract (commands + events) — single source of truth
  ipc/client.ts      Typed wrappers over Tauri invoke/listen
src-tauri/           Rust/Tauri backend
  src/commands.rs    The 7 Tauri commands (mirrors ipc/types.ts)
  src/pty.rs         portable-pty ↔ tmux-attach bridge
  src/tmux.rs        `tmux -L termhub` process wrappers (isolated socket)
  src/events.rs      terminal://output | state | exit payloads
docs/PLAN.md         Forward build plan (0.5 → 2.0)
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

- **[docs/PLAN.md](./docs/PLAN.md)** — phased plan from 0.5 (personal alpha; parallel-agent supervision lands here) through 2.0.
- **[PRD.md](./PRD.md)** — full product spec. **[REVIEW.md](./REVIEW.md)** — review + the verified Claude Code integration facts (hooks, sessions, statusline, SDK) the plan relies on.
