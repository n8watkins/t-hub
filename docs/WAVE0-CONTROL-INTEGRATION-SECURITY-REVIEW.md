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
The successful workspace-wide gate above is pre-remediation provenance only.
One post-remediation `cargo test --workspace` invocation did not complete and must not be treated as a passing current-head broad gate.
Its Cargo parent PID `1665116` and desktop `t_hub_lib` child PID `1665814` remained alive for more than 31 minutes, blocked in `do_wait` or `futex`, with no recoverable final test output.
Captain terminated only those two Wave 0-owned hung processes.
No post-remediation workspace-wide result is claimed.
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
Commit `2e4a332` introduced the initial distinct `PendingDispatchRelease` recovery record with the original Project, protected profile name, repository, card, run, agent, initial operation identity, and transaction Crew terminal.
The profile-name-only recovery description in this historical section is superseded by the later durability remediation below.
It was not sufficient to defend a protected profile endpoint remap, release preparation before terminal teardown, or concurrent recovery cleanup.
Current recovery retains a frozen keyed endpoint identity and repository identity, prepares its exact CleanupPending Crew before teardown, and serializes periodic and ordinary cleanup through one per-Crew guard.
Confirmed recovery removes a record only with its exact transaction-owned CleanupPending Crew.
It does not retire a reused terminal identity when that Crew is absent or replaced.
The newer remap and response-loss regressions replace the older claim of a second release POST.
The explicit after-claim ambiguity test retains the correctly named initial-claim recovery intent because no durable Crew binding exists at that phase.
At `2e4a332`, the serialized dispatch filter passed 25 tests with one existing real-agent test intentionally ignored, the Harness filter passed 15 tests, and formatting, `cargo clippy -p t-hub --all-targets -- -D warnings`, and `git diff --check` passed.

Fresh durability rereview found that the first frozen-release record was persisted only after a release response error.
Commits `9d3ce7e` and `1568bbd` replace that behavior with a durable release state machine and preserve it across Captain replacement.
Before every post-bind release POST, rollback persists the exact frozen recovery and its transaction-owned Crew as `CleanupPending` in `Prepared`, then atomically advances it to `InFlight` before sending the request.
An error response advances the durable record to `Ambiguous` only after that transition is persisted.
If that ambiguity persistence fails, the prior durable `InFlight` state remains authoritative rather than being replaced by an in-memory claim of rollback.
No release success clears either record until an exact trusted release receipt or authoritative released-run evidence is observed.
Recovery never reconstructs the original scope from the current Captain Project binding.
It uses only the frozen connection profile, repository, card, run, agent, and operation identity.
For `InFlight` or `Ambiguous` recovery, it validates the protected profile, exact keyed endpoint identity, frozen repository, and current card before reading authoritative exact-run evidence.
If Powder already reports the exact run released after response loss, recovery clears local transaction-owned state without a second release POST.
If the run remains active with the exact card and agent, recovery may perform one exact release using the frozen scope.
`Prepared` recovery first validates frozen profile, repository, current card claim, and exact run evidence, ensures its transaction terminal is gone, then atomically advances to `InFlight` before any exact release POST.
If that recovery-side POST has an ambiguous response, the durable `InFlight` record remains authoritative for the next reconciliation.

Schema version 11 made the endpoint-pinned recovery shape incompatible with an older binary that could silently discard it.
Schema version 12 replaced its raw endpoint field with an unsalted endpoint digest.
Schema version 13 replaces that credential-derived value with a standard HMAC-SHA-256 endpoint identity keyed by the protected client credential and makes a version 12 release recovery fail closed before any recovery or network call.
Version 12 snapshots without a release recovery load and upgrade on their next write.
Snapshot validation requires each release recovery to map to exactly one `CleanupPending` Crew under the exact Project with matching terminal, card, run, agent, and frozen-scope marker.
The reciprocal marker check rejects orphaned Crew recovery state, mismatched or foreign records, and duplicate recovery state before any remote release.

The earlier Prepared restart description is superseded by the post-rereview test below.
The response-loss test proves a release POST can commit while its response is lost, and that restart reconciles released run evidence without a blind second POST.
The same test forces ambiguity-state persistence failure after the POST and proves restart recovers from the last durable `InFlight` state without touching the replacement scope.
Malformed persistence pair coverage exercises orphan, foreign Project, mismatched agent, and missing reciprocal marker records with no network client construction.

