# T-Hub De-Powder Migration Plan

## Status

This document is the decision-complete implementation plan for retiring Powder from T-Hub for now.

The implementation baseline is the independently reviewed integration commit `efd3271a4efcde2b801a4b07fa4316560b8d9d15`.

Do not reset the repository to the historical pre-Powder commit.

The historical commit `e0b2998455da8fc53c22ac402887fd4434364476` is a behavioral reference only.

Preserve every approved commit, active terminal, existing worktree, provider conversation, and durable Captain or Crew record while performing this migration.

Do not release or complete existing Powder claims as part of the migration.

Do not delete or mutate external Powder cards, runs, reviews, evidence, repositories, or credentials.

## Goal

T-Hub will be a durable agent-session supervisor rather than a task manager.

The primary workflow will be:

```text
Captain decides
  -> T-Hub starts an agent
  -> T-Hub records the Captain-to-agent relationship and assignment
  -> the agent works in the selected directory or worktree
  -> the agent records concise checkpoints
  -> the Captain reviews Git evidence
  -> the Captain advances or completes the work
```

Starting and supervising an agent must not require a board, card, claim, run, criterion, work log, Powder profile, Powder repository, Powder credential, or Powder server.

## Product Decisions

These decisions are locked for this migration.

- Powder is removed from all live T-Hub product paths in the next release.
- Powder is not replaced with a new task or card system inside T-Hub.
- T-Hub keeps a small local record of agent sessions and their human-readable checkpoints.
- The Captain decides when to start an agent and invokes T-Hub to host it.
- T-Hub does not invent assignments, create tasks, schedule work, or infer completion from terminal activity.
- Existing Powder data remains preserved as inert historical compatibility data.
- New T-Hub code must make no Powder network request during startup, dispatch, supervision, reconciliation, cleanup, or shutdown.
- Public product language changes from `Crew dispatch` to `Start agent`.
- Internal `CrewRef` naming may remain temporarily where renaming would add control-plane risk without changing the public model.
- Completion never automatically closes a terminal, deletes a worktree, merges, pushes, installs, publishes, or releases.
- Destructive, cross-Project, merge, push, install, publish, and release actions retain their existing higher-level authorization gates.

## Preserved Capabilities

The migration must preserve useful work added after the pre-Powder baseline.

- Durable Captain identity and restart recovery.
- Captain and agent parent-child relationships.
- Project registration and canonical repository identity.
- Workspaces, tabs, terminals, tmux recovery, and worktree support.
- Codex and Claude provider support.
- Provider conversation identifiers and resume points.
- Context and usage telemetry.
- Permission observation and launch attestation that does not depend on Powder.
- Agent status normalization.
- History and recent-session recovery.
- Audit events for T-Hub control operations.
- Bounded control responses, request identity, retry safety, and cleanup protections.
- Cortana as an optional supervisor above Captains.

## Minimal Durable Agent Record

Each agent record must contain only the information required to recover and supervise a durable coding session.

```text
agentSessionId
captainSessionId
projectId
assignment
directory
worktreePath, when detected
branch, when detected
workspaceTabId, when assigned
harness
provider
providerConversationId, when known
resumePoint, when present
runtimeState
workStage
createdAt
updatedAt
```

`runtimeState` describes observed process state.

Allowed runtime states are `starting`, `running`, `idle`, `needsPermission`, `exited`, and `unavailable`.

`workStage` describes the explicit human workflow state.

Allowed work stages are `assigned`, `working`, `needsInput`, `readyForReview`, `awaitingIntegration`, `complete`, and `stopped`.

Provider activity must never overwrite an explicit `needsInput`, `readyForReview`, `awaitingIntegration`, `complete`, or `stopped` work stage.

Terminal disappearance must never be interpreted as successful completion.

## Public Control Contract

### `start_agent`

Add one primary Captain operation.

```text
start_agent(
  captainSessionId,
  assignment,
  directory,
  harness?,
  name?,
  workspaceTabId?
)
```

`captainSessionId`, `assignment`, and `directory` are required.

`directory` must already exist and must resolve inside the Captain's registered Project repository or one of its Git worktrees.

The Captain may use the existing worktree operation before `start_agent` when isolation is required.

T-Hub detects and records the worktree and branch instead of requiring the caller to repeat them.

`harness` defaults to the Captain's harness and may be explicitly set to `codex` or `claude`.

