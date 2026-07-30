# Simplify Cortana to a reattach-or-create shell singleton

Created: 2026-07-29.
Status: item 1 (the Windows empty-cwd spawn fix) landed in PR #92.
The Cortana simplification is BUILT and green on `refactor/cortana-singleton`, and is not yet verified in a Windows build.

This is the approved implementation plan.
It supersedes Phase 3 of `docs/OPTIMIZATION-PLAN-2026-07-29.md`, which said to root-cause `observe-managed-runtime-owner exit 91`.
The decision recorded here is to remove the mechanism instead of debugging it.

## What the implementation changed relative to this plan

Three things came out differently, and each is a correction to the plan rather than a deviation from its intent.

1. `cortana_reconcile.rs` is NOT deleted; it is reduced to the persisted data model (244 lines, no logic, no tests).
Deleting it outright would have taken the dormant field types with it, and the plan itself requires those fields to stay parseable.

2. The Fleet claim is no longer published by T-Hub.
`claim_captain` requires a LIVE harness in the terminal, and the singleton starts a plain shell, so there is nothing to claim at create time.
The agent the user starts is what claims, which means `enforce_attach_authority` had to change: the RECORDED singleton may now assign the Cortana role to its own terminal.
Without that the role was unreachable, because only an already-authoritative Cortana could assign it.
This was not anticipated in the plan and is the one genuine authority change in the work.

3. Authority no longer requires evidence about the RUNNING PROCESS, only about the record and the terminal.
This surfaced in the `mcp_e2e` continuity gate, whose Cortana fixture is a durable record with a live terminal and an active Fleet claim but no attested managed process.
That fixture used to be refused; it is now admitted, because the attestation that distinguished it is exactly what was removed.
The mutation path still independently requires the terminal to be live (`exact_live_identity_terminal`), so the residual surface is the one this plan already names: a same-user process occupying that exact tmux session.
Worth stating plainly because it is a widening, not merely a test edit.

4. The two-pass lock dance is gone rather than preserved.
Reconcile now takes dispatch admission, then the identity transaction, then provisioning, for its whole run - the same single order `commission_captain` uses.
The old inspect-under-provisioning-then-re-enter pass existed to keep a 30-second poll off the admission lock; the reconcile is now short enough that it is not worth the complexity.
`concurrent_captain_commission_and_cortana_recovery_follow_one_lock_order` was rewritten to prove the surviving property (the two interleave without wedging) instead of the retired mechanism.

## Context

Cortana is currently broken and has been for a long time.
It fails every reconcile with `observe-managed-runtime-owner exit 91` ("systemd, cgroup, process, nonce, and tmux ownership did not agree") followed by `retire-prepared-managed-runtime exit 120`.
The durable record has reached generation 16 with 15 revoked identities, which is the signature of a launch path that has been thrashing for a long time.
Earlier today the reconcile loop was failing 2,891 consecutive times in a single session, and its retry cost was degrading the whole app until it was backed off in PR #90.

The root cause of exit 91 was never established, and that is deliberate: rather than keep debugging a mechanism we do not need, remove the mechanism.

The reason the machinery is complex is narrow and specific.
`cortana_reconcile.rs` (1,008 lines) plus the systemd scope / cgroup / nonce / harness attestation path exists to **discover runtimes in Cortana's home directory and cryptographically vet whether each one is legitimately ours**, then rank them on a generation ladder and quarantine duplicates.
Several evidence fields are annotated in-code with "must never authorize adoption", which shows how much of the design is about safely adopting things T-Hub did not launch.

The intended outcome: Cortana becomes a singleton shell that T-Hub either reattaches to or creates, with no discovery and no vetting, because T-Hub only ever trusts the terminal id it wrote down itself.

## What Cortana becomes

Exactly two states on T-Hub load:

1. **Reattach.** The durably recorded Cortana terminal id has a live tmux session, so adopt it and refresh its control environment.
2. **Create.** No live session, so create exactly one tmux session in the orchestrator home running a plain login shell, and record its terminal id.

No agent auto-start.
No auto-resume.
T-Hub guarantees one shell exists in the orchestrator home; the user decides what runs in it.

