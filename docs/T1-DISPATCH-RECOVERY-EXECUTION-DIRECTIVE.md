# T1 Dispatch Recovery Execution Directive

> Historical recovery directive: Powder and Crew references below describe the
> retired dispatch implementation and are retained for compatibility history.
> Current work uses the durable agent-session model.

## Purpose

This directive converts the T1 dispatch incident review into an authorized development sequence that can make progress without using the unreliable installed T-Hub dispatch path to repair itself.
It synthesizes the incident account in [T1-DISPATCH-RECOVERY-REVIEW.md](./T1-DISPATCH-RECOVERY-REVIEW.md) with the independent review verdict of `APPROVE-WITH-CHANGES`.
It is the zero-context prompt and execution contract for the T-Hub Captain.

This directive authorizes source investigation, isolated implementation, tests, commits, and independent review within the ownership boundary below.
It does not authorize a production T-Hub install, production restart, push, publish, Powder mutation, manual registry edit, claim release, production Crew dispatch, or worktree cleanup.

## Desired Outcome

Produce one reviewed source candidate that:

1. Prevents local launch-preflight failure from creating a Powder claim or durable Crew binding.
2. Reconciles an already-released exact Powder run without issuing a second release mutation.
3. Distinguishes transaction-owned pre-launch teardown from destructive post-launch terminal cleanup.
4. Handles transient Windows-to-WSL subprocess degradation through one typed, bounded policy.
5. Implements T1 run-bound mutations and T3 ambiguous-operation recovery after the recovery foundation is stable.
6. Passes focused tests, full repository gates, independent review, and an isolated side-by-side canary before any production installation is considered.

## Verified Starting State

- Canonical checkout: `/home/natkins/projects/tools/t-hub/t-hub-app`.
- Canonical branch: `fix/captain-control-runtime`.
- Canonical source before this directive: `826c5fe`.
- Incident review packet commit: `6749d26`.
- Recovery implementation base: `6749d26` unless canonical advances through a separately reviewed, non-conflicting documentation-only change.
- T30 is integrated.
- T2 is integrated through translated commits `59a2cb5` and `7ea4dc5`.
- Powder P1 through P4 are deployed and independently verified.
- T1 card `thub-powder-run-bound-mutations` is `ready` with a null claim.
- Failed run `run-DLXjuRCgLGq2` is authoritatively `released`.
- Failed terminal `2e4b7c3b` and tmux session `th_2e4b7c3b` are absent.
- The stale Crew row remains `cleanupPending` and is preserved as incident evidence.
- The T1 worktree is clean at `826c5fe` and contains no T1 implementation.
- T3 has not started.

## Governing Decision

Do not dispatch a new recovery Crew through the installed production T-Hub runtime.
The recovery owner must work directly in a fresh isolated Git worktree created from the approved canonical base.

The Captain may take direct implementation ownership in that worktree.
If another direct coding session is assigned, exactly one session owns the shared files and the Captain remains review and integration coordinator only.
Do not create concurrent editing lanes for `control.rs`, `tmux.rs`, `harness/mod.rs`, `bounded_exec.rs`, or `powder.rs`.

The recovery, T1, and T3 source work should proceed sequentially under the same exclusive shared-surface owner.
Do not perform an intermediate production installation merely to bootstrap later source development.

## Protected State

Preserve all of the following:

- `.lavish/`.
- `CLAUDE.md`.
- `docs/DECK-AGENTS-DESIGN.md`.
- Generated changelogs.
- The existing T1 worktree.
- The `cleanupPending` Crew row until a sanitized deterministic fixture captures the defect.
- The saved `th_e0b5708e` incident snapshot.
- Every unrelated tmux session, registry row, Powder card, Powder claim, worktree, and branch.

Do not manually edit `captains.json`.
Do not retry release for `run-DLXjuRCgLGq2`.
Do not reclaim or redispatch T1 during recovery development.

