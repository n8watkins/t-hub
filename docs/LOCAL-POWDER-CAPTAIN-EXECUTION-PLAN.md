# Local Powder Captain Execution Plan

## Purpose

This document is the zero-context execution plan for the existing T-Hub Captain to finish T-Hub's local Powder integration safely.
It does not create another Captain and does not authorize work by itself.
The General starts this plan by directing the existing Captain to read this document and execute the specifically authorized stage.

The target is a local-first installation in which the Windows T-Hub application uses the protected `n8desktop-wsl` profile to reach the user's Powder service.
DigitalOcean, Cloudflare hosting, a GitHub App, Sanctum, and public deployment are outside this plan.

## Authority and Required Reading

Before acting, the Captain must recover its durable T-Hub identity and read these documents in order:

1. [POWDER-INTEGRATION.md](./POWDER-INTEGRATION.md) defines the ownership boundary between T-Hub and Powder.
2. [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md) defines the authoritative roadmap and the T1 through T3 dependency order.
3. [CAPTAIN-POWDER-HANDOFF.md](./CAPTAIN-POWDER-HANDOFF.md) contains the latest recorded runtime, Crew, review, and integration evidence.
4. [CAPTAIN-ACTIVE-ASSIGNMENT.md](./CAPTAIN-ACTIVE-ASSIGNMENT.md) records the earlier local acceptance attempt and its safety constraints.
5. [STATUS-MODEL.md](./STATUS-MODEL.md) defines lifecycle status authority.
6. [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md) defines shared checkout ownership and cleanup rules.

This plan supersedes only the old Stage 1 instruction in `CAPTAIN-ACTIVE-ASSIGNMENT.md` after the General explicitly starts a stage from this document.
Historical evidence in that file remains valid.

## Current Planning Baseline

The Captain must treat this section as a planning snapshot rather than live authority.
It must reread the installed T-Hub registry, the exact Crew terminals, the worktrees, the completion sentinels, and the Powder board before changing anything.

The expected starting facts are:

- Powder runs locally in WSL and is exposed privately to Windows through the protected `n8desktop-wsl` profile.
- The canonical Powder repository is the user's fork, while `misty-step/powder` remains an optional fetch-only upstream.
- Powder P1 through P4 are implemented, tested, deployed locally at commit `8217c29`, and backed by schema version 18.
- Project `project-e28c0579-4e78-4de1-b225-d69aab93c143` binds T-Hub repository `t-hub` to `n8desktop-wsl`.
- T2 card `thub-powder-lifecycle-serialization` has implementation and independent-review evidence in its isolated worktree.
- T30 card `thub-control-client-deadline` has implementation and independent-review evidence, but its live integration state and completion sentinel must be reconciled.
- T1 card `thub-powder-run-bound-mutations` must remain undispatched until its Powder dependencies, ownership window, and installed client capability are verified.
- T3 card `thub-powder-evidence-recovery` must remain undispatched until T2, the Powder recovery contract, and exclusive integration ownership are verified.
- Card `thub-local-acceptance` is the dedicated real local acceptance card.
- The installed Windows runtime may be older than the reviewed source and must not be assumed to contain T2, T30, T1, or T3.

The Captain must record any difference between this snapshot and live state in its checkpoint before proceeding.
Live Powder state wins over a document, sentinel, terminal transcript, or local registry cache.

## Non-Negotiable Safety Rules

1. The Captain must use the `$captain` skill and recover its durable manifest before dispatching or supervising Crew.
2. The Captain must use one exclusive integration owner for `powder.rs`, `control.rs`, the CLI Powder surface, the MCP Powder surface, and directly coupled tests during T1 and T3.
3. The Captain must not overlap T1 or T3 with another Crew that owns any shared control-plane file.
4. The Captain must preserve `.lavish/`, `CLAUDE.md`, `docs/DECK-AGENTS-DESIGN.md`, and every other protected user artifact.
5. The Captain must use isolated branches and worktrees for implementation and review.
6. The Captain must commit every verified logical change separately and must never add an agent co-author.
7. The Captain must not merge, install, restart the production application, reap terminals, release claims, delete worktrees, push, publish, or deploy unless the General authorizes that exact stage.
8. The Captain must not place a raw Powder API key in a command line, log, work log, report, profile file, or terminal transcript.
9. Protected Powder profiles must resolve credentials through their configured command.
10. The Captain must not implement card-only completion, a read-then-write run check, or any local emulation of a missing Powder precondition.
11. Every mutating request must use a stable operation identity and must reconcile ambiguous outcomes before retrying.
12. A timeout, partial response, malformed response, authentication failure, version mismatch, stale run, conflict, or exhausted recovery budget must fail closed.
13. A completion sentinel is supporting evidence only and never overrides Powder, the Crew binding, Git state, or the final report.
14. Independent review must use a separate context and must reproduce the important failure modes rather than trusting the implementation report.
15. Cleanup occurs only after authoritative completion or release is proved and all owned resources agree.

