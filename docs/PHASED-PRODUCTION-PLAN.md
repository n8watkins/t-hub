# T-Hub Phased Production Plan

**Updated:** 2026-07-15.
**Plan source:** `5b8a542` on `main` plus the product decisions recorded after that commit.
**Installed build:** T-Hub `0.3.93` from exact detached source `e95eb56`, restored on the canonical profile as Windows PID `23436` when this plan was refreshed.
**Purpose:** This is the canonical zero-context roadmap for completing T-Hub.

## How to Use This Plan

Read this document before starting a new implementation session.
Use [REVIEW-INDEX.md](./REVIEW-INDEX.md) to distinguish canonical, supporting, historical, and archived documents.
Treat the phase exit gates as product requirements, not suggestions.
Work may proceed in parallel only where the dependency map explicitly permits it.
Do not use a later phase to waive an earlier correctness or safety gate.
Commit each verified logical change separately.
Push, publish, install, create external repositories, spend money, or perform destructive cleanup only with the General's authorization.

The user artifacts `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` must remain untouched unless the General explicitly approves changing their status.

## Product Vocabulary

- **General:** The human user and final authority.
- **Cortana:** The permanent lightweight T-Hub orchestrator identity.
- **Project:** A saved codebase and its canonical repository or main worktree metadata.
- **Assignment:** The durable responsibility given to one Captain within a Project.
- **Captain:** A durable agent identity responsible for an Assignment and any Crew it creates.
- **Workspace:** A coherent workstream or feature grouping controlled by a Captain.
- **Crew:** A bounded worker agent assigned to a Workspace, worktree, and normally a Powder card.
- **Powder board:** The authoritative work ledger containing cards, claims, runs, work logs, input requests, completion evidence, and events.
- **Harness:** The agent runner, initially Codex or Claude Code, with future adapters such as a GLM-compatible runner.
- **Provider:** The model or account provider used by a Harness.
- **History:** The provider-agnostic catalog of resumable and archived agent sessions.
- **Provider limits:** Account-level usage or rate-limit windows, distinct from conversation context and local resource pressure.

## Settled Operating Decisions

1. Cortana always exists as a durable identity.
2. Cortana may change terminal, Harness, Provider, or model without losing identity or checkpoints.
3. Cortana is a lightweight operational coordinator, not a Captain-of-Captains that decomposes implementation work.
4. Multiple Captains may work in one Project.
5. A Captain owns an Assignment, not the entire Project.
6. A Captain may control zero, one, or several Workspaces.
7. A Workspace contains related Crew and worktrees.
8. A Captain terminal does not require a dedicated work Workspace.
9. Completing a Workspace does not retire its Captain.
10. Resetting context does not retire a Captain.
11. Broken terminals trigger recovery rather than retirement.
12. Cortana retires a Captain only through explicit or previously delegated retirement intent and only after safety checks pass.
13. Captains may message other Captains for coordination, requests, and technical help.
14. Peer messaging grants communication but no command authority over another Captain or its Crew.
15. T-Hub should default Cortana, Captains, and Crew to unrestricted execution while displaying that authority clearly.
16. The initial Codex default is the user's configured `gpt-5.6-sol` with medium reasoning effort.
17. The control plane should be CLI-first, with MCP retained as an optional thin adapter over the same operations.
18. History, lifecycle telemetry, voice, notifications, and settings should be provider-agnostic.
19. Powder remains authoritative for work state, while T-Hub remains authoritative for runtime identity, terminals, Workspaces, and owned resources.
20. Raw CPU, RAM, process, and context samples remain local to T-Hub rather than turning Powder into a telemetry database.
21. Agent work state and runtime health are independent axes governed by [STATUS-MODEL.md](./STATUS-MODEL.md).
22. Worktree identity, ownership, freshness, and cleanup safety are computed once by the backend under [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md).

## Current Baseline

The local Powder authority is healthy on WSL at `127.0.0.1:4017`.
Windows reaches Powder privately through Tailscale Serve at `https://n8desktop-wsl.tailae53f1.ts.net`.
The protected Powder profile is `n8desktop-wsl` and authenticated remote operations have passed.
The `t-hub` Powder board and `thub-local-acceptance` card exist.
No T-Hub Project is currently registered.
The visible legacy Captain records are pinned visual records without complete Project or Powder bindings.

Terminal resource counters and hot, warm, and cold lifecycle states are implemented.
The earlier installed application reproduced xterm `loadCell` and `isWrapped` lifecycle failures.
Source commit `6870444` fixed the teardown race, and the packaged `3b83b9e` build completed cold rehydration with zero `loadCell`, `isWrapped`, or window errors.
Source commit `35fbae2` also prevents a second packaged launch from creating a competing control server and duplicate PTY attachments.
Source commit `3b83b9e` defers frontend resize commands until remote PTY attachment is confirmed, eliminating repetitive startup `no live terminal` diagnostics in packaged testing.
Source commit `a00ce7d` adds explicit Git initialization to the shared Project registration transaction used by Captain creation and MCP.
The transaction atomically reserves a new `.git` directory, initializes `main`, refuses any pre-existing `.git` entry, and removes only the `.git` directory it created when a later registration boundary fails.
Source commit `2cf4a42` adds explicit creation of one absent empty-codebase leaf to the same transaction.
It requires Git initialization, never replaces an existing path, never creates missing parents, and uses non-recursive rollback for the directory it owns.
Diagnostic logs are bounded at startup.
Installed `0.3.68` reduced the retained backup from `135,278,300` bytes to `8,388,493` bytes while preserving complete newest lines.
Installed `0.3.75` replaces the global Board URL and iframe with a native read-only Board resolved from the focused terminal's durable Project and protected Powder binding.
The packaged no-Project path shows an honest state without an iframe or manual Board URL field.
The bound Project success path remains gated on Phase 8 real Project acceptance.
Installed `0.3.76` replaces the separate Dev and Preview tabs with one **Run and Preview** surface and removes terminal-output URL scanning and automatic navigation.
Packaged verification proved that the previously reproduced WebView inspection URL remained in PTY scrollback while the preview URL stayed empty and no iframe was created.
Source commit `61f56ba`, packaged first as `0.3.77`, adds typed root-package target discovery and generation-safe backend lifecycle snapshots.
Packaged `0.3.78` discovered the T-Hub root's four declared scripts, started `pnpm run dev`, detected Vite at `http://localhost:1420/`, and stopped the exact managed run without leaving its Vite descendant alive.
Source commits `fbacc8f`, `16480b7`, and `19dc3c7`, packaged as installed `0.3.81` from `8f5fffa`, make standard Tauri Vite targets bind all WSL interfaces and preserve a localhost URL when Windows can already reach it.
Packaged acceptance started the real T-Hub Vite target, loaded `http://localhost:1420/` in the Preview iframe, returned Windows HTTP 200, bound ports `1420` and `1421` to `0.0.0.0`, and removed both listeners and the exact managed Vite PID on Stop.
Source commit `9d95fa9`, packaged as installed `0.3.82` from `ec55526`, gives each managed package target one WSL process group behind an application-owned stdin lifeline and retained Windows Job Object.
Packaged acceptance proved that Stop returned in 161 milliseconds, removed the run marker and every pnpm, Vite, and esbuild process, released ports `1420` and `1421`, and restarted successfully on the same port.
Forcing the installed application to exit while the restarted target was active also removed the complete process group and both listeners while preserving all seven unrelated tmux sessions and pane PIDs.
Installed `0.3.82` also passed representative Next.js acceptance against `apps/site`: Next `14.2.35` returned Windows HTTP 200, loaded both expected site sentinels in Preview, stopped in 161 milliseconds, and removed the complete npm and Next process group.
Source commit `5011803` adds a typed package-less static target backed by a Windows loopback-only server with traversal, hidden-path, reparse-point, symlink, MIME, method, and file-size protections.
Source commit `3177d81` clears a managed Preview URL and iframe when its run stops, and `0cd5861` packages the final result as installed `0.3.84`.
Packaged static acceptance auto-loaded the authoritative loopback URL, served HTML, CSS, JavaScript, and a nested page, denied raw, encoded, and double-encoded traversal plus hidden and symlink paths, stopped and remounted cleanly, and closed its listener on forced application exit.
The disposable acceptance session was removed afterward, and all seven canonical tmux sessions and pane PIDs remained unchanged.
Source commit `1484750` serializes managed Preview operations per terminal, protects snapshots with operation and generation ownership, rehydrates rejected frontend starts from the authoritative backend snapshot, bounds static request and response work, validates exact loopback Host headers, and serves files through capability-relative no-follow handles.
Commit `9005117` packaged that source as `0.3.85`; its native Windows focused Preview suite passed 23 tests, including directory-junction rejection, and its standalone executable, NSIS, and MSI SHA-256 values are `870E71B240B13675F2F717EB786C97F91FDABBD5BFD89E0504E43B2D9E87624D`, `B227A48FDEEF8589E629CA286898FE42ECFCA336C860AE177AC17A6C02A5E121`, and `81A38DB62EE8A68F43DD3F5C3473BC16044F28BF6B4B225FD85630F7315DA454`.
That `0.3.85` artifact was not installed because native compilation exposed two platform-specific test warnings.
Source commit `d05073d` removes those warnings, and `5ea945c` packages the result as installed `0.3.86`.
The exact `0.3.86` source passed 621 Linux Rust library tests with one ignored, warning-denied Clippy, the production frontend build, and 23 focused native Windows Preview tests without warnings.
Packaged `0.3.86` discovered exactly one typed static target, enforced MIME, method, security-header, traversal, hidden-path, symlink, size, and exact Host rules, preserved unique run ownership across restart and stale Stop, and cleared its authoritative URL on final Stop.
While one terminal had a nonreading 16 MiB response, its Stop completed in 206 milliseconds and a second terminal independently reached `running` with a distinct run ID and URL.
After the final normal relaunch, only the application control listener remained and the six tmux sessions present before installation retained the same names and pane PIDs.
The previously recorded session `th_a486c7fc` was already absent before installation, so it is not claimed as preserved by this acceptance run.
Generic non-Tauri Vite launch adapters and stale WSL-address recovery remain open.
Source commit `776439a`, packaged as `0.3.78` from `b4a1c5d`, removes full tmux capture replay from terminal attachment and stops clearing inline transcript during header Refresh.
The packaged acceptance preserved all eight tmux pane PIDs and the same Codex process chain through install, header Refresh, and full relaunch while the active draft appeared exactly once.
The Codex header identity has been checked interactively, while the Claude header still needs interactive confirmation.

