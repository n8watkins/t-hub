# T-Hub Roadmap — Execution Plan

**Status:** Active plan, **verified against the live codebase** (six read-only audits, file:line-grounded). Turns [HERDR-PARITY.md](./HERDR-PARITY.md) (the *why* + gap analysis) into an *executable* plan organized for **parallel agent execution**. Companion: [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) (item ⑥).

**Active scope = Wave 0 + Wave 1.** Remote/SSH (⑥) and the beyond-parity wildcards are deferred to *Later / someday* (bottom). All paths below are real (`apps/desktop/…` monorepo).

## Principles

- **Personal system, will be public.** No auth / multi-tenant / enterprise plumbing. But "public" means polish matters → UX quick wins carry weight.
- **No plugin system.** Out of scope — extensibility is MCP.
- **Keep the three differentiators first-class:** cost/context economics, supervision tree, MCP. Nothing below regresses them.
- **Parallelize via worktree isolation.** Independent workstreams run as concurrent agents in their own git worktree (`isolation: "worktree"`), merged at a short integration step. The few shared files (seams below) are append-heavy → low semantic conflict.

## Shared seams (files multiple workstreams touch — merge carefully)

| Seam | Touched by | Type |
|---|---|---|
| `apps/desktop/src-tauri/src/lib.rs` (`invoke_handler![]`, setup) | WS-4, WS-6 | additive list |
| `apps/desktop/src-tauri/crates/t-hub-mcp/src/tools.rs` + `src/control.rs` dispatch | WS-4, WS-5a | additive |
| `apps/desktop/src-tauri/crates/t-hub-protocol/src/lib.rs` (AgentRequest/Response) | WS-4 | additive enum |
| `apps/desktop/src/ipc/git.ts` / `ipc/controlBridge.ts` | WS-1, WS-4 | additive |
| `apps/desktop/src/store/settings.ts` | WS-2, WS-3, WS-5b | additive fields |
| `apps/desktop/src/components/Terminal.tsx` | WS-1 only | single owner |
| `apps/desktop/src/components/Canvas.tsx` | WS-3 only | single owner |

---

## Wave 0 — Quick wins (~1 day total; 2 agents)

### WS-1 · Terminal UX: copy-on-select + open-file-on-click
**Verified:** WebLinksAddon **already loaded** with a custom click→`openExternal` handler (`Terminal.tsx:289`) — **clickable URLs already work, no work needed.** Clipboard is already abstracted via `clipboardWrite()` (`Terminal.tsx:65`, Tauri plugin + `navigator.clipboard` fallback; WebView2 needs the plugin path). There is **no** `onSelectionChange` handler today. FilePanel opens files *internally* (`FilePanel.tsx:200 openFile`) — there's no frontend path to open a file in the reader from a terminal yet.
- **Scope:** (a) **copy-on-select** — add `term.onSelectionChange` → on settle, `clipboardWrite(term.getSelection())`; **debounce** (fires rapidly mid-drag); don't clear selection; keep existing Ctrl+C/Ctrl+V intact. (b) **Ctrl+click a file path → open in reader** — needs a tiny `store/fileOpen.ts` (or a `controlBridge` `open_file` case) that FilePanel watches; resolve relative paths against the tile's `cwd`.
- **Files:** `apps/desktop/src/components/Terminal.tsx`; new `apps/desktop/src/store/fileOpen.ts`; `apps/desktop/src/components/FilePanel.tsx` (watch + open).
- **Effort:** copy-on-select ~1–2 h; file-open ~half day.
- **Risks:** `onSelectionChange` fires per drag tick → **debounce**; path-detection false positives (git hashes, URLs) → strict regex requiring a real path shape; relative-path root needs the tile cwd.
- **Decision (baked in):** copy-on-select is **immediate on selection settle**, selection stays highlighted (matches Claude Code / iTerm).
- **Accept:** highlight → paste elsewhere works; Ctrl+click a path → opens in Files reader (abs + relative); existing copy/paste unaffected.

