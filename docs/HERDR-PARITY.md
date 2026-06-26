# T-Hub — herdr Feature-Parity Roadmap (staying a desktop app)

> ⚠️ **Most of this is now SHIPPED — see [ROADMAP-PLAN.md](./ROADMAP-PLAN.md) for current state; this doc is the original gap analysis.** Items ①notifications, the prefix-key model, ③worktree, ④`wait_for_status` + rules engine, and ⑤durable restore are all **built and committed** (Wave 0 + Wave 1, plus WS-9). Only **⑥ remote / SSH (server split)** is still deferred. The text below is kept as the design rationale — read it for *why* each gap mattered, not as a to-do list. Shipped items are stamped ✅ inline.

**Status:** Design / proposal (now largely shipped — see banner above). Captures the gaps where **herdr** is ahead of T-Hub and the concrete work to close each — **while T-Hub remains a Tauri desktop application.** Grounded in the current code; module references are real. Companion to [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) (which covers item ⑥ in depth).

> The decision: **keep T-Hub a desktop app, and close the ~7 things herdr does that we don't.** Everything else we already match (tile grid, theming, sessions) or *lead* — **cost economics, the supervision tree, and the MCP control channel are unmatched by herdr and every other competitor.** Those stay first-class; the work below is purely about catching up where herdr is genuinely ahead.

---

## Where herdr is actually ahead

From the parity analysis, herdr leads on only seven things. Everything T-Hub already does (multiplexing, persistent sessions, theming) is matched; the three differentiators above we *lead*. The seven gaps, in build order:

| # | Gap | Tier | Effort | Lands in | Status |
|---|-----|------|--------|----------|--------|
| ① | OS toast notifications | 0 | ~1–2 d | `supervision.rs` → `agent/emit.rs`, `lib.rs`, Settings | ✅ shipped |
| — | Clickable links / file paths in tiles | 0 | ~1 d | `components/Terminal.tsx` (xterm web-links) | ✅ shipped |
| — | Prefix-key / command-palette input model | 1 | ~1 wk | new `store/keymap.ts` + command registry (frontend) | ✅ shipped (`Ctrl+B` prefix + `Ctrl+K` palette) |
| ③ | Git worktree primitive | 1 | ~1–1.5 wk | `git.rs`, `ipc/git.ts`, Files/Git panel | ✅ shipped |
| ④ | `wait_for_status` + event-action rules | 1 | ~1.5 wk | `commands_05.rs`, MCP tool, generalize `store/autoContinue.ts` | ✅ shipped (`wait_for_status` + `store/rules.ts`) |
| ⑤ | Durable workspace restore / reattach-on-launch | 1 | ~1–2 wk | startup path in `lib.rs`, layout store, `tmux.rs` enumerate | ✅ shipped (`tile_sessions` + Recovery panel) |
| ⑥ | Remote / SSH (server split) | 2 | weeks | see [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) | ⏳ deferred |

**Tier 0–1 total ≈ 5–7 weeks.** Tier 2 (remote) is a separate, larger project. *(Tier 0–1 is now done — ① through ⑤ all shipped; only Tier 2 remote remains.)*

---

## Tier 0 — Quick wins (days each)

### ① OS toast notifications — ✅ SHIPPED
- **Gap:** herdr fires native OS toasts on agent events; T-Hub's notifications are minimal/future-work.
- **Work:** add `tauri-plugin-notification`. Emit a toast on FR-012 status transitions that need a human — `NeedsQuestion`, `NeedsPermission`, `Completed`, `Failed`, `RateLimited`. The transition already flows through `supervision.rs` → `agent/emit.rs`; `displayStatus()` in `store/supervision.ts` already classifies states. Add a notification sink in `lib.rs setup()` and a per-status on/off toggle in Settings.
- **Why first:** highest value-per-hour; reuses the existing status spine.

### Clickable links & file paths in terminals — ✅ SHIPPED
- **Gap:** herdr ships `[[link_handlers]]`; T-Hub's xterm tiles don't linkify output.
- **Work:** add xterm's `web-links` addon in `components/Terminal.tsx`; add a file-path matcher whose click opens the path in T-Hub's reader (reuse `open_file`).

---

## Tier 1 — Core parity (1–2 weeks each)

### Prefix-key / command model (the Ctrl+b idea) — ✅ SHIPPED
- **Gap:** herdr has a prefix + chord input model with rebindable keys; T-Hub *used to use* hardcoded hotkeys.
- **Shipped as:** a `Ctrl+B` prefix + `Ctrl+K` fuzzy command palette over a rebindable keymap (`store/keybindings.ts` + command registry). Bindings are rebindable and persisted.
- **Work:** a frontend keymap layer (`store/keymap.ts`) + a command registry + a command-palette component + leader/prefix capture. Bindings rebindable and persisted. Mostly frontend; no backend change. Side benefit: every future feature becomes discoverable via the palette instead of needing a new hardcoded key.

