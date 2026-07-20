# Codex Session Workflow Review and Supervisory Model

## Status

This document is a read-only review snapshot of Codex work performed from July 18 through July 20, 2026.
It records workflow findings, user decisions, and recommendations rather than acting as an implementation plan or authoritative product backlog.
Repository, runtime, branch, and agent state described here can become stale and must be verified before any action is taken.
Canonical operating, relationship, status, roadmap, CLI, and worktree contracts should change only when their corresponding implementation becomes active and verified.

## Scope and Method

The review examined recent Codex rollout records, user prompts, assistant status reports, tool activity, context compactions, repository history, branch relationships, worktree inventory, build provenance, and observed packaged-GUI behavior.
The review emphasized repeated patterns across Captain, Crew, implementation, review, Appturnity, build, installation, and bug-investigation sessions.
Sensitive prompt contents and credentials were not copied into this document.

## Executive Summary

The recent sessions produced substantial and often high-quality engineering work.
Independent review agents found real defects, agents generally used scoped commits, and the final packaged-GUI investigation identified the workspace persistence failure correctly.

The dominant weakness was not implementation ability.
It was the absence of a single, monotonic path from assignment to a known shippable baseline.
The words implemented, reviewed, complete, integrated, tested, packaged, installed, and live-verified were repeatedly used as if they described nearly the same state.
That ambiguity caused repeated status questions, premature completion claims, extra builds, and work beginning from stale baselines.

The workflow should establish an exact baseline commit before dispatching Crew and preserve that provenance through integration, packaging, installation, and live verification.
Implementation, independent review, acceptance testing, integration, packaging, installation, and live verification should be reported separately.
For a stated engineering scope, `complete` should mean that the exact commit passed its required acceptance checks and independent review.
Integration, installation, and live verification are separate later states and must never be inferred from `complete`.
Visible product bugs require packaged-GUI end-to-end evidence, even when lower-level automated tests pass.

The supervisory model should also change.
Exactly one durable Cortana identity should remain running or recovering, while Captains and Cortana concentrate on decisions, prioritization, decomposition, delegation, evidence review, and escalation.
Multi-step investigation, terminal and repository inspection, resource recovery, and administrative mutations should be performed by privileged Crew under durable, revocable, role-scoped grants.

The workflow should not impose an arbitrary three-to-four-Crew cap.
It should dispatch every genuinely independent lane whose ownership and dependencies are explicit, up to healthy limits reported by the runtime governor.
Capacity must remain reserved for Cortana, standing administrators, and recovery, and mutable shared interfaces must have a named integration owner and ordering contract before parallel work begins.

## Evidence Snapshot

The following measurements are useful indicators rather than billing figures.
The token counters include extensive cached context replay.

| Observation | Evidence |
| --- | --- |
| A long Captain session accumulated excessive orchestration work. | One July 18 Captain rollout recorded 2,659 tool calls, 10 context compactions, and about 410.8 million cumulative input tokens, of which about 405.1 million were cached. |
| Long-lived Crew sessions also accumulated heavy context pressure. | Two sampled Crew rollouts recorded 4,131 and 2,259 tool calls, with 20 and 14 compactions respectively. |
| The de-Powder implementation remained too large for one uninterrupted context. | The rollout recorded 1,011 tool calls, 5 compactions, and about 135.8 million cumulative input tokens, of which about 133.2 million were cached. |
| Users had to reconstruct project state conversationally. | Short prompts included at least 15 direct status questions, 7 questions about what remained, 6 build or install questions, and 3 merge-state questions. |
| Worktree inventory had become difficult to reason about. | The repository exposed 101 worktrees at review time, including 45 T-Hub worktrees and 37 detached Windows build worktrees. |
| The remote baseline lacked useful audit history. | The `origin/main` commit titled `Fix cold-start shell placement` represented 163 changed files, about 76,488 additions, and about 17,719 deletions. |
| The installed tree was preserved, but lineage was confusing. | The tree at `origin/main` commit `3b7486f` matched the verified `89932b3` build tree, while local `main` was still one commit behind and baseline reconciliation remained separate. |
| Parallel lanes began before the baseline was settled. | Responsive-header, Cortana-recovery, and Codex-history Crew branches were created from `1309431` while the reconciliation branch was still being validated. |