Provider permission posture comes from established repository and Captain policy.

The caller cannot pass raw credentials, Powder identity, unrestricted endpoints, or arbitrary provider permission overrides.

The operation must persist the assignment and a `starting` agent record before launching the harness.

The assignment is the authoritative launch prompt and is delivered exactly once during the accepted launch attempt.

The success response must contain the agent session ID, Captain session ID, Project ID, directory, detected worktree and branch, harness, runtime state, work stage, and whether the assignment was delivered.

If launch fails, T-Hub records the failure honestly and cleans up only the newly created terminal when ownership is proven.

No external rollback is needed because agent start has no Powder side effect.

### Read and checkpoint operations

Provide or adapt the following bounded operations.

- `list_agents` returns cursor-paginated summaries for one Captain or Project.
- `get_agent` returns full detail for one authorized agent.
- `agent_checkpoint` appends a bounded human-readable progress or handoff summary.
- `agent_events` returns cursor-based lifecycle and checkpoint deltas.
- `captain_bootstrap` returns a compact recovery snapshot.

`list_agents` excludes removed agents by default.

Removed-agent history must be available through an explicit state filter and cursor pagination.

`captain_bootstrap` omits full assignment bodies by default.

`get_agent` is the authoritative way to retrieve a full assignment.

Bootstrap and list responses must include a stable digest and event cursor so callers can request deltas instead of polling full state.

Checkpoint text must be bounded, attributed, timestamped, and stored in T-Hub's local durable registry or event journal.

Checkpoint records are session history and must not grow into cards, criteria, dependency graphs, estimates, priorities, or a task board.

### Lifecycle authority

An agent may set its own stage to `working`, `needsInput`, or `readyForReview` through `agent_checkpoint`.

The owning Captain may set the agent to any non-destructive work stage, including `awaitingIntegration`, `complete`, or reopening it to `working`.

Only the owning Captain or a higher authorized supervisor may stop the agent session.

Agent completion must preserve the terminal and worktree until the existing landed-work and cleanup safety checks pass.

Agents cannot start other durable agents through this contract.

### Existing terminal operations

Keep terminal read, focus, message, key, status, safe close, and supervision-tree operations.

Keep worktree creation and guarded removal as separate operations.

Do not overload `start_agent` with worktree creation, merging, or cleanup.

## Powder Retirement Contract

Remove the following from the advertised MCP catalog, CLI help, UI, and active documentation.

- `dispatch_crew`.
- `list_powder_boards`.
- `bind_project_powder`.
- `project_board_snapshot`.
- Powder status and health operations.
- Powder heartbeat, evidence, work-log, review, and completion operations.
- Powder connection-profile and repository arguments during Project registration.
- Powder board and Board panel UI types.
- Powder fields in Captain commissioning.
- The `th powder` command family.

For one compatibility release, the control dispatcher must recognize retired Powder operation names and return a structured `powder_retired` error.

The error must recommend the relevant agent operation and must perform no network request or local mutation.

The CLI may retain a hidden `th powder` tombstone for that release.

The tombstone must return the stable JSON error envelope, `error.kind` equal to `powder_retired`, and process exit code 4.

Removed Powder tools must disappear from the MCP catalog immediately so models stop selecting them.

Unknown Powder fields supplied to new Project or agent APIs must be rejected rather than silently ignored.

## Registry Migration

Create a timestamped backup of `~/.t-hub/captains.json` before the first write using the new schema.

The new reader must load every existing registry schema through version 17.

Move the serialized Powder structures needed for compatibility into a non-networking legacy module.

Continue deserializing the existing `powder`, `powderWork`, pending claim, pending release, mutation-intent, lifecycle, and completion fields.

Preserve those fields when rewriting a migrated record so historical evidence is not silently destroyed.

No runtime path may interpret those fields as current authorization or contact Powder to reconcile them.

On migration, a legacy Powder-bound Crew with a live terminal becomes an ordinary active agent.

A legacy Crew with a missing terminal becomes stopped or removed according to durable terminal evidence.

`cleanupPending` that existed only because a Powder release was uncertain must not block agent supervision or starting replacement work.

The unresolved external record remains preserved in legacy data for human inspection.

Pending Powder claims, releases, and mutation intents become inert historical records.

They must never be retried automatically.

