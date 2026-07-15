# T-Hub Captain Active Assignment

## Purpose

This document is the active instruction for Captain terminal `c2940be4` on ship `t-hub-app`.
The Captain must use the `$captain` skill and recover its durable manifest before acting.
The installed acceptance target is T-Hub `0.3.103` from source commit `8654986`.

## Current Authorization

Run Stage 1 only.
Do not begin the future production queue in this document.
Do not modify Powder source.
Do not edit the coordinator worktree.
Do not merge, push, install, publish, or release anything.

## Stage 1 - Real Captain and Crew Acceptance

Use Powder card `thub-local-acceptance` from repository `t-hub`.
Dispatch exactly one Codex Crew through T-Hub MCP.

Use these exact checkout values:

- Worktree: `/home/natkins/projects/tools/t-hub/t-hub-worktrees/local-acceptance`
- Branch: `test/local-acceptance`
- Sentinel: `/tmp/t-hub-crew-done/t-hub-app/local-acceptance.done`

The Crew assignment is read-only installed-runtime acceptance.
The Crew must not change repository files or create an empty commit.
The Crew must work only inside the assigned worktree and task scope.
The Crew must escalate destructive, security, installation, release, merge, push, or external decisions.

The Crew must verify all of the following:

1. The Powder card belongs to canonical repository `t-hub`.
2. T-Hub acquires the authoritative Powder claim and run.
3. T-Hub persists a durable Crew binding before launching the Harness.
4. One Crew terminal starts in the exact worktree and branch.
5. Interactive Codex is live and received the authoritative dispatch brief.
6. Powder receives the expected lifecycle and work-log evidence.
7. The completion sentinel is created only after the runtime checks pass.
8. The card completes or releases without leaving a stale claim.
9. The Captain checkpoints the final terminal, card, run, checkout, evidence, outcome, and residual risks.

The Captain must inspect the dispatched terminal before sending any additional text.
The Captain must not trust Codex lifecycle status alone while provider-aware status remains incomplete.
The Captain must use the Crew binding, terminal process, Powder run, report, and sentinel together as evidence.

## Stop Conditions

Stop immediately if dispatch fails or reports incomplete rollback.
Stop immediately if the card repository, claim owner, worktree, branch, Harness, or terminal is ambiguous.
Stop immediately if more than one Crew terminal appears.
Stop immediately if the Crew attempts to edit the repository.
Stop immediately if a raw credential appears in output, prompts, files, or Powder evidence.
Reconcile authoritative T-Hub and Powder state before reporting any failure.
Do not retry an ambiguous mutation with a new request identity.

The Captain must not touch `thub-captain-card-envelope` during Stage 1.
The Captain must not adopt or clear the existing `card-envelope.done` or `worktree-validation.done` sentinels.

## Stage 1 Exit Gate

Stage 1 passes only when the real Powder claim, durable Crew binding, exact checkout, live Codex Harness, work log, completion report, new sentinel, final card state, and Captain checkpoint agree.
Stage 1 fails if any claim, Crew record, terminal, or owned resource remains stale or ambiguous.

After Stage 1, stop and report one of these exact classifications:

- `STATUS: acceptance passed`
- `DECISION-NEEDED: acceptance blocked`
- `EMERGENCY: security or destructive risk`

## Future Production Queue

This queue is planning context only until the General authorizes a production Assignment.

1. Automatically match and bind existing Powder repositories from exact Git identity while hiding unrelated boards from the normal Captain flow.
2. Integrate safe board creation only after Powder exposes a non-overwriting create-if-absent contract.
3. Reproduce and repair the installed registered-Project Board and remaining Run and Preview issues.
4. Repair coordinator-to-Captain monitoring, lifecycle status, fleet watches, and durable messaging.
5. Complete Codex and Claude Captain, Crew, Workspace, claim, rollback, event, recovery, and retirement acceptance.
6. Connect provider-neutral History, Codex resume, auto-continue, and usage persistence without extending the legacy Claude-only Recent contract.
7. Restore audible voice requests and reduce workspace and terminal switching latency.
8. Verify the Claude terminal header.
9. Run the packaged 1, 4, 8, and 16 terminal performance matrix after implementation stabilizes.

Powder create-if-absent, scoped repository authorization, idempotent retry identity, and fork semantics belong to a separate Powder Captain and repository.

## Monitoring Contract

The Captain monitors its own Crew through the durable Crew roster, Powder claim and run, terminal inspection, report, and sentinel.
The General and read-only coordinator monitor the Captain without steering its Crew directly.

The external monitor should verify these signals:

1. Captain terminal `c2940be4` remains live in the canonical T-Hub checkout.
2. Exactly one new Crew terminal appears after dispatch.
3. Powder card `thub-local-acceptance` moves from `ready` to `claimed` with one run.
4. The durable Captain record contains the same Crew terminal, card, run, worktree, and branch.
5. The Crew terminal runs Codex in the exact worktree.
6. Powder receives attributed work-log evidence and a final state.
7. A newly timestamped `local-acceptance.done` sentinel appears.
8. The Captain writes a final checkpoint and no stale claim remains.

Current coordinator monitoring is passive and incomplete.
Cross-session transcript reads are denied, Codex work status is `unknown`, fleet wake monitoring is not active, and audited messaging currently times out.
Read-only tmux capture and direct Powder inspection may be used as diagnostic fallbacks, but they do not grant authority to steer or reap the Captain or Crew.
A sentinel alone never proves completion.

## Captain Start Command

The General can start this assignment with one line:

```text
Read docs/CAPTAIN-ACTIVE-ASSIGNMENT.md and execute Stage 1 only.
```
