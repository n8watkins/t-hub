# Crew brief: control-channel spawn commands are not safely retryable

You are a crew agent on branch `spawn-retry` (cut from origin/main 56e7da1, version 0.3.56).
Work ONLY in this worktree: `/home/natkins/projects/tools/t-hub/t-hub-app/.claude/worktrees/spawn-retry`.
Captain: terminal 84ce1cae. Ship: t-hub-app.

## The bug (general-ordered fix; reported by the monorepo-app captain, confirmed lived by this ship)

Control-socket commands that spawn things can FAIL on the response leg while still APPLYING server-side.
The caller cannot tell whether the command took effect, and a reasonable retry creates duplicates or ghosts.

Field incidents (all 2026-07-09):
- Incident A: `create_worktree` returned success and registered terminal f0f3207b (tab placement + crewRecorded), but the tmux session NEVER materialized. Ghost registry entry.
- Incident B: two `spawn_terminal` calls both returned "failed to read control response: Resource temporarily unavailable (os error 11)". Harbor scan showed nothing landed. Retries succeeded. Then the ORIGINAL two spawns materialized late -> duplicate unbriefed sessions in the same worktrees (collision hazard).
- Incident C (this ship): `close_terminal` on a NON-EXISTENT session returns ok:true - callers cannot distinguish a real kill from a phantom close. Ghost specimens so far: f0f3207b, 709c7252.
- Incident D (LIVE, TODAY ~14:35, while staffing YOU): captain's `create_worktree` over the raw socket timed out on the response leg; nothing applied (no worktree, no tmux session, no git process). Since that call, the ENTIRE control channel is wedged: even read commands (list_terminals, wsl_health) accept TCP but never answer, 25s+. App pid 7632 alive and Windows-responsive, endpoint 127.0.0.1:64466 current. A single in-flight command appears able to wedge every subsequent handler - smells like a shared lock (ctx.tabs / ctx.captains / apply-sink) held across a blocking operation. Root-cause this as part of the same seam.

## The three asks (from the general, via the orchestrator note)

1. IDEMPOTENCY: make spawn-class commands (spawn_terminal, create_worktree) idempotent or safely retryable. E.g. accept a client-supplied requestId/idempotency key; a retry of the same request must not double-apply; return the outcome of the prior identical request. A queryable "what happened to request X" resolves ambiguity without guessing.
2. EAGAIN RESPONSE PATH: the command was accepted; only the response leg failed. Fix both sides: server-side response write robustness (retry on WouldBlock rather than dropping the connection after side effects are committed), and client-side read (control_client.rs) retry-on-EAGAIN up to an overall deadline rather than surfacing an ambiguous error immediately.
3. REGISTRY-VS-REALITY: never report a terminal id as live before its tmux session actually exists (verify e.g. tmux has-session before registering/returning). close_terminal must discriminate killed vs already-gone in its response (keep the tmux-level idempotency, surface the outcome).

Plus (from Incident D): ensure one stuck command cannot wedge the whole control channel - audit lock scope across blocking calls in the spawn/worktree handlers.

## Recon map (verified today against the working tree; trust but re-verify lines)

All in `apps/desktop/src-tauri/src/control.rs` unless noted:
- Server accept loop: 1530, 1572 (thread per connection); response write: write_response at 1959-1966 - single blocking write AFTER all side effects; an Err there drops the connection with mutations already committed. This is the duplicate-maker.
- spawn_terminal handler: 3710-3835. Order: UI check -> tab resolve -> spawn_tmux_terminal (3782, sync) -> place_tile_with_fallback (3788) -> record_crew (3793) -> forward_apply (3814, async fire-and-forget bool) -> response.
- create_worktree handler: 3003-3136. git worktree_add (3039) -> tab mint (3047) -> spawn (3067-3086; on spawn Err it LOGS and continues with terminal_id None) -> record_crew (3090) -> forward_apply (3114) -> response. Ghost path: registration succeeds, forward_apply to the UI is fire-and-forget - if the UI never receives it, the terminal is registered but never adopted/rendered.
- close_terminal: 3931-3955; tmux::kill_session_tree (tmux.rs 415-451) returns Ok for already-gone sessions (is_already_gone check ~443), and the handler always returns ok:true with no outcome discriminator.
- Client: crates/t-hub-mcp/src/control_client.rs 139-184 - 15s read timeout, read_line error formatted as "failed to read control response: {e}" (the EAGAIN surface).
- Protocol: ControlRequest {token, command, args, v} at 62-79 - NO requestId/correlation field today. TabRegistry/CaptainsRegistry have seq revisions (internal, not request-level).
- Tests: control.rs 4448+ (spawn/close/worktree unit tests); probe client scripts/probes/t1_lib.py. No tests for response-write failure today.

## Design guidance (you own the design; justify deviations in the PR)

- Prefer a client-supplied `requestId` in args for spawn-class commands + a server-side completed-request outcome cache (bounded, e.g. last N or TTL) + a `get_request_status` command. Wire the t-hub-mcp client and t1_lib.py to send requestIds and auto-resolve ambiguous failures.
- Keep close_terminal idempotent but honest: outcome field killed | already_gone; ok:true stays.
- Registration order: tmux session verified live BEFORE the id is placed/recorded, or roll back placement on failure.
- Do not hold registry locks across subprocess calls (git, tmux) or socket writes.

## Constraints and definition of done

- Model policy: you run Opus 4.8, effort high. Use subagents (Task tool) for read-heavy exploration/verification - protect your own context.
- Repro first where feasible: extend the unit tests + add a probe script exercising the failure (e.g. a client that kills the connection before reading the response, then retries with the same requestId and asserts no duplicate).
- Rust: cargo test + cargo check clean (0 err/0 warn for touched code). Frontend untouched unless required.
- NO version bump, NO build, NO /no-mistakes (its CI step has a known cwd bug). Commit granularly, push branch `spawn-retry` to origin, open a PR with gh CLI (base main). PR body: root causes, design decisions, test evidence, risk notes.
- The live app is WEDGED right now - do NOT rely on the control socket for E2E; unit tests + probe scripts against your own test binary are the path. Do NOT restart or touch the running app (pid 7632) - the captain/orchestrator coordinate restarts.
- Report: when done (or blocked, or findings warrant early escalation), write a concise report to REPORT.md in this worktree, then as your VERY LAST action run: touch /tmp/t-hub-crew-done/t-hub-scribe/spawn-retry.done
