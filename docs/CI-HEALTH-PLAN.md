# CI Health Plan

Status of the `main` CI gate (`.github/workflows/test.yml`) and a plan to get it back to green.
Created 2026-07-24 after the `control.rs` simplification landed and its pre-landing CI gate (PR #75) surfaced pre-existing failures.

## TL;DR

`main` CI is currently RED, but NOT because of the `control.rs` refactor.
The refactor itself is clean (it needed one `cargo fmt` fix, since applied; `cargo clippy --workspace --all-targets -- -D warnings` is clean; its `control::tests` pass 426/0 locally).
The red comes from two pre-existing failure classes that rode in on the ~212-commit backlog pushed straight to `main` with zero PR CI: (1) a perf-benchmark contract that calls `powershell.exe` on the Linux runner, and (2) preview/devserver Python-process-supervision tests that PASS locally but lack their Python identity/helper/env setup in CI (NOT a reaping/`--init` problem - that is ruled out; the runner is a bare `ubuntu-latest` VM).
Both are RECENT: the last green `main` push CI was commit `31fee52a` (2026-07-23), and both breakages are among the 212 commits since (perf = Package-6 work; supervision = a 30+-commit preview-runtime feature).

## The two failure classes

### 1. Packaged runtime benchmark contract - calls `powershell.exe` on the Linux runner

- **Symptom**: the "Packaged runtime benchmark contract" check (and the "Frontend (vitest + tsc)" job that runs it as a step) fails.
- **Root cause**: `scripts/perf/perf-benchmark.test.sh` invokes `scripts/perf/measure-thub.test.ps1`, whose line ~177 calls `powershell.exe`. On `ubuntu-latest` there is no `powershell.exe`, so it errors: `The term 'powershell.exe' is not recognized`.
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
  - `control::tests::scoped_harness_attestation_rejects_live_process_substitution_and_allows_tool_children` (passes locally at 426/0)
- **Root cause (NOT reaping/init - that hypothesis is RULED OUT)**: the Rust job runs on bare `ubuntu-latest` (a VM with a real init), so orphan reaping is fine. `preview::supervisor` is a **Python process supervisor**: it resolves a *trusted Python executable identity* (`trusted_python_identity()` / `revalidate_python_identity`) then `Command::new(python)` execs Python helper scripts (the test helper begins `root, relative, python = sys.argv[1:4]; ... os.execv(python, ...)`). The CI panics are `Os { code: 2, kind: NotFound }` (ENOENT on spawn), `assertion failed: status.success()`, and helper-output mismatches (`right: "T_HUB_PREVIEW_READY 1 spoof 9 9 9 9 9 WAITING"`). So the CI runner lacks the **Python environment / helper / identity setup** these tests need (or resolves a Python whose identity fails the trusted-identity validation). Confirmed CI-environment-specific: `cargo test --lib preview::supervisor::tests::setsid_target_and_escaped_descendant_are_reaped_within_bound` PASSES locally in 0.11s (python3 3.12 at `/usr/bin/python3`).
- **Origin**: a large, RECENT preview-runtime supervisor feature - 30+ `feat/fix(preview)` commits between the last-green `main` push CI (`31fee52a`, 2026-07-23) and HEAD (e.g. "feat(preview): supervise static targets", "feat(preview): wire shared managed runtime", "fix(preview): harden supervisor authentication and cleanup"). It added the Python-supervision tests and was developed + pushed straight to `main` without ever running through PR CI, so its CI-environment gap went unnoticed until now.
- **Not the refactor**: 11 of 12 are in `preview`/`devserver` modules the `control.rs` refactor never touched; the 12th (`control` attestation) passes locally.
- **Fix options** (needs the preview-runtime author, who knows the intended trusted-Python setup):
  - **Provision the CI environment** the tests expect: identify which Python + helper + env (`T_HUB_PREVIEW_*`?) the trusted-identity path requires on `ubuntu-latest` (ubuntu-latest already ships `python3` at `/usr/bin/python3`, so the gap is a specific identity/path/env or an assumed helper, not "python missing"), and add it to the "Install system dependencies" step.
  - **Or gate the host-dependent tests** behind an env flag (e.g. `T_HUB_PREVIEW_SUPERVISION_E2E=1`): run where the setup exists, skip-with-a-`log`-line in CI, plus a tracking issue to restore coverage once the CI env is provisioned.
  - Prefer provisioning (keeps coverage); gate only as an interim.
- **Owner**: the preview-runtime supervisor author. Not the `control.rs` refactor.

## Plan

Bisect is DONE: last-green `main` push CI is commit `31fee52a` (2026-07-23); 212 commits sit between it and HEAD; class 1 traces to the Package-6 perf work and class 2 to the recent preview-runtime supervisor feature (both above). Ordered by size/independence:

1. **Fix class 1 (perf contract)** - SMALL, self-contained, do first. OS-guard the `powershell.exe` call in `scripts/perf/measure-thub.test.ps1` (skip / use a Linux path when `$IsWindows` is false or `powershell.exe` is absent), or branch in `scripts/perf/perf-benchmark.test.sh` on `$RUNNER_OS`/`uname`, or move that CI step to a `windows-latest` job. This alone turns the **Frontend** job green. Owner: perf/Package-6.
2. **Fix class 2 (preview/devserver supervision)** - MEDIUM, needs the preview-runtime author. It is a CI-environment gap (Python trusted-identity / helper / env not present on `ubuntu-latest`), NOT reaping and NOT a code bug (tests pass locally). Path: (a) reproduce one failing test in a clean `ubuntu-latest`-like env to see exactly what path/identity/env is missing; (b) provision it in the "Install system dependencies" step; (c) if it genuinely cannot run in CI, env-gate the host-dependent tests (`T_HUB_PREVIEW_SUPERVISION_E2E=1`) with a `log` skip line + a tracking issue. Prefer (b).
3. **Re-run `main` CI** after each fix; target a fully green gate. Each fix is independent, so land + verify them separately.
4. **Prevent recurrence**: land future work through PRs (so `test.yml` runs BEFORE it hits `main`), OR at minimum run the full CI-equivalent gate locally before a direct push - `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `../scripts/workspace_gate.sh`, and `pnpm typecheck && pnpm vitest run`. The 212-commit direct-to-`main` backlog with zero PR CI is exactly how both reds accumulated unnoticed; this is the highest-leverage fix.

## Branch-protection note

`main` requires 2 status checks (the Rust + Frontend jobs). The landing push of the refactor **bypassed** them (admin bypass), because those checks are currently red from the pre-existing issues above.
Until the gate is green again, either (a) continue to bypass consciously for unrelated changes, or (b) temporarily relax the required-checks set to the jobs that are actually green - but the durable fix is to make the gate green, not to route around it.
