# T-Hub — Worktree-Centric Workflow (locked design)

**Status:** Locked design. Builds on the shipped WS-4 (worktree primitive) and WS-3 (rebindable keymap). Implementation tracked as **WS-9** in [ROADMAP-PLAN.md](./ROADMAP-PLAN.md).

## The model (one sentence)

A **worktree is just a separate folder for one branch of a repo.** To make one, T-Hub needs:
- **which branch?** → you type it.
- **which repo?** → the repo of the terminal you're focused on.
- **where?** → a *sibling* folder next to that repo: `<repo>-worktrees/<branch>`.

The repo comes from **where you already are** — never guessed.

### Worked example
You're focused on a terminal in `~/projects/foo` (a repo). You trigger "new worktree" and type `login-fix` → T-Hub creates `~/projects/foo-worktrees/login-fix` and opens a new tab with a terminal already inside it.

## Anchoring rules

- **Repo = the focused tile's repo**, resolved to the **main repo root** (via `git rev-parse --git-common-dir` / the existing `GitInfo.worktree_root` + `is_linked_worktree`). So creating a worktree *from inside a worktree* still lands as a sibling of the **main** checkout — `foo-worktrees/feature-b` — never nested.
- **Sibling, never inside.** The folder is next to `foo`, never inside `foo` and never at `$HOME`/`/`.
- **No repo in context → ask, don't guess.** If the focused tile is a `~` scratch shell (or the workspace is empty), pop a **repo picker** instead of inventing a path. The picker list is built from data we already have: recent-session cwds (`~/.claude/projects/**` → dedupe to repo roots) + any open tile's repo.

## Path convention (locked)

**Sibling:** `<parent-of-repo>/<repo>-worktrees/<branch-sanitized>` (e.g. `feature/x` → dir `feature-x`). The `<repo>-worktrees/` base is created on demand and is configurable later; sibling is the default because it keeps the main checkout clean and `git worktree` is happy with paths outside the main tree.

## The three "new" actions + binds (locked)

Split by **layer** — a *terminal* (a tile) vs a *workspace* (a tab). The frequent, dumb action stays a direct hotkey; the deliberate "start something new" actions live behind the `Ctrl+B` prefix. All are rebindable (WS-3 keymap); these are defaults.

| Key | Creates | Worktree? |
|---|---|---|
| **`Ctrl+T`** (direct) | One more **terminal in the current tab**, inheriting the focused tile's cwd | **No** — just a shell |
| **`Ctrl+B c`** (prefix) | A **new plain tab** (workspace), no repo binding | No |
| **`Ctrl+B w`** (prefix) | A **new tab that is a git worktree** — branch the focused repo, sibling folder, tile born inside it | Yes |
| **`Ctrl+K`** | **Command palette** — find/run any action by name (type "worktree", Enter) | — |

**Principle:** `Ctrl+T` is the frequent, surprise-free one — it adds a shell next to what you're doing and **never** creates a worktree. If you're in the `login-fix` worktree, `Ctrl+T` gives a second shell *in that same worktree* (more hands on the task, not a new one). The two deliberate "new tab" actions (`c` plain, `w` worktree) sit behind the prefix because they're intentional and less frequent.

> One-line mental model: **`Ctrl+T` = one more terminal here · `Ctrl+B w` = start a task (worktree) · `Ctrl+B c` = blank workspace.**

## Lifecycle (so it feels safe)

- **Closing a worktree workspace = detach** its tiles (tmux stays alive); the worktree folder **stays on disk** — your branch/work persists.
- **Removing a worktree** will be a separate, explicit action that deletes the folder only after the unified status service authorizes it.
- Source `0.3.87` temporarily suspends removal before UI detachment or Git mutation, and `force` cannot bypass that gate.
- **Re-open** an existing worktree into a new workspace from a **worktrees list** (`git_worktree_list`). Worktrees are durable, not throwaway.

## The one enabling primitive

Everything above needs **the tile to know its live cwd** — from `tmux display -p '#{pane_current_path}'` — so T-Hub can `git_info` it to find the repo (and offer "new worktree" only when you're in one). This is the **same** primitive that unblocks relative-path file-open (the WS-1 TODO). Wire live cwd into the tile once → both features light up.

## Implementation sketch (WS-9)

1. **Live cwd per tile** — backend reads `#{pane_current_path}` for each tile; expose to the frontend (a field on the terminal state). *Unlocks this + relative file-open.*
2. **Anchor resolution** — given a tile cwd, resolve repo main-root via `git_info` (already have the fields); derive the sibling worktree path from a branch name.
3. **Actions + binds** — register `newWorktreeWorkspace` (`Ctrl+B w`) and `newPlainWorkspace` (`Ctrl+B c`) in the keymap registry (WS-3); `Ctrl+T` stays the plain terminal spawn. All in the palette.
4. **Repo picker fallback** — a small picker fed by recent-session cwds + open-tile repos, shown when there's no repo in context.
5. **Worktrees list** — surface `git_worktree_list` so existing worktrees can be re-opened.