Commit `5b15f01` adds endpoint and repository validation before recovery, atomic pre-teardown preparation, strict cross-record identity bounds, and exact-Crew-only recovery clearing.
Commit `ae59b3a` rechecks the durable recovery after entering the shared per-Crew cleanup guard, so a queued periodic reconciliation cannot replay a release after ordinary cleanup has already cleared it.
At code head `ae59b3a`, the serialized dispatch filter passed 25 tests with one existing real-agent test intentionally ignored.
The serialized control Powder filter passed 63 tests.
The Powder client filter passed 30 tests.
The Harness filter passed 15 tests.
The close filter passed 7 tests.
The agent suite passed 55 unit tests, 3 Codex TAP E2E tests, and 1 unobserved E2E test.
The CLI suite passed 47 unit tests and 10 Powder contract tests.
The MCP library and binary suites passed 16 and 75 tests respectively.
Formatting, all-target clippy for the changed desktop crate, and `git diff --check` passed.
An attempted follow-on Cargo command named a nonexistent `e2e` test target after the agent suite had already run its E2E targets successfully.
Cargo rejected that command before executing product tests.
The exact-run work-log append was attempted once with stable operation identity `work-log:wave0-durability-1154214`.
The sanctioned CLI returned `powder_mutation_unsupported` because deployed run-bound capability verification failed before any mutation.
An authenticated `send` attempt to Captain session `0c7b7560` was also rejected before delivery because this Crew token has only read control capability.
No append or Captain message is claimed as delivered.

The installed T-Hub runtime was not modified, installed, or restarted.
The currently installed Crew run-bound mutation surface rejected work-log capability verification during this task, so the Captain must maintain the exact-run Powder work log through a sanctioned working surface.

Commit `e2b2148` adds two deterministic recovery-race regressions required by the latest durability review.
`dispatch_release_recovery_serializes_periodic_reconcile_and_ordinary_cleanup` blocks canonical original-scope run evidence during periodic reconciliation while ordinary close cleanup waits on the same Crew guard.
The original release commits with an EOF response, the queued ordinary cleanup observes the retained exact recovery after waiting, validates released evidence, and clears it without a second POST.
The test proves one original-scope release POST, two original-scope evidence reads, zero replacement-profile evidence or release I/O, empty pending claim and release collections, and preservation of the replacement Project Powder binding.
`dispatch_release_inflight_before_post_survives_restart_without_blind_repost` pauses a post-bind authority rollback at `release_inflight_before_post` after terminal teardown and durable `InFlight` persistence.
It then simulates process death by dropping the barrier resume, reloads the registry, validates the original repository and exact card/run/agent evidence, sends the only release POST from recovery, and proves the terminated transaction cannot post after restart.
The test proves the transaction terminal is gone before restart, no provider command ran, the recovered registry has no durable Crew, pending claim, or pending release, and the replacement profile receives zero evidence or release I/O.
The test-only post-barrier check compares the durable `InFlight` record, not the pre-transition `Prepared` value, so normal rollback paths retain their exact release behavior.
Final clean-head verification for `e2b2148` and this evidence update passed 27 serialized dispatch tests with one existing real-agent test intentionally ignored, 63 serialized control Powder tests, 30 Powder client tests, 15 Harness tests, 55 agent unit tests, 3 Codex TAP E2E tests, 1 unobserved agent E2E test, 7 close tests, and the foreign close or heartbeat authority regression.
The CLI suite passed 47 unit tests and 10 Powder contract tests.
The MCP library and binary suites passed 16 and 75 tests respectively.
`cargo fmt --all -- --check`, `cargo clippy -p t-hub -p t-hub-agent --all-targets -- -D warnings`, `git diff --check`, and `git diff --cached --check` passed.
Tracked worktree cleanliness passed.
The only remaining worktree entry is the protected untracked `CLAUDE.md`, which this work did not modify.
An authenticated `th send` report to Captain session `0c7b7560` was attempted for frozen head `fa983da06eb83930ff99c148693a82bc14b11b22` with these commits, counts, and residual risk.
The installed control plane rejected it with gated code 5 because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