## Dependency Order

The Captain must execute the work in this order:

```text
Live reconciliation
    -> T30 integration and transport proof
    -> T2 integration and lifecycle proof
    -> Candidate build and authorized installation
    -> Baseline local acceptance
    -> T1 run-bound mutations
    -> T3 ambiguous-operation recovery
    -> Combined review and full gates
    -> Authorized final installation
    -> Final local acceptance and cleanup
```

The Captain must stop at the end of each stage and report its evidence unless the General explicitly authorized multiple consecutive stages.

## Stage 0 - Reconcile Live State

### Entrance

The General authorizes the Captain to inspect the current local integration state.
This stage is read-only except for the Captain's own durable checkpoint.

### Actions

1. Recover the Captain manifest and confirm the exact Project, Assignment, ship, terminal, and repository identities.
2. Read the T-Hub Powder board through the protected `n8desktop-wsl` profile.
3. Record the status, claim owner, run, criteria, proof plan, and latest work logs for T30, T2, T1, T3, and `thub-local-acceptance`.
4. Inspect the T30 and T2 terminal processes, worktree branches, exact commit tips, tracked cleanliness, protected artifacts, and completion sentinels.
5. Compare the canonical T-Hub head with each candidate branch using `git log`, `git diff`, and `git range-diff` where commit translation exists.
6. Confirm the installed T-Hub version, executable hash, process identity, control endpoint, registry path, and current terminal count.
7. Confirm that native Windows control access and WSL client access are each classified as working, failing, or untested.
8. Confirm the local Powder health route, readiness route, schema version, deployed commit, repository binding, and protected credential-command health without printing credentials.
9. Write a checkpoint that distinguishes authoritative facts, inferred facts, missing evidence, and decisions requiring the General.

### Exit Gate

Stage 0 passes only when every active claim and Crew has one unambiguous owner and the Captain can name the next exclusive integration lane.
If a mutation may have succeeded without a confirmed response, the Captain must classify the stage as blocked and reconcile it by operation identity before any retry.

## Stage 1 - Integrate T30 and Prove the Control Transport

T30 prevents control clients from hanging indefinitely on stale or silent endpoints.
It is the first integration gate because every later Powder acceptance depends on a trustworthy bounded control path.

### Entrance

- T30 has a tracked-clean implementation branch and independent review with no unresolved finding.
- No other active Crew owns the same CLI, desktop, or MCP control-client files.
- The General authorizes integration but not installation unless installation is stated separately.

### Required Behavior

The integrated clients must:

1. Enforce one bounded overall deadline across inherited-endpoint use, endpoint discovery, replacement connection, response reading, mutation reconciliation, and error reporting.
2. Recover promptly when an inherited endpoint accepts a connection but never returns a response.
3. Adopt a replacement endpoint without overwriting a newer concurrent endpoint rotation.
4. Bound response frames before parsing and scan incoming data without repeated unbounded rescans.
5. Return stable, credential-safe structured timeout and protocol errors.
6. Reconcile an ambiguous idempotent mutation after partial response loss without issuing more than one permitted same-request-ID reissue.
7. Never retry a non-idempotent mutation after an ambiguous write.
8. Preserve clean JSON stdout and stable process exit behavior for CLI consumers.
9. Preserve MCP JSON-RPC framing and error shape.

### Verification

The Captain must rerun focused tests for all three clients, including:

- A refused inherited endpoint.
- A connected but silent inherited endpoint.
- A healthy slow response that remains within the command budget.
- Endpoint replacement during recovery.
- A response exactly at the frame limit.
- An oversized, malformed, or unterminated response.
- A partial-frame trickle that reaches the absolute deadline.
- A partial idempotent mutation response followed by completed, failed, unknown, and unavailable operation status.
- A concurrent endpoint rotation that rejects stale adoption.
- Clean stdout, bounded stderr, bounded output, stable exit code, and credential non-disclosure.