## What Worked Well

### Independent review found material defects

Independent reviewers identified remaining Powder lifecycle paths, missing agent authorization, incomplete lifecycle transitions, event cursor weaknesses, stale active documentation, frontend gaps, and ancestry mismatches.
These were substantive findings rather than stylistic preferences.

### Agents usually preserved logical changes

Within the working implementation line, agents generally created separate commits for schema work, idempotency, authorization, lifecycle handling, frontend recovery, tests, documentation, and packaging changes.
Verification frequently included Rust tests, frontend tests, CLI tests, MCP tests, Clippy, formatting, and diff checks.

### The final GUI investigation found the real synchronization failure

The detailed investigation showed that the application treated structured native errors as legitimate stale snapshots and adopted old server state over recent local changes.
It also found that ordinary shells could be placed temporarily in the reserved Captain workspace, causing the backend to reject later layout reports.
Those findings explained why workspaces and terminals appeared briefly and then disappeared.

### The Appturnity plan review prevented destructive cleanup

The Appturnity review found that branches described as patch-equivalent actually contained unmatched commits.
It prevented premature branch deletion and adjusted the plan without performing the destructive work.

### Removing Powder was directionally justified

The Powder-backed workflow required extensive credential isolation, board state, claim and run reconciliation, retry handling, and cross-session supervision.
The operational cost had become disproportionate to the planning value it provided.
Replacing it with a smaller durable agent-session ledger is a sound direction, provided the replacement remains bounded and observable.

## Findings and Recommendations

### 1. Completion language was premature

The de-Powder migration was described as complete before independent review found several release-blocking gaps.
Powder cleanup was later described as complete before another audit found masked or unfinished behavior.
The full Rust workspace was green immediately before the user reported that the central workspace-creation flow was still broken.

Use the following states separately:

1. `implemented` means the code exists.
2. `reviewed` means an independent reviewer approved the exact commit.
3. `tested` means the required acceptance checks passed on the exact commit.
4. `complete` means the stated scope is both tested and independently reviewed.
5. `integrated` means the complete commit is present in the named canonical baseline.
6. `packaged` means an artifact was built from that baseline.
7. `installed` means that artifact replaced the intended installation.
8. `live-verified` means the required flow passed against the installed application, whether tested by a human or an AI agent.

A commit identifier is required provenance for every state rather than a substitute for one of the states.
Status reports must not collapse `complete`, `integrated`, `installed`, and `live-verified` into a generic finished condition.
For visible product bugs, the stated acceptance contract must include packaged-GUI end-to-end evidence.
Operational resolution should not be claimed until the intended installed application is live-verified, even if the implementation scope became complete earlier.

### 2. User-visible bugs were not reproduced deeply enough before the first fixes

The first workspace diagnosis reproduced the server-side inconsistency but did not exercise the full packaged GUI lifecycle.
Several recovery and hardening commits followed before the exact create, disappear, close, restart, and persistence loop was traced end to end.
The user eventually had to request a detailed bug review before the investigation isolated the first incorrect state transition.

For user-visible bugs, use this mandatory sequence:

1. Reproduce the exact behavior in the installed GUI.
2. Record frontend state, native response, authoritative registry state, and process state at the first divergence.
3. Add a regression test that fails for the observed reason.
4. Implement one targeted fix.
5. Obtain independent review of the exact fix commit.
6. Run all required acceptance checks on that exact commit.
7. Build and install the exact reviewed and tested baseline.
8. Replay the original create, close, restart, and persistence flow in the packaged GUI.
9. Record the source commit, artifact identity, installation target, and observed result.
10. Consider hardening and quick wins only after the original flow passes.

