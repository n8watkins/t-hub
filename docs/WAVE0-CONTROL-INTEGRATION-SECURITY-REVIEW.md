# Wave 0 Control Integration Security Review Packet

Date: 2026-07-17

Powder card: `thub-wave0-control-integration`

Powder run: `run-odTeCbFC1-Gz`

Integration branch: `integrate/wave0-control`

This packet is evidence for an independent security review.
It is not an approval and does not complete the Powder card.

## Reviewed Inputs and Provenance

The canonical coordinator base is `a3b136a4005afea05b2d72f21f5742d4ad0f08fe` from `fix/captain-control-runtime`.
The frozen run-bound Powder mutation input is `4cc6e4438e602b0054812bbc3e99e0fab5e33ff5` from `fix/powder-run-bound-mutations`.
The frozen Crew launch-attestation input is `82e8d48cfd0abf3802cf15dd362987186a0c5af7` from `fix/codex-permission-launch-attestation`.

The run-bound mutation lane was integrated by merge commit `6b172dc98f168b08c17712e50e4c7db7c6aeea44`.
Its parents are the exact coordinator base and the exact frozen mutation head.

The launch-attestation lane was integrated by merge commit `59d4000f8644b2499d74c03b24e49bc9fdfbd624`.
Its parents are the run-bound integration commit and the exact frozen launch-attestation head.

Independent review findings were remediated by commit `08beabc7f09fa9c9e0d3fde3159f540fadba8bc9`.
That commit restores the exact reviewed marker contract and closes the queued-heartbeat and terminal-close races described below.
The follow-up renewal identity finding was remediated by commit `3b2bd8e8b4d17d89f3548ff9fd9c9fb2b8d2af88`.
That commit binds pre-mutation authority and post-mutation receipts to the durable Powder claim identity.
The post-rereview authority-generation finding was remediated by commit `e011125bebdbd82be13ab0724fb0fa08c23fcaf4`.
That commit revalidates queued Captain close authority, rejects authority ABA on queued heartbeat, and protects expiry persistence with an exact-scope compare-and-set.
The foreign-heartbeat information-disclosure finding was remediated by commit `8b056fb35a59f4a73d257c3239c379a465d3c704`.
That commit authorizes the target Crew or current Captain from a registry snapshot before resolving Project or Powder scope.

Both frozen reviewed heads are ancestors of the integration head.
Neither reviewed head was rebased or modified.

## Pre-Edit Conflict Analysis

Read-only merge-base, merge-tree, file-diff, and commit-equivalence analysis was completed before editing.

The merge base between the coordinator and the run-bound mutation lane is `826c5fecb6294195e1f1ce3553bcc7225ae6fa64`.
The merge base between the coordinator and the launch-attestation lane is `17c00ee1236ab021b6691445cb04515a1e38cf9c`.
The merge base between the two reviewed lanes is also `17c00ee1236ab021b6691445cb04515a1e38cf9c`.

The coordinator and run-bound lane produced a clean read-only merge tree `4c9292974042f5ed512197bbd3ba7617a7662fc6`.
The coordinator and launch-attestation lane produced textual conflicts in `apps/desktop/src-tauri/crates/t-hub-agent/src/main.rs`, `apps/desktop/src-tauri/src/control.rs`, and `apps/desktop/src-tauri/src/harness/mod.rs`.
The two reviewed lanes produced the same three textual conflict files, while `apps/desktop/src-tauri/src/powder.rs` auto-merged.
The final launch-attestation merge exposed 57 textual conflict groups: 5 in the agent entry point, 34 in control, and 18 in the Harness adapter.

The direct semantic file overlap between the two reviewed lanes was `apps/desktop/src-tauri/src/control.rs` and `apps/desktop/src-tauri/src/powder.rs`.
Coordinator changes also overlapped launch attestation in the agent lifecycle producer, Harness adapters, control persistence, and real-process fixtures.
Commit-equivalence analysis showed that the first ten launch-attestation commits already had patch-equivalent coordinator commits, while the final ten commits carried the new bypass policy, exact marker provenance, release receipt identity, process provenance, rollback isolation, and frontend permission-axis work.

## Deliberate Reconciliation

The integration preserves the run-bound mutation lane's exact card, run, repository, agent, operation, digest, criterion, reviewer, proof, and recovery validation.
The integration preserves durable terminal conflict tombstones and refuses card-only automated completion.
The integration preserves bounded evidence, bounded mutation payloads, replay-safe operation recovery, ambiguous response recovery, and fail-closed capability gates.