The durable inbox substrate implements persistence, ordering, priorities, receipts, crash recovery, sender attribution, and role-based access controls.
It is not yet a complete Captain and Crew communication product.
Agent-to-agent send is not exposed through the normal CLI or MCP catalogs.
Generic delivery, receive, acknowledgement, message history, and frontend visibility remain incomplete.

Claude currently has the strongest T-Hub integration through fifteen lifecycle hooks and a structured status-line bridge.
Codex has a current lifecycle-hook framework, but T-Hub has not integrated it.
Interactive Codex therefore lacks dependable context, supervision, attention, voice, and History parity.

The Windows and WSL TTS endpoints are healthy on ports `7477` and `7478`.
Voice settings are enabled with Kokoro selected and attention announcements enabled.
Automatic spoken announcements currently depend on needs-input status transitions, which interactive Codex does not reliably produce.

The installed `th` CLI reports version `0.2.0`.
Source commit `07e74f4` upgrades the control protocol and recovers from a stale inherited endpoint after application port rotation while preserving the caller's token capability.
Live `health` and `ls` checks now rediscover the packaged application promptly in the normal case.
One post-install `ls` call still hit the bounded 10-second WSL command timeout and succeeded on retry, so transient WSL command latency remains Phase 1 work.
The source documents a newer interface, but the CLI still lacks most Captain, Powder, Workspace, resource, and inbox commands.
The source CLI already has a useful Rust control-client boundary, a no-argument fleet view, deterministic human rendering, a stable JSON envelope, and an established exit-code taxonomy that should be preserved.
The CLI contract audit also found that unknown flags can be accepted silently, per-subcommand help is absent, `worktree rm` has no explicit confirmation gate, some diagnostics can leak into JSON-mode stderr without a structured suggestion, and JSON collections are described as unbounded.
The project-specific target is now defined in `docs/cli-contract.md` and intentionally uses stable JSON without an AXI dependency or a claim of AXI compliance.
Tile headers use authoritative Git branch and dirty data, but the Worktrees dialog exposes only branch, path, and main or linked state.
Recent, Captain, and Workspace rows still contain folder-name worktree heuristics that can disagree with authoritative Git state.
The shared status indicator also combines agent work with terminal lifecycle in some surfaces and can replace exact agent status with a generic terminal tooltip.

## Critical Path

The release critical path is:

1. Terminal and control reliability.
2. Owned-resource lifecycle safety.
3. Durable identity and organizational model.
4. Provider-agnostic Harness integration.
5. CLI-first control and durable messaging.
6. Codebase and Captain creation.
7. Real Powder-backed Captain and Crew acceptance.
8. Primary product surfaces and observability.
9. Measured performance, security, and release hardening.

## Phase 1 - Terminal and Control Reliability

**Status:** Complete through installed `0.3.78` from source `b4a1c5d`.
Installed `0.3.67` reproduced the end-user xterm lifecycle failure as `Cannot set properties of undefined (setting 'isWrapped')` during parser line feed.
Installed `0.3.69` then reproduced `Cannot read properties of undefined (reading 'replaceCells')` during rapid Workspace switching and zoom-driven resize.
Source commits `cfa4139` and `cbc558b` serialize resize behind accepted xterm writes and leave xterm's parser callback stack before buffer mutation.
Source commit `1e005e6` converts matching backend detach races into liveness-checked reattachment instead of unhandled `no live terminal` rejections.
One warm stress pass and two cold restart passes on installed `0.3.71` preserved the same eight tmux session IDs and produced zero xterm corruption, detach-race, or unhandled errors.
Duplicate launch retained one Windows PID, `th ls --json` returned all eight sessions after every restart, and diagnostic retention remained within its configured bound.
Installed `0.3.77` reproduced a second user-visible lifecycle regression: cold rehydration replayed a linearized tmux capture and then streamed the attach client's current redraw, visually duplicating an entire Codex frame and its composer draft.
The same path could clear inline transcript during a width-changing header Refresh before the asynchronous backend redraw completed.
Source commit `776439a` makes the attached tmux client the only current-screen renderer and removes the resize-time transcript clear.
Installed `0.3.78` restored the same Codex transcript and one visible draft after header Refresh and full application relaunch, with all eight tmux sessions and the exact Codex process chain unchanged.

### Goal

Make the installed terminal cockpit and its control clients trustworthy before expanding orchestration.

### Work

1. Reproduce the xterm failures through packaged Windows end-user actions.
2. Exercise rapid Workspace switching, terminal parking, warm expiration, cold disposal, restoration, resize, fullscreen, pop-out, and application restart.
3. Correlate each exception with lifecycle state, terminal ID, slot ownership, queued output, replay, resize, and CanvasAddon state.
4. Fix disposal, write, replay, resize, and renderer ordering so callbacks cannot reach disposed state.
5. Preserve subscribe-before-attach and authoritative tmux replay boundaries.
6. Add bounded diagnostic log rotation and retention.
7. Reduce repetitive diagnostics while preserving failure evidence.
8. Upgrade and install the current `th` CLI.
9. Add endpoint rediscovery, stale-pin recovery, bounded timeouts, and non-hanging failure behavior to `th`.
10. Verify that T-Hub owns and closes its Windows supervisor process tree safely.

### Tests and Evidence

- Add a regression test that reproduces each xterm race before implementing its fix.
- Add delayed-output and rapid hot to cold to hot lifecycle tests.
- Add CLI restart, stale endpoint, timeout, protocol mismatch, and malformed-response tests.
- Run frontend tests, Rust workspace tests, TypeScript, formatting, Clippy with warnings denied, and a production frontend build.
- Run packaged Windows interaction tests with at least five terminals.

### Exit Gate

- No `loadCell`, `isWrapped`, blank-canvas, duplicate-attach, or stale-slot failure appears in packaged testing.
- Terminal input works immediately after cold restoration.
- Scrollback and the current prompt match authoritative tmux state after restoration.
- Five terminals survive application restart and remain correctly labeled.
- `th health`, `th ls`, and a mutation-denial test return promptly against live and restarted applications.
- Diagnostic logs remain within the configured retention bound.

## Phase 2 - Unified Owned-Resource Lifecycle

