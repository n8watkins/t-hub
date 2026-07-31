# T-Hub Optimization Plan

Created: 2026-07-29.
Last updated: 2026-07-29.
Status: Phases 0, 1, 2 and 4 not started. Phase 3 is SUPERSEDED by `docs/CORTANA-SIMPLIFICATION-PLAN.md`.

This document is the working plan for the remaining performance and correctness work found during the 2026-07-29 optimization review.
It is written to be picked up with NO prior conversation context.
Update the Progress Log at the bottom as work lands, and change the Status line above.

---

## 1. How to use this document

Read sections 2 through 4 first.
Section 2 tells you what is already fixed, so you do not re-investigate solved problems.
Section 3 gives the measured baselines, which are the only way to tell whether a change actually helped.
Section 4 lists the traps that already cost real effort on this codebase.

Then pick the lowest-numbered unfinished phase in section 5 and work it.
Each phase states its own definition of done and how to verify it.

---

## 2. Already shipped on 2026-07-29 (do not redo)

All of the following are merged to `main` and installed as 0.3.152.

| PR | change | evidence it worked |
| --- | --- | --- |
| #89 | Cortana durable-identity self-heal | Verified in production: dead pointer `694bdd27...` was replaced by `42de3ef0...` and the durable record rebound |
| #89 | Dismissable Cortana error banner, with per-message suppression | UI only |
| #89 | Lazy-loaded the settings panel out of the boot graph | Measured: entry chunk 1,269.14 kB to 1,186.45 kB |
| #89 | Unpark fast path for workspace switching | Markers confirm `switch:unparked fast:true` in production |
| #90 | Cortana reconcile backoff, 30s to a 5 minute cap | rust-main blocks went from 73/hour to 0 in a 10 minute post-install sample |
| #90 | Paced the managed-owner poll, 10ms to 250ms | Latent hazard only, see the scope note in that commit |
| #90 | hangwatch attribution (`lastCommand`, `cmdsDuringBlock`) | Instrument, not a fix |
| #91 | Typed terminal's echo skips the shared emit schedule | User confirmed: "the typing is no longer slow" |
| #91 | Removed a DUPLICATE output throttle | Global output rate was 5 events/sec instead of the documented 10 |
| #91 | Background pacing compresses to 10ms during a typing window | Removes head-of-line blocking ahead of the echo |

The typing latency complaint that drove most of this session is RESOLVED.

### The most important lesson from #91

`emit_reader_event` was calling `throttle_output_emit()` on top of the dispatcher's own call, and the dispatcher is its only caller.
Every output event therefore reserved two 100ms slots.
This also silently defeated the first version of the typing fix, because a terminal exempted at the dispatcher was immediately re-throttled inside `emit_reader_event`.
Tests were green on that no-op.
The bug was found only by going back to verify the fix rather than trusting a green suite.

---

## 3. Measured baselines

These came off a real Windows build. Compare against them; do not compare against intuition.

### Startup

| marker | value | note |
| --- | --- | --- |
| `boot:entry` | 787 ms | Entry chunk parse plus 14 side-effect mount modules |
| `boot:first-paint` | 792 ms | React mount itself is nearly free |
| `boot:inventory` (fast case) | 707 ms, 2 walks | About 350 ms per walk |
| `boot:inventory` (slow case) | 7,995 ms, 2 walks | About 4,000 ms per walk, cause UNEXPLAINED |
| `boot:reconcile-authoritative` | 1,806 ms fast / 8,928 ms slow | No authoritative layout before this |
| JS heartbeat block at boot | 3,850 ms | Heap grew 23 MB to 83 MB across it |

### Workspace switching

| marker | value |
| --- | --- |
| `switch:unparked` | 587 ms and 610 ms per tile, both with `fast:true` |
| Switch inside the 2s park grace | No detach at all, no `unparked` marker, instant |

### Main-thread blocks, before the #90 and #91 fixes