Crew dispatch uses the fleet's explicit unrestricted local Harness posture and keeps the T-Hub control capability at `read`.
The provider command uses the native Codex or Claude bypass flag without a wrapper obscuring the final foreground provider process.
Dispatch observes a baseline shell, verifies the provider process transition, performs a final pre-persistence re-observation, persists the permission axes, and performs a final post-persistence re-observation.
Every final observation binds the pane generation, process lifetime, executable device and inode, ancestry, and provider-native permission posture.
Any attestation or persistence failure invokes transactional terminal, durable Crew, and exact Powder claim rollback.

The interactive Codex degraded marker combines the coordinator lifecycle telemetry with the reviewed lane's exact tmux session, window, pane, pane PID, process, registry generation, schema, entity, and `AgentCommand` contract.
The supervisor consumes the generic telemetry fields before event-specific reduction, so the exact reviewed marker type still degrades the session without inferring false working state.
The marker has bounded credential-safe output and must succeed before the shell executes Codex.

Powder release now rejects receipts whose card, run, or agent differs from the exact expected claim.
Control cleanup builds that expected claim from freshly verified authoritative ownership.
Local Crew bindings remain retained when a release receipt is substituted or release cannot be confirmed.
Heartbeat and renewal now reject an authoritative claim agent that differs from the durable Crew binding.
Both operations also reject any card, run, or agent receipt substitution before updating durable claim expiry.
The expiry update compares the exact Crew, Captain, Project, Powder binding, and scoped authority generation observed before remote renewal.
Terminal close captures exact Captain authority before waiting and revalidates it after the per-Crew lifecycle guard before tmux teardown.
Heartbeat now performs a minimal Crew and Captain ownership check from one registry snapshot before resolving the full Powder scope.
Foreign target probes therefore fail with a generic ACL denial without disclosing target Project or Powder binding state.
The existing full-scope checks, Harness liveness checks, operation guard, and post-guard authority-generation revalidation remain in force.

The frontend snapshot adapter retains `harnessPermission` and `tHubCapability` as separate compatibility axes.
Unknown values remain omitted instead of being accepted as authoritative state.

The production `apps/desktop/src-tauri/src/tmux.rs` is unchanged from the coordinator base.
Only hermetic control test fixtures were reconciled to serialize real tmux process tests, keep an anchor process alive, reap isolated sessions, and keep issued claim, evidence, and release receipt identities coherent.

## Independent Review Findings and Remediation

Independent review found that the first integration changed the reviewed degraded-marker event from `AgentCommand` to `CoreAction`.
The event is restored to `AgentCommand` without losing provider-neutral degraded telemetry, and the producer, exact marker E2E tests, supervisor reducer test, and combined real-agent gate now lock that contract.

Independent review found that an authenticated Captain heartbeat could pass authority checks, wait behind a per-Crew operation guard, and renew after Captain authority changed.
Heartbeat now resolves and validates the exact current owner again after acquiring the guard and rechecks Harness liveness before any Powder renewal.
A deterministic test holds a completion guard, queues the authenticated Captain heartbeat, releases the Captain while it waits, and proves that the request is rejected with zero renewal posts.

Independent review found that terminal close could kill the tmux session before acquiring the per-Crew lifecycle guard.
Terminal close now holds the cleanup guard across liveness planning, tmux tree teardown, remote Powder disposition, and the final durable registry transition.
A deterministic test queues close behind completion, proves the worker remains alive while close waits, completes the exact run, then proves close reports `killed`, observes `already_completed`, removes the binding, and leaves the session gone.

Independent review found that renewal validated the bound run but not the durable Powder agent before mutation and did not validate exact card, run, and agent identity in the renewal receipt.
The guarded renewal path now revalidates active lifecycle state and real Harness liveness, compares the authoritative claim agent to durable ownership, and persists expiry only after an exact receipt.
One test proves authoritative agent substitution produces zero renewal posts, and another proves substituted card, run, or agent receipts cannot update durable expiry.
A queued-liveness test holds the renewal guard, stops the exact Harness while heartbeat waits, and proves zero renewal posts after the guard is released.

Fresh rereview found that a former Captain could queue terminal close, be replaced while the close waited, and then kill the replacement Captain's Crew.
Close now captures the current Captain terminal, ship, Project identity, and scoped generation before waiting and revalidates the same authority immediately after the guard is acquired.
The replacement test proves the old close is rejected before tmux teardown and that the live Crew terminal remains alive.

Fresh rereview also required replacement and ABA proof for heartbeat, exact-scope expiry persistence after an in-flight remote renewal, and reconciler liveness revalidation inside its renewal guard.
Queued heartbeat now rejects both a distinct Captain replacement and release-reclaim ABA with zero renewal posts.
The renewal compare-and-set refuses to persist an accepted remote receipt after the scope generation changes, retaining the previous expiry.
The reconciler test observes liveness before its guard, stops the Harness while renewal waits, and proves the post-guard recheck emits zero remote renewals.

