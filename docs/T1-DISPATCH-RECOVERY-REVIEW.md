# T1 Dispatch Recovery Assessment and Proposed Solution

## Purpose

This document gives an independent reviewer enough context to evaluate the failed T1 dispatch, the resulting lifecycle inconsistencies, and the proposed recovery sequence without relying on prior conversation history.
It is a review packet, not authorization to implement, merge, install, restart, release claims, or redispatch T1.

## Authoritative Scope

- Project and Powder repository: `t-hub`.
- Canonical checkout: `/home/natkins/projects/tools/t-hub/t-hub-app`.
- Canonical branch: `fix/captain-control-runtime`.
- Canonical head at assessment time: `826c5fe`.
- T1 Powder card: `thub-powder-run-bound-mutations`.
- Failed T1 run: `run-DLXjuRCgLGq2`.
- Failed T1 terminal: `2e4b7c3b`.
- Failed T1 worktree: `/home/natkins/projects/tools/t-hub/t-hub-worktrees/powder-run-bound-mutations`.
- Failed T1 branch: `fix/powder-run-bound-mutations`.

The T1 worktree remains clean at `826c5fe`.
No T1 implementation commit was produced.
T1 and T3 are paused.

## Runtime State Observed on 2026-07-16

The installed executable is `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe`.
The installed version is `0.3.104`.
The installed executable SHA-256 is `95ddaac1ac62fb3874e16379f3d0c2aaa4aef2d6f12316031b6be4771e870148`.
After a manual Windows restart, the application PID was `41108` and the control endpoint was `127.0.0.1:60081`.

The authoritative Powder card is `ready`.
The card claim is null.
Run `run-DLXjuRCgLGq2` is authoritatively `released`.
The T-Hub Crew registry still retains terminal `2e4b7c3b` as `cleanupPending` with its local Powder work state marked `active`.
The tmux session `th_2e4b7c3b` is absent.
Its original pane PID `2342459` is absent.
The stale Crew row is intentionally preserved as regression evidence.

## Verified Incident Sequence

1. The Captain dispatched T1 exactly once through the sanctioned T-Hub `dispatch_crew` lifecycle.
2. T-Hub spawned terminal `2e4b7c3b`, acquired Powder run `run-DLXjuRCgLGq2`, and persisted the Crew binding before Harness launch.
3. The launch-attestation baseline failed with `provider-native process evidence is unreadable` before a Codex Harness began work.
4. The failed rollback retained the Crew binding because T-Hub could not stop the terminal through its normal process-tree lifecycle.
5. Direct inspection proved the terminal contained exactly one idle `zsh` pane, had no child processes, and had a clean worktree.
6. Normal close timed out in the tmux liveness probe.
7. The authorized forced close reached `kill_session_tree`, which timed out after ten seconds.
8. A later explicitly authorized direct command, `tmux -L t-hub kill-session -t th_2e4b7c3b`, succeeded immediately.
9. The tmux session and original pane PID were then conclusively absent.
10. The exact Powder run was released once through the sanctioned Powder CLI.
11. Powder moved the card to `ready`, cleared the claim, and marked the exact run `released`.
12. After the Windows application restart, T-Hub changed the Crew row from `active` to `cleanupPending` but did not remove it.
13. A single authorized post-restart `close_terminal` call returned `outcome: already_gone` and `crewBindingRetained: true`.
14. That cleanup call attempted the normal Powder release path and received `Powder card 'thub-powder-run-bound-mutations' has status 'ready' without the Crew claim`.
15. The cleanup lifecycle treated that authoritative already-released state as failure and retained the stale binding.

No second manual Powder release was attempted.
No worktree was deleted.
No T1 retry or T3 dispatch occurred.

## Separate Lost-Session Observation

The pre-restart snapshot contained unrelated tmux session `th_e0b5708e` with one pane, PID `1062785`, command `zsh`, and cwd `/tmp`.
That session and PID were absent after restart.

Read-only reconciliation found no T-Hub Captain or Crew owner, no Powder card or run binding, no Git worktree or branch, no completion sentinel, no report, and no durable audit or process-history reference for `th_e0b5708e`.
The path `/tmp` is not a Git worktree.