The Captain must also reproduce the real installed symptom before installation and prove the repaired behavior after installation.
Native Windows direct control and WSL `th` and `t-hub-mcp` access must be recorded separately.

### Exit Gate

Stage 1 passes when T30 is integrated into the candidate branch, focused and workspace gates pass, independent review remains satisfied after integration, and the exact candidate commit is recorded.
Installed-runtime proof belongs to Stage 3 unless the General authorized installation as part of Stage 1.

## Stage 2 - Integrate T2 and Prove Lifecycle Serialization

T2 prevents a delayed heartbeat, renewal, release, completion, close, cleanup, or reconciler action from crossing another lifecycle transition for the same Crew.

### Entrance

- T2 has a tracked-clean implementation branch, a completion report, and independent review with no unresolved finding.
- T30 integration has established a bounded control path.
- No active Crew owns `apps/desktop/src-tauri/src/control.rs` or another shared T2 integration file.

### Actions

1. Verify the T2 implementation and review commits against the recorded handoff.
2. Integrate T2 after T30 in a dedicated integration commit or a traceable commit series.
3. Resolve conflicts by preserving the single per-Crew lifecycle guard and the single bounded control-client deadline.
4. Rerun deterministic barrier tests after conflict resolution.
5. Verify that unrelated Crew can still progress concurrently.
6. Verify that cleanup revalidates lifecycle state while holding the same guard used by renewal and heartbeat.
7. Verify restart and reconciler behavior from persisted Crew bindings.

### Required Race Matrix

| Race | Required result |
| --- | --- |
| Completion versus heartbeat | Heartbeat cannot renew after completion begins or commits. |
| Cleanup versus queued renewal | Renewal revalidates state inside the guard and does not reach Powder. |
| Release versus completion | Only the authorized terminal transition reaches Powder. |
| Close versus reconciler | The loser observes the updated lifecycle state and does not make a stale mutation. |
| Crew A versus Crew B | Unrelated Crew are not globally serialized. |
| Restart during pending cleanup | Recovery resumes from durable state without duplicating effects. |

### Exit Gate

Stage 2 passes when T2 and T30 coexist on one candidate head, the complete race matrix passes deterministically, and independent integration review reports no unresolved correctness issue.

## Stage 3 - Build, Install, and Prove the Candidate

### Entrance

- Stages 1 and 2 have one reviewed candidate commit.
- The repository gates pass from a tracked-clean worktree.
- The General explicitly authorizes production installation and application restart.

### Pre-Installation Snapshot

The Captain must record:

- The installed T-Hub version and executable hash.
- The candidate Git SHA and declared versions.
- The application PID, control endpoint, and registry files.
- The exact live Captain and Crew terminal IDs and pane PIDs.
- The Powder card and run bound to every Powder-backed Crew.
- The current terminal count and any fleet watches.
- The rollback installer or exact previous executable.

The Captain must not install while an active Crew is in an ambiguous mutation or cleanup state.
If a restart could interrupt active work, the Captain must stop and request a narrower maintenance window.

### Installation Proof

1. Build the exact candidate from a tracked-clean worktree.
2. Verify that the produced binary identifies the intended version and source boundary.
3. Install only the artifact built from the recorded candidate.
4. Restart the application once.
5. Confirm that every pre-existing tmux session and terminal identity survived as expected.
6. Confirm registry integrity and durable Captain and Crew bindings.
7. Confirm native Windows control operations.
8. Confirm WSL CLI operations through `th`.
9. Confirm WSL MCP operations through `t-hub-mcp`.
10. Confirm Powder health through the protected profile without exposing credentials.
11. Run a bounded no-mutation control soak and check logs for callback, frame, timeout, and credential failures.

### Rollback

Rollback is required if the application fails to start, registry state becomes ambiguous, existing sessions disappear, control access regresses, Powder credentials leak, or the candidate cannot complete bounded health and list operations.
Rollback must restore the recorded previous artifact without deleting the registry, Powder database, tmux sessions, or user worktrees.
After rollback, the Captain must verify the same snapshot dimensions and report any remaining divergence.

### Exit Gate

Stage 3 passes when the installed artifact matches the candidate, native Windows and WSL control clients behave within the bound, all preserved sessions remain accounted for, and rollback remains available.

## Stage 4 - Run Baseline Local Acceptance

This stage proves the repaired transport and serialized lifecycle using the existing safe operations before automated completion is enabled.

