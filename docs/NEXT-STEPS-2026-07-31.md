# T-Hub next steps

Created: 2026-07-31.
Written to be picked up with NO prior conversation context.

Companion documents, read in this order if you are new to the state:
`docs/OPTIMIZATION-PLAN-2026-07-29.md` (the performance and correctness plan, its measured baselines, and the traps that already cost real time)
and `docs/CORTANA-SIMPLIFICATION-PLAN.md` (why Cortana was rebuilt and what changed relative to the plan).

---

## Where things stand as of 2026-07-31

Nothing is in flight.
`main` is 0.3.162, the installed Windows build is 0.3.162, there are zero open pull requests, and the only branches anywhere are `main` and the deliberate `native-archive` (also tagged `native-pivot-final`).

Landed and verified in production on 2026-07-30:

| change | evidence it worked |
| --- | --- |
| Windows empty-cwd spawn fix (#92) | a no-cwd terminal spawned into `~`; no `could not resolve WSL path ''` since |
| Cortana rebuilt as a reattach-or-create singleton (#93) | `terminalId` recorded, `recovery: healthy`, wedged `managedLaunch` and the 15-entry quarantine ledger cleared, zero `t-hub-*.scope` units |
| reattach, not duplicate | after a restart the SAME tmux session was adopted (unchanged creation time), same identity, no second shell |
| fresh create | closing the shell produced exactly one replacement, which appeared on its own |
| tile renders and stays put (#95, #96, #97) | tile in Captain Workspace, zero `Workspace placement denied` since 0.3.157 |
| sidebar designation (#98) | `cortana:first-healthy` marker read 6.2s on the following boot, versus a 64s wait before |

Five separate gates had assumed "Cortana means an active Fleet claim", which stopped being true when the auto-claim was removed.
All five are fixed; the pattern is worth remembering, because every one passed a green backend suite and was only found by using the app.

---

## 1. Verify the Cortana claim path

This is the LAST unverified claim in the whole rewrite, and it is the one that decides whether the read-only-MCP problem is actually solved.
Everything else has production evidence; this has none.

**Do this:** start `codex` or `claude` inside the Cortana tile (Captain Workspace).

The agent should read the seeded `~/.t-hub/orchestrator/AGENTS.md`, derive its own terminal id from `tmux display-message -p '#S'` (strip the `th_` prefix), and call:

```
claim_captain { "captainSessionId": "<id>", "role": "cortana", "provider": "codex" | "claude" }
```

**What to check afterwards:**

```
python3 -c "import json; c=json.load(open('/mnt/c/Users/natha/.t-hub/captains.json')); \
print([x for x in c['captains'] if x.get('role')=='cortana'])"
```

- A Cortana claim exists, `state: Active`, pointing at the Cortana terminal id.
- The agent has CONTROL capability, not read-only. This is the read-only-MCP question: the endpoint and session token rotate on every app start, and the reattach path re-injects them with `tmux::set_session_environment_many`. If the agent comes back read-only after a restart, that fix did NOT work and the manual re-seat workaround is still needed.
- The sidebar shows Cortana with its crown.

**If the claim is refused** with `only General/Cortana may assign the Cortana role or slug`, the agent is not running in the recorded orchestrator shell.
Check `cortana.terminalId` in `captains.json` against the tile it is actually in, rather than forcing it.

Relevant code: `apps/desktop/src-tauri/src/control/cortana.rs` (the lifecycle), `enforce_attach_authority` and `recorded_cortana_singleton` in `control.rs` (the gate), `apps/desktop/src-tauri/resources/orchestrator-agents.md` (the seeded instructions).

---

## 2. Read the switch-timing split, then decide Phase 1

Phase 1 of the optimization plan proposes "stay attached": keep the PTY attached when a tile is parked and suppress background output at the dispatcher.
That is only the right fix if the control round trip dominates the cost.
The plan is explicit that choosing before measuring is how effort was wasted earlier in this work, so the measurement now exists and should be read first.

**After some normal use of 0.3.162:**

```
grep '"phase":"switch:unparked"' /mnt/c/Users/natha/.t-hub/diag.log | tail -5
```

Each marker now carries the split:

| field | what it measures |
| --- | --- |
| `ms` | the total, which baselined at 587-610 ms per tile |
| `attachMs` | the control round trip that carries the scrollback seed |
| `drainMs` | waiting out the write queue before the grid can be cleared |
| `resetMs` | `term.reset()` itself |
| `replayMs` | ENQUEUEING the seed, not painting it |
| `replayBytes` | how much seed was enqueued, so `replayMs` can be read honestly |

**How to decide:**

- If `attachMs` dominates, Phase 1 as written is the right fix and worth starting.
- If the time is in `drainMs` or `resetMs`, the problem is local xterm work and stay-attached would not help much; the fix is elsewhere and Phase 1 should be re-scoped before any code is written.
- If `replayBytes` is large, the seed size itself is the lever.

Note that `replayMs` is deliberately enqueue-only.
Making it measure paint would require adding a second `waitForWrites()` to the unpark path, which instrumentation has no business doing.

Phase 1 also carries a known risk recorded in the plan: it touches `apps/desktop/src/components/Terminal.tsx`, which holds a lot of hard-won handling for attach loss, muted frames, and geometry.

---

## 3. Smaller things that are open

- **Phase 2** (startup latency) and **Phase 4** (switch write amplification) in the optimization plan are unstarted. Phase 2 begins with a measurement, not a fix: `boot:inventory` was 707 ms on one boot and 7,995 ms on another and that eleven-fold variance is not explained.
- **Two latent test-infra defects on main**, found during the branch triage and still open:
  `ControlContext::new` (`control.rs`) defaults `live_sessions` to the real `tmux::list_sessions()` shell-out with no `#[cfg(test)]` variant, unlike its `provider_capacity` neighbours, so spawn-path tests that omit `.with_live_sessions(...)` silently need live tmux;
  and five `current_exe()` re-exec sites remain unfixed (`apps/cli/src/control.rs`, `crates/t-hub-agent/src/journal.rs`, `crates/t-hub-mcp/src/control_client.rs`, and two in `tests/mcp_e2e.rs`).
- **The 64s first-reconcile** seen once on 0.3.157 was never explained. It did not reproduce (6.2s on the next boot) and the sidebar no longer depends on it, but `cortana:first-healthy` is in place to catch it if it returns.
- **`/fleet-orchestrator`** (in `~/.claude/skills/`, outside this repo) now instructs the agent to claim. The seeded `AGENTS.md` covers Codex and Claude automatically, so the skill is only needed for the full doctrine.

---

## 4. Rules that still apply

These are the corrective for six wrong diagnoses during this work, and they are cheap.

1. Record the baseline BEFORE changing anything.
2. One change per build. Do not batch, and do not put an unrelated change in a build whose purpose is verifying something else.
3. Verify against the baseline, not against a green test suite.
4. `cargo test --workspace --lib` is NOT the gate. `--lib` skips every integration target including `mcp_e2e`, which gates Cortana authority end to end. Two green local runs still went red on CI for exactly this. Run `apps/desktop/scripts/workspace_gate.sh full`, plus `pnpm test:browser` when any frontend contract changes.
5. Backend green does not mean it works in the app. Five UI-visible Cortana bugs all passed a green suite because the tests asserted the registry and the durable record, both of which were correct every time, and never asserted what the UI was told.
6. `control::tests` needs live tmux and false-reds under load. Re-run before believing a red, and run the case in isolation to confirm.
7. When a claim does not survive measurement, correct the record, including amending pushed commits and rewriting pull request descriptions.
