# Captain, Crew, and Powder Integration Handoff

**Date:** 2026-07-13.
**Repository:** `/home/natkins/projects/tools/t-hub/t-hub-app`.
**Local branch:** `main` at `85eb54d`.
**Status:** The implementation, first review fixes, second review fixes, and local verification are complete.
The Windows T-Hub application has not been rebuilt or installed and the commits have not been pushed.

## Reset Resume Point

The canonical role is now Captain.
Shipmate remains only as a compatibility alias that loads the Captain skill.
Codex and Claude use the same repository-managed Captain protocol.

T-Hub now persists projects, Captain assignments, harness identity, Captain and Crew conversation checkpoints, Crew metadata, Powder mappings, Powder claims, and Powder event cursors in the versioned Captain registry.
This state is designed to reconstruct a Captain after conversation replacement, context reset, application restart, or terminal relocation.

Powder remains authoritative for cards, runs, claims, work logs, input requests, completion evidence, and its event stream.
T-Hub remains authoritative for project registration, ship identity, terminal identity, harness selection, checkout paths, Crew liveness, and card/run-to-terminal mappings.
No Powder source files were changed.

## Commits

The implementation and review fixes are the contiguous range `15840dd^..85eb54d`.

1. `15840dd feat(skills): rename shipmate protocol to captain`
2. `baf18eb feat(captain): persist project and Powder context`
3. `fc3504e feat(projects): add durable registration and Powder mapping`
4. `cfbf04c feat(captain): commission project-aware captains`
5. `cc7b97d feat(powder): bind Crew lifecycle to claims`
6. `253b705 feat(captain): add project commissioning flow`
7. `721e97d refactor(protocol): derive default priority`
8. `400db3b feat(powder): consume durable event tail`
9. `4d0e378 feat(powder): reconcile events into captain inbox`
10. `4776194 ci: gate main and releases on quality checks`
11. `b6de34e fix(captain): verify coding harness liveness`
12. `7cff5de fix(captain): fail closed on registry persistence`
13. `375f0d2 fix(captain): retain incomplete Crew rollbacks`
14. `545ea4d fix(captain): preserve Powder events across rebinds`
15. `049706d docs(captain): route staffing through Crew dispatch`
16. `fc691f6 feat(captain): persist conversation checkpoints`
17. `2bfaf34 fix(powder): require secure authorized preflight`
18. `b45b01e feat(captain): install skills for both harnesses`
19. `9bc2ae2 ci: enforce MSRV and warning-free Clippy`
20. `fa70f1d fix(captain): detect runtime-wrapped harnesses`
21. `96a9314 fix(captain): retain failed Crew cleanup state`
22. `82f25c4 fix(captain): preflight dual-harness installs`
23. `5441c0e fix(captain): restore skill installer executable`
24. `85eb54d fix(captain): preserve repeated Crew cleanup retries`

## What Was Implemented

### Canonical Captain Skill

- `skills/captain/SKILL.md` is the canonical reset-safe protocol.
- `skills/shipmate/SKILL.md` is a thin compatibility alias.
- `~/.codex/skills/captain` and `~/.claude/skills/captain` point to the canonical repository skill.
- Both harnesses now resolve their Shipmate alias to the repository alias.
- The former Claude Shipmate directory was archived at `~/.t-hub/legacy-skills/shipmate-pre-captain-2026-07-13`.
- Recovery starts from durable T-Hub and Powder state instead of conversation memory or terminal location.

### Durable Project and Captain Identity

- `captains.json` schema version 4 stores registered projects, conversation checkpoints, and Powder event cursors.
- Project registration canonicalizes the Git main worktree and prevents split identities for one checkout.
- A project stores its canonical repository root, display name, remote metadata, default branch, Powder repository, and protected connection-profile name.
- A Captain stores its project ID, assignment, harness, conversation continuity, reset source, and Crew roster.
- `captain_bootstrap` reconstructs the project, assignment, ship, harness, Crew, Powder mapping, and recovery instructions.

### Captain Commissioning