Commit `d1a5c0f` closes the producer-consumer release-recovery race found after `a78005c`.
The originating post-bind rollback now acquires the same per-Crew Cleanup guard as periodic and ordinary recovery before persisting `Prepared` and retains it through terminal teardown, `Prepared` to `InFlight`, exact release, and exact clear or retained ambiguity.
`rollback_trusted_dispatch_guarded` is the guarded-inner helper, so the producer does not recursively acquire its own guard.
Four deterministic tests pause the producer at `release_prepared_before_teardown` and `release_inflight_before_post` while, respectively, periodic reconciliation or ordinary cleanup queues on that Crew guard.
Each proves one exact original-scope release POST, no consumer evidence read or release POST after it waits, no replacement-scope I/O, terminal removal, empty pending claim and release collections, and preservation of the replacement Project binding.
At `d1a5c0f`, the serialized dispatch filter passed 31 tests with one existing real-agent test intentionally ignored.
The serialized control Powder filter passed 63 tests, the Powder client filter passed 30 tests, the Harness filter passed 15 tests, the close filter passed 7 tests, and the foreign close or heartbeat authority regression passed.
The agent suite passed 55 unit tests, 3 Codex TAP E2E tests, and 1 unobserved E2E test.
`cargo fmt --all -- --check`, `cargo clippy -p t-hub -p t-hub-agent --all-targets -- -D warnings`, `git diff --check`, and `git diff --cached --check` passed.
No post-remediation workspace-wide test result is claimed.
The standalone CLI suite passed 47 unit tests and 10 Powder contract tests.
One immediately chained MCP command was started from the standalone CLI manifest, where Cargo rejected `-p t-hub-mcp` before MCP test execution because that package is outside that manifest's package set.
The MCP suite was then run from the desktop workspace and passed 16 library tests and 75 binary tests.

Commit `11a2204` adds frozen-endpoint current-card validation to dispatch release recovery and repairs the prior Prepared false-positive fixture.
Recovery now reads authoritative current card evidence after repository validation and before run evidence.
Already-released convergence requires an exact released run plus a ready card with no current claim, and emits no release POST.
An active release requires the exact current card claim run and agent plus the exact active run before it can POST.
Reclaimed, different-claim, unknown-run, and malformed-card evidence retain the durable recovery and issue zero release mutation or replacement-scope I/O.
The repaired Prepared regression uses a real transaction-owned tmux terminal and coherent `t-hub` ownership.
It proves terminal teardown, durable `Prepared` to `InFlight` transition before the response-loss release attempt, one original-scope card read, one run read, one exact release POST, and zero replacement-scope I/O.
The first repair attempt started a second `FakeHarnessSession` while the Captain fixture held its test-only process-attestation tmux guard, so the test blocked before exercising recovery.
Only that attempt's exact zsh, Cargo, and `t_hub_lib` test PIDs `1818144`, `1818347`, and `1819163` were terminated.
The final test creates its transaction-owned tmux terminal directly under the existing fixture guard and has no production `tmux.rs` change.
Replacement, same-terminal ABA, and exhausted-baseline success tests now assert both pending dispatch collections are empty.
At `11a2204`, the serialized dispatch filter passed 32 tests with one existing real-agent test intentionally ignored.
The serialized control Powder filter passed 63 tests, the Powder client filter passed 30 tests, the Harness filter passed 15 tests, and the close filter passed 7 tests.
`cargo fmt --all -- --check`, `cargo clippy -p t-hub -p t-hub-agent --all-targets -- -D warnings`, `git diff --check`, and `git diff --cached --check` passed.
No post-remediation workspace-wide test result is claimed.

