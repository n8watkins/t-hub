# Lane B — Sidebar Strips (agent kickoff brief)

**You are the agent for this lane.** Read top to bottom, then execute. Plan
context: [../FEATURE-PLAN.md](../FEATURE-PLAN.md). You work in the worktree
`…/wt-sidebar-strips` on branch **`feat/sidebar-strips`**, branched off `main`.
Another agent runs **Lane A** (`feat/tile-identity`) in parallel — stay in YOUR
files so the two never conflict.

## What TermHub is
A Tauri 2 + React 18 + TS + Tailwind + Zustand desktop app — a cockpit of xterm
terminal tiles over a Rust PTY↔tmux engine. Frontend lives in `apps/desktop/src`.
The left **sidebar** has: a **Workspaces** list (each workspace over its terminals),
a **Recent** list, a bottom-pinned **Usage** strip (Claude cost/context/limits),
and a bottom **WSL** health strip. A dev instance may be running off the MAIN
checkout (not this worktree); verify here with typecheck.

## Your files — own these; do NOT touch anything else
- `apps/desktop/src/components/Sidebar.tsx`
- `apps/desktop/src/components/WslHealth.tsx`
- `apps/desktop/src/components/UsageStrip.tsx`
- `apps/desktop/src/components/WorkspacesList.tsx`
- `apps/desktop/src/store/workspace.ts` (only if D2 needs a store tweak)

**Do NOT touch (other lanes own these):** `Tile.tsx`, `store/theme.ts`,
`ClaudeIcon.tsx`/`CodexIcon.tsx`, `Terminal.tsx`, anything under `src-tauri/`.

## Setup
```bash
cd <this worktree>
pnpm install
pnpm --filter termhub typecheck    # baseline — should already pass
```

## Tasks

### B1 — Compact collapsed WSL · S–M
In `Sidebar.tsx`, the bottom **WSL** strip (`BottomStatus`) collapses to nothing
useful. When collapsed, keep a one-line summary visible IN the collapsed bar — at
least RAM (used/total), ideally a CPU% too — using the same `metrics` it already
reads (`HostMetrics`). Expanded behavior (`WslHealth.tsx`) stays.

### B2 — Collapsed Usage still shows numbers · S–M
The bottom **Usage** strip (`UsageSection` in `Sidebar.tsx` → `UsageStrip.tsx`)
should, when collapsed, still show basic numbers (e.g. the 5-hour / weekly / context
percentages) in the bar rather than nothing/just bars. Keep the expanded full view.

### D2 — Drag a sidebar terminal into another workspace · M
In `WorkspacesList.tsx`, the nested terminal rows (`TerminalRow`) should be drag
SOURCES; dropping one on another workspace row moves that terminal to that
workspace by calling the existing `moveTileToTab(terminalId, tabId)` (already in the
workspace store — used by the tile-header drag). Use pointer-based drag (see
`src/lib/pointerDrag.ts`; HTML5 DnD dies over xterm). Give clear drop affordance on
the target workspace row.

## Verify · commit · land on main
1. `pnpm --filter termhub typecheck` passes.
2. Review your diff — ONLY your files changed.
3. Commit in logical chunks; end messages with
   `Co-Authored-By: Claude <noreply@anthropic.com>`.
4. Land it: `git fetch origin && git rebase origin/main` (clean — disjoint from
   Lane A), re-run typecheck, then `git push origin HEAD:main`.

## Done when
Collapsed WSL shows live RAM (≈CPU); collapsed Usage shows the key percentages; a
sidebar terminal can be dragged into another workspace; typecheck green; pushed to
`main`.