### Entrance

- Stage 3 passed.
- `thub-local-acceptance` is ready and unclaimed, or its previous run has been authoritatively reconciled and released.
- The exact acceptance worktree contains only its documented protected baseline.
- The General authorizes one real dispatch and its required lifecycle mutations.

### Scenario

1. Dispatch exactly one Codex Crew through the sanctioned T-Hub surface.
2. Verify that T-Hub validates the Project, checkout, branch, repository, card, and protected Powder profile.
3. Verify that T-Hub acquires one authoritative Powder claim and run before launching the Harness.
4. Verify that T-Hub persists the exact Crew, terminal, card, run, worktree, branch, Harness, and operation bindings.
5. Verify that the Harness is live in the exact worktree and received the authoritative brief.
6. Exercise a heartbeat and renewal while terminal liveness is proven.
7. Exercise a run-bound work-log append only if the installed sanctioned surface supports the deployed Powder contract.
8. Reconcile any ambiguous work-log result by operation identity rather than resubmitting with a new identity.
9. End the baseline run through the safest currently enabled release path because automated completion remains disabled before T1 and T3.
10. Verify that Powder, the T-Hub registry, the terminal roster, the Crew report, and the sentinel agree.
11. Verify that no stale claim, run, Crew binding, terminal, or extra worktree change remains.

### Stop Conditions

The Captain must stop immediately on multiple claims, multiple Crew, identity mismatch, raw credential output, ambiguous mutation outcome, unexpected worktree change, incomplete rollback, stale claim, or unbounded control delay.
The Captain must reconcile authoritative state before reporting failure.

### Exit Gate

Stage 4 passes when claim, heartbeat, renewal, optional safe work log, release, cleanup, and all durable identities agree without stale state.
It does not prove automated completion.

## Stage 5 - Implement T1 Run-Bound Mutations

T1 enables T-Hub to consume Powder's deployed run-bound review and completion primitives without weakening their authority.

### Entrance

- Powder P1 through P4 are deployed on the exact local service used by `n8desktop-wsl`.
- Powder capabilities or version output proves the required API contract.
- The T1 Powder card is transitioned to ready through a sanctioned operation if its recorded dependencies are satisfied.
- The Captain assigns one Crew exclusive ownership of all shared Powder and control integration files.
- T2 and T30 are already present in the Crew's base commit.

### Required Design

Every completion intent must carry and persist:

- The canonical Powder repository and card identity.
- The expected current run identity.
- A stable operation identity derived once for the durable Crew intent.
- The exact acceptance criterion identities being reviewed.
- Authenticated reviewer identity and review proof.
- Completion proof and normalized authoritative receipts.
- The Project, Assignment, Captain, Crew, terminal, worktree, branch, and Harness bindings needed for audit and recovery.

T-Hub must perform capability and version checks before enabling automated completion.
Unsupported or older Powder servers must leave automated completion disabled with an actionable error.
There must be no card-only fallback and no client-side read-then-complete approximation.

T-Hub must validate every returned receipt against the requested repository, card, run, operation, criterion, reviewer, and final state.
A mismatch must enter a visible fail-closed recovery state rather than being accepted as success.

### Required Tests

- Run A completes successfully when it remains the current run and all proof is valid.
- A delayed Run A request cannot complete reclaimed Run B.
- Release, expiry, timeout, restart, and retry cannot change the expected run silently.
- An identical operation replay returns the same authoritative result without duplicate effects.
- The same operation identity with a changed payload is rejected as a conflict.
- Missing or invalid criteria, reviewer identity, or completion proof is rejected.
- Unsupported Powder capability or version leaves completion disabled.
- Authentication, malformed response, bounds, timeout, stale run, and receipt mismatch fail closed.
- Backend, CLI, MCP, and any enabled UI surface preserve the same operation contract.
- Barrier-controlled concurrency tests prove lifecycle serialization around completion.

### Exit Gate

T1 passes when all adapters send the exact identities, validate the authoritative receipt, reject stale runs, preserve replay semantics, and receive independent review with no unresolved finding.
T1 must not be enabled in the installed runtime until T3 also passes.

## Stage 6 - Implement T3 Evidence and Completion Recovery

T3 makes lost responses recoverable without duplicate evidence or effects.

### Entrance

- T1 is implementation-complete on the integration base.
- T2 lifecycle serialization is present.
- Powder operation-status recovery and run-bound work-log behavior are deployed and version-verified.
- One Crew exclusively owns the shared integration files.