- The New Terminal menu now opens a project-aware Captain commissioning dialog.
- The user can select a registered project or register an existing Git repository.
- The user chooses Codex or Claude, provides the Captain assignment, and confirms the Powder repository and connection profile.
- Commissioning checks Powder health before starting the Captain process.
- Commissioning prevents duplicate active Captains for one project.
- Spawn, ship claim, project binding, and harness startup have rollback behavior and request idempotency.
- The dialog was visually checked at 1280 by 720 and 390 by 844 without overflow or overlap.

### Crew and Powder Lifecycle

- `dispatch_crew` validates the project checkout and Powder card repository before creating work.
- T-Hub starts a bare read-capability terminal before claiming the card.
- T-Hub persists the authoritative card ID and run ID before launching Codex or Claude.
- Failures roll back the terminal, claim, and uncommitted Crew record only after terminal stop and Powder release are both confirmed.
- A failed Powder release leaves a durable `cleanupPending` Crew record with its card and run binding intact.
- The lease reconciler renews claims only when tmux proves that the Crew terminal is alive.
- The reconciler releases claims and marks Crew removed only when tmux proves that the terminal is gone.
- Ambiguous terminal liveness causes no Powder mutation.
- Closing a Crew terminal attempts to release its Powder claim after stopping the process.
- Powder credentials are resolved from a protected profile and are never stored in `captains.json`.

### Powder Event Synchronization

- T-Hub consumes Powder's read-authorized `/api/v1/events/tail` SSE endpoint.
- The SSE parser validates `powder.card_event.v1` and strictly increasing sequence IDs.
- New or changed Powder bindings snapshot the current event head to avoid replaying historical events into a fresh Captain inbox.
- Idempotent rebinding preserves the existing cursor.
- Events are filtered by the project's canonical Powder repository.
- Events from other repositories advance the global stream cursor without notifying the Captain.
- Relevant events enter the active Captain's durable T-Hub inbox before the cursor advances.
- An inbox overflow or stream failure prevents advancement past an undelivered relevant event.
- A crash between inbox persistence and cursor persistence can redeliver one event, so Powder event IDs are explicit idempotency keys.
- Event names and change payloads are treated generically, including the currently under-documented `work-log-appended` event.

### CI and Release Gating

- The quality workflow now runs for pull requests, pushes to `main`, manual dispatch, and as a reusable release gate.
- Rust formatting, Rust workspace tests, Clippy, TypeScript, Vitest, and the production frontend bundle are checked.
- Windows installer signing cannot begin until the reusable quality jobs pass on the exact release commit.
- CI validates the declared Rust 1.89 MSRV with the locked workspace.
- Full-workspace Clippy runs with `-D warnings`.

### Local Harness Installation

- `scripts/captain/install-thub-codex.sh` installs the MCP binary and the Captain and Shipmate skills for both Codex and Claude.
- Skill conflicts are checked before the MCP binary or Codex registration changes.
- The four skill destinations are committed as one rollback-capable transaction.
- The current review-fix build has not been installed.
- New Codex and Claude sessions are required after installation so their cached MCP and skill catalogs reload.

## Verification Evidence

- `cargo test --workspace`: 664 passed, 0 failed, 1 ignored.
- `cargo +1.89.0 check --workspace --locked`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `pnpm typecheck`: passed.
- `pnpm --filter t-hub-desktop test`: 400 passed across 40 files.
- `pnpm build`: passed.
- Vite still reports existing large-chunk and mixed static/dynamic import warnings.
- Browser verification showed meaningful content, no Vite error overlay, and no dialog overflow at desktop and mobile viewports.

## External State and Blockers

### T-Hub Runtime Is Still Old

The installed Windows T-Hub application does not contain these backend changes yet.
Restarting the existing installed application alone will not activate the new control commands or Powder reconciler.
The local commits must be pushed, a Windows build must pass the new quality gate, and that build must be installed before runtime testing.

### Powder Runtime Is Not Configured