## Source Ownership Boundary

The recovery owner has exclusive ownership of the required portions of:

- `apps/desktop/src-tauri/src/control.rs`.
- `apps/desktop/src-tauri/src/tmux.rs`.
- `apps/desktop/src-tauri/src/harness/mod.rs`.
- `apps/desktop/src-tauri/src/bounded_exec.rs`.
- `apps/desktop/src-tauri/src/powder.rs` only for typed evidence parsing or client contract support.
- Directly coupled Rust, CLI, MCP, process-contract, and integration tests.
- Recovery-specific documentation and credential-safe fixtures.

Do not broaden the lane into unrelated UI, layout, history, messaging, worktree deletion, or Powder server development.

## Branch Reconciliation Before Implementation

Treat T2 as already integrated.
Do not merge or translate `425435d` again.

Do not merge `feat/powder-control-lifecycle` or `56c1b0f` wholesale.
That branch contains large pre-stabilization Powder client and lifecycle changes.
It may be mined only for a specific reviewed test or small source fragment whose semantics match the deployed P1 through P4 contracts.

Inspect `3e0ac41` as a separate launch-policy prerequisite.
If its `CREW_DEFAULT_PERMISSION` semantics and enforcement are still absent from canonical, translate that logical change as a standalone reviewed prerequisite commit before recovery work.
Do not merge the divergent branch.
Record the exact decision and evidence in the first checkpoint.

## Required Implementation Sequence

Each item below is one logical, independently verified commit.
Do not accumulate several items into one unreviewable change.

### Commit 0 - Reconcile the Launch Policy if Required

Translate only the still-missing semantic change from `3e0ac41`.
Keep the change separate from recovery behavior.
Run its focused Harness and control tests before committing.

If canonical already contains equivalent behavior, record that finding and create no empty commit.

### Commit 1 - Preflight Crew Dispatch Before Remote Effects

Correct the dispatch transaction order.

The required order is:

1. Validate the Project, checkout, card identity, repository, and dispatch ownership.
2. Spawn one transaction-owned terminal.
3. Obtain a valid idle-shell provider-process baseline.
4. Claim the exact Powder card and run.
5. Persist the exact Crew, terminal, worktree, branch, Harness, card, and run binding.
6. Launch the provider Harness.
7. Obtain and validate post-launch provider-native permission evidence.
8. Persist the launch attestation and report dispatch success.

A baseline probe failure must cause:

- Zero Powder claim requests.
- Zero Powder release requests.
- Zero durable Crew bindings.
- One bounded teardown attempt for the transaction-owned terminal.
- A credential-safe, typed failure result.

Do not make a remote Powder effect before the local transport and baseline process boundary pass.

### Commit 2 - Exact-Run Cleanup Idempotency

Extend authoritative Powder reality beyond only `Active` and `Completed`.
Add an exact terminal release state whose acceptance requires:

- The canonical repository matches the durable Project binding.
- The card identity matches the durable Crew binding.
- The run identity matches the durable Crew binding.
- The exact run state is `released`.
- The card is `ready`.
- The card claim is null.
- Terminal absence has been established under the applicable terminal policy.

Return normalized outcomes that distinguish at least:

- `released_now`.
- `already_released`.
- `already_completed`.
- `recovery_pending`.
- `identity_conflict`.

The Powder client may parse typed card and run evidence.
The control lifecycle owns the decision that exact released evidence means successful cleanup convergence.

An `already_released` replay must not issue another Powder release request.
A different released run, a reclaimed card, an unknown response, a missing card, a repository mismatch, or malformed evidence must fail closed.

Remove the Crew binding only after terminal reality and exact Powder state both converge.

### Commit 3 - Stage-Aware Terminal Rollback

Separate terminal cleanup into at least these contexts:

1. A transaction-owned terminal before any Harness launch command was sent.
2. A terminal after a Harness launch command may have run.
3. An existing terminal outside the current dispatch transaction.

