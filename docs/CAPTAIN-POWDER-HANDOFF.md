# Captain, Crew, and Powder Integration Handoff

**Updated:** 2026-07-13.
**Repository:** `/home/natkins/projects/tools/t-hub/t-hub-app`.
**Branch:** `main`.
**Implementation head before the final deployment update:** `154e1a1`.
**Installed Windows build:** `154e1a1`, T-Hub `0.3.64`.

## Executive Status

The reviewed Captain, Crew, Powder, CI, and installer changes are committed through `c8f00cc`.
The Windows production build from `154e1a1` is installed and running.
T-Hub MCP control has been proven against the installed application.
A generic Captain create, claim, inventory, release, process close, and tab cleanup smoke passed against the installed application.
The T-Hub repository is registered as a durable project, but it is not Powder-bound.
The authoritative Powder deployment is unreachable from the current tailnet and no agent-scoped credential source is configured.
Canonical project commissioning, reset recovery, Powder claims, and Crew dispatch therefore remain blocked and correctly fail closed.
The right-click Codex header defect from `c8f00cc` is released and installed.
Three independent skill and handoff reviews identified additional Captain parity and production gaps that are recorded below.

## Current Runtime

- The installed application is `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe`.
- The application was installed from the successful production release run for commit `154e1a1`.
- The running application reported PID `18228`, control protocol version `2`, and address `127.0.0.1:64871` after installation.
- The Windows control handshake is `C:\Users\natha\.t-hub\control.json`.
- The WSL handshake path resolves to the same Windows control state.
- The installed MCP binary is `~/.t-hub/bin/t-hub-mcp`.
- The installed and release MCP binaries both had SHA-256 `4e800dc1c1c05d51bbaea2602858dc75eabf5f775dd820dc753d241a0327fe0d`.
- The current Codex tmux terminal is `th_43c9d74c`.
- Right-click pinning registered that terminal as active Captain `ship-43c9d74c`.
- Its MCP token still reports `read`, not `control`.
- Its Captain record has no project, assignment, harness, conversation checkpoint, or Powder binding.
- `captain_bootstrap` therefore returns `Captain is not bound to a registered project`.
- `captain_checkpoint` is refused because the terminal token is read-only.
- The earlier `t-hub-app` Captain record is now orphaned after the conversation and terminal replacement.

## What Was Implemented

### Captain and Shipmate Skills

- `skills/captain/SKILL.md` is the canonical Captain protocol.
- `skills/shipmate/SKILL.md` is a compatibility alias.
- Managed Captain and Shipmate copies are installed for both Codex and Claude.
- All four installed copies currently hash-match the repository sources.
- All four installed directories contain `.t-hub-managed` ownership markers.
- A new Codex or Claude session is required after installation for its cached skill catalog to reload.

### Durable Project and Captain State

- The versioned Captain registry stores registered projects, Powder mappings, Captain assignments, harness identity, conversation checkpoints, Crew records, Powder claims, event cursors, and cleanup state.
- Project registration canonicalizes the Git main worktree.
- `captain_bootstrap` reconstructs a commissioned Captain from durable state after context replacement or application restart.
- `captain_checkpoint` persists the harness conversation identifier and a concise reset-safe resume point.

### Project-Aware Commissioning

- `register_project` records an existing Git repository.
- `bind_project_powder` binds that project to one canonical Powder repository and one protected connection profile.
- `commission_captain` checks Powder authorization before spawning Codex or Claude with control capability.
- Commissioning stores the project, assignment, harness, ship, and workspace ownership.
- Duplicate active project Captains are rejected or reused instead of being silently duplicated.

### Crew and Powder Lifecycle

- `dispatch_crew` validates the project checkout and Powder repository before claiming work.
- Crew terminals receive read capability by default.
- Powder remains authoritative for cards, runs, claims, work logs, input requests, and completion evidence.
- T-Hub remains authoritative for projects, ships, terminal identity, harness selection, checkout paths, Crew liveness, and card-to-terminal bindings.
- Claim rollback retains a durable `cleanupPending` record until terminal stop and Powder release are independently confirmed.
- The lease reconciler renews only when terminal and harness liveness are proven.
- Ambiguous liveness causes no Powder mutation.
- No Powder source files were changed.

