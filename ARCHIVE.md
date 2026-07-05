# T-Hub Native (GPUI) - Archived 2026-07-05

This is the extracted history of the T-Hub native cockpit pivot: a GPUI (Zed's UI framework) rewrite of the T-Hub terminal-hub frontend, talking to the existing Rust server over a loopback control socket.
The pivot was paused by decision on 2026-07-05: the remaining work (installer, updater, tray, settings/theming polish) was judged too much for efficiency gains that were hard to quantify day-to-day.
The webview (Tauri) frontend remains the shipping product.

## State at archive time

Functionally flip-ready as a daily cockpit; distribution never built.

- Full terminal emulation via alacritty_terminal: truecolor, mouse reporting, selection, search, links, ligatures, procedural box-drawing/Powerline sprites.
- Measured: ~180 fps with 12 live tiles at 4K, damage-clipped painting ~18% CPU win over full repaint, binary PTY framing ~27% wire reduction.
- Cockpit chrome: workspace tabs, auto-grid + adjustable ratios + drag-reorder + fullscreen, full tile header (git branch, worktree badge, dirty dot, context meter, editable work names), kill-with-confirm, supervision cues.
- Panels (Files/Preview/Dev-runner) mounted as a toggleable side surface (prefix+f).
- Command palette, prefix keymap (Ctrl+B leader), rebindable action registry.
- Multi-window satellites over one shared connection and attach pool.
- Sounds/OS notifications, resume flow, single-instance guard.
- 313 headless tests green at archive time.

## What was never built

Installer/updater/tray (was gated on the server split), settings UI, theming, rules engine, crash-recovery review.
See docs/NATIVE-FINISH-PLAN.md lanes P and D for the exact remaining plan.

## Docs

The docs/ directory carries the pivot documentation extracted from the main repo: execution guide with per-task results log, parity audit (77-row matrix vs the webview), render-pivot design, finish plan, GPUI spike results, font catalogue, and screenshots.

## Provenance

Extracted from github.com/n8watkins/t-hub via git subtree split of apps/native (34 commits).
The same history exists in the main repo as branch native-archive and tag native-pivot-final.
The server-side counterpart (control socket, PTY protocol v2, tmux orchestration) lives on in the main repo - it is shared infrastructure, not native-only.

## Reviving

Build: cargo build --release in this repo's root (Windows MSVC or WSL with the linux gpui backend).
Run: the binary connects to a running T-Hub app's control socket (handshake at ~/.t-hub/control.json).
Start with docs/NATIVE-PIVOT-EXECUTION.md section 5 (results log) - it records every task's deviations and gotchas.
