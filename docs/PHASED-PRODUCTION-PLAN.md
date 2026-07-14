# T-Hub Phased Production Plan

**Updated:** 2026-07-14.
**Starting source:** `0571993` on `main`.
**Installed build:** T-Hub `0.3.66` from `a4fd704`.

## Planning Principles

Correctness, recoverability, clear ownership, and measurable resource behavior take priority over feature count.
Every phase must pass its exit gate before dependent work begins.
Each verified logical change receives its own commit.
Installation, publication, release, destructive cleanup, and external repository creation remain explicit operations.

The current user artifacts `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` must remain untouched unless the General explicitly approves changing their status.

## Current Baseline

The local Powder authority is healthy on WSL and reachable from Windows through the protected `n8desktop-wsl` Tailscale profile.
No T-Hub project is currently registered.
The two visible legacy Captain records are pinned visual records without a project or Powder binding.
Codex header identity has been checked, while the Claude header still needs interactive verification.
Board defaults to the wrong endpoint, and Preview requires an unclear Dev then Preview sequence.

Terminal resource counters and hot, warm, and cold lifecycle states are implemented.
The installed application is logging xterm `loadCell` and `isWrapped` failures that may be lifecycle races.
The diagnostic logs are oversized.

After removing 33 leaked Appturnity agent-browser sessions, WSL settled at approximately 3.1 GiB used memory and 98 to 99 percent idle CPU.
T-Hub itself used approximately 50 MiB, while its complete Windows descendant tree used approximately 1.06 GiB working set.
Seven T-Hub WebView2 processes accounted for approximately 733 MiB of that working set.
The T-Hub WebView2 GPU and renderer processes remained visible instantaneous CPU consumers after the unrelated browser leak was removed.

## Phase 1 - Terminal Correctness and Diagnostic Control

### Goal

Make the installed terminal lifecycle trustworthy before adding more automation or UI flows.

### Work

1. Reproduce the xterm errors through the packaged Windows application using end-user actions.
2. Exercise rapid workspace switching, terminal parking, 30-second warm expiration, cold disposal, restoration, resize, fullscreen, pop-out, and application restart.
3. Correlate each xterm exception with lifecycle state, terminal ID, slot ownership, queued output, replay, resize, and CanvasAddon state.
4. Fix disposal, write, replay, and resize ordering so no callback can reach a disposed xterm buffer or canvas.
5. Preserve the subscribe-before-attach and authoritative tmux replay boundary.
6. Add regression tests for the reproduced race, including delayed output and rapid hot to cold to hot transitions.
7. Add bounded diagnostic log rotation and retention.
8. Reduce repetitive diagnostic events while preserving failure evidence.
9. Verify that T-Hub still owns and closes the Windows supervisor process tree safely.

### Exit Gate

- No `loadCell`, `isWrapped`, blank-canvas, duplicate-attach, or stale-slot failure in packaged testing.
- Terminal input works immediately after cold restoration.
- Scrollback and current prompt match authoritative tmux state after restoration.
- Five terminals survive application restart and remain correctly labeled.
- Diagnostic logs remain within the configured retention bound.
- Frontend, Rust, TypeScript, build, and focused packaged lifecycle tests pass.

## Phase 2 - Unified Owned-Resource Lifecycle

### Goal

Prevent browsers, dev servers, terminals, worktrees, and Powder claims from outliving their useful owners without destroying recoverable work.

### Work

1. Define one resource ownership model for terminals, Crew, browser sessions, dev servers, worktrees, Powder claims, and temporary profiles.
2. Record the owner terminal, ship, project, Crew member, Powder card, process root, creation time, last activity, lease, and cleanup state where applicable.
3. Route browser creation through a managed T-Hub operation instead of allowing untracked `agent-browser` daemons.
4. Reuse one browser per active verification owner when isolation is not required.
5. Guarantee normal browser closure in a cleanup or `finally` path.
6. Renew browser leases only while the owner and browser command are live.
7. Mark resources orphaned when their owner disappears, then apply a visible grace period before cleanup.
8. Terminate known browser process trees gracefully, escalate after a bounded timeout, and remove temporary profiles only after processes exit.
9. Scope browser cleanup to registered process roots and known temporary profiles so normal Chrome is never targeted.
10. Manage Windows Lighthouse processes with the same ownership contract and process-tree containment.
11. Keep Captain and Crew records recoverable until landed-work and claim-release checks pass.
12. Add a Resources view showing owner, state, age, activity, and the exact effect of cleanup.
13. Add a reviewed **Clean orphaned resources** action.
14. Reconcile owned resources at T-Hub startup and after WSL restart.

### Exit Gate

- One hundred browser verification start and stop cycles return to the original process count.
- Killing a browser client, Crew terminal, T-Hub window, or WSL bridge leaves no unowned browser tree after the grace period.
- Normal Windows Chrome remains untouched in automated cleanup tests.
- Dirty or unmerged worktrees are never removed automatically.
- Powder claims are released only after confirmed terminal shutdown and are retained visibly when release fails.
- The Resources view agrees with operating-system process evidence.