For a transaction-owned pre-launch terminal, use the narrowest safe teardown because no coding Harness was launched.
Do not require a descendant-tree sweep when the transaction can prove that no Harness launch command was sent.

For post-launch or existing terminals, preserve process-tree cleanup and fail closed when destructive authority is not established.
Two `Unknown` liveness probes must not automatically authorize killing a possibly live unrelated or post-launch session.

Replace the current general `Unknown` plus `Unknown` force-close inference with a typed recovery result.
Only explicit transaction ownership or independent conclusive process evidence may authorize teardown while the normal liveness path is degraded.

Registry removal and Powder cleanup must occur only after the relevant terminal policy reaches a conclusive state.

### Commit 4 - Shared Bounded Windows-to-WSL Execution Policy

Introduce or extend one shared execution layer for Windows-to-WSL subprocess calls used by lifecycle-critical paths.

The policy must distinguish:

- Spawn failure.
- Transport timeout.
- Process nonzero exit.
- Definitive target absence.
- Malformed output.
- Oversized output.
- Stale evidence.
- Wrong provider.
- Wrapper-obscured evidence.
- Missing, wrong, conflicting, or malformed permission evidence.

Only spawn failure and transport timeout are candidates for one transient retry.
A nonzero process exit is retryable only when a narrowly identified transport condition proves that the command itself did not produce a stable semantic result.
Malformed, stale, wrong-provider, wrapper-obscured, or permission-invalid evidence must fail immediately.

Use one total deadline for a normal probe operation.
Do not create two full timeout windows by retrying with a fresh deadline.

Rollback receives its own small emergency recovery budget after the primary dispatch deadline.
This emergency budget must not permit a new claim, new identity, changed payload, or broader destructive action.

Review all lifecycle-critical callers of `bounded_exec::output_with_timeout`.
The success path currently joins output reader threads without a deadline.
Make the no-unbounded-park guarantee true even when a grandchild inherits an output pipe after the direct child exits.

Prevent background usage, history, home-discovery, or similar probes from consuming all useful WSL subprocess capacity while a lifecycle operation needs recovery.
Use bounded admission, reserved lifecycle capacity, degradation backoff, or an equivalently robust policy without creating one global head-of-line-blocking mutex.

### Commit 5 and Later - Implement T1

After Commits 1 through 4 pass focused tests and review, implement T1 run-bound mutations directly in the same isolated source lane.

T1 must:

- Send the exact repository, card, expected run, stable operation identity, criterion identities, reviewer identity, review proof, and completion proof required by deployed Powder.
- Persist the durable operation intent before mutation.
- Validate every authoritative receipt against every requested identity.
- Reject stale runs and changed-payload replays.
- Keep card-only completion unavailable.
- Expose the same contract through backend, CLI, MCP, and any enabled UI surface.
- Remain disabled when Powder capability or version support is insufficient.

Commit each verified logical T1 change separately.

### Following T1 - Implement T3

Implement T3 under the same exclusive shared-surface owner after T1 is implementation-complete.

T3 must:

- Persist operation identity, payload digest, repository, card, expected run, mutation kind, and Crew intent before dispatch.
- Reconcile timeout, partial response, EOF, connection loss, and restart through Powder operation status.
- Reissue only when the versioned Powder contract permits it, using the same operation identity and identical payload.
- Adopt Powder's normalized authoritative record.
- Reject stale-run evidence.
- Surface committed, pending, recovered, rejected, stale, conflict, expired, unsupported, malformed, and timeout states consistently through backend, CLI, and MCP.

Do not install T1 before T3 passes.

## Deterministic Test Matrix

### Dispatch Preflight

- A healthy idle-shell baseline permits the transaction to proceed to one Powder claim.
- A transient baseline transport timeout followed by bounded recovery succeeds within one total deadline.
- A sustained baseline transport timeout creates no Powder claim, no release, and no Crew binding.
- Stable malformed baseline evidence fails immediately with no Powder effect.
- A transaction-owned terminal is absent after pre-launch rollback.

