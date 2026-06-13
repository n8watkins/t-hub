# TermHub — Forward Build Plan (0.5 → 2.0)

**Status:** Planning track (no code). Source of truth for product/architecture is [PRD.md](../PRD.md); technical risks and the verified Claude Code facts are in [REVIEW.md](../REVIEW.md). The 0.1 IPC contract is [src/ipc/types.ts](../src/ipc/types.ts).

**Scope of this document:** the phased implementation plan *after* the 0.1 nucleus. Each release section carries: Goal · Workstreams · Key Claude Code mechanisms · Data-model additions · Exit criteria · Risks/watch-items.

---

## 0. Foundation — what 0.1 already gives us (do not re-plan)

The 0.1 "playable proof" nucleus is being scaffolded in parallel and is the floor every later release builds on. It establishes:

- **Tauri 2 + React/TS** desktop shell with a WebView canvas.
- **PTY spine:** `portable-pty`/ConPTY spawning `wsl.exe` → `tmux -L termhub attach` (one client per visible tile).
- **Isolated tmux server** (`-L termhub`), one named session per terminal, surviving UI close.
- **Auto-grid canvas:** add/remove/focus tiles; deterministic insertion after the focused tile.
- **The IPC contract** ([src/ipc/types.ts](../src/ipc/types.ts)): commands `spawn_terminal`, `attach_terminal` (returns base64 scrollback), `write_terminal`, `resize_terminal`, `close_terminal` (detach, keep process), `kill_terminal` (stop process), `list_terminals`; events `terminal://output` (base64 bytes), `terminal://state`, `terminal://exit`; `TerminalState = starting|live|detached|exited|error`.

**0.1 exit (assumed met before 0.5 starts):** ≥6 live terminals rearrange and survive UI closure without process loss.

> **Two carry-forward truths from REVIEW.md that shape every later phase.**
> 1. **Process durability is app-close-only; conversation durability is reboot-survivable.** `wsl --shutdown` / Windows reboot tears down the WSL2 VM → tmux server, every shell, and every in-flight turn die. Only the **transcript on the VHDX** survives. "Recovery after restart" therefore means *resume Claude conversations by exact ID + restart shells*, never "reattach to a live `npm run dev`." (REVIEW §1.)
> 2. **The event spine is: Claude hook → WSL-side journal → termhub-agent → Windows core → UI event.** The journal (not live process state) is the authority for reconstruction intent, and it survives the Windows app being closed. Build this spine in 0.5; everything else rides it.

### Conventions used below

- **Hook stdin base fields** (on *every* hook, verified): `session_id`, `transcript_path`, `cwd`, `permission_mode`, `effort`, `hook_event_name`; inside subagents also `agent_id`, `agent_type`. Exact-ID capture at `SessionStart` is therefore reliable.
- **Statusline JSON** (verified fields): `rate_limits.five_hour.resets_at`, `rate_limits.seven_day.resets_at` (Unix epoch), `rate_limits.*.used_percentage`, `context_window.*`, `cost.*`. **Caveat:** the `rate_limits` block exists **only for Claude.ai Pro/Max** and **only after the session's first API response** — treat reset time as initially unknown and degrade gracefully when absent. Non-worktree git branch is **not** in the statusline; derive it via the agent's `git branch --show-current`.
- **Session flags** (verified): `-c`/`--continue` (most recent in cwd), `-r`/`--resume [name|id]`, in-session `/resume`; **ID lookup is scoped to project dir + its worktrees, name search spans the repo**; picker widening `Ctrl+W` (all worktrees) / `Ctrl+A` (all projects); fork via `/branch` or **`--fork-session`** (new ID, copied history — note: "approved for this session" permissions do **not** carry into a fork).
- **Transcripts:** `~/.claude/projects/<project>/<session-id>.jsonl`; default cleanup **30 days** via `cleanupPeriodDays` (any settings.json scope; min 1, `0` rejected); `CLAUDE_CONFIG_DIR` relocates the store.

---

# 0.5 — Personal alpha