These facts establish an unclassified session loss.
They do not establish that the session was disposable.
The saved snapshot must remain incident evidence, but this unknown should not consume the T1 recovery critical path unless new ownership or work evidence appears.

## Conception of the Problem

The project is blocked by three control-plane recovery defects that compound each other.
The T1 feature itself did not fail because no T1 Harness started.
The authoritative Powder run-bound contracts are deployed and the exact failed run has already been released.

### Defect 1: Cleanup Is Not Idempotent Across Authoritative Powder State

T-Hub cleanup assumes that a retained local Crew binding with local Powder state `active` must still have a releasable current Powder claim.
That assumption is false after an exact run has already been authoritatively released through a sanctioned recovery path.

When Powder reports the same card as `ready`, its claim as null, and the exact expected run as `released`, cleanup should recognize a converged terminal state.
Instead, the current lifecycle treats `ready without the Crew claim` as a release failure and preserves the Crew binding forever.

The correction must be exact-run aware.
It must never treat an arbitrary ready card, a different released run, an unknown response, or a missing card as successful cleanup.

### Defect 2: `kill_session_tree` Shares the Wedged Subprocess Path It Is Supposed to Recover

`kill_session_tree` enumerates pane PIDs, walks a bounded descendant tree, sends `SIGKILL`, and finally runs `tmux kill-session` through one bounded shell subprocess.
During the incident, that internal subprocess exceeded its ten-second timeout even though the target contained only one idle shell and no children.

The equivalent direct tmux kill later completed immediately.
An isolated one-pane reproduction of the tree-kill script completed in approximately `0.01s` when the subprocess path was healthy.

This evidence localizes the incident to the internal WSL or subprocess execution path rather than the idle shell itself.
The recovery path needs an explicit bounded policy for transient subprocess wedges without turning an unknown liveness result into permission to kill a potentially live session.

### Defect 3: Provider-Native Evidence Collapses Transient Probe Failure Into Invalid Evidence

The launch baseline reads one tmux foreground process and bounded `/proc` metadata under a two-second subprocess deadline.
Any timeout, spawn failure, nonzero exit, stderr output, malformed record, or invalid process shape becomes the same credential-safe `UnreadableEvidence` classification.

Failing closed is correct.
Treating a transient subprocess wedge identically to stable malformed or wrong-provider evidence without one bounded recovery opportunity is operationally brittle.

The evidence probe needs a total deadline and narrowly classified transient retry behavior.
Stable malformed evidence, stale identity, wrapper-obscured evidence, conflicting permission flags, and wrong-provider evidence must continue to fail immediately and transactionally roll back.

## Failure Interaction

The dispatch path currently has this failure sequence:

```text
spawn terminal
  -> claim Powder run
  -> persist Crew binding
  -> probe launch baseline
  -> transient evidence probe failure
  -> rollback requests process-tree kill
  -> internal subprocess wedge times out
  -> terminal and Crew binding remain
  -> exact Powder claim requires manual recovery
  -> later authoritative Powder release succeeds
  -> cleanup cannot recognize already-released exact run
  -> cleanupPending row remains and blocks safe redispatch
```

Each individual safeguard is conservative, but the composition has no convergent recovery state.
The design needs an idempotent terminal condition across tmux reality, exact Powder run state, and the durable Crew registry.

## Required Invariants

1. A delayed or stale request for run A must never mutate reclaimed run B.
2. Terminal absence must be proven independently from an ambiguous subprocess timeout.
3. Cleanup may remove a Crew binding only after the terminal is definitively absent and the exact Powder run is terminally reconciled.
4. A ready card with a null claim is insufficient by itself.
5. The exact bound run must also be authoritatively `released` or completed through an explicitly accepted terminal state.
6. Identical cleanup replay must converge without a second remote effect.
7. Changed identity, card, run, repository, operation, or proof data must fail closed.
8. Provider permission attestation must remain authoritative and provider-native.
9. No retry may expand the total operation deadline into multiple full timeout windows.
10. A control-plane failure must leave enough durable evidence for deterministic reconciliation after restart.

## Proposed Seven-Point Solution

### 1. Preserve the Lost-Session Incident Without Keeping It on the Critical Path

