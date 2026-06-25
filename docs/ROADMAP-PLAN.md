# T-Hub Roadmap — Execution Plan

**Status:** Active plan. Turns [HERDR-PARITY.md](./HERDR-PARITY.md) (the *why* + gap analysis) into an *executable* plan organized for **parallel agent execution**. Companion: [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) (item ⑥ deep dive).

**Active scope = Wave 0 + Wave 1.** Remote/SSH (⑥) and the beyond-parity wildcards are explicitly deferred to *Later / someday* (bottom of this doc) — wanted, but not scheduled.

## Principles

- **Personal system, will be public.** Keep it simple — no auth / multi-tenant / enterprise plumbing. But "public" means polish + README matter, so UX quick wins (copy-on-select, notifications, links) carry weight.
- **No plugin system.** Dropped from scope — extensibility is covered by MCP. Don't build `herdr-plugin.toml`-style machinery.
- **Keep the three differentiators first-class:** cost/context economics, supervision tree, MCP. Nothing below regresses them.
- **Parallelize via worktree isolation.** Independent workstreams run as concurrent agents, each in its own git worktree (`isolation: "worktree"`), merged at a short integration step. The few shared files (the "seams" below) are append-heavy lists → low semantic conflict.

## Shared seams (the only files multiple workstreams touch — merge carefully)

| Seam | Touched by | Conflict type |
|---|---|---|
| `src-tauri/src/lib.rs` (invoke-handler list, plugin setup) | ②③④⑤⑥, ① | additive list |
| `src/ipc/types.ts` / `ipc/git.ts` (IPC contract) | ③④ | additive |
| `t-hub-mcp/src/tools.rs` + `control.rs` dispatch (MCP tools) | ③④ | additive |
| `src-tauri/Cargo.toml` (deps) | ① | additive |
| `src/components/Terminal.tsx` | copy-on-select + links | **same file → one workstream** |

---

## Wave 0 — Quick wins (2 agents in parallel, ~days)

