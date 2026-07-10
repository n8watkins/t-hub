# T-Hub captain handoff (fresh, 2026-07-09 wind-down)

Zero-context resume doc for the next t-hub-app captain.
Ship file (authoritative roster + full history): `~/.t-hub/captain/ships/t-hub-scribe.md`.
Fleet doctrine: `~/.t-hub/captain/{ESCALATION,MODEL-POLICY,RETIREMENT}.md`; crew-brief escalation block: `~/.t-hub/captain/BRIEF-ESCALATION-BLOCK.md`.

## Who you are

You are the captain of ship **t-hub-app** (registry slug `t-hub-app`), terminal `84ce1cae`, repo `/home/natkins/projects/tools/t-hub/t-hub-app`.
The general (user) commands via the orchestrator **Cortana** (terminal `e05764f5`).
You DELEGATE all project work to crew; you stay at orchestration altitude.
On resume: invoke `/shipmate`, claim via `claim_captain` (MCP) or `~/.t-hub/captain/claim-captain.sh 84ce1cae t-hub-app` (raw-socket fallback), rebuild the roster from the ship file + `list_terminals`.

## Current state (end of this wave)

- **main is at 0.3.58** (commit `a3d1700`), installed and running (pid was 8556, binary FileVersion 0.3.58). Repo tree clean.
- This wave shipped **0.3.54 → 0.3.58**. 0.3.58 merged PRs #47 (auto-continue full redesign, DEFAULT-ON) and #48 (control-socket flap fix: tmux+git subprocess bounding via a shared `bounded_exec` helper + M1 full re-probe-on-reap).
- **Two untracked files** (`.lavish/`, `docs/DECK-AGENTS-DESIGN.md`) are pre-existing, NOT this ship's work - leave them.
- **All crew reaped, all worktrees removed** (except a doc-staleness crew active during wind-down - reaped as part of it). The only other tmux session, `9a32f554`, is the **monorepo-app captain** (another ship) - do NOT touch it.

## ⚠ TOP PRIORITY on resume - the flap fix did NOT work

0.3.58's #48 bounded every tmux and git subprocess, but a **live control-socket wedge STILL reproduces on 0.3.58**: connect succeeds, but EVERY command hangs >11-18s - including non-tmux reads (`get_theme`, `list_tabs`, `wsl_health`), not just `list_terminals`. Meanwhile the tmux server itself is fast (`list-sessions` 0.006s) and only 3 sessions exist. So the wedge is in the **accept/serve/dispatch path** (upstream of handlers), NOT the tmux subprocess #48 bounded. The flap crew explicitly ruled the accept loop out; the evidence contradicts that. This is escalated as DECISION-NEEDED and is the #1 NEAR itinerary item: reopen the flap investigation with the corrected evidence (every command hangs incl. non-tmux ⇒ serve/dispatch seam - candidate: a global lock/worker-pool/`ACTIVE_CONNS` exhaustion in `serve`, or a stuck subscribed/attach connection holding a shared resource). Until fixed, the operational workaround stands: drive crew via **raw tmux** on the `-L t-hub` socket (invisible in the webview until an app restart re-adopts them); a restart transiently clears the wedged process.

## How to operate this ship (learned this wave)

- **Control socket flaps/wedges** - never diagnose "app up/down" from a bare TCP connect; round-trip a real command (`list_terminals`) with a timeout. See memory `control-socket-transient-wedge`.
- **Spawn crew via raw tmux** while the socket is wedged: `tmux -L t-hub new-session -d -s th_<id> -c <worktree> -x 220 -y 50; tmux -L t-hub set-option -t th_<id> window-size manual`, then `send-keys` the harness (`claude --model claude-opus-4-8 --effort high --dangerously-skip-permissions`) and the brief. Raw-tmux crew are native-client-adopted but NOT webview-visible until a restart.
- **`send-keys` backtick trap**: backticks/`$()`/quotes in a message get shell-interpreted locally and mangle the send - build the message in a single-quoted heredoc and send with `-l` (literal), or write plain prose. Memory `send-keys-backtick-trap`.
- **Completion signaling**: every crew brief ends with `touch /tmp/t-hub-crew-done/t-hub-scribe/<name>.done`; run ONE background watcher (`run_in_background` Bash loop over that dir) that exits on the first sentinel and wakes you.
- **Model policy** (`MODEL-POLICY.md`): captain = Fable 5 high; crew = Opus 4.8 high (review crew = Opus 4.8 xhigh). Pin `--model claude-opus-4-8 --effort high|xhigh` on every spawn (session default can leak xhigh - restart with `--effort high` if so).
- **Escalation doctrine** (`ESCALATION.md`, v2 2026-07-10): decisions at the lowest capable level; captain decides all technical/ship matters and reports STATUS, escalates only above-ship items (merge/push/install/outward, product, spend, cross-ship, security, unclear standing-order intent) to Cortana as DECISION-NEEDED (options + recommendation). Report classes: STATUS / DECISION-NEEDED / EMERGENCY. Fold `BRIEF-ESCALATION-BLOCK.md` into EVERY crew brief. Helm feature CANCELLED - courtesy no-inject-over-the-general-while-typing only.
- **announce.sh deploy step**: the NSIS installer ships the binary, NOT the captain-dir fleet scripts. After any `apps/desktop/scripts/announce.sh` change lands, deploy it: `cp <repo>/apps/desktop/scripts/announce.sh ~/.t-hub/captain/announce.sh && chmod +x`; verify with `announce.sh --gate` + an announce.log line. Memory `captain-voice-announcements`.
- **Version bump** on every code commit via `bump-version.sh` (docs exempt); sync Cargo.lock via `cargo check`, never hand-sed.