### Durable Recovery State Machine

Before dispatching a mutation, T-Hub must durably store the operation identity, payload digest, target repository, card, expected run, mutation kind, and Crew intent state.

After a definitive success response, T-Hub must validate and adopt Powder's normalized authoritative record.
After a definitive rejection, T-Hub must persist and surface the rejection without treating it as success.
After a timeout, partial response, EOF, connection loss, or process interruption, T-Hub must query operation status using the same operation identity.

Recovery outcomes are:

| Powder outcome | T-Hub action |
| --- | --- |
| Committed or replayed | Validate and adopt the authoritative stored result. |
| Pending | Remain visibly pending and poll within one bounded recovery policy. |
| Rejected | Persist the rejection and stop automatic retry. |
| Unknown but retry permitted | Reissue at most as allowed with the same operation identity and identical payload. |
| Unknown and retry not proven safe | Fail closed and request reconciliation. |
| Expired operation record | Fail closed unless the versioned contract provides a safe deterministic next action. |
| Payload digest conflict | Surface a hard conflict and never replace the original operation. |
| Stale run | Reject the evidence or completion and preserve the current run. |
| Authentication, version, bounds, or malformed failure | Fail closed without mutation retry. |

The recovery loop must use one bounded overall deadline and must survive application restart from durable state.
The normalized record returned by Powder is authoritative even when Powder sanitizes, redacts, or otherwise normalizes submitted evidence.

### Required Tests

- Work-log response loss followed by a committed status produces one stored log.
- Completion response loss followed by a committed status produces one completion.
- A retry uses the identical operation identity and payload digest.
- A changed payload under the same operation identity produces a conflict.
- A stale run cannot append evidence to or complete the current run.
- Restart during pending recovery resumes without duplicate mutation.
- Operation retention expiry fails closed.
- Normalized or scrubbed work-log content is adopted from Powder's stored result.
- CLI and MCP expose stable pending, recovered, rejected, stale, conflict, and timeout results.
- Malformed, oversized, unauthenticated, and unsupported-version responses remain bounded and credential-safe.
- Completion recovery remains serialized against heartbeat, renewal, release, close, cleanup, and reconciler actions.

### Exit Gate

T3 passes when ambiguous work-log and completion outcomes are always reconciled by operation identity, stale-run evidence is rejected, normalized records are adopted, restart recovery is deterministic, and independent review finds no unresolved issue.

## Stage 7 - Combined Review and Repository Gates

### Review Scope

The independent reviewer must inspect the complete T30, T2, T1, and T3 interaction rather than reviewing each lane only in isolation.
The reviewer must challenge authority, concurrency, replay, restart, adapter parity, bounded transport, and cleanup behavior.

The minimum combined test matrix is:

| Area | Required proof |
| --- | --- |
| Transport | Refused, silent, rotated, partial, oversized, malformed, and slow endpoints remain bounded. |
| Authority | Exact repository, card, run, operation, criterion, reviewer, proof, Crew, and Project identities are enforced. |
| Concurrency | Completion cannot race renewal, heartbeat, release, close, cleanup, or reconciliation for the same Crew. |
| Replay | Identical replay is idempotent and changed payload is a conflict. |
| Recovery | Response loss, restart, pending status, retention expiry, and partial reissue do not duplicate effects. |
| Adapter parity | Backend, CLI, MCP, and enabled UI surfaces share the same semantics and errors. |
| Security | Credentials and sensitive evidence are absent from logs, stdout, stderr, protocol errors, and Powder work logs. |
| Cleanup | Completion or release leaves no stale claim, run binding, Crew, terminal, or owned disposable resource. |

### Repository Gates

Run the Rust gates from `apps/desktop/src-tauri`:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run the desktop frontend gates from the repository root:

```sh
pnpm --filter t-hub-desktop test
pnpm typecheck
pnpm build
```

Run the MCP process proof when the changed surface includes MCP or shared control behavior:

```sh
apps/desktop/scripts/mcp_proof.sh
```

Run `git diff --check`, verify tracked worktree cleanliness, verify protected artifacts, and record every explicit test ignore.
An ignored test must have a separately executed named gate when it covers required production behavior.

### Exit Gate

Stage 7 passes only when the full combined branch is reviewed, all required gates pass from the exact candidate commit, and every review finding is fixed and rereviewed.

