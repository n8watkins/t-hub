# T-Hub — Session Handoff

**Last updated:** 2026-07-06 · **Branch:** `main` at `82b3486`, fully pushed · **App version:** `0.3.39` (built locally, installed, running).

> **Zero-context handoff.** Read this file in full, plus [CAPTAIN-CHAT-PHASES.md](./CAPTAIN-CHAT-PHASES.md) (the next task lives there).
> Every decision below is already made — **do not re-ask the user anything answered here.**
> Working dir: `/home/natkins/projects/tools/t-hub/t-hub-app`.

---

## 1. What this is

**T-Hub** — a Tauri 2 desktop "command center" for running and supervising many Claude Code / Codex agent sessions at once.

- **Stack:** Rust backend (`apps/desktop/src-tauri`) + React/TypeScript/Tailwind frontend (`apps/desktop/src`) with xterm.js terminals; pnpm monorepo (`apps/site` = marketing site, `apps/cli` = CLI).
- **Terminals:** on Windows it drives WSL tmux (`tmux -L t-hub`, sessions named `th_<id[..8]>`); `#[cfg(unix)]` attaches tmux directly (WSLg dev variant).
- **Backend surface:** Tauri commands + the `t-hub-mcp` MCP server + a loopback control channel (`control.rs`, 127.0.0.1 NDJSON + per-launch token, handshake at `~/.t-hub/control.json`) — the control channel is the spine.
- **THE NATIVE (GPUI) PIVOT IS OVER.** The general ended it 2026-07-05; code + docs live on the `native-archive` branch (tag `native-pivot-final`); `apps/native` is removed from main. Do not propose native work. The webview app is the product.

## 2. State — what shipped in the 2026-07-05/06 session

All merged to `main` and running in the installed 0.3.39 app:

