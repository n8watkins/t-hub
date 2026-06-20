# Lane C — Terminal Input (agent kickoff brief) · DEFERRED

> **DO NOT START until Lane A (`feat/tile-identity`) has merged to `main`.** This
> lane's drop/paste target lives on the tile and would contend with Lane A's
> `Tile.tsx` rework. Branch this worktree off `main` *after* Lane A lands, so you
> build on its `Tile.tsx`.

**You are the agent for this lane.** Plan context:
[../FEATURE-PLAN.md](../FEATURE-PLAN.md). Worktree `…/wt-terminal-input`, branch
**`feat/terminal-input`**. This lane touches the **Rust backend** + the command
registry, so it needs a one-time `cargo` build (the frontend lanes don't).

## What TermHub is
A Tauri 2 + React + Rust app; each tile is an xterm attached via PTY → `tmux` →
the agent. Keystrokes/paste flow `xterm.onData → writeTerminal(id, data)` (see
`apps/desktop/src/components/Terminal.tsx`). New Tauri commands are added in
`src-tauri/src/commands_05.rs`, registered in `src-tauri/src/lib.rs`'s
`invoke_handler`, and mirrored in `src/ipc/types.ts` (`Commands05`) +
`src/ipc/client05.ts`.

## Your files — own these
- `apps/desktop/src/components/Terminal.tsx`
- a new backend module for path-translate / clipboard-image (e.g. `src-tauri/src/dropin.rs`)
- `src-tauri/src/commands_05.rs`, `src-tauri/src/lib.rs`
- `src/ipc/types.ts` (`Commands05`), `src/ipc/client05.ts`

**Do NOT touch:** `Tile.tsx`, sidebar components, `theme.ts`, `workspace.ts`.

## Setup
```bash
cd <this worktree>
pnpm install
pnpm --filter termhub typecheck
( cd apps/desktop/src-tauri && cargo check -p termhub )   # one-time build
```

## Tasks

### C1 — Drag a file/folder/image onto a tile → insert its path · M
Wire the Tauri webview file-drop (the app gets a drop event with native paths).
On drop over a tile, write the path(s) into that tile's PTY via
`writeTerminal(terminalId, text)`. Translate Windows paths `C:\Users\…` →
`/mnt/c/Users/…`, quote paths with spaces, join multiple with spaces, and add a
trailing space. (A small Rust helper can do the path translation if cleaner.)

### C2 — Paste an image into the terminal → temp file + path · M–L
On a clipboard paste that contains an IMAGE (not text), save the image to a temp
file under the project (or a temp dir), then insert that file's path into the PTY
(Claude/Codex read image paths). Needs clipboard-image read (a Rust command via
`tauri-plugin-clipboard-manager` or raw) + temp-file write. Normal text paste
already works — don't regress it.

## Verify · commit · land on main
1. `pnpm --filter termhub typecheck` + `cargo check -p termhub` pass.
2. Review your diff — ONLY your files changed.
3. Commit in logical chunks; `Co-Authored-By: Claude <noreply@anthropic.com>`.
4. Land: `git fetch origin && git rebase origin/main`, re-verify, `git push origin HEAD:main`.

## Done when
Dropping a file/folder onto a tile types its WSL path; pasting an image inserts a
temp-file path; text paste unaffected; typecheck + cargo check green; pushed to
`main`.