## Stage 8 - Final Authorized Installation

The General must explicitly authorize this stage even if Stage 3 installation was previously authorized.

The Captain must repeat the Stage 3 snapshot, build, artifact identity, installation, session-preservation, control-client, protected-profile, and rollback proofs using the final T1 and T3 candidate.
The installed artifact must match the exact reviewed commit.
Automated completion remains disabled until the installed capability check succeeds against the local Powder service.

Stage 8 passes only when the installed runtime proves the same contracts that passed from source.

## Stage 9 - Final Local Acceptance

### Scenario

Use `thub-local-acceptance` or a fresh dedicated acceptance run on that same card after authoritative reconciliation.
Do not use personal or unrelated operator backlog data.

1. Claim the card through T-Hub and record the exact run and operation identities.
2. Persist the Crew binding before Harness launch.
3. Prove heartbeat and renewal while the Crew is live.
4. Append a run-bound work log with a stable operation identity.
5. Simulate or inject response loss and prove operation-status recovery returns the single normalized stored record.
6. Review the exact run-scoped criteria with authenticated reviewer proof.
7. Complete the exact expected run with completion proof and a stable operation identity.
8. Replay the identical completion and prove that Powder returns the same result without a second effect.
9. Attempt a changed-payload replay and prove a conflict.
10. Create or use a controlled Run A versus Run B sequence and prove delayed Run A cannot mutate Run B.
11. Verify that T-Hub reaches the correct terminal Crew state and that cleanup does not race a late heartbeat or renewal.
12. Verify Powder card history, work logs, criterion review, operation records, run state, and claim state.
13. Verify T-Hub Project, Captain, Crew, terminal, worktree, report, sentinel, and lifecycle state.
14. Verify that no stale claim, duplicate log, duplicate completion, orphaned Crew, leaked credential, or unexpected worktree delta remains.

### Acceptance Evidence Packet

The Captain's final checkpoint must include:

- The installed T-Hub version, executable hash, and source commit.
- The deployed Powder commit, schema version, and capability output.
- The Project, repository, profile, Captain, Crew, terminal, card, and run identities.
- The stable operation identities for work log, review, and completion.
- The normalized authoritative receipts with sensitive content removed.
- The test and review results tied to exact commits.
- The before and after terminal, claim, run, registry, and worktree state.
- The rollback artifact and whether rollback was exercised.
- Every remaining limitation or deferred production concern.

### Final Classification

The Captain must finish with exactly one classification:

- `STATUS: local Powder integration accepted`
- `DECISION-NEEDED: local Powder integration blocked`
- `EMERGENCY: security or destructive risk`

## Immediate Next Action

The existing Captain should not dispatch T1 or T3 immediately.
Its next action is Stage 0 live reconciliation.
If the live evidence confirms the recorded implementation and review boundaries, the next authorized integration order is T30 first and T2 second.
Only after the integrated candidate is installed and the baseline local acceptance passes should the Captain transition T1 into an executable lane.
T3 follows T1 and must share one exclusive integration owner or a strictly serialized ownership handoff.

The General can begin with this instruction:

```text
Read docs/LOCAL-POWDER-CAPTAIN-EXECUTION-PLAN.md, recover your durable Captain manifest, and execute Stage 0 only.
```

## Current Captain State - 2026-07-16

This addendum supersedes the stale immediate-next-action wording above for the recovered `t-hub-app` Captain.

Powder P1-P4 are satisfied by the locally deployed Powder commit `8217c29` with schema version 18 and independent QA verification.

The corrected canonical T-Hub branch `fix/captain-control-runtime` contains the reviewed Stage 2 result at `7ea4dc5`.

Stage 1 formatting repair `d8aa935` has the same patch identity as `444131b`, while the parallel `444131b` history is not an ancestor of the canonical result.

T30 remains represented by the translated history through `171b83b`, and T2 is represented by commits `59a2cb5` and `7ea4dc5`.

The Stage 2 candidate passed the CLI and desktop formatting gates, `git diff --check`, the 32-test Powder matrix across ten runs, MCP E2E, the full desktop workspace tests, and warnings-denied Clippy.

Independent combined T30/T2 review approved the candidate, and a fresh independent validation of the canonical fast-forward is required before any further implementation work.

T1 and T3 remain separate later work and must not be dispatched during this validation boundary.

Installation, restart, publication, claim completion or release, and worktree cleanup remain unauthorized unless separately directed.
