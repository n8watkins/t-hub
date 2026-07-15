# T-Hub as a Worktree-First IDE

## 1. Thesis

A **worktree-first IDE** makes the git worktree — one branch, one fully isolated checkout coexisting on disk — the atomic unit of work. N tasks run as N parallel agents in N worktrees, with zero stash/checkout context-switching. The unit of parallelism stops being "a thread in my editor" and becomes "a task = a branch = a worktree = an agent."

T-Hub is uniquely positioned to be that IDE because it is already a **multi-session cockpit, not a single-document editor**. Every tile is a persistent, tmux-backed agent that can be born inside its own worktree, and T-Hub already supervises status, cost, and git state across all of them at once. The hard part of worktree-first — running many isolated checkouts in parallel and keeping a coherent view over them — is T-Hub's existing substrate, not a feature it has to bolt on. The worktree primitive itself is **shipped** (WS-4/WS-9); the remaining work is composition and UX, not a rebuild.

## 2. The Shift: why the agent era makes worktree-first the right model

Git worktrees let one repository have N working trees on disk simultaneously, each pinned to a different branch/commit, all sharing one object store. Branch B is a real directory you can build, test, and edit while branch A sits untouched in its own directory — no stash, no checkout, no clobbering uncommitted work.

In a single-agent world that was a niche convenience. In the agent era it becomes the **core primitive**: you want agent #1 grinding a refactor on `feat/auth`, agent #2 fixing a bug on `fix/race`, and agent #3 reviewing on `main` — *all at the same time*, each on its own branch, each with its own filesystem so their edits never collide and their `git status` never bleeds into each other.

Contrast the traditional single-checkout IDE (VS Code / Cursor): the editor is bound to **one** working directory on **one** branch. Parallel branch work means either:

- **(a) `git stash` / `git checkout` thrashing** — serializing what should be parallel, and risking lost or half-applied state; or
- **(b) N full clones / N editor windows** — each a heavyweight, separately-configured silo with no shared view across them.

Worktrees solve the disk-isolation problem cleanly. What's been missing is a cockpit that treats "many worktrees, many agents" as the *default shape of work* rather than an exotic git trick. That cockpit is the worktree-first IDE.

## 3. What T-Hub Already Does (shipped)

The worktree create, list, and reopen primitives are in place end-to-end.
Removal is temporarily suspended in source `0.3.88` pending the unified safety verdict.

**Core git surface (`git.rs`, registered in `lib.rs:474`).** Five Tauri async commands shell out via `run_git` (on Windows, through `wsl.exe -d <distro> --cd <cwd> -- git …` with `CREATE_NO_WINDOW`), each argument as its own argv entry so branch names with slashes and shell metacharacters are safe:

- **LIST** — `git_worktree_list` parses `git worktree list --porcelain` into `WorktreeInfo[] {path, branch, isLinked}`; the main worktree is returned first (`isLinked:false`), linked worktrees after; detached/bare entries carry `branch:null`.
- **CREATE with smart branch handling (WS-9)** — `git_worktree_add` runs `git show-ref --verify --quiet refs/heads/<b>` to decide the argv (`worktree_add_args`): existing local branch → bare checkout; non-existing branch → create-and-checkout (`-b`); no branch → git's path-derived default.
- **REMOVE** - `git_worktree_remove` synchronously refuses before Git while the unified worktree status service is unavailable.

**Anchor-to-main resolution (`worktreeTarget.ts`).** `resolveWorktreeTarget(cwd, branch)` calls `gitWorktreeList`, picks the single `isLinked===false` entry as the main root, and builds a **sibling** path `<parent-of-root>/<root-name>-worktrees/<sanitized-branch>`. Worktrees never nest — invoked from inside a linked worktree, it still anchors to the main checkout. No main entry → `{kind:'no-repo'}`, which pops the repo picker rather than guessing.

**The create flow (`Ctrl+B w`).** The tmux-style prefix handler in `Canvas.tsx` arms the prefix; `w` resolves to `newWorktreeWorkspace` → `keymapExecutor.doNewWorktreeWorkspace`, which captures the focused tile's **live** cwd and opens `WorktreePrompt`. On submit, `resolveWorktreeTarget` derives the sibling path, then `store.addWorktreeWorkspace(repoRoot, worktreePath, branch)` calls `gitWorktreeAdd`, makes and activates a fresh tab, renames it after the branch, `spawnTerminal({cwd: worktreePath})`, and focuses the tile — so you land in a new tab with a terminal **already inside the worktree dir on that branch**. Git failures surface inline so the prompt stays open to retry.

**The keybind / discovery workflow (`keybindings.ts`, `commands.ts`).** Rebindable, tmux-style prefix (default `Ctrl+B`): `Ctrl+B w` = new worktree workspace, `Ctrl+B c` = plain tab, `Ctrl+B l` = worktrees list, `Ctrl+T` = plain terminal in the current tile (deliberately *not* a worktree). All discoverable in the `Ctrl+K` palette.