**PRD roadmap row:** "Replace the normal terminal for daily multi-agent work."

## Goal
A senior engineer can run a full workday with 6–12 visible Claude sessions and trust the working / waiting-on-subagents / completed / failed awareness, with exact-session identity, a duplicate-resume guard, transactional autosave, and a reviewed recovery flow. **Parallel-agent supervision is first-class here, not deferred** — the orchestrator→subagent tree and "waiting-on-subagents" classification ship in 0.5.

## Workstreams

### A. WSL control agent + event spine
- Build the bundled Linux binary `termhub-agent` and the long-lived `wsl.exe -d <distro> -- termhub-agent --stdio` bridge over newline-delimited JSON (NDJSON), versioned protocol messages.
- Implement the **hook → WSL journal → agent → core → UI** path end-to-end. Hooks append to a durable WSL-side append-only journal (`EventJournalEntry`) and notify the agent; the agent forwards to the Windows core; the core fans out to UI events. Journal survives Windows app closure and is replayed on reconnect.
- Agent responsibilities for 0.5: tmux/session registry & commands, WSL metrics (RAM/swap/CPU/load/distro state/process counts), `git`/worktree queries, hook/status ingestion.
- **Watch-item from REVIEW:** the single stdio NDJSON pipe can head-of-line block (a bulk read stalls a metrics ping). Design request prioritization or a separate channel for bulk reads vs. control/metrics **now**, even if minimally.

### B. Claude adapter — identity, status, hooks
- Install TermHub hook handlers (with explicit consent — see Risks) for: `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, `StopFailure`, `PermissionRequest`, `Notification`, `Elicitation`, `SubagentStart`, `SubagentStop`, `TaskCreated`, `TaskCompleted`, `CwdChanged`, `WorktreeCreate`, `WorktreeRemove`.
- **Exact session-ID capture at `SessionStart`** (from `session_id` base field) → write/update `AgentSessionRecord`.
- Install the **status bridge**: receive Claude's JSON status data and persist the latest snapshot keyed by exact session ID (context %, usage, rate-limit block when present).
- Adapter exposes generic ops: discover, start-new, resume-exact, fork, get-status, get-context, get-rate-limits, verify-resumability. Use the Agent SDK for **import/resume/fork/verify only** — *not* as the metadata source (SDK returns enumeration + transcript but **not** summary/cwd/branch/first-prompt; derive those from the transcript or the local index).

### C. Parallel-agent supervision (pulled forward — priority)
- **Orchestrator→subagent tree model.** On `SubagentStart` (carries `agent_id`, `agent_type`) create a child node under the owning `session_id`; on `SubagentStop` mark it done. Maintain per-subagent state (running/completed) keyed by `agent_id`.
- **Task signals.** `TaskCreated`/`TaskCompleted` track outstanding background tasks per session.
- **"Waiting-on-subagents" classification (FR-012):** when a main agent's `Stop` fires but `agent_id`-scoped subagents or open tasks remain active, classify the orchestrator as **waiting-on-subagents**, *not* completed (PRD §5.6, §13 sequencing note). This is the headline status-model upgrade for 0.5.
- **Read-only tree view** in the sidebar/tile detail: orchestrator with its live/finished children and outstanding task count. (Deeper worktree mapping is 2.0; the *tree from events* is here.)
- **`Elicitation`** maps cleanly to the **needs-question** state — wire it as the canonical "agent needs input" signal alongside `Notification`/`PermissionRequest`.

### D. Status model (FR-012)
Represent and surface: working · waiting-on-subagents · needs-question · needs-permission · completed · failed · rate-limited · detached · restoring · expired. Map sources:
| State | Primary signal |
|---|---|
| working | `UserPromptSubmit` / active turn, no terminal `Stop` yet |
| waiting-on-subagents | `Stop` on main agent **while** `agent_id` children / tasks outstanding |
| needs-question | `Elicitation` (preferred) / `Notification` |
| needs-permission | `PermissionRequest` |
| completed | `Stop` with no outstanding subagents/tasks |
| failed | `StopFailure` / `SessionEnd` (abnormal) / `terminal://exit` non-zero |
| rate-limited | statusline `rate_limits.*` near limit / blocked turn |
| detached | tile closed, tmux alive (`TerminalState=detached`) |
| restoring | recovery in progress |
| expired | transcript missing on resumability check |