### WS-2 · OS toast notifications ① *(~85% pre-built — verified)*
**Verified:** `src/lib/notify.ts` (386 lines) maps all 5 human-needed FR-012 statuses → toast+synthesized chime, with **dedup + 6 s startup warmup**; `notifyMount.ts` is **already imported in `main.tsx`**; `settings.ts` already persists `notificationsEnabled` + `soundsEnabled`. It is a *working feature that degrades to sound-only* because the Tauri plugin isn't installed. **It is not a no-op.**
- **Scope:** install the plugin (4 edits) + expose Settings toggles. (a) `package.json` → `@tauri-apps/plugin-notification`; (b) `src-tauri/Cargo.toml` → `tauri-plugin-notification = "2"`; (c) `tauri.conf.json` → `"notification": {}`; (d) `capabilities/default.json` → notification permissions. (e) add the two toggles to the Settings panel (store already wired).
- **Files:** the 4 config files under `apps/desktop/` + the Settings UI component.
- **Effort:** **~30 min** for the toasts + ~1–2 h for the toggles UI.
- **Risks:** one-time OS permission prompt on first fire (expected); spam already handled by dedup+warmup; *optional* focus-aware suppression (skip toast when the window is focused) is a nice add, not required.
- **Accept:** drive a session to `NeedsPermission` → OS toast; toggle off → silent; no startup burst.

> WS-1 owns `Terminal.tsx`; WS-2 owns config + Settings → fully independent, run concurrently.

---

## Wave 1 — Core parity (~4–5 weeks; up to 5 agents)

### WS-3 · Prefix-key model + command palette ② *(Tier 1, ~1 wk)*
**Verified:** capture-phase `document` keydown in `Canvas.tsx:288` **does** beat xterm (xterm uses bubbling `attachCustomKeyEventHandler`, `Terminal.tsx:317`). tmux's `C-b` **is** disabled server-side (`tmux.rs:208` — `prefix None` + unbind). **No** existing action registry — greenfield. Settings persistence pattern exists (`settings.ts` localStorage, versioned key). Full current-hotkey inventory captured (Ctrl+T/W/B/Tab/1-9/zoom/Esc in Canvas; copy/paste/page/zoom in Terminal).
- **Scope:** new `store/keybindings.ts` (prefix + direct bindings, persisted), `lib/keymapExecutor.ts` (command registry — migrate all current hotkeys into it), `lib/prefixKeyHandler.ts` (armed-state machine + visual hint + timeout), `components/CommandPalette.tsx` (fuzzy + interactive rebind), Settings keybindings section. Refactor `Canvas.tsx` keydown to dispatch through the registry.
- **Model — HYBRID (two tiers, both rebindable):**
  - **Direct hotkeys (no prefix)** — high-frequency / spatial nav: cycle workspaces `Ctrl+Tab`/`Ctrl+Shift+Tab`, jump `Ctrl+1-9`, new/close `Ctrl+T`/`Ctrl+W`, zoom `Ctrl+=`/`-`/`0`, palette `Ctrl+K`, focus-toggle (relocated, e.g. `Ctrl+J`).
  - **Prefix (`Ctrl+B` then a key)** — the expanding tail: worktree create, rename tab, session navigator/restore, rules, settings, theme, detach, split variants. The prefix adds *capacity* for new commands without consuming more raw Ctrl-chords; it does not replace the nav keys.
- **Effort:** ~1 wk (~5 new files, ~1000 LOC).
- **Decision (resolved): default prefix = `Ctrl+B`.** tmux freed it server-side (`tmux.rs:208`), it's the tmux-default users expect, and it's what we want. Two consequences: (1) **relocate** the current `Ctrl+B` focus-toggle (`Canvas.tsx:236`) to a new direct key; (2) capturing it shadows the shell's readline `backward-char` (milder than `Ctrl+A`'s `beginning-of-line`) — covered by "double-tap prefix → send literal." Prefix stays configurable.
- **Risks:** validate persisted bindings on load (fall back to defaults); don't shadow browser/Tauri devtools keys; the prefix armed-state needs a visible hint + timeout so a swallowed keystroke is obvious.
- **Accept:** direct nav hotkeys unchanged; `Ctrl+B → c` = new tab (prefix); palette opens/fuzzes/executes; rebinding persists; old focus-toggle still reachable on its new key.

