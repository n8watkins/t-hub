# T-Hub — Session Handoff

**Last updated:** 2026-06-25 · **Branch:** `main` · **App version:** `0.2.0` (shipped)

> **Zero-context handoff.** Read this file in full, plus any doc it links that's
> relevant to your task. Every decision below is already made — **do not re-ask
> the user anything answered here.** Working dir: `/home/natkins/projects/tools/t-hub/t-hub-app`.

---

## 1. What this is

**T-Hub** — a Tauri 2 desktop "command center" for running and supervising many Claude Code / Codex agent sessions at once (a local cockpit for persistent agent + shell terminals).

- **Stack:** Rust backend + React/TypeScript/Tailwind frontend with xterm.js terminals.
- **Repo:** a **pnpm monorepo**. `apps/desktop` = the app (Tauri); `apps/site` = the marketing site. Work happens in `apps/desktop`.
- **How it reaches the agents:** on **Windows** it drives WSL via `wsl.exe -e bash` running **`tmux -L t-hub`**; under `#[cfg(unix)]` it attaches tmux **directly** (this is the Linux/WSLg dev variant). Each terminal's tmux session is `th_<terminalId>`.
- **Backend surface:** ~50 `#[tauri::command]`s + an **MCP server** (`t-hub-mcp`, 22 tools) + a **loopback control channel** (`control.rs`, a `127.0.0.1` TCP server with a per-launch token — this is the seam the server split builds on).
- Codebase/repo identifiers deliberately stay `t-hub` (lowercase); the user-facing name is "T-Hub".

---

## 2. State — v0.2.0 SHIPPED

**`v0.2.0` is tagged (`9ee6b75`) and the GitHub Release is published** — signed `T-Hub_0.2.0_x64-setup.exe` + `latest.json`, so **auto-update is LIVE** for users.

This session shipped the whole **"herdr-parity wave"**:
- **Git worktree workflow** — `Ctrl+B w` / `c` / `l` plus a repo picker; worktrees created as a sibling `<repo>-worktrees/<branch>`.
- **Rebindable keymap** — `Ctrl+B` prefix + `Ctrl+K` command palette.
- **Event→action rules engine** (`store/rules.ts` + `lib/rulesMount.ts`).
- **OS toast notifications** (native, via `tauri_plugin_notification`).
- **Native session-restore** (relaunch offers to restore live agent tiles).
- **PERF overhaul** — fixed a freeze and RAM-creep (see §5 gotcha + `PERF-AUDIT.md`).

**Post-release cheap-wins, also on `main`** (commits after the `9ee6b75` release):
- `30e863b` — `refactor: dedup host_distro, persist codec, warmup (backlog cleanup)`
- `bec5ff7` — `feat(tray): light recovery actions — reload window + reconnect agent`
- `8a3674c` — `test: stand up vitest + first frontend tests` (vitest now wired; 53 frontend tests)

**Verification (all green at the tip of `main`):** `cargo build` clean · **185 Rust lib tests** pass · `tsc` clean · **53 vitest** pass · the CI **prod** build was green.