Retain the `th_e0b5708e` pre-restart snapshot and the finding that ownership is unknown.
Do not classify it as disposable, recreate it, or mutate unrelated claims.
Resume investigation only if a card, worktree, report, owner, or process-history reference appears.

### 2. Reconcile the Completed Control-Plane Lanes Before New Ownership Begins

The following completed branches and sentinels already exist:

- `fix/codex-permission-launch-attestation` at `3e0ac41` with `codex-permission-launch-attestation.done`.
- `fix/powder-lifecycle-serialization` at `425435d` with `powder-lifecycle-serialization.done`.
- `feat/powder-control-lifecycle` at `56c1b0f` with `powder-control-lifecycle.done`.

Collect their reports, verify their commits and tests, and resolve their active ownership before assigning another `control.rs` owner.
Do not discard or duplicate their completed work.

### 3. Create One Recovery Card and Assign Exactly One Recovery Crew

All three fixes converge on `apps/desktop/src-tauri/src/control.rs`.
Parallel implementation would create ambiguous ownership and unsafe merge pressure.

Use one authoritative Powder recovery card, one isolated worktree, and one durable Crew.
Assign exclusive ownership of the required portions of:

- `apps/desktop/src-tauri/src/control.rs`
- `apps/desktop/src-tauri/src/tmux.rs`
- `apps/desktop/src-tauri/src/harness/mod.rs`
- `apps/desktop/src-tauri/src/powder.rs` only if the exact-state read contract cannot be expressed through the existing client
- directly coupled focused control, process, CLI, and MCP tests

The Captain remains the integration owner and does not implement alongside the Crew.

### 4. Produce Three Separate Logical Commits

#### Commit A: Exact-Run Cleanup Idempotency

Validate the canonical repository, card, exact expected run, and authoritative Powder response.
Treat card `ready`, claim null, and exact expected run `released` as a successful cleanup terminal state.
Return an explicit normalized result that distinguishes `released_now` from `already_released`.
Remove or terminally reconcile the Crew binding only after terminal absence is definitive.
Fail closed for card/run mismatch, stale or reclaimed runs, unknown state, authorization failure, malformed response, or ambiguous timeout.

#### Commit B: Bounded `kill_session_tree` Wedge Recovery

Reproduce the internal WSL subprocess timeout with a one-pane idle-shell fixture.
Separate definitive tmux absence, confirmed liveness, and subprocess unknown.
Add one bounded recovery strategy under a single total deadline.
Do not allow an unknown probe to become an unreviewed live-session kill.
Ensure registry and Powder cleanup execute only after terminal reality is conclusive.

#### Commit C: Bounded Provider-Evidence Recovery

Separate transient probe timeout or spawn failure from stable malformed or security-invalid evidence.
Allow only a narrowly bounded transient retry under one total deadline.
Require authoritative post-launch provider-native identity and permission evidence before dispatch success.
Keep malformed, stale, wrapper-obscured, conflicting-mode, and wrong-provider evidence fail-closed.
Preserve transactional rollback of the terminal, exact Powder run, and Crew binding.

### 5. Require Deterministic E2E Reproduction, Full Gates, and Independent Review

The implementation must prove the incident classes at process boundaries rather than only testing helper functions.

The minimum deterministic matrix is:

- Active exact run releases once and cleanup removes the binding.
- Already-released exact run plus ready/null-claim card replays cleanup without another remote effect.
- Different released run does not satisfy cleanup.
- Reclaimed run B cannot be mutated by cleanup for run A.
- Response loss after authoritative release converges through read-and-verify recovery.
- Healthy one-pane tree kill succeeds.
- Delayed subprocess stays within one total deadline.
- First transient subprocess timeout followed by healthy recovery converges.
- Sustained subprocess timeout leaves evidence and does not kill a possibly live session.
- Already-gone terminal is idempotent.
- First provider probe transiently fails and a bounded retry succeeds.
- Stable malformed, stale, wrapper, wrong-provider, and conflicting permission evidence remain rejected.
- Rollback after launch failure leaves no live Harness, active Powder claim, or unreconciled Crew row.
- Restart reconstruction reaches the same terminal state.

