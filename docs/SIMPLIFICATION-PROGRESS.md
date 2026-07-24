# Maintainability Simplification - Plan & Progress

Tracking doc for the ongoing effort to simplify the largest maintenance liabilities in this repo.
Started 2026-07-23; this file is the single reference for what is done and what remains.

## Goal

Reduce the two headline liabilities - the `control.rs` monolith and the `workspace.ts` god-store - plus retire dead code, without changing behavior.
Every change is behavior-preserving, verified (tests / typecheck), committed separately, and path-scoped so it does not sweep concurrent-session edits.

## Progress (done, on `main`)

| Commit | Change | Result |
| --- | --- | --- |
| `236459a` | `control.rs` inline test module -> `control/tests.rs` | 73,435 -> 33,914 |
| `4722060` | `ThemeEditor.tsx` -> `components/theme-editor/` (4 files) | 2,053 -> 1,281 |
| `2924c69` | `control.rs` file/git read handlers -> `control/handlers_files.rs` | -> 33,823 |
| `2e82bc0` | **WS2**: `workspace.ts` god-store -> 9 zustand slices + `internal.ts` under `store/workspace/` | 2,642 -> 667 |
| `30d9102` | **WS1**: `control.rs` history handlers -> `control/handlers_history.rs` | -> 26,717 |

Also verified (authored by a concurrent session): the retired **Powder runtime** was fully removed (`powder.rs` deleted, handlers + tests gone); `main` compiles and the lib test suite is green.
`control.rs` has gone from 73,435 to ~26,717 lines total across this effort.

## Remaining workstreams

### WS1 - split `control.rs` production half into submodules (IN PROGRESS)

Goal: take `control.rs` from ~26,717 to roughly 5,000-6,000 lines (core dispatch + serve loop + shared types/helpers) by moving handler groups into `control/handlers_*.rs`.
Done so far: `handlers_files.rs`, `handlers_history.rs`.

**Remaining groups** (do biggest/hardest first, one verified commit each):
- `captains_registry.rs` (~7,600 lines) - the `impl CaptainsRegistry` state machine + `FleetRole`/`ClaimState` enums.
  Hardest: it is an `impl` block, so its methods (not just free fns) need `pub(super)` where called from the parent, and the struct fields stay accessible because the child module sees the parent's private items.
- `idempotency.rs` (~5,000) - `RequestCache` + provider-capacity evidence + control leases.
- `handlers_status.rs` - `get_status`, `wait_for_status`, `supervision_tree`, `list_agents`, `agent_events`, `dispatch_preflight`.
- `handlers_agents.rs` - `agent_checkpoint`, `agent_followup`, `record_agent_delivery`.
- `handlers_tabs.rs` - `new_tab`/`close_tab`/`rename_tab`/`focus_tab`/`move_tile`/`list_tabs`/`open_file`.
- `handlers_worktrees.rs` - `create_worktree`/`remove_worktree`/`list_worktrees` + git-capability checks.
- `handlers_captains.rs` - `claim_captain`/`release_captain`/`rename_captain`/`report_workspace_tabs`.
- `handlers_fleet.rs` - `watch_fleet`/`unwatch_fleet`/`list_fleet_watches`.
- `handlers_spawn.rs` - `spawn_terminal`/`start_agent`/`commission_captain`/`attach_captain` + spawn-capacity eval.
- `handlers_comms.rs` - `plane_send`/`inbox_ack`/`inbox_status`/`check_authorization`.

**Stays in `control.rs`**: `ControlContext`/`ControlRequest`/`ControlResponse`/`ControlHandshake`/`EventFanout`/`TabRegistry` types; the serve/listen loop (`start`, `serve`, `handle_conn`, `serve_pty_attach`); discovery/handshake + identity/token resolution; the dispatch entry points (`dispatch_authenticated`, `dispatch`, `dispatch_with_caller`, `required_tier`); and shared helpers (`arg_str`, `deny`, error taggers, `now_ms`).

**The proven extraction pattern** (replicate exactly):
1. Pick a cohesive, contiguous handler cluster.
   Confirm contiguity by listing every `^fn` in the range - no unrelated functions interleaved.
2. Move the block verbatim into `control/handlers_<name>.rs`.
   Prepend a module doc + `use super::*;` (a child module sees the parent's private items).
3. Make the moved top-level items `pub(super)`.
   Blanket `pub(super)` on `fn`/`const`/`static`/`struct`/`enum`/`type` is correct here, because the sibling `control/tests.rs` references nearly every helper; a "minimal" set does not help and leaving types private triggers the `private_interfaces` lint (fix by making the types `pub(super)` too).
4. In `control.rs`, replace the moved block with `mod handlers_<name>;` + `use handlers_<name>::*;`.
   The glob re-export means both the dispatch arms and the sibling test module resolve the names unchanged - no call-site edits.
5. Verify: `cargo check --tests` must be warning-free (watch the `private_interfaces` lint), then `cargo test --lib control::tests` (baseline 426 pass / 0 fail, ~6 min), then `cargo clippy --lib`.
6. Commit path-scoped (`control.rs` + the new submodule + version-bump files) after running `apps/desktop/scripts/bump-version.sh`.

### WS3 - consolidate `wsl_bash` helper (BLOCKED / deferred)

Promote `files::wsl_bash` to public and route the ~8 direct `wsl.exe ... -e bash -lc` callers through it.
BLOCKED here: `wsl_bash` and its callers are all `#[cfg(windows)]`, so the Linux `cargo check` never compiles them, and the Windows cross-check fails in this sandbox (`ring`'s mingw C build hits Permission denied).
Needs a Windows build to verify.
Also low value (cosmetic DRY) and the callers are heterogeneous (the `wsl_home` probes have no cwd) - do not force them all through one signature.

### WS4 - Powder Phase B (deferred, careful)

Remove the persisted Powder structs + fields still in `control.rs` (`PowderProjectBinding`, `PowderWorkBinding`, `PendingDispatchClaim/Release`, etc.) once a schema migration guarantees old on-disk registries upgrade cleanly.
Blocker: `PendingDispatchRelease` has `deny_unknown_fields`, so removing fields without a pre-deserialization migration breaks old registries.
Steps: bump `CAPTAINS_SCHEMA_VERSION` 31 -> 32; add a pre-deserialization JSON migration that strips `powder`/`powderWork`/`pendingDispatch*` from `< 32` docs; drop `deny_unknown_fields`; delete the structs + residual reconciliation; update `tests/fixtures/captains-schema-*.json` + add a migration test.
Not urgent - the residue is inert backward-compat.

### WS5 - other frontend god-files (backlog, opportunistic)

Same decomposition as ThemeEditor: `FilePanel.tsx` (1,720), `Tile.tsx` (1,631), `Terminal.tsx` (1,531), `Canvas.tsx` (1,274), `Sidebar.tsx` (941).
Each is small/low-risk and independent.

## Verification reference

- Backend: `cd apps/desktop/src-tauri && cargo check --tests` then `cargo test --lib control::tests` (baseline 426/0) then `cargo clippy --lib`.
- Frontend: `cd apps/desktop && pnpm typecheck` (exit 0) + `pnpm vitest run` (593 tests baseline).
- Every code commit: run `apps/desktop/scripts/bump-version.sh` (docs-only commits are exempt).

## Coordination

Another session commits to `main` concurrently (it authored the Powder removal and collided with a delegated agent mid-task).
Before any `control.rs` work, check `git log` / `git status` on `control.rs` for concurrent edits, and keep commits path-scoped.