**⚠️ Known gap:** the v0.2.0 **Windows** build was **not** runtime-tested — only the **Linux/WSLg dev variant** was exercised locally. If the Windows build surprises, **fix-forward with v0.2.1** (don't block on it).

---

## 3. Next steps (ordered)

1. **SERVER-SPLIT M1 — the headline next project.**
   **Before doing anything, read [`docs/SERVER-SPLIT-AND-ROADMAP.md`](./SERVER-SPLIT-AND-ROADMAP.md) IN FULL.** It is now a **cold-start build guide** (not a survey): §6 has the **M1→M4** detail, §8 has the **pre-M1 decisions to settle first**, and the file references are verified against the v0.2.0 tree. The one-line bet: pull T-Hub's "brain" out of the desktop GUI into a headless `t-hub-server` that lives where the agents live (WSL/remote), so any device can connect and get the full cockpit. **M1 = decouple locally**: route a slice of GUI↔backend through the control-channel socket on one machine (no remote yet, zero user-visible change) — the foundation. The seam is **`control.rs`**. Forward-compat discipline to start immediately: **new backend features get added as control-channel commands/events, not in-process-only Tauri calls.**
2. **More vitest coverage** — component tests; close the test-audit gaps. See `docs/AUDIT.md` / `docs/SMOKE-TEST.md`.
3. **Heavier tray actions** (restart-tmux, full-WSL-shutdown) — the tray currently has only the *light* recovery actions (reload window + reconnect agent). Add the heavier ones **only when actually needed**.
4. **WS-9 nits:** `sanitizeBranchToDir` has a many-to-one collision risk, and remote-branch handling has a `-b`-vs-DWIM ambiguity. See [`docs/WORKTREE-WORKFLOW.md`](./WORKTREE-WORKFLOW.md) and [`docs/ROADMAP-PLAN.md`](./ROADMAP-PLAN.md).
5. **Parked differentiators** (not yet scoped): budget governor, worktree fleet launcher, MCP supervision event stream.

---

## 4. Conventions & gotchas (hard-won)

**Build / release**
- **Version lives in 3 files:** `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, `apps/desktop/src-tauri/Cargo.toml`. Bump **all three**, run `cargo build` to sync `Cargo.lock`, then commit.
- A pushed `v*` tag **or** `gh workflow run release.yml -f variant=prod` builds the **PROD** Windows installer, publishes a GitHub Release, and updates `latest.json`. **CI builds from the REMOTE — push first.**
- `variant=dev` builds a side-by-side **"T-Hub Dev"** (`com.t-hub.dev`, isolated socket) for testing; it coexists with prod and can't disturb live sessions. Variant model: [`docs/DEV-BUILD.md`](./DEV-BUILD.md).

**Local dev run**
- `pnpm -C apps/desktop tauri dev` builds the **Linux/WSLg** variant (no `wsl.exe`; attaches tmux directly). Great for cross-platform feature logic, but it does **NOT** cover the Windows `wsl.exe` path or WebView2 rendering.
- **Isolate a test instance** so it can't touch live sessions/state:
  `T_HUB_TMUX_SOCKET=t-hub-localtest T_HUB_CONTROL_FILE=~/.t-hub-localtest/control.json T_HUB_DIAG_FILE=~/.t-hub-localtest/diag.log`
- An **inherited** `T_HUB_*` env var WINS over per-user resolution — a prod app launched from a WSL shell that a dev/test instance polluted will silently run on the wrong socket / log to the wrong path. The startup marker `t-hub: started vX (diag -> …)` reveals the resolved diag path; if it points somewhere unexpected, the env is polluted (relaunch from a clean shell).

**Verify commands**
- Rust: `cd apps/desktop/src-tauri && cargo build` · `cargo test --lib`
- Frontend: `cd apps/desktop && pnpm typecheck` (tsc) · `pnpm test` (vitest)

**Traps**
- **`tauri.conf.json` must NOT have a `plugins.notification` config block.** The notification plugin takes no config there; even an empty `{}` **PANICS on startup** ("invalid type: map, expected unit"). It's wired via `.plugin(tauri_plugin_notification::init())` + capabilities only. This passes `cargo build` + `tsc` but crashes **only at runtime** — **always run the app, not just build it.** (This was a release-blocker; fixed in `c2c50a6`.)
- **Never `pkill -f "<pattern>"` where the pattern also matches your own running bash command** — it kills your own shell (exit 144). Use exact names (`pkill -x`) or PIDs.
- **The freeze was Windows-`wsl.exe`-specific** (`git_info` spawning `wsl.exe` 6×/tile/5s) — fixed via `spawn_blocking` + a per-cwd TTL cache. **Not reproducible on the Linux build**, so don't expect it locally.

**Commits**
- Conventional Commits. Trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Commit after each logical change; **push only when the user asks**; branch first on the default branch (`main` was pushed directly for the v0.2.0 release deliberately).
- **Subagent caveat:** subagents sometimes commit mid-batch despite instructions — verify commit boundaries didn't cross-contaminate.

---

## 5. File map (key entry points)

**Backend (`apps/desktop/src-tauri/src/`)**
- `control.rs` — loopback control channel = **the seam for the server split** (start M1 here).
- `agent/emit.rs` + `agent/mod.rs` — the event spine (EventEmitter trait; `launch_argv` is the core↔agent seam).
- `git.rs` — worktree primitive + the `git_info` TTL cache (the freeze fix).
- `commands.rs` / `pty.rs` / `tmux.rs` — terminals (PTY, tmux attach, `pane_info`).
- `supervision.rs` — FR-012 agent status / supervision tree.

**Frontend (`apps/desktop/src/`)**
- `store/keybindings.ts` + `lib/keymapExecutor.ts` — the rebindable keymap.
- `components/WorktreePrompt.tsx` / `components/WorktreesList.tsx` — worktree UI.
- `lib/rulesMount.ts` + `store/rules.ts` — the event→action rules engine.
- `lib/notify.ts` — OS toast notifications.

**Roadmap / design docs (`docs/`)** — link these, don't duplicate them:
- [`SERVER-SPLIT-AND-ROADMAP.md`](./SERVER-SPLIT-AND-ROADMAP.md) — **read for M1** (item ⑥; M1→M4 + pre-M1 decisions).
- [`ROADMAP-PLAN.md`](./ROADMAP-PLAN.md) — the herdr-parity wave + shipped status (Wave 0/1, WS-1…WS-9).
- [`PERF-AUDIT.md`](./PERF-AUDIT.md) — the freeze + RAM-creep investigation.
- [`WORKTREE-WORKFLOW.md`](./WORKTREE-WORKFLOW.md) — worktree UX + the WS-9 nits.
- [`SMOKE-TEST.md`](./SMOKE-TEST.md) — pre-release runtime checklist.
- [`HERDR-PARITY.md`](./HERDR-PARITY.md) — the parity rationale/matrix.
- [`DEV-BUILD.md`](./DEV-BUILD.md) — prod-vs-dev variant model + the `t-hub` identifier inventory.