The new runtime must not complete, release, heartbeat, patch, or otherwise mutate any existing Powder card or run.

Automatic downgrade after the first new-schema write is unsupported.

Restoring an older runtime requires explicitly restoring the timestamped registry backup first.

Keep legacy deserialization until the General separately authorizes permanent historical-data removal.

## Current Work Preservation

The following approved component heads must remain reachable and unchanged while the migration branch is prepared.

- Collaboration client head `696973d6b5abc5d3fa683092843c5126266925c6`.
- Terminal lifecycle head `b535437398230bc0ea2a6a218cd34ba08e36c3df`.
- CLI and MCP head `c6f249ca0438780aec7ede62f1f51140deaf78b5`.
- Approved integration head `efd3271a4efcde2b801a4b07fa4316560b8d9d15`.

The active Powder-bound sessions and worktrees must not be closed, released, reaped, overwritten, or reused by the migration implementation.

Before implementation begins, record the exact live terminal list, worktree list, branch heads, registry backup path, and installed application version in the implementation report.

## Implementation Sequence

### Phase 1: Control interface and registry migration

Use one owner for the control-plane lane because the registry, dispatch, lifecycle, and reconciliation code overlap heavily.

Start a new migration branch from `efd3271a4efcde2b801a4b07fa4316560b8d9d15`.

Cherry-pick this plan document's commit onto that branch if the branch does not already contain it.

Implement and verify these changes as separate logical commits.

1. Add the legacy Powder data module and schema-17 migration fixtures without changing behavior.
2. Add the agent runtime and work-stage model with backward-compatible Crew loading.
3. Add bounded bootstrap, agent detail, pagination, digest, checkpoint, and event-delta contracts.
4. Implement `start_agent` using existing terminal, identity, harness, workspace, and Git services.
5. Remove Powder authority from startup, dispatch, reconciliation, status projection, terminal close, and cleanup.
6. Add no-network assertions and retired-operation tombstones.

Freeze the control request and response schemas after this phase.

Do not begin parallel CLI, MCP, or frontend adaptation until the schema freeze is committed and reviewed.

### Phase 2: CLI and MCP adaptation

Assign one owner to the CLI and MCP lane after the control schema freezes.

Add these CLI commands.

```text
th agents start
th agents list
th agents show
th agents checkpoint
th agents events
```

Keep strict flags, stable JSON envelopes, deterministic ordering, clean JSON stdout, structured errors, and the documented exit taxonomy.

Add matching thin MCP adapters over the same backend operation names and fields.

Remove Powder tools from the MCP catalog and replace CLI Powder commands with the one-release tombstone.

Update structural CLI process tests and MCP catalog, schema, parity, and authorization tests.

### Phase 3: Frontend adaptation

Assign one owner to the frontend lane after the control schema freezes.

Remove the Board panel, Powder board selection, Powder Project binding, and Powder Captain commissioning fields.

Add a `Start agent` action using the frozen `start_agent` contract.

Display runtime state and work stage separately.

Display the latest checkpoint, directory, detected branch, harness, and owning Captain.

Use `agent_events` cursors for incremental updates rather than full-state polling.

Preserve generic web preview functionality that is not Powder-specific.

### Phase 4: Runtime deletion and integration

Use one integration owner to combine the reviewed lanes.

Delete the Powder HTTP client and network-facing tests after no remaining live path imports it.

Delete the CLI Powder implementation, Powder contract tests, Board components, and Powder-only IPC bindings.

Remove dependencies used exclusively by Powder after verifying the complete dependency graph.

Archive historical Powder design and review documents by clearly labeling them historical.

Do not rewrite historical claims as though Powder never existed.

Update current Captain, MCP, CLI, and workflow documentation to use the agent-session model.

Reconcile non-Powder feature branches against the frozen interface before integrating them.

Do not resolve shared-file conflicts by discarding newer provider, permission, workspace, history, or recovery behavior.

### Phase 5: Release candidate

Produce version `0.3.105` through the existing version process.

Do not manually edit generated changelogs.

Build and test without requiring a Powder process, profile, credential, endpoint, or network route.

Do not install, restart, publish, push, or release without separate General authorization.

## Required Verification

### Registry and migration tests

