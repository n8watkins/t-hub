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
The lifecycle-guard heartbeat reauthorization finding was remediated by commit `8bfa0fcd6bcde133a35abcbd58976cde0992ffd6`.
The removed-Crew close authorization finding was remediated by commit `17c8ed5e7b7f1b31c295631dccd1435a178dccf6`.
The initial Powder claim receipt identity finding was remediated by commit `a712df7a1d7222272f3c128d20e06ad074ff797c`.
The initial-claim transactionality finding was remediated by commit `bf3c3ca2f4fafecb40a9a9efea7a1ba7fe3d5f4c`.
The M4 dispatch-authority ABA and M5 bounded initial-claim identity findings were remediated by commit `021eb3c`.
The consolidated H1 through H4 rereview remediation was implemented by commit `0a71d32`.

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
Heartbeat repeats the same minimal ownership authorization after acquiring the renewal guard and before any repeated scope resolution.
Terminal close performs minimal lifecycle ownership authorization before checking Removed Crew history or starting Project and Powder reconciliation.
Initial claim receipts must match both the requested card and the protected profile's configured agent before dispatch can persist a Crew binding.
Before the initial claim POST, dispatch now persists a trusted pending intent containing the Project, protected profile, repository, card, configured agent, and stable operation identity.
Malformed, substituted, or response-lost initial receipts retain that intent, remove only the local terminal, never issue a release, and block redispatch until authoritative card reconciliation.
Dispatch now captures the Captain and Project scoped authority generation before remote preflight and revalidates the exact Captain terminal, ship, Project, repository root, Powder binding, and caller ship authority before spawn, after a claim, before launch, before attestation persistence, and before success.
The durable Crew bind checks the captured Captain, Crew, and Project generation while holding the registry mutation serializer, so a same-terminal release and reclaim cannot attach work to the replacement Captain.
An exact trusted claim is released only on a transaction-owned bind failure.
If that release cannot be confirmed, the pending initial-claim intent remains durable and dispatch reports incomplete rollback instead of claiming completion.
Initial-claim response parsing uses a 64 KiB bounded reader and rejects empty, whitespace-only, control-containing, and over-512-byte card, run, or agent identities.
The bind compare-and-set now uses the original dispatch Captain and Project authority tuple under the registry mutation lock, together with the transaction's Crew generation.
It does not recapture Captain or Project generations after the claim, so a same-terminal release and reclaim cannot become the expected authority.
Every post-claim dispatch authority failure now follows transaction-owned rollback using the exact trusted receipt rather than rediscovering mutable Project or Powder scope.
An ambiguous exact release marks a durable Crew cleanup-pending state when a binding exists and retains the initial-claim recovery intent.

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

Fresh rereview found that heartbeat revalidated generation after waiting but resolved full scope before reauthorizing a queued caller.
A former Captain whose Crew scope was removed while the renewal guard was held received the target-specific unknown-Crew error.
Heartbeat now repeats snapshot-only exact-Crew-or-current-Captain authorization inside the guard before resolving current scope.
The deterministic scope-removal regression proves generic ACL denial and zero renewal posts.

Fresh rereview found that `close_terminal` inspected Removed Crew state before lifecycle ownership authorization.
A cross-ship caller could reach historical cleanup and learn that a removed foreign Crew ship lacked a Project binding.
Close now authorizes from minimal registry ownership before the removed-state probe.
The regression proves generic ACL denial, no durable registry sequence change, retained local binding, and zero Powder release or renewal posts.

Fresh rereview found that the initial Powder claim response was only structurally parsed before dispatch persisted it.
A substituted card or agent could therefore become a durable Crew binding.
The Powder client now validates the requested card and configured profile agent before returning the initial claim.
Client regressions cover card and agent substitution, and the dispatch regression proves rollback removes the spawned terminal without a foreign binding or release request.

Fresh rereview found that a malformed, substituted, or lost initial-claim response could still represent a committed remote claim.
Dispatch previously removed the local terminal and reported all side effects rolled back despite having no trusted release identity.
The new pending-dispatch claim state survives restart and is keyed by trusted protected profile, repository, card, configured agent, and stable operation identity.
Response loss and substituted receipts retain recovery-pending state, issue no release, and block an identical retry after authoritative active-claim reconciliation.

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