### E. Duplicate-resume guard — "lease, not latch" (priority hardening)
- Maintain a **live-attachment registry** keyed by exact session ID. Because hooks are global, even a non-TermHub `claude` fires `SessionStart` into the journal → detect **Live externally** (PRD §8.3).
- On a spawn/resume attempt against a session already marked live, offer **Focus existing / Fork** (`--fork-session`) instead of a second unsafe resume; hide raw override behind an explicit interleave warning.
- **Treat liveness as a lease** (REVIEW §5): heartbeat + TTL + **startup reconciliation against actual tmux/PID**. A crash with no `SessionEnd` must not strand a session as permanently "live" and block a legitimate resume. Make stale-liveness a first-class reconciliation case beside stale-PID.

### F. Workspace tabs + sidebar (the daily surface)
- Workspace tabs (sidebar-first; no permanent top strip): create/rename/reorder/duplicate-layout/delete-empty; move tiles between tabs **without process interruption**. Ctrl+Tab switcher overlay.
- Sidebar areas: workspace/session tree (provider, name, project, state, attention badges), attention queue (questions/failures/completed main turns → click switches tab + focuses tile + acknowledges), utility area (WSL health + Claude usage, low-priority).
- Session **label priority** (PRD §5.4): user/custom name → Claude summary → first-prompt summary → project folder + generic suffix; raw IDs in details only.