### 3. Supervisors performed too much investigation and administration directly

Long sessions repeatedly replayed large histories, polled agents, reread terminal output, inspected repository state, and compacted context.
One earlier Captain review measured thousands of terminal reads and extremely large Captain registry responses.
This work displaced supervisory judgment and increased the chance that the Captain lost track of integration and release state.

Adopt adaptive delegation:

- A supervisor may read one bounded authoritative summary directly when that is enough to make a decision.
- Multi-step investigations, terminal inspection, repository inspection, session cleanup, resource recovery, worktree maintenance, and all administrative mutations should be delegated to authorized Crew.
- Captains and Cortana retain responsibility for prioritization, task decomposition, ownership assignment, review gates, evidence evaluation, escalation, and final decisions.
- Standing administrative aides should handle routine operations.
- Additional administrative Crew may be dispatched when independent investigations or recovery lanes can proceed safely in parallel.
- Completion should be event-driven where possible instead of repeatedly polling unchanged terminals.
- Registry, checkpoint, dispatch, and supervision responses should return compact projections by default.
- A long-running session should checkpoint and rotate when context pressure threatens reliable decisions, while its durable identity and grants remain intact.

Delegation does not remove supervisor accountability.
It separates execution from judgment so that evidence is gathered by scoped Crew and evaluated by the responsible supervisor.

### 4. Administrative authority needs durable, scoped roles

A control token or control connection should mean only that its holder can call control operations.
It must not imply that every operation or target is authorized.
Effective authorization should combine the presented control capability with a durable role, current grants, target scope, operation scope, and revocation state.

Introduce a durable `Ship Admin` Crew role with these properties:

- The owning Captain appoints a Ship Admin, and the appointment remains active until revoked or invalidated.
- The role carries the Captain's operational authority only within that exact ship.
- The role may perform delegated worktree maintenance, session cleanup, resource recovery, status investigation, and other authorized administration.
- The role cannot appoint administrators, re-delegate authority, become Captain, cross into another ship, or exercise authority the Captain does not possess.
- Destructive operations still require authoritative ownership checks and every exact approval that would be required if the Captain acted directly.
- One standing Ship Admin per active Captain is the default, not a maximum.

Introduce a durable combined `Fleet Admin` Crew role with these properties:

- Cortana appoints a Fleet Admin, and the appointment remains active until revoked or invalidated.
- The role may inspect and administer Captains across the fleet only within Cortana's authority.
- The role supports cross-Captain status reports, recovery, resource maintenance, and retirement preparation.
- Multiple Fleet Admins may exist concurrently when independent work justifies them.
- One standing Fleet Admin should be maintained by default.
- The role cannot direct implementation, acquire Captain authority, grant roles, or bypass approvals reserved to the General.

Agent dispatch and durable identity records should include the delegated role, delegating supervisor identity, ship or fleet scope, grant generation, revocation state, permitted operation set, and audit attribution.
Every privileged action should be attributable to both the acting Crew member and the delegating supervisor.
Revocation must block new operations across reconnects and restarts.
Supervisor retirement, ship ownership changes, and other ownership invalidation must revoke or invalidate dependent administrative authority.

### 5. Status reporting did not answer the user's actual operational questions

The user repeatedly asked how the work was going, whether agents were active, what remained, whether changes were merged, and whether the current build was installed.
These questions indicate that ordinary agent updates did not provide a stable operational picture.

Every Captain status update should include:

- The canonical baseline branch and exact commit.
- Each active agent, its durable role, its worktree, its delegated scope, and its owned outcome.
- Work that is implemented but awaiting review or testing.
- Work that is independently reviewed but awaiting acceptance tests.
- Work that is complete but awaiting integration.
- The exact canonical baseline and automated gate status.
- The packaged artifact version and source commit.
- The installed application version, artifact identity, and source commit.
- Live verification status for the original user flow.
- The current blocker and exact next action.