### Powder Event Synchronization

- T-Hub consumes Powder's durable event tail.
- Relevant events are persisted to the Captain inbox before the cursor advances.
- Events for other repositories advance the global cursor without waking the Captain.
- Undelivered relevant events prevent cursor advancement.
- Event IDs are treated as idempotency keys for crash recovery.

### CI and Release

- Pull requests and pushes to `main` run Rust formatting, workspace tests, warning-free Clippy, TypeScript, Vitest, and the production frontend build.
- CI validates Rust `1.89.0` as the declared MSRV.
- The release workflow depends on the reusable quality workflow.
- CI run `29296074677` passed for commit `154e1a1`.
- Production release run `29296200008` passed for commit `154e1a1`.
- The release produced `T-Hub_0.3.64_x64-setup.exe` and its Tauri updater signature.
- The installer SHA-256 was `3953339031442323beb318012b53b41bf0cf458701397f291fb047ee263c3e04`.
- The installer is not Authenticode signed.

## Runtime Acceptance Completed

The following smoke was executed through the installed production MCP server:

1. Created a control-capability terminal in a background test tab.
2. Claimed the terminal as `captain-control-smoke`.
3. Verified the Captain in the authoritative registry.
4. Released the Captain claim.
5. Closed the terminal and its process tree.
6. Closed the empty test tab.
7. Verified that no test Captain, terminal, or tab remained live.

This proves generic process and registry control.
It does not prove Powder-backed project commissioning or Crew dispatch.

## Codex Header Defect

The right-click header menu displayed `Claude Session ID` in a Codex terminal when the Claude supervision index retained an older binding for the same tile.
A component-level end-user reproduction failed before the fix.
Commit `c8f00cc` now displays the Claude-only identifier only when the tile's detected foreground client is Claude.
The regression test covers a Codex tile with a stale Claude UUID and proves that neither the label nor stale UUID is rendered.
A real Claude tile still displays and copies its Claude session ID.

Verification for `c8f00cc`:

- Focused `Tile.test.tsx`: 11 passed.
- Desktop TypeScript check: passed.
- Desktop Vitest suite: 401 passed across 40 files.
- Production frontend build: passed.
- Vite still reports existing mixed import and large chunk warnings.

The deeper durable identity issue is not fixed by `c8f00cc`.
`claim_captain` can still copy a stale Claude StatusBridge UUID into a Codex Captain record.
The frontend client detector is best-effort and can be ambiguous for runtime-wrapped clients.
Provider-aware terminal identity must replace the generic `claudeUuid` fast path for Codex records.

## Powder External Blocker

The Powder remote doctor documents `https://sanctum.tail5f5eb4.ts.net:10001` as the expected endpoint.
The current WSL tailnet suffix is `tailae53f1.ts.net`.
Windows is also connected to the current `tailae53f1` tailnet account.
No `sanctum`, `powder`, or `bastion` peer is visible from either Tailscale client.
The documented hostname does not resolve.
The same `sanctum` hostname under the current tailnet suffix also does not resolve.
There is no matching SSH alias or alternate local endpoint.
`~/.t-hub/powder-profiles.json` is absent.
No Powder environment variable, password-manager CLI, or sanctioned key command is available.

These external items are genuinely required:

1. Make the authoritative Powder deployment reachable from this machine, or confirm its replacement endpoint.
2. Provide a command that returns an existing agent-scoped Powder key.
3. Provide the Powder agent name matching that key.

Do not put the raw key in this file, the repository, or chat.
Do not create a second local Powder authority as a production substitute.

The protected profile should use mode `0600` and this shape:

```json
{
  "schemaVersion": 1,
  "profiles": {
    "production": {
      "baseUrl": "https://confirmed-powder-endpoint",
      "agentName": "matching-agent-name",
      "apiKeyCommand": "command that prints the existing agent-scoped key"
    }
  }
}
```

## Independent Skill Review

Three read-only agents reviewed the Captain skill, Shipmate compatibility path, installer, runtime state, CI, and handoff.

### High Findings