Commit `0fd0faa` closes the credential-safety review finding for frozen dispatch-release recovery scope.
`PendingDispatchRelease` initially stored only `connectionEndpointDigest`, an unsalted SHA-256 value computed from the validated and normalized protected profile base URL.
That historical representation is superseded by the keyed identity remediation below.
It did not persist the profile URL, path, query, or fragment, so `captains_sync_apply` could not forward those values in a registry snapshot.
Recovery reconstructed the protected profile locally and exact-compared the prior digest before repository, card, run-evidence, terminal, or release network I/O.
An endpoint or profile remap retains the durable original recovery with a generic error and makes zero original- or replacement-scope requests.
Snapshot schema version 12 accepts a version 11 snapshot with no release recovery and stamps version 12 on its next write.
Any version 11 snapshot carrying a recovery fails closed before network use.
A legacy raw `connectionEndpoint` recovery document also fails deserialization before recovery can construct a client.
The deterministic gateway-secret regression uses token-like path, query, and fragment values in both the original and remapped protected profile URLs.
It proves registry JSON, `sync_captains` payloads, `Debug` output, and endpoint-remap errors contain none of those values, while the historical persisted digest remains present.
It also proves zero card evidence, run evidence, or release requests reach either scope after the remap.
At `0fd0faa`, the focused endpoint secrecy regression, schema upgrade and legacy raw-state regressions, and the serialized dispatch-release filter with 10 tests passed.
A broader serial dispatch command selected 34 tests but did not return a complete aggregate summary after printing its first 28 progress markers, so it is not counted as a completed 34-test gate.
Its six remaining cases, including `dispatch_restart_rejects_contender_without_releasing_successful_winner`, were each isolated once and passed serially.
The 63-test control Powder filter, 30-test Powder client filter, 15-test Harness filter, and 7-test close filter passed serially.
The agent package suite passed 55 unit tests, 3 Codex TAP E2E tests, and 1 unobserved E2E test.
The CLI suite passed 47 unit tests and 10 Powder contract tests.
The MCP library and binary suites passed 16 and 75 tests respectively.
`cargo clippy -p t-hub -p t-hub-agent --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`, and `git diff --cached --check` passed.
One attempted `cargo test -p t-hub-agent --lib` invocation was rejected before test execution because that binary package has no library target.
One attempted nonexistent `codex_permission_e2e` target was likewise rejected before test execution.
No post-remediation workspace-wide test result is claimed.

Commit `d31e847` adds deterministic keyed-identity acceptance coverage without changing the design.
The HMAC-SHA-256 primitive is checked against RFC 4231 test case 1.
Endpoint identity coverage also proves identical protected URL and key inputs are stable, a changed key produces a distinct identity, and the identity does not contain the test key material.
At `d31e847`, the three endpoint-identity tests and the complete 35-test Powder client filter passed.
`cargo clippy -p t-hub --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
No post-remediation workspace-wide test result is claimed.

Commit `8619968` replaces recovery's formatted endpoint-identity string comparison with decoded HMAC tag verification through the standard `hmac::Mac::verify_slice` API.
The verifier returns typed fail-closed outcomes for a missing protected credential, malformed persisted identity, or HMAC mismatch.
It accepts only the canonical `hmac-sha256:` prefix followed by 64 lower-case hexadecimal tag characters before passing decoded bytes to the standard verifier.
Recovery maps each typed outcome to an endpoint-free static error and performs no repository, card, run, terminal, or release I/O on any verification failure.
The deterministic client coverage proves a matching identity succeeds, malformed length and upper-case tag encodings fail, and a changed endpoint or rotated credential fails verification.
The dispatch recovery regression invokes the malformed-identity path plus protected credential rotation and proves the loopback receives zero card, run, or release requests.
The existing remap regression continues to prove zero wrong-scope I/O on a keyed endpoint mismatch.
At `8619968`, all 36 Powder client tests and 7 dispatch-release-recovery tests passed serially.
`cargo clippy -p t-hub --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
No post-remediation workspace-wide test result is claimed.

