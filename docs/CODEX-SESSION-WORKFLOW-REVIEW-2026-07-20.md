# Codex Session Workflow Review

## Status

This document is a read-only review snapshot of Codex work performed from July 18 through July 20, 2026.
It records workflow findings and recommendations rather than acting as an implementation plan or authoritative product backlog.
Repository, runtime, branch, and agent state described here can become stale and must be verified before any action is taken.

## Scope and Method

The review examined recent Codex rollout records, user prompts, assistant status reports, tool activity, context compactions, repository history, branch relationships, worktree inventory, build provenance, and observed packaged-GUI behavior.
The review emphasized repeated patterns across Captain, Crew, implementation, review, Appturnity, build, installation, and bug-investigation sessions.
Sensitive prompt contents and credentials were not copied into this document.

## Executive Summary

The recent sessions produced substantial and often high-quality engineering work.
Independent review agents found real defects, agents generally used scoped commits, and the final packaged-GUI investigation identified the workspace persistence failure correctly.

The dominant weakness was not implementation ability.
It was the absence of a single, monotonic path from assignment to a known shippable baseline.
The words implemented, committed, reviewed, integrated, built, installed, and live-verified were repeatedly used as if they described nearly the same state.
That ambiguity caused repeated status questions, premature completion claims, extra builds, and work beginning from stale baselines.

The highest-leverage improvement is to make release state explicit and authoritative.
No feature lane should begin until its baseline is named and stable, and no user-visible bug should be called complete until the exact packaged flow passes after installation.

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

1. `implemented` means the code exists in a working tree.
2. `committed` means the logical change has a stable commit.
3. `reviewed` means an independent reviewer approved the exact commit.
4. `integrated` means the approved commit is present in the named canonical baseline.
5. `verified` means required automated gates pass on that exact baseline.
6. `packaged` means artifacts were built from that exact tree.
7. `installed` means the intended artifact replaced the intended installation.
8. `live-verified` means the original user-visible flow passed in the installed application.

The word complete should be reserved for the final required state.

### 2. User-visible bugs were not reproduced deeply enough before the first fixes

The first workspace diagnosis reproduced the server-side inconsistency but did not exercise the full packaged GUI lifecycle.
Several recovery and hardening commits followed before the exact create, disappear, close, restart, and persistence loop was traced end to end.
The user eventually had to request a detailed bug review before the investigation isolated the first incorrect state transition.

For user-visible bugs, use this mandatory sequence:

1. Reproduce the exact behavior in the installed GUI.
2. Record frontend state, native response, authoritative registry state, and process state at the first divergence.
3. Add a regression test that fails for the observed reason.
4. Implement one targeted fix.
5. Build and install the exact fixed commit.
6. Replay the original create, close, restart, and persistence flow.
7. Consider hardening and quick wins only after the original flow passes.

### 3. Orchestration consumed too much context and attention

Long sessions repeatedly replayed large histories, polled agents, reread terminal output, and compacted context.
One earlier Captain review measured thousands of terminal reads and extremely large Captain registry responses.
This overhead made supervision slower and increased the chance that the Captain lost track of integration and release state.

Use the following guardrails:

- A Captain should normally supervise no more than three or four active implementation lanes.
- File-changing work should use durable worktree Crew with explicit ownership.
- Ephemeral subagents should be limited to bounded read-only mapping or independent review.
- New agents should receive a minimal assignment packet rather than inherited conversation history.
- A session should checkpoint and rotate after two compactions or sustained high context pressure.
- Completion should be event-driven where possible instead of repeatedly polling unchanged terminals.
- Registry, checkpoint, dispatch, and supervision responses should return compact projections by default.

### 4. Status reporting did not answer the user's actual operational questions

The user repeatedly asked how the work was going, whether agents were active, what remained, whether changes were merged, and whether the current build was installed.
These questions indicate that ordinary agent updates did not provide a stable operational picture.

Every Captain status update should include:

- The canonical baseline branch and commit.
- Each active agent, its type, its worktree, and its owned outcome.
- Work that is implemented but awaiting review.
- Work that is reviewed but awaiting integration.
- Current automated gate status.
- Packaged artifact version and source commit.
- Installed application version and source commit.
- Live verification status for the original user flow.
- The current blocker and exact next action.

The word parallelized should always include the number and type of active agents.

### 5. Baseline and build provenance became fragile

The latest verified source tree was preserved on `origin/main`, but it appeared as one very large commit with a narrow and misleading title.
The logical commit series remained recoverable through the prior line and build worktree, but ordinary history no longer explained what the remote commit contained.
At the same time, local `main`, the reconciliation branch, installed artifacts, and newly created Crew branches represented different integration states.

Adopt the following controls:

- Maintain exactly one named integration baseline for an active release candidate.
- Land or freeze shared interfaces before dependent lanes begin.
- Preserve logical commit history with a merge or reviewed rebase instead of collapsing a broad migration into a narrowly named commit.
- Build Windows and WSL artifacts from the same reviewed tree hash.
- Emit an artifact manifest containing branch, source commit, tree hash, version, installer hash, build time, and signature status.
- Record the installed executable version and source tree beside the artifact manifest.
- Remove or archive obsolete build worktrees after their artifacts are no longer needed.

### 6. Known test flakiness was accepted as a release note