## FUTURE ITINERARY

### NEAR (deferred or almost reached this session - pick up first)

1. **[TOP] Reopen the control-socket flap investigation.** 0.3.58's #48 did NOT fix the live wedge (every command hangs incl. non-tmux ⇒ serve/dispatch path, not tmux; tmux server itself is healthy). Root-cause the accept/serve/dispatch seam (global lock / worker-pool / ACTIVE_CONNS exhaustion / stuck subscribed-or-attach connection). This is the general's headline pain (empty workspace, unusable MCP). Reproduce, fix, xhigh review.
2. **F2 - EventFanout snapshot-under-lock.** `emit_event` holds the `subs` mutex across per-subscriber socket writes (bounded 5s each by SO_SNDTIMEO). The #48 flap crew flagged it as a latent seam (not the observed cause): N sequential stalled subscribers ⇒ ~N×5s emit stall, delaying event delivery/adoption forwards. Fix = snapshot subscribers under lock, write unlocked. May be related to item 1 - check first.
3. **Bound the other-subsystem subprocesses.** The #48 F1 completeness sweep found unbounded `.output()`/`.status()` in files.rs, codex.rs, usage.rs, devserver.rs, recent.rs, claude/install.rs (and control.rs `tailscale_ip4` startup-only, benign). Route control-reachable ones through the shared `bounded_exec` helper for the same "no handler parks forever" invariant.
4. **PR #45 M1 spawn_terminal re-probe honest-limit.** The re-probe closes the create_worktree reaped-duplicate but `spawn_terminal` returns `None` (server-minted id, nothing to probe by) and relies on the 600s reap window. If spawn duplicates recur, add a probe key (e.g. client-supplied spawn tag) so spawn is re-probable too.
5. **PR #44 LOW watch-items** (all noted in the #44 PR body): header ctx% meter mid-turn flicker (default-OFF, small blast radius); tile-header button crowding at the narrowest widths (now 5 shrink-0 buttons); DRY the O(n) `sessionIdByTmux` reverse scan (a forward index retires it).
6. **Auto-continue redesign follow-ups** (from the two xhigh reviews, all LOW, shipped as-is): default-ON flip surprises v1 curated opt-in users (needs a **release note** - flag to the general); re-verify the modal detection anchors against a real limited pane when one safely presents (anchors were verified against strings grepped from the Claude Code binary, not a live render); the account-wide Codex reset fans out to all watched Codex tiles at reset.

### FURTHER (on our list, never got to this wave)

1. **PR #34 orchestrator-wake fast-follows** (3 MEDIUM, on hold since 0.3.54): stale-UUID handling, no-suppression-timeout, live-validate the wake path.
2. **no-mistakes CI-step cwd bug** - the shared no-mistakes daemon runs `gh pr checks` from its non-repo cwd (`~/.no-mistakes`) so the CI step hangs forever though checks actually pass. Fix = give the CI step repo context (chdir/--repo). This is the no-mistakes TOOL repo, a different ship - route via Cortana. Memory `no-mistakes-ci-step-cwd-bug`. (Why every crew this wave ran plain commit+push+PR, never /no-mistakes.)
3. **Registry slug-rename persistence inconsistency** - a pre-restart slug rename rolled back across a restart for this ship while monorepo-app's survived; registry persistence is inconsistent across restarts.
4. **ensure-thub-mcp debug-binary repoint** - the per-repo `.mcp.json` provisioning points at the local DEBUG t-hub-mcp build; override via `T_HUB_MCP_BIN` once t-hub ships a packaged sidecar. Memory `captain-self-register-provisioning`.
5. **WorkspacesList Cortana rename** - small UI follow-up (the orchestrator folder-name row), general-queued for a future batch.
6. **Server-split M2-M4** (remote), webview supervision cues, MCP parity for `create_worktree`/`remove_worktree`/`wait_for_status` (currently raw-socket-only) - the standing longer-horizon goals from the native-pivot survivors.

## What shipped this wave (for context)

- **0.3.54**: orchestrator-wake (#34), scribe gate (#35), pty forwarder leak (#36), per-session header glitch (#37), endpoint reconnect (#38).
- **0.3.55**: captains-render-fix (#39) - externally-claimed captains render regardless of tile placement.
- **0.3.56**: Cortana crown pane header (#40), Scribe v1 dictation-state migration (#41).
- **0.3.57**: doc-staleness (#42), voice-gate dual-source dictation gate + announce.log (#43), UI batch - kill+restart button / ctx% setting / attribution / chime trim (#44), spawn-retry idempotency + de-wedge (#45), auto-continue small Esc+continue fix (#46).
- **0.3.58**: auto-continue full redesign default-ON (#47), control-socket flap fix - tmux+git subprocess bound + M1 full fix (#48). **Caveat: the live serve-path wedge is NOT resolved (see TOP PRIORITY).**