1. Right-click pinning and `claim_captain` do not create a fully commissioned Captain.
Only `commission_captain` stores the project, assignment, harness, control capability, and Powder relationship required by bootstrap and Crew dispatch.
The pin UI and Captain skill must distinguish overlay pinning from project commissioning, or an atomic project-attachment operation must be added.

2. Claude commissioning receives Codex-specific `$captain` invocation text.
The harness adapter must emit `$captain` for Codex and `/captain` for Claude.

3. The repository installer copies skills for Claude but provisions MCP only for Codex.
Claude needs a tested user-scoped MCP installation path and harness-aware environment validation.

4. Codex MCP convergence checks only the command path.
Stale arguments or other registration fields can survive and break a newly started Codex session.

5. Generic Captain identity can attach a Claude UUID to a Codex terminal.
Captain continuity and rebind logic must be provider-aware.

### Medium Findings

1. Codex conversation continuity is documented but is not checkpointed automatically from `CODEX_THREAD_ID`.

2. Commissioning can default `workspaceTabIds` to an empty list and suppress normal tab inference.

3. MCP registration replacement is not transactional and can leave T-Hub unregistered when the add step fails.

4. The top-level install is not transactional across binary replacement, MCP registration, and skill installation.

5. Managed skill copies contain no source revision or hash, so installed copies can silently become stale.

### Low Findings

1. The Shipmate alias uses Codex-specific `$captain` language even in its Claude installation.

2. Crew reaping instructions can imply removing the canonical checkout instead of only a linked Crew worktree.

## Production Stability Gaps

- Right-click pinning is not full Captain commissioning.
- Canonical Powder commissioning and Crew dispatch have not been exercised against the authoritative server.
- Claude MCP provisioning and harness-specific skill invocation are incomplete.
- Provider-aware Codex and Claude conversation identity is incomplete.
- Authenticode signing is absent.
- Branch protection does not yet require every available quality job and lacks strict review, conversation, and administrator enforcement.
- Security scanning remains disabled.
- CI has no packaged Windows and WSL end-to-end test.
- GitHub Actions dependencies use movable tags.
- The production Tauri CSP remains unset.
- The frontend bundles remain large and need intentional splitting.

## Ordered Next Work

1. Visually confirm that the installed Codex header no longer displays a Claude session ID.
2. Redesign right-click pinning so the UI clearly distinguishes overlay pinning from full project commissioning.
3. Add an atomic project-attachment or commissioning path for an existing terminal when that workflow is desired.
4. Make Captain identity, bootstrap prompts, checkpointing, and MCP installation harness-aware.
5. Make MCP registration and top-level installation transactional and convergence-complete.
6. Resolve Powder network reachability and configure the agent-scoped protected profile.
7. Bind the registered T-Hub project and require `powder_status` to pass.
8. Commission disposable Codex and Claude project Captains.
9. Verify reset recovery by terminal ID and ship slug.
10. Dispatch disposable Codex and Claude Crew against real ready Powder cards.
11. Verify claim renewal, terminal close release, rollback retention, and event delivery.
12. Complete Windows signing, security scanning, CSP hardening, strict branch protection, and packaged end-to-end CI.

## Fresh Context Procedure

1. Read `AGENTS.md`, this handoff, `skills/captain/SKILL.md`, `docs/POWDER-INTEGRATION.md`, and `docs/PRODUCTION-READINESS.md`.
2. Run `git status --short` and preserve the user's `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` artifacts.
3. Run `git log --oneline -8` and inspect any commits after `c8f00cc`.
4. Confirm that the installed build is still `154e1a1` or later.
5. Do not treat a right-click pin as proof of control capability or project commissioning.
6. Use `my_capability`, `list_captains`, and `captain_bootstrap` to verify runtime truth.
7. Do not dispatch canonical Crew until the project has a verified Powder binding.
8. Keep Powder authoritative and do not modify Powder to accommodate T-Hub.
9. Re-run the complete relevant gates after every change and commit each verified logical change.

## Resume Point

The immediate continuation is to address the reviewed Captain skill and pinning gaps before calling the Captain and Crew relationship stable.
Powder-backed acceptance remains externally blocked until the authoritative endpoint and agent-scoped credential command are available.