73 rust-main blocks in one hour, 28,946 ms total, largest 6,211 ms.
A `keydown` was observed blocked 296 ms directly behind a 6,211 ms block, and another at 208 ms.
Only 13 of the 73 correlated with the Cortana reconcile, so the reconcile was never the dominant cause.

### Bundle

Entry chunk is 1,186.45 kB after the panel split.
Remaining weight is xterm (489 kB unminified, needed at first paint), react-dom, and core app code.
`lucide-react` already tree-shakes via named imports.
The 3.7 MB `@iconify-json/vscode-icons` set is correctly lazy and is NOT in the boot graph.
Bundle size is close to tapped out as a startup lever; the panel split recovered only 6.5 percent.

---

## 4. Instrumentation available

### Phase markers

`dmark(phase, detail)` in `apps/desktop/src/lib/diag.ts` is ALWAYS ON, unlike the gated `tlog`.
It is rare by construction: a boot emits under a dozen, a switch one per visible tile.
Never call it per frame, per output chunk, or inside the pool sync loop.

Existing phases: `boot:entry`, `boot:first-paint`, `boot:inventory`, `boot:reconcile-attempt`, `boot:reconcile-authoritative`, `switch:begin`, `switch:unparked`.

Read them with:

```
grep '"t":"mark"' /mnt/c/Users/natha/.t-hub/diag.log
```

Note that diag lines are prefixed with an ISO timestamp before the JSON, so a naive `json.loads(line)` fails.
Strip the prefix first.

### Main-thread hang attribution

`{"t":"hang","src":"rust-main",...}` now carries `lastCommand` and `cmdsDuringBlock`.
`lastCommand` is recorded at the seven surviving SYNCHRONOUS `#[tauri::command]` sites, since Tauri runs those on the main thread.
A block attributed to `none` is itself a signal: it points away from the command layer and toward emit dispatch or the event loop.

### JS-side detector

`{"t":"hang","src":"heartbeat"|"event"|"longtask"}` comes from the renderer.
`src:"event"` with `name:"keydown"` is the direct measurement of typing latency.
Only blocks at or above roughly 200 ms are recorded, so this UNDERSTATES typical latency.

---

## 5. Phases

### Phase 0: zero-risk cleanup

No investigation required. Do this first.

1. DONE 2026-07-30 for four of the six, and the other two were assessed WRONG here.
   Deleted after verifying the content is on `main` by finding the actual code, not by ancestry (`git cherry` and `--merged` are both useless on this repo - every commit bumps the version, and everything lands squashed):
   `perf/lazy-settings-panel` (lazy/Suspense settings panel, `dmark`, `switch:unparked` all present), `refactor/split-control-tests` (its whole diff reverse-applies to main; the origin copy was deleted too), `fix/terminal-stability` (`RemotePty::is_alive` plus its two callers, `TERMINAL_COLD_AFTER_MS = 300_000`, the Canvas poll split, the prune deferral), and `review/control-continuity-remediation-903435e` (identical tree to `crew/control-continuity-remediation`).
   COMPLETED 2026-07-30: all 14 remaining local branches were triaged by CONTENT and deleted; only `main` is left locally.
   Every one was already landed - eight conclusively (identical trees/blobs, or their whole diff reverse-applies to main), five by full symbol coverage with main a strict superset (`audit-log-real`, `crew/package3-preview-service`, `fix/startup-reconciliation`, `fix/startup-reconciliation-release`, `wip/agent-followup-review`), and `integrate/package56-preflight-29bc` in a BETTER form: main carries the `a155` variant, which fixes a transposed-jq-field bug the 29bc copy still has.
   CORRECTION to the note that previously stood here: the `integrate/*-a155` vs `-29bc` file differences are their BASE, not their work - those 7 files are exactly the Powder-removal set, and the two `terminal-stability` twins' own content deltas are byte-identical to each other. Only `package56-preflight` differed in its own content, and that difference was the bug.
   The two large branches were the ones worth checking rather than assuming: `wip/agent-followup-review` (49 commits) had 18 of 39 files byte-identical to main, 203 introduced Rust symbols with exactly one absent, and 129 added functions with none absent; `fix/startup-reconciliation-release` (23 commits) had all 16 `remote_pty.rs` symbols present, its two non-matches being main's own refinements (a `u64` sequence guard replaced by a stronger compare-and-swap).
   ONE piece of content was lost with the prune, deliberately: `OwnedTransportFailure` in `wip/agent-followup-review` (`control.rs`, ~40 lines) - a test fixture that holds a bound loopback listener and accepts-then-closes every connection, so an "unreachable endpoint" is DETERMINISTIC under parallel tests and no later fixture can claim the released ephemeral port.
   Its consumers died with the Powder project-board handler, so it was orphaned. It is recorded here rather than ported because porting a fixture with no consumer is speculative - but if an unreachable-endpoint test is ever needed, that pattern is the one to rebuild, and see the unreachable-wait trap in section 7 for why it matters.
   Two latent test-infra defects on main surfaced during the triage and remain OPEN: `ControlContext::new` still defaults `live_sessions` to the real `tmux::list_sessions()` shell-out with no `#[cfg(test)]` variant (unlike its `provider_capacity` neighbours), so spawn-path tests that omit `.with_live_sessions(...)` still need live tmux; and five `current_exe()` re-exec sites are unfixed (`apps/cli/src/control.rs`, `crates/t-hub-agent/src/journal.rs`, `crates/t-hub-mcp/src/control_client.rs`, and two in `tests/mcp_e2e.rs`).