`~/.t-hub/powder-profiles.json` does not exist.
No sanctioned `POWDER_API_KEY`, `POWDER_API_KEY_CMD`, `op`, macOS `security`, or local `powder` executable was available in the current WSL environment.
The Powder repository's remote doctor currently expects `https://sanctum.tail5f5eb4.ts.net:10001`, but the endpoint must be confirmed instead of assumed.
An existing agent-scoped Powder credential source must be provided without changing Powder or using an admin key.

A profile should have mode `0600` and this shape:

```json
{
  "schemaVersion": 1,
  "profiles": {
    "production": {
      "baseUrl": "https://confirmed-powder-endpoint",
      "agentName": "matching-agent-key-name",
      "apiKeyCommand": "command that prints the existing agent-scoped key"
    }
  }
}
```

Do not put the raw key in this handoff or in repository files.
The configured `agentName` must match the Powder agent key identity because T-Hub intentionally does not use an admin key to impersonate terminal IDs.

### Remote CI Has Not Run

The commits are local and have not been pushed.
The reusable workflow and release dependency therefore have not yet been evaluated by GitHub Actions.
Workflow YAML parses locally, but GitHub must validate the reusable workflow call on the pushed commit.

## Independent Review

An independent review of `15840dd^..4776194` completed without modifying files.
The reviewer found seven high-severity blockers and four medium-severity findings.
All eleven first-review findings were addressed and verified in focused tests.

### First-Review High-Severity Findings - Resolved

1. Relevant Powder events are skipped permanently when no active Captain terminal exists.
`apply_powder_events` advances the cursor even when it cannot enqueue to a recipient, and terminal-keyed inbox state is not migrated when a ship is re-adopted.
See `apps/desktop/src-tauri/src/control.rs:6494`, `apps/desktop/src-tauri/src/control.rs:6543`, `apps/desktop/src-tauri/src/control.rs:6589`, and `apps/desktop/src-tauri/src/control.rs:1860`.
Use a durable ship-keyed notification target, or keep the cursor stationary until a durable recipient exists and migrate pending delivery during rebind.

2. Tmux session liveness is incorrectly treated as harness liveness.
Codex and Claude startup commands return to a persistent shell after the harness exits, so a dead agent can block recommissioning and retain a Powder lease indefinitely.
See `apps/desktop/src-tauri/src/commands.rs:294`, `apps/desktop/src-tauri/src/commands.rs:322`, `apps/desktop/src-tauri/src/control.rs:5779`, and `apps/desktop/src-tauri/src/control.rs:6430`.
Track the harness process or provider lifecycle and require both terminal and harness liveness for Captain deduplication and lease renewal.

3. Transactional operations report durable success when `captains.json` persistence fails.
Registry serialization, write, and rename failures are logged and swallowed while project, Captain, Crew, claim, and cursor mutations still return success.
See `apps/desktop/src-tauri/src/control.rs:1283`, `apps/desktop/src-tauri/src/control.rs:1333`, `apps/desktop/src-tauri/src/control.rs:1351`, `apps/desktop/src-tauri/src/control.rs:1451`, `apps/desktop/src-tauri/src/control.rs:1526`, and `apps/desktop/src-tauri/src/control.rs:1565`.
Make persistence fallible for transactional Captain and Powder mutations and acknowledge success only after the durable write commits.

4. The canonical Captain skill bypasses the new Powder transaction.
The staffing procedure still directs agents through `create_worktree` with `startupCommand: "codex"` instead of `dispatch_crew`, and it has no equivalent Claude Crew flow.
See `skills/captain/SKILL.md:86`.
Rewrite staffing around `captain_bootstrap` and `dispatch_crew`, with Codex and Claude represented as harness choices in the same lifecycle.

5. Reset-recovery fields cannot be maintained through any command.
Captain and Crew records define conversation continuity and a Captain resume point, but commissioning initializes them empty and no MCP command updates them.
See `apps/desktop/src-tauri/src/control.rs:868`, `apps/desktop/src-tauri/src/control.rs:1016`, `apps/desktop/src-tauri/src/control.rs:5894`, `apps/desktop/src-tauri/crates/t-hub-mcp/src/tools.rs:369`, `apps/desktop/src-tauri/crates/t-hub-mcp/src/tools.rs:406`, and `skills/captain/SKILL.md:168`.
Add an authenticated ship-scoped checkpoint command and provider continuity backfill.