**Status:** Active after Phase 1; managed development-server ownership is partially implemented and packaged `0.3.90` verifies that worktree removal is suspended fail closed, while the unified resource record, browser lifecycle, worktree status service, Resources surface, startup reconciliation, and full exit gate remain open.
The unified worktree snapshot's Captain, Assignment, Workspace, Crew, resource-lease, and Powder ownership fields depend on the Phase 3 B1 durable identity interfaces, so independent Phase 2 work may proceed but the full worktree slice cannot exit before B1 stabilizes.
Installed `0.3.86` reproduced the owned-resource failure by deleting a disposable linked worktree while a live tmux session remained rooted inside it, leaving that pane at a `(deleted)` cwd.
Source `0.3.90` now refuses graphical, direct Tauri, control, MCP, and CLI removal before UI detachment or Git mutation, including with force.
The exact detached `0.3.88` native Windows suite proved the refusal but exposed a test fixture that mixed native Windows Git registration with the production WSL Git removal path.
Commit `f62f188` replaces that fixture with the real production WSL path boundary, and `2c6a429` bumps the corrected source to `0.3.89` under the every-change version policy.
The exact detached `0.3.89` suite then proved the WSL Git fixture creation and public refusal but exposed a Windows UNC access denial in its host-side existence assertion for a mounted-drive fixture.
Commit `3841c2e` checks that same fixture through its retained native host path without changing any production WSL Git operation, and `e26fe2e` bumps the corrected source to `0.3.90`.
The exact detached `0.3.90` native Windows suite passed all four focused removal tests.
Installed `0.3.90` then verified the graphical and direct Tauri preflight, normal, and forced paths against disposable live worktrees: every operation returned the exact temporary-unavailable refusal before UI detachment or Git mutation, the graphical tile remained present, Git registration remained intact, and both live tmux pane paths remained valid.
Source tests cover the same synchronous refusal for control, MCP, and CLI callers; the installed read-only session did not elevate itself with raw credentials to repeat those mutation channels at runtime.
This is a temporary suspension rather than implementation of the unified worktree status service, so full service activation and acceptance remain open.

### Goal

Prevent terminals, browsers, development servers, worktrees, and Powder claims from outliving useful owners without destroying recoverable work.

### Work

1. Define one resource record for terminals, Crew, browsers, development servers, worktrees, Powder claims, temporary profiles, and Windows subprocess trees.
2. Record the owner identity, Project, Assignment, Captain, Workspace, Crew member, Powder card, process root, creation time, last activity, lease, and cleanup state.
3. Route browser creation through a managed T-Hub operation instead of untracked `agent-browser` daemons.
4. Reuse one browser per active verification owner when isolation is unnecessary.
5. Guarantee normal browser and development-server closure through owned cleanup paths.
6. Renew leases only while both owner and resource remain live.
7. Mark resources orphaned when owners disappear and expose a visible grace period before cleanup.
8. Terminate registered process trees gracefully and escalate after a bounded timeout.
9. Remove temporary profiles only after their processes exit.
10. Never target ordinary user Chrome processes.
11. Manage Windows Lighthouse and preview processes with the same ownership contract.
12. Keep Captain and Crew records recoverable until landed-work and claim-release checks pass.
13. Add a Resources view with owner, state, age, activity, and proposed cleanup effect.
14. Add a reviewed **Clean orphaned resources** action.
15. Reconcile owned resources at T-Hub startup and after WSL restart.
16. Implement the unified worktree status service and safety decisions defined in `docs/WORKTREE-STATUS-CONTRACT.md`.

### Tests and Evidence

- Run one hundred managed browser start and stop cycles.
- Kill browser clients, Crew terminals, T-Hub, and the WSL bridge at controlled points.
- Verify that dirty, unmerged, or leased worktrees are never automatically removed.
- Verify that Powder claims release only after confirmed terminal shutdown.
- Verify process ownership against Windows and WSL operating-system evidence.
- Verify that backend, CLI, MCP, and graphical surfaces return equivalent worktree identity, ownership, freshness, and safety decisions.
- Verify that graphical, Tauri, control, MCP, and CLI removal paths all fail closed before UI detachment or Git mutation while the unified service is unavailable, including with force.
- After the unified service exists, verify main, dirty, locked, terminal-owned, leased, claimed, stale, and unknown decisions plus preflight-to-mutation serialization before re-enabling removal.

### Exit Gate

- The browser cycle returns to the original process count.
- Orphaned registered resources disappear after the documented grace period.
- Ordinary Chrome remains untouched.
- Failed claim release remains visible and recoverable.
- The Resources view agrees with operating-system evidence.
- Dirty, leased, main, locked, stale, and unknown worktrees cannot be automatically removed or reused.

## Phase 3 - Durable Identity and Organizational Model

### Goal

Implement permanent Cortana identity, multiple Captains per Project, Assignment ownership, and correct Workspace semantics.

### Work

1. Separate Cortana identity from its current terminal, Harness, Provider, model, and conversation.
2. Auto-recover or recreate Cortana's runtime while preserving its durable identity and last safe checkpoint.
3. Allow explicit Cortana Harness and model changes through a reviewed operation.
4. Replace the one-live-Captain-per-Project registry constraint with multiple durable Assignments per Project.
5. Give each Captain a durable Assignment identity independent of its terminal and provider conversation.
6. Allow a Captain to control zero, one, or several Workspaces.
7. Treat Workspaces as coherent workstreams rather than Project, Captain, or Crew synonyms.
8. Allow Captains to create, name, rename, close, and reconcile their Workspaces.
9. Allow Captains to assign related Crew and worktrees to a Workspace.
10. Keep Captain identity alive after its final Workspace or Crew closes.
11. Add explicit checkpoint, context reset, recovery, and retirement state machines.
12. Implement the settled Cortana retirement policy with cleanup safety gates.
13. Display role, Assignment, Project, Harness, model, context, and unrestricted authority clearly.
14. Migrate legacy pinned and commissioned records without silently granting authority.

### Tests and Evidence

- Add registry migration tests from every supported previous schema.
- Commission two Captains in the same Project with distinct Assignments.
- Reset context and replace the Harness runtime without changing durable Captain identity.
- Kill and recover Cortana without creating a second Cortana identity.
- Verify that an empty Workspace does not retire its Captain.
- Verify that idleness and context pressure cannot trigger retirement.
- Verify retirement fails while unsafe Crew, claims, worktrees, browsers, or servers remain.

### Exit Gate

- One Project safely supports multiple live Captains.
- Cortana survives runtime replacement as one permanent identity.
- Captain, Assignment, Workspace, Crew, and Project records remain distinct and understandable.
- Recovery and retirement behavior match the settled policy.

## Phase 4 - Provider-Agnostic Harness Integration

### Goal

Give Codex, Claude Code, and future Harness adapters one normalized lifecycle contract.

### Normalized Adapter Contract

Each Harness adapter must define:

- Installation and version detection.
- Authentication and readiness checks.
- Start, resume, interrupt, checkpoint, reset, and recover operations.
- Provider session and conversation identity.
- Model and reasoning configuration.
- Permission mode and visible effective authority.
- Turn lifecycle and structured failures.
- Context telemetry.
- Provider-limit telemetry.
- Provider-limit auto-continue scheduling, cancellation, and recovery.
- Tool, task, and subagent lifecycle where available.
- History discovery and resume metadata.
- Hook installation, trust, health, repair, and removal.
- Capability flags for features the provider cannot supply.
- Authoritative and derived inputs for both axes in `docs/STATUS-MODEL.md`.

### Normalized Events

Adapters should map provider events into:

- `session.started`
- `session.ended`
- `turn.started`
- `turn.completed`
- `turn.failed`
- `input.requested`
- `permission.requested`
- `tool.started`
- `tool.completed`
- `context.compacting`
- `context.compacted`
- `subagent.started`
- `subagent.completed`
- `task.created`
- `task.completed`
- `cwd.changed`
- `worktree.created`
- `worktree.removed`

### Work