Latest rereview found that `heartbeat_crew_powder` resolved full Powder scope before ACL authorization.
A cross-ship Captain could therefore learn that a foreign Crew ship had no Project binding from the target-specific error.
Heartbeat now uses a snapshot-only target ownership gate before full scope resolution and returns the generic exact-Crew-or-owning-Captain ACL denial for foreign probes.
The regression test targets an active foreign Crew with a durable work binding but no Project binding and proves the loopback Powder server observes zero renewal posts.

## Focused Verification

`cargo test -p t-hub-agent` passed 56 unit tests, 3 Codex tap E2E tests, and 1 exact unobserved-marker E2E test before duplicate compatibility code was removed.
The final workspace run passed the resulting 55 agent unit tests, 3 Codex tap E2E tests, and 1 exact unobserved-marker E2E test.

`cargo test -p t-hub --lib harness::tests -- --test-threads=1` passed 15 Harness adapter, generation, process, ancestry, parser, and final-observation tests.
`cargo test -p t-hub --lib powder::tests -- --test-threads=1` initially passed 25 Powder client, capability, operation, evidence, receipt, and bounded-output tests.
The focused control Powder suite initially passed 53 of 54 tests and exposed one fixture identity inconsistency after exact release-agent validation was integrated.
The loopback fixture had issued a `t-hub` claim but later described the same run as owned by `powder-agent`.
The fixture now carries the issued claim agent through evidence and the default release receipt, and the isolated failing test passed.
The final workspace run confirmed the entire control Powder suite passes.

The focused dispatch suite initially passed 7 tests, ignored the separately scripted real-agent gate, and exposed the same fixture identity inconsistency in two rollback tests.
Both rollback tests passed after the fixture coherence fix.
Four real process-level permission-attestation tests passed.
The exact release-receipt substitution test, rollback retention test, and launch-attestation rollback test passed.
Five scoped authority tests and the foreign/confused-deputy Powder authorization test passed.
Both permission-axis persistence tests passed.

`scripts/captain/verify-codex-permission-integration.sh` built the combined-tree agent and passed the ignored real-agent launch gate.
The gate verified that the exact owning Codex Crew marker is written before provider execution and that the real tmux session remains correctly attributed.

After independent review remediation, the deterministic queued-heartbeat authority race, close-versus-completion race, rollback retention, and complete close-terminal group all passed.
The exact degraded-marker consumer test passed, and `cargo test -p t-hub-agent` again passed 55 unit tests, 3 Codex tap E2E tests, and 1 exact unobserved-marker E2E test.
The focused control Powder suite passed all 55 tests after the first remediation.
The combined real-agent verification script passed with the restored `AgentCommand` marker.

After the renewal identity remediation, the Powder client suite passed 26 tests and the focused control Powder suite passed all 58 tests.
The added coverage includes heartbeat and renewal receipt substitution, durable agent substitution, Captain replacement while queued, Harness exit while queued, successful guarded renewal, and stale-state rejection.
Targeted `cargo clippy -p t-hub --all-targets -- -D warnings` also passed at the renewal-remediation head.

After the post-rereview authority-generation remediation, the focused control Powder suite passed all 62 tests and the close-terminal group passed all 4 tests.
The targeted authenticated lifecycle-authority, full-token denial, Captain checkpoint, and 26-test Powder client suites also passed.
Formatting, targeted all-target clippy, and `git diff --check` passed at commit `e011125bebdbd82be13ab0724fb0fa08c23fcaf4`.

After the foreign-heartbeat remediation, `captain_cannot_close_or_heartbeat_foreign_crew` and the new deterministic cross-ship non-Project probe test passed.
The focused control Powder suite passed all 62 tests, the close-terminal group passed all 4 tests, and the Powder client suite passed all 26 tests.
Formatting, targeted all-target clippy, and `git diff --check` passed at commit `8b056fb35a59f4a73d257c3239c379a465d3c704`.

The standalone CLI Powder contract suite passed 10 tests.
The MCP Powder schema tests passed in both library and binary targets.
The real authenticated MCP Powder dispatch E2E initially stopped before execution because its expected standalone debug MCP binary had not been built.
After `cargo build -p t-hub-mcp`, the exact E2E passed.

## Broad Gates

`cargo fmt --all -- --check` passed for the desktop Rust workspace.
`cargo clippy --workspace --all-targets -- -D warnings` passed for the desktop Rust workspace.
`cargo test --workspace` passed once without rerunning the broad gate.
The core library reported 817 passed and 2 documented ignored tests.
The MCP E2E target reported 2 passed and 1 helper ignored.
All agent, MCP, protocol, and documentation tests executed by the workspace gate passed.