6. Ambiguous Crew rollback can discard the only binding needed to release a claim.
The rollback path can ignore a failed terminal close and then remove the Crew binding, leaving a live terminal and Powder claim with no retryable local record.
See `apps/desktop/src-tauri/src/control.rs:6069`, `apps/desktop/src-tauri/src/control.rs:6081`, and `apps/desktop/src-tauri/src/control.rs:6092`.
Preserve an explicit rollback-pending record until terminal stop and claim release are independently confirmed.

7. Powder bearer credentials can be transmitted over plaintext HTTP.
The client accepts arbitrary `http://` endpoints and attaches its bearer token.
See `apps/desktop/src-tauri/src/powder.rs:154`.
Require HTTPS for production profiles and allow HTTP only for an explicit loopback development mode.

### First-Review Medium-Severity Findings - Resolved

1. Captain commissioning checks `/healthz` but does not prove that the configured credential can perform an authorized read.
Add an authorization probe before process creation.

2. The MCP installer does not install or link the Captain and Shipmate skills for both supported harnesses.
Make skill installation idempotent and reproducible instead of relying on machine-local symlinks.

3. CI does not test the declared Rust 1.77 MSRV, and full-workspace Clippy remains warning-tolerant.
Raise or validate the MSRV and establish a warning-clean Clippy baseline.

4. MCP schemas do not encode the backend requirement that `dispatch_crew` and `captain_bootstrap` receive a Captain session or ship address.
Add schema `anyOf` requirements for the supported addressing fields.

### Second Review

A second independent review covered the first-review fix range through `9bc2ae2`.
It confirmed eight first-review fixes and found three remaining regressions.

1. Real Codex sessions launch through Node and were still classified as dead.
`fa70f1d` now resolves the pane foreground process group and treats unfamiliar commands as indeterminate instead of dead.
A live tmux test covers the Node-wrapped Codex topology.
2. A failed rollback release marked the Crew terminal removed despite claiming the binding was retained.
`96a9314` introduces a durable `cleanupPending` state and keeps the Powder card/run binding available for retry.
An end-to-end tmux lifecycle test forces profile resolution failure and verifies the retained record.
3. A late unmanaged Claude skill conflict could leave Codex and the MCP binary partially updated.
`82f25c4` preflights every destination before MCP mutation and installs all four skill targets transactionally.
The isolated installer test verifies that a final-target conflict leaves the binary, registration, and every skill destination unchanged.

The second-review findings are resolved locally.

A final focused review found that a second failed manual cleanup attempt could change `cleanupPending` to `removed`.
`85eb54d` makes cleanup-pending retention automatic until Powder confirms release and extends the lifecycle test through two failed attempts.
The reviewer found no other blocker in runtime-wrapped Codex detection or the four-target skill transaction.

### Remaining Test Gaps

- No process-level test exits Codex or Claude while leaving the fallback shell alive and then checks Captain deduplication and lease renewal.
- No persistence-failure test covers commissioning or dispatch rollback.
- No dispatch end-to-end test exercises a real Powder server, authorization, claim, and rollback together.
- UI tests omit failure paths, cancellation during commissioning, focus handling, and duplicate submissions.
- CI has no Windows and WSL packaged end-to-end coverage, and the reusable workflow has not run remotely.

## Fresh-Context Procedure