1. Move Claude-specific supervision assumptions behind the adapter boundary.
2. Integrate current Codex lifecycle hooks with `t-hub-agent`.
3. Add structured telemetry for interactive Codex sessions rather than relying on output activity.
4. Bind Codex thread IDs and Claude session IDs to durable T-Hub identities.
5. Add Codex context telemetry for the outer tile, sidebar, Cortana health, and reset recommendations.
6. Enable and verify the native Codex status line.
7. Apply unrestricted flags to fresh and resumed interactive Codex and Claude sessions.
8. Keep an Advanced override without burdening the normal commissioning flow.
9. Replace **Claude hooks** settings with **Agent integrations**.
10. Show each adapter's installed version, hooks, telemetry, History, permissions, and degraded capabilities.
11. Design the registry so a GLM-compatible adapter can be added without changing History or organizational schemas.
12. Implement Codex auto-continue after provider-limit reset by preserving the exact thread, pending continuation, reset time, and durable Captain or Crew identity.
13. Deduplicate scheduled continuation across app restarts, provider retries, repeated limit events, and simultaneous frontend clients.
14. Allow the General, Captain, or owning Crew policy to cancel or disable a pending continuation before it runs.
15. Replace provider-specific or terminal-output status inference with the two-axis work-state and runtime-health model.

### Tests and Evidence

- Add adapter contract tests that run against Codex and Claude fixtures.
- Add real interactive start, resume, input request, completion, failure, compaction, and context tests where each provider exposes the event.
- Add explicit degraded-capability tests where one provider lacks an event.
- Verify hook trust and repair behavior without overwriting user-authored hooks.
- Verify provider switching preserves T-Hub identity but never mixes incompatible conversation IDs.
- Verify authoritative, derived, stale, unknown, and conflicting status observations without fabricating unsupported provider events.
- Test Codex auto-continue with real and fixture limit events, exact reset-time scheduling, early retry backoff, app restart, duplicate events, cancellation, missing threads, and already-completed work.
- Verify auto-continue never submits a continuation to a different thread, retired identity, closed Assignment, or manually stopped session.

### Exit Gate

- Codex and Claude both drive dependable working, needs-input, completed, and failed states.
- Both Harnesses expose effective permission mode and provider identity.
- Codex context and resume identity are visible and recoverable.
- Codex auto-continue resumes the exact limited thread once after the provider window resets, or reports a clear recoverable failure.
- A future adapter can implement the normalized contract without changing Project, Captain, Workspace, History, or inbox schemas.
- Work completion, attention, runtime failure, and recovery remain distinct on every supported Harness.

## Phase 5 - CLI-First Control Plane

### Goal

Make `th` the canonical token-efficient control interface and keep MCP as an optional adapter.

Normalize the existing Rust CLI against `docs/cli-contract.md` before expanding its command surface.
Preserve the existing control-client architecture, JSON envelope, compatible aliases, and exit-code taxonomy unless a separately reviewed versioned migration requires a change.

### Work

1. Normalize the existing argument parser so every command rejects unknown flags and extra positional arguments before side effects.
2. Add concise per-subcommand `--help` with arguments, flags, defaults, and examples.
3. Preserve the stable `{ ok, command, data, error }` JSON envelope and established `0`, `2`, `3`, `4`, `5`, and `6` exit taxonomy.
4. Extend structured errors compatibly with stable symbolic kinds, actionable suggestions, and bounded optional details.
5. Make empty collections explicit, ordering deterministic, and human and JSON output bounded with totals plus `--all` or `--full` escape hatches.
6. Require `--confirm` before destructive effects, retain `--yes` only as a temporary compatibility alias where it already exists, and add `--dry-run` where practical.
7. Define one shared command catalog and schema source for the control server, CLI, and MCP adapter.
8. Add CLI groups for fleet, Projects, Captains, Crew, Workspaces, resources, Powder, History, inbox, context, provider limits, recovery, and retirement.
9. Preserve per-session identity, role, Project, and ownership checks through CLI calls.
10. Add idempotency keys and request-status recovery to every retryable mutation.
11. Add bounded waits and event subscriptions instead of encouraging polling loops.
12. Filter MCP tool exposure by role and capability.
13. Keep MCP for typed clients while avoiding a forty-tool schema burden in every agent context.
14. Add concise agent instructions and command help so agents discover CLI syntax on demand.
15. Ensure CLI and MCP return equivalent results for the same backend operation.
16. Consider `th capabilities --json` only after the expanded catalog makes capability discovery worth its maintenance cost.
17. Make worktree commands consume the unified backend snapshot rather than maintaining separate Git safety logic in the CLI.

### Tests and Evidence

- Add process-level contract tests for JSON isolation, strict flags and arguments, empty results, exit categories, no-ops, destructive confirmation, deterministic ordering, truncation, and `--full` behavior.
- Add parity tests that execute each shared operation through CLI and MCP.
- Add authorization tests for General, Cortana, Captain, Crew, read-only, and trusted-host callers.
- Measure prompt and tool-schema token overhead before and after role filtering.
- Test restart, timeout, retry, idempotency, and ambiguous-response recovery.
- Prefer structural JSON assertions and use exact-output snapshots only for a small set of intentionally reviewed public contracts.

### Exit Gate

- An agent can operate its allowed T-Hub workflow through `th` without MCP.
- MCP remains functional without defining separate behavior.
- CLI and MCP cannot bypass each other's authorization or identity rules.
- The reduced tool surface demonstrates lower context overhead.
- Unknown input fails before side effects, destructive actions require explicit confirmation, and all supported JSON output remains bounded, parseable, and compatible.

## Phase 6 - Durable Inbox and Agent Communication

### Goal

Complete a visible, recoverable communication layer for General, Cortana, Captains, and Crew.

### Work

1. Re-key recipients from temporary terminal IDs to durable role identities with terminal delivery bindings.
2. Expose send, list, read, reply, acknowledge, accept, decline, and complete operations through CLI and MCP.
3. Drain messages at safe provider turn boundaries for every supported recipient role.
4. Add an automatic receive and acknowledgement loop to each Harness adapter.
5. Preserve natural-language bodies alongside structured message types.
6. Support instruction, status, blocker, decision, completion, lifecycle, and coordination messages.
7. Link messages to Projects, Assignments, Workspaces, Crew, and Powder cards where applicable.
8. Distinguish enqueued, delivered, read, accepted, declined, and completed states.
9. Retain human-readable message history after transport queue compaction.
10. Add unread badges and an on-demand Messages timeline.
11. Allow Captain-to-Captain communication without granting terminal, Crew, or retirement authority.
12. Label cross-Project peer messages clearly.
13. Require transferred implementation work to receive an explicit Assignment or Powder card when ownership changes materially.
14. Add secret redaction and bounded retention controls.

### Recommended Retention Default

Keep local message bodies for thirty days.
Keep non-secret delivery metadata longer for recovery and audit.
Keep user-pinned messages until explicitly removed.

### Tests and Evidence

- Test crash recovery between enqueue, delivery, read, and acknowledgement transitions.
- Test ordering, priorities, overflow, duplicate acknowledgement, and idempotent reply behavior.
- Test every permitted and denied role pair.
- Test terminal replacement while messages remain queued.
- Test body redaction and verify that event telemetry does not expose message content implicitly.
- Run packaged Captain-to-Crew, Crew-to-Captain, Cortana-to-Captain, and Captain-to-Captain conversations.

### Exit Gate

- Messages survive application and terminal restarts.
- Each role receives messages only through permitted routes.
- The General can inspect message content and lifecycle without terminal scrollback.
- Peer communication cannot mutate another Captain's authority or resources.

## Phase 7 - Codebase and Captain Creation

### Goal

Make Captain creation understandable for saved, existing, and completely new codebases.

### Work

1. Replace **Registered project** with **Saved codebase** in user-facing copy.
2. Replace **Register repository** with **Choose existing codebase**.
3. Rename **Powder repository** to **Powder board** or **Work board**.
4. Move protected connection-profile selection under **Advanced** and default it when unambiguous.
5. Add saved codebase, existing WSL folder, and new codebase entry paths.
6. Build a WSL-native folder picker with home and recent shortcuts, breadcrumbs, parent navigation, Git indicators, and manual-path fallback.
7. Detect the canonical main worktree, remote, default branch, dirty state, and existing worktrees.
8. Use the unified worktree status contract for preflight identity, ownership, freshness, and safety decisions.
9. Offer explicit Git initialization for non-repository folders.
10. Add a reviewed new-codebase transaction for empty projects, templates, and clones.
11. Never silently replace a directory or initialize version control.
12. Add Powder board selection and explicit creation when Powder authorization permits it.
13. Add a preflight summary for filesystem changes, Git state, Powder, Assignment, Harness, model, permissions, and external effects.
14. Use the same backend transaction for graphical and Cortana conversational flows.
15. Commission the Captain identity without forcing creation of an unrelated work Workspace.
16. Offer creation of an initial Workspace when the Assignment already names a coherent workstream.
17. Roll back incomplete state while preserving pre-existing directories and useful work.