After the fresh M1, M2, and M3 remediation, the focused control Powder suite passed all 63 tests and the close-terminal group passed all 4 tests.
The explicit cross-ship removed-Crew close regression passed.
The dispatch suite passed 10 tests with the existing real-agent integration gate intentionally ignored, and the Powder client suite passed all 27 tests.
Formatting, targeted all-target clippy, and `git diff --check` passed at code head `a712df7a1d7222272f3c128d20e06ad074ff797c`.

After initial-claim transactionality remediation, the focused Powder client suite passed all 27 tests, the dispatch suite passed 11 tests with the existing real-agent gate intentionally ignored, and the focused control Powder suite passed all 63 tests.
The response-loss restart and identical-retry regression, substituted-receipt regression, and foreign removed-Crew close regression passed.
Formatting, targeted all-target clippy, and `git diff --check` passed at code head `bf3c3ca2f4fafecb40a9a9efea7a1ba7fe3d5f4c`.

After M4 and M5 remediation, deterministic tests passed for a distinct Captain replacement after remote preflight with zero spawn or claim posts, same-terminal release-reclaim plus Powder rebind at bind CAS with exact terminal and claim rollback, and an ambiguous trusted release with durable recovery intent retained.
The malformed initial-claim dispatch regression passed with no durable Crew binding, no provider launch, and no Powder release.
The Powder client boundary suite passed empty, whitespace, control, oversized identity, oversized body, and exact-boundary identity cases.
The focused control Powder suite passed all 63 tests.
`cargo clippy -p t-hub --lib --tests -- -D warnings`, formatting, and `git diff --check` passed at `021eb3c`.

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

The post-M4 focused dispatch filter originally exposed a hermetic pre-launch unreadable-process-evidence race in `dispatch_test_harness_command_failures_roll_back_all_side_effects`.
The H4 deterministic fixture correction resolved that test without changing production tmux behavior.

The H4 correction is production-path coverage for the authoritative zsh environment.
Fresh zsh startup can transiently yield `LaunchAttestationError::UnreadableEvidence` from the bounded foreground-process observer.
That error intentionally collapses several lookup and process-evidence failures, so this packet does not attribute the behavior to a specific unreadable subcase.
Dispatch now requires two identical bounded observations before provider launch, so an unstable fresh-zsh transition cannot be accepted as a stable baseline.
The stable-baseline policy makes at most eight observation-pair attempts and rejects terminal, process, executable, or ancestry changes explicitly.
Dispatch revalidates the original authority tuple immediately after stable baseline and before provider send, with transaction-owned exact-claim rollback on revocation.
Direct policy coverage includes transient unreadable recovery, identity-change rejection, non-unreadable immediate failure, bounded exhaustion, and Captain replacement after stable baseline with zero provider send.
This leaves the provider-native launch and every final attestation check unchanged and does not modify production `tmux.rs` or force bash.
`dispatch_test_harness_command_failures_roll_back_all_side_effects`, `dispatch_test_harness_command_success_persists_separate_permission_axes`, and `dispatch_restart_rejects_contender_without_releasing_successful_winner` all passed after the correction.

Commit `6f14ae7` adds the H4 rereview coverage required to make those assertions load-bearing.
The stable-pair policy now has separate process-identity-only and executable-identity-only changes, with unchanged pane generation and ancestry, proving both comparisons independently reject the pair.
The distinct-Captain replacement test and a new same-terminal release-reclaim ABA test both pause after the stable baseline, assert the original authority generation rejects before provider send, and prove the FakeHarnessCommand invocation marker was never written.
Both tests retain the replacement Captain and its expected Project or Powder binding, leave no pending initial-claim intent, remove the transaction-owned Crew terminal, and validate the exact release route, run body, and configured claim agent.
The exhausted `UnreadableEvidence` dispatch test proves all eight bounded attempts fail closed, no provider command executes, the terminal and durable Crew binding are removed, no pending intent remains, and the exact trusted release route, run body, and configured claim agent are retained.
The hermetic Captain fixture now starts its command as tmux's initial session command instead of injecting it into a fresh zsh pane, eliminating an unrelated fresh-shell input race without changing production `tmux.rs`.
The real-zsh Crew dispatch regressions remain in the serialized dispatch filter.
At `6f14ae7`, the serialized dispatch filter passed 22 tests with one existing real-agent test intentionally ignored, the Harness filter passed 15 tests, and formatting, `cargo clippy -p t-hub --all-targets -- -D warnings`, and `git diff --check` passed.