- **Spawn latency** (`517f880`, 0.3.37): `tmux.rs new_session` batched ~13 wsl.exe launches into 2 `;`-chained tmux command sequences. `kill_session` is now a test-only primitive (`93012fa`); production uses `kill_session_tree`.
- **Lane N** (PRs #4 `81c2ba6`, #5 `3ad0262`, #6 `7c056d9`): native chrome parity work, now archived along with the crate.
- **PR #7** (`c535d78`) attach stability: `remote_pty.rs` verifies `tmux has_session` before emitting EXIT (alive → `STATE Detached`); `Terminal.tsx` verifies liveness before the exited banner, auto-reattaches visible tiles with capped backoff (250ms ×2 → 5s), heals never-attached tiles via a detached-state sweep. Killed the recurring false "process exited" tiles.
- **PR #8** (`ed42edf`) headless server-authoritative organization: `control::TabRegistry` (monotonic seq) owns tabs/tiles; `spawn_terminal`/`create_worktree` take `tabName`/`tabId`, spawn server-side, place without focus steal; stale UI reports rejected with snapshot-to-adopt; new `close_tab` command (refuses last tab; `force` re-adopts live sessions); satellites' reporters are inert. E2E: `scripts/probes/headless_org_e2e.py` (24 checks).
- **PR #9** (`8ebef40`) captain overlay: pin a session as captain (tile right-click or palette), **Ctrl+B C** summons it as a floating panel over any tab (pooled TerminalView takeover — no second attach), **Shift+Esc** passes a literal Esc to the captain (interrupt Claude), **Esc** dismisses with validated focus restore; geometry persisted (`t-hub.captain.v1` + geometry store); `newPlainWorkspace` chord relocated **Ctrl+B C → Ctrl+B S** with a rebind-respecting migration; one unified Esc dispatch point in `lib/escOverlays` (overlay → fullscreen order).
- **Docs:** `docs/CAPTAIN-CHAT-PHASES.md` (next work, phases agreed), archive banners on the three NATIVE-*.md docs, `docs/NATIVE-FINISH-PLAN.md` marked archived.

**Verified working:** 0.3.39 installed via local NSIS build and relaunched; all tmux sessions survived; crews were reaped through the new headless close path.

**Known deferred items:** overlay pixel-level drag/resize never E2E'd (WSLg cannot send mouse input; the general does the manual pass); `remote_pty.rs` liveness check probes LOCAL tmux — must be revisited for remote endpoints (M2b); PR #6's wire read-timeout note still open for other socket clients.

## 3. Next steps (in order)

1. **Phase 1 of [CAPTAIN-CHAT-PHASES.md](./CAPTAIN-CHAT-PHASES.md) — captain list + switcher.** UI-only. Captain store becomes a list + `activeCaptainId` (MRU), migration from `t-hub.captain.v1` to `.v2`, Ctrl+B C cycles pinned captains while summoned (Esc still dismisses), overlay-header switcher, titlebar anchor count badge + dropdown, per-captain palette entries, tests per the doc. Key files: `apps/desktop/src/store/captain.ts`, `components/CaptainOverlay.tsx`, `lib/escOverlays.ts`, `lib/keymapExecutor.ts`, `store/keybindings.ts`.
2. Phase 2+ per the phases doc (ship-registry unification, fleet view) — do NOT start without the general.
3. Standing adjacent goals (tracked in the phases doc §Standing): server split M2-M4 (remote — the settled priority), MCP parity for `create_worktree`/`remove_worktree`/`wait_for_status`, wire read-timeouts.

## 4. Conventions & gotchas (hard-won this session)

- **Version bump on EVERY code commit** (`apps/desktop/scripts/bump-version.sh`, then `cargo check` in `src-tauri` to sync `Cargo.lock` — NEVER hand-edit the lock). Docs commits exempt. One bump per landed change — crews never bump; the captain/orchestrator bumps at merge.
- **Local Windows build** (docs: memory `local-windows-build`): Windows clone at `C:\Users\natha\projects\Tools\t-hub\t-hub-app` — do NOT `git merge` into it (it has local `tauri.conf.json` mods: `targets: ["nsis"]`, `createUpdaterArtifacts: false`); **rsync sources over it** (`apps/desktop/src`, `src-tauri/src`, Cargo.toml/lock, package.json), `sed` its `tauri.conf.json` version, then `powershell.exe … pnpm tauri build`. Output: `…\bundle\nsis\T-Hub_<v>_x64-setup.exe`; install with `/S`; PowerShell flags cargo's stderr as an error — check for "Finished 1 bundle".
- **A build that says `Aborting` on the first line still "succeeds"** — that's the clone's git merge failing before rsync'd sources build; verify the synced sources contain your change (grep a symbol) before trusting an installer.
- **Orchestration:** crews are spawned via the control socket (`create_worktree`; reference client `scripts/probes/t1_lib.py` — `connect()` returns `(sock, hs)`, use `LineReader`, pass `v=None`). Crew shipping: PR per branch, captain merges after review. The auto-mode classifier hard-blocks: launching `claude --dangerously-skip-permissions` via send_text (needs the general's explicit fresh words), pushing to a NEWLY created external repo (hand the general the command), and sometimes `git push origin main` (ask for the words "push main").
- **Review pattern that worked:** per-PR focused Explore agents (finders) + adversarial verify; findings sent back to the same crew as a fix round on the same branch.
- **WSLg E2E gotchas** (memory `wslg-webview-e2e-gotchas`): X11 backend broken, keyboard-only via RAIL, rendering can freeze — verify via `tmux capture-pane` + localStorage/SQLite ground truth, not pixels. Timebox crews' E2E environment fights; ship with unit tests + deferred manual pass instead.
- **MCP `read_terminal` can transiently fail** (`os error 11`) while heavy E2E churns the app; `tmux -L t-hub capture-pane -t th_<id> -p` is the reliable fallback.
- **Ship registry:** `~/.t-hub/captain/ships/*.md` (this ship: `t-hub-native.md`, captain terminal `7bb9bb2f`). Sentinel dir `/tmp/t-hub-crew-done/t-hub-native/`. The three `audit-*` worktrees under `.claude/worktrees/` belong to ANOTHER ship (unlanded commits — hands off).

## 5. File map (for the next task)

- `docs/CAPTAIN-CHAT-PHASES.md` — the phase plan; phase 1 is the task.
- `apps/desktop/src/store/captain.ts` — captain designation store (single id today; becomes a list).
- `apps/desktop/src/components/CaptainOverlay.tsx` — the floating panel (geometry, focus contract, pool takeover).
- `apps/desktop/src/lib/escOverlays.ts` — THE single Esc/Shift+Esc dispatch point; extend, don't add listeners.
- `apps/desktop/src/lib/keymapExecutor.ts` + `src/lib/commands.ts` — command registry (`toggleCaptainOverlay`, `pinCaptainFocused`).
- `apps/desktop/src/store/keybindings.ts` — chords + the v1 migration pattern to copy for v2.
- `apps/desktop/src/store/workspace.ts` — `adoptRegistry` (PR #8), `cleanupTileSideState` → `forgetCaptain` unpin path.
- `apps/desktop/src-tauri/src/control.rs` — TabRegistry + socket commands (phase 2 territory).