### G. Persistence + recovery review
- **Two-track persistence (§8.2):** active workspace snapshot vs. historical catalog kept distinct from day one (prevents reopen-floods-me).
- **Transactional autosave** after every material state change; commit within 500 ms.
- **App-reopen-while-alive (§6.5):** load snapshot, query the isolated tmux server, match by stable TermHub ID, reattach visible tiles, keep hidden-tab sessions detached, restore geometry/zoom/selection/sidebar/unread states.
- **Reviewed recovery after WSL/Windows restart (§6.6):** a review screen (not auto-open-everything) listing only last-snapshot or auto-recovery sessions; per-entry project/worktree, previous process, exact session ID, transcript availability, last activity, policy; Apply-to-all / grouped / one-by-one; **every attempted command + result written to the crash/recovery journal.** Frame restart recovery honestly as resume-conversation + restart-shell (carry-forward truth #1).

### H. Keyboard navigation + WSL health
- Keyboard-first tab/tile/session navigation (collision-test on Windows).
- Compact WSL RAM/swap/CPU/load/distro-state/process-count + warnings in the utility area (data from the agent).

## Key Claude Code mechanisms to use (verified)
- **Hooks:** the full 0.5 set above; **`SubagentStart`/`SubagentStop` (with `agent_id`)**, **`TaskCreated`/`TaskCompleted`**, **`Elicitation`** are the substrate for parallel-agent supervision and ship-forward into 0.5.
- **Base stdin fields** `session_id` (exact-ID capture) + `cwd` + `transcript_path` on every hook.
- **`SessionStart` `hookSpecificOutput.additionalContext`** available as an extra lever to seed project/worktree context (kept minimal in 0.5; full open-file injection is 1.5).
- **Statusline** `context_window.*`, `cost.*`, and `rate_limits.*` (with the Pro/Max + after-first-API-response caveat) for the usage display.
- **Session flags** `-r`/`--resume [id]` (project-dir+worktree scoped) and **`--fork-session`** for the Focus/Fork choice.
- **Agent SDK** `listSessions`/`getSessionInfo`/`getSessionMessages`/`resume`/`fork` for import/verify only.

## Data-model additions (PRD §8)
- `AgentSessionRecord` — populated: `provider`, `provider_session_id`, `terminal_id?`, `project_id`, `display_name`, `summary`, `transcript_path`, `created_at`, `last_activity_at`, `context_used_pct`, `resumability`, `live_attachment_state`, `provider_metadata`.
- `WorkspaceTab` (id, name, order, layout_mode, layout_json, zoom_default) and `TerminalRecord` (id, tab_id, tmux_server, tmux_session, project_id, cwd, shell, state, last_seen_at, close_behavior, recovery_policy, custom_command) — the snapshot-track schema.
- `EventJournalEntry` (timestamp, source, entity_id, event_type, payload, result) — the spine's durable log.
- **New (subagent supervision):** a `SubagentNode` association (parent `session_id`, `agent_id`, `agent_type`, state, started_at/ended_at) + an outstanding-task counter per session. *(Extends §8 to carry the pulled-forward tree.)*
- `ProjectRecord` minimal (id, root_path, repo_root, display_name, distro) to anchor sessions; full file index is 1.0.

## Exit criteria (PRD §14.2)
- Start two distinct Claude sessions in the same directory → shown as separate exact IDs with human-readable labels.
- Attempt to resume an already-live exact session → receive **Focus existing / Fork**, not a second unsafe resume.
- Receive + display question, main-turn-completed, and failure notifications in the tile **and** sidebar.
- After `wsl --shutdown`, open recovery review and resume **selected** exact sessions without opening every catalog record.
- Display Claude context + subscription usage when present, degrade gracefully when absent.
- **(Pulled-forward acceptance):** an orchestrator running subagents is classified **waiting-on-subagents** (not completed) until its `agent_id` children/tasks finish.

## Risks / watch-items (REVIEW)
- **Hook installation mutates `~/.claude/settings.json`** — onboarding promised not to touch the shell, but this *is* config editing. Require explicit consent, non-destructive merge (user may already have hooks), survive hand-edits, ship a clean uninstall.
- **"Live externally" stale-lock** if a crash skips `SessionEnd` — mitigated by the lease model (heartbeat+TTL+reconciliation); make it a tested reconciliation case.
- **NDJSON head-of-line blocking** — prioritize/separate bulk vs. control early.
- **SQLite durability** — enable WAL with an explicit `synchronous` setting; otherwise the 500 ms autosave guarantee is aspirational.
- **Restart over-trust** — UX copy must not imply a live process resumes after a VM teardown.
- **WebGL context ceiling looms** (only matters once many tiles are visible) — carry the V0.1 perf harness item ("12 WebGL contexts without loss") forward; plan canvas fallback for non-focused tiles.

---

# 1.0 — Daily driver

**PRD roadmap row:** "Remove routine dependence on VS Code/Notepad."

## Goal
Files open instantly for the selected worktree from a hydrated index; small edits save safely and atomically; historical sessions across projects are navigable; sounds and settings are in place. The parallel-agent tree from 0.5 is **hardened and persisted** (survives reconnect/replay).

## Workstreams

### A. Persistent file index (WSL-native)
- Build the index **inside WSL** (native Linux paths, gitignore rules, inotify). Index **names + metadata only, never contents**.
- Persist compact `FileIndexEntry` rows in SQLite; **hydrate into memory at startup**; update incrementally via inotify events through the agent.
- Ignore `.git`, dependency/build dirs, binary blobs, configurable patterns. Precompute normalized path + basename + extension + key-file flags for search.

### B. Worktree context + truth hierarchy (§10.4)
- Resolve the selected session's exact worktree/branch/cwd via: (1) Claude structured status + `CwdChanged`/`WorktreeCreate|Remove` hook events → (2) `git rev-parse` / `git branch --show-current` / `git worktree list --porcelain` verification in WSL → (3) optional TermHub/agent metadata for edge cases. A Claude-authored MD file is a *fallback* contract only, never primary.
- Selecting a terminal switches file context to that session's exact worktree + shallow default tree (§6.8).

### C. Search + tree views
- Fuzzy **basename/path/extension** search; scopes: current-folder · worktree (default) · repository · all-projects.
- Shallow tree + **Recent · Key Files · Pinned**; remember per-session/worktree navigation; folder expansion is UI state, not a rescan.

### D. Reader / editor
- Rendered **Markdown** read mode (default) + Source toggle; read-only source; editable source (CodeMirror 6); safe `.env` raw/structured editing.
- **Separate lightweight editor window first**, then temporary split (both required, §6.8/FR-017).
- Saves write through the WSL agent with **atomic replace** + external-change detection (visible to the running agent).

### E. Historical session catalog (§6.7)
- Catalog UI separate from the active snapshot; filters: provider · project · worktree · active · resumable · expired · archived · context · last-activity.
- Cheap metadata for **hundreds–thousands** of sessions; TermHub metadata may persist indefinitely; mark a session **non-resumable** if its transcript was cleaned up/moved (resumability check against `~/.claude/projects/.../<id>.jsonl`).

### F. Crash journal surfacing + notifications + settings
- Surface the event/crash journal (from the 0.5 spine) as a reviewable recovery/audit log.
- **Sounds:** Claude-asks-question, main-turn-completed, session-fails (defaults); **no** default rate-limit sound; per-project/per-event sounds; optional desktop notifications limited to when unfocused.
- **Settings:** keyboard binds, density, sidebar mode, optional top switcher, themes, sounds, recovery defaults, retention-warning, adapter settings.

### G. Subagent supervision — persistence + reconnect (continue from 0.5)
- Persist the `SubagentNode` tree + task counters so they **survive journal replay** and Windows app restart; reconcile on agent reconnect.
- Catalog/tile surfaces show historical "had N subagents" without needing the agents live.

## Key Claude Code mechanisms to use (verified)
- **`CwdChanged`, `WorktreeCreate`, `WorktreeRemove`** hooks feed the worktree truth hierarchy (tier 1).
- **Transcript path** (`transcript_path` base field) + `~/.claude/projects/<project>/<session-id>.jsonl` layout for resumability checks and metadata derivation (summary/first-prompt parsing the SDK won't give).
- **`cleanupPeriodDays`** (any settings.json scope; min 1) drives the resumable-vs-expired warning; onboarding may offer a longer retention value **with explicit consent only**.
- **Agent SDK** `getSessionMessages`/`getSessionInfo` for on-demand catalog import (not aggressive polling — memory behavior unbenchmarked).
- Continued **`SubagentStart/Stop` + `TaskCreated/Completed`** consumption for the persisted tree.

## Data-model additions (PRD §8)
- `ProjectRecord` (full: + `indexed_at`, `settings`), `WorktreeRecord` (id, project_id, path, branch, source, last_verified_at).
- `FileIndexEntry` (project_id, worktree_id, relative_path, kind, extension, modified_at, flags).
- `AgentSessionRecord.worktree_id` now populated; catalog filter indices.
- Persisted `SubagentNode` tree (from 0.5) + reconnect-reconciliation fields.

## Exit criteria (PRD §14.3)
- Selecting a terminal instantly loads the correct project/worktree file context from the hydrated index.
- Search `md`, `.env`, or a multi-token path query → relevant files under the target latency (~100 ms post-hydration).
- Open Markdown rendered + edit a config file in a TermHub editor window → atomic save visible to the running agent.
- Browse active/detached/resumable/expired historical Claude sessions across projects.
- Usable at **12 visible, 24 live/background, ≥1,000 catalog records** in the fixture.

## Risks / watch-items (REVIEW)
- **inotify exhaustion on big monorepos** (`max_user_watches`/`ENOSPC`, per-directory recursive watches) — handle `ENOSPC`, fall back to periodic reconciliation, surface an index-health indicator; add a manual Refresh.
- **WSL file-watcher gaps → stale index** — periodic low-priority reconciliation alongside the watcher.
- **NDJSON head-of-line blocking** now real (large file reads compete with metrics) — the 0.5 prioritization/separate-channel design must be in place.
- **Secret exposure via retention** — extending `cleanupPeriodDays` raises recovery value *and* local plaintext-secret exposure; the UI must explain the tradeoff (§10.2).
- **WebView2 WebGL ceiling (~16)** — at the 12-visible target with browser chrome consuming contexts, plan canvas fallback for non-focused tiles / pooled renderer; keep it on the perf harness.
- **Editor external-change conflict** — detect + reconcile (covered in UX tests).

---

# 1.1 — Night mode

**PRD roadmap row:** "Resume opted-in work after subscription reset." Ships **after** core V1 reliability is trusted (locked decision §17).

## Goal
A deliberately scheduled session resumes **exactly once, at the right time**, without duplicating a live session — surviving a TermHub restart and fully audited.

## Workstreams

### A. Rate-limit detection + overlay
- Detect rate-limit state and reset timestamp from **structured statusline data**: `rate_limits.five_hour.resets_at` / `seven_day.resets_at` + `*.used_percentage`.
- **Honor the verified caveat (PRD §6.10 step 1):** the `rate_limits` block exists only for Pro/Max and only **after the session's first API response**. The scheduler treats reset time as **initially unknown**, captures it once a session has made ≥1 call, and degrades gracefully when absent.
- On a session becoming blocked, show a **non-audio** overlay/badge asking whether to auto-resume after reset (no default rate-limit sound).

### B. Reset scheduler + guarded continuation
- Persist a `ScheduledAction` with exact session ID, terminal ID, project/worktree, reset time, **safety buffer**, and continuation prompt.
- **At execution, verify (§6.10 step 4):** session is **not already live elsewhere** (the 0.5 lease registry), project not paused, **transcript still exists**.
- Recreate or attach the terminal → **resume the exact session** (`-r <id>`) → wait for readiness → submit the configured continuation message.
- **Single-execution idempotency key** so it fires exactly once even across restarts/replays.
- Controls: cancel, snooze, apply-to-all, global night-mode presets.

## Key Claude Code mechanisms to use (verified)
- **Statusline** `rate_limits.five_hour.resets_at` / `seven_day.resets_at` (Unix epoch) + `used_percentage` — with the **Pro/Max + after-first-API-response** caveat as a hard design constraint.
- **`-r`/`--resume <id>`** with ID scoped to project dir + worktrees (capture the right cwd at schedule time).
- **Live-attachment lease** (from 0.5) as the "not already live" precondition.
- Hook **`SessionStart`** firing on the resumed session confirms readiness before sending the continuation prompt.

## Data-model additions (PRD §8)
- `ScheduledAction` (id, terminal_id, agent_session_id, execute_at, action, prompt, state) — fully realized, plus a **safety-buffer** field and an **idempotency key**.
- Persisted reset-time capture on `AgentSessionRecord.provider_metadata` (since it's unknown until first API response).

## Exit criteria (PRD §14.4)
- A rate-limited session offers a scheduled resume at the structured reset time with a configurable buffer.
- Restarting TermHub does **not** lose the schedule.
- The scheduler **refuses** to run when the session is already live, the transcript is unavailable, or the schedule was cancelled.
- Successful continuation occurs **exactly once** and appears in the event journal.

## Risks / watch-items (REVIEW / PRD §16)
- **Reset time unknown until first API response** — the dominant correctness risk; never schedule against a guessed time; show "reset time pending" state.
- **Automation surprises** — opt-in per session, confirmation tiers, audit journal, cancellation, single-execution idempotency.
- **Duplicate exact session at fire-time** — the lease check + startup reconciliation must be solid before night mode is trusted (this is *why* 1.1 follows core reliability).
- **Fork permission caveat** — if a continuation ever forks, "approved for this session" permissions do **not** carry into the fork; prefer exact resume for night mode.

---

# 1.5 — Automation and preview

**PRD roadmap row:** "Integrate external browser and agent control." Gated on core reliability holding (locked §17).

## Goal
One managed preview page per project/session is reused reliably with background reload; Claude can organize TermHub within permission boundaries via MCP; open-file context is injected safely; process attribution improves.

## Workstreams

### A. Managed Chromium preview
- Launch a dedicated **Chrome for Testing / Chromium** profile (modern Chrome 136+ requires a **non-default user-data dir** for remote debugging — isolated profile, never the user's personal Chrome).
- **On-demand Playwright/Node sidecar** (bundled, started only when needed so it adds no idle memory).
- Maintain **one managed page per preview target**; store a stable preview-target ID + browser page/target ID; **reload in the background by default**, bring to foreground only on explicit Preview or opt-in rule.
- Preview discovery priority (§6.9): explicit TermHub declaration → Claude/footer link metadata → terminal URL detection → process/port association → saved project setting.

### B. MCP command surface
- Local MCP server backed by the **same internal command bus** (no fragile UI automation).
- **Permission tiers (§11.2):** Read (allowed) · Organization (allowed + visible audit event) · Process-changing (confirmation required) · Destructive (denied unless explicitly enabled; confirmation still required) · Secret-bearing (denied, never returned implicitly).

### C. Open-file context injection (§10.7)
- Associate the open file + optional selection with the exact Claude session; inject **only path/selection metadata** via **`UserPromptSubmit` `hookSpecificOutput.additionalContext`** — never auto-copy file or secret contents, never rewrite CLAUDE.md.

### D. Process attribution improvements
- Better association of TermHub/Claude/Node processes and ports to sessions (feeds preview discovery + WSL health), short of the full 2.0 worktree/subagent mapping.

## Key Claude Code mechanisms to use (verified)
- **`UserPromptSubmit` `additionalContext`** for open-file injection (verified supported); also available on `SessionStart`, `PreToolUse`/`PostToolUse`, `Stop`, etc. if seeding is wanted.
- MCP server consuming the internal command bus; permission tiers enforced in the bus, not the UI.
- Statusline/hook data continues to feed health + attribution.

## Data-model additions (PRD §8)
- Preview target entity: stable `preview_target_id`, source (declared/detected/port), URL, managed browser page/target ID, last-reload-at.
- MCP audit entries → `EventJournalEntry` (organization actions emit visible audit events).
- Open-file-context association (session_id, file path, selection) for injection.

## Exit criteria (PRD §13)
- One preview page per project/session is **reused reliably** (no duplicate launches; background reload works).
- Claude can **organize TermHub within permission boundaries** (Read/Organization without prompts; Process-changing with confirmation; Destructive/Secret-bearing denied by default).

## Risks / watch-items (REVIEW / PRD §11.3, §16)
- **Terminal escape-sequence hardening** — beyond URL sanitization, explicitly gate **OSC 52 (clipboard write)** and **window-title injection** from untrusted agent output (real exfiltration/spoofing vectors in xterm.js).
- **Misleading/non-local URLs** — confirm before opening non-local or unexpected origins; sanitize footer/link metadata.
- **Managed-browser profile isolation** — must be a separate profile; never touch the user's Chrome data dir.
- **MCP destructive/process-changing calls** — confirmation tiers + audit journal are the safety net; test the denied-by-default paths.
- **Sidecar idle cost** — keep Playwright/Node off until needed; verify it doesn't regress the memory posture.

---

# 2.0 — Agent operations

**PRD roadmap row:** "Handle deeper context and parallel structures." This is where the *deeper* parallel work lands — the read-only awareness was already pulled into 0.5/1.0 per the verified sequencing note.

## Goal
TermHub can explain and transition complex parallel agent work **without relying on terminal motion** — context-threshold handoff policies, automated subagent/worktree mapping, additional provider adapters, and API-usage dashboards.

## Workstreams

### A. Context threshold + handoff policies
- Watch `context_window.*` usage; at configurable thresholds, surface/auto-execute handoff policies (e.g., summarize-and-fork, checkpoint, or prompt-for-handoff) — built on the now-trusted exact-resume + `--fork-session` machinery.

### B. Automated subagent ↔ worktree mapping (deeper than 0.5)
- Move beyond the read-only event tree: correlate `SubagentStart` (`agent_id`/`agent_type`) + `WorktreeCreate/Remove` + `git worktree list --porcelain` to **map each subagent to its worktree/branch automatically** and present the parallel structure spatially (not via terminal switching).
- This is the part §13 *keeps* at 2.0 ("automated worktree mapping"); the 0.5 tree is its foundation.

### C. Additional provider adapters
- Add Codex / other terminal agents through the existing adapter interface (workspace, PTY, persistence, file, preview, notification systems stay generic — agent-agnostic core, §2.4).

### D. API usage dashboards
- Aggregate `cost.*` + `rate_limits.*` + context usage across sessions into dashboards (per-project/per-provider), distinct from the per-session statusline display.

## Key Claude Code mechanisms to use (verified)
- **`context_window.*`** statusline usage for threshold/handoff triggers.
- **`--fork-session`** / `/branch` for handoff-by-fork (mind the non-carrying "this session" permissions).
- **`SubagentStart/Stop` (`agent_id`, `agent_type`) + `WorktreeCreate/Remove`** correlated with `git worktree list --porcelain` for automated mapping.
- **`cost.*` + `rate_limits.*`** aggregation for dashboards.
- **Worktree-per-agent + `--fork-session`** as the canonical pattern for spawning isolated parallel work (each forked agent in its own worktree).

## Data-model additions (PRD §8)
- Context-policy config per project/session; handoff/checkpoint records (link parent↔child session IDs).
- `SubagentNode` extended with resolved `worktree_id` (automated mapping); subagent↔worktree correlation table.
- Provider-adapter registry rows (generalize `AgentSessionRecord.provider`).
- Usage-aggregation rollups (period, provider, project, cost, context).

## Exit criteria (PRD §13)
- TermHub can **explain and transition complex parallel agent work without relying on terminal motion** (handoffs, subagent/worktree map, dashboards all functional).

## Risks / watch-items (REVIEW / PRD §16)
- **Scope creep into IDE** — context policies and dashboards must stay supervisory; keep language servers, Git UI, debugging, extension systems as hard non-goals (§3.3, §16).
- **Claude integration drift** — versioned adapter + capability detection + changelog tests + graceful fallback to transcript metadata, since 2.0 leans hardest on hook/statusline schemas.
- **Automation surprises at scale** — handoff policies are automation; keep them opt-in with audit + cancellation + idempotency (same discipline as night mode).
- **Subagent attribution is explicitly imperfect in early releases (§3.3)** — automated mapping is best-effort; surface uncertainty rather than asserting false precision.

---

## Appendix — cross-cutting watch-items (apply to every phase)

| Item | Source | Standing mitigation |
|---|---|---|
| Process durability = app-close only; conversation = reboot-survivable | REVIEW §1 | Journal is reconstruction authority; honest restart UX. |
| Liveness is a **lease**, not a latch | REVIEW §5 | Heartbeat + TTL + startup reconciliation vs. tmux/PID. |
| Hook install edits `~/.claude/settings.json` | REVIEW | Explicit consent, non-destructive merge, survive hand-edits, clean uninstall. |
| WebGL context ceiling (~16) in WebView2 | REVIEW §3 | Canvas fallback for non-focused tiles; perf-harness gate from 0.1. |
| Resize / SIGWINCH + smallest-attached-client sizing | REVIEW §4 | Dedicated resize test; `window-size latest`; detached hidden clients must not shrink the active pane. |
| NDJSON single-pipe head-of-line blocking | REVIEW | Request prioritization / separate bulk channel. |
| SQLite durability | REVIEW | WAL + explicit `synchronous`; verify abrupt-termination survival. |
| inotify `ENOSPC` on monorepos | REVIEW | Handle ENOSPC; periodic reconciliation; index-health indicator. |
| OSC 52 / window-title injection from agents | REVIEW | Gate clipboard writes + title changes from untrusted output. |
| Rate-limit block absent until first API response (Pro/Max only) | REVIEW / PRD §6.10 | Treat reset time as unknown; capture after first call; degrade gracefully. |
| Agent SDK metadata is thin | REVIEW §10.3 | Own lightweight index; derive summary/cwd/branch/first-prompt from transcripts. |
| End-to-end keypress latency hides the WSL hop | REVIEW §4 | Measure keypress→on-screen-echo including WSL, even with a looser target. |