The word parallelized should always include the number and type of active agents.
Reports should show implementation, review, testing, integration, packaging, installation, and live verification as separate fields.

### 6. Baseline and build provenance became fragile

The latest verified source tree was preserved on `origin/main`, but it appeared as one very large commit with a narrow and misleading title.
The logical commit series remained recoverable through the prior line and build worktree, but ordinary history no longer explained what the remote commit contained.
At the same time, local `main`, the reconciliation branch, installed artifacts, and newly created Crew branches represented different integration states.

Adopt the following controls:

- Establish an exact source commit before dispatching any Crew assignment.
- Preserve dirty unrelated user work and exclude it from the Crew baseline.
- Maintain exactly one named integration baseline for an active release candidate.
- Commit shared schemas and interfaces before dependent Crew begin.
- Include the exact source commit in every assignment, identity record, and checkpoint.
- Preserve logical commit history with a merge or reviewed rebase instead of collapsing a broad migration into a narrowly named commit.
- Record which baseline commit and Crew commits produced every integration result.
- Build Windows and WSL artifacts from the same reviewed tree hash.
- Emit an artifact manifest containing branch, source commit, tree hash, version, installer hash, build time, and signature status.
- Record the installed executable version and source tree beside the artifact manifest.
- Remove or archive obsolete build worktrees only after their artifacts are no longer needed and authoritative safety checks permit removal.

### 7. Fixed Crew-count guidance confuses capacity with safety

A fixed recommendation of three or four active implementation lanes is arbitrary.
It can underuse a healthy system when many independent lanes exist, and it can overload a constrained system when those lanes contend for providers, worktrees, shared files, or integration ownership.

Use governor-backed adaptive capacity instead:

- Parallelize every genuinely independent lane whose ownership, baseline, dependencies, and acceptance contract are explicit.
- Bound concurrency using runtime governor limits, machine health, provider limits, worktree availability, and integration-collision checks.
- Reserve sufficient capacity for Cortana, standing Ship Admins and Fleet Admins, and administrative recovery.
- Reject or defer parallel dispatch when lanes share mutable schemas, interfaces, or files unless one integration owner and an ordering contract are established.
- Allow more than four Crew when the governor reports healthy capacity and isolation checks pass.
- Prefer one owner over artificial parallelism when a lane cannot be decomposed without shared mutable state.

Capacity planning should be dynamic and visible.
Dispatch preflight should explain admitted lanes, deferred lanes, reservations, and the constraint that determined the current limit.

### 8. Cortana startup needs backend singleton reconciliation

The current adopt-only startup behavior cannot guarantee that Cortana remains available after terminal, Harness, or application failure.
It also does not provide a deterministic answer when more than one candidate runtime appears.

Replace it with an idempotent backend reconciliation operation that:

- Recovers or creates exactly one durable Cortana runtime.
- Preserves Cortana's durable identity and checkpoints when replacing its terminal or Harness.
- Serializes concurrent startup attempts under a stable operation identity.
- Detects duplicate candidates and selects the authoritative generation deterministically.
- Quarantines or safely retires non-authoritative duplicates.
- Fails closed and displays a degraded recovery state when identity or generation authority is uncertain.
- Reserves enough governor capacity for Cortana and administrative recovery even under high Crew load.

One Cortana means one durable supervisor identity with at most one authoritative active runtime.
It does not limit the number of Cortana-owned Fleet Admins or other Crew.

### 9. Known test flakiness was accepted as a release note

The remote baseline commit message recorded three concurrency-sensitive control tests that passed with one test thread.
Serial execution is useful for diagnosis, but it does not resolve the underlying isolation problem.
The repository instructions explicitly require test flakiness to be fixed even when it is not introduced by the current task.