Required gates include:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cd apps/desktop/src-tauri && ../scripts/workspace_gate.sh
git diff --check
```

Run all focused control, tmux, harness, Powder, CLI, MCP, and process-contract suites affected by the changes.
Record exact commands and outcomes.
Require independent security-sensitive control-plane review before integration.

### 6. Integrate, Install, and Prove Recovery With a Dedicated Canary

After review, integrate only with explicit General authorization.
Build and install only with separate explicit authorization.
Take the full runtime, registry, tmux, worktree, and Powder snapshot before the one controlled restart.

Do not use T1 as the first production canary.
Use a dedicated disposable acceptance card and isolated worktree to prove dispatch, permission attestation, failure rollback, exact-run cleanup replay, restart recovery, and absence of stale bindings.
Preserve all unrelated sessions and claims.

### 7. Redispatch T1 Exactly Once After Recovery Passes

Only after the dedicated canary passes should T1 return to `ready` dispatch eligibility.
Dispatch exactly one T1 Crew from the verified canonical head in the preserved isolated worktree or a newly authorized replacement.
Require exact-run work logs, focused and full gates, clean logical commits, and independent review.
Keep T3 blocked until T1 passes and its evidence is accepted.

## Suggested Recovery Card Acceptance Criteria

1. Cleanup of an absent terminal converges when the exact bound run is already authoritatively released and its card is ready with a null claim.
2. Cleanup never accepts another run's released state, a reclaimed card, an ambiguous response, or card-only state.
3. A deterministic single-idle-shell fixture reproduces the internal `kill_session_tree` timeout and proves bounded recovery.
4. Provider evidence retries only transient timeout or spawn failures under one total deadline.
5. Stable security-invalid evidence never becomes retry-success.
6. Failed dispatch rollback leaves no live terminal, active exact claim, or active/recoverable Crew binding.
7. Identical cleanup replay has no second remote side effect and returns the same normalized terminal result.
8. Restart recovery reaches the same registry, tmux, and Powder state.
9. Focused suites, full workspace gates, formatting, Clippy, and diff checks pass.
10. Independent control-plane review finds no unresolved high- or medium-severity defect.

## Reviewer Questions

1. Is exact card, exact run, ready status, null claim, and released run sufficient for idempotent cleanup, or is an additional Powder receipt required?
2. Which layer should normalize `already_released`: the Powder client, the control lifecycle, or both with one typed contract?
3. Can the `kill_session_tree` recovery remain fully inside the tmux abstraction, or does it require a broader bounded-exec or WSL transport repair?
4. What retryable error taxonomy cleanly separates transient evidence transport failure from malformed or security-invalid evidence?
5. Does the proposed single total deadline cover baseline evidence, launch, post-launch evidence, and rollback, or should rollback receive a separate emergency bound?
6. Which existing completed branch should be the semantic base for the recovery lane, and which changes should be translated rather than merged?
7. Are any existing tests coupled to the live `t-hub` tmux socket or operator Powder database?
8. Does the canary prove no second Powder release mutation occurs after an already-released state?
9. Is preserving the `cleanupPending` row sufficient regression evidence, or should a credential-safe fixture be captured before implementation?
10. Are there any paths that still infer terminal death from an `Unknown` liveness result?

## Constraints

- Do not redispatch or reclaim T1 during review or recovery implementation.
- Do not start T3.
- Do not manually edit `captains.json`.
- Do not retry release for `run-DLXjuRCgLGq2`.
- Do not delete the T1 worktree.
- Do not remove the `cleanupPending` row before a deterministic fixture captures the defect.
- Do not install, restart, push, publish, merge, or clean worktrees without separate General authorization.
- Do not modify Powder source for a T-Hub-specific workaround.
- Preserve `.lavish/`, `CLAUDE.md`, `docs/DECK-AGENTS-DESIGN.md`, generated changelogs, and unrelated user changes.

## Requested Independent Review Output

The reviewer should return:

1. A verdict of `APPROVE`, `APPROVE-WITH-CHANGES`, or `REJECT`.
2. Findings ordered by severity with exact file and symbol references.
3. Any missing invariant or incident reproduction.
4. A recommended ownership boundary and base commit for the isolated recovery lane.
5. A corrected commit order if the proposed three-commit sequence is unsafe.
6. The exact minimum test and canary gates required before T1 can be retried.

The reviewer must not implement, merge, install, restart, release claims, or mutate the operator Powder database as part of this review.