Definition of done: branches deleted, both code changes in one build, nothing regressed.
Neither code change is separately measurable, so verification is only that the app still behaves.

### Phase 1: the re-seed cluster

This is the largest measured remaining cost, and two symptoms share one mechanism.

The mechanism is `term.reset()` followed by a `capture-pane` re-seed.
It costs 587 to 610 ms per tile on a workspace switch.
At boot every terminal does it at once, which is the 3,850 ms JS-thread block and the 23 MB to 83 MB heap growth.

Steps:

1. First, split the measured 587 to 610 ms into round-trip time versus seed-write time. Add a sub-timer around the seed write and report it in the existing `switch:unparked` marker detail. Do not skip this: the fix below is only correct if the seed write actually dominates.
2. Implement stay-attached. Keep the PTY attached when a tile is parked, and instead suppress output events for background terminals at the dispatcher in `apps/desktop/src-tauri/src/remote_pty.rs`. Resume emission when the tile is foregrounded.
3. Enforce that only the foreground client asserts a tmux size. Sessions are pinned to `window-size latest` (see `apps/desktop/src-tauri/src/tmux.rs` around line 776), so a long-lived background client that asserts a size could win the negotiation and mis-size the visible pane.

Known risk: this touches `apps/desktop/src/components/Terminal.tsx`, which carries a lot of hard-won handling for attach loss, muted frames, and geometry.
It also needs a new re-seed command, because no existing Tauri command returns scrollback without re-attaching.

Definition of done: `switch:unparked ms` drops materially below 587, and the boot JS heartbeat block shrinks.
Both are already instrumented, so no new tooling is needed to verify.

### Phase 2: startup latency, measurement first

This phase deliberately does NOT start with a fix.
`boot:inventory` measured 707 ms on one boot and 7,995 ms on another, an eleven-fold variance that is not understood.
Choosing a fix before explaining that variance is how effort was wasted earlier in this work.

Steps:

1. Measure per-walk timing, and record whether the persistent WSL agent was connected at the time. The hypothesis to test is that the slow case is the cold-agent fallback scan rather than the agent snapshot path. `list_terminals` prefers the agent, see `apps/desktop/src-tauri/src/commands.rs` around line 843.
2. Only then choose a fix. Candidates: render tiles immediately from the persisted layout and reconcile behind first paint, reduce per-walk cost, or do nothing if the fast path turns out to be typical.
3. Independently, defer the non-visual side-effect mount modules past first paint. They currently run inside the 787 ms pre-paint window. Candidates in `apps/desktop/src/main.tsx`: `updateMount`, `autoContinueMount`, `rulesMount`, `voiceAnnounceMount`, `engineStatusMount`.
4. Time the five synchronous store loads inside the Tauri `setup` function, `apps/desktop/src-tauri/src/lib.rs` roughly lines 778 to 908, before changing them. These are on the window-creation path and read from the Windows home directory.