Treat unexplained parallel failures as release blockers.
Give integration tests unique tmux sockets, ports, registries, temporary directories, and process identities.
Do not convert a failing parallel gate into a passing serial gate without documenting and fixing the shared-state defect.

### 10. Visible Crew topology was confusing

The Captain initially created extra helper terminals and distributed Crew in a way the user did not expect.
The user clarified that one Crew member should normally have one visible terminal and that related T-Hub Crew should share one workspace.
The Captain workspace also disappeared from the visible UI when no Captain was registered, which looked like data loss even though it was an empty-state behavior.

Make the default topology explicit:

- Each Crew member owns one visible terminal.
- Related Crew are grouped in one named workspace.
- Helper shells remain hidden or attached within the owning Crew context.
- Captain identities recover as single instances within their ships.
- The durable Cortana identity has at most one authoritative active runtime and visibly reports recovery state.
- The sidebar shows a clear empty state when the reserved Captain workspace exists but no Captain is registered.
- Closed Codex sessions remain visible through provider-neutral History.
- Ship Admin and Fleet Admin roles are visible without making them appear to be Captains or implementation owners.

### 11. Infrastructure work dominated product work

Nearly all reviewed activity focused on T-Hub and Powder, while Appturnity received a short plan review but little durable execution.
This may be intentional, but it creates a risk that orchestration infrastructure consumes the capacity it was meant to unlock.

Use explicit capacity reservations when multiple products matter.
The governor and supervisors should preserve the chosen allocation while still admitting every safe independent lane within it.
A change in allocation should be a deliberate prioritization decision, not a side effect of a fixed Crew cap or accumulated infrastructure sessions.

## Immediate Operational Risk at Review Time

The installed and remote source trees were equivalent at review time, so the recent workspace fixes were not lost.
However, the local baseline was still being reconciled while three feature Crew branches had already been created from the older `1309431` line.
Those feature lanes could perform valid work against obsolete interfaces and then require avoidable conflict resolution or reimplementation.

The safest immediate sequence is:

1. Pause feature implementation that depends on the unsettled baseline.
2. Preserve dirty unrelated user work outside the candidate baseline.
3. Finish and verify the baseline reconciliation.
4. Move local `main` to the approved baseline through the repository's chosen integration procedure.
5. Rebase, recreate, or retarget active feature Crew from that exact commit.
6. Commit shared interfaces before dispatching dependent Crew.
7. Reproduce each lane's target behavior again before continuing implementation.
8. Record the exact baseline commit in every Crew assignment and checkpoint.

## Recommended Operating Model

### Baseline gate

Select and record one exact source commit before Crew dispatch.
Exclude dirty unrelated user changes, and commit shared interfaces before dependent assignments begin.

### Assignment gate

Every assignment should define one user outcome, one exact baseline commit, acceptance criteria, required end-to-end flow, allowed files or subsystem, delegated role, delegating supervisor, permitted operations, and target reporting state.

### Supervisory gate

Captains and Cortana should make decisions, set priorities, decompose outcomes, assign ownership, evaluate evidence, enforce review gates, and escalate blocked authority.
They may consume a single bounded authoritative summary directly, but should delegate multi-step investigation and administrative execution.

### Authorization gate

Control capability, durable identity, role grant, target scope, operation scope, ownership state, and revocation generation must all authorize a privileged operation.
No delegated role may create more authority than its supervisor holds.

### Parallelization gate

Dispatch every genuinely independent lane admitted by the runtime governor after ownership, dependency, worktree, provider, and integration-collision checks pass.
Reserve capacity for Cortana, standing administrators, and recovery.
Shared mutable schemas, interfaces, or files require a single integration owner and an explicit ordering contract.

### Review and testing gate

Independent review should inspect the exact commit before completion is reported.
Required acceptance checks should run on that same commit.
Visible product bugs require packaged-GUI end-to-end acceptance evidence in addition to lower-level automated checks.

