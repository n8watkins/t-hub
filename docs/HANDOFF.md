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

- **main is at 0.3.59** (bump commit `648b7c1`), installed and running.
0.3.59 merged PRs #49 (EventFanout snapshot-then-write-unlocked) and #50 (relay-wedge self-heal: `rebind_control` + client wedge-detector + stale-pin fallback).
- **Two untracked files** (`.lavish/`, `docs/DECK-AGENTS-DESIGN.md`) are pre-existing, NOT this ship's work - leave them.
- All crew reaped, all worktrees removed.
The only other captain-adjacent tmux sessions are Cortana (`e05764f5`) and the **monorepo-app captain** (`9a32f554`, another ship) - do NOT touch them.

## ✅ RESOLVED - the "control-socket wedge" saga (read before trusting old wedge reports)

The long-running wedge decomposed into REAL app bugs (fixed in #45, #49) plus a **diagnostic artifact** that survived every fix:
app-spawned sessions carry spawn-time `T_HUB_CONTROL_ADDR`+`T_HUB_CONTROL_TOKEN` env pins; `t1_lib.connect()` and the pre-#50 MCP client prefer the pin over `control.json`; every app restart rotates the port, so pinned tooling silently targets a DEAD port forever after.
The WSL2 mirrored relay times out slowly on dead-port connects instead of refusing, so a dead pin presents exactly like a wedged live server.
The full corrected evidence trail is on PR #50 (post-merge comment) and in memory `control-socket-transient-wedge`.
Rules of thumb: never probe socket health through an env-pinned client; raw-connect to the CURRENT `control.json` addr; a slow WSL connect-timeout to a Windows loopback port usually means dead port, not wedged server.
An intermediate "WSL relay per-port flow wedge" theory (2026-07-09) is FALSE - do not resurrect it from old reports.

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

1. **Post-0.3.59 wedge-saga carry-items.**
(a) Live-verify PR #50's heal loop if a REAL wedge ever presents (none may - the residual symptom was the stale-pin artifact; the rebind is defense-in-depth).
(b) Known limitation N2: a connect-level wedge presentation would not trigger the Timeout-only heal (documented in PR #50).
(c) Fleet hygiene: after every install/restart, long-running app-spawned sessions' pinned MCP/tooling go dark on the dead port - fresh sessions in this repo get the F2-fixed client (debug `t-hub-mcp` rebuilt from post-#50 main, 2026-07-10); consider surfacing the same fallback in any other pin-preferring consumer.
(d) PR #49 M2 design note from review: cross-client event ordering is now per-socket, not global - matters when M2 adds a second subscriber.
2. **(done)** F2 EventFanout snapshot-under-lock - shipped as PR #49 in 0.3.59.
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
7. **Doc debt** (flagged by the 0.3.58 wind-down doc sweep, deferred as MEDIUM+ rewrites): re-baseline `docs/ROADMAP-PLAN.md` + `docs/SERVER-SPLIT-AND-ROADMAP.md` onto the 0.3.58 wave (they still cite `v0.2.0`-era figures + a pre-#42 "next build"); expand `docs/MCP.md` §2 to document #45's control-channel idempotency/retry contract (`requestId`/`RequestCache`/`get_request_status`, `close_terminal` killed|already_gone) - the tool catalog is accurate but the robustness layer is undocumented; optional one-word nit in `docs/FEATURE-PLAN.md:3` ("current 0.1.67" reads wrong at 0.3.58, but it is a historical banner - leave unless re-baselining).

## What shipped this wave (for context)

- **0.3.54**: orchestrator-wake (#34), scribe gate (#35), pty forwarder leak (#36), per-session header glitch (#37), endpoint reconnect (#38).
- **0.3.55**: captains-render-fix (#39) - externally-claimed captains render regardless of tile placement.
- **0.3.56**: Cortana crown pane header (#40), Scribe v1 dictation-state migration (#41).
- **0.3.57**: doc-staleness (#42), voice-gate dual-source dictation gate + announce.log (#43), UI batch - kill+restart button / ctx% setting / attribution / chime trim (#44), spawn-retry idempotency + de-wedge (#45), auto-continue small Esc+continue fix (#46).
- **0.3.58**: auto-continue full redesign default-ON (#47), control-socket flap fix - tmux+git subprocess bound + M1 full fix (#48).
- **0.3.59**: EventFanout snapshot-then-write-unlocked (#49), relay-wedge self-heal - rebind command + client wedge-detector + stale-pin fallback (#50).
The wedge saga is RESOLVED (see the section above); the residual "wedge on 0.3.58" turned out to be the stale-env-pin artifact.