EXPLICITLY OUT OF SCOPE: removing or shortening the inventory convergence loop in `stableLiveTerminalIds`, `apps/desktop/src/ipc/controlBridge.ts` around line 459.
That loop is deliberate and three tests lock its behavior, including one that asserts exactly three inventory calls during continuous terminal churn.
An earlier attempt to simplify it would have broken those tests and re-opened the races they cover.

Definition of done: `boot:first-paint` and `boot:reconcile-authoritative` improve against the section 3 baselines.

### Phase 3: Cortana exit 91

SUPERSEDED on 2026-07-29 by `docs/CORTANA-SIMPLIFICATION-PLAN.md`.
That plan removes the discovery and attestation mechanism rather than debugging why it disagrees with itself, so steps 1 through 3 below are no longer the work.
The reasoning is that the machinery exists to discover runtimes in Cortana's home and cryptographically vet whether each is legitimately ours, and T-Hub does not need that: it can trust only the terminal id it wrote down itself.
The rest of this section is kept as the record of what the failure looked like.

Independent of every other phase and shares no files, so it can run in parallel with Phase 1.

Cortana is still down.
Everything shipped on 2026-07-29 stops its failure from degrading the rest of the app; none of it stands Cortana up.

The failure is `observe-managed-runtime-owner exit 91`, reported as "systemd, cgroup, process, nonce, and tmux ownership did not agree", followed by `retire-prepared-managed-runtime exit 120`, "prepared managed unit was populated, reused, or unverifiable".

Steps:

1. Read the prepare, observe, and retire path end to end and determine WHICH of the five agreement checks fails. Do not guess. One hypothesis, that a stale orphan scope was blocking observation, was tested by stopping the orphan and disproved when failures continued.
2. Fix the structural gap that IS confirmed: `prepared_converged` in `apps/desktop/src-tauri/src/tmux.rs` around line 1889 only VERIFIES that a unit has already converged to dead. It never stops one. So a still-running scope can never be retired, and the durable write-ahead record stays in "cleanup pending" forever with no self-recovery.
3. Investigate why the durable record reached generation 16 with 15 revoked identities. That churn is a symptom worth understanding, not cosmetic.

Useful state to inspect, on Windows:

```
/mnt/c/Users/natha/.t-hub/captains.json     (the cortana block)
/mnt/c/Users/natha/.t-hub/identities.json   (identities and the revoked set)
systemctl --user list-units 't-hub*' --all
```

Definition of done: Cortana reaches `healthy` with a live terminal, AND a deliberately failed launch self-cleans instead of wedging.

### Phase 4: switch write amplification

Changing which tab is visible currently performs four durable writes: a synchronous `localStorage.setItem`, a SQLite snapshot, a shared JSON file write, and a control round-trip to report the layout.

Debounce the active-tab persist by roughly 300 ms.
A tab switch has no data worth protecting against a crash.

Relevant sites: `setActiveTab` in `apps/desktop/src/store/workspace/tabs.ts` around line 248, `savePersisted` in `apps/desktop/src/store/workspace.ts` around line 275, and the subscribe that fires `reportWorkspaceTabs` in `apps/desktop/src/ipc/controlBridge.ts` around line 1160.

Last because it is the smallest measured effect, and Phase 1 may change the picture.

---

## 6. Working discipline

Six diagnoses were wrong during the 2026-07-29 session before the real causes were found.
Every one of them came from moving faster than the evidence.
These rules are the corrective, and they are cheap.