Anti-duplication comes from the durable record plus an in-flight guard, not from discovering and quarantining extras.

## Why this is safe

Capability is not weakened.
Cortana's control capability comes from its identity secret and that identity's role, gated in `acl.rs` on `AclRole::Cortana`.
The attestation machinery never granted capability; it only decided which discovered runtime was the authoritative Cortana.

The trust posture actually narrows.
Today T-Hub will adopt a runtime it did not launch, if that runtime passes vetting.
After this change it adopts only the exact terminal id it previously recorded, and only when tmux confirms that session is alive.
The residual exposure is that another process under the same user could occupy that specific tmux session and be adopted without cryptographic checks.
The tmux socket is already user-owned, so this is a same-user boundary, and it is a smaller surface than discovery-plus-vetting.
This is a deliberate decision and should be recorded in the code comments where the adoption happens.

## A side benefit worth capturing

`tmux::set_session_environment` (`apps/desktop/src-tauri/src/tmux.rs:719`) can refresh a live session's environment.
Applying it on the reattach path fixes a separate long-standing problem: after an app restart a surviving Cortana came back with a read-only MCP token, because the control endpoint address and token rotate on every restart while the session's cached environment did not.
A newly started agent in a refreshed shell picks up the current endpoint, so the manual re-seat workaround stops being necessary.

## Policy: Cortana versus captains

Captains already have the lifecycle this plan wants, in `ClaimState` (`apps/desktop/src-tauri/src/control/captains_registry.rs:5946`):

- `Active`: live and pointed at a terminal.
- `Orphaned { since }`: terminal unambiguously gone, identity and crew retained indefinitely for re-adoption.
- `Vacant`: released but re-claimable.

Cortana should behave the same way, differing only in four respects:

| aspect | captains | Cortana |
| --- | --- | --- |
| cardinality | many | exactly one |
| creation | on demand | auto on T-Hub load |
| cwd | per ship | fixed to the orchestrator home |
| identity role | Captain | Cortana (carries control capability) |

Orphan retention, reattach, and resume-by-id semantics should be identical, not parallel implementations.
This plan does not refactor Cortana onto the shared `ClaimState` type, because that touches the captain registry that other work depends on.
It aligns the semantics and leaves the type unification as a follow-up.

## Effect on captains that already exist: none

This is worth stating plainly because it is the natural worry.

Captains and Cortana already use separate durable state.
Captains live in the `captains` array with their own `provider_session_id`, `conversation_id`, `resume_point`, and `ClaimState`.
Cortana lives in its own `cortana` block with the generation and attestation fields.
Everything this plan deletes is inside the `cortana` block or in Cortana-only code paths.

Existing captains therefore keep working unchanged: same claim states, same crew retention, same resume ids.
The plan deliberately does NOT refactor Cortana onto the shared `ClaimState` type, precisely so the captain registry other work depends on is not disturbed.
Aligning the two onto one type is a sensible follow-up once Cortana is stable, not part of this change.

The one shared file touched is `control/captains_registry.rs`, and only to remove the Cortana attestation mutations.
`rebind_pruned_cortana_identity` must be kept, since the identity self-heal still needs it.

## Command structure per harness, verified against the installed CLIs

Verified against codex-cli 0.146.0 on this machine, and against `harness/codex.rs` and `harness/claude.rs`.

Full-bypass posture (`PermMode::BypassPermissions`, the crew and captain default):

| | fresh | resume |
| --- | --- | --- |
| Codex | `codex --dangerously-bypass-approvals-and-sandbox '<prompt>'` | `codex resume --dangerously-bypass-approvals-and-sandbox '<id>'` |
| Claude | `claude --dangerously-skip-permissions '<prompt>'` | `claude --resume '<id>' --dangerously-skip-permissions` |

Note the two structural asymmetries, which is what makes this feel inconsistent from the outside:

1. Codex resume is a SUBCOMMAND (`codex resume <id>`); Claude resume is a FLAG (`claude --resume <id>`).
2. Flag ORDER differs. Codex puts permission flags BETWEEN `resume` and the id; Claude puts them AFTER the id. See `harness/codex.rs:63` versus `harness/claude.rs:56`.