### Exact-Run Cleanup

- An active exact run releases once and cleanup removes the Crew binding.
- An already-released exact run plus ready card and null claim returns `already_released` without another release request.
- Response loss after authoritative release converges through exact read and verification.
- A different released run does not satisfy cleanup.
- Reclaimed run B cannot be mutated or cleaned by a request for run A.
- Card-only ready state is insufficient without exact released-run evidence.
- Identical cleanup replay has no additional remote effect.
- Changed repository, card, run, or operation identity fails closed.

### Terminal Recovery

- Healthy transaction-owned pre-launch teardown succeeds.
- Healthy post-launch process-tree teardown kills only the owned tree.
- Already-gone terminal cleanup is idempotent.
- Sustained subprocess timeout preserves an existing or post-launch terminal as recovery pending.
- Two `Unknown` probes do not authorize an unrelated live-session kill.
- Conclusive target absence permits registry and Powder reconciliation.
- Restart reconstruction reaches the same terminal recovery state.

### Shared WSL Execution

- Fast subprocess output is preserved.
- A hung subprocess returns within its bound.
- A timed-out direct child with a pipe-inheriting grandchild cannot park the caller.
- A successfully exited direct child with a pipe-inheriting grandchild cannot park the caller.
- Background probes back off or yield capacity during transport degradation.
- A lifecycle operation retains bounded recovery capacity during background probe load.
- Retryable transport errors and non-retryable semantic or security errors remain distinct.

### T1 and T3

- Run A completes only while it remains current.
- A delayed Run A mutation cannot change reclaimed Run B.
- Identical operation replay returns one authoritative result.
- Changed payload under the same operation identity conflicts.
- Work-log and completion response loss recover without duplicate effects.
- Restart during pending recovery resumes deterministically.
- Normalized Powder evidence is adopted rather than reconstructed client-side.
- Unsupported versions and malformed, oversized, unauthenticated, or stale responses remain bounded and fail closed.

## Verification Gates

Run focused tests after every logical commit.
Record the exact command, result, duration, and any skipped or ignored test.