Commit `ad74042` closes gateway response-body credential disclosure on matching keyed endpoints.
Every externally surfaced Powder HTTP status error now retains only the typed error kind and a bounded generic `Powder HTTP status <code>` message.
No untrusted gateway status body is parsed, redacted, persisted, returned, or logged by this client path.
The Powder client regression covers both 403 and 500 responses whose JSON error body echoes protected path, query, fragment, and API-key tokens.
It proves returned and `Debug` errors retain the expected typed class while containing none of those values.
The dispatch-release regression uses the exact same matching HMAC identity and invokes both direct recovery and periodic reconciliation for 403 and 500 gateway echoes.
It proves the returned recovery error, periodic log-facing message, registry JSON, sync payload, and recovery `Debug` output omit every endpoint and API-key token.
The credential-rotation regression changes only the protected API credential while retaining the protected URL.
It proves the recomputed HMAC differs, recovery retains the exact pending record, and zero repository, card, run, or release I/O occurs before the generic endpoint-identity failure.
At `ad74042`, five production-load regressions, 13 dispatch-release regressions, 33 Powder client regressions, the serial 63-test control Powder filter, and the 15-test Harness filter passed.
`cargo clippy -p t-hub --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
No post-remediation workspace-wide test result is claimed.

Commit `5dc37df` replaces the prior unsalted endpoint digest with a standard HMAC-SHA-256 endpoint identity.
The HMAC key is the protected client API credential and is never persisted, synchronized, logged, or returned by the board surface.
`PendingDispatchRelease` stores only `connectionEndpointIdentity`, while recovery decodes that persisted tag and verifies it against a protected-profile HMAC before repository, card, run, terminal, or release I/O.
If the protected credential is unavailable, the identity cannot be recomputed and dispatch or recovery fails closed before remote work.
Schema version 13 rejects every version 12 release recovery, including the prior `connectionEndpointDigest` shape, as incompatible state that remains preserved and write-blocked.
This does not describe the keyed value as categorically credential-free.
The endpoint remap regression continues to prove exact change detection and zero wrong-scope I/O.
The new board snapshot regression uses a token-bearing protected path, query, and fragment and proves `external.url` contains only the public origin plus `/board`.
The matching-endpoint transport regression now uses the keyed identity and still proves error, periodic reconciliation, registry, sync payload, and `Debug` output contain no secret.
At `5dc37df`, five production-load regressions, eleven dispatch-release regressions, 32 Powder client regressions, four board-snapshot regressions, the serial 63-test control Powder filter, and the 15-test Harness filter passed.
`cargo clippy -p t-hub --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
No post-remediation workspace-wide test result is claimed.

Commit `b359faf` closes matching-endpoint transport-error endpoint disclosure.
`ureq::Error::Transport` now preserves the typed `Unreachable` classification while returning the bounded endpoint-free message `Powder transport failure`.
This prevents `ureq`'s full failing URL rendering from entering recovery errors or periodic reconciliation logs.
`external_board_url` now emits only the validated scheme and authority followed by `/board`, excluding protected profile path, query, and fragment material from board sync output.
The Powder client transport regression uses a matching loopback endpoint with token-like path, query, and fragment values and proves the error and `Debug` rendering contain none of them while retaining `Unreachable`.
The dispatch-release recovery regression uses the same matching-identity transport failure, invokes periodic reconciliation, and proves the recovery error, registry JSON, sync payload, and recovery `Debug` output contain no endpoint or API-key token.
At `b359faf`, the four production-load regressions, eleven dispatch-release regressions, 31 Powder client regressions, and serial 63-test control Powder filter passed.
`cargo clippy -p t-hub --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
No post-remediation workspace-wide test result is claimed.

Commit `698955e` closes the production-load fail-open for incompatible dispatch-release recovery state.
`SnapshotReadError` now distinguishes incompatible recovery from generic corrupt state.
Loading either a primary or backup with nonempty pre-v12 recovery, unknown release fields, or a release record that cannot deserialize or validate preserves both files byte-for-byte, starts with no actionable registry state, and blocks every write until explicit safe handling.
It does not fall back to a stale backup and does not quarantine either file.
`PendingDispatchRelease` uses `deny_unknown_fields`, and the pre-deserialization classifier recognizes a schema-13 raw `connectionEndpoint` alongside a valid keyed identity without including that raw value in the error.
Production-load regressions cover a schema-11 release primary without a backup, the same primary beside an older clean backup, an incompatible release recovery in backup beside a current primary, and a schema-12 release record with a token-bearing raw endpoint field.
Each proves registry writes and redispatch fail, reconciliation performs zero card evidence, run evidence, or release requests, no apply event is emitted, no fallback overwrite or quarantine occurs, and the primary and backup bytes remain unchanged.
At `698955e`, the four production-load regressions, two schema regressions, ten dispatch-release regressions, the serial 63-test control Powder filter, 30-test Powder client filter, and 15-test Harness filter passed.
`cargo clippy -p t-hub --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
No post-remediation workspace-wide test result is claimed.

An authenticated `th send` report to Captain session `0c7b7560` was attempted from final evidence head `001650a31d1088d24c370ad9d90882075fce5442`.
The installed control plane rejected it with gated code 5 because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