**Multiple entry points, one creation path.** Beyond the keybind, `FilePanel` "New worktree…" offers a raw worktree-path field and optional branch, while the `WorktreesList` modal reopens existing worktrees.
The MCP and control channel exposes `create_worktree`, which runs Git and forwards the tab and spawn to the UI with `alreadyCreated:true`, so `store.addWorktreeWorkspace` remains the single creation path.
The `remove_worktree` tool remains discoverable but synchronously returns the temporary safety refusal.

**Current removal override.** Source `0.3.88` suspends every public worktree-removal entry point before UI detachment or Git mutation until the unified status service exists.
The creation and reopen paths remain available.

**Error handling is concrete.** "branch already checked out elsewhere" is detected (`already_checked_out_branch`) and surfaced with the branch named; a pre-existing target directory yields an actionable "remove that leftover directory or pick a different branch name."

**Safe lifecycle.** Source `0.3.88` runs an authoritative preflight before any detach and currently fails closed while the unified service is unavailable.
Closing a worktree tab still detaches its tiles, and the branch and work remain on disk for reopening.

**How it plugs into the cockpit.** Worktrees map cleanly onto T-Hub's model: one git worktree → one workspace **tab** containing a **tile** (terminal) whose cwd is the worktree dir, on its own branch. Each tile can run its own agent, so N feature branches = N concurrent agents, each isolated in its own checkout, all in one window. The per-tile git chip (`Tile.tsx`) and FilePanel badge show branch / linked-worktree tag / dirty count, making the cockpit a fleet view. The layer split is the core UX principle: `Ctrl+T` adds *more hands on the same task* (another terminal in the same worktree); `Ctrl+B w` starts a *new task* (a new worktree).

## 4. Why T-Hub Is the Natural Substrate

The architecture is already shaped like the worktree-first ideal. Three properties carry the argument:

1. **Many persistent, isolated sessions in one window.** T-Hub is a Tauri cockpit whose first-class object is the agent tile: each terminal is its own tmux session (`th_<terminalId>`), persistent across app restarts, detachable, with its own cwd. N agents already coexist in one grid — the multi-worktree shape needs **no new concurrency model**, just a worktree behind each tile.

2. **Each tile already lives in (and knows) its own worktree.** The primitive is shipped (see §3). Each tile reads its live cwd (`#{pane_current_path}`) and runs `git_info` on it, so the cockpit already knows, per tile, the branch, the worktree root, whether it's a linked worktree, and its dirty count (`GitInfo` in `git.rs`).

3. **Supervision and cost across all of them.** This is the differentiator a plain editor structurally can't match. T-Hub has a hook-derived supervision tree (FR-012 status model, `supervision.rs`) and per-session cost/context economics (`usage.rs`, `codex.rs`, statusline ingestion), surfaced as a shared status indicator over every tile — plus an MCP control channel (`t-hub-mcp`, `control.rs`) with `wait_for_status`, spawn, send, and a supervision-tree read. So "many agents, each in its own worktree" comes with a single pane of glass: which branch each agent is on, whether it's working / blocked / done, and how much it has spent — and a programmable surface to orchestrate them.

A traditional editor gives you one checkout and one cursor. T-Hub gives you a fleet view that is *already* branch-, status-, and cost-aware. That is precisely the substrate worktree-first needs and editors lack — which is why getting to fully worktree-first is a small step here, not a rewrite.

## 5. Gaps & What's Missing (honest accounting)

What is **not** there yet:

- **No worktree fleet launcher.** The marquee parked differentiator — "worktree fleet launcher with aggregate cost rollup" (`ROADMAP-PLAN.md:146`) — does not exist. Today worktrees are created one at a time; there is no batch/parallel launch and no per-fleet cost rollup. *(Aspirational.)*
- **No per-worktree status / merge / cleanup UX.** `WorktreesList` rows show only branch + path + main/linked tag — no per-row dirty count, ahead/behind, last-activity, or agent status; no merge-back-to-main; no bulk "prune merged/stale worktrees" (only one-at-a-time Remove with confirm).
- **The prompt creates silently.** `WorktreePrompt` doesn't preview the resolved sibling path or pre-check for a branch/dir collision; the user only learns of a collision from the inline error *after* submit.
- **`sanitizeBranchToDir` is many-to-one.** `feat/x` and `feat-x` collapse to the same dir `feat-x`. Git surfaces the "already exists" collision (no silent corruption), but there's no flatten-vs-nest disambiguation — a deferred WS-9 nit.
- **Remote-branch handling is `-b`-vs-DWIM naive.** `branch_exists` only checks local `refs/heads/<b>`, so a remote-only branch (`origin/x`) takes the `-b` create path (forking a fresh local branch) instead of DWIM-tracking `origin/x` — a deferred WS-9 nit.
- **Worktree lifecycle isn't on the durable timeline.** Journal events `WorktreeCreate` / `WorktreeRemove` are **reserved** (parseable in `hooks.rs`, present in `t-hub-protocol`) but **not emitted** by `worktree_add` / `worktree_remove`.
- **The agent protocol is asymmetric.** `AgentRequest::GitWorktrees` is **list-only**; add/remove are reachable only via the control channel — functional but inconsistent.
- **The `<repo>-worktrees/` base path is hardcoded** in `resolveWorktreeTarget` (design notes "configurable later").
- **Naming-convention mismatch.** `RecentList.tsx` expects a `wt-<branch>` directory convention, but the workflow creates `<repo>-worktrees/<branch-sanitized>` — so RecentList won't recognize this workflow's worktrees by the `wt-*` segment and falls back to the parent folder.
- **Worktree removal is temporarily unavailable.** Graphical, Tauri, control, MCP, and CLI callers receive the same fail-closed refusal while the unified status service is incomplete.

