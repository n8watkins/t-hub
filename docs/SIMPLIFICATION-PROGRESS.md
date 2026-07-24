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
| `3cbe2aa` | **WS1**: `control.rs` fleet-watch handlers -> `control/handlers_fleet.rs` | -> 26,507 |
| `df37fd6` | **WS1**: `control.rs` worktree handlers -> `control/handlers_worktrees.rs` | -> 25,840 |
| `969f822` | **WS1**: `control.rs` inherent `impl CaptainsRegistry` (99 methods) -> `control/captains_registry.rs` | -> 20,338 |
| `fd737dd` | **WS1**: `control.rs` agent handlers -> `handlers_agents.rs` (15 fns) + status/monitoring handlers -> `handlers_status.rs` (13 fns) | -> 18,979 |
| `bb65c0f` | **WS1**: `control.rs` comms-plane handlers -> `handlers_comms.rs` (8 fns) | -> 18,690 |
| `1e5a88e` | **WS1**: `control.rs` delegated-admin handlers -> `handlers_admin.rs` (28 fns + 3 types) | -> 17,466 |

Also verified (authored by a concurrent session): the retired **Powder runtime** was fully removed (`powder.rs` deleted, handlers + tests gone); `main` compiles and the lib test suite is green.
`control.rs` has gone from 73,435 to ~17,466 lines total across this effort.

## Remaining workstreams

### WS1 - split `control.rs` production half into submodules (IN PROGRESS)

Goal: take `control.rs` from ~17,466 to roughly 5,000-6,000 lines (core dispatch + serve loop + shared types/helpers) by moving handler groups into `control/handlers_*.rs`.
Done so far: `handlers_files.rs`, `handlers_history.rs`, `handlers_fleet.rs`, `handlers_worktrees.rs`, `captains_registry.rs`, `handlers_agents.rs`, `handlers_status.rs`, `handlers_comms.rs`, `handlers_admin.rs`.

Note (sibling-to-sibling resolution PROVEN): a helper moved into submodule A is reachable from sibling submodule B through `control`'s `use A::*;` re-export + B's `use super::*;`.
This is the same mechanism `control/tests.rs` already uses to reach the moved handlers, and it now also holds for production siblings (`handlers_fleet` reaches `target_statuses` in `handlers_status`).
So shared helpers can move into whichever submodule owns them; they need not stay in `control.rs`.

**Remaining groups** (do biggest/hardest first, one verified commit each):
- `handlers_spawn.rs` - `spawn_terminal`/`start_agent`/`commission_captain`/`attach_captain` + the spawn-capacity/provider-capacity evaluation cluster + crew-launch helpers.
  These are the largest remaining contiguous blocks: a spawn-capacity eval cluster (`admit_spawn`/`evaluate_spawn_capacity`/`provider_capacity_evidence`/...) sits just above `start_agent`+`spawn_terminal`+`spawn_tmux_*`; `commission_captain`/`attach_captain`+crew-launch helpers are a separate block ~1,700 lines higher. Extract as sub-clusters.
- `handlers_captains.rs` - `captain_checkpoint`/`rename_captain`/`claim_captain`/`claim_captain_locked`/`release_captain`/`report_workspace_tabs`.
- `handlers_tabs.rs` - `new_tab`/`close_tab`/`rename_tab`/`focus_tab`/`move_tile`/`list_tabs`/`open_file` + the org-apply helpers (`broadcast_apply`/`forward_apply`/`organization_apply`/`with_sync`/`organization_sync_apply`).
  Note: these are scattered (interleaved with unrelated fns), not one contiguous block - split by sub-cluster or defer until neighbours are extracted.
- `idempotency.rs` - `RequestCache` + provider-capacity evidence + control leases.
  Reassessed: NOT one contiguous ~5k cluster - the `RequestCache`/`CaptainControlLeases` structs+impls (~600 lines) are interleaved with provider-capacity types, `SpawnPurpose`/`SpawnAdmissionGuard`, and `PreviewRootAuthority` that `ControlContext` and spawn admission depend on. Extract the RequestCache+lease sub-cluster only, or defer.

Also still available in the `captains_registry.rs` neighbourhood: the `#[cfg(test)] impl CaptainsRegistry` helper impl and `impl Default for CaptainsRegistry` could fold into that submodule for cohesion; the `CaptainsRegistry` struct + supporting types (`CaptainsInner`, `ClaimDisposition`, `ShipMembership`, ...) are pervasively referenced by `ControlContext`/handlers, so moving them needs care with `pub(super)` + the `private_interfaces` lint.

Done: `handlers_fleet.rs`, `handlers_worktrees.rs`, `captains_registry.rs` (inherent `impl CaptainsRegistry` - 99 methods; struct + test/Default impls stay in parent, `mod`-only include since inherent methods resolve crate-wide), `handlers_agents.rs` (agent lifecycle), `handlers_status.rs` (status/supervision/host-monitoring), `handlers_comms.rs` (inbox/plane-send/authorization), `handlers_admin.rs` (delegated-admin lifecycle + execution).

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
