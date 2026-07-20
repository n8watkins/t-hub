# Depowder Migration Implementation Report

Snapshot time: 2026-07-19T21:01:46-07:00.

## Repository state

The implementation branch is `fix/captain-control-runtime`.

The implementation branch HEAD is `6dbb5fe79b1701e6b017a5e028ca6571f994889e`.

The merge base with `efd3271` is `a86e5bffc030244049943458a422829976c8ae62`, so this branch is not descended from the approved integration head `efd3271a4efcde2b801a4b07fa4316560b8d9d15`.

This ancestry discrepancy remains an explicit release gate.

No rebase, reset, or history rewrite was performed.

## Required component heads

The required component heads were present in local worktrees at snapshot time:

- Collaboration client: `696973d6b5abc5d3fa683092843c5126266925c6` in `powder-collaboration-workflow`.
- Terminal lifecycle: `b535437398230bc0ea2a6a218cd34ba08e36c3df` in `captain-crew-terminal-state`.
- CLI and MCP: `c6f249ca0438780aec7ede62f1f51140deaf78b5` in `powder-cli-mcp-vnext`.
- Approved integration: `efd3271a4efcde2b801a4b07fa4316560b8d9d15` in `powder-collaboration-integration` and `codex-thub-workflow-remediation`.

The worktree inventory was captured with `git worktree list --porcelain`.

The branch-head inventory was captured with `git for-each-ref --format='%(refname:short) %(objectname)' refs/heads`.

All active worktrees were preserved.

## Live terminal state

The live `t-hub` tmux server reported these sessions:

- `th_0d06769a` with one window.
- `th_253a60dc` with one window.
- `th_3f750daf` with one window.
- `th_9f5092dd` with one window.
- `th_b1fc38a6` with one window.

No active terminal was closed, reaped, or reused during this work.

## Registry and installed version

The expected registry path `/home/natkins/.t-hub/captains.json` was not present at snapshot time.

No `captains.json*` backup files were found under `/home/natkins/.t-hub`.

The repository-local desktop version was `0.3.104` before the release-candidate bump.

The repository-local desktop version is now `0.3.105`.

The installed application identity could not be verified from repository-local state.

## Verification

The latest full Rust library run passed with 914 tests and 2 ignored tests.

The CLI test suite passed with 54 tests.

The MCP end-to-end suite passed with 1 active test and 1 ignored test.

The frontend typecheck passed.

Clippy passed with warnings denied for the desktop and MCP packages.

Rust formatting and `git diff --check` passed.

## Release status

The implementation changes are locally verified and committed through `1115035`.

The release-candidate version bump is committed in `30dc360`.

The final active-path audit cleanup is committed in `1115035`.

The MCP-focused suite passed with 15 library tests and 74 binary tests.

The final full Rust library run passed with 914 tests and 2 ignored tests.

The final formatting and diff checks passed.

The branch ancestry mismatch against `efd3271` still requires an explicit integration decision before release.
