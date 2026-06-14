# TermHub (T-Hub) — Audit & Next-Direction Analysis

**Audited:** 2026-06-14 · **App version:** 0.1.16 · **Branch:** `audit/analysis` (isolated worktree)
**Scope:** stale code / doc drift · correctness bugs (Rust + React/TS) · strategic next direction · competitive positioning.
**Reviewer:** Claude (Opus 4.8), read-only. App was not run; findings are from code + docs.

All file references are absolute. Severity: **P0** (broken / data-loss / security) · **P1** (real bug or
dead user-facing feature) · **P2** (latent / low-blast-radius / polish).

---

## Executive summary

TermHub is in genuinely good shape internally: the supervision reducer, hook install/uninstall, the
agent-bridge emit spine, the SQLite layer (WAL + history ring), and the files index are all well-factored
and **heavily unit-tested**. The biggest problems are not crashes — they are **dead wiring** and **drift
between what the app advertises and what it does**:

1. **Clicking a session in the sidebar still does nothing** — the #1 punch-list item from the v0.1.5
   handoff is still unfixed at v0.1.16. `onSelectSession` is wired to discarded React state.
2. **3 of the 20 advertised MCP tools always fail** (`get_theme`, `set_theme`, `spawn_terminal`) —
   the MCP server advertises them but the control dispatcher refuses them, and the theme refusal is now
   factually wrong (the Tauri theme commands exist).
3. **The docs are ~11 patch releases behind the code.** `docs/HANDOFF.md` and `docs/SESSION_AWARENESS.md`
   describe a v0.1.5 app whose sidebar, hooks location, and feature set no longer exist as written.
4. **A handful of real-but-low-blast-radius bugs** (UTF-8 byte-slice panics on `id`, recovery-history
   ring churned by trivial saves, an unguarded file-write command).

Strategically, the supervision substrate is the crown jewel and the product is *under-using* it. The
highest-leverage next moves are: make supervision **navigable** (close the dead click), make the
attention queue the **center of gravity**, and turn the rich-but-hidden status data into a **cross-session
dashboard** — these convert the already-built backend into the differentiated "many-agents cockpit" the
PRD promises.

### Top 5 prioritized next steps

