# T-Hub — Active Feature Plan

Features we are building **now** — split into parallel **worktree lanes** so
multiple terminal sessions can drive them at once off `main`. This is NOT a
someday list: parked/abandoned items live in [BACKBURNER.md](./BACKBURNER.md)
(only the Win11 snap-flyout + the localhost iframe preview); the long-horizon
architecture is [PLAN.md](./PLAN.md).

## How we parallelize
- **One lane = one git worktree = one terminal session** (its own Claude agent),
  off `main`. Lanes own **disjoint files** so they never conflict.
- Each lane **pushes its own slice to `main`** when green; the pushing agent
  reviews its diff and pushes only what belongs to its lane.
- **Subagents are fine WITHIN a lane.** Cross-lane parallelism is the *worktrees*,
  not the Agent tool's worktree isolation (broken here — use real `git worktree`).
- The **command registry** (`lib.rs` invoke list, `Commands05`, `client05.ts`) and
  the **Rust backend** are touched only by **Lane C** — so the parallel frontend
  lanes never need a `cargo` build (just `pnpm install` + typecheck).

Sizes: **S** small · **M** multi-file · **L** subsystem / needs a spike.

---

## Wave 1 — TWO parallel lanes (run now; frontend-only, cheap setup)

### Lane A — `feat/tile-identity`
**Owns:** `Tile.tsx`, `ClaudeIcon.tsx`, new `CodexIcon.tsx`/`assets/`, `theme.ts`
(tile-color bits), new `store/clientType.ts`.
- **A1** Replace the "Claude" *text* with the `ClaudeIcon` (keep tooltip). **S**
- **A2** Codex icon — copy `…/Downloads/codex-img-removebg-preview.png` into the app,
  show it for Codex tiles. **S**
- **A3** Detect the client per tile — **claude / codex / shell** — from the tile's
  spawn command/title (frontend-only). Export `clientForTerminal(id)` from a NEW
  file `store/clientType.ts`. **M** ← *shared contract Wave 3 (B3, E1) consumes.*
- **D1** Workspace color cascades to **all** tiles in the workspace (header/border/
  accent, not just the focus ring), updating live. **S–M (bug)**
- **Avoid:** `workspace.ts` (Lane B's if needed), Sidebar/UsageStrip/etc. (Lane B),
  Terminal/backend (Lane C).

### Lane B — `feat/sidebar-strips`
**Owns:** `Sidebar.tsx`, `WslHealth.tsx`, `UsageStrip.tsx`, `WorkspacesList.tsx`,
and `workspace.ts` (only if D2 needs a store tweak).
- **B1** Compact collapsed **WSL**: a one-line summary (RAM, maybe CPU%) in the
  collapsed bar instead of nothing. **S–M**
- **B2** Collapsed **Usage** still shows basic numbers in the bar. **S–M**
- **D2** Drag a sidebar terminal row into another workspace → reuse `moveTileToTab`.
  **M**
- **Avoid:** `Tile.tsx`/`theme.ts` (Lane A), Terminal/backend (Lane C).

> A and B share **zero files** (A reads the workspace store but only B may edit it;
> A owns `theme.ts`, B owns the sidebar components). Either can push first; the
> other rebases cleanly.

## Wave 2 — Lane C (after Lane A merges)

### Lane C — `feat/terminal-input`
**Owns:** `Terminal.tsx`, a backend drop/clipboard module, `commands_05.rs`,
`lib.rs`, `types.ts`, `client05.ts`.
- **C1** Drag a file/folder/image onto a tile → insert its path into the PTY
  (`C:\…`→`/mnt/c/…`, quote spaces, multiple). **M**
- **C2** Paste an image → save to a temp file, insert the path (Claude reads image
  paths). **M–L**
- **Deferred until A merges** because the drop/paste target lives on the tile and
  would otherwise contend with Lane A's `Tile.tsx` rework. Needs a one-time `cargo`
  build (backend lane).

## Wave 3 — cross-cutting (after their deps land)

- **B3 · Codex usage + Claude/Codex split** (needs A3 + B's UsageStrip). **L.**
  Show Codex usage beside Claude, each with its icon. **Corrected data reality:**
  Codex `/status` AND a configurable `/statusline` expose **context + 5-hour +
  weekly** limits as percentages (opt-in `five-hour-limit`/`weekly-limit` items) —
  full parity with Claude, and a clean statusline tap-point analogous to Claude's.
- **E1 · Auto-resume on rate-limit** (touches Tile [A] + Terminal [C] + a backend
  timer). **L.** Per-tile toggle; detect "limit reached — resets at <time>"; resume
  when it passes. Claude-first, provider-agnostic. Support BOTH auto-detect AND a
  manual "auto-resume time" terminal setting (all sessions share one account, so
  the reset time is effectively shared).

## Carried-over follow-ups
- **PR status on the git chip** (`gh pr list --head <branch>`). **M.** (Lane A area.)
- **Status off the durable journal** (non-cursor-advancing status channel). **L.**
- **Detached, EDITABLE Files window.** **M.**
- **Page-up copy-mode polish** if the feel is off (pending live test). **S.**

---

## Plan review notes (decisions)
- **2 parallel lanes, not 3:** Lane C (terminal-input) is deferred until Lane A
  merges — its drop/paste target sits on the tile and would contend with Lane A's
  `Tile.tsx` rework. Slower, but conflict-free, which is the priority.
- **`workspace.ts` ownership:** assigned to Lane B (D2). Lane A's D1 stays in
  `theme.ts` + `Tile.tsx` and must not edit `workspace.ts`.
- **A3 in its own file** (`store/clientType.ts`) so neither lane edits a shared
  store file for the client-detection contract.
- **Codex usage is full-parity** (corrected) — B3 isn't degraded to tokens-only.
