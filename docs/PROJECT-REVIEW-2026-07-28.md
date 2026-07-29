# Project review - 2026-07-28

A full-project review of `t-hub-app` at `main` @ `8ba599fc` (v0.3.140).
Covers code health, correctness and security, and runtime behaviour.

## Headline

Nothing is broken.
Every quality gate is green, and the security core is genuinely well built.
The problems are structural debt, and the single largest one is not in the source tree at all.

### Gate results (all run locally on 2026-07-28)

| Gate | Command | Result |
| --- | --- | --- |
| Fast lane | `pnpm test:fast` | pass |
| Standard lane | `pnpm test:standard` | pass, 72s |
| Process lane | `pnpm test:process` | pass, 4m58s |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | **0 warnings, 0 errors** |
| Rust format | `cargo fmt --all -- --check` | enforced in CI, clean |
| TypeScript | `tsc --noEmit` | clean |
| Frontend unit | `vitest run` | 632 passed / 69 files |
| CI on `main` | `.github/workflows/test.yml` | green, run `30362303919` |

### Security assessment: solid, no findings

The control-socket authorization surface was reviewed directly and holds up.

- `ct_token_eq` (`control.rs:4059`) is a real constant-time comparison.
- `resolve_capability` (`control.rs:4230`) hard-caps any non-loopback peer to `ReadOnly` even when it presents the full control token.
- An empty read token authorizes nothing, guarded explicitly at `control.rs:4232`.
- The empty-control-token path is closed at the source: `lib.rs:318` applies `.filter(|token| !token.is_empty())` to `T_HUB_CONTROL_TOKEN`.
- `open_handshake_no_follow` (`t-hub-mcp/src/control_client.rs:422`) opens the handshake with `O_NOFOLLOW`, refusing symlink substitution.
- The audit log is an HMAC-SHA256 keyed chain with a production verifier and fail-closed writes (`audit.rs`).

One defence-in-depth nit only, not exploitable today: `resolve_capability` guards `read_token.is_empty()` but has no matching guard for `ctx.token.is_empty()`.
Every production construction path populates it, so this is a one-line symmetry fix, not a vulnerability.

---

## Findings

Ordered by impact.
Items 2 and 3 are in progress as of this document; the rest are open.

### 1. Roughly 41 GB of stale worktrees, and one of them holds real unmerged work

There are 18 registered worktrees consuming **42 GB**, of which **41 GB is Cargo `target/`** output.

Largest offenders:

| Size | Worktree |
| --- | --- |
| 14 GB | `captain-control-continuity/` |
| 8.4 GB | `terminal-stability/` |
| 5.7 GB | `powder-tests-cleanup/` |
| 5.2 GB | `pr84-merge-95745c64-b241d5d3/` |
| 5.2 GB | `pr84-merge-95745c64-4974535f/` |

Every branch was checked for unmerged content.
All are fully merged **except one**, which is covered in finding 2 below.

**Methodology warning, important for any future cleanup.**
`git cherry` is unreliable on this repository and produces false "unmerged" verdicts.
The cause is the every-commit version-bump policy: each commit also touches `package.json`, `Cargo.toml`, and `tauri.conf.json`, which changes the patch-id and defeats equivalence detection.

`fix/terminal-stability` is the clearest example.
`git cherry` reports all 6 of its commits as unmerged.
File-by-file comparison shows the opposite:

- `Canvas.tsx` and `identity.rs` are byte-identical to `main`.
- `remote_pty.rs::is_alive()` is on `main` at line 392 with the same doc comment, and `main` extends it further via `commands.rs:510 remote_reuse_is_healthy(reusable, probe_ok, resize_ok, conn.is_alive())`.
- `terminalLifecycle.ts` on `main` is a strict superset, adding a forced `onChange()` when the count cap demotes warm terminals.

The correct verification is a content diff, not `git cherry`:

```sh
diff <(git show <branch>:<path>) <(git show main:<path>)
```

Only two worktrees are dirty, and only with untracked scratch files (`STABILITY-FIXES.md`, a stray `CLAUDE.md`).
Nothing tracked would be lost.

### 2. `fix/startup-reconciliation-release` holds about 21 commits of unmerged work

This is the one worktree that must **not** be pruned, and it is arguably a finding in its own right.

The branch sits 23 commits ahead of `main` and 184 behind, last touched 2026-07-26.
Content verification confirms the work never landed.
`main`'s `apps/desktop/src/components/Terminal.tsx` has not been modified since 2026-07-14.

What `main` has: background terminals stay attached with their output flush throttled.

What the branch adds on top, and `main` lacks:

- `BACKGROUND_PTY_DETACH_MS` plus `scheduleBackgroundDetach` / `clearBackgroundDetach` / `shouldKeepOutputAttached`, which actually **detach the PTY stream** for background terminals after a 2 second grace period rather than merely throttling the flush.
- `apps/desktop/src/components/Terminal.backgroundPty.test.tsx`, 233 lines, absent from `main` entirely.
- Terminal spawns and workspace reconciliation moved off the window thread.
- Startup reconciliation race fixes and retry preservation.
- Terminal output flood throttling kept below Windows saturation.
- Bounded Windows remote-PTY reader shutdown.
- Documentation updates to `README.md`, `docs/HISTORY-CONTRACT.md`, `docs/MCP.md`, `docs/ORCHESTRATOR-OPERATING-MODEL.md`, `docs/PERF-AND-DRAG-WORKLOG.md`, `docs/SMOKE-TEST.md`.

This work is directly relevant to the reported slow-load and terminal-disconnect symptoms.
**Action: triage into a PR or make an explicit decision to abandon it. Do not delete the worktree until that decision is made.**

### 3. The test-suite imbalance is real, and it is one file

| Language | Production lines | Test lines | Ratio |
| --- | --- | --- | --- |
| Rust | 55,268 | **87,155** | **1.58 : 1** |
| TypeScript | 42,408 | 13,402 | 0.32 : 1 |

The TypeScript ratio is healthy.
The Rust ratio is inverted, and it concentrates almost entirely in one place.

`apps/desktop/src-tauri/src/control/tests.rs` is **27,150 lines, 986 KB, 443 test functions**.
That is 31 percent of all Rust test code in a single file.

The structural defect is precise.
A prior refactor split `control.rs` into 14 production submodules, every one under 1,600 lines, and left every test behind:

```
handlers_spawn.rs      1,524 lines ->   0 tests
handlers_admin.rs      1,571 lines ->   0 tests
captains_registry.rs   6,797 lines ->   0 tests
... all 14 submodules  ->   0 tests
tests.rs              27,150 lines -> 443 tests
```

The test file is larger than all 14 production submodules combined (16,754 lines).

Test length distribution is not the problem.
The median test is 38 lines and 364 of 443 are under 75 lines.
But four are unmaintainable:

| Lines | Test |
| --- | --- |
| 837 | `agent_delivery_command_keeps_completion_and_release_states_distinct` |
| 686 | `managed_cortana_with_lost_session_authority_is_replaced_after_restart_without_signal` |
| 540 | `scoped_harness_attestation_rejects_live_process_substitution_and_allows_tool_children` |
| 330 | `captured_observed_launch_reload_and_duplicate_reconcile_finalize_once` |

**The count is not the problem.**
1,316 lib tests against 55,268 production lines is roughly one test per 42 lines, which is normal.
Deleting tests would trade real coverage for a metric.
The fix is to split the file along the 14 module boundaries it already mirrors.

Note: the two `cleanup/powder-*` branches were an earlier attempt at exactly this, removing 17,322 lines from `tests.rs`.
Both are now 300-plus commits behind `main` and their content is already merged, so they are not a usable starting point.

### 4. `pnpm test` gives a false green

Both `test:fast` and `test:standard` run with `--skip control::tests --skip tmux::tests`.

That skips **488 of 1,316 lib tests, 37 percent**, including the entire socket, ACL, spawn, and admin surface.
Only `test:process` and `test:full` cover them, and they take about 5 minutes.

CI runs the full gate via `workspace_gate.sh` with no argument, which defaults to `full`, so this is not a hole in the merge path.
But the command a developer reaches for by default does not exercise the most dangerous code in the repository.

Suggested fix: rename the profiles so the default is honest, or make `standard` include the process lane and introduce a separate explicitly-named quick lane.

### 5. No JavaScript or TypeScript linting exists anywhere

There is no ESLint, no Prettier, and no Biome configuration in the repository, and no lint step in `.github/workflows/test.yml`.
This covers 42,408 lines of production TypeScript and React.

Rust gets `cargo clippy -D warnings` plus `cargo fmt --check`.
TypeScript gets `tsc --noEmit` and nothing else.

The concrete cost: the Canvas defect fixed in `de6aa82a` was a `useEffect` that listed `focusedId` in its dependency array, which tore down and rebuilt a 15 second poll interval and spawned a `wsl.exe` subprocess on **every tile click**.
`react-hooks/exhaustive-deps` is exactly the rule that surfaces that class of defect.

### 6. Live bug: WSL-side control discovery is stale and nothing detects it

Reproduced on the development machine during this review:

```
~/.t-hub/control.json          -> 127.0.0.1:45949  (Jul 19, pid 2141630)  <- connection refused
/mnt/c/Users/natha/.t-hub/...  -> 127.0.0.1:59417  (Jul 28, pid 47880)    <- live
```

