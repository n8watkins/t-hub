# T-Hub — Active Feature Plan

Features we are building **now** — split into parallel **worktree lanes** so
multiple terminal sessions can drive them at once off `main`. This is NOT a
someday list: parked/abandoned items live in [BACKBURNER.md](./BACKBURNER.md)
(only the Win11 snap-flyout + the localhost iframe preview); the long-horizon
architecture is [PLAN.md](./PLAN.md).

## How we parallelize
- **One lane = one git worktree = one terminal session** (its own Claude agent),
  working off `main`. Lanes own **disjoint files** so they never conflict.
- Each lane **pushes to `main`** when its slice is green; the pushing agent
  reviews its own diff and pushes only what belongs to its lane.
- **Subagents are fine WITHIN a lane** (fan-out reads/edits). Cross-lane
  parallelism is the *worktrees*, not the Agent tool's worktree isolation (broken
  in this env — use real `git worktree add`).
- The **command registry** (`lib.rs` invoke list, `types.ts` `Commands05`,
  `client05.ts`) is edited by **only Lane C**, so no two lanes touch it.

Sizes: **S** small · **M** multi-file · **L** subsystem / needs a spike.

---

## Wave 1 — three independent lanes (parallel NOW)

### Lane A — `feat/tile-identity`
**Owns:** `Tile.tsx`, `ClaudeIcon.tsx`, new `CodexIcon.tsx` / `assets/`, `theme.ts` (tile-color bits).
- **A1** Replace the "Claude" *text* with the `ClaudeIcon` (keep the tooltip). **S**
- **A2** Add the Codex icon — copy `…/Downloads/codex-img-removebg-preview.png` into
  the app, show it for Codex tiles. **S**
- **A3** Detect the client per tile — **claude / codex / shell** — from the tile's
  spawn command/title (frontend-only; no backend). Export
  `clientForTerminal(id) → "claude" | "codex" | "shell"`. **M** ← *shared contract
  Wave 2 (B3, E1) consumes.*
- **D1** Workspace color cascades to **all** tiles in the workspace (header/border/
  accent, not just the focus ring) and updates live. **S–M (bug)**
- **Do NOT touch:** Sidebar/UsageStrip/WslHealth/WorkspacesList (Lane B), Terminal /
  backend commands (Lane C).

### Lane B — `feat/sidebar-strips`
**Owns:** `Sidebar.tsx`, `WslHealth.tsx`, `UsageStrip.tsx`, `WorkspacesList.tsx`.
- **B1** Compact collapsed **WSL**: keep a one-line summary (RAM, maybe CPU%) in the
  collapsed bar instead of nothing. **S–M**
- **B2** Collapsed **Usage** still shows basic numbers in the bar. **S–M**
- **D2** Drag a sidebar terminal row into another workspace → reuse `moveTileToTab`.
  **M**
- **Do NOT touch:** Tile.tsx / theme.ts (Lane A), Terminal / backend (Lane C).

### Lane C — `feat/terminal-input`
**Owns:** `Terminal.tsx`, backend drop/clipboard module, `commands_05.rs`, `lib.rs`,
`types.ts`, `client05.ts`.
- **C1** Drag a file/folder/image onto a tile → insert its path into the PTY,
  translating `C:\…`→`/mnt/c/…`, quoting spaces, supporting multiple. **M**
- **C2** Paste an image into the terminal → save to a temp file, insert the path
  (Claude reads image paths). **M–L**
- **Do NOT touch:** Tile.tsx (Lane A), Sidebar/etc. (Lane B).

> Lanes A/B are frontend-only; Lane C owns the command registry, so the three
> diffs don't overlap — merge order doesn't matter.

## Wave 2 — after Wave 1 merges (need Wave-1 outputs)

- **B3 · Codex usage + Claude/Codex split** (needs A3). **L.** Show Codex usage
  distinctly from Claude in the Usage strip, both with their icons.
  **Codex data reality (researched):** the Codex CLI exposes context-window + token
  counters via `/status` / `/statusline`, but the **5-hour / weekly rolling limits
  are NOT available via the CLI** for ChatGPT Plus (web-only —
  chatgpt.com/codex/settings/usage; open OpenAI issue #15281/#20310). So show Codex
  **context/tokens** (parity with Claude's context meter); treat rolling-window %
  as best-effort / possibly web-only.
- **E1 · Auto-resume on rate-limit** (touches Lane A's Tile + Lane C's Terminal + a
  backend timer). **L.** Per-tile toggle; detect the "limit reached — resets at
  <time>" output; resume when it passes. **Claude first, provider-agnostic.** Since
  all sessions share one account the reset time is effectively shared — support
  BOTH auto-detect (scan PTY output) AND a manual "auto-resume time" in terminal
  settings as override/fallback.

## Carried-over follow-ups
- **PR status on the git chip** (`gh pr list --head <branch>`). **M.** (Lane A area.)
- **Status off the durable journal** (non-cursor-advancing status channel; startup
  compaction is the stopgap). **L.** (agent crate.)
- **Detached, EDITABLE Files window.** **M.**
- **Page-up copy-mode polish** if the feel is off (pending live test). **S.**
