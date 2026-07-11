# T-Hub captain handoff (refreshed 2026-07-11, 0.3.63 increment complete on main)

## ⏸ ACTIVE RESUME POINT (2026-07-11 ~12:1x - BUILD PHASE: items 1-3 + wedge cap ALL MERGED; 0.3.63 increment complete, ONE deploy confirm pending)

**THE PROGRAM (one line):** all four items ratified 2026-07-10; items 1 (Phases 1+2), 2, and 3 are now BUILT and MERGED; the batched 0.3.63 increment is COMPLETE ON MAIN awaiting the general's single deploy confirm; next builds are item-1 Phase-3 ACLs -> item-4 -> typing-guard LAST.

**MERGED (all with full gate: xhigh review -> fix round -> same-reviewer delta -> the general's DIRECT in-session word):** PR #55 `d599fa9` (Phase-1 backdoor close), PR #56 `0ba4360` (Phase-2 durable inbox + identity slice), PR #57 `753f738` (item-2 identity re-key), PR #59 `7b3afb4` (item-3 security defaults, all EIGHT ratified knobs), PR #58 `3daedc8` (spawn-wedge liveness de-conflation, option B - operational, outside the program queue).
Review records durable in `~/.t-hub/captain/reviews/` (`pr55`-`pr59-review`, design checks, ratified proposals).
Canon: `~/.t-hub/captain/ORCHESTRATION-PROGRAM.md`.

**DEPLOY (the next general decision):** ONE 0.3.63 confirm covers items 1-3 + the wedge cap.
Deploy checklist before TRUSTING the flips in the field: (1) manual §3.1 gate check 3 - real webview attach + send_keys on the WSLg build (`T_HUB_CONTROL_HARDEN=0` = instant rollback); (2) DPAPI runtime-verify on the Windows build (dev/CI was Linux, path only cross-checked); (3) rotation note - pre-existing fleet sessions re-spawn + remote re-pair when rotation later fires (Windows first-restart adopts, does not rotate); (4) the PreToolUse blocking gate is OFF-by-default (distinct consent to enable).

**BUILD QUEUE (unchanged discipline - worktree crew Opus 4.8 high permissionless, PR-only, NO bump, xhigh review, fix round, same-reviewer delta, the general's word):** item-1 Phase-3 ACLs (NEXT; consumes item-3's session-token-on-ControlRequest + plane-confirm seams, documented in the PR #59 body) -> item-4 build -> item-1 Phase-4 typing-guard LAST.
**IN FLIGHT:** option-A bounded-exec sweep (branch `bounded-sweep`, the wedge accumulation CAUSE; folds #58-review MED-1 force-escape + LOW-1 structured retryable flag) on the held diag crew; its review goes to the held reviewer-58.
**MERGE DOCTRINE:** EVERY merge = the general's word, DIRECT in-session confirm (AskUserQuestion); relay alone gets refused (live R-C1).

**⚠ WEDGE (root-caused, capped, cause-fix in flight):** ONE mechanism - `has_session` conflated probe TIMEOUT with absence (no adoption map exists); accumulation = unbounded on-handler `.output()` calls (#48 never swept; usage.rs `claude -p /usage` worst).
PR #58 (merged, NOT LIVE until install) makes it honest/retryable + never transfer-grade; option A removes the cause.
The RUNNING pre-#58 app still lies: false "no such session" on live sessions, and a timed-out `close_terminal` can EXECUTE LATE - verify kills against raw tmux, never trust the app's outcome under degradation.
**Break-glass relay (works regardless):** Write message to file, `tmux -L t-hub load-buffer -b <buf> <file>`, `paste-buffer -b <buf> -t th_<id> -d`, `send-keys -t th_<id> Enter`, capture to verify submission.

**PERMS:** `.claude/settings.local.json` = `defaultMode bypassPermissions` + allow rules; classifier quirk: it may chain-flag raw-tmux capture-pane Bash calls in permissionless-crew contexts - use MCP `read_terminal` for pane reads (raw `-e` captures for typing-guard checks have passed).
**Operating reminders:** GHOST TRAP - Claude Code renders generated prompt-suggestions DIM in idle input lines; plain-text reads make them look like typed human decisions; discriminate with `tmux -e` capture (ESC[2m = ghost = clear to send; plain = human = HOLD); Escape does NOT clear a ghost; NEVER attribute an input-line "decision" to the general without the -e check.
MCP PARITY LANDED: `create_worktree`/`remove_worktree`/`wait_for_status` are native MCP tools (spawnedBy records crew) - raw-socket t1_lib.py is genuine break-glass only.
VISIBLE-FIRST spawns + resize 220x50 BEFORE kickoff; KICKOFF-VERIFICATION (confirm PROCESSING); crew migration = `claude --resume <uuid>`; sentinel watcher = background loop over `/tmp/t-hub-crew-done/t-hub-scribe/` with a heartbeat exit; briefs in `/tmp/flap-probe/*-BRIEF.md`; copy review findings from /tmp to `reviews/` BEFORE reaping reviewers.

---

Zero-context resume doc for the next t-hub-app captain.
Ship file (authoritative roster + full history): `~/.t-hub/captain/ships/t-hub-scribe.md`.
Fleet doctrine: `~/.t-hub/captain/{ESCALATION,MODEL-POLICY,RETIREMENT}.md`; crew-brief escalation block: `~/.t-hub/captain/BRIEF-ESCALATION-BLOCK.md`.

## Who you are

You are the captain of ship **t-hub-app** (registry slug `t-hub-app`), terminal `ab440bfa`, repo `/home/natkins/projects/tools/t-hub/t-hub-app`.
The general (user) commands via the orchestrator **Cortana** (terminal `e05764f5`).
You DELEGATE all project work to crew; you stay at orchestration altitude.
On resume: invoke `/shipmate`, claim via `claim_captain` (MCP) or `~/.t-hub/captain/claim-captain.sh ab440bfa t-hub-app` (raw-socket fallback), rebuild the roster from the ship file + `list_terminals`.
NOTE: this terminal id itself migrated once (84ce1cae died to a closeWorkspace SIGKILL 2026-07-10; PR #53 removed the mis-placement vector and added the captain-guard) - if it migrates again, re-claim under the new id and update this doc.

## Current state (2026-07-10, evening)

- **0.3.62 is INSTALLED + VERIFIED** (main @ `d358044`, pid 17752, endpoint rotates per control.json).
Carries: #53 adopt/attach per-tile isolation + authoritative placement + closeWorkspace captain-guard (silent re-place, general-ratified) + test-socket isolation; #54 managed Kokoro lifecycle engine supervisor behind the **default-OFF** `T_HUB_MANAGED_KOKORO` flag (runtime-inert until a deliberate flag-on wave).
The joint build caught a real PR #54 `cfg(windows)` regression (missing Win32_System_JobObjects/Win32_Security feature flags) - landed as its own commit `d358044`; the Windows build-verify is why it surfaced.
Post-install verify: socket round-trip 1.2s, 6 terminals adopted, NO ghost/blank-UI (P1 class clear); flag-off inertness confirmed (kokoro unit still `active` + health 200, `T_HUB_MANAGED_KOKORO` unset).
STILL NEEDS the general's visual confirm: per-tile error-isolation containing a throw and the captain-guard sparing a captain on tab-close are compiled-in but only exercisable by a live UI action (not safely restart-triggerable). Open #54 nit: `forgetting_copy_types` at engine_supervisor.rs:630 (redundant `mem::forget`).
- **The ORCHESTRATION PROGRAM design phase is COMPLETE** (2026-07-10): all four items ratified same-day through the full adversarial gate; item-1 Phases 1+2 and item-2 built (PRs #55/#56 merged, #57 merge-ready). See the ACTIVE RESUME POINT above for the build queue and the active app-spawn-wedge anomaly.
- **The kokoro interim systemd user unit still owns port 7478** and keeps serving after the install; the unit -> managed-child cutover is a SEPARATE deliberate step gated on the wave-1 flag-on validation (general-confirmed).
- **Two untracked files** (`.lavish/`, `docs/DECK-AGENTS-DESIGN.md`) are pre-existing, NOT this ship's work - leave them.
- Other ships share the tmux server (Cortana `e05764f5`, monorepo `9a32f554`, behavior-tracker `3e3b6479` + crew, appturnity crews) - touch ONLY your own roster.

## ✅ RESOLVED - the "control-socket wedge" saga (read before trusting old wedge reports)

The long-running wedge decomposed into REAL app bugs (fixed in #45, #49) plus a **diagnostic artifact** that survived every fix:
app-spawned sessions carry spawn-time `T_HUB_CONTROL_ADDR`+`T_HUB_CONTROL_TOKEN` env pins; `t1_lib.connect()` and the pre-#50 MCP client prefer the pin over `control.json`; every app restart rotates the port, so pinned tooling silently targets a DEAD port forever after.
The WSL2 mirrored relay times out slowly on dead-port connects instead of refusing, so a dead pin presents exactly like a wedged live server.
The full corrected evidence trail is on PR #50 (post-merge comment) and in memory `control-socket-transient-wedge`.
Rules of thumb: never probe socket health through an env-pinned client; raw-connect to the CURRENT `control.json` addr; a slow WSL connect-timeout to a Windows loopback port usually means dead port, not wedged server.
An intermediate "WSL relay per-port flow wedge" theory (2026-07-09) is FALSE - do not resurrect it from old reports.

## How to operate this ship

- **Socket health**: never diagnose "app up/down" from a bare TCP connect; round-trip a real command with a timeout, against the CURRENT control.json addr (memory `control-socket-transient-wedge`). The wedge-discriminator runbook: `~/.t-hub/captain/WEDGE-RUNBOOK.md`.
- **VISIBLE-FIRST spawning (standing procedure)**: spawn ALL crew via the socket (`create_worktree` or `spawn_terminal` with tabName/tabId) so terminals render in the workspace from birth - reviews and builds included. Raw tmux ONLY when a live round-trip proves the socket degraded, and migrate such crew to a visible tile once it recovers. Force `window-size manual` + 220x50 on every new pane (the unsized-client 2-char trap), resize BEFORE sending the kickoff.
- **KICKOFF VERIFICATION**: after every send-keys kickoff, capture the pane and confirm the crew is PROCESSING (spinner/cost ticking) - a collapsed pane can eat the Enter and the kickoff sits unsubmitted (this cost a 40-minute silent stall once).
- **Crew migration = `claude --resume <session-uuid>`** (general's directive): record every crew's Claude session UUID in the roster at spawn (newest small+fresh `.jsonl` under `~/.claude/projects/<munged-worktree>/`); to move a crew, spawn the destination, STOP the old claude, `--resume <uuid>` in the new terminal, kill the old tmux session. Never a fresh re-kick; never `--continue` guessing. Memory `crew-migration-resume-by-id`.
- **RELAY TYPING-GUARD (interim, until the comms plane ships)**: before any send-keys relay to a human-facing terminal, capture the destination pane and check the LAST prompt line for typed content (the prompt char is followed by a non-breaking space - match content, not whitespace); if non-empty, HOLD and retry - never interleave with a human mid-keystroke.
- **`send-keys` backtick trap**: backticks/`$()`/quotes get shell-interpreted and mangle the send - single-quoted heredoc + `-l` (literal), or plain prose; long messages can render as a paste placeholder ("paste again to expand") - verify submission. Memory `send-keys-backtick-trap`.
- **Completion signaling**: every crew brief ends with `touch /tmp/t-hub-crew-done/t-hub-scribe/<name>.done`; run ONE background watcher over that dir; clear collected sentinels promptly (any lingering `.done` re-fires the next watcher).
- **Model policy** (`MODEL-POLICY.md`): captain = Fable 5 high; crew = Opus 4.8 high (review crew = Opus 4.8 xhigh); pin `--model claude-opus-4-8 --effort high|xhigh` on every spawn.
- **Escalation doctrine** (`ESCALATION.md`): decisions at the lowest capable level; STATUS / DECISION-NEEDED / EMERGENCY report classes; fold `BRIEF-ESCALATION-BLOCK.md` into EVERY crew brief; merge/push-main/install/outward/product/spend escalate via Cortana.
- **Review discipline**: xhigh review on every control-path or destructive-adjacent PR; reviewer verdicts go at the END of reports (pane-capture survival); reviewers write findings to a file when a fix crew needs them; hold the same reviewer for delta re-verifies.
- **announce.sh deploy step**: the NSIS installer ships the binary, NOT the captain-dir fleet scripts; after any `apps/desktop/scripts/announce.sh` change lands, copy to `~/.t-hub/captain/announce.sh` and verify. Memory `captain-voice-announcements`.
- **Version bump** on every code commit via `bump-version.sh` (docs exempt); sync Cargo.lock via `cargo check`, never hand-sed. Ship practice: PRs carry NO bump; one bump per batch at build time.

## FUTURE ITINERARY

### THE ORCHESTRATION PROGRAM (general-approved roadmap, 2026-07-10; DESIGN-PROPOSALS-FIRST - no implementation until he ratifies each design)

Source: `~/.t-hub/captain/reviews/orchestration-adversarial-review-2026-07-10.md` + addendum (both passes; approved as roadmap).
Captain's distillation: `~/.t-hub/captain/reviews/CAPTAIN-CRIB-SHEET-2026-07-10.md`.
Clock starts at the 0.3.62 install. Proposals go up ONE AT A TIME via Cortana for the general's ratification; drafting of item N+1 pipelines while item N awaits ratification; each proposal is adversarially design-checked by an independent xhigh crew before escalation; format = problem / design / blast-radius+migration / effort / specific general-decisions.
Priority order:
1. **UNIFIED COMMS PLANE** (keystone, FIRST): one authenticated, attributed, receipted, ordered per-recipient channel; retires raw send-keys to break-glass; hosts the typing-guard as its drain predicate (turn-boundary EXISTS at fleet.rs `is_ready_for_wake`; not-being-typed-into MUST BE BUILT input-side - PTY-output parsing is a verified dead end); one-way-input policy as queue ACLs with an EMERGENCY fast-lane; receipt-on-DRAIN; applied at EVERY hop; inbox + typing-guard are ONE queue with two predicates, never two.
This item ABSORBS the P2 fleet-wide typing-guard (general's four-part sketch + Cortana's premise correction live in the review addendum + crib sheet - single source there, not restated here).
Design-crew brief is staged at `/tmp/flap-probe/COMMS-PLANE-DESIGN-BRIEF.md`.
2. **IDENTITY RE-KEY** to ship/role; terminal id demoted to mutable pointer; crew ownership follows the ship; auto-rebind on migration; Claude session UUID as continuity anchor.
3. **SECURITY DEFAULTS**: read/control token split default-on (fix the webview scrape via the in-process `local_control_token` seam); full token off shared-readable disk; mechanical gates on push-main/spend/outward via the existing tier machinery.
4. **RULEBOOK ENFORCEMENT**: capability matrix with LAW/GATE/NORM per cell; single-writer memory rules (registry = sole roster truth, ship files = rendered views, durable pending-decisions store); instruction-layer precedence + provenance + versioning.
Parallel track: the REAP-SHIP design proposal drafts alongside; ORCHESTRATOR-REPRESENTATION builds once its own design is ratified.
Mandated build order within item 1: close the raw-tmux backdoor FIRST, then inbox, then ACLs, then typing-guard LAST.

### NEAR

1. **Wave-1 flag-on validation for the engine supervisor** (gates the kokoro unit -> managed-child cutover): Windows build with `T_HUB_MANAGED_KOKORO` on in a controlled window; validate real spawn/kill, the adopt-and-disable-unit sequence, measure the true Piper cold start, live-verify the fallback toast/amber/remap; then the deliberate cutover with Cortana sequencing. Deferred-documented findings F5/F8 ride this wave.
2. **REAP-SHIP feature bundle** (design proposal first - destructive ops): deterministic `reap_ship` keyed off the captains registry with a HARD landed-gate per crew; prereqs: tab creator/owner metadata; all spawns socket-flowed for spawnedBy; registry self-heal for dead-session ghost tiles (PR #53 lineage - the design takes the fold-or-sequence question explicitly). Interim doctrine: RETIREMENT.md manual checklist.
3. **First-class ORCHESTRATOR representation** in the agents workspace (distinct from captains; interim `cortana` claim-slug removal is part of its definition of done; consider extending the Cortana-crown concept).
4. **closeWorkspace captain-guard confirm prompt** (optional; general chose SILENT re-place at PR #53 ratification - do NOT build unless he asks).
5. **Raw-session adoption gap**: the webview cannot adopt a live raw-tmux session (`move_tile` no-ops - no tile object); workaround = socket spawn + `--resume <uuid>` migration; proper fix = an adopt-session path (next onion layer past #51).
6. **Host load levers** (data first): dual-side load recorder at `~/.t-hub/captain/load-recorder.sh` -> load.log (nohup - dies on WSL restart, re-run or unit-ize).
Findings so far: WSL saturates CPU at compile peaks; Windows is memory-starved (WSL VM 19G of 31G + Chrome; 0.4-0.5G free), which makes `wsl.exe` spawns glacial - the exact seam behind the app's bounded-subprocess timeouts under load.
Levers: `.wslconfig` memory cap, Chrome trim, more RAM; optional product item = in-app resource surfacing.
7. **Post-0.3.59 wedge-saga carry-items**: (a) live-verify PR #50's heal loop if a real wedge ever presents; (b) N2 known limitation - a connect-level wedge presentation would not trigger the Timeout-only heal; (c) fleet hygiene - long-running app-spawned sessions' env-pinned tooling goes dark after every restart until the F2-fixed client is everywhere; (d) PR #49 M2 note - cross-client event ordering is per-socket, not global.
8. **Bound the other-subsystem subprocesses** (from the #48 sweep): unbounded `.output()`/`.status()` in files.rs, codex.rs, usage.rs, devserver.rs, recent.rs, claude/install.rs - route control-reachable ones through `bounded_exec`.
9. **PR #45 M1 spawn_terminal re-probe honest-limit**: add a probe key (client-supplied spawn tag) if spawn duplicates recur.
10. **PR #44 LOW watch-items**: ctx% meter mid-turn flicker; tile-header button crowding; DRY the `sessionIdByTmux` reverse scan.
11. **Auto-continue follow-ups** (all LOW): default-ON release note for v1 opt-in users; re-verify modal anchors against a real limited pane; account-wide Codex reset fan-out.

### FURTHER

1. **PR #34 orchestrator-wake fast-follows** (3 MEDIUM, on hold since 0.3.54): stale-UUID handling, no-suppression-timeout, live-validate the wake path.
2. **no-mistakes CI-step cwd bug** (different ship - route via Cortana). Memory `no-mistakes-ci-step-cwd-bug`.
3. **Registry slug-rename persistence inconsistency** across restarts.
4. **ensure-thub-mcp debug-binary repoint** via `T_HUB_MCP_BIN` once a packaged sidecar ships. Memory `captain-self-register-provisioning`.
5. **WorkspacesList Cortana rename** (small UI, general-queued).
6. **Server-split M2-M4** (remote), webview supervision cues, MCP parity for `create_worktree`/`remove_worktree`/`wait_for_status` (raw-socket-only today).
7. **Doc debt** (MEDIUM+ rewrites, still open): re-baseline `docs/ROADMAP-PLAN.md` + `docs/SERVER-SPLIT-AND-ROADMAP.md` (still cite v0.2.0-era figures); expand `docs/MCP.md` §2 with the #45 idempotency/retry contract; `docs/FEATURE-PLAN.md:3` historical banner nit.

## What shipped (for context)

- **0.3.54**: orchestrator-wake (#34), scribe gate (#35), pty forwarder leak (#36), per-session header glitch (#37), endpoint reconnect (#38).
- **0.3.55**: captains-render-fix (#39).
- **0.3.56**: Cortana crown pane header (#40), Scribe v1 dictation-state migration (#41).
- **0.3.57**: doc-staleness (#42), voice-gate dual-source + announce.log (#43), UI batch (#44), spawn-retry idempotency + de-wedge (#45), auto-continue small fix (#46).
- **0.3.58**: auto-continue full redesign default-ON (#47), control-socket flap fix (#48).
- **0.3.59**: EventFanout snapshot-then-write-unlocked (#49), relay-wedge self-heal (#50). The wedge saga resolved (stale-env-pin artifact - see above).
- **0.3.60**: agents plane renders socket-commissioned captains (#51; E2E-verified post-install).
- **0.3.61**: TTS engine health + never-silent fallback chime (#52); interim kokoro systemd supervision cutover alongside (kokoro-tts repo, local master).
- **0.3.62** (in flight): adopt/attach per-tile isolation + captain-guard + test-socket isolation (#53); managed Kokoro lifecycle engine supervisor, default-OFF flag (#54; amber degraded state + voice remap included; flaky churn test fixed + isolated by #53).