Phase 7 remains active.
Phase 7 item 8 depends on the Phase 2 unified worktree status service, whose durable ownership fields consume the Phase 3 B1 identity interfaces.
The multiple-Captain exit gate also depends on Phase 3 B1 replacing the one-live-Captain-per-Project constraint with durable Assignment identity.
Product-flow work may proceed only against stable shared contracts.
Items 1 through 7, 9, 11, and the existing-codebase portions of 13 through 15 are implemented.
Installed `0.3.72` now launches commissioned Codex and Claude Captains with explicit unrestricted permission flags, and its packaged review screen reports that authority as `Unrestricted`.
Installed `0.3.73` discovers visible canonical boards through the protected Powder profile, exposes bounded pagination through the shared control and MCP operation, and replaces free-text board entry with an accessible selection flow.
Packaged verification listed all 25 real boards from `n8desktop-wsl`, including `t-hub` with its one acceptance card, and preserved the selection in preflight.
Installed `0.3.74` adds a reviewed **Create new codebase** choice for one absent empty-codebase leaf, initializes Git with `main`, and reports the exact filesystem and external effects before creation.
Packaged cancel verification reviewed `/home/natkins/t-hub-cancel-proof-0-3-74`, closed the dialog, and confirmed that no directory was created.
Installed `0.3.86` reproduced the open template and clone gap: **Create new codebase** offers only **Starting point: Empty Git repository** and states that template and clone starting points will be added later.
The graphical flow currently sequences `register_project` and `commission_captain` in the frontend.
Registration transactionally owns its optional directory, Git initialization, Project record, and selected Powder binding, while commissioning separately rolls back only incomplete Captain state.
One reviewed backend transaction shared by graphical and Cortana flows, including explicit cross-operation resume or rollback, remains open.
The complete graphical packaged E2E matrix for existing non-Git success, empty success, template, and clone flows remains open.
The shared registration contract now requires `initializeGit: true` before it changes a non-repository folder, and its Rust integration tests cover success, downstream-failure rollback, pre-existing-file preservation, and refusal to rewrite a pre-existing `.git` entry.

### Tests and Evidence

- Add packaged E2E for saved, existing Git, existing non-Git, empty, template, and clone flows.
- Inject failure at every transaction boundary and verify safe resume or rollback.
- Test two Captains commissioned into the same Project.
- Test cancel behavior without residual directories, Project records, boards, or terminals.
- Test graphical and conversational parity.

### Exit Gate

- The user can create a Captain without understanding internal registry or credential-profile details.
- Multiple Captains in one Project receive distinct Assignments.
- No unwanted Workspace is created merely because a Captain exists.
- Preflight and rollback behavior remain understandable at every boundary.

## Phase 8 - Real Powder Captain and Crew Acceptance

### Goal

Prove the complete multi-Captain and Crew workflow against the local Powder authority.

### Work

1. Reconcile or retire legacy pinned Captain records without losing live terminal state.
2. Register the T-Hub codebase and bind it to the canonical `t-hub` Powder board through `n8desktop-wsl`.
3. Commission Codex and Claude Captains with distinct Assignments in the same Project.
4. Verify both terminal headers interactively.
5. Create real acceptance cards and dispatch Codex and Claude Crew into deliberate Workspaces.
6. Verify checkout, worktree, card ownership, claim acquisition, Harness launch, and sidebar visibility.
7. Verify claim heartbeat and renewal only while tmux proves liveness.
8. Exercise Captain-to-Captain and Captain-to-Crew messaging around a real dependency.
9. Verify terminal close, claim release, Crew state, owned-resource cleanup, and safe worktree retention.
10. Verify incomplete dispatch rollback at every failure boundary.
11. Verify Powder event delivery, cursor advancement, idempotent wake handling, and board filtering.
12. Verify Captain context reset, Cortana recovery, T-Hub restart, WSL restart, and durable bootstrap recovery.
13. Clean disposable acceptance state through the owned-resource workflow.

### Tests and Evidence

- Use real Powder cards rather than mocks for final acceptance.
- Keep deterministic mocked tests for each failure boundary.
- Capture Project, Captain, Workspace, Crew, terminal, claim, run, and message evidence before and after cleanup.
- Verify no raw key appears in logs, prompts, project state, message history, or documentation.

### Exit Gate

- Both Harnesses commission, recover, supervise, message, and retire cleanly.
- Both Crew Harnesses claim, work, report, and close cleanly.
- Multiple Captains coexist in one Project without identity or Workspace collisions.
- Powder and T-Hub agree on cards, runs, claims, terminals, messages, and cleanup outcomes.

## Phase 9 - Primary Product Surfaces

### Goal

Make Board, Run and Preview, Files, History, Provider limits, Messages, Resources, and settings work without hidden setup knowledge.

### Work

1. Resolve Board from the focused Project's Powder binding rather than `http://localhost:4000`.
2. Display clear unbound, unauthorized, unreachable, and framing-blocked states.
3. Avoid credentials in URLs and frontend-persisted state.
4. Preserve external-browser fallback when framing is blocked.
5. Replace Dev then Preview with one **Run and Preview** flow.
6. Detect package scripts, allow command selection, bind a reachable interface, detect the port, and probe Windows reachability.
7. Show startup output, health, URL, stop, restart, ownership, and failure reasons together.
8. Suspend hidden Board and Preview activity without a visible consumer.
9. Reuse the WSL picker for Files roots.
10. Implement or remove the dead `filesRootDir` setting.
11. Replace **Recent** with provider-agnostic **History**.
12. Group History by durable Project, Captain, role, Harness, and conversation.
13. Support resume, recover, archive, and compatibility states for Codex and Claude.
14. Rename **Usage** to **Provider limits**.
15. Keep conversation context, provider limits, and local resource pressure visually distinct.
16. Add Messages and Resources surfaces with compact unread and warning badges.
17. Add Agent integrations settings and effective unrestricted-permission badges.
18. Add per-session Codex and Claude auto-continue controls with pending, scheduled, cancelled, resumed, and failed states.
19. Show the provider reset time, exact target session, and cancellation action without exposing internal credentials or prompts unnecessarily.
20. Add clear Project, Assignment, Captain, Workspace, Crew, worktree, and board labels.
21. Render work state as the primary status and runtime degradation as a separate secondary signal under `docs/STATUS-MODEL.md`.
22. Replace path-derived worktree labels with authoritative branch and worktree identity wherever current state is available.
23. Verify keyboard access, narrow layouts, high DPI, error states, and visual quality.

Board items 1 through 4 and the Board portion of item 8 are implemented in source commit `6c6e4ee` and packaged as installed `0.3.75` from `15ab30f`.
The backend resolves Captain, Crew, or canonical Git main-worktree identity to one registered Project and returns a bounded repository-filtered Powder snapshot without sending credentials to the frontend.
The native read-only surface covers loading, empty, unbound, unauthorized, unreachable, missing-repository, truncated, and generic error states, with retry and an explicitly unfiltered external full-board fallback where applicable.
Packaged verification on the T-Hub tile showed **No registered Project**, zero iframes, and no Board URL input, correcting the reproduced redirect to `http://192.168.0.102:4000/`.
The registered and bound Project success state still requires the Phase 8 real Powder acceptance flow.
Run and Preview item 5 and the Preview portion of item 8 are implemented in source commit `96998fc` and packaged as installed `0.3.76` from `7ced938`.
The packaged T-Hub tile exposed exactly one combined tab and one unified panel containing both managed runner controls and the empty preview state.
The old inspection endpoint remained present in terminal scrollback, but the preview URL stayed empty with zero iframes and no detected-URL chips.
The typed-target and generation-safe lifecycle portions of items 6 and 7 are implemented in source commit `61f56ba` and packaged in installed `0.3.78`.
The packaged T-Hub root exposed exactly `dev`, `build`, `tauri`, and `typecheck`, started the real Vite target, detected `http://localhost:1420/`, and stopped the managed run and its Vite descendant.
Source commits `fbacc8f`, `16480b7`, and `19dc3c7`, packaged as installed `0.3.81` from `8f5fffa`, complete Windows reachability for the representative standard Tauri Vite target.
The installed package kept the Preview iframe on reachable `http://localhost:1420/`, returned Windows HTTP 200, and removed both listeners and the managed Vite PID on Stop.
Source commit `9d95fa9`, packaged as installed `0.3.82` from `ec55526`, owns the complete managed package process group through normal Stop, same-port restart, natural parent exit, TERM-resistant descendants, and forced application exit.
The installed package removed every pnpm, Vite, and esbuild process plus ports `1420` and `1421` during both normal Stop and forced application exit while preserving the seven unrelated tmux sessions.
Installed `0.3.82` passed the representative Next.js target, including Windows HTTP 200, expected rendered sentinels, exact-run Stop, and full npm and Next process-group cleanup.
Source commits `5011803` and `3177d81`, packaged as installed `0.3.84` from `0cd5861`, add and complete the representative package-less static target.
The installed static server bound only Windows `127.0.0.1`, published its authoritative URL without log parsing, auto-loaded and restored the iframe across tab remount, denied traversal, hidden, and symlink requests, removed the iframe and URL on Stop, restarted cleanly, and closed on forced application exit.
Source commit `1484750`, completed by `d05073d` and packaged as installed `0.3.86` from `5ea945c`, hardens concurrent Start and Stop ownership, authoritative rejected-start recovery, static path-race confinement, exact Host validation, bounded request handling, and bounded shutdown.
Packaged acceptance passed the static HTTP and confinement matrix, stale-run ownership, 206-millisecond nonreading-client Stop, independent cross-terminal Start, listener cleanup, and preservation of the six-session pre-install tmux baseline.
The Run and Preview exit-gate requirement for representative Vite, Next.js, and static projects is complete.
Framework-aware generic Vite arguments and stale WSL-address recovery remain open follow-up hardening.