### WS-4 · Git worktree primitive ③ *(Tier 1, ~1–1.5 wk)*
**Verified:** `GitInfo` already has `worktree_root` + `is_linked_worktree` (`git.rs:33`, helper `:147`). Journal enum `WorktreeCreate`/`WorktreeRemove` reserved but **unused** (`t-hub-protocol/src/lib.rs:446`). Agent protocol already has a **list** op (`AgentRequest::GitWorktrees`) — but **no add/remove**. Command-registration + MCP-tool + tab/tile-spawn patterns all confirmed and reusable.
- **Scope:** add `git_worktree_add/remove` (mirror the `wsl.exe --cd`/unix pattern in `git.rs`/`commands_05.rs`); extend `t-hub-protocol` AgentRequest/Response with add/remove; agent-side exec; register in `lib.rs`; MCP `create_worktree`/`remove_worktree` tools (`tools.rs` + `control.rs` dispatch, Organization tier); frontend atomic action (worktree → `addTab` → spawn tile in the worktree cwd) reusing `store/workspace.ts:addTab/addAfterFocused`.
- **Effort:** ~1–1.5 wk.
- **Risks:** worktree paths must be **POSIX inside WSL** (git runs in WSL); `git worktree add` fails if branch already checked out → clear error; **removing a worktree with live tiles orphans processes** → detach tiles first (mirror `close_terminal`). Record `WorktreeCreate`/`Remove` journal events.
- **Accept:** create-from-branch → new tab with tiles in the worktree dir; `git_info(path).is_linked_worktree==true`; list/remove work; remove-with-live-tiles is safe.

### WS-5a · `wait_for_status` primitive ④ *(Tier 1, ~1–2 d)*
**Verified (assumption corrected):** the `Supervisor` is a **snapshot-only reducer** — `status(session_id)` reads a HashMap (`supervision.rs:206`); there is **no condvar/channel/subscription**. So a watch-based wait would mean new infra on the journal hot path. **Long-poll is the clean fit.**
- **Scope:** MCP/control tool `wait_for_status {sessionId, targetStatus|[], timeoutMs?}` that polls `get_status` (~500 ms) until target or timeout, returning `{finalStatus, elapsedMs, timedOut}`. Add to `tools.rs` catalog + `control.rs` dispatch (Read tier). Returns immediately if already at target.
- **Files:** `crates/t-hub-mcp/src/tools.rs`, `src-tauri/src/control.rs`; optional mirror in `commands_05.rs`.
- **Effort:** ~1–2 d. *Backend-only, independent — could even be pulled into Wave 0.*
- **Risks:** mandatory timeout (default 30 s) so a hung `Working` session doesn't block forever; poll current state before sleeping to avoid missing a fast transition.
- **Accept:** blocks until `completed`/etc.; honors timeout; supports compound targets; immediate when already satisfied.

### WS-5b · Event-rules engine ④ *(Tier 1, ~3–4 d, frontend-first)*
**Verified:** autocontinue today is **frontend logic** in `lib/autoContinueMount.ts` (187 lines) reacting to events: trigger = usage `usedPercentage≥99% + resetsAt`, action = inject `"continue\r"`, dedup via a `handled` map. It's hardcoded to one trigger + one action.
- **Scope:** generalize into a `store/rules.ts` + `lib/rulesMount.ts` that subscribe to the existing `session://status` / supervision events (same pattern as `autoContinueMount`): *on transition → {notify | send-text | spawn terminal | restart | run command}*, with a **loop-guard** (cooldown + max-spawns-per-session/min). Migrate the existing autocontinue into a default rule. Settings rules UI. **Frontend-first** (localStorage); a backend `rules.rs` is deferred.
- **Effort:** ~3–4 d.
- **Risks:** **respawn-spin** — cap spawns per (session, rule) per window; dedup window configurable (presets: off / 5 s / 60 s). This is your *auto-start-on-session-end*.
- **Accept:** rule "on Completed → spawn shell" fires once; loop-guard blocks spin; autocontinue still works (migrated rule).