## Phase 3 - Codebase and Captain Creation

### Goal

Make Captain creation understandable for saved, existing, and completely new projects.

### Work

1. Replace **Registered project** with **Saved codebase**.
2. Replace **Register repository** with **Choose existing codebase**.
3. Rename **Powder repository** to **Powder board** or **Work board** in user-facing UI.
4. Move connection-profile selection under **Advanced** and default to the one healthy profile when unambiguous.
5. Add three entry paths: saved codebase, existing WSL folder, and new codebase.
6. Build a WSL-native folder picker with home and recent shortcuts, breadcrumbs, parent navigation, Git indicators, and manual-path fallback.
7. Detect the canonical main worktree, remote, default branch, dirty state, and existing worktrees before registration.
8. Offer explicit Git initialization when an existing folder is not a repository.
9. Add a reviewed new-codebase transaction supporting empty projects, templates, and clones.
10. Never silently replace an existing directory or initialize version control without confirmation.
11. Add Powder board selection and, when Powder authorization supports it, explicit board creation.
12. Add one preflight summary covering filesystem changes, Git state, Powder health, board binding, assignment, harness, and external effects.
13. Use the same backend operations for the graphical flow and the Cortana conversational flow.
14. Create a named project workspace at commissioning time for future Crew.
15. Roll back incomplete state safely while preserving pre-existing directories and useful local work.

### Exit Gate

- Saved, existing Git, existing non-Git, empty new, template new, and clone flows pass packaged E2E tests.
- Cancel and failure at every transaction boundary leave understandable, resumable state.
- The UI never requires the user to understand the internal project record or protected-profile implementation.
- The orchestrator and graphical UI produce identical project, Powder, Captain, and workspace records.
- One live Captain per project is enforced with a clear explanation and recovery path.

## Phase 4 - Real Powder Captain and Crew Acceptance

### Goal

Prove the complete Captain and Crew workflow against the local Powder authority.

### Work

1. Reconcile or remove the two legacy pinned Captain records without losing live terminal state.
2. Register the T-Hub codebase and bind it to the canonical `t-hub` Powder board through `n8desktop-wsl`.
3. Commission a disposable Codex Captain through the product UI.
4. Commission a disposable Claude Captain through the product UI after the first Captain is safely released.
5. Verify the Codex and Claude header labels interactively.
6. Create real acceptance cards and dispatch Codex and Claude Crew into deliberate shared project workspaces.
7. Verify checkout and worktree validation, card ownership, claim acquisition, harness launch, and sidebar Crew visibility.
8. Verify claim heartbeat and renewal while tmux proves liveness.
9. Verify terminal close, claim release, Crew state update, and safe worktree retention.
10. Verify incomplete dispatch rollback at every failure boundary.
11. Verify Powder event delivery, cursor advancement, idempotent wake handling, and repository filtering.
12. Verify Captain context reset, T-Hub restart, WSL restart, and durable bootstrap recovery.
13. Clean all disposable acceptance state through the Phase 2 resource workflow.

### Exit Gate

- Both Captain harnesses commission, recover, supervise, and release cleanly.
- Both Crew harnesses claim, work, report, and close cleanly.
- Crew appear in the intended workspace and under the correct Captain.
- No raw key appears in project state, logs, terminal prompts, or documentation.
- Powder and T-Hub agree on cards, runs, claims, terminals, and cleanup outcomes.

## Phase 5 - Board, Preview, Files, and Workspace Polish

### Goal

Make the primary project surfaces work without hidden setup knowledge.

### Work

1. Resolve Board from the focused project's Powder binding instead of the global `http://localhost:4000` default.
2. Display a clear unbound or unavailable state rather than a generic broken iframe.
3. Open the correct Powder board without placing credentials in URLs or frontend-persisted state.
4. Preserve external-browser fallback when framing is blocked.
5. Replace the Dev then Preview sequence with one **Run and Preview** flow.
6. Detect package scripts, allow command selection, bind the server to a reachable interface, detect the port, and probe Windows reachability.
7. Show startup output, health, URL, stop, restart, and failure reasons in one place.
8. Suspend or dispose hidden preview and board activity when it has no visible consumer.
9. Reuse the WSL picker for Files roots and Captain creation.
10. Either implement the existing `filesRootDir` setting or remove the dead setting.
11. Add clear Captain, Crew, workspace, worktree, and Powder board labels throughout the sidebar and dialogs.
12. Verify pixel quality, keyboard access, error states, narrow layouts, and high-DPI behavior.

### Exit Gate

- Board opens the correct project board with no manual URL configuration.
- Run and Preview starts and stops representative Vite, Next.js, and static-server projects.
- Preview distinguishes unreachable servers from iframe security restrictions.
- Files and Captain creation select the same canonical WSL path.
- No hidden Board or Preview surface produces sustained CPU activity.

## Phase 6 - Measured Runtime Efficiency

### Goal