### Completion gate

Mark the stated scope `complete` only when the exact commit is both independently reviewed and acceptance-tested.
Do not infer integration, packaging, installation, or live verification from completion.

### Integration gate

Complete commits should be integrated one dependency layer at a time.
The integration branch should run all required gates after each wave, and dependent lanes should rebase before continuing when shared code changes.
Integration records should identify the source baseline and every Crew commit that produced the canonical result.

### Release gate

Package the exact integrated baseline and preserve its artifact manifest.
Install that artifact into the intended target and verify the original user-visible scenario against the installed application.
Report packaging, installation, and live verification separately regardless of who performs the verification.

### Cleanup gate

After integration and artifact retention are confirmed, authorized administrative Crew may prepare obsolete worktrees, orphaned agent records, stale Captain pins, and superseded build directories for removal.
Destructive cleanup may proceed only when the authoritative ownership and worktree safety services report the exact target removable and every required approval is present.
Fully automatic obsolete-worktree cleanup should wait until those ownership, retention, and destructive-action guarantees are authoritative.

### Cortana continuity gate

The backend should continuously reconcile toward one durable Cortana identity with at most one authoritative active runtime.
Recovery must preserve identity and checkpoints, serialize concurrent attempts, quarantine duplicates, expose uncertainty as degraded state, and retain capacity for administrative recovery.

## Recommended Captain Status Format

```text
Outcome: <one-sentence user outcome>
Baseline: <branch> at <exact commit>
Agents: <active count, durable roles, and owned lanes>

Implemented: <items and exact commits, or none>
Reviewed: <items and exact commits, or none>
Tested: <acceptance checks and exact commits, or none>
Complete: <reviewed and tested scopes, or none>
Integrated: <canonical baseline commit, or none>
Packaged: <artifact version and source commit, or none>
Installed: <target, artifact identity, and source commit, or none>
Live verified: <exact installed flow and result, or none>

Capacity: <governor limit, reservations, and deferred lanes>
Blocker: <one blocker or none>
Next action: <one concrete action>
```

## Recommended Prompt Pattern

```text
Implement <outcome> from exact baseline <commit>.
Preserve and exclude unrelated dirty user work.
Do not start quick wins or unrelated features until the original acceptance flow passes.
Dispatch every genuinely independent lane that passes governor, ownership, dependency, worktree, and collision checks.
Reserve capacity for Cortana, standing administrators, and recovery.
Keep one visible terminal per Crew member and group related Crew in one workspace.
Delegate multi-step investigation and administrative execution to authorized Crew.
Report implemented, reviewed, tested, complete, integrated, packaged, installed, and live-verified states separately.
For visible product bugs, require packaged-GUI end-to-end evidence.
Do not describe the installed product issue as resolved until the installed application passes <exact user flow>.
```

## Priority and Effort Matrix

Effort informs sequencing and dependency planning, but it should not outweigh quality, simplicity, robustness, scalability, or long-term maintainability.

| Recommendation | Impact | Effort | Treatment |
| --- | --- | --- | --- |
| Correct completion terminology and Captain status format | High | Easy | Apply immediately to skills, prompts, and review documentation. |
| Require an exact baseline commit before Crew dispatch | High | Easy | Apply immediately as a dispatch gate. |
| Require packaged-GUI E2E for visible bugs | High | Easy operationally | Apply immediately, then automate evidence capture later. |
| Remove the three-to-four-Crew policy cap | High | Easy | Replace it with adaptive, governor-backed capacity rules. |
| Require supervisors to delegate administrative execution | High | Easy as policy | Update Captain and Cortana operating instructions first. |
| Add durable Ship Admin and Fleet Admin roles | High | Hard | Implement durable identity, persistence, access control, grants, CLI, MCP, and UI support. |
| Guarantee one automatically recovered Cortana | High | Hard | Implement backend singleton reconciliation, recovery, duplicate handling, and startup tests. |
| Safely delegate worktree destruction and cleanup | High | Hard | Depend on the unified authoritative worktree safety service and delegated authorization. |
| Add dynamic Crew capacity planning and reservations | Medium-high | Moderate | Extend governor reporting and dispatch preflight after role foundations. |
| Add complete provenance from baseline through live verification | Medium-high | Moderate | Extend status records, artifact manifests, and Captain reporting. |
| Cosmetic wording and sidebar label refinements | Low | Easy | Perform with the related status and role UI work. |
| Fully automatic obsolete-worktree cleanup | Low-medium | Hard | Defer until ownership, retention, and destructive-action evidence are authoritative. |