An authenticated `th send` report for the incompatible-load remediation was attempted from evidence head `323278735f11cd84c8d49c8fcd18fcafe00a570a`.
The installed control plane again rejected it with gated code 5 because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

An authenticated `th send` report for the matching-endpoint transport remediation was attempted from evidence head `684c2c4b3f6d6e6e17f0cc50ef87221790cb5c58`.
The installed control plane again rejected it with gated code 5 because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

An authenticated `th send` report for the keyed endpoint identity remediation was attempted from evidence head `4ec3d44c714f19b1cc8db801107336f6d1ea1cb3`.
The installed control plane again rejected it with gated code 5 because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

An authenticated `th send` report for the HTTP status-body remediation was attempted from evidence head `8481f8eccf351e65238546e0837ea431c04a2365`.
The installed control plane again rejected it with gated code 5 because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

An authenticated `th send` report for the HMAC endpoint-verification remediation was attempted from evidence head `64b09c3fc1863c07daa6fd003b0503b64c56971a`.
The installed control plane rejected it because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.
The exact-run work-log append was attempted once with stable operation identity `work-log:wave0-hmac-verify-8619968`.
The sanctioned CLI returned `unsupported` because deployed run-bound capability verification failed before any mutation.
No work-log append is claimed as delivered.

Final-gate preflight at approved code head `ef3a893877ab7e84dbf0b0800e58f010e5befd18` confirmed the exact HEAD, no tracked or indexed changes, only protected untracked `CLAUDE.md`, and no active Cargo or `t_hub_lib` test process.
From `apps/desktop/src-tauri`, `export T_HUB_TMUX_SOCKET=t-hub-wave0-ef3a893-$PPID` followed by `timeout 180s "$BIN" 'tmux::tests::' --nocapture --test-threads=1`, with `BIN=target/debug/deps/t_hub_lib-4e9da78cd4dbc996`, passed 11 serial tests.
The same fresh-socket setup with `timeout 240s "$BIN" 'process_level_permission_attestation' --nocapture --test-threads=1` passed 4 serial tests.
The same fresh-socket setup with `timeout 120s "$BIN" --exact control::tests::dispatch_release_inflight_before_post_survives_restart_without_blind_repost --nocapture --test-threads=1` passed 1 test.
The same fresh-socket setup with `timeout 180s "$BIN" --exact control::tests::dispatch_restart_rejects_contender_without_releasing_successful_winner --nocapture --test-threads=1` passed 1 test.
`node_modules` was absent, so local build setup ran `pnpm install --frozen-lockfile` only.
It accepted the current lockfile, reused all 395 packages without downloads, and did not modify tracked package metadata or lockfiles.
`pnpm --filter t-hub-desktop typecheck` passed.
`pnpm --filter t-hub-desktop test` passed 57 files and 480 tests.
The one permitted current-head workspace gate ran once from `apps/desktop/src-tauri` with `export T_HUB_TMUX_SOCKET=t-hub-wave0-workspace-ef3a893-$PPID` and `RUST_TEST_THREADS=1 timeout --signal=TERM --kill-after=30s 30m cargo test --workspace`.
It did not pass.
The `t_hub_lib` target began its 880-test run and reported failures in `apply_forwards_are_broadcast_to_event_subscribers`, `attach_captain_refuses_read_only_and_preserves_existing_control_capability`, `claim_and_release_are_audited_and_forward_the_captains_snapshot`, `claim_conflicts_liveness_and_bad_release_are_dispatch_errors`, `codex_claim_never_inherits_a_stale_claude_session_id`, and `commission_captain_spawns_binds_bootstraps_and_deduplicates`.
The command's execution-output cell detached while that single process continued, so its final aggregate pass, fail, and later-test counts are not recoverable from the local output channel.
After the process exited, no Cargo or `t_hub_lib` process remained.
This gate was not rerun.
No production code was changed because the observed broad-gate failures were not reproduced or attributed to the approved HMAC endpoint-verification remediation.
The failed current-head workspace gate remains a release-blocking residual requiring diagnosis before any completion decision.