The one-time broad workspace gate preceded the independent-review remediation commit.
Post-remediation `cargo fmt --all -- --check` and targeted `cargo clippy -p t-hub -p t-hub-agent --all-targets -- -D warnings` passed.
The post-remediation deterministic race, cleanup, rollback, marker-consumer, and complete agent suites passed as recorded above.

`cargo fmt --all -- --check` passed for the standalone CLI crate.
`cargo clippy --all-targets -- -D warnings` passed for the standalone CLI crate.
`cargo test` passed 47 CLI unit tests and 10 Powder CLI contract tests.

`git diff --check` passed.
The tracked worktree was clean after the integration commits and gates.
The pre-existing protected untracked `CLAUDE.md` remains untouched.

Frontend dependencies were not present in this checkout, so frontend typecheck and Vitest were not run and no dependency installation was attempted.
The frontend changes are limited to retaining and validating the two permission axes in the existing snapshot adapter and store type, with corresponding adapter expectations included in the diff.

## Failures and Residual Risks

Before the launch-attestation lane was integrated, the run-bound lane's focused control suite had one hermetic tmux teardown failure with `server exited unexpectedly`.
The reviewed launch-attestation lane's serialized and anchored tmux fixtures resolved that failure without modifying production tmux code.
Post-remediation rollback and close tests exposed two more last-session fixture races with the same tmux shutdown result.
Both tests now use the existing serialized anchor fixture and pass without production tmux changes.

The two integration-time failures described above were isolated once each and traced to hermetic fixture identity, not production release or attestation behavior.
The full workspace gate subsequently passed the affected tests.

The first post-remediation targeted clippy run found one `needless_borrow` in the new heartbeat path.
The exact lint was corrected, and the targeted clippy gate then passed.

The frontend typecheck and Vitest residual remains because dependencies were unavailable.
Independent review should inspect the small TypeScript adapter and store diff directly.

The installed T-Hub runtime was not modified, installed, or restarted.
The currently installed Crew run-bound mutation surface rejected work-log capability verification during this task, so the Captain must maintain the exact-run Powder work log through a sanctioned working surface.

No push, protected-branch merge, install, restart, deploy, publish, release, or Powder completion was performed.
No independent reviewer has approved this integration yet.

## Independent Reviewer Checklist

1. Verify both frozen source heads and the canonical coordinator base are exact merge parents in the recorded order.
2. Verify no run-bound mutation path accepts a card-only identity, substituted repository, changed operation digest, changed result body, or unauthenticated reviewer.
3. Verify terminal conflict tombstones are definitive, durable, replay-safe, and do not block later distinct operations after durable recording.
4. Verify ambiguous transport outcomes recover by the same operation identity and digest without duplicate mutation.
5. Verify completion remains gated on exact current-run criteria, reviewer identity, proof identity, repository authority, and fresh authoritative evidence.
6. Verify dispatch attests provider-native unrestricted argv, pane generation, process identity, executable identity, ancestry, and exact permission posture at every acceptance boundary.
7. Verify any failure after claim, terminal creation, durable Crew binding, or attestation persistence rolls back transactionally or retains an honest cleanup-pending binding.
8. Verify release success requires exact card, run, and agent receipt identity and that substituted receipts retain local ownership evidence.
9. Verify the Codex degraded marker is exact-pane bound, bounded, credential-safe, and cannot create a false working state.
10. Verify Harness local permission and T-Hub control capability remain separate in persistence, MCP and CLI output, and frontend snapshot compatibility.
11. Verify production tmux behavior is unchanged and fixture-only serialization cannot leak into runtime behavior.
12. Decide whether frontend dependency installation and a separate TypeScript gate are required before exact-run approval.
13. Verify a heartbeat queued behind another Crew lifecycle operation revalidates the exact current Captain and worker liveness before renewal.
14. Verify terminal close holds the same per-Crew lifecycle guard before tmux teardown and through authoritative Powder cleanup and persistence.
15. Verify heartbeat and renewal compare authoritative claim agent to durable Crew ownership before mutation and require exact card, run, and agent receipts before persisting expiry.
16. Verify a queued Captain close rejects replacement or authority ABA before tmux teardown and cannot cross into a replacement Captain's Crew lifecycle.
17. Verify queued heartbeat revalidates current authority generation, and expiry persistence compare-and-sets the exact pre-renewal Crew, Captain, Project, Powder binding, and generation scope.
18. Verify the reconciler repeats Harness liveness validation inside the renewal guard before it can issue a remote renewal.
19. Verify a foreign heartbeat probe is authorized from minimal registry ownership before full Project or Powder scope resolution and cannot disclose target binding state or issue a remote renewal.