Before declaring the combined source candidate ready, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/desktop/src-tauri && ../scripts/workspace_gate.sh
git diff --check
```

Also run all focused control, tmux, Harness, bounded-exec, Powder client, CLI, MCP, and process-contract tests affected by the changes.
Do not accept a skipped process test without recording why it skipped and identifying the real-process gate that covers it.

Require independent review after the recovery foundation and again after combined T1 and T3 integration.
The final reviewer must inspect the T30, T2, recovery, T1, T3, and lifecycle interaction rather than only reviewing each commit in isolation.

## Side-by-Side Canary Before Production

Do not make the production runtime the first environment that executes the recovery candidate.

Build and run the supported `devbuild` variant with isolated state, including:

- A dedicated `T_HUB_TMUX_SOCKET`.
- A dedicated `T_HUB_CONTROL_FILE`.
- A dedicated `T_HUB_CAPTAINS_FILE`.
- Dedicated diagnostic, database, layout, and server-key paths where supported.
- A disposable Powder acceptance card and isolated worktree only after the General separately authorizes that Powder mutation.

Follow [DEV-BUILD.md](./DEV-BUILD.md) and verify that no production tmux session, registry row, database, control file, or worktree is touched.

The side-by-side canary must prove:

1. Healthy dispatch and provider-native permission attestation.
2. Baseline probe failure before claim with zero Powder effects.
3. Post-claim failure rollback with one exact release.
4. Already-released cleanup replay with zero additional release effects.
5. Restart recovery on the isolated registry.
6. No stale Crew binding after authoritative convergence.
7. Background WSL probe load does not starve lifecycle recovery.
8. T1 work-log, criterion review, completion, and T3 lost-response recovery use exact operation and run identity.

Tear down only the isolated canary state after its evidence is captured and reviewed.

## Production Installation Gate

Production installation remains separately authorized.
Do not install or restart merely because source tests pass.

Before requesting installation authorization, report:

- Exact candidate commit.
- Complete commit list and ownership.
- Focused and full gate results.
- Independent-review verdicts and resolved findings.
- Side-by-side canary evidence.
- Production snapshot and rollback plan.
- Exact dedicated production canary scenario.
- Confirmation that no active production Crew is in an ambiguous mutation or cleanup transition.

After one authorized production installation and restart, use a dedicated disposable acceptance card before touching T1.
Verify the complete session and PID roster against the pre-installation snapshot.
Then allow the fixed reconciler to process the preserved `cleanupPending` evidence and report the exact result.

Do not redispatch duplicate T1 implementation work.
Close or complete T1 only through the accepted implementation and runtime evidence path after the dedicated canary passes.

## Stop Conditions

Stop source mutation and report `DECISION-NEEDED` when:

- Canonical advanced with a conflicting shared-file owner.
- The approved base cannot be established cleanly.
- A test touches the production tmux socket, operator Powder card, production registry, or production control file.
- Exact terminal ownership cannot be established before a destructive test.
- Powder identity, run state, or mutation outcome is ambiguous.
- A changed-payload retry or second remote effect is observed.
- Credentials, tokens, prompts, or provider arguments appear in logs or fixtures.
- A deterministic source gate fails and the failure cannot be isolated to the current logical change.
- The recovery design requires modifying Powder server behavior for a T-Hub-specific workaround.

Do not stop source development merely because the production registry still contains the preserved stale row, the old released run exists, the old terminal is absent, or the installed runtime does not yet contain the source candidate.

## Required Checkpoints

### Checkpoint 1 - Base and Ownership

Report:

- Fresh worktree path and branch.
- Exact base commit.
- Tracked cleanliness and protected untracked files.
- Exclusive shared-file owner.
- `3e0ac41` translation decision.
- Confirmation that T2 is not being reintegrated.
- Confirmation that no production T-Hub dispatch or Powder mutation occurred.

### Checkpoint 2 - Recovery Foundation

Report:

- Commits 1 through 4.
- Exact source symbols changed.
- Focused tests and deterministic incident reproductions.
- Independent-review verdict.
- Any unresolved finding.

### Checkpoint 3 - T1 and T3 Source Candidate

Report:

- T1 and T3 commit list.
- Operation and run identity contracts.
- Focused and full gate results.
- Combined independent-review verdict.
- Exact candidate commit.

### Checkpoint 4 - Side-by-Side Canary

Report:

- Isolated runtime paths and socket names without credentials.
- Disposable card and run identities if separately authorized.
- Request counts proving idempotency.
- Terminal, registry, and Powder state before and after each scenario.
- Confirmation that production state was untouched.

End at `DECISION-NEEDED` for production installation authorization.

## Prompt to Send to the T-Hub Captain

> Read `docs/T1-DISPATCH-RECOVERY-EXECUTION-DIRECTIVE.md` completely and treat it as the governing execution contract.
> Take direct exclusive source ownership of the T1 dispatch recovery lane without dispatching a production T-Hub Crew.
> Start from the approved canonical base, create a fresh isolated Git worktree, reconcile `3e0ac41` exactly as directed, and implement the recovery commits in order.
> Continue into T1 and T3 only after the recovery foundation passes focused tests and independent review.
> Commit every verified logical change separately.
> Use only isolated tests and a side-by-side devbuild canary before requesting production installation authorization.
> Do not mutate Powder, install, restart, push, publish, manually edit the registry, redispatch T1, or clean existing worktrees without separate authorization.
> Stop and report `DECISION-NEEDED` only at the directive's stated stop conditions or when the side-by-side candidate is ready for production installation authorization.