### WS-1 · Terminal UX: copy-on-select + clickable links *(NEW + Tier 0)*
- **Goal:** highlighting text auto-copies (like Claude Code's terminal); Ctrl+click opens URLs/paths.
- **Files:** `src/components/Terminal.tsx` (+ small clipboard util). Owns this file solo.
- **Steps:** (a) `term.onSelectionChange` → on mouseup, write `term.getSelection()` to clipboard (`@tauri-apps/plugin-clipboard-manager` or `navigator.clipboard`), guard empty; don't break existing paste. (b) load `@xterm/addon-web-links` for URLs + a file-path matcher whose click calls `open_file`.
- **Verify:** select → paste elsewhere; click a URL → browser opens.

### WS-2 · OS toast notifications ① *(Tier 0, ~80% pre-wired)*
- **Goal:** OS toast on human-needed FR-012 transitions.
- **Files:** `Cargo.toml` (+`tauri-plugin-notification`), `lib.rs` (register plugin + capability json), `src/lib/notify.ts` (already maps status→toast — flip from no-op to real `sendNotification`), Settings (per-status toggles).
- **Steps:** add plugin + permission; wire `notify.ts`; gate via Settings store; request permission on first fire.
- **Verify:** drive a session to `NeedsPermission` → toast; toggle off → suppressed.

> WS-1 and WS-2 are independent (Terminal.tsx vs. Cargo/lib/notify) → run concurrently.

---

## Wave 1 — Core parity (4 agents in parallel, ~weeks)

### WS-3 · Prefix-key model + command palette ② *(Tier 1)*
- **Goal:** tmux-style prefix (default `Ctrl+A`) + fuzzy command palette; rebindable, persisted.
- **Files:** new `src/store/keymap.ts` (action registry + prefix state machine + bindings), `src/Canvas.tsx` (capture-phase keydown already beats xterm — hook prefix here), new `CommandPalette.tsx`, Settings keybindings UI. Migrate existing hardcoded hotkeys into the registry.
- **Verify:** `Ctrl+A c` = new tab; palette opens/fuzzes/executes; rebinding persists across restart.
- **Seam:** frontend-only; no backend conflict.

### WS-4 · Git worktree primitive ③ *(Tier 1)*
- **Goal:** first-class worktree — create/open/remove, atomic "checkout + tab + tiles in one call."
- **Files:** `git.rs` (+`git_worktree_add/list/remove`, mirror existing `git_info` wsl/unix paths), `ipc/git.ts`, `lib.rs` (register), `store/workspace.ts` (`addWorktreeWorkspace`), UI in Files/Git panel, MCP tool (`tools.rs`+`control.rs`). `WorktreeCreate` journal enum already reserved.
- **Verify:** create worktree from a branch → new tab with tiles in the worktree dir; list/remove work.
- **Seam:** `lib.rs`, `ipc/git.ts`, MCP tools (shared w/ WS-5).

### WS-5 · `wait_for_status` + event-rules engine ④ *(Tier 1)*
- **Goal:** block-until-status primitive + configurable event→action rules (incl. **auto-start a session when one ends**).
- **Files:** `commands_05.rs` (`wait_for_status` async long-poll), `supervision.rs` (status-change subscription/condvar), MCP tool (`tools.rs`+`control.rs`), generalize `src/store/autoContinue.ts` → `store/rules.ts` (*on transition → {notify | send-text | spawn | restart | run}*) with **loop-guard** (cooldown / max-restart), Settings rules UI.
- **Verify:** MCP `wait_for_status` resolves on transition; rule "on Completed → spawn shell" fires; loop-guard blocks spin.
- **Seam:** `commands_05.rs`, MCP tools (shared w/ WS-4), `supervision.rs` (light, shared w/ WS-6).

### WS-6 · Native session-restore after restart ⑤ *(Tier 1)*
- **Goal:** restore tiles/agents after backend/tmux/host restart.
- **Files:** `db.rs` (new `tile_sessions(terminal_id, session_id, cwd, agent_kind, …)`), `claude/status.rs` (populate from status bridge), `lib.rs setup()` (boot-detect orphaned tiles), reuse `recent.rs` recall + `RecoveryReview.tsx`.
- **Verify:** kill app with live agent tiles → relaunch → offered restore → `claude --resume` reattaches.
- **Seam:** `lib.rs setup()`, `db.rs`, `status.rs`.

> WS-3/4/5/6 hit different modules; the only real overlap is the append-only `lib.rs` handler list + MCP tool registration (WS-4, WS-5). Integration step merges those.

### WS-8 (optional, small) · Named-session socket namespacing
- **Goal:** harden the dev-vs-prod control-channel "which instance?" bug we actually hit. Namespace the control socket / handshake per named session.
- **Files:** `control.rs`, env resolution. Small backend task; can fill a parallel slot in Wave 1.

---

## Later / someday (explicitly deferred — not in the active plan)

The active roadmap is **Wave 0 + Wave 1.** Everything below is parked until those land; all are wanted, none are scheduled.

- **⑥ Remote / SSH (the server split)** — the one big project. Per [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md): M1 decouple locally (`RemoteTarget` routing the WSL/tmux/git hops through `ssh`) → M2 PTY over the wire → M3 overlay panels server-side → M4 multi-client. Sequential, after Wave 1 — it refactors the exact backend seams (`lib.rs`, `control.rs`, the WSL/tmux/git hops) everything else touches. Run as a dedicated long-lived worktree when picked up.
- **Beyond-parity wildcards** (T-Hub's edge — things *neither* tool does, leaning into our differentiators):
  - **Cost-aware orchestration / budget governor** — pause/throttle agents on spend or context budget; route work to the cheapest idle agent.
  - **Worktree fleet launcher** with **aggregate cost rollup** across the fleet.
  - **MCP-wrap the supervision graph as a subscribable event stream** — an orchestrator blocks on `subagent_blocked`, turning observation into coordination.
- **Smaller items:** generic "unknown-agent" detection · external custom-agent injection (`report-metadata`-style) · first-class `t-hub` CLI over the control channel · mobile/responsive narrow mode. None block parity.

---

## Execution mechanics

1. **Per wave:** launch the workstreams as concurrent agents, each with `isolation: "worktree"`.
2. **Integration step (per wave):** merge worktrees in seam order — backend command lists (`lib.rs`), then IPC types, then MCP tools — run `cargo build` + `pnpm build`/`tsc` + `cargo test`, commit each workstream as its own logical commit.
3. **Verify each WS** builds and (where feasible) runs before merging; never merge a red build.
4. **Wave gate:** Wave 1 starts after Wave 0 merges; Wave 2 after Wave 1 merges.