- Load a real-shaped schema-17 fixture containing active, completed, cleanup-pending, and ambiguous Powder records.
- Prove the fixture loads without opening a network connection.
- Prove assignments, provider conversations, directories, worktrees, branches, checkpoints, and Captain ownership survive migration.
- Prove legacy fields survive a read and write cycle without becoming authoritative.
- Prove a registry backup is created before the first migration write.
- Prove a failed backup prevents the migration write.
- Prove unknown future schema versions still fail closed.

### Agent-start tests

- Start Codex in a registered Project checkout.
- Start Claude in a registered Project checkout.
- Start both providers in an existing Git worktree.
- Reject a directory outside the Captain's Project.
- Reject cross-Project Captain and agent access.
- Persist the starting record before provider launch.
- Backfill the provider conversation ID without changing durable agent identity.
- Record an honest failure when terminal creation, harness launch, or permission attestation fails.
- Prove retries do not create duplicate agent records or terminals.

### Lifecycle and event tests

- Keep runtime state separate from work stage.
- Preserve `needsInput`, `readyForReview`, `awaitingIntegration`, and `complete` across provider activity and restart.
- Never infer completion from idle or missing terminal state.
- Enforce Crew-self and owning-Captain checkpoint authority.
- Return stable event cursors and only events after the requested cursor.
- Bound checkpoint text, event batches, active-agent summaries, and removed-agent pages.
- Omit assignment bodies from bootstrap and list responses by default.

### Powder isolation tests

- Prove startup makes zero Powder calls.
- Prove Captain bootstrap makes zero Powder calls.
- Prove agent start and reconciliation make zero Powder calls.
- Prove terminal close and application shutdown make zero Powder calls.
- Prove every retired Powder command returns `powder_retired` without a side effect.
- Prove no protected Powder profile or credential is required to build, test, or run T-Hub.

### CLI, MCP, and frontend tests

- Verify strict CLI usage and stable JSON output for every new agent command.
- Verify MCP schemas use `additionalProperties: false` and expose no credential or endpoint fields.
- Verify read-capability agents cannot start other agents.
- Verify a Captain cannot mutate another Captain's agent.
- Verify the full UI flow from Captain creation through agent start, checkpoint, review, integration, completion, restart recovery, and guarded cleanup.
- Verify no Board or Powder controls remain visible.
- Verify layout, status labels, empty states, errors, and loading states in the real desktop UI.

### Repository gates

- Run focused tests after every logical commit.
- Run the complete relevant Rust, CLI, MCP, and frontend suites before integration approval.
- Run `cargo fmt --all -- --check`.
- Run warnings-denied Clippy for affected Rust targets.
- Run frontend lint and type checks.
- Run `git diff --check`.
- Investigate and fix flaky tests rather than waiving them.
- Require an independent review of control-plane, registry-migration, permission, cleanup, and public protocol changes.

## Acceptance Criteria

The migration is complete only when all of the following are true.

- A Captain can start and supervise multiple Codex or Claude agents without Powder.
- A new agent needs only an assignment and an existing directory, with optional harness and presentation fields.
- T-Hub durably recovers Captain, agent, provider conversation, Git, workspace, lifecycle, and checkpoint state after restart.
- Human-readable progress is available without introducing cards or a task board.
- Bootstrap and ongoing supervision use bounded snapshots and cursor deltas.
- Powder is absent from the active UI, CLI help, MCP catalog, Project flow, Captain flow, agent flow, and runtime dependency path.
- Existing Powder records remain preserved but inert.
- Current approved work and active sessions remain recoverable.
- No merge, push, install, restart, publish, release, or destructive cleanup occurs without explicit authorization.

## First Actions for the Implementation Terminal

1. Read `AGENTS.md`, `docs/cli-contract.md`, and this entire plan.
2. Confirm the canonical repository and list every worktree before creating a branch or editing files.
3. Confirm `efd3271a4efcde2b801a4b07fa4316560b8d9d15` exists and remains clean in its integration worktree.
4. Record the current registry and terminal state through read-only T-Hub operations.
5. Create an isolated migration worktree and branch from `efd3271a4efcde2b801a4b07fa4316560b8d9d15`.
6. Cherry-pick this document's commit if needed.
7. Begin only Phase 1 and keep the control-plane lane single-owner until its interface is frozen and reviewed.
8. Commit every verified logical change separately with clear messages and no agent co-author.
9. Report exact commits, tests, failures, residual risks, and the next unblocked phase.