Independent review found that a post-bind ambiguous release could retain only a CleanupPending Crew after the initial-claim intent had been cleared.
That Crew's normal restart cleanup could then resolve the replacement Captain Project and Powder binding instead of the original release scope.
Commit `2e4a332` introduces a distinct `PendingDispatchRelease` recovery record with the original Project, protected profile, repository, card, run, agent, initial operation identity, and transaction Crew terminal.
It is retained atomically with the exact CleanupPending Crew only after a trusted post-bind release becomes ambiguous, so it never misrepresents a trusted bound claim as an unresolved initial claim.
Both periodic reconciliation and ordinary Crew cleanup resolve this record directly from its frozen original profile and exact card, run, and agent receipt identity before any mutable Captain or Project scope lookup.
Confirmed recovery clears only the exact recovery record and transaction-owned CleanupPending Crew, preserving replacement Captain state and Project or Powder bindings.
Deterministic barriers at before launch, before attestation persistence, and before success each perform same-terminal release-reclaim plus Project profile and repository rebind, force an EOF-ambiguous first release, restart the registry, and prove recovery sends its second exact release only to the original profile server.
The replacement-profile server receives zero requests in every phase test.
The explicit after-claim ambiguity test retains the correctly named initial-claim recovery intent because no durable Crew binding exists at that phase.
At `2e4a332`, the serialized dispatch filter passed 25 tests with one existing real-agent test intentionally ignored, the Harness filter passed 15 tests, and formatting, `cargo clippy -p t-hub --all-targets -- -D warnings`, and `git diff --check` passed.

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
12. Verify the captured dispatch authority generation is compared both before side effects and atomically during Crew binding, including release-reclaim ABA.
13. Verify malformed or oversized initial-claim responses remain recovery-pending and cannot create a durable Crew binding or provider launch.
14. Verify every post-claim dispatch authority revalidation uses transaction-owned exact-claim rollback rather than returning a bare authority error.
12. Decide whether frontend dependency installation and a separate TypeScript gate are required before exact-run approval.
13. Verify a heartbeat queued behind another Crew lifecycle operation revalidates the exact current Captain and worker liveness before renewal.
14. Verify terminal close holds the same per-Crew lifecycle guard before tmux teardown and through authoritative Powder cleanup and persistence.
15. Verify heartbeat and renewal compare authoritative claim agent to durable Crew ownership before mutation and require exact card, run, and agent receipts before persisting expiry.
16. Verify a queued Captain close rejects replacement or authority ABA before tmux teardown and cannot cross into a replacement Captain's Crew lifecycle.
17. Verify queued heartbeat revalidates current authority generation, and expiry persistence compare-and-sets the exact pre-renewal Crew, Captain, Project, Powder binding, and generation scope.
18. Verify the reconciler repeats Harness liveness validation inside the renewal guard before it can issue a remote renewal.
19. Verify a foreign heartbeat probe is authorized from minimal registry ownership before full Project or Powder scope resolution and cannot disclose target binding state or issue a remote renewal.
20. Verify heartbeat repeats minimal target ownership authorization after acquiring its lifecycle guard and rejects removed former-Captain scopes without a renewal.
21. Verify close terminal authorizes foreign removed-Crew targets before historical Project or Powder resolution and leaves no local or remote side effect on denial.
22. Verify initial Powder claim receipts match the requested card and configured profile agent before dispatch persists any Crew binding.
23. Verify any ambiguous initial claim retains a trusted durable recovery intent, attempts no untrusted release, survives restart, and blocks duplicate redispatch until authoritative reconciliation.