### Tests and Evidence

- Add component tests for every empty, loading, degraded, error, and success state.
- Add cross-surface status tests that assert exact labels, tooltips, accessible text, freshness, and worktree identity.
- Add browser E2E for Board and Preview, including iframe fallback.
- Add History resume tests across Codex and Claude.
- Add auto-continue UI tests for opt-in, opt-out, cancellation, scheduled recovery, duplicate events, and failed exact-thread resume.
- Add accessibility checks and keyboard-only flows.
- Perform packaged pixel review at representative Windows scaling values.

### Exit Gate

- Board opens the correct Project board without manual URL configuration.
- Run and Preview starts and stops representative Vite, Next.js, and static projects.
- Files and Captain creation use the same canonical WSL path semantics.
- History resumes Codex and Claude sessions accurately.
- Codex and Claude auto-continue state is visible, controllable, and bound to the correct session.
- Hidden surfaces produce no sustained CPU activity.

## Phase 10 - Cortana Operations, Context, Voice, and Notifications

### Goal

Give Cortana lightweight operational awareness and make attention cues provider-independent.

### Work

1. Add `fleet_health`, `captain_health`, `context_status`, `resource_summary`, and `list_owned_resources` operations.
2. Add `navigate_to_captain`, `recover_captain`, `checkpoint_captain`, and `retire_captain` operations.
3. Generate threshold events in T-Hub rather than making Cortana continuously poll with model tokens.
4. Derive liveness from terminals, Harness processes, and lifecycle events rather than Captain self-report alone.
5. Generate context-reset recommendations after safe turn boundaries and meaningful Assignment milestones.
6. Require a durable checkpoint and unresolved-decision review before reset recommendations.
7. Preserve Captain identity, Workspaces, and Crew across context resets.
8. Feed Codex and Claude attention states into the same chime, desktop-notification, and voice paths.
9. Separate controls for needs-input, completion, failure, recovery, and retirement cues.
10. Preserve Scribe talk-over protection and voice-engine fallback visibility.
11. Attribute cues to the correct Cortana, Captain, or Crew identity.
12. Consider per-Captain chime or voice identity only after the common cue path is reliable.

### Tests and Evidence

- Test threshold generation without any model process running.
- Verify that idle, empty, or high-context Captains are not retired automatically.
- Verify that reset recommendations do not appear while unsafe work or unanswered decisions remain.
- Test voice and notification transitions for both Harnesses.
- Test Scribe hold and delayed delivery behavior.
- Test TTS failure, fallback, recovery, and user-disabled states.

### Exit Gate

- Cortana can inspect and recover the fleet without implementation-level control over Crew.
- Context recommendations are useful, safe, and provider-independent.
- Codex and Claude produce equivalent user-facing attention cues for equivalent states.
- Voice failures are visible rather than silent.

## Phase 11 - Measured Runtime Efficiency

**Status:** Active; matching bridge deployment and graphical `GitInfo` routing acceptance are complete, while recurring host-bridge process churn still makes the declared one-terminal packaged baseline ineligible.
The first isolated attempt was invalidated when Windows Explorer launched a separate normal T-Hub process.
The documented retry pinned installed `0.3.90` PID `49712` and completed 55 samples over 61.05 seconds with one visible idle shell tile, but the host bridge produced eight births and eight deaths across four incomplete CPU intervals.
The artifact therefore reports `release_acceptance_eligible: false` and is diagnostic evidence rather than an accepted baseline.
The 29.94-second recurrence matched the visible tile's then-active 30-second full Git-header poll, whose Windows fallback created a new WSL process tree on every cache miss.
Source commits `5ced6c2` and `bd0d8dd` add the `GitInfo` protocol operation, route full snapshots through the persistent agent, cap the agent collector below the desktop request timeout, distinguish disconnected, unsupported, and command-failure outcomes, and add a real stdio round-trip test against the matching agent.
The one-shot Windows fallback remains only for a disconnected bridge or an explicitly unsupported older agent; an agent command failure returns degraded Git state without starting competing fallback work.
Commit `8dd94c9` emits one successful agent-source marker per process and keeps every exceptional route visible for packaged acceptance.
Commits `a9b7082` and `42de985` also remove full-suite attach-test interference by quiescing the churn workload and restoring process-global agent integration-test environment before deleting its fixtures.
Commit `d73f9cb` versions desktop `0.3.93`, with agent and protocol `0.5.1` from `f821957`; exact source `e95eb56` was then built and installed for packaged acceptance.
Installed `0.3.93` with agent `0.5.1` passed the graphical same-cwd Git-header proof, emitted one `git_info source=agent` marker, and emitted no fallback or agent-error marker in the proof window.
The isolated packaged retry at PID `20132` completed 55 samples over 60.96 seconds with one visible idle shell tile and the same exclusive agent route, but recurring host-bridge triplets produced 12 births, 15 deaths, and seven incomplete intervals.
Artifact `artifacts/perf/t-hub-0.3.93-1t-20260715T0204-r2.json` therefore remains diagnostic and reports `release_acceptance_eligible: false`.
Read-only descendant tracing attributed the residual periodic host-bridge lane to terminal reconciliation, which collected tmux sessions and pane metadata through recurring Windows-to-WSL subprocesses.
Source commit `3816bf4` adds the additive `TerminalSnapshot` protocol operation and routes normal `list_terminals` reconciliation through the persistent WSL agent.
The compatibility scan is limited to one bootstrap attempt for a disconnected or explicitly unsupported agent, is never resumed after agent success, and does not run after a timeout or agent command failure.
The agent collector bounds each of its two sequential steps to four seconds, drains both output pipes concurrently, and kills and reaps the collector process group on timeout.
The uninstalled source is desktop `0.3.94` with agent and protocol `0.5.2`.
Its source gate passed 471 frontend tests, TypeScript and the production frontend build, 641 desktop Rust tests with one ignored, all Rust workspace and MCP end-to-end suites, warning-denied Clippy, formatting, diff checks, focused inherited-pipe and large-output timeout regressions, and the performance harness self-tests.
Installed `0.3.93` with agent `0.5.1` remains unchanged and cannot prove the new capability.
Phase 11 remains blocked pending exact matching `0.3.94` and agent `0.5.2` deployment, graphical `terminal_snapshot source=agent` evidence with no fallback or error marker, and an eligible repeated packaged one-terminal artifact.
The 4, 8, and 16 terminal scenarios must not run until the one-terminal gate is eligible.

### Goal

Reduce steady CPU, memory, process, and startup cost using packaged measurements rather than intuition.

### Work