1. Record the baseline BEFORE changing anything. The markers in section 4 exist for this.
2. One phase per build. Batching is what allowed a duplicate-throttle no-op to nearly ship as a fix.
3. Verify against the baseline, not against a green test suite. The suite was green on the no-op.
4. Run new tests in PARALLEL, not only serially. A process-global added during this work caused a reliable cross-test failure that a serial run hid.
5. Measure rates, not instantaneous counts. A `wsl.exe` "leak" of 231 and then 478 processes turned out to be churn; sampling every 1.5 seconds over 18 seconds showed the count was mostly zero.
6. When a claim does not survive measurement, correct the record, including amending already-pushed commits and rewriting pull request descriptions.

---

## 7. Traps already discovered on this codebase

Keep these in mind; each one cost real time.

- Platform-asymmetric cost. On Linux a tmux or systemd probe is a cheap local call; on Windows each one is a `wsl.exe` process spawn. A 10 ms poll loop that is harmless in dev and in the test suite becomes a spawn storm in production.
- `git cherry` is unreliable here. Every commit bumps the version, which breaks patch-id matching. Verify landed content by checking for the actual code on `main`, not by ancestry or cherry.
- Version bump per commit means cherry-picking across branches always conflicts on `package.json`, `Cargo.toml`, `Cargo.lock`, and `tauri.conf.json`. Cherry-pick with `-n`, reset those four files, then re-run `apps/desktop/scripts/bump-version.sh`.
- The app installs PER USER at `%LOCALAPPDATA%\T-Hub`, not into `Program Files`.
- The local Windows build needs two uncommitted edits to `tauri.conf.json`: `"targets": ["nsis"]` and `"createUpdaterArtifacts": false`. An rsync from the repo overwrites them, so re-apply after syncing.
- `control::tests` requires a live tmux and is slow. It is covered by the `process` lane, not the `fast` lane. Run it directly for changes to `control.rs` or `tmux.rs`.
- `cargo test --workspace --lib` is NOT the gate. The `--lib` flag skips every integration-test target, which is where `mcp_e2e` lives - the suite that gates Cortana and captain authority end to end. Two green `--lib` runs still went red on CI for that reason. Run `apps/desktop/scripts/workspace_gate.sh full`, which is exactly what CI runs, plus `pnpm test:browser` when any frontend contract changes.
- `pnpm test:browser` starts its own vite server on port 4180. A failed local run reporting `ERR_CONNECTION_REFUSED` is a dead server, not a test failure; re-run the whole `pnpm test:browser` command rather than a single Playwright case, and read the CI log for the real assertion.
- Tests that mock `../lib/diag` must mock every export they touch. Three test files mocked only `tlog`, so adding `dmark` made any call throw inside an attach try-block, which surfaced as a bogus triple-attach.
- Tests that mock `../lib/diag` must mock every export they touch. Three test files mocked only `tlog`, so adding `dmark` made any call throw inside an attach try-block, which surfaced as a bogus triple-attach. It happened a SECOND time when `diagEnabled` was added, so those three mocks now spread the real module (`...(await importOriginal())`) instead of enumerating exports; keep them that way.
- Do not arm an open-ended `until` wait on a condition you have reason to believe will not occur. Two such waits ran for 5 hours 41 minutes and 3 hours 37 minutes during this session before being noticed, each burning a sleep-polling shell. One waited for Cortana to become healthy, which is the very thing Phase 3 exists to fix. The other waited for hang-attribution lines that the session's own fixes had made impossible to produce. Both conditions were unreachable at the moment the wait was armed. Use a bounded wait with a timeout so an unreachable condition fails loudly instead of spinning silently, and prefer a single check plus a decision over an indefinite poll when the thing being waited on is itself under investigation.

---

## 8. Progress Log

Append an entry per landed change. Keep it short and factual, and include the measured effect.