The remote baseline commit message recorded three concurrency-sensitive control tests that passed with one test thread.
Serial execution is useful for diagnosis, but it does not resolve the underlying isolation problem.
The repository instructions explicitly require test flakiness to be fixed even when it is not introduced by the current task.

Treat unexplained parallel failures as release blockers.
Give integration tests unique tmux sockets, ports, registries, temporary directories, and process identities.
Do not convert a failing parallel gate into a passing serial gate without documenting and fixing the shared-state defect.

### 7. Visible Crew topology was confusing

The Captain initially created extra helper terminals and distributed Crew in a way the user did not expect.
The user clarified that one Crew member should normally have one visible terminal and that related T-Hub Crew should share one workspace.
The Captain workspace also disappeared from the visible UI when no Captain was registered, which looked like data loss even though it was an empty-state behavior.

Make the default topology explicit:

- Each Crew member owns one visible terminal.
- Related Crew are grouped in one named workspace.
- Helper shells remain hidden or attached within the owning Crew context.
- Captain and Cortana identities recover automatically as single instances.
- The sidebar shows a clear empty state when the reserved Captain workspace exists but no Captain is registered.
- Closed Codex sessions remain visible through provider-neutral History.

### 8. Infrastructure work dominated product work

Nearly all reviewed activity focused on T-Hub and Powder, while Appturnity received a short plan review but little durable execution.
This may be intentional, but it creates a risk that orchestration infrastructure consumes the capacity it was meant to unlock.

Set an explicit work-in-progress allocation when multiple products matter.
For example, reserve one lane for T-Hub reliability and one lane for product delivery, and require a deliberate decision before infrastructure work consumes both.

## Immediate Operational Risk at Review Time

The current installed and remote source trees were equivalent, so the recent workspace fixes were not lost.
However, the local baseline was still being reconciled while three feature Crew branches had already been created from the older `1309431` line.
Those feature lanes could perform valid work against obsolete interfaces and then require avoidable conflict resolution or reimplementation.

The safest immediate sequence is:

1. Pause feature implementation that depends on the unsettled baseline.
2. Finish and verify the baseline reconciliation.
3. Move local `main` to the approved baseline through the repository's chosen integration procedure.
4. Rebase, recreate, or retarget active feature Crew from that exact baseline.
5. Reproduce each lane's target behavior again before continuing implementation.
6. Record the baseline commit in every Crew assignment and checkpoint.

## Recommended Operating Model

### Assignment gate

Every assignment should define one user outcome, one canonical baseline commit, acceptance criteria, required E2E flow, allowed files or subsystem, and completion state.

### Parallelization gate

Parallel work should begin only after ownership and dependency analysis proves that the lanes can proceed from the same stable baseline.
Shared schemas and interfaces should be frozen or assigned to one owner before downstream work starts.

### Review gate

Independent review should happen before completion is reported.
The reviewer should inspect the exact commit and reproduce the most important acceptance path rather than reviewing an unspecified moving branch.

### Integration gate

Reviewed commits should be integrated one dependency layer at a time.
The integration branch should run all required gates after each wave, and dependent lanes should rebase before continuing when shared code changed.

### Release gate

The exact integrated tree should be packaged once, installed once, and verified through the original user-visible scenario.
Additional feature work should wait until that scenario passes.

### Cleanup gate

After integration and artifact retention are confirmed, obsolete worktrees, orphaned agent records, stale Captain pins, and superseded build directories should be removed through a safe and auditable cleanup flow.

## Recommended Captain Status Format

```text
Outcome: <one-sentence user outcome>
Baseline: <branch> at <commit>
Agents: <active count and type>

Implemented: <items or none>
Reviewed: <items or none>
Integrated: <items or none>
Tests: <green, failing, or running>
Packaged: <version and source commit, or no>
Installed: <version and source commit, or no>
Live verified: <exact flow and result, or no>

Blocker: <one blocker or none>
Next action: <one concrete action>
```

## Recommended Prompt Pattern

```text
Implement <outcome> through packaged-GUI E2E verification.
Do not start quick wins or unrelated features until the original flow passes.
Use at most three agents, and base every lane on <commit>.
Keep one visible terminal per Crew member and group related Crew in one workspace.
Report implemented, reviewed, integrated, tested, packaged, installed, and live-verified states separately.
Do not call the task complete until the installed application passes <exact user flow>.
```

## Priorities

### P0

- Finish baseline reconciliation before continuing dependent feature work.
- Rebase or recreate active feature Crew from the approved baseline.
- Require packaged-GUI verification before declaring user-visible work complete.
- Fix the remaining parallel control-test flakiness.

### P1

- Add the authoritative release-state summary to every Captain update.
- Add artifact provenance linking source, package, installation, and runtime.
- Bound orchestration responses and replace repeated polling with incremental events.
- Rotate long-running Captain and Crew contexts earlier.

### P2

- Consolidate visible Crew into one terminal per agent and one workspace per related group.
- Add clear Captain and Cortana recovery and empty-state behavior.
- Clean up obsolete worktrees and detached build directories after retention checks.
- Establish an explicit capacity split between T-Hub infrastructure and product delivery.

## Final Assessment

The engineering quality was strongest when the workflow used independent exact-commit review, narrow ownership, explicit tests, and live evidence.
It was weakest when a long-running session simultaneously planned, implemented, integrated, packaged, supervised many agents, and answered status questions from memory.

The system does not primarily need more agents.
It needs fewer concurrent states, a stable baseline, truthful completion semantics, and one exact end-to-end release path.
