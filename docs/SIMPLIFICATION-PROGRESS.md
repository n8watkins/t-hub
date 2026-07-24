# Maintainability Simplification - Plan & Progress

Tracking doc for the ongoing effort to simplify the largest maintenance liabilities in this repo.
Started 2026-07-23; this file is the single reference for what is done and what remains.

## Goal

Reduce the two headline liabilities - the `control.rs` monolith and the `workspace.ts` god-store - plus retire dead code, without changing behavior.
Every change is behavior-preserving, verified (tests / typecheck), committed separately, and path-scoped so it does not sweep concurrent-session edits.

## Landing / merge process

This effort commits directly to local `main` (the same convention the prior sessions used; the coordination note below is about concurrent `main` commits). There is no feature branch or PR for it.

- **State**: local `main` is a clean fast-forward ahead of `origin/main` (verify: `git merge-base --is-ancestor origin/main HEAD`). A `git push origin main` is a fast-forward - no rebase/merge needed.
- **Scope caveat**: pushing `main` publishes ALL local commits ahead of origin, not just this effort's. That is expected (shared branch), but if only this work should land in isolation, cherry-pick this effort's commits onto a branch first: `git switch -c refactor/control-split <base>` then `git cherry-pick <range>`, and open a PR.
- **Pre-push gates** (all currently green): `cd apps/desktop/src-tauri && cargo test --lib control::tests` (426/0), `cargo clippy --lib` (clean), and a full `cargo test --lib` (1181/1182 - the single failure is one of two KNOWN pre-existing parallel-load flakes: `harness::tests::launch_resolution_handles_ci_bash_and_available_login_shells` or `control::tests::delayed_node_wrapper_waits_for_exact_trusted_native_child`; both pass in isolation and are unrelated to this refactor - re-run in isolation to confirm before blaming an edit).
- **Frontend**: untouched by the `control.rs` split (WS2's `workspace.ts` work landed earlier). A full merge gate would still run `cd apps/desktop && pnpm typecheck && pnpm vitest run`, but this effort's Rust-only commits do not change frontend behavior.
- **Push happens only on the General's say-so** (per the repo commit policy); this doc does not authorize an automatic push.

## Progress (done, on `main`)

| Commit | Change | Result |
| --- | --- | --- |
| `236459a` | `control.rs` inline test module -> `control/tests.rs` | 73,435 -> 33,914 |
| `4722060` | `ThemeEditor.tsx` -> `components/theme-editor/` (4 files) | 2,053 -> 1,281 |
| `2924c69` | `control.rs` file/git read handlers -> `control/handlers_files.rs` | -> 33,823 |
| `2e82bc0` | **WS2**: `workspace.ts` god-store -> 9 zustand slices + `internal.ts` under `store/workspace/` | 2,642 -> 667 |
| `30d9102` | **WS1**: `control.rs` history handlers -> `control/handlers_history.rs` | -> 26,717 |
| `3cbe2aa` | **WS1**: `control.rs` fleet-watch handlers -> `control/handlers_fleet.rs` | -> 26,507 |
| `df37fd6` | **WS1**: `control.rs` worktree handlers -> `control/handlers_worktrees.rs` | -> 25,840 |
| `969f822` | **WS1**: `control.rs` inherent `impl CaptainsRegistry` (99 methods) -> `control/captains_registry.rs` | -> 20,338 |
| `fd737dd` | **WS1**: `control.rs` agent handlers -> `handlers_agents.rs` (15 fns) + status/monitoring handlers -> `handlers_status.rs` (13 fns) | -> 18,979 |
| `bb65c0f` | **WS1**: `control.rs` comms-plane handlers -> `handlers_comms.rs` (8 fns) | -> 18,690 |
| `1e5a88e` | **WS1**: `control.rs` delegated-admin handlers -> `handlers_admin.rs` (28 fns + 3 types) | -> 17,466 |
| `0cdd74a` | **WS1**: `control.rs` spawn machinery -> `handlers_spawn.rs` (27 fns) | -> 15,978 |
| `17c4833` | **WS1**: `control.rs` captain handlers (claim lifecycle + provisioning) -> `handlers_captains.rs` (16 fns + 1 const) | -> 14,792 |
| `0de9005` | **WS1**: `control.rs` terminal I/O + tab handlers -> `handlers_terminal.rs` (14 fns + 3 types) + `handlers_tabs.rs` (11 fns) | -> 13,804 |
| `d7a3d48` | **WS1**: `control.rs` read_terminal -> `handlers_terminal.rs`, open_file -> `handlers_files.rs` | -> 13,682 |
| `0083456` | **WS1**: fold `#[cfg(test)]` + `Default` `CaptainsRegistry` impls into `captains_registry.rs` | -> 13,554 |
| `fab795a` | **WS1**: `RequestCache` + captain-control leases -> `control/idempotency.rs` | -> 13,002 |
| `0f267a4` | **WS1**: captains data model pt.1 (`FleetRole`/`ClaimState`/`CrewState`/`CrewRef`) -> `captains_registry.rs` | -> 12,849 |
| `10e78c8` | **WS1**: captains data model pt.2 (`CaptainsRegistry` struct + authority machinery + `CaptainsInner`) -> `captains_registry.rs` | -> 12,424 |
| `2a6f2c5` | **WS1**: captains data model pt.3 (workspace/pending-op/snapshot records) -> `captains_registry.rs` | -> 12,260 |
| `ceb79b0` | **WS1**: captains data model pt.4 (`FleetIdentity`/disposition/`ProjectRecord`) -> `captains_registry.rs` | -> 12,069 |

Also verified (authored by a concurrent session): the retired **Powder runtime** was fully removed (`powder.rs` deleted, handlers + tests gone); `main` compiles and the lib test suite is green.
`control.rs` has gone from 73,435 to ~12,069 lines total across this effort.
**The `captains_registry.rs` module - the whole planned scope (impl + data model) - is now DONE.**

Type-extraction gotcha (idempotency): moving a struct whose fields the sibling `control/tests.rs` inspects needs those FIELDS made `pub(super)`, not just the struct - once the struct leaves `control` for `control::idempotency`, `tests.rs` (a sibling, not a descendant) can no longer see ancestor-private fields. The compiler surfaces these one struct at a time, so re-check until clean.

Two more gotchas from these later moves (both fixed in-commit):
- Splitting a cluster mid-way through a fn's doc comment strands the doc (`error: expected item after doc comment`) in the source module and leaves the fn undoc'd in the target - cascading into "undeclared" errors because the stranded-doc module fails to compile and its glob re-export vanishes. Cut on blank lines between items, never inside a doc block.
- `handlers_files` is declared WITHOUT a `use handlers_files::*;` glob (its dispatch arms call `handlers_files::fn` qualified), so a fn moved into it must be dispatched with the qualified path too - a bare call won't resolve.

Two portability gotchas surfaced during these moves (both fixed in-commit):
- `handlers_spawn.rs`: an `include_str!("../provider-capacity.json")` is resolved relative to the source file's dir, so moving `src/control.rs` -> `src/control/handlers_spawn.rs` required `../../provider-capacity.json`.
- `handlers_captains.rs`: a moved *public* free fn (`recover_pending_fleet_operations`, called from `lib.rs` as `control::...`) needs an explicit `pub use handlers_captains::recover_pending_fleet_operations;` in `control.rs`, because the private `use handlers_captains::*;` glob does not re-export it on the `control::` path. (Inherent `pub` methods are unaffected - they resolve by type, not module path, which is why `captains_registry.rs`'s 33 `pub` methods needed no re-export.)

## Remaining workstreams

### WS1 - split `control.rs` production half into submodules (all IN-PLAN work DONE)

Original goal: take `control.rs` from ~26,717 toward ~5,000-6,000 lines. Reassessed: ~12k is the realistic floor, since the plan's "Stays in `control.rs`" list keeps the bulk of the core types + dispatch + serve loop. All in-plan moves are now complete; `control.rs` sits at ~12,069.
Submodules: `handlers_files.rs`, `handlers_history.rs`, `handlers_fleet.rs`, `handlers_worktrees.rs`, `handlers_agents.rs`, `handlers_status.rs`, `handlers_comms.rs`, `handlers_admin.rs`, `handlers_spawn.rs`, `handlers_captains.rs`, `handlers_terminal.rs`, `handlers_tabs.rs`, `idempotency.rs`, and `captains_registry.rs` (impl + full data model).

**Both the free-function handler extraction AND the planned `captains_registry.rs` data-model move are COMPLETE.** What remains in `control.rs` (~12.1k) is the core the plan says STAYS:
1. Type definitions the plan keeps: the wire types (`ControlRequest`/`Response`/`Handshake`), `EventFanout`, `TabRegistry`, `ControlContext`, `RequestCache`/`CaptainControlLeases` moved to `idempotency.rs`, the PTY-attach machinery (`RebindController`/`AttachForwarderGuard`/`ConnGuard`/`SharedPtyWriter`), `CommandTier`, `Capability`; plus the deferred Powder Phase-B types (`PowderWorkBinding`/`PendingDispatch*`/`PowderMutation*`/`PowderProjectBinding`/`PowderWorkState` - WS4) and the captains business-logic free fns (`validate_*`/`reconcile_*`/`apply_workspace_report`/`migrate_project_identities`/`deserialize_crew`).
2. The dispatch match + serve/listen loop + handshake/identity/token resolution (core - STAYS).
3. Read handlers still adjacent to dispatch (`list_terminals`, `list_captains`, `list_projects`, `cortana_bootstrap`, `claude_usage`/`codex_usage`, `host_metrics`, `archive_recent_project`).

There is NO remaining in-plan WS1 work. Any further shrink would move items on the plan's "Stays" list (below) and needs the plan re-agreed first.

Note (sibling-to-sibling resolution PROVEN): a helper moved into submodule A is reachable from sibling submodule B through `control`'s `use A::*;` re-export + B's `use super::*;`.
This is the same mechanism `control/tests.rs` already uses to reach the moved handlers, and it now also holds for production siblings (`handlers_fleet` reaches `target_statuses` in `handlers_status`).

**`captains_registry.rs` data-model move - DONE** (pt.1 `0f267a4`, pt.2 `10e78c8`, pt.3 `2a6f2c5`, pt.4 `ceb79b0`). It was a NON-CONTIGUOUS, four-sub-cluster move (the model types interleave with the wire types that stay, the deferred Powder types, and business-logic free fns). Lessons captured for future type moves:
- The struct/model types are `pub` and referenced cross-module. Public ones re-exported via `pub use captains_registry::{CaptainsRegistry, ClaimState, CrewRef, CrewState, FleetRole, ProjectRecord};` in `control.rs`; the private internal types via a plain `use captains_registry::*;`. `ProjectRecord` needed the `pub use` for the `tests/mcp_e2e.rs` INTEGRATION test (`tests/` is a scan blind spot the `src/`-only grep missed - `cargo check --tests` is the real gate).
- Mixed TRAIT impls (`From`/`Default`/`Drop`/`Display` - methods take NO visibility modifier) with INHERENT impls (private methods need `pub(super)`): a blanket `^    fn ` -> `pub(super)` is WRONG. Move the block verbatim with only column-0 private types made `pub(super)`, then let the compiler pinpoint the inherent methods (`between`/`scoped`/`advance`) and the struct FIELDS (`CaptainsInner`/`CaptainsRegistry`/`DispatchBarrier`/`Close*Result`) the parent + tests inspect - `pub(super)` those.
- Left in place (as planned): the Powder Phase-B types (WS4) and shared helpers (`now_ms`, schema-version consts).

**Explicitly NOT in scope** (the plan's "Stays in `control.rs`" list names these as staying, so do NOT move them without re-agreeing the plan): the wire types (`ControlRequest`/`Response`/`Handshake`), `EventFanout`, `TabRegistry`, `ControlContext`, the serve/listen loop **including `serve_pty_attach`** (and its `RebindController`/`AttachForwarderGuard`/`ConnGuard`/`SharedPtyWriter` internals), discovery/handshake, identity/token resolution **including `required_tier`/`CommandTier`/`Capability`** (`Capability` is also referenced cross-module from `delegated_admin.rs`), the dispatch entry points, and shared helpers (`arg_str`/`deny`/`now_ms`/`tmux_target`/`captains_path`). A `pty_attach.rs` or `authz_tier.rs` split would contradict this list; the read handlers by dispatch (`list_terminals`/`list_captains`/`list_projects`/`cortana_bootstrap`/usage/`host_metrics`) sit inside the dispatch core and are best left there. These are possible FUTURE scope, but only if the plan is revisited - they are not agreed work.

Reality check on the 5-6k target: with only the in-plan `captains_registry.rs` data-model move left, `control.rs` lands around ~11k, not 5-6k. The original 5-6k figure assumed moving core types the "Stays" list keeps, so ~11k is the realistic floor without re-scoping.

Done (type clusters): folded `#[cfg(test)]` + `Default` `CaptainsRegistry` impls into `captains_registry.rs`; `idempotency.rs` (`RequestCache` + `CaptainControlLeases`); the full captains data model (pt.1-4: roles/state/`CrewRef`; the `CaptainsRegistry` struct + authority machinery + `CaptainsInner`; workspace/pending-op/snapshot records; `FleetIdentity`/disposition/`ProjectRecord`) into `captains_registry.rs`.

Done (handlers): `handlers_fleet.rs`, `handlers_worktrees.rs`, `captains_registry.rs` (inherent `impl CaptainsRegistry` - 99 methods - plus the whole data model + test/Default impls; the parent keeps only a `mod` + a targeted `pub use`/`use *` for the public/internal types), `handlers_agents.rs` (agent lifecycle), `handlers_status.rs` (status/supervision/host-monitoring), `handlers_comms.rs` (inbox/plane-send/authorization), `handlers_admin.rs` (delegated-admin lifecycle + execution), `handlers_spawn.rs` (spawn-capacity eval + spawn/tmux handlers), `handlers_captains.rs` (claim lifecycle + captain provisioning + crew launch), `handlers_terminal.rs` (break-glass writers + close_terminal lifecycle + read_terminal), `handlers_tabs.rs` (org-apply + tab mutations + list_tabs).

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

**Impl-block variant** (used for `captains_registry.rs`): to extract a single inherent `impl Type { ... }` block, move the whole `impl` verbatim, keep the struct + other impls (test/`Default`) in the parent, and prepend only `use super::*;`.
Prefix each private method (`^    fn `) with `pub(super)`; leave `pub fn` methods as-is.
In the parent, replace the block with a bare `mod <name>;` and NO glob import - inherent methods resolve crate-wide by type, so nothing needs re-exporting, and a `use <name>::*;` on an impl-only module would just be an unused import.
This compiled warning-free first try (no `private_interfaces` fallout: a `pub(super)` method exposing a parent-module-private type is the same visibility level, so no leak).

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