No push, protected-branch merge, install, restart, deploy, publish, release, or Powder completion was performed.
Three independent reviewers approved code head `ef3a893877ab7e84dbf0b0800e58f010e5befd18` with no remaining code findings before the final-gate execution documented above.
The failed current-head workspace gate blocks completion and requires diagnosis despite that code-review approval.

An authenticated `th send` report of the final-gate results was attempted from evidence head `90113eef630436cbba54519f8eee1f45eaded457`.
The installed control plane rejected it because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

At `1c069ab42c5b932866bab790f9f3d4dd75f50c70`, the six legacy real-tmux tests were reproduced individually on the fresh isolated socket `t-hub-wave0-1c069ab-legacy-repro` with `RUST_TEST_THREADS=1`, `timeout 120s`, `--nocapture`, and `--test-threads=1`.
`apply_forwards_are_broadcast_to_event_subscribers` failed at `src/control.rs:19394` when its final `close_terminal` unwrap received `tmux kill-session-tree failed (exit 1): server exited unexpectedly`.
`attach_captain_refuses_read_only_and_preserves_existing_control_capability` failed at `src/control.rs:22113` with the same final close error.
Its read-only capability assertion completed before teardown, so no startup or Alive wait was added.
`commission_captain_spawns_binds_bootstraps_and_deduplicates` failed at `src/control.rs:21906` with the same final close error.
`claim_and_release_are_audited_and_forward_the_captains_snapshot` failed at `src/control.rs:22701` with the same final close error.
`codex_claim_never_inherits_a_stale_claude_session_id` failed at `src/control.rs:22768` with the same final close error.
`claim_conflicts_liveness_and_bad_release_are_dispatch_errors` failed at `src/control.rs:22841` with the same final close error.
These results confirm the test-fixture final-session teardown race and do not expose a production control, Powder, HMAC, schema, or permission-attestation behavior defect.

Commit `74e5e52` changes only the `#[cfg(test)]` module in `src/control.rs`.
It acquires the existing `ProcessAttestationTmuxGuard` at the beginning of exactly those six tests, keeping a separate anchor session alive while each test's own terminal is closed.
It preserves every tested `close_terminal` call and does not change production `tmux.rs`, error handling, liveness classification, or any production source.
On fresh socket `t-hub-wave0-1c069ab-legacy-fixed`, each of the six exact tests then passed individually with `RUST_TEST_THREADS=1`, an external 120-second timeout, `--nocapture`, and one test thread.
The requested serial fresh-socket control matrix ran once on `t-hub-wave0-1c069ab-control-focused` with `RUST_TEST_THREADS=1`, `timeout 600s cargo test -p t-hub control::tests:: -- --nocapture --test-threads=1`.
It began 364 tests and its captured portion was green, including all six remediated fixtures.
Its output channel detached before Cargo's final aggregate was returned, while the one test process continued and then exited.
The serial matrix was not rerun, so no complete 364-test aggregate is claimed.
On fresh sockets, the existing tmux matrix passed 11 tests and the process-level permission-attestation matrix passed 4 tests.
`cargo fmt --all -- --check`, `cargo clippy -p t-hub --all-targets -- -D warnings`, `git diff --check`, and `git diff --cached --check` passed.
The workspace gate was not run again.
This focused test-only remediation requires a fresh exact-head independent review before any completion decision.
An authenticated `th send` report for this focused remediation was attempted from evidence head `94cdc62c718f28a3024bff19f4b09786f6d12fe4`.
The installed control plane rejected it because `send_text` requires control capability and this Crew token is read-only.
No Captain message is claimed as delivered.

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
24. Verify incompatible pending-release recovery in either primary or backup blocks writes and redispatch, preserves both files without fallback or quarantine, and exposes no actionable cleanup state.
25. Verify a schema-13 recovery rejects unknown or raw endpoint fields before client construction and cannot emit credential-bearing bytes through registry, sync, errors, or logs.
26. Verify matching-endpoint transport failures retain only an endpoint-free typed classification and external board links omit protected profile path, query, and fragment data.
27. Verify the persisted endpoint identity is standard HMAC-SHA-256 keyed only by the protected client credential, never described as credential-free, and fails closed when it cannot be recomputed.
28. Verify every HTTP status-body failure is generic and typed, cannot echo protected endpoint or API-key material through recovery or periodic logs, and credential rotation rejects recovery before network I/O.
