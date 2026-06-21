# Lane A — Tile Identity (agent kickoff brief)

**You are the agent for this lane.** Read this top to bottom, then execute. Plan
context: [../FEATURE-PLAN.md](../FEATURE-PLAN.md). You work in the worktree
`…/wt-tile-identity` on branch **`feat/tile-identity`**, branched off `main`.
Another agent runs **Lane B** (`feat/sidebar-strips`) in parallel — stay in YOUR
files so the two never conflict.

## What T-Hub is
A Tauri 2 + React 18 + TS + Tailwind + Zustand desktop app — a cockpit of xterm
terminal tiles (each running Claude Code / Codex / a shell) over a Rust PTY↔tmux
engine. Frontend lives in `apps/desktop/src`. A dev instance may be running off the
MAIN checkout (not this worktree); verify your work here with typecheck.

## Your files — own these; do NOT touch anything else
- `apps/desktop/src/components/Tile.tsx`
- `apps/desktop/src/components/ClaudeIcon.tsx` + new `apps/desktop/src/components/CodexIcon.tsx`
- `apps/desktop/src/store/theme.ts` (tile-color bits only)
- new `apps/desktop/src/store/clientType.ts` (A3)
- `apps/desktop/src/assets/` (Codex image)

**Do NOT touch (other lanes own these):** `Sidebar.tsx`, `WslHealth.tsx`,
`UsageStrip.tsx`, `WorkspacesList.tsx`, `store/workspace.ts`, `Terminal.tsx`,
anything under `src-tauri/`.

## Setup
```bash
cd <this worktree>
pnpm install                       # if node_modules is missing (fast; hard-linked)
pnpm --filter t-hub-desktop typecheck    # baseline — should already pass
```

## Tasks

### A1 — Replace the "Claude" text with the icon · S
In `Tile.tsx`, the header shows a "Claude" text chip beside `<ClaudeIcon>`. Drop
the WORD, keep just the icon (`title="Claude"` for the tooltip). `ClaudeIcon`
already exists at `components/ClaudeIcon.tsx`.

### A2 — Codex icon · S
The blue Codex glyph is at
`/mnt/c/Users/natha/Downloads/codex-img-removebg-preview.png`. Bring it into the
app — either copy it to `apps/desktop/src/assets/codex.png` and render it, or trace
it into a `CodexIcon.tsx` (mirror `ClaudeIcon.tsx`). Show it for Codex tiles (see
A3). It's a blue/purple gradient `>_` mark.

### A3 — Detect the client per tile · M ← shared contract, do carefully
Create `apps/desktop/src/store/clientType.ts` exporting:
```ts
export function clientForTerminal(id: TerminalId): "claude" | "codex" | "shell"
```
Infer from the tile's spawn command / title (the `TerminalInfo` in the workspace
store carries command/title — inspect it). Heuristic: contains "claude" → claude;
"codex" → codex; else shell. Keep it pure + synchronous (read the workspace store).
`Tile.tsx` uses it to choose the Claude vs Codex icon (A1/A2). **Keep the signature
stable** — Wave 3 (Codex usage, auto-resume) imports it.

### D1 — Workspace color cascades to ALL tiles · S–M (bug)
Today a workspace color only tints the tile FOCUS RING, and not reliably. Make the
tile chrome (header background / border / accent) of EVERY tile in a workspace
derive from that workspace's color, updating live when it changes. Workspace colors
live in `theme.ts` `workspaceColors` (keyed by tabId); a tile finds its tab via the
workspace store (read-only). Priority: per-terminal override > workspace color >
default.

## Verify · commit · land on main
1. `pnpm --filter t-hub-desktop typecheck` passes.
2. Review your diff — ONLY your files changed.
3. Commit in logical chunks; end messages with
   `Co-Authored-By: Claude <noreply@anthropic.com>`.
4. Land it: `git fetch origin && git rebase origin/main` (clean — disjoint from
   Lane B), re-run typecheck, then `git push origin HEAD:main`.

## Done when
Tiles show the right client icon (Claude/Codex) with no "Claude" text; changing a
workspace color recolors every tile in it; `clientForTerminal()` exported + stable;
typecheck green; pushed to `main`.
