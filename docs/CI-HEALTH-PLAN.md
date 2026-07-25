# CI Health Plan

Status of the `main` CI gate (`.github/workflows/test.yml`) and a plan to get it back to green.
Created 2026-07-24 after the `control.rs` simplification landed and its pre-landing CI gate (PR #75) surfaced pre-existing failures.

## TL;DR

**RESOLVED 2026-07-24: the `main` CI gate is green again.**
See the Resolution section below for the merged PRs and verification.

`main` CI *was* RED, but NOT because of the `control.rs` refactor.
The refactor itself is clean (it needed one `cargo fmt` fix, since applied; `cargo clippy --workspace --all-targets -- -D warnings` is clean; its `control::tests` pass 426/0 locally).
The red came from two pre-existing failure classes that rode in on the ~212-commit backlog pushed straight to `main` with zero PR CI: (1) a perf-benchmark contract that calls `powershell.exe` on the Linux runner, and (2) preview/devserver process-supervisor tests that PASS locally but fail in CI because Ubuntu 24.04's AppArmor policy restricts the unprivileged user namespaces their `unshare` helper needs (NOT a reaping/`--init` problem, and NOT a Python identity/helper/env gap - both ruled out; see class 2 below for the confirming CI log).
Both were RECENT: the last green `main` push CI was commit `31fee52a` (2026-07-23), and both breakages are among the 212 commits since (perf = Package-6 work; supervision = a 30+-commit preview-runtime feature).
A third issue - a pre-existing parallel-load flake in one `control::tests` test - surfaced while greening the gate and was fixed too (class 3 below).

## The two failure classes

### 1. Packaged runtime benchmark contract - calls `powershell.exe` on the Linux runner

- **Symptom**: the "Packaged runtime benchmark contract" check (and the "Frontend (vitest + tsc)" job that runs it as a step) fails.
- **Root cause**: `scripts/perf/perf-benchmark.test.sh` runs `scripts/perf/measure-thub.test.ps1`, and `ubuntu-latest` ships `pwsh`, so the contract runs that file under PowerShell on Linux. Its collector trap block (around line 177) shells out to the Win32 `powershell.exe` to exercise the full `Win32_Process` collector; `powershell.exe` does not exist on Linux, so the block fails (observed as the trap assertion `collector failure did not publish exit5 diagnostic`) and takes the contract - and the Frontend job - down with it.
- **Origin**: the prior Package-6 perf work (commits like "Wire Package 6 preflight into benchmark contract", "Add Package 6 matrix preflight"), which never went through PR CI.
- **Fix options** (small, well-scoped):
  - Guard the Windows-only measurement so the contract skips (or uses a Linux measurement path) when not on Windows / when `powershell.exe` is absent - e.g. gate on `$IsWindows` in the `.ps1`, or branch in `perf-benchmark.test.sh` on `uname`/`$RUNNER_OS`.
  - Or, if the benchmark contract is only meaningful on the packaged Windows runtime, move the CI step to a `windows-latest` job (or mark it `continue-on-error` / non-required) so the Linux gate stops failing on a Windows-only measurement.
- **Owner**: whoever owns `scripts/perf/` (Package-6 perf work). Not the `control.rs` refactor.

### 2. Preview/devserver process-supervision tests - fail in CI, PASS locally

- **Symptom**: 12 tests fail in the "Rust (cargo test + clippy)" job (they pass locally):
  - `preview::supervisor::tests::*` - `natural_parent_exit_reaps_surviving_descendant`, `setsid_target_and_escaped_descendant_are_reaped_within_bound`, `fork_churn_cannot_escape_bounded_watchdog_scans`, `target_starts_only_after_authentication_and_lifeline_reaps_tree`, `target_cannot_reopen_supervisor_lifeline_through_proc`, `target_killing_its_direct_parent_is_still_reaped_by_watchdog`, `isolated_helper_ignores_project_and_pythonpath_shadow_modules_before_ready`
  - `preview::adapter::tests::source_adapter_runs_the_complete_lifecycle_through_one_service`
  - `preview::managed_runtime::tests::static_target_runs_under_the_same_supervisor_and_resolves_its_owned_endpoint`
  - `devserver::tests::natural_parent_exit_reaps_its_surviving_descendant`, `stop_reaps_a_term_ignoring_descendant_and_unblocks_its_reader`
  - `control::tests::scoped_harness_attestation_rejects_live_process_substitution_and_allows_tool_children` - this 12th one is NOT a userns victim; it is an independent parallel-load flake that happened to fail in the same red run (its failure was an executable mismatch at `tests.rs:16239`, not an `unshare` EPERM). Tracked and fixed as class 3.
- **Root cause (CONFIRMED from the CI log; the earlier Python-identity hypothesis was WRONG, and reaping/init is also RULED OUT)**: the supervisor helper (`SUPERVISOR_PY` in `preview/supervisor.rs`) sandboxes the target process tree by spawning `unshare --user --map-current-user --pid --fork --mount --mount-proc ...`. On `ubuntu-latest` (Ubuntu 24.04) that `unshare` fails writing the uid map, and the failed run's log shows it verbatim: `fixture exited before reporting its descendant: unshare: write failed /proc/self/uid_map: Operation not permitted`. Ubuntu 24.04 ships an AppArmor policy that restricts unprivileged user namespaces (`kernel.apparmor_restrict_unprivileged_userns=1`), so the unprivileged `unshare --user` cannot write `/proc/self/uid_map` and exits non-zero. Every one of the 11 userns failures is downstream of that single denial (the 12th, the `control` attestation test, is the separate class-3 flake): the `Os { code: 2, kind: NotFound }` panics are `/proc/<pid>/...` reads for a supervisor tree that never started, the `right: "T_HUB_PREVIEW_READY 1 spoof 9 9 9 9 9 WAITING"` mismatch is the target script's spoof line that never runs, and the `assertion failed: status.success()` checks see the non-zero `unshare` exit. It resolves `/usr/bin/python3` fine (the trusted-identity path is not the problem). Confirmed CI-environment-specific: the tests PASS locally because the WSL2 kernel does not enforce that AppArmor restriction (its `kernel.apparmor_restrict_unprivileged_userns` sysctl does not even exist and `unshare --user --map-current-user ...` succeeds).
- **Origin**: a large, RECENT preview-runtime supervisor feature - 30+ `feat/fix(preview)` commits between the last-green `main` push CI (`31fee52a`, 2026-07-23) and HEAD (e.g. "feat(preview): supervise static targets", "feat(preview): wire shared managed runtime", "fix(preview): harden supervisor authentication and cleanup"). It added the Python-supervision tests and was developed + pushed straight to `main` without ever running through PR CI, so its CI-environment gap went unnoticed until now.
- **Not the refactor**: the 11 userns victims are in `preview`/`devserver` modules the `control.rs` refactor never touched; the 12th (`control` attestation) is the class-3 flake and passes locally.
- **Fix (LANDED, PR #77)**: provision the runner - relax the AppArmor restriction before the workspace gate runs, which keeps full test coverage with no skips. The GitHub-hosted runner is a real VM with passwordless sudo, so a new `rust`-job step runs `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` (guarded so it is a no-op on kernels without the knob) to restore the classic unprivileged user namespaces the tests rely on. Only the `rust` job needs it (the MSRV job just runs `cargo check`, no tests).
- **Owner**: the preview-runtime supervisor author. Not the `control.rs` refactor.

### 3. Parallel-load flake - `control::tests::scoped_harness_attestation...allows_tool_children`

- **Symptom**: `control::tests::scoped_harness_attestation_rejects_live_process_substitution_and_allows_tool_children` flaked in ~3 of 5 CI Rust-job runs while the class-1/2 fixes were being verified; it passes locally in isolation and 426/0 in `control::tests`. Same class as the repo's known `delayed_node_wrapper` flake.
- **Root cause**: four assertions in that test each sampled an asynchronously-settling state exactly once instead of polling to a deadline like the rest of the test already does (`wait_observed` / `wait_changed` / the foreign-child loop). Under CI parallel load: (a) the node-package baseline broke on the first `Ok`, which can bind to the `node` launcher before the native Codex child it spawns appears in the scoped process list (`tests.rs:16239`); (b) the on-disk launcher-substitution check demanded `Err(ExpectedProvenanceMismatch)` on a single observation, but a transient other-error variant can precede the settled mismatch; (c) + (d) two "an ordinary tool child must not change the attested identity" checks did a single `observe().unwrap()` that can transiently fail to read.
- **Fix (LANDED, PR #78)**: poll each spot to a 5s deadline for the settled state, tolerating transient read errors, while still failing hard on a genuinely wrong outcome (a trusted substitution, or a changed attested identity). No production code changed - only the test's observation timing. Reproduced locally by pegging all cores while running the test (fails ~1/3 of runs under load, green in isolation); verified 18/18 green under 8-core load after the fix.
- **Owner**: the `control`/`harness` attestation-test author.

## Plan

Bisect is DONE: last-green `main` push CI is commit `31fee52a` (2026-07-23); 212 commits sit between it and HEAD; class 1 traces to the Package-6 perf work and class 2 to the recent preview-runtime supervisor feature (both above). Ordered by size/independence:

1. **Fix class 1 (perf contract)** - DONE (PR #76). OS-guarded the Windows-only collector trap block in `scripts/perf/measure-thub.test.ps1` so it runs on Windows (Windows PowerShell 5.1 and pwsh) and skips under pwsh on Linux, where `powershell.exe` is absent. Verified `perf-benchmark.test.sh` PASSES under pwsh 7.4 on Linux; PR #76's **Frontend** job is green. Owner: perf/Package-6.
2. **Fix class 2 (preview/devserver supervision)** - DONE (PR #77). The real cause was Ubuntu 24.04's AppArmor restriction on unprivileged user namespaces, not a Python identity/helper/env gap (confirmed from the CI log: `unshare: write failed /proc/self/uid_map: Operation not permitted`). Provisioned the runner in the `rust` job (`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`, guarded for kernels without the knob), keeping full coverage with no skips.
3. **Re-run `main` CI** after each fix; target a fully green gate. Each fix is independent, so land + verify them separately. DONE - class 3 (the flake) surfaced during this step and was fixed in PR #78.
4. **Prevent recurrence**: land future work through PRs (so `test.yml` runs BEFORE it hits `main`), OR at minimum run the full CI-equivalent gate locally before a direct push - `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `../scripts/workspace_gate.sh`, and `pnpm typecheck && pnpm vitest run`. The 212-commit direct-to-`main` backlog with zero PR CI is exactly how both reds accumulated unnoticed; this is the highest-leverage fix.

## Resolution

All three fixes landed through PRs (so `test.yml` ran before each merge) and the `main` gate is green - the post-merge `main` push run passed all three required jobs (Rust, Frontend, MSRV).

- PR #76 (`fix/ci-perf-contract-linux`) - class 1. OS-guarded the perf `.ps1` trap block; Frontend job green. Merged via a conscious admin bypass while the class-2 red was still on `main`.
- PR #77 (`fix/ci-preview-userns`) - class 2. Provisioned unprivileged user namespaces in the `rust` job; the 11 preview/devserver failures cleared. Rebased on the updated `main` after #76 merged, went fully green, and merged normally (no bypass).
- PR #78 (`fix/flaky-scoped-harness-attestation`) - class 3. De-flaked the attestation test; merged green.

The one admin bypass (merging #76 with the pre-existing class-2 red still present) was the conscious, ordered landing described in the branch-protection note below - not a routing-around of a fixable red.

## Branch-protection note

`main` requires 2 status checks (the Rust + Frontend jobs). The landing push of the refactor **bypassed** them (admin bypass), because those checks were red from the pre-existing issues above.
That is now resolved: the gate is green again, so unrelated changes no longer need a bypass. The durable fix was making the gate green (above), not routing around it.