| # | Step | Why it's highest-leverage | Effort |
|---|------|---------------------------|--------|
| 1 | **Wire `onSelectSession`** → switch to the tab + focus the tile for a clicked Attention/Session row. | The entire supervision sidebar is *read-only-and-inert* today; this single fix makes the headline 0.5 surface actually drive navigation. The backend already correlates session↔terminal by cwd. | S |
| 2 | **Repair the MCP↔control contract**: implement `get_theme`/`set_theme` control handlers (Tauri commands already exist) and decide `spawn_terminal` (implement-with-confirm or drop from advertised tools). | "Claude can drive TermHub" is a flagship claim; 15% of the toolset is dead. Either honor it or stop advertising it. | S–M |
| 3 | **Promote the Attention queue to a first-class, keyboard-driven surface** (global jump-to-next-attention hotkey, persistent badge, ack flow). | This is the actual job-to-be-done ("supervise many agents"); the data is already flowing (`session://status`, `attentionSessions`). | M |
| 4 | **Cross-session usage dashboard** (aggregate context/cost/rate-limit across all sessions, with reset-time countdown). | The statusline data is ingested and per-session-rendered but never aggregated into the "are any of my agents about to stall?" view that justifies a multi-agent cockpit. | M |
| 5 | **Bring the docs to v0.1.16 + add a lease-based liveness / duplicate-resume guard** (the PRD's exact-session-ID safety story, still unbuilt). | Docs drift is actively misleading the next agent; the duplicate-resume guard is the PRD's signature safety feature and a real footgun (interleaved transcripts) that is currently unaddressed. | M |

---

## 1. Stale code / doc drift

### 1a. Docs predate the code by ~11 releases (P1 — actively misleading)

The code is at **0.1.16**; the docs describe **0.1.5**.

- **`docs/HANDOFF.md`** (line 3): "App version: `0.1.5`", "@ `5fb33c2`". Its entire §5 punch-list, §4
  "in flight", and §0 "the one thing to do next" describe a v0.1.5 world. Several listed "next steps"
  have since shipped (label accuracy, sidebar terminal-row click, file tree height). It does **not**
  mention any of: customizable per-event hook install, the WSL-bottom status strip, file editing/save,
  the right-click tile menu, the shell plugin / "open externally", the Theme editor, or Recovery review.
  **§0's "do next" (wire the session click) is, ironically, still correct** — see bug 2a.
- **`docs/SESSION_AWARENESS.md`** (line 96): "The **HookInstallPanel** is mounted in the sidebar."
  **False since v0.1.12.** Hooks moved to Settings → Hooks (`src/components/Sidebar.tsx:363` comment
  "Hooks moved to Settings → Hooks"; the panel is now mounted only via
  `src/components/ThemeEditor.tsx:560`). It also says the live status spine is "LIVE" — accurate — but
  the install location is wrong, which is the one thing a user needs to find.
- **`README.md`** still describes the app as the "0.1 playable proof nucleus (scaffolded)" with a
  "7 Tauri commands" surface. The real surface is ~30 commands across 12 modules. The repository-layout
  block omits `agent/`, `claude/`, `control.rs`, `db.rs`, `files.rs`, `theme.rs`, `supervision.rs`, etc.
- **`docs/PLAN.md`** is internally consistent as a *plan* but never annotated with what actually
  shipped, so it reads as forward-looking when ~all of 0.5 (workstreams A–H) is in fact built. A
  reader can't tell built-from-planned. Recommend a "Status: shipped in 0.1.x" stamp per workstream.
- **`docs/BACKBURNER.md`** is the only doc that is current and accurate (snap-flyout + web-preview).

**Action:** rewrite HANDOFF for the v0.1.16 state; fix the SESSION_AWARENESS hook-location line; refresh
README's status + layout; stamp PLAN workstreams shipped/planned.

### 1b. MCP server advertises tools the control channel refuses (P1 — dead features)

`src-tauri/crates/termhub-mcp/src/tools.rs` advertises **20** tools; `src-tauri/src/control.rs`'s
`dispatch()` (line 297) is the real executor. Three advertised tools can never succeed:

- **`get_theme` / `set_theme`** — `control.rs:333-336` returns *"the theme command handler is not wired
  in this build yet (parallel theme track)"*. **This is now factually stale**: `theme::get_theme` /
  `theme::set_theme` Tauri commands are registered in `src-tauri/src/lib.rs:261-262` and fully
  implemented in `src-tauri/src/theme.rs`. The control channel just needs handlers that read/write
  `ThemeState` (it would need the `ThemeState` handle threaded into `ControlContext`, which today only
  carries status/supervisor/files).
- **`spawn_terminal`** — `control.rs:327` hard-gates it off (`gated_process_change`), yet the MCP server
  advertises it and its description says "confirmation required." A user enabling it via Claude gets a
  flat refusal. Decide: implement spawn-with-confirm, or remove it from `tools.rs` so the advertised
  surface matches reality.

`list_tabs` (`control.rs:437`) also returns a permanent empty list with a "not yet wired" note even
though the workspace tab store is now real — it could read the live tabs (modulo the
control-channel-has-no-store constraint; this would need a sink/read path like the apply-sink).

### 1c. Dead state / unused wiring (P1 + P2)

- **`onSelectSession` chain is dead** (P1). `src/App.tsx:164` `const [, setSelectedSession] = useState(...)`
  discards the value; `App.tsx:339` passes `setSelectedSession` as `onSelectSession`; `Sidebar.tsx:308`
  and `:346` call it on every Attention-row and Session-row click. Net effect: **clicking a supervised
  session or an attention item does nothing.** (Full write-up in §2a.)
- **`capture_visible` is dead** (`src-tauri/src/tmux.rs:385`, `#[allow(dead_code)]`) — retained "for
  potential visible-only reattach seeding" but never called.
- **`files.rs` carries a module-wide `#![allow(dead_code)]`** (`src-tauri/src/files.rs:30`) and
  `claude/hooks.rs` (line 20) / `claude/install.rs` (`install_hooks`, line 212) have function-level
  allows for the "subagent will wire this later" era that has since passed. Worth re-checking whether
  these are genuinely reachable now and removing the blanket allows so dead code surfaces again.
- **`agent/mod.rs:300`** hardcodes `core_version: "termhub 0.5.0"` in the Hello handshake — drifts from
  the real 0.1.16 version. Cosmetic, but it's the version the agent logs see.

### 1d. Recovery history ring is churned by trivial saves (P1 — feature half-defeated)

`src-tauri/src/db.rs:174` `append_snapshot` runs on **every** `save_workspace_snapshot`, and the
frontend's `persist()` (`src/store/workspace.ts`) fires on focus change, zoom, drag, tab switch, etc.
With `SNAPSHOT_HISTORY_CAP = 20` (db.rs:64), the 20-slot recovery ring is dominated by near-identical
focus/zoom snapshots, so the Recovery-review UI can't actually "rewind to the last good *layout*" —
the good arrangement scrolls out of the ring within a few minutes of normal use. **Fix options:** only
append when the *structural* layout (tabs/order) changed (dedup against the last appended JSON), or
append on a debounce, or raise the cap. The intent in the module doc ("roll back across a bad session")
is not met by the current trigger frequency.

---

## 2. Bugs (correctness)

### 2a. Sidebar session/attention click is a no-op — the dead `onSelectSession` (P1)

- **Where:** `src/App.tsx:164,339`; consumed at `src/components/Sidebar.tsx:308,346`.
- **Bug:** `setSelectedSession` is the setter of a `useState` whose value is never read (`const [, set...]`).
  Every Attention-queue row and every Sessions-tree row calls `onSelectSession?.(sessionId)`, which sets
  unread state and does nothing observable. This is the exact item flagged as "the one thing to do next"
  in HANDOFF §0/§5.1 — still open 11 patches later.
- **Suggested fix:** in `App.tsx`, replace the dead setter with a handler that resolves the session→
  terminal (the cwd correlation already exists via `terminalForCwd` in `store/workspace.ts`, and
  `controlBridge.applyControl`'s `focus_session` case already implements the exact "find owning tab →
  setActiveTab → setFocus" logic). Factor that into a `focusSession(sessionId)` store action and call it
  from both the dead App wiring and `controlBridge`, removing the duplication.
- **Severity:** P1 (headline supervision surface is inert).

### 2b. UTF-8 byte-slice panic on terminal id (P2)

- **Where:** `src-tauri/src/commands.rs:222` (`attach_terminal`) and `:344` (`kill_terminal`):
  `format!("th_{}", &id[..id.len().min(8)])`.
- **Bug:** slicing a `&str` by byte index panics if byte 8 lands inside a multi-byte UTF-8 char. The
  command runs on a Tauri worker; a panic poisons it / surfaces as an opaque failure. IDs are
  app-minted hex today (`spawn_terminal` uses `uuid.simple()[..8]`, all ASCII), so this is latent — but
  `attach_terminal`/`kill_terminal` take `id` as untrusted input from the frontend (or a replayed
  persisted layout from before the #16 id change).
- **Suggested fix:** use a char-safe truncation (`id.chars().take(8).collect::<String>()`) or just key
  off the full id (the session name was minted from it). Note `attach_terminal` already does
  `&id[..id.len().min(8)]` *inside* an existing-session lookup branch where the live entry's
  `tmux_session` is preferred — only the `None` fallback path hits the slice.
- **Severity:** P2.

### 2c. `read_text_file` / `write_text_file` have no path confinement (P1/P2 — security posture)

- **Where:** `src-tauri/src/files.rs:951` (`read_text_file`), `:962` (`write_text_file`), plus the
  control-channel `open_file` → `control_read_text` (`files.rs:897`, `control.rs:479`).
- **Bug:** both `normalize()` any caller-supplied path and read/write it anywhere the process can reach
  (any WSL path via the UNC mapping, any Windows path). `write_text_file` is frontend-only today (not on
  the control channel), but `open_file` (which reads arbitrary file contents) **is** exposed over MCP as
  an "Organization (audited)" tool — i.e. a remote agent can read any file on disk, including secrets,
  with no root confinement. REVIEW.md explicitly calls out "Secret-bearing: denied, never returned
  implicitly" as a permission tier; `open_file` violates it.
- **Suggested fix:** confine reads/writes to a configured set of project roots (or at least reject paths
  outside the active workspace cwds), and gate `open_file` behind the secret-bearing tier per
  REVIEW.md §11.2. At minimum, refuse obvious secret paths (`.env`, `id_rsa`, `.ssh/`, credentials).
- **Severity:** P1 for the MCP `open_file` exfiltration path; P2 for the local-only write.

### 2d. `SessionEnd` can wrongly mark a finished session "failed" (P2 — UX correctness)

- **Where:** `src-tauri/src/supervision.rs:177-184`.
- **Bug:** `SessionEnd` downgrades to `Failed` unless the session is already `Completed`. A session that
  ended cleanly while in `WaitingOnSubagents` (main Stop, children still wrapping up) or that never
  emitted a terminal `Stop` (user quit Claude normally) is classified **Failed**. The code comment
  acknowledges "we can't always distinguish." This will mislabel ordinary session exits red in the
  attention queue (and `attentionSessions` surfaces `failed`), training the user to ignore the red badge.
- **Suggested fix:** treat `SessionEnd` as terminal-but-not-failed by default (e.g. a `Detached`/`Ended`
  state), and only mark `Failed` when paired with a `StopFailure` or a non-zero exit signal. The PRD's
  FR-012 has distinct `failed` vs `completed`/`detached` states for exactly this.
- **Severity:** P2 (false-alarm noise, erodes trust in the queue).

### 2e. Status/title correlation is cwd-based and silently ambiguous (P2 — known, by design)

- **Where:** `src/store/workspace.ts:1265` `terminalForCwd` (and the `agent://title` subscription).
- **Bug/limitation:** TermHub terminals are keyed by tmux id; Claude sessions by `session_id`. The only
  link is the working directory. When two terminals share a cwd (very common: two agents in the same
  repo — the literal core use case), the correlation returns `null` and **no title/status reaches the
  tile at all**. So in the multi-agent-in-one-repo scenario the product is built for, tile labels and
  the session↔tile link degrade to nothing. HANDOFF §4 acknowledges the ambiguity.
- **Suggested fix:** capture the exact Claude `session_id` at spawn time. When TermHub spawns
  `claude`/`claude --resume`, it controls the pane; it can inject/recover the session id (e.g. read the
  newest `~/.claude/projects/<proj>/<id>.jsonl`, or have the `SessionStart` hook's cwd+pid map back to
  the tmux pane via `pane_info`). This is the substrate for the duplicate-resume guard too (§3).
- **Severity:** P2 (degrades exactly the headline scenario; not a crash).

### 2f. `pane_info` over `wsl.exe` is one `bash -lc` per `list_terminals` poll (P2 — perf)

- **Where:** `src-tauri/src/tmux.rs:304` `pane_info` + `commands.rs:369`.
- **Note:** every `list_terminals` shells a full `wsl.exe -- bash -lc 'tmux …'` (login shell). If the UI
  polls `list_terminals` on an interval (to keep cwd/labels live — `updateTerminalsMeta` exists for
  exactly this), each poll pays a login-shell + WSL-hop cost. Not a bug, but at the "12 visible / 24
  live" target this is a recurring multi-hundred-ms cost. Consider a single long-lived query path
  through the agent bridge (which is already a persistent stdio process inside WSL) instead of a fresh
  `wsl.exe` per poll.
- **Severity:** P2 (scales poorly; the agent bridge is the right home for this).

### Things that are *correct* (verified, not bugs)

- The supervision reducer (`supervision.rs`) — FR-012 classification incl. `WaitingOnSubagents` and the
  Stop→child-finish→Completed transition — is correct and well-tested.
- Hook merge/remove (`claude/hooks.rs` + `install.rs`) — non-destructive, idempotent, atomic write,
  one-time backup, consent-gated, marker-tagged uninstall. Genuinely solid; matches REVIEW's hardening ask.
- SQLite (`db.rs`) — WAL + `synchronous=NORMAL` per REVIEW; atomic append+trim transaction; None-backed
  degrade. Good.
- tmux `list_sessions` avoids the `-F '#{…}'` wsl.exe comment-eats-`#` trap; `pane_info` correctly
  single-quotes the format inside `bash -lc`. The hard-won gotcha is respected.
- `journal_cursor` is monotonic (won't regress on out-of-order seq) — tested.
- Control channel: loopback-only, per-launch token, `0600` handshake file, token checked before any
  dispatch. Reasonable for a local channel.

---

## 3. Next direction (strategic, prioritized)

The product's differentiator is **supervising many agent sessions**, and the supervision/journal/status
substrate is already built and tested. The gap is that this substrate is **under-surfaced and
non-interactive**. Prioritized by leverage (built backend → unlock with small frontend):

### P0-strategic — Make supervision *navigable* (close the dead loop)
Fix §2a so a click on a session/attention row reveals its tile. Without this, every other supervision
investment is invisible. Then: a **global "jump to next agent that needs me" hotkey** that walks the
attention queue (needsQuestion/needsPermission/failed/rateLimited/completed). This is the single feature
that turns "many terminals" into "supervised many terminals." Rationale: the whole value prop is
*attention routing*; the data (`attentionSessions`, `session://status`) is already live.

### P1-strategic — The attention queue as center of gravity
Today it's one collapsed accordion section among five (`Sidebar.tsx`). For a supervisor running 12
agents, the queue *is* the app. Make it: always-visible, sorted by urgency, with inline ack and a
count badge in the titlebar/tray. Add desktop toasts (HANDOFF §5.6 already scoped the Tauri
notification plugin — still unbuilt). Rationale: directly serves the core loop; mostly UI over existing state.

### P1-strategic — Exact-session-ID identity + duplicate-resume guard
The PRD's signature safety feature (interleaved-transcript footgun) is **not built**. Correlation is
cwd-based and ambiguous (§2e). Capture the real `session_id` at spawn/resume, maintain a live-attachment
**lease** (heartbeat + TTL + startup reconciliation vs tmux/PID, per REVIEW §5), and offer
**Focus existing / Fork** instead of an unsafe second resume. This is both a correctness fix (§2e) and
the thing that makes TermHub *safe* for the parallel-agent workflows the README pitches. Rationale:
highest *trust* leverage; everything downstream (night-mode resume, handoff policies) depends on it.

### P1-strategic — Cross-session usage dashboard
`StatusSnapshot` (context %, cost, 5h/7d rate-limit windows) is ingested per session and shown
per-session (`UsageLine`) and crudely aggregated (`UsageSummary` peak/sum). Build the real thing: a
panel that answers "which of my agents is about to hit a context wall or a rate-limit reset, and when?"
with reset-time countdowns (`rate_limits.*.resets_at`). This is the "cockpit" justification. Rationale:
data is already flowing; turns raw numbers into the decisions a supervisor actually makes.

### P2-strategic — Agent-agnostic seam (the stated long-term goal)
The README aims to "become agent-agnostic," and the architecture is *almost* there (generic journal,
supervision reducer, status bridge), but everything is hardcoded to Claude hooks. Define the adapter
trait now (discover/start/resume/fork/status/events) even with only the Claude impl, so adding Codex/
others later (PLAN 2.0) is a new impl, not a refactor. Rationale: cheap to define now while the seams
are fresh; expensive to retrofit after more Claude-specific code accretes. Do **after** the identity/
lease work, since that's the part most likely to need per-provider logic.

### P2-strategic — Persist the supervision tree
`Supervisor` is in-memory only (`agent/mod.rs`). It rebuilds from journal replay on reconnect (good),
but a Windows app restart with a fresh agent loses live state until replay; and there's no historical
"this session had N subagents" record (PLAN 1.0 §G). Persist `SubagentNode`/task counters to SQLite.
Rationale: needed for the historical catalog + recovery-review-of-agents, but lower urgency than the
interactive surfaces above.

---

## 4. Competitive analysis

TermHub sits in a crowded-but-unconsolidated space. What comparable tools do that TermHub doesn't, and
where the differentiated opening is:

| Tool / class | What it does well that TermHub lacks | Relevance to TermHub |
|---|---|---|
| **tmux / Zellij** | Battle-tested multiplexing, session persistence, plugins, broadcast-input to many panes, layouts-as-config. Zellij has a polished floating-pane/tab UX. | TermHub *rides* tmux, so it inherits persistence — but it lacks **broadcast input** (send one prompt to N agents) and **layout presets**, both natural for a multi-agent driver. Broadcast-prompt is a clear quick win the others can't match (they're not agent-aware). |
| **VS Code + agent extensions (Cline, Roo, Copilot, Continue)** | Full editor, diff review, inline file edits, language servers, git UI, extension ecosystem, MCP support. | TermHub deliberately is *not an IDE* (good — don't chase this). But VS Code agent panels have **no multi-session supervision** — they're single-conversation. TermHub's tree/attention-queue is exactly what they lack. Keep the file viewer minimal; do not grow toward LSP/debugger (PRD non-goal). |
| **Warp** | Best-in-class terminal UX: blocks, AI command suggestions, command palette, beautiful rendering, Warp Drive (shared workflows), now multi-agent "Warp Agents". | Warp is the closest competitor and is moving INTO multi-agent. Its weakness: it's its own terminal/agent, not a supervisor of *your* Claude Code sessions with *your* hooks/transcripts. TermHub's moat is **deep Claude Code integration** (real hooks, exact session IDs, transcript-aware resume, rate-limit/context from the real statusline). Lean into that — Warp can't supervise an external `claude` process's lifecycle. |
| **Conductor / multi-agent orchestration UIs (Conductor, Crystal, Claude Squad, vibe-kanban)** | Worktree-per-agent automation, parallel-task kanban boards, spawn-N-agents-on-a-task, diff-review-and-merge flows, run agents on isolated git worktrees. | This is TermHub's *direct* competitive set and the fastest-moving. They typically **auto-create a git worktree per agent** and present a board/diff-merge workflow. TermHub has the supervision tree but **no worktree automation, no spawn-on-task, no diff/merge surface** (PLAN 2.0 defers worktree mapping). This is the biggest feature gap vs. the closest rivals. |
| **Agent dashboards (LangSmith, Langfuse, Helicone, AgentOps)** | Cross-run cost/latency/token dashboards, traces, eval, alerting. | These are observability for *API* agents, not interactive terminal supervision. TermHub's per-session statusline data could become a lightweight **local** version of this (cost/context/rate-limit over time) without the cloud — a differentiator for privacy-conscious solo devs. |

### Where the differentiated opportunity is

1. **"Supervisor of your own Claude Code, not yet-another-agent."** Every competitor either *is* the
   agent (Warp, Cline) or orchestrates API calls. TermHub uniquely supervises the *real* `claude`
   process with its *real* hooks, transcripts, rate limits, and resume semantics. No one else does
   exact-session-ID lifecycle + duplicate-resume safety + transcript-aware recovery. **This is the moat —
   and it's exactly the part that's least finished (§3 identity/lease).** Finishing it is both the
   bug-fix and the differentiator.

2. **Attention-routing for many agents.** The Conductor-class tools focus on *spawning* parallel work;
   TermHub's tree+attention-queue is better positioned for *supervising* long-running ones (which needs
   you, which is blocked, which is burning rate limit). Make the attention queue great (§3 P1) and that's
   a distinct lane.

3. **Worktree automation is the table-stakes gap to close.** To compete with Conductor/Crystal/Claude
   Squad, TermHub needs at least "spawn an agent in a fresh worktree" and a place to see/merge its diff.
   The hooks already emit `WorktreeCreate/Remove` and the agent can run `git worktree`; the substrate is
   there, the UX isn't. This is the clearest "catch up to rivals" item, distinct from the "lean into the
   moat" items above.

4. **Don't chase IDE parity.** VS-Code-agent-extensions own the editor; the PRD's "not an IDE" boundary
   is correct and defended. The file viewer/editor should stay a convenience, not a roadmap centerpiece.

---

## Appendix — file map of findings

- **Dead session click:** `src/App.tsx:164,339`; `src/components/Sidebar.tsx:308,346`;
  fix reuses logic in `src/ipc/controlBridge.ts:83` and `src/store/workspace.ts:1265`.
- **MCP↔control drift:** `src-tauri/crates/termhub-mcp/src/tools.rs:277-392` (advertised) vs
  `src-tauri/src/control.rs:297-343` (executed); theme commands real at `src-tauri/src/lib.rs:261` /
  `src-tauri/src/theme.rs`.
- **UTF-8 slice:** `src-tauri/src/commands.rs:222,344`.
- **File path confinement:** `src-tauri/src/files.rs:897,951,962`; `src-tauri/src/control.rs:479`.
- **SessionEnd→Failed:** `src-tauri/src/supervision.rs:177-184`.
- **Recovery ring churn:** `src-tauri/src/db.rs:64,174,322`; `src/store/workspace.ts:522` (persist freq).
- **Stale docs:** `docs/HANDOFF.md:3`, `docs/SESSION_AWARENESS.md:96`, `README.md` (status + layout),
  `docs/PLAN.md` (no shipped/planned stamp).
- **Hardcoded version:** `src-tauri/src/agent/mod.rs:300`.
- **Supervision in-memory only:** `src-tauri/src/agent/mod.rs:122` (`supervisor: Mutex<Supervisor>`).