### ③ Git worktree primitive — ✅ SHIPPED
- **Gap:** herdr has `worktree create/open/remove/list` as a first-class workspace primitive; T-Hub had only `git_info`/`git_commit`.
- **Shipped as:** `git_worktree_add/list/remove` (Rust + MCP tools) plus the worktree-centric workflow — `Ctrl+B w` (new worktree tab), `Ctrl+B c` (new plain tab), `Ctrl+B l` (worktrees list). See [WORKTREE-WORKFLOW.md](./WORKTREE-WORKFLOW.md).
- **Work:** add worktree commands to `git.rs` (`git worktree add/list/remove`), expose via `ipc/git.ts`, and a UI flow: *create worktree → spawn a tile with cwd = the worktree path*, plus list/switch/remove in the Files/Git panel. Tie a worktree to a tab so a feature-branch workspace is one click.

### ④ `wait_for_status` + event-action rules — ✅ SHIPPED
- **Gap:** herdr exposes `herdr wait agent-status` and runs commands on events. T-Hub *had* the status model but no "block until status X" primitive and no user-configurable "on event → do thing." Autocontinue was hardcoded.
- **Shipped as:** the `wait_for_status` MCP/control tool, and the generalized rules engine in `store/rules.ts` (the old hardcoded autocontinue migrated to a default rule) with a loop-guard.
- **Work:**
  - (a) a `wait_for_status` command in `commands_05.rs` that resolves when a session reaches a target FR-012 status; mirror it as an MCP tool.
  - (b) generalize `store/autoContinue.ts` into a small **rules engine**: *on status transition → {notify | send-text | spawn terminal | restart | run command}*. This is the same mechanism behind "auto-start a session when one ends," "auto-continue on done," etc. — as configurable rules, not hardcoded behavior.
- **Why it matters:** converts a T-Hub internal (the hook-derived status spine) into the automation surface herdr exposes — and it's *stronger* than herdr's, because our statuses are hook-derived, not output-pattern-guessed. Remember the **loop-guard** (cooldown / max-restart) so respawn-on-exit can't spin.

### ⑤ Durable workspace restore / reattach-on-launch — ✅ SHIPPED
- **Gap:** herdr cleanly restores sessions; T-Hub keeps tmux alive (detach-on-close) and persists layout, but **auto-reattach on relaunch** wasn't a first-class flow.
- **Shipped as:** the `tile_sessions` table + a boot-time orphan correlation, surfaced in the Recovery panel (`RecoveryReview.tsx`) — Restore spawns `claude --resume <id>` in the stored cwd.
- **Work:** verify current layout persistence, then on startup enumerate surviving `t-hub`-socket tmux sessions (`tmux.rs`) and **automatically rehydrate tabs/tiles and reattach** — closing and reopening the app restores the whole cockpit, not just the processes. Surface orphaned sessions (alive in tmux, no tile) for one-click re-adopt.

---

## Tier 2 — The big one

### ⑥ Remote / SSH (server split) — ⏳ STILL DEFERRED (the one gap left)
- **Gap:** herdr has `--remote`; T-Hub is local-only.
- **Work:** see [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md). Extract a headless `t-hub-server` into WSL, promote the `control.rs` channel to carry PTY + events over Tailscale, make the GUI a thin client (M1→M4). Tiles are easy (PTY bytes); the overlay panels (cost/supervision/files) are the work, because their data sources must be re-pointed from "local WSL" to "the server."
- **Note:** staying a desktop app does **not** cost you this — a Tauri client over a remote server is still a desktop app; you gain "open my T-Hub from another machine." This is the one place herdr is meaningfully ahead and it's a real project.

---

## Tier 3 — Optional (full-surface parity only)

- **Generic agent output-matching** — herdr detects *any* agent via output patterns (`pane.output_matched`); T-Hub is Claude-hook-specific (richer for Claude, blind to others). Add an optional pattern matcher for non-Claude tools. Low priority unless you run non-Claude agents.
- **First-class `t-hub` CLI** — herdr's CLI is shell-scriptable; T-Hub's control channel is only spoken by MCP. A small `crates/t-hub-cli` over `control.rs` (mirror of `t-hub-mcp`) gives `t-hub spawn/send/wait/...` for scripts. Pairs naturally with `wait_for_status`.
- **Plugin/event extensibility** — herdr has a plugin system; T-Hub has MCP. Different philosophies; MCP arguably covers it. Build only if you want third-party extensions.

---

## Don't regress the lead

While closing the above, keep first-class the three things **no competitor (herdr included) has**:
- **Cost / context economics** (`usage.rs`, `codex.rs`, statusline ingestion)
- **Supervision tree** (`supervision.rs`, the FR-012 model)
- **MCP control channel** (`t-hub-mcp`, `control.rs`)

These are why someone picks T-Hub over herdr.