Correction on `--yolo`: it is NOT available on the installed Codex.
`codex --help` on 0.146.0 does not list it, and `harness/codex.rs:111` documents the choice explicitly ("never the `--yolo` alias, which is absent from the installed help").
The long flag `--dangerously-bypass-approvals-and-sandbox` is the correct form and is what T-Hub emits.
`--yolo` is still recognized by the attestation layer (`harness/mod.rs:1604`, `2426`) as a bypass-equivalent and CONFLICTING option, so a captain launched by hand with `--yolo` would be flagged as not matching the expected posture.
If `--yolo` support is actually wanted, that is a separate decision about accepting it as a valid attested form.

Other postures, for completeness:

| posture | Codex | Claude |
| --- | --- | --- |
| AcceptEdits | `--sandbox workspace-write` (no network, so no `git push`) | `--permission-mode acceptEdits` |
| Default | `--sandbox read-only` | no flag |

## Separate bug found: cannot add a terminal to a workspace on Windows

Reported during planning and root-caused, so it is captured here rather than lost.
This is INDEPENDENT of the Cortana work and should land as its own commit or PR.

Symptom, from the diag log at 2026-07-30T02:47:03 and again at 02:47:06:

```
spawnTerminal failed  spawn_terminal: could not resolve worktree activity: could not resolve WSL path ''
```

Root cause, and it is a genuine Windows-only defect:

- `resolve_cwd` (`apps/desktop/src-tauri/src/commands.rs:238`) deliberately returns an EMPTY string on Windows. That is documented and correct: "On Windows the tmux pane runs inside WSL, so a Windows path would be meaningless as `-c`; default to empty and let the pane inherit wsl.exe's working directory."
- `commands.rs:301-302` then passes that empty cwd straight into `admission_context.admit_worktree_activity(&cwd, "spawn_terminal")`.
- `admit_worktree_activity` (`worktree_coordinator.rs:2248`) calls `canonical_posix_path_allow_missing`, which cannot resolve `''` and returns the error at `files.rs:1475`.

So on Windows, spawning a terminal WITHOUT an explicit cwd fails every time.
The worktree admission gate was added without accounting for the documented empty-cwd case that the same codebase treats as the correct Windows default.

The control-socket path has the same defect from a different direction: `control/handlers_spawn.rs:1233` falls back to `std::env::var("HOME").unwrap_or_default()`, and `HOME` is typically unset on Windows, so it also yields an empty string. Its own comment admits the gap: "Mirror `commands::resolve_cwd`'s unix arm ($HOME fallback)" - it mirrors only the unix arm and has no Windows arm. It also does not filter a `Some("")` sent by a caller, which `resolve_cwd` does filter.

Fix direction, to be decided during implementation:

- Preferred: resolve a real WSL-side default for Windows (the WSL user home) so admission has an actual path to gate on, and filter empty or whitespace cwd in the control handler the way `resolve_cwd` already does.
- Fallback: have `admit_worktree_activity` treat an empty candidate as "no worktree to gate" and return a no-op guard. Simpler, but it means a spawn with an unknown cwd skips worktree-retirement protection, so the no-op must be explicit and commented rather than incidental.

Verification: on Windows, add a terminal to a workspace with NO cwd specified and confirm it spawns. Add a regression test for the empty-cwd case at both entry points, since they fail for different reasons.

Also seen once, at 2026-07-29T23:19:47, and worth a look while in this area but not necessarily the same bug:

```
workspace registry sync failed: Workspace report attempted to redesignate
Captain terminal 'a7f950b0' outside startup reconciliation
```

Raised at `control.rs:2072`. It may be a legitimate guard firing on a stale report, or a real ordering problem in the layout up-sync. Investigate before assuming either.

## Resume, for reference (not automated by this plan)

Worth recording because it was researched and it explains why auto-resume is unnecessary rather than impossible.

Conversations survive a machine reboot; tmux sessions do not.

