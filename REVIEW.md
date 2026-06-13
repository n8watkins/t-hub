# TermHub PRD v1.0 — Technical Review

**Reviewed:** 2026-06-13 · **Document:** [PRD.md](./PRD.md) (v1.0, 2026-06-13) · **Reviewer:** Claude (Opus 4.8)

## Verdict

Implementation-ready and unusually well-researched. The conceptual spine — rigorously separating the **tile** (view), the **terminal process** (tmux), and the **agent conversation** (Claude session ID) — is the right insight, and it is enforced consistently through the lifecycle rules (§4), resumability states (§8.3), and two-track persistence (§8.2). Provider citations are accurate (the Chrome 136 remote-debugging change, the same-session interleave warning, portable-pty/WezTerm). This review assumes the product decisions are locked (§17) and focuses on the technical risks worth hardening *before or early in* the build. **The Claude Code integration assumptions (§9.6 hooks, §10) were independently verified against current docs — see [Verification](#verification-claude-code-integration-assumptions); they hold up well.**

## What's genuinely strong

- **Nucleus-first sequencing.** Front-loading the PTY→wsl→tmux spike and gating everything behind proven reliability (0.1 exit criteria) is how to avoid building an IDE that can't keep a terminal alive.
- **Exact-session-ID as identity + duplicate-resume guard** directly defuses Claude's real footgun (interleaved transcripts). Verified: the docs explicitly warn that resuming one session in two terminals interleaves the transcript.
- **Two-track persistence** (active snapshot vs. historical catalog) prevents the "reopen floods me with 1,000 projects" failure mode.
- **Honest non-goals.** The "not an IDE" boundary is stated and defended in the risk table.

## Top risks & gaps (ranked)

### 1. tmux survives an app close — but *not* a WSL VM teardown
`wsl --shutdown` and Windows reboots tear down the WSL2 VM, killing the tmux server, every shell, and any in-flight Claude turn. So "recovery after restart" (§6.6) can never *reattach to live processes* — it can only **resume Claude conversations by exact ID** (transcripts live on the VHDX, which survives) and **restart shells**. The exec-summary wording is careful, but phrases like "24 live sessions" and "process persistence" invite over-trust. Windows also triggers `wsl --shutdown` on its own (memory reclaim, updates), so this isn't rare.
**Action:** State explicitly that *process* durability = app-close only, *conversation* durability = reboot-survivable, and make the event journal the authority for reconstruction intent. A running `npm run dev` is gone after a VM restart — the UX must say so, not imply a resume.

### 2. Hook-event taxonomy (§9.6) — verified accurate ✓ (I was wrong to flag this)
*Originally flagged as partly speculative; that was a stale assumption on my part.* All 17 assumed events exist under those exact names in current Claude Code — including the ones I doubted (`StopFailure`, `PermissionRequest`, `SubagentStart`, `TaskCreated/Completed`, `CwdChanged`, `WorktreeCreate/Remove`) — and every hook's stdin carries `session_id`, `transcript_path`, and `cwd` as base fields, so exact-ID capture at `SessionStart` works as designed. The status model (FR-012) is fully supportable from real events. Newer hooks (`Elicitation`, `SubagentStart/Stop` with `agent_id`, `TaskCreated/Completed`) are a direct asset for parallel-agent supervision. **No action needed** — see [Verification](#verification-claude-code-integration-assumptions).

### 3. 12 simultaneous WebGL xterm contexts will hit the WebView2 context ceiling
Chromium (WebView2 is Edge/Chromium) caps live WebGL contexts (~16), and exceeding it forces context-loss eviction — exactly at the "12 visible" target with browser chrome also consuming contexts.
**Action:** Plan a canvas-renderer fallback for non-focused visible tiles (WebGL only on the focused tile), or a shared/pooled renderer, and add "12 WebGL contexts without loss" to the V0.1 perf harness rather than discovering it at week 6.

### 4. The PTY nucleus's real hazard is *resize*, and the latency metric hides the WSL hop
Keystroke I/O through ConPTY→`wsl.exe`→tmux is the easy part; SIGWINCH propagation (xterm resize → ConPTY → tmux client → pane → program) is the classic breakage, and tmux sizes a window to its *smallest attached client* — a stale hidden client can shrink the active pane. Also, the p95 "<30 ms *excluding WSL/application response*" target (§12) excludes the part the user actually feels.
**Action:** Add a dedicated resize test to the PTY harness; set `window-size latest` / audit `aggressive-resize`; ensure detached hidden clients don't constrain the active window. Measure end-to-end keypress→on-screen-echo (including the WSL hop) even with a looser target.

### 5. The duplicate-resume guard's "Live externally" detection needs a lease, not a latch
It works because hooks are global, so even a non-TermHub `claude` fires `SessionStart` into the journal — clever. But a crash with no `SessionEnd` leaves a session stuck "live," which then *blocks* a legitimate resume.
**Action:** Treat liveness as a lease (heartbeat + TTL + startup reconciliation against actual tmux/PID), not a latch. Make stale-liveness a first-class reconciliation case alongside the "stale PID" tests.

## Smaller hardening items

- **Hook installation mutates Claude config.** Onboarding promises not to touch the user's shell, but installing hooks *is* editing `~/.claude/settings.json`. Require explicit consent, do a non-destructive merge (the user may already have hooks), survive hand-edits, and ship a clean uninstall.
- **inotify exhaustion on big monorepos.** The "large monorepo index" perf scenario will hit `max_user_watches`/`ENOSPC`; recursive watches are added per-directory. Handle ENOSPC and fall back to periodic reconciliation.
- **Single stdio NDJSON bridge can head-of-line block.** A large file read on the same pipe as a metrics ping stalls the ping. Add request prioritization or a separate channel for bulk reads vs. control/metrics.
- **Terminal escape-sequence hardening.** Beyond "sanitize URLs," explicitly gate **OSC 52 (clipboard write)** and window-title injection from untrusted agent output — both are real exfiltration/spoofing vectors in xterm.js.
- **SQLite durability.** "Survive abrupt termination" + "commit within 500 ms" needs WAL mode with an explicit `synchronous` setting called out; otherwise the autosave guarantee is aspirational.

## Roadmap

The gating discipline (each release has a hard exit criterion; Chromium/MCP gated on "if reliability gates pass") is excellent. The **timeline is optimistic specifically around the PTY/tmux/resize spike and the WSL agent** — those edge cases (resize, Unicode, EOF, orphan reconciliation, WebGL ceiling) historically consume more than the "Days 1–4" allotment. Treat Days 1–4 as "prove it's possible" and expect the *robust* version to bleed into the weeks 2–3 window. Don't backfill a nucleus slip by starting the file index early.

One sequencing opportunity surfaced by verification: because Claude Code now ships first-class **`SubagentStart/Stop` (with `agent_id`)**, **`TaskCreated/Completed`**, and **`Elicitation`** hooks, the parallel-agent *awareness* the PRD defers to **2.0** (subagent/worktree mapping) is buildable far earlier — the event substrate already exists. If parallel-agent supervision is the priority, pull a read-only orchestrator→subagent tree into **0.5/1.0** rather than 2.0.

---

## Verification (Claude Code integration assumptions)

Checked against current Claude Code docs on **2026-06-13** via three parallel research agents plus two direct doc fetches to resolve a discrepancy. Headline: **the PRD's Claude Code assumptions hold up — better than this review initially assumed.** Sources: `/docs/en/hooks`, `/sessions`, `/cli-reference`, `/data-usage`, `/statusline`, `/agent-sdk/sessions`.

### §9.6 hooks — ✓ fully accurate
All 17 assumed events are real and correctly named. Universal base fields on **every** hook: `session_id`, `transcript_path`, `cwd` (plus `permission_mode`, `effort`, `hook_event_name`, and `agent_id`/`agent_type` inside subagents). Context injection via `hookSpecificOutput.additionalContext` is supported by `SessionStart`, `UserPromptSubmit`, `UserPromptExpansion`, `SubagentStart`, `PreToolUse`/`PostToolUse`, `Stop`, and more — so **§10.7 (open-file injection) is valid**, and `SessionStart` injection is an extra lever for seeding project/worktree context. Useful newer hooks: `Elicitation` (clean "agent needs input" signal → maps to the "needs-question" state), `SubagentStart/Stop` (carry `agent_id` for per-subagent tracking), `TaskCreated/Completed`.

### §10.1–10.2 sessions, resume/fork, retention — ✓ confirmed (with exact specifics)
- **Resume:** `-c`/`--continue` (most recent in cwd), `-r`/`--resume [name|id]`, in-session `/resume`. Session-**ID** lookup is scoped to the current project dir + its worktrees; **name** search spans the repo.
- **Picker scope is interactive:** `Ctrl+W` widens to all worktrees of the repo, `Ctrl+A` to every project on the machine.
- **Interleave warning is explicitly documented.** Fork via `/branch` or `--fork-session` (new session ID, copied history). Note: permissions approved "for this session" do **not** carry into a fork.
- **Transcripts:** JSONL at `~/.claude/projects/<project>/<session-id>.jsonl`; removed after **30 days** by default; `cleanupPeriodDays` lives in any settings.json scope (user/project/local; min 1, `0` rejected). `CLAUDE_CONFIG_DIR` relocates the whole store.

### §10.3 Agent SDK session discovery — ⚠ thinner than the PRD implies
The SDK exposes `listSessions` / `getSessionInfo` / `getSessionMessages` / `resume` / `fork` / `continue` / `rename` / `tag` and a custom `SessionStore`. **But** the metadata API does **not** return summary, cwd, branch, or first-prompt — you parse the transcript or maintain your own index. This *validates* the PRD's stated plan (own lightweight index; don't poll the SDK aggressively); just don't expect rich metadata from the SDK itself. **Suggested edit:** soften §10.3's "exposes … summary, branch, cwd, first prompt" to "exposes enumeration/resume/fork; richer metadata is derived from transcripts or a local index."

### Statusline (supports §5.5 utility area, §6.10 night mode) — ✓ available, one real caveat
Reset timestamps exist: `rate_limits.five_hour.resets_at` / `seven_day.resets_at` (Unix epoch), plus `*.used_percentage`, full `context_window.*` usage, and `cost.*`. **Caveat that affects night mode:** the `rate_limits` block appears only for Claude.ai **Pro/Max** and only **after the first API response in a session** — so the scheduler can't read a reset time until the session has made at least one call, and must degrade gracefully when the block is absent. Non-worktree git branch is **not** in the statusline schema (use the agent's `git branch --show-current`, already planned in §10.4). **Suggested edit:** add this caveat to §6.10 step 1.