The WSL-side handshake was 9 days stale and pointed at a dead port.
Two independent causes combine:

1. **No maximum-age check.**
   `parse_handshake_endpoint` (`t-hub-mcp/src/control_client.rs:403`) rejects a handshake published in the future (`published_at > now + 5min`) but never rejects one that is too old.
   A 9-day-old handshake passes validation cleanly.
2. **The symlink workaround is now impossible.**
   `open_handshake_no_follow` (`control_client.rs:422`) uses `O_NOFOLLOW`, which is correct hardening, but it is what broke the previously documented `~/.t-hub/control.json` symlink workaround.
   The replacement, a plain file copy, rots silently.

The result is the "false app-down" failure mode: any WSL-side consumer on default discovery gets a plausible-looking handshake to a dead port.

Suggested fix: add a staleness bound on `published_at` and a liveness check on the published `pid`, so a stale handshake produces a clear diagnostic instead of a silent hang.
The underlying topology issue is that the app runs as a Windows binary publishing only to the Windows home, while WSL-side consumers read the WSL home.

### 7. Retired subsystems still shaping the codebase

**`devserver.rs` is entirely dead.**
3,291 lines total: 2,075 production plus 1,216 test, containing 37 test functions.
It has **zero references** from anywhere else in the codebase.
`lib.rs:26` describes it as a "retired runner retained only for regression comparison tests".
It is a module that exists solely to test itself.

**Powder is removed but still pervasive.**
The runtime was deleted on 2026-07-23, but roughly **380 references remain** across 8 source files:

| References | File |
| --- | --- |
| 173 | `control/tests.rs` |
| 96 | `control/captains_registry.rs` |
| 81 | `control.rs` |
| 23 | `control/handlers_terminal.rs` |
| 2 | `agent_session.rs` |
| 1 each | `lib.rs`, `control/handlers_worktrees.rs`, `fixtures/captains-schema-18.json` |

The persisted schema still models `PowderWorkBinding`, `PowderMutationIntent`, `PowderMutationKind`, and `PowderWorkState`.
Phase B, the schema migration, was deferred and remains deferred.

### 8. Documentation sprawl

`docs/` holds 64 markdown files totalling 5.2 MB including screenshots.
**48 of the 64 are referenced nowhere outside `docs/` itself.**

Only 16 are reachable from `README.md` or `AGENTS.md`.
The orphan set includes the entire `NATIVE-*` pivot series, for a pivot that was cancelled and archived.

### 9. Three pull requests frozen since 2026-07-12

| PR | Title | State | Size |
| --- | --- | --- | --- |
| #67 | Create Orchestrator | `CONFLICTING` / DIRTY | +948 / -31, 11 files |
| #72 | Keyed tamper-evident audit chain | `CONFLICTING` / DIRTY | +1034 / -105, 4 files |
| #73 | Captains first-class | draft, `MERGEABLE` / CLEAN | +354 / -23, 10 files |

**#72 is superseded and can simply be closed.**
`main` already carries the keyed HMAC chain (`audit.rs:43-47`), `verify_self` (`audit.rs:882`), `startup_integrity_check` (`audit.rs:945`), and the standalone `verify` / `verify_with_head` entry points.

### 10. Test-suite flake surface

125 `sleep`-based waits across the Rust sources, 45 of them inside `control/tests.rs`.
There is no `serial_test` usage and no `nextest`.
Sleep-based synchronization is the mechanism behind several previously observed flakes.

Worth revisiting as part of the finding 3 split: prefer poll-to-deadline over fixed sleeps.

---

## Recommended order

| # | Action | Effort | Risk | Payoff |
| --- | --- | --- | --- | --- |
| 1 | Triage `fix/startup-reconciliation-release` into a PR or abandon it | medium | low | recovers real terminal-stability work |
| 2 | Prune the other worktrees | ~5 min | none | reclaims about 41 GB |
| 3 | Add ESLint with `react-hooks` and wire it into CI | small | low | highest defect prevention per hour |
| 4 | Split `control/tests.rs` along the 14 module boundaries | medium | low, mechanical | makes the suite maintainable |
| 5 | Delete `devserver.rs` | small | none | removes 3,291 lines and 37 pointless tests |
| 6 | Bound handshake age and check `pid` liveness | small | low | kills the false app-down class |
| 7 | Close PR #72, rebase or close #67 | small | none | clears the queue |
| 8 | Make the default test profile honest about what it skips | small | low | removes a false-confidence trap |
| 9 | Prune orphaned docs; finish Powder Phase B | medium | low | reduces noise |