1. Read this handoff, the repository `AGENTS.md`, `skills/captain/SKILL.md`, `docs/POWDER-INTEGRATION.md`, and `docs/PRODUCTION-READINESS.md`.
2. Run `git status --short` and confirm only `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` remain as pre-existing untracked user artifacts.
3. Run `git log --oneline -16` and confirm local `main` ends at `85eb54d` or a later commit.
4. Do not push, release, install, merge, or modify Powder without explicit user authorization.
5. Re-run the complete Rust and frontend gates after any additional change.
6. Confirm the Powder endpoint and obtain a sanctioned agent-scoped credential command.
7. Create the protected Powder profile and verify it with the new `powder_status` command after the reviewed T-Hub build is installed.
8. Push only when the user explicitly requests it.
9. Build and install the Windows application only after remote CI passes, high findings are closed, and the user explicitly authorizes installation.
10. Restart T-Hub after installation.
11. Start a new Codex or Claude session so its cached MCP and skill catalog reloads.
12. Run the runtime acceptance plan below before calling the integration complete.

## Runtime Acceptance Plan

### Project and Captain

1. Open New Terminal and select Captain.
2. Register an existing Git repository and bind its confirmed Powder repository and production profile.
3. Commission a Codex Captain with an explicit assignment.
4. Verify one visible Captain terminal appears in the intended T-Hub workspace.
5. Attempt to commission the same project again and verify T-Hub returns the existing Captain instead of spawning a duplicate.
6. Repeat on a disposable project with Claude to verify harness parity.

### Reset Recovery

1. Record the ship slug, project ID, Captain terminal ID, assignment, and Powder binding.
2. Reset or replace the Captain conversation without moving the terminal.
3. Call `captain_bootstrap` by terminal ID or ship slug before accepting work.
4. Verify the recovered project, assignment, harness, Crew roster, and Powder mapping exactly match the pre-reset state.
5. Restart T-Hub and repeat the bootstrap check.

### Crew Claim Lifecycle

1. Use a disposable ready Powder card whose repository matches the registered project.
2. Dispatch one Codex Crew member into a valid checkout or worktree.
3. Verify Powder shows the expected agent-scoped claim and run ID.
4. Verify T-Hub maps that card and run to the exact Crew terminal.
5. Verify a heartbeat or reconciler cycle extends the claim only while tmux proves the terminal alive.
6. Close the Crew terminal and verify the claim is released and the Crew record becomes removed.
7. Repeat with Claude Crew.

### Event Synchronization

1. Record the project's event cursor after binding.
2. Change a card in another Powder repository and verify no Captain wake-up occurs while the cursor advances.
3. Change a card in the bound repository and verify one durable Captain inbox message contains the event ID and card identity.
4. Restart T-Hub and verify the event is not replayed after its cursor was persisted.
5. Interrupt T-Hub between inbox persistence and cursor persistence in a disposable environment and verify any replay is recognizable by the same event ID.
6. Fill or constrain the Captain inbox in a disposable environment and verify the cursor does not advance past a failed relevant delivery.

### Failure and Security Cases

1. Use an unreachable Powder endpoint and verify Captain commissioning fails before a process starts.
2. Use a card from a different Powder repository and verify Crew dispatch is refused.
3. Make the Powder profile file group-readable and verify T-Hub refuses it on Unix.
4. Remove the API key source and verify no Powder-backed Crew is dispatched.
5. Force ambiguous tmux liveness and verify no claim is renewed or released.
6. Verify ordinary Codex and Claude terminal presets remain read-capability sessions.
7. Verify only project-aware Captain commissioning requests control capability.

## Remaining Production-Stability Work

The requested integration is implemented, but the project is not yet stable-release ready.
The current stop-ship items remain documented in `docs/PRODUCTION-READINESS.md`.
The highest priorities are packaged Windows and WSL end-to-end automation, Authenticode signing, dependency and secret scanning, webview CSP and devtools hardening, and strict branch protection.
The production frontend also needs intentional chunk splitting because the icon bundle is approximately 3.72 MB and the main application bundle is approximately 1.20 MB before compression.

## Decision Boundary

The next safe action is push and remote CI when explicitly authorized.
The next external dependency is a confirmed Powder endpoint plus a sanctioned agent-scoped credential source.
Deployment remains blocked until the Windows build passes remote CI and the Powder profile is configured.
After that, the deployment sequence is push, remote CI, Windows build, install, T-Hub restart, and new harness sessions, each requiring the user's explicit authorization where it is outward-facing or changes the installed application.
