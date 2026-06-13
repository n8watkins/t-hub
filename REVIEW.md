# TermHub PRD v1.0 — Technical Review

**Reviewed:** 2026-06-13 · **Document:** [PRD.md](./PRD.md) (v1.0, 2026-06-13) · **Reviewer:** Claude (Opus 4.8)

## Verdict

Implementation-ready and unusually well-researched. The conceptual spine — rigorously separating the **tile** (view), the **terminal process** (tmux), and the **agent conversation** (Claude session ID) — is the right insight, and it is enforced consistently through the lifecycle rules (§4), resumability states (§8.3), and two-track persistence (§8.2). Provider citations are accurate (the Chrome 136 remote-debugging change, the same-session interleave warning, portable-pty/WezTerm). This review assumes the product decisions are locked (§17) and focuses on the technical risks worth hardening *before or early in* the build.

## What's genuinely strong

- **Nucleus-first sequencing.** Front-loading the PTY→wsl→tmux spike and gating everything behind proven reliability (0.1 exit criteria) is how to avoid building an IDE that can't keep a terminal alive.
- **Exact-session-ID as identity + duplicate-resume guard** directly defuses Claude's real footgun (interleaved transcripts). Most tools get this wrong.
- **Two-track persistence** (active snapshot vs. historical catalog) prevents the "reopen floods me with 1,000 projects" failure mode.
- **Honest non-goals.** The "not an IDE" boundary is stated and defended in the risk table.

## Top risks & gaps (ranked)

### 1. tmux survives an app close — but *not* a WSL VM teardown
`wsl --shutdown` and Windows reboots tear down the WSL2 VM, killing the tmux server, every shell, and any in-flight Claude turn. So "recovery after restart" (§6.6) can never *reattach to live processes* — it can only **resume Claude conversations by exact ID** (transcripts live on the VHDX, which survives) and **restart shells**. The exec-summary wording is careful, but phrases like "24 live sessions" and "process persistence" invite over-trust. Windows also triggers `wsl --shutdown` on its own (memory reclaim, updates), so this isn't rare.
**Action:** State explicitly that *process* durability = app-close only, *conversation* durability = reboot-survivable, and make the event journal the authority for reconstruction intent. A running `npm run dev` is gone after a VM restart — the UX must say so, not imply a resume.

### 2. The hook-event taxonomy in §9.6 is partly speculative
`SessionStart/End`, `UserPromptSubmit`, `Stop`, `SubagentStop`, `Notification`, `PreToolUse/PostToolUse` are solid ground. But `StopFailure`, `PermissionRequest`, `TaskCreated/Completed`, `CwdChanged`, `WorktreeCreate/Remove` may not exist under those names. The whole status model (FR-012) depends on these firing.
**Action:** Validate the full list against the live hooks reference; for any signal without a dedicated event, derive it from `PreToolUse`/`PostToolUse` payloads or statusline polling, and budget for that. *(Verification in progress — see "Verification" below.)*

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

## Verification (Claude Code integration assumptions)

Risk #2 (hook-event taxonomy) and the §10 Claude-session claims (resume/fork semantics, transcript storage + `cleanupPeriodDays` default, statusline JSON fields, Agent SDK session APIs) are being validated against the current Claude Code documentation. Findings and any corrections to assumed names/fields/defaults will be appended here.