The easy policy changes should be adopted first because they improve truthfulness and dispatch safety without depending on new authority mechanisms.
The hard role, singleton, and destructive-operation work should follow an explicit implementation sequence so that UI or CLI controls never precede authoritative backend enforcement.
Canonical contracts should be updated only as each corresponding implementation becomes active and verified.

## Verification Plan for the Recommended Model

- Prove that concurrent startup calls produce one Cortana identity and one authoritative live runtime.
- Kill Cortana's terminal, Harness, and application independently and verify automatic recovery without identity loss.
- Inject duplicate Cortana candidates and verify deterministic, fail-closed reconciliation.
- Test Ship Admin access against own-ship, sibling-ship, foreign-ship, General-reserved, and re-delegation operations.
- Test Fleet Admin inspection and maintenance across Captains while denying implementation direction and General-reserved actions.
- Verify that revocation immediately blocks new operations and survives application restart.
- Verify that supervisor retirement or ownership change invalidates dependent administrative grants.
- Verify that a Ship Admin can execute an exactly authorized worktree cleanup only after the worktree safety service reports the target removable.
- Dispatch more than four independent Crew and verify governor limits, reserved supervisor capacity, ownership isolation, and integration behavior.
- Verify that shared-file or shared-schema collisions prevent unsafe parallel dispatch.
- Verify that status surfaces never collapse `complete`, `integrated`, `installed`, and `live-verified`.
- Run packaged Windows end-to-end tests for visible changes and record the exact source commit, artifact, installation, and observed flow.

## Assumptions and Boundaries

- One Cortana means one durable supervisor identity with at most one authoritative active runtime, not one total Cortana-owned Crew member.
- One standing Fleet Admin and one standing Ship Admin per active Captain are defaults, not maximums.
- Ship Admin and Fleet Admin authority persists until explicit revocation, supervisor retirement, or ownership invalidation.
- Delegation transfers execution authority within the supervisor's existing boundary and never creates new product, destructive, release, installation, or external authority.
- Multiple Fleet Admins or Ship Admins may exist when independent work justifies them and governor capacity permits them.
- A human or an AI agent may perform live verification, but the evidence must refer to the intended installed application and exact flow.
- The review document remains an analysis artifact.
- Canonical operating, relationship, status, roadmap, CLI, and worktree contracts will be updated only when their corresponding implementation becomes active and verified.

## Final Assessment

The engineering quality was strongest when the workflow used independent exact-commit review, narrow ownership, explicit acceptance checks, and live evidence.
It was weakest when a long-running supervisor simultaneously investigated, administered, implemented, integrated, packaged, and reconstructed status from memory.

The system should use as many independent Crew lanes as healthy, reserved capacity permits rather than imposing a fixed policy cap.
That concurrency depends on one exact dispatch baseline, explicit ownership, collision-aware admission, role-scoped administrative execution, and complete provenance.
Captains and the single durable Cortana supervisor should remain focused on judgment and evidence, supported by standing Ship Admin and Fleet Admin Crew that can execute bounded operational work without acquiring supervisory authority.

The result is a workflow with truthful completion semantics and a traceable path from baseline through live verification.