| harness | resume command | transcript location |
| --- | --- | --- |
| Codex | `codex resume '<id>'` (subcommand) | `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, date-scoped, cwd inside the `session_meta` payload |
| Claude | `claude --resume '<id>'` (flag) | `~/.claude/projects/<mangled-cwd>/<uuid>.jsonl`, cwd encoded in the directory name |

Both forms already exist behind `trait HarnessAdapter` (`apps/desktop/src-tauri/src/harness/mod.rs:409`), including `resume_cortana_argv`, implemented at `harness/codex.rs:57` and `harness/claude.rs:45`.
The practical difference is that Claude resolves sessions relative to the directory it is launched from, while Codex looks up globally by date.
For Cortana this never matters, because its cwd is fixed.
`history/catalog.rs` already indexes both providers and derives cwd from transcript contents, and `history_resume` already carries durable pre-spawn reservations for idempotency.
So a user-driven resume is fully supported today with no new code.

## Implementation

### 1. New Cortana lifecycle in `control.rs`

Replace `reconcile_cortana` and its helpers with a single function whose whole job is: resolve the orchestrator home, read the durable terminal id, check tmux liveness, then either refresh-and-adopt or create-and-record.

Reuse, do not reimplement:

- `resolve_orchestrator_home` / `orchestrator_home` (`control.rs:8808`, `8827`) for the fixed cwd.
- `tmux::new_session_with_env` (`tmux.rs:815`) for creation. This is the same call the crew and captain spawn path uses at `control/handlers_spawn.rs:1448`, so Cortana stops having a bespoke launch path.
- `tmux::has_session` / `tmux::session_liveness` for liveness. Treat only a definitive `Gone` as absent; an `Unknown` probe is a degraded control plane and must not cause a second shell to be created. This distinction already exists and is the de-conflation that fixed an earlier spawn wedge.
- `tmux::set_session_environment` (`tmux.rs:719`) to refresh the control endpoint on adopt.
- `identity::IdentityStore` mint / `get` / `bind_tile` / `is_revoked` for the Cortana-role identity, injected as session env at creation so any agent later started in the shell inherits control capability.
- `CAPTAIN_WORKSPACE_ID` placement so the tile lands in the reserved workspace as it does today.

Keep writing `identityId`, `terminalId`, and `conversationId` when known.
Stop writing `generation`, `owner`, `managedLaunch`, `activeHarnessAttestation`, `activeHarnessAttestationRecovery`, and `quarantineLedger`.

Preserve the identity self-heal already shipped in PR #89: a durable `identity_id` that no longer resolves should be re-minted and rebound, while a revoked id still fails closed.
That logic is small and correct and should survive the rewrite.

### 2. Delete the discovery and attestation machinery

- `apps/desktop/src-tauri/src/cortana_reconcile.rs`: delete. 1,008 lines, 19 unit tests. This is the generation ladder, quarantine planning, and candidate vetting.
- `control.rs`: remove `discover_cortana_runtimes` (around line 9424) and the candidate/quarantine/orphan-replacement helpers around lines 9686-10510.
- `control/captains_registry.rs`: remove the prepare / observe / commit / retire mutations for managed launches and attestation, roughly 87 references. Keep `rebind_pruned_cortana_identity` (added earlier today) since the self-heal still needs it.
- `tmux.rs`: remove `new_managed_session_with_env` (line 920), `observe_managed_runtime_owner`, `retire_prepared_managed_runtime` (line 2321), `prepared_converged` (line 1889), `revalidate_managed_runtime_owner`, and the embedded Python cgroup-effect helper along with its exit-code vocabulary. Confirm no non-Cortana caller depends on these before removing.
- `harness/mod.rs`: remove the Cortana launch-attestation surface. Keep `resume_cortana_argv` and `fresh_cortana_argv`, which are just command construction.

### 3. Persisted schema

Keep the `CortanaDurableIdentity` struct field shape parseable so the existing `captains.json` still loads, but stop reading and writing the removed fields.
Drop the invariant that ties `identity_id.is_some()` to `generation > 0`, since generation stops being maintained.

This avoids a migration on a versioned, invariant-checked file that currently holds real state, at the cost of a few dormant fields.
The alternative, physically removing the fields and bumping `CAPTAINS_SCHEMA_VERSION`, is cleaner but needs a tested migration path for the existing file and is not worth coupling to this change.

### 4. Frontend

`apps/desktop/src/lib/ensureOrchestrator.ts` keeps the monitor and its failure backoff from PR #90, but the result contract simplifies.
`CortanaReconcileResult` currently requires `generation >= 1` for a healthy result (`parseCortanaReconcileResult`); that check must change since generation is no longer maintained.
Actions collapse from `keep | adopt | recover | create | degraded` to `adopt | create | degraded`.

The dismissable banner from PR #89 stays as-is.

### 5. Tests

Delete the tests that cover deleted behavior: `control/tests/cortana_launch.rs` (18 tests, 2,891 lines) and `control/tests/cortana_quarantine.rs` (16 tests, 2,538 lines) are almost entirely managed-launch and quarantine coverage.
Rewrite `control/tests/cortana_bootstrap.rs` (10 tests, 650 lines) against the new contract.

New tests must cover, at minimum:

- Create when nothing exists, and the terminal id is durably recorded.
- Adopt when the recorded session is alive, with no second session created.
- Idempotency: two reconciles in a row produce exactly one session.
- A definitively gone session creates exactly one replacement, not one per attempt.
- An `Unknown` liveness probe does NOT create a second shell.
- The reattach path refreshes the control environment.
- A pruned identity is re-minted and rebound; a revoked identity fails closed. This preserves the PR #89 regression test.

Keep the existing test named `reconcile_cortana_remints_a_pruned_durable_identity_but_not_a_revoked_one`, adapting it to the new flow.

## Verification

Backend, in `apps/desktop/src-tauri`:

```
cargo clippy --lib
cargo test --workspace --lib -- --skip control::tests --skip tmux::tests
cargo test -p t-hub --lib control::tests
cargo test -p t-hub --lib tmux::tests
```

Run `control::tests` directly; it needs a live tmux and is not in the fast lane.
Run the new tests in parallel as well as serially, since process-global state has caused order-dependent failures in this codebase.

Frontend, in `apps/desktop`: `pnpm typecheck && pnpm lint && pnpm vitest run`.

End to end on Windows, which is the part that actually matters here:

1. Build and install per `docs/OPTIMIZATION-PLAN-2026-07-29.md` section 7, remembering the two uncommitted `tauri.conf.json` edits (`"targets": ["nsis"]`, `"createUpdaterArtifacts": false`).
2. Fresh case: confirm no Cortana session exists, launch T-Hub, and verify exactly one new session appears in the orchestrator home with a login shell, and that `captains.json` records its terminal id.
3. Reattach case: restart T-Hub with that session alive, and verify it is adopted with NO second session created. This is the regression that matters most.
4. Duplicate-prevention case: restart several times in a row and confirm the session count stays at one. Compare against today's behavior, where each failed attempt leaked a session and a systemd scope.
5. Env refresh case: after a restart, start an agent in the adopted shell and confirm it has control capability rather than a read-only token.
6. Confirm no `t-hub-*.scope` systemd units are created at all: `systemctl --user list-units 't-hub*' --all` should stay empty.
7. Confirm the reconcile stops erroring: `grep "Cortana recovery failed" /mnt/c/Users/natha/.t-hub/diag.log` should produce nothing new.

## Suggested landing order

1. DONE, PR #92. The Windows empty-cwd spawn fix. Small, independent, and it unblocks a thing that is broken right now. Do not couple it to the Cortana work.
2. The Cortana simplification, as one reviewable PR.
3. Optional follow-up: unify Cortana onto the shared `ClaimState` type.

## Notes for whoever executes this

- This is a large deletion in a security-sensitive area with about 44 existing control tests. Land it as one reviewable PR, not folded into perf work.
- Before deleting anything from `tmux.rs`, confirm no non-Cortana caller depends on it. The managed-runtime helpers appear Cortana-only but that must be checked, not assumed.
- Record the trust-posture decision in a comment at the adoption site, so the next reader knows the vetting was removed deliberately and why.
- Update `docs/OPTIMIZATION-PLAN-2026-07-29.md`: this supersedes Phase 3, which currently says to root-cause exit 91. Note in the Progress Log that the mechanism was removed rather than debugged.