1. Capture clean packaged baselines with 1, 4, 8, and 16 declared sessions.
2. Include hot, warm, cold, Board, Preview, Captain, Crew, browser, inbox, and voice scenarios.
3. Attribute WebView2 CPU to renderer work, GPU work, xterm, animation, polling, or repaint scheduling.
4. Stop unnecessary animation frames, canvas redraws, cursor work, and layout measurement on hidden or unchanged surfaces.
5. Skip Powder event polling when no active Captain can receive events.
6. Cache Powder profiles, credentials, clients, and HTTP connection pools with explicit refresh behavior.
7. Enable binary PTY framing with a tested version fallback.
8. Remove the live JSON and base64 terminal-output path.
9. Coalesce terminal, focus, Git, History, usage, resource, and pane scans.
10. Pause low-priority polling for hidden windows, cold terminals, inactive panels, and disabled features.
11. Reduce watchdog cadence when event-driven diagnostics can prove health.
12. Lazy-load and prune icon resolvers by selected theme.
13. Reduce the main and icon JavaScript chunks.
14. Measure process birth and death, handles, threads, sockets, relays, and memory recovery.
15. Run a twenty-four-hour packaged soak.

### Tests and Evidence

- Keep all scenarios scripted and record exact source, installed hash, PID, terminal count, and interval completeness.
- Reject performance conclusions from runs with unexplained process churn or incomplete CPU intervals.
- Measure input latency and cold restoration, not only memory.
- Compare before and after artifacts for each optimization.

### Exit Gate

- Hidden and cold terminals create no sustained rendering CPU.
- Closing resources returns process and memory counts toward baseline.
- The 1, 4, 8, and 16 session matrix meets documented budgets.
- The soak shows no growing process, handle, socket, log, queue, or memory trend.

## Phase 12 - Security, Release, Documentation, and Production Acceptance

### Goal

Make the validated product safe, traceable, installable, and understandable.

### Work

1. Document the expected Windows-to-WSL Tailscale route and the nonessential WSL self-hairpin limitation.
2. Resolve repeated Tailscale DNS or duplicate-bind warnings that affect supportability.
3. Complete Tauri Content Security Policy hardening for app, Board, and Preview surfaces.
4. Add Authenticode signing for the executable and installer.
5. Add dependency, secret, vulnerability, and license scanning.
6. Complete strict branch protection and required status checks.
7. Keep external workflow actions pinned to immutable revisions.
8. Add packaged Windows, WSL, tmux, Codex, Claude, Powder, messaging, Board, Preview, voice, and cleanup E2E coverage.
9. Validate installer upgrade, rollback, state migration, and uninstall behavior.
10. Verify protected Powder permissions and credential redaction on every path.
11. Produce an SBOM and retain source commit, build identity, installer hash, and installed-binary hash.
12. Update user documentation for all settled product vocabulary and workflows.
13. Mark historical design documents superseded only with explicit approval.
14. Preserve Lavish and deck artifacts as instructed by the General.
15. Update the zero-context handoff with exact source, runtime state, tests, measurements, and remaining risks.
16. Keep `docs/REVIEW-INDEX.md` current so historical and archived reviews cannot silently become active backlog.
17. Bump the desktop version for every changed packaged build and reject reuse of a version already tagged to different source.
18. Build and install the signed production artifact from the exact reviewed commit.
19. Push and publish only when the General requests it.

### Tests and Evidence

- Run the entire automated quality gate on the exact release source.
- Run final interactive Captain and Crew acceptance on the installed Windows build.
- Verify version, PID, executable hash, sessions, Powder, Tailscale, Board, Preview, History, messaging, voice, resources, and cleanup.
- Audit the working tree for generated files, secrets, and preserved user artifacts.

### Exit Gate

- No Critical, High, or unresolved Medium finding remains under the documented threat model.
- The signed installer upgrades without losing identities, sessions, Workspaces, History, inbox state, or protected bindings.
- Documentation matches visible behavior and terminology.
- The installed version and binary hash match the release artifact.
- The handoff names the next action without relying on conversation history.

## Parallel Implementation Map

Parallel work should use isolated worktrees, explicit file ownership, separate Powder cards, and one integration owner.
Parallel lanes must not edit the same registry schema, migration, or core lifecycle file without an agreed boundary.

### Tranche A - Immediate Foundation

These lanes may proceed in parallel:

- **A1 Terminal correctness:** xterm race reproduction, lifecycle fixes, and packaged tests.
- **A2 CLI reliability:** upgrade `th`, fix restart recovery, add timeout tests, preserve protocol compatibility, and do not expand the command catalog yet.
- **A3 Resource schema design:** specify ownership records and reconciliation without enabling cleanup yet.
- **A4 Provider event research and fixtures:** capture Codex and Claude lifecycle fixtures without changing the live reducer.
- **A5 Documentation and terminology:** keep canonical definitions synchronized without changing historical artifacts.

Integration order is A1 and A2 first, followed by the safe activation of A3.
A3 must implement worktree ownership and safety from `docs/WORKTREE-STATUS-CONTRACT.md` rather than introducing a resource-only approximation.
A3 may land Git-only suspension and safety scaffolding before B1, but it must consume B1 for Captain, Assignment, Workspace, and Crew ownership rather than creating a parallel identity model.

### Tranche B - Identity, Providers, and Control

These lanes may proceed in parallel after the Phase 1 control contract is stable:

- **B1 Cortana and multi-Captain registry:** identity schemas, migrations, Assignment records, and retirement state.
- **B2 Workspace model:** Captain-to-Workspace control and Crew membership.
- **B3 Codex adapter:** hooks, interactive telemetry, context, History, and permission launch behavior.
- **B4 Claude adapter normalization:** move existing hooks and status telemetry behind the shared contract.
- **B5 CLI contract and shared catalog:** first normalize the existing CLI to `docs/cli-contract.md`, then add shared schemas, role filtering, command groups, and CLI-to-MCP parity tests.
- **B6 Inbox identity and UI data model:** durable recipients, message states, retention, and read APIs.

B1 owns shared identity migrations.
B3 and B4 must consume B1's identity interfaces rather than each introducing provider-specific durable fields.
B3 and B4 must emit the two independent status axes defined in `docs/STATUS-MODEL.md`.
B5 owns command definitions.
B5 must land contract behavior and process-level tests before broad command generation so new commands inherit the correct interface.
B6 owns messaging schema and must not bypass B1 authority.

### Tranche C - Product Flows

These lanes may proceed in parallel after the identity and adapter contracts stabilize:

- **C1 Codebase picker and preflight UI.**
- **C2 New-codebase and rollback transaction.**
- **C3 Board binding and authentication states.**
- **C4 Run and Preview lifecycle.**
- **C5 History and Provider limits UI.**
- **C6 Messages and Resources UI.**
- **C7 Cortana health and recovery commands.**
- **C8 Voice and notification parity.**

Each lane must use shared Project, identity, resource, and adapter APIs.
No lane may create a local substitute for a missing backend contract.

### Tranche D - Acceptance and Hardening

These lanes may proceed in parallel after real Powder acceptance passes:

- **D1 Performance matrix and soak automation.**
- **D2 Security and credential audit.**
- **D3 Packaged cross-Harness E2E.**
- **D4 Accessibility and visual quality.**
- **D5 Installer, signing, update, and rollback.**
- **D6 Documentation, handoff, and release evidence.**

Release integration waits for every Phase 12 gate.

## Testing Doctrine

1. Reproduce user-visible bugs in the packaged product before fixing them.
2. Add a failing automated regression at the closest reliable layer.
3. Test pure state transitions with unit tests.
4. Test adapter and protocol contracts with fixtures from real Harness output.
5. Test registry, authorization, migration, and rollback through Rust integration tests.
6. Test UI state, accessibility, and error presentation through component tests.
7. Test complete user workflows through packaged Windows E2E.
8. Use real Powder only for final acceptance while retaining deterministic mock failure tests.
9. Test every mutation at success, explicit rejection, timeout, crash, retry, and rollback boundaries.
10. Run format, lint, warnings-denied compilation, frontend tests, Rust workspace tests, TypeScript, and production builds before each logical commit.
11. Record interactive checks that cannot yet be automated and convert stable checks into automation later.
12. Do not declare provider parity based only on both terminals launching.
13. Test authoritative, derived, stale, unknown, and conflicting state explicitly rather than collapsing uncertainty into a healthy default.

## Claude and Codex Parity Matrix

The matrix describes current T-Hub support, not the provider's theoretical capabilities.