- 2026-07-29: Plan created. Phases 0 through 4 defined. Nothing started.
- 2026-07-29: Added the unreachable-wait trap to section 7 after two background waits were found spinning for 5h41m and 3h37m on conditions that could not become true. No code change.
- 2026-07-29: Fixed a Windows-only defect where adding a terminal with no explicit cwd failed every time.
  Baseline: three `spawn_terminal: could not resolve worktree activity: could not resolve WSL path ''` lines in the live diag log, and three new tests that reproduce that exact string against the pre-fix code.
  `commands::resolve_cwd` returned an empty string on Windows by design, and the worktree admission gate cannot canonicalize `''`.
  `resolve_spawn_cwd` now resolves the real WSL home through `files::user_home_path` (the same resolver `orchestrator_home` uses), the control-socket handler shares it instead of mirroring only its `$HOME` arm, and `WorktreeCoordinator::admit_activity` accepts an empty candidate as an explicitly unscoped admission for the degraded case.
  Unscoped is the conservative direction, not a hole: `path_within` answers `true` for an unresolvable candidate, so such an admission is refused while any retirement is active and blocks a new one from starting.
  Not a phase in this plan; it is item 1 of the Cortana simplification plan, landed separately as agreed.
- 2026-07-29: Cortana was rebuilt as a reattach-or-create shell singleton on `refactor/cortana-singleton` (0.3.153), superseding Phase 3.
  Baseline: 3,194 `Cortana recovery failed` lines in the live diag log, a durable record at generation 16 with 15 revoked identities, a `managedLaunch` stuck in `prepared` that nothing could ever retire, and one orphaned `t-hub-*.scope` still running.
  The exit-91 root cause was deliberately not established: the mechanism that fails exists to discover and vet runtimes T-Hub did not launch, and it is not needed, so it was removed instead of debugged.
  Net -7,600 lines, with the compiler confirming no non-Cortana caller depended on the tmux managed-runtime cluster.
  Green: 811 fast-lane, 405 `control::tests`, 30 `tmux::tests`, 636 vitest, clippy and typecheck clean; the new tests pass in parallel and serially.
  CORRECTION (0.3.154): that verification was incomplete and CI caught two suites it missed, both of them asserting the retired contract.
  `cargo test --workspace --lib` skips every integration-test target, so `mcp_e2e` (which gates Cortana authority end to end) and the Playwright browser suite were never run.
  Use `apps/desktop/scripts/workspace_gate.sh full` - the script CI itself runs - not a narrower `--lib` invocation.
  The `mcp_e2e` update is a genuine widening and is recorded in the plan doc: authority now rests on the durable record plus a live terminal, with no evidence required about the process running inside it.
  NOT yet verified in a Windows build, which is the only measurement that decides whether Cortana is actually up.
- 2026-07-30: Phase 0 code changes plus the Phase 1 step-1 measurement, on `perf/phase0-and-switch-timing`.
  `save_shared_layout` moved to `spawn_blocking` (an `async fn` doing blocking `std::fs::write` on every tab switch), with its synchronous half now unit-tested.
  The four pool-sync `tlog` sites are guarded by an exported `diagEnabled()`, and `rectStr` is only built when diagnostics are on: it cost several `Math.round` calls per terminal per sync with diagnostics off.
  `switch:unparked` now carries the split the phase needs before choosing a fix - `attachMs` (control round trip carrying the seed), `drainMs` (write-queue wait), `resetMs` (`term.reset()`), `replayMs` + `replayBytes` (seed enqueue, not paint).
  Neither code change is separately measurable, per the phase's own definition of done; the marker split is what the next Windows install should be read for.
  Green: `workspace_gate.sh full`, clippy `-D warnings`, fmt, typecheck, 636 vitest, 7 Playwright.
- 2026-07-30: Phase 0 branch prune, four of six deleted (see the corrected step 1 above).
  The plan's claim that three branches were exact duplicates of a sibling held for one of them and was wrong for the other two, which is why the prune was verified branch by branch rather than executed as written.
- 2026-07-30: Branch prune completed - all 14 remaining local branches deleted after content verification; only `main` remains locally. PRs #67 and #73 closed as superseded by the Cortana singleton rewrite, with slice-by-slice evidence recorded on each. 82 remote branches on origin are untouched.