### WS-6 · Native session-restore after restart ⑤ *(Tier 1, ~1–2 wk)*
**Verified:** `db.rs` has `kv` + `snapshots` tables with `CREATE TABLE IF NOT EXISTS` (easy to add one). StatusSnapshot carries `session_id` (`status.rs:46`), `cwd` (`:52`), `tmux_session` `th_<id>` (`:67`) — but **NO `agent_kind`** (drop it; `claude --resume` is agent-agnostic). `RecoveryReview.tsx` **already** surfaces orphaned tmux sessions + reconciles (`:543`). Boot path (`lib.rs` setup) does **not** yet detect resumable orphans.
- **Scope:** add `tile_sessions(terminal_id PK, session_id, cwd, tmux_session, created_at)` to `db.rs`; upsert from the status-ingest path; a boot task after `db::init()` correlating `tile_sessions` × `list_terminals` × transcript-exists → `list_orphaned_sessions()` command; extend `RecoveryReview.tsx` with an "Orphaned sessions" section whose Restore spawns `claude --resume <id>` in the stored `cwd`.
- **Effort:** ~1–2 wk.
- **Risks:** drop `agent_kind` (corrected); key on `tmux_session` (robust) not cwd; guard double-resume (warn if already placed); skip entries whose transcript is gone.
- **Accept:** kill app with live agent tiles → relaunch → offered restore → `claude --resume` reattaches in the right cwd.

> WS-3 (frontend/Canvas), WS-4 (git+MCP+protocol), WS-5a (MCP), WS-5b (frontend stores), WS-6 (db+boot+RecoveryReview) hit largely different modules. Seams: `lib.rs` handler list (WS-4, WS-6), MCP `tools.rs`/`control.rs` (WS-4, WS-5a), `settings.ts` (WS-2, WS-3, WS-5b) — all append-only; integration step merges them.

### WS-8 (optional, small) · Named-session socket namespacing
Harden the dev-vs-prod control-channel "which instance?" bug. Namespace the handshake/socket per named session. `control.rs` + env resolution. Fills a parallel slot if wanted.

---

## Execution mechanics

1. **Per wave:** launch workstreams as concurrent agents, each with `isolation: "worktree"`.
2. **Integration step (per wave):** merge in seam order — backend command lists (`lib.rs`) → IPC types → MCP tools → frontend stores — then `pnpm build`/`tsc` + `cargo build` + `cargo test`. One logical commit per workstream.
3. **Verify** each WS builds (and where feasible runs) before merging; never merge a red build.
4. **Gate:** Wave 1 starts after Wave 0 merges.

**Suggested ordering tweak:** WS-2 (~30 min) and WS-5a (~1–2 d) are tiny and backend/config-isolated — knock them out first as confidence-builders, even before the full Wave 0/1 fan-out.

---

## Later / someday (explicitly deferred — not in the active plan)

The active roadmap is **Wave 0 + Wave 1.** Everything below is wanted, none scheduled.

- **⑥ Remote / SSH (the server split)** — the one big project. Per [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md): M1 decouple locally (`RemoteTarget` routing the WSL/tmux/git hops through `ssh`) → M2 PTY over the wire → M3 overlay panels server-side → M4 multi-client. Sequential, after Wave 1 — it refactors the same backend seams everything else touches.
- **Beyond-parity wildcards** (lean into our differentiators):
  - **Cost-aware orchestration / budget governor** — pause/throttle on spend or context budget; route to the cheapest idle agent.
  - **Worktree fleet launcher** with **aggregate cost rollup**.
  - **MCP-wrap the supervision graph as a subscribable event stream** — an orchestrator blocks on `subagent_blocked`.
- **Smaller items:** generic "unknown-agent" detection · external custom-agent injection · first-class `t-hub` CLI over the control channel · mobile/responsive narrow mode.
