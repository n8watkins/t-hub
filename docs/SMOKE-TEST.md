# T-Hub Smoke Test — before cutting a release build

Covers everything changed this session (perf Tiers 1–3, Wave 0/1 features, WS-9 worktree workflow, pre-release wins). All is build- + unit-test-verified but **never run live** — this is that gate. Each item is **action → expected**; tick it or note the failure (with the commit tag for tracing).

**Run:** `cd apps/desktop && pnpm tauri dev` (use `! pnpm tauri dev` in the session to pipe logs here). Have a real repo + a Claude session handy.

---

## A. The freeze + perf (the headline — Tiers 1–3)
The whole reason for the perf work. Test under LOAD.

- [ ] **No freeze under many busy tiles.** Open **8+ terminals**, several running agents/streaming output (a build, `claude` doing tool calls, a `dev` server logging). Switch tabs, scroll, type. → **UI stays responsive; no multi-second hangs.** *(Tier 1: spawn_blocking pollers + git_info collapse + output coalescing)*
- [ ] **Many tiles in one repo don't spawn-storm.** Open several tiles all `cd`'d into the same git repo. → smooth; no periodic stutter every ~5s. *(git_info 6→1 wsl call + per-cwd cache, `8e12fbf`)*
- [ ] **Heavy output doesn't lock the window.** `cat` a large file / run a `--verbose` build in a tile. → output flows, window still interactive (the rest of Windows was always fine; now T-Hub is too). *(rAF output coalescing, `a15416f`)*
- [ ] **Final output lands before the exit banner.** Run a command that prints then exits (e.g. `echo done; exit`). → you see `done` **above** `[process exited]`, not after. *(exit-drain fix, `11158ae`)*
- [ ] **(Optional) RAM doesn't creep.** Spawn/close/resume many sessions over a while; watch `vmmemWSL` + the app's RAM in Task Manager. → bounded, not monotonic. *(Tier 2 evictions + cleanup)*

## B. WS-9 worktree workflow (newest + most complex)
- [ ] **Create a worktree.** Focus a tile inside a git repo → **`Ctrl+B` then `w`** → type a **new** branch name → Enter. → a new tab opens with a tile in `…/<repo>-worktrees/<branch>`, on that new branch (`git branch` confirms). *(`52a8ebd` + `-b` create `418433f`)*
- [ ] **Existing branch checks out** (not errors). `Ctrl+B w` → type an **existing** branch → it's checked out in the new worktree (no `-b` error).
- [ ] **From inside a worktree → siblings the MAIN repo.** In a worktree tile, `Ctrl+B w` again → new worktree lands as `…/<repo>-worktrees/<other>`, **not nested** under the current worktree. *(anchor-to-main fix, `24310a9`)*
- [ ] **Plain new tab.** `Ctrl+B` then `c` → fresh empty tab, no worktree.
- [ ] **Worktrees list.** `Ctrl+B` then `l` → modal lists the repo's worktrees (main/linked tags). **Open** re-opens one in a tab; **Remove** (linked only) confirms then deletes. *(`2fe1948`)*
- [ ] **Repo picker (no-repo).** `Ctrl+B w` from a non-repo tile (e.g. `~`) → a "pick a repo" list of recent/open repos appears (no "coming soon" stub). Pick one → flow continues.
- [ ] **Leftover-dir error is clear.** Try to create a worktree whose dir already exists → message says "remove the leftover directory / pick a different branch", not raw `fatal:`. *(`c450bd3`)*

## C. Wave 0 / Wave 1 features
- [ ] **Copy-on-select.** Drag-select text in a terminal → paste elsewhere → it's there (no Ctrl+C needed). *(`a00085c`)*
- [ ] **Ctrl+click an ABSOLUTE path** in output (e.g. `/home/you/file.ts`) → opens in the Files reader. *(`a00085c`)*
- [ ] **Ctrl+click a RELATIVE path** (e.g. `src/app.tsx` from a build error, while the tile is in that project) → opens the right file (resolved against the tile cwd). Prose like `foo/bar` (no extension) is **not** underlined. *(`88a4e87`)*
- [ ] **OS toast.** Drive a Claude session to a permission/question prompt, or let one complete → a desktop toast fires (`needsPermission`/`completed`/etc.). Toggle it off in Settings → silent. *(`c5915d8`)*
- [ ] **Command palette.** `Ctrl+K` → fuzzy-search "worktree"/"theme"/… → Enter runs it. Rebind a command → persists across relaunch. *(`7a70af2`)*
- [ ] **Prefix model.** `Ctrl+B` shows the prefix hint; a follow-up key fires; **double-tap `Ctrl+B`** sends a literal Ctrl+B to the shell. Existing direct hotkeys (Ctrl+T/W/Tab/1-9) still work. *(`7a70af2`)*
- [ ] **Keymap doesn't fire over inputs.** With the palette/branch-prompt open, typing doesn't trigger app commands; rebinding to an already-bound chord captures it (doesn't fire the command). *(`ae7f336`)*
- [ ] **Rules engine.** Settings → Rules → enable "open a terminal when a session ends" (or add one) → end a session → the action fires **once** (loop-guard holds). *(`853e8ba`)*
- [ ] **Session restore.** With live agent tiles, **quit the app**, relaunch → Recovery offers the orphaned sessions → **Restore** runs `claude --resume` in the right cwd. *(`c2b6c8d`)*

## D. Regression-fix spot checks (things reviews caught + we fixed)
- [ ] **Ended sessions show the RIGHT status.** Let a session complete/fail → the sidebar shows **completed/failed**, *never* "unknown". *(the HIGH we fixed, `c438c71`)*
- [ ] **Commit updates dirty-count immediately.** Commit from the Files panel → the header dirty indicator clears at once (not ~3.5s later). *(git-cache invalidation, `11158ae`)*
- [ ] **Rules/toasts don't get stuck.** With several sessions churning statuses, rules + toasts still fire (warmup can't mute forever). *(`48d029a`/`2b5bc9f`)*

## E. WSL config (apply when ready — restarts WSL)
- [ ] Apply `~/.wslconfig` (20GB cap + autoMemoryReclaim): run **`wsl --shutdown`** from a Windows PowerShell (kills live tmux sessions — do at a stopping point), relaunch. → WSL memory caps ~20GB and hands idle RAM back over time.

---

### If something fails
Note the **item + commit tag** above and the symptom — that pins it to a specific change. The two areas with residual *known* low-risk gaps (won't block, but watch): branch names that sanitize to the same dir (`feat/x` vs `feat-x` → git errors clearly), and a remote-only branch taking the `-b` create path. Both are in `docs/ROADMAP-PLAN.md` under WS-9.