## 6. Roadmap to Fully Worktree-First

Prioritized, each step composing already-shipped primitives.

1. **Make "one task = one worktree = one agent" the default new-work gesture.** Today new work splits across `Ctrl+T` (shell here), `Ctrl+B c` (plain tab), and `Ctrl+B w` (worktree tab). Promote "new worktree + agent" to the primary "start something new" action and the empty-workspace call-to-action, so the UI *nudges* toward the worktree-first model (`commands.ts`, `keymapExecutor.ts`, `WorktreePrompt.tsx`).
2. **Build the worktree fleet launcher** (`ROADMAP-PLAN.md:146`). One action that spins K worktrees + K agents from a list of branches/task prompts — paste N branch names → N sibling worktrees → N tiles, each agent started in its own checkout. This is the headline gesture, and it composes the shipped `git_worktree_add` + tile-spawn primitives.
3. **Add per-worktree ahead/behind status.** `GitInfo` already carries branch + worktree_root + is_linked_worktree + dirty_count; extend it with ahead/behind vs upstream/base and surface a compact per-worktree badge (dirty / ahead / behind / clean) on every tile and in the worktrees list.
4. **Ship branch lifecycle, merge, and cleanup UX.** Implement the unified removal verdict first, then close the loop on reopen with "merge this worktree's branch back to main" and "task done, merge or open PR, remove the worktree, and reap the agent" as one safe gesture.
   Resolve the parked WS-9 `sanitizeBranchToDir` collision and remote-only branch tracking issues in the same tranche.
5. **Add aggregate cost rollup + a budget governor** (`ROADMAP-PLAN.md:146` and `:145`). Sum cost/context per worktree-agent into a fleet total, and pause/throttle/route work on spend or context budgets — turning the existing per-session economics (`usage.rs` / `codex.rs`) into fleet-level orchestration unique to a worktree-first cockpit.
6. **Make the supervision tree worktree-aware.** Group/filter the supervision view and status indicators by worktree/branch (`supervision.rs`, `StatusIndicator.tsx`), and expose the parked MCP supervision **event stream** so an orchestrator agent can block on "worktree X's agent is blocked/done" and drive the fleet programmatically (building on the shipped `wait_for_status`).
7. **Carry worktree-first over the server split** (`SERVER-SPLIT-AND-ROADMAP.md`, M1–M3 shipped, M4 next). Once the headless server-in-WSL owns the fleet, the worktree fleet plus its per-worktree git/cost/supervision state is reachable from any device, and named-session namespacing lets a whole fleet be addressed as one instance — "my whole worktree fleet, from my phone."

## 7. Competitive Framing

**vs VS Code / Cursor.** Single-checkout, single-branch editors. Parallel branch work forces stash/checkout serialization or N disconnected clones/windows — and even with "worktrees as folders" extensions, there is no unified view of N agents' status, branch, dirty state, and spend across checkouts. T-Hub inverts the default: the worktree is the unit, the grid of agents *is* the workspace, and the fleet view is built in. A worktree-first IDE is a category these editors can't enter without first becoming multi-session supervisors.

**vs plain terminal + git worktree + tmux.** This is the manual version of what T-Hub automates — and it's exactly what T-Hub is built on, so there's no impedance mismatch. By hand you script `git worktree add`, juggle tmux windows, and re-derive "which agent is blocked, which branch is dirty, what did this cost" every time. T-Hub keeps the tmux/worktree durability (sessions survive restarts) and adds the cockpit: one gesture to spin a worktree+agent, live per-worktree git state, the supervision tree, cost rollup, and OS notifications on blocked/done.

**vs other multi-agent tools (e.g. herdr and terminal-native orchestrators).** Per `HERDR-PARITY.md`, T-Hub has reached **parity** on worktrees, prefix-keymap/palette, rules engine, and durable restore — and **leads** on three things no competitor matches: cost/context economics, a *real* (hook-derived, not output-pattern-guessed) supervision tree, and the MCP control channel. Those three are what turn "I can launch many agents" into "I can supervise a worktree fleet." Branch-isolated parallelism is table stakes; knowing the live status, cost, and git state of each worktree-agent and orchestrating them programmatically is the moat — and the server split extends it to "reach my worktree fleet from any device," which no single-checkout editor offers.
