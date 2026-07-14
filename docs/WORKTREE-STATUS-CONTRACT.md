# Unified Worktree Status Contract

## Purpose

This document defines the authoritative worktree state shared by the T-Hub backend, `th`, MCP, and the graphical interface.
It supplements the interaction and path rules in [WORKTREE-WORKFLOW.md](./WORKTREE-WORKFLOW.md).
No client may independently decide whether a worktree is safe to open, reuse, remove, or clean up.

## Authority

The backend computes worktree status from Git, T-Hub's durable identity and resource registries, live terminal evidence, and Powder bindings.
Git is authoritative for repository, branch, checkout, dirty, merge, and worktree metadata.
T-Hub is authoritative for terminal, Captain, Assignment, Workspace, Crew, and owned-resource bindings.
Powder is authoritative for cards and claims.
Folder-name conventions are display hints only and must not be used as branch or ownership authority.

Every snapshot must include its observation time and whether each unavailable field is unknown, unsupported, or stale.
A client must preserve unknown state rather than converting it into clean, merged, unleased, or safe.

## Canonical Snapshot

A worktree snapshot should expose the following project-specific fields where available.

### Identity

- Project identifier.
- Canonical repository root.
- Worktree path.
- Main or linked worktree kind.
- Current branch or an explicit detached state.
- Current HEAD commit.
- Default branch.
- Git locked or prunable metadata.

### Git State

- Dirty state as `clean`, `dirty`, or `unknown`.
- Changed-entry count when known.
- Merge state as `merged`, `unmerged`, `not-applicable`, or `unknown`.
- Ahead and behind counts when inexpensive and reliable.
- Missing-directory and stale-registration state.

### Ownership and Leases

- Active terminal identifiers rooted in the worktree.
- Owning Captain, Assignment, Workspace, and Crew identifiers when bound.
- Owned-resource lease state and expiry when applicable.
- Powder board, card, claim, and claim-expiry references when bound.
- Last meaningful activity time from T-Hub-owned evidence.

### Safety Decision

- Removal decision as `blocked`, `safe`, `requires-force`, or `unknown`.
- Reuse decision as `blocked`, `safe`, or `unknown`.
- Ordered reason codes and human explanations.
- The exact resources and state that an approved action would affect.

## Safety Policy

The main worktree is never removable through T-Hub.
A dirty, locked, actively leased, or unknown worktree is never automatically removed or reused.
An unmerged clean worktree may be classified as `requires-force`, but force must remain separate from destructive confirmation.
An explicit force option must not override dirty state, active leases, unknown state, or the main-worktree boundary.
Automatic cleanup must require a clean, merged, unleased, known linked worktree with no unresolved Powder claim or owned process.
Missing directories and stale Git metadata require reconciliation rather than silent deletion.

## Display Contract

Dense tile and sidebar surfaces should show the authoritative branch, linked-worktree marker, dirty marker, and a degraded marker when status is stale or unknown.
The Worktrees view should show branch, path, clean state, merge state, active leases, owner, Powder work, last activity, and removal verdict.
Tooltips and detail views should expose the reason behind blocked, forced, stale, or unknown state.
Recent, Captain, and Workspace surfaces must stop deriving branch names from `wt-*` or folder-name conventions when authoritative Git state is available.

## Command Contract

`th worktree ls --json` and the corresponding MCP operation must return the same ordered snapshot records used by the graphical interface.
Human output may select fewer fields, but it must not recompute or weaken the backend safety decision.
Destructive commands must support dry-run and explicit confirmation as defined in [cli-contract.md](./cli-contract.md).
Batch cleanup must report each worktree outcome and must preserve blocked or unknown worktrees.

## Freshness and Reconciliation

Each snapshot must carry an `observedAt` timestamp and source-specific freshness where state can age independently.
Window focus may request refresh, but clients should consume backend caching and in-flight deduplication rather than spawning Git once per tile.
T-Hub must reconcile worktrees at startup, after WSL restart, after terminal replacement, and before any destructive or reuse decision.
An event-driven refresh should follow T-Hub-owned create, remove, commit, branch, lease, claim, and cleanup operations.

## Tests Required Before Activation

- Test main, linked, detached, locked, stale, missing, dirty, clean, merged, unmerged, leased, and unknown states.
- Test nested terminal paths and multiple terminals sharing one worktree.
- Test Captain, Workspace, Crew, Powder card, and claim bindings.
- Test stale cache refresh and WSL restart reconciliation.
- Test that CLI, MCP, and graphical views receive equivalent safety decisions.
- Test that dirty, leased, main, and unknown worktrees cannot be automatically removed or reused.
- Test that force and confirmation remain independent gates.

## Migration

The existing `GitInfo`, `WorktreeInfo`, and CLI worktree scan structures should converge behind one backend status service.
Clients may migrate incrementally, but no new worktree feature should add another path-derived status model.
Existing folder-name heuristics may remain only as a clearly labeled fallback for historical records that cannot be resolved through Git.