Reduce T-Hub's steady CPU, memory, process, and startup cost using packaged measurements rather than intuition.

### Work

1. Capture clean packaged baselines with 1, 4, 8, and 16 declared terminals after Phase 1 and Phase 2.
2. Include hot, warm, cold, Board, Preview, Captain, Crew, and browser-resource scenarios.
3. Attribute the two observed WebView2 CPU consumers to renderer work, GPU work, xterm canvas, animation, polling, or repaint scheduling.
4. Stop unnecessary animation frames, canvas redraws, cursor work, and layout measurement when surfaces are hidden or unchanged.
5. Skip Powder event polling when no active Captain can receive events.
6. Cache Powder profile resolution, credentials, clients, and HTTP connection pools with explicit refresh behavior.
7. Enable the existing binary PTY framing with a tested version fallback.
8. Remove the live JSON and base64 encode/decode chain from the terminal output hot path.
9. Coalesce terminal, focus, Git, recent-session, usage, and pane scans.
10. Pause low-priority polling for hidden windows, cold terminals, inactive panels, and disabled features.
11. Reduce watchdog cadence after failures are measurable through event-driven diagnostics.
12. Lazy-load and prune icon resolvers by selected theme.
13. Reduce the current approximately 1.21 MB main JavaScript chunk and 3.72 MB icon chunk before gzip.
14. Measure process birth and death, handles, threads, sockets, WSL relay processes, and memory recovery after close.
15. Run a 24-hour packaged soak with normal workspace switching and agent activity.

### Exit Gate

- Packaged runs are release-eligible with no unexplained process churn or incomplete CPU intervals.
- Hidden and cold terminals create no sustained rendering CPU.
- Closing terminals, previews, browsers, and workspaces returns process and memory counts toward baseline.
- The 1, 4, 8, and 16 terminal matrix meets documented CPU, memory, input-latency, and restoration budgets.
- The 24-hour soak has no growing process, handle, socket, log, or memory trend.

## Phase 7 - Infrastructure, Security, and Release Hardening

### Goal

Make the validated behavior safe and repeatable as a production release.

### Work

1. Resolve the WSL Tailscale DNS configuration and repeated duplicate-bind warnings.
2. Document the expected Windows-to-WSL route and the nonessential WSL self-hairpin limitation.
3. Complete Tauri Content Security Policy hardening for app, Board, and Preview surfaces.
4. Add Authenticode signing for the Windows executable and installer.
5. Add dependency, secret, vulnerability, and license scanning.
6. Complete strict branch protection and required status checks.
7. Add packaged Windows, WSL, tmux, Codex, Claude, Powder, Board, Preview, and resource-cleanup E2E coverage.
8. Keep every external workflow action pinned to an immutable revision.
9. Validate installer upgrade, rollback, state migration, and uninstall behavior.
10. Verify protected Powder profile permissions and credential redaction on every supported path.
11. Produce an SBOM and retain build identity, source commit, installer hash, and installed-binary hash.

### Exit Gate

- Security scans and required CI checks pass on the exact release source.
- The signed installer upgrades from the previous installed build without losing sessions or registry state.
- Packaged E2E covers the complete Captain and Crew acceptance path.
- No Critical, High, or unresolved Medium finding remains under the documented threat model.
- Release artifacts can be traced to an exact commit and immutable workflow inputs.

## Phase 8 - Documentation, Versioning, and Production Release

### Goal

Ship a coherent product whose terminology, help, handoff, and installed behavior agree.

### Work

1. Update user documentation for codebases, projects, Powder boards, Captains, Crew, workspaces, worktrees, and resource cleanup.
2. Mark historical design documents as superseded only after explicit approval to modify them.
3. Retain Lavish review artifacts as history or move them to an approved archive location.
4. Update the zero-context handoff with exact source, installed build, runtime state, tests, measurements, and remaining risks.
5. Bump the desktop version only after the intended release contents are verified.
6. Build the signed production installer from the exact reviewed commit.
7. Install and run final interactive acceptance on Windows.
8. Verify version, PID, executable hash, sessions, Powder, Tailscale, Board, Preview, Captain, Crew, and cleanup after installation.
9. Push and publish only when the General requests it.

### Exit Gate

- Documentation matches visible product terminology and behavior.
- The installed version and binary hash match the release artifact.
- Final interactive and automated acceptance pass on the installed build.
- The working tree contains no accidental generated, secret, or user-artifact changes.
- The handoff names the next action without relying on conversation history.

## Recommended Execution Order

Phases 1 and 2 are the immediate correctness and resource-safety tranche.
Phase 3 follows only after those foundations are stable.
Phase 4 proves the real orchestration model before UI surfaces are declared complete.
Phase 5 finishes the primary user experience.
Phase 6 optimizes the accepted behavior with clean measurements.
Phases 7 and 8 harden and release the resulting system.

Small independent preparations may run in parallel when they cannot change shared runtime state or obscure a phase's acceptance measurements.
No later phase should be used to waive an earlier exit gate.