| Capability | Claude Code today | Codex today | Required outcome |
| --- | --- | --- | --- |
| Interactive launch | Supported | Supported | Apply explicit unrestricted defaults and identity labels to both |
| Interactive unrestricted permissions | Inherited or flag-dependent | Inherited or flag-dependent | Apply and display the effective bypass mode consistently |
| Provider session identity | Strong through `SessionStart` hooks | Partial for interactive sessions | Bind both to durable T-Hub identities |
| Turn lifecycle | Strong hook coverage | Headless tap plus weak interactive inference | Normalize structured interactive events |
| Needs-question detection | `Elicitation` and filtered notifications | No complete T-Hub bridge | Derive from Codex hooks or app-server events |
| Permission-request detection | Hooked | Provider hook exists but is not integrated | Feed both into one attention path |
| Completion detection | `Stop` hook | Provider `Stop` hook exists but is not integrated | Feed both into one reducer |
| Failure detection | `StopFailure` and session-end evidence | No exact `StopFailure` hook | Derive from turn events, process result, and structured errors |
| Context telemetry | Structured status-line bridge | Native footer only for the user | Add structured Codex context telemetry |
| Provider limits | Supported through status line and fallback | Account usage strip exists | Normalize global quota display |
| Subagent supervision | Hooked | Provider hooks exist but are not integrated | Normalize start and stop events |
| Task lifecycle | Claude-specific task hooks | No direct equivalent confirmed | Mark capability and derive only when reliable |
| Worktree lifecycle | Claude-specific hooks | No direct equivalent confirmed | Use T-Hub-owned worktree operations as the common authority |
| Directory changes | Claude `CwdChanged` hook | No direct equivalent confirmed | Use terminal or T-Hub process evidence where necessary |
| Compaction lifecycle | Not currently integrated by T-Hub | Codex has pre and post compact hooks | Add normalized context compaction events where available |
| Tool lifecycle | Not currently part of T-Hub supervision | Codex has pre and post tool hooks | Keep optional and avoid noisy default UI |
| History and resume | Claude-only Recent implementation | No unified History | Build adapter-backed History for both |
| Context meter in tiles | Claude-only | Missing | Make provider-independent |
| Auto-continue after provider limit | Implemented through the Claude-specific flow | Missing | Build durable exact-thread Codex scheduling, cancellation, deduplication, and recovery |
| Voice attention announcements | Works when Claude status transitions arrive | Usually absent because interactive status is weak | Drive voice from normalized events |
| Chimes and OS notifications | Stronger through Claude events | Degraded | Drive both from normalized events |
| Hook installation UI | Claude-only | Missing | Replace with Agent integrations |
| Hook trust model | Claude settings merge | Codex requires explicit hook review and hash trust | Surface provider-specific trust without hiding it |
| Native agent voice input | Enabled in the current Claude configuration | No equivalent T-Hub-managed Codex setting | Prefer provider-agnostic Scribe input rather than require native parity |
| Provider-native notifications | Claude notification hooks feed T-Hub | Codex TUI notifications exist outside T-Hub | Normalize important events inside T-Hub and leave native notifications optional |
| Provider plugins and marketplaces | Claude plugins and marketplaces are configured separately | Codex plugins use a different configuration system | Show integration health without trying to force one provider's plugin model onto another |
| MCP provisioning | Installed and tested | Installed and tested | Prefer CLI-first shared operations and role-filter MCP |
| CLI control | Available but incomplete | Available but incomplete | Make Harness-independent and canonical |
| Model and reasoning display | Partial | Configured externally | Display effective model and reasoning without requiring selection each launch |
| Harness switching | Not a durable identity operation | Not a durable identity operation | Preserve T-Hub identity across reviewed runtime replacement |

## Intentional Provider Differences

Provider parity means equivalent T-Hub outcomes, not identical provider internals.
Claude may continue to provide unique task, notification, worktree, and directory hooks.
Codex may continue to provide unique compaction and tool lifecycle hooks.
T-Hub should expose optional detail where useful while keeping the common Captain and Crew workflow consistent.
Unsupported events must be labeled as unavailable or derived rather than fabricated.

## Outstanding Considerations and Recommended Defaults

### Same-User Isolation

The current application boundary does not protect against a malicious process running as the same WSL user.
Strong isolation requires separate OS users, containers, or a broker that keeps tmux and credentials outside agent-readable state.
Recommended initial decision: document the same-user trust boundary and defer hard isolation until the core workflow passes acceptance.

### Powder Board Cardinality

A Project may eventually need more than one Powder board.
Recommended schema: support one default board plus optional Assignment-specific bindings without forcing the UI to expose multiple boards initially.

### Multi-Captain Git Coordination

Multiple Captains in one Project increase branch, worktree, shared-file, and landing conflicts.
Recommended policy: every Crew member owns one validated worktree, every Captain Assignment has a branch namespace, Powder claims carry work ownership, and overlapping Captains coordinate through visible messages.

### Cross-Project Captain Messaging

Captains may need expertise from another Project.
Recommended policy: allow explicitly addressed cross-Project messages, label them clearly, grant no file or terminal access, and require explicit work transfer before implementation ownership changes.

### Offline and Partial Failure

T-Hub, Powder, Tailscale, the Harness, and the model Provider can fail independently.
Recommended policy: preserve read and recovery functions offline, fail authority-dependent mutations safely, and show which subsystem is unavailable.

### Secrets and Retention

Inbox bodies, terminal captures, History, logs, and Powder references can contain sensitive material.
Recommended policy: redact known secret shapes, avoid implicit body logging, use bounded local retention, and provide explicit deletion and pinning.

### Provider Limits and Auto-Continue

Provider limit behavior differs across services and can change.
Expose provider limits globally and keep context per conversation.
Implement auto-continue as a normalized adapter capability with an explicit per-session setting.
Codex auto-continue must persist the exact thread ID, intended continuation, earliest reset time, owning T-Hub identity, cancellation state, and idempotency key.
If Codex cannot resume safely, T-Hub must retain the pending recovery visibly rather than sending input to an uncertain shell or conversation.

### Model and Harness Switching

A runtime switch can strand a provider conversation or introduce incompatible identifiers.
Recommended policy: require a checkpoint, stop the old runtime, start the replacement, bind the new conversation, and retain the old conversation in History.

### Resource Budget

The six-concurrent-Crew idea is an initial operational default rather than a proven hardware limit.
Recommended policy: do not enforce a hard limit until packaged 1, 4, 8, and 16-session measurements establish warning and queue thresholds.

## Outstanding Questions

No product question blocks Phase 1 or Phase 2.
The following questions can be resolved before their dependent phases:

1. Should the first UI expose Assignment-specific Powder boards, or support them only in the schema and Advanced settings?
2. Should message-body retention default to thirty days, or should the General choose a different local retention period?
3. Does the General want hard same-user isolation before public distribution, or is the documented local trust boundary acceptable for the first production release?
4. Which GLM Harness or OpenAI-compatible runner should become the third adapter after Codex and Claude parity is complete?
5. Should completion voice announcements remain opt-in and separate from needs-input speech?
6. Should Codex interactive telemetry combine lifecycle hooks with app-server or structured turn events for states the hooks cannot prove?
7. Should provider-specific capabilities appear in an Advanced detail view while the normal UI presents the shared workflow?

Recommended answers are already recorded above so implementation need not pause unless the General wants different policy.
The recommended Codex telemetry answer is to use hooks for lifecycle boundaries and a structured Codex event source for context, failures, and any missing turn detail.
The recommended UI answer is to preserve provider-specific detail in Agent integrations while keeping the normal Captain and Crew workflow common.

## Zero-Context Resume Checklist

1. Load the active workspace `AGENTS.md` instructions supplied to the session and read this document.
2. Read `docs/CAPTAIN-POWDER-HANDOFF.md`, `docs/ORCHESTRATOR-OPERATING-MODEL.md`, `skills/captain/SKILL.md`, `docs/POWDER-INTEGRATION.md`, and `docs/PERFORMANCE-BENCHMARK.md` for the active phase.
3. Run `git status --short --branch` and preserve `.lavish/` plus `docs/DECK-AGENTS-DESIGN.md`.
4. Run `git log --oneline -12` and inspect work after this plan.
5. Confirm the installed Windows PID, executable path, version, and hash rather than assuming source is deployed.
6. Confirm the active phase and its dependencies.
7. Reproduce the relevant user-visible behavior before editing a bug fix.
8. Use an isolated worktree and Powder card for parallel implementation.
9. Run the phase-specific tests and global quality gates.
10. Commit the verified logical change with a clear message and no automatic co-author line.
11. Update this plan only when product decisions, dependencies, or phase status materially change.
