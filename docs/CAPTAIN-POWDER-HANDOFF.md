# Captain, Crew, Powder, and Performance Handoff

## Canonical Planning Note

The runtime evidence in this handoff is current through the installed `0.3.100` build.
The authoritative forward roadmap is [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).
The document-status authority is [REVIEW-INDEX.md](./REVIEW-INDEX.md).
That plan now includes the settled permanent Cortana identity, multiple Captains per Project, Assignment-based ownership, provider-agnostic Harness integration, CLI-first control, durable messaging, History, voice parity, and parallel implementation lanes.
The CLI-first phase is governed by [cli-contract.md](./cli-contract.md), which preserves the existing Rust client architecture and stable JSON contract while scheduling strict parsing, safety, bounded output, help, and contract tests for later implementation.
Agent status is governed by [STATUS-MODEL.md](./STATUS-MODEL.md), and shared worktree state is governed by [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md).
Where the narrower ordered list in this handoff differs from the phased plan, follow the phased plan.

**Updated:** 2026-07-15.
**Repository:** `/home/natkins/projects/tools/t-hub/t-hub-app`.
**Branch:** `main`.
**Source head before this handoff update:** `8635374`.
**Installed Windows build:** locally built T-Hub `0.3.100` from exact detached source `8635374`.

## Executive Status

The Captain, Crew, cross-harness provisioning, Handoff skill, authority hardening, and initial performance work are implemented and committed on `main`.
The final independent authority review reports no remaining Critical, High, or Medium application-level finding under the documented same-user threat model.
Source `1484750` passed 55 frontend files and 470 tests, TypeScript, the Rust workspace and MCP end-to-end suites, formatting, warning-free Clippy, and the production frontend build.
The earlier exact `0.3.86` source then passed 621 Linux Rust library tests with one ignored, warning-free Clippy, the production frontend build, and 23 focused native Windows Preview tests without warnings.

The current production artifact is installed and running from `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe`.
The installed executable SHA-256 is `AC7B6169A638F57FF7E6CA699E7016C3C9715C7E968753D654A3C9095CD944F0`.
It is running on the canonical profile as PID `14868`.
The exact NSIS installer SHA-256 is `85AA44A2A30EB4EF45AC5554E35050A08FD84DBFFDACB1CECB753AA657DCDE53`.

Source `0.3.95` at commit `8231b5e` preserves partial Codex usage snapshots, normalizes 5-hour and weekly windows by duration, and migrates the last-known cache.
Source `0.3.96` at commit `c40d52a` adds a Windows Explorer folder chooser to the shared WSL picker used by existing-folder and new-codebase Captain flows.
Source `0.3.97` at commit `4759df0` closes the independent review finding in the Codex usage merge by preserving an unknown-duration primary beside a recognized weekly window and advancing expired windows before later partial polls merge.
Source `0.3.98` at commit `4e264f0` establishes the unexposed provider-neutral History identity and Claude/Codex parser foundation without changing the legacy Recent UI or exposing incomplete actions.
Source `0.3.99` at commit `3afb521` repairs Codex Captain commissioning by removing the exec-only `--skip-git-repo-check` flag from interactive Codex and giving only Captain and Crew orchestration a bounded 120-second response window.
Source `0.3.100` at commit `f8ef9aa` reuses Powder event clients for five minutes, refreshes after a Powder request failure, and suppresses the Windows credential-command console.
Exact detached source `8635374` packages those source changes as installed `0.3.100`.

The local Powder authority is running as a WSL user service on `127.0.0.1:4017` and is reachable from Windows through Tailscale Serve at `https://n8desktop-wsl.tailae53f1.ts.net`.
The local `http://127.0.0.1:4017/healthz` endpoint returned HTTP 200 with the Powder health payload during the 2026-07-15 review.
The protected `n8desktop-wsl` profile retrieves an agent-scoped key from WSL, and an authenticated remote write has passed.
That profile currently has no repository-admin credential.
The `t-hub` Powder board and `thub-local-acceptance` card exist.
Project `project-e28c0579-4e78-4de1-b225-d69aab93c143` now durably registers this repository and binds it to the `t-hub` board through `n8desktop-wsl`.
The earlier installed `0.3.94` commissioning attempt spawned one control-capability terminal, but interactive Codex `0.144.4` rejected the exec-only `--skip-git-repo-check` flag and exited.
The backend rolled that terminal back and left the Project intact without a commissioned Captain.
The installed control client timed out first and surfaced Windows error 10060 instead of the authoritative rollback error.
The same durable Project also activated the 15-second Powder event reconciler, which rebuilt its client and visibly launched the profile's PowerShell credential command about every 16.5 seconds.
Installed `0.3.100` contains both repairs, but real commissioned Captain and Crew acceptance remains incomplete until the preserved Project passes one trusted graphical commissioning retry.

The requested automatic board creation for new codebases is not safe against Powder's current API.
Powder exposes repository upsert, not create-if-absent, so a concurrent creator can appear between T-Hub's read and write and have its settings overwritten.
The reviewed uncommitted Powder prototype was rejected and fully removed because it could not close that race without changing Powder.
T-Hub must not modify Powder to accommodate this flow and must remain fail-closed until Powder independently provides a non-overwriting create precondition.
The rejected prototype also exposed two recovery requirements that remain part of the Phase 7 gate: reuse one reviewed request identity across ambiguous retries, and make a preserved unbound Project directly resumable after Powder or binding failure.

Provider-neutral History implementation is now governed by [HISTORY-CONTRACT.md](./HISTORY-CONTRACT.md).
The contract requires deterministic Harness-plus-conversation identity, distinct same-cwd conversations, backend-owned resume and focus actions, non-destructive per-conversation archive overlays, legacy Claude archive discovery, explicit cache disposal, and dual-Harness packaged acceptance.
The current Claude-only Recent implementation remains unchanged and must not be extended with Codex rows before those identity and action boundaries exist.
The `0.3.98` foundation now locks exact length-prefixed Harness identity, lowercase Codex rollout identity, filename-matching child metadata, real `model_provider`, UTC timestamp normalization, malformed-record degradation, per-conversation Claude archive identity, bounded Unicode text, and explicit unavailable actions.
It is not connected to `history_list`, control, MCP, CLI, frontend IPC, or History UI yet.
Its 16 focused tests, 659 passed desktop Rust tests with one ignored, Rust workspace and MCP end-to-end suites, strict all-feature Clippy, all 480 frontend tests, TypeScript, the production frontend build, version consistency, diff checks, and independent review passed.

The runtime was reverified before this handoff update.
Installed T-Hub is PID `14868`, version `0.3.100`, at `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe`, with SHA-256 `AC7B6169A638F57FF7E6CA699E7016C3C9715C7E968753D654A3C9095CD944F0`.
The installed WSL agent reports `0.5.2` and SHA-256 `813DB68E3DA42A790532258CC89FBBAFC5ABFECFCDD9810FD4D912EB7F14658A`.
Five tmux sessions are currently visible on the canonical `t-hub` socket.
All five pre-install session names and pane PIDs survived the upgrade unchanged.
The exact detached build produced a standalone executable with SHA-256 `950F9C91124CAFBB817FF1A0B1EF496615E9B6222FBC1793D1CABC0D2EAEE8AC`, an NSIS installer with SHA-256 `85AA44A2A30EB4EF45AC5554E35050A08FD84DBFFDACB1CECB753AA657DCDE53`, and an MSI with SHA-256 `9BBD43D3A951FFB6E845E7D828AB84AB42437C09B877FE6926CB3BEF9D5D9C6E`.
The expected local updater-signing error occurred only after both installers were complete because the private release key is not present on this machine.
A 50-second live process sample spanning more than three Powder event poll intervals observed zero PowerShell or cmd children owned by T-Hub PID `14868`.

## Integrated Commits

The main implementation sequence in this work is:

- `b0de55e feat(skills): add cross-harness handoff`
- `0366653 feat(captain): harden cross-harness provisioning`
- `3b1d626 feat(captain): separate pinning from commissioning`
- `2ebc2d6 fix(captain): harden provisioning ownership`
- `3afd8be test(perf): add packaged runtime benchmark`
- `ec5aa9f test(perf): report p95 benchmark statistics`
- `04bf4a1 perf(terminal): blink only the focused cursor`
- `197e3c5 perf(voice): gate Scribe polling on announcements`
- `0b5453f perf(desktop): key terminal event subscribers`
- `b329516 fix(captain): enforce durable authority boundaries`
- `27f3f7e fix(perf): make runtime benchmark attribution explicit`
- `a06079b fix(voice): close announcement state races`
- `1cde859 fix(control): enforce captain authority boundaries`
- `7fd5e50 fix(control): close remaining authority gaps`
- `29328c9 test(tmux): tolerate transient probe pressure`
- `47a099a fix(captain): validate runtime identity metadata`
- `e59977f fix(identity): make lifecycle persistence transactional`
- `a50e60b docs: refresh captain powder handoff`
- `2f17a71 docs: record production deployment validation`
- `c0881a3 ci: pin actions to immutable Node 24 releases`
- `e2a55c0 fix(identity): prefer durable harness metadata`
- `212774a fix(tmux): allow healthy WSL command latency`
- `7f395fb docs: hand off final runtime validation`
- `21379d2 perf(terminal): expose live resource counters`
- `d827882 perf(terminal): add hot warm cold lifecycle`
- `6cd97f8 test(terminal): cover pool lifecycle rehydration`
- `12c9e0b chore(release): bump desktop to 0.3.65`
- `a4fd704 fix(windows): own supervisor job handle`
- `a6230aa fix: bound retained diagnostic logs`
- `67fd348 fix: close diagnostic backup before replacement`
- `cfa4139 fix: serialize terminal resize behind writes`
- `cbc558b fix: leave xterm parser before buffer mutation`
- `1e005e6 fix: recover terminal IPC detach races`
- `3576957 chore: bump desktop to 0.3.71`
- `409675a fix: launch captains with unrestricted authority`
- `5b4e9d2 chore: bump desktop to 0.3.72`
- `0be8504 feat: select captains from Powder boards`
- `4cddab2 chore: bump desktop to 0.3.73`
- `61f56ba feat: discover typed Run targets`
- `ee5eb66 chore: bump desktop to 0.3.77`
- `776439a fix: prevent duplicate terminal redraws`
- `b4a1c5d chore: bump desktop to 0.3.78`
- `fbacc8f fix: expose Tauri Vite previews to Windows`
- `987606a chore: bump desktop to 0.3.79`
- `16480b7 fix: bind mirrored WSL previews on all interfaces`
- `25585db chore: bump desktop to 0.3.80`
- `19dc3c7 fix: preserve reachable mirrored preview URLs`
- `8f5fffa chore: bump desktop to 0.3.81`
- `9d95fa9 fix: own managed development process trees`
- `ec55526 chore: bump desktop to 0.3.82`
- `5011803 feat: serve typed static Preview targets`
- `2cbdbb8 chore: bump desktop to 0.3.83`
- `3177d81 fix: clear stopped managed Preview URLs`
- `0cd5861 chore: bump desktop to 0.3.84`
- `1484750 fix: harden managed Preview lifecycle`
- `9005117 chore: bump desktop to 0.3.85`
- `d05073d test: silence Windows-only Rust warnings`
- `5ea945c chore: bump desktop to 0.3.86`
- `1daf25f test: satisfy strict History lint`
- `3afb521 fix: commission Codex captains reliably`
- `f8ef9aa fix: bound Powder credential polling churn`

## Captain and Crew Model

`skills/captain/SKILL.md` is the canonical Captain protocol.
`skills/shipmate/SKILL.md` is a compatibility alias and should not be used as the product name going forward.
Codex uses `$captain`, while Claude uses `/captain`.
Both harnesses receive managed Captain, Shipmate compatibility, and Handoff skills.
Both harnesses have tested user-scoped T-Hub MCP provisioning.

Right-click pinning is a visual overlay action only.
Pinning does not grant control capability, bind a project, verify Powder, or create a commissioned Captain.
`commission_captain` creates a new control-capability Captain for a registered and Powder-verified project.
`attach_captain` binds an existing control-capability harness to a registered and Powder-verified project.
The terminal location, tab, or current working directory does not establish Captain authority.

The current Codex session is terminal `43c9d74c` and was pinned as `ship-43c9d74c` in the older runtime.
Its MCP capability is `read`, and `captain_bootstrap` reports that it is not bound to a registered project.
It is not a full Captain and must not bypass that boundary by reusing raw tokens.
After the production restart, this resumed session retained a stale endpoint but successfully rediscovered the new listener while preserving its read-only capability.
The live server rejected `spawn_terminal` from this session because it lacks control capability.

## Durable State and Reset Recovery

T-Hub persists registered projects, canonical roots, Powder bindings, Captain assignments, provider and harness identity, conversation checkpoints, Crew bindings, Powder work, event cursors, and cleanup state.
Captain and Crew runtime identity metadata is restricted to `codex` or `claude` and is validated on load and mutation.
Provider and harness must agree when both are present.
Claude UUID continuity cannot appear on Codex records.
Legacy identity metadata is normalized during load without weakening current-schema validation.

Registry writes are atomic and backed up.
Semantic corruption is validated before use.
A newer schema in either the primary or backup file is preserved byte-for-byte and blocks writes until T-Hub is upgraded.
Identity mint, bind, retire, prune, and revoke operations are transactional and roll memory back when persistence fails.
Spawn, close, kill, reconciliation, and startup callers propagate or explicitly report identity persistence failures.

After a context reset or application restart, a Captain must call `captain_bootstrap`, reconcile live terminals and provider identities, read Powder state when available, and only then accept or dispatch work.
Conversation history is a cache, not the source of truth.

## Authority Boundary

The control socket distinguishes a trusted in-process Tauri caller from a socket caller.
A shared Full capability token does not substitute for a valid per-session identity or trusted host provenance.
Omitting or presenting an invalid session identity cannot bypass cross-ship read, write, inbox, plane, abort, lifecycle, project, or Captain authority checks.
Crew cannot self-assign the reserved Cortana role or slug.
A Captain cannot close, checkpoint, renew, or otherwise mutate a foreign ship's Crew.

This is an application-level same-user boundary, not OS isolation.
Another arbitrary process running as the same WSL user can inspect `/proc`, access the shared tmux socket, inspect tmux session state, or drive panes directly.
Protecting against a malicious same-user agent requires separate OS principals or containers and a broker that keeps tmux control and bearer secrets outside agent-readable state.
That stronger isolation is not implemented and is an explicit production threat-model decision.

## Powder Integration

Powder remains authoritative for cards, claims, runs, work logs, input requests, completion evidence, and event history.
T-Hub remains authoritative for projects, ships, terminals, harness selection, checkout paths, Crew liveness, and card-to-terminal bindings.
No Powder source files were modified.

Project registration and binding validate the Powder repository through Powder's existing authenticated repository endpoint.
Aliases are resolved to Powder's canonical repository name before persistence and dispatch validation.
Commissioning requires a healthy protected Powder profile.
Crew dispatch validates the project checkout and Powder card, claims work, persists the binding, starts the selected harness, verifies liveness, and rolls back failures.
Ambiguous liveness causes no Powder mutation.

The historical endpoint `https://sanctum.tail5f5eb4.ts.net:10001` belongs to the Powder author's environment and is not the authority for this installation.
This installation intentionally uses its own local Powder authority for testing.
Powder runs on WSL at `127.0.0.1:4017` and Tailscale Serve exposes it privately to this tailnet at `https://n8desktop-wsl.tailae53f1.ts.net`.
The protected profile is named `n8desktop-wsl`, not `production`.
Its credential command retrieves the agent key from WSL without storing the raw key in the T-Hub project registry.
The remaining integration task is to register the T-Hub codebase, bind it to the `t-hub` Powder board with this profile, and perform real Captain and Crew acceptance.

The protected profile should use mode `0600` and this shape:

```json
{
  "schemaVersion": 1,
  "profiles": {
    "n8desktop-wsl": {
      "baseUrl": "https://n8desktop-wsl.tailae53f1.ts.net",
      "agentName": "t-hub",
      "apiKeyCommand": "protected command that retrieves the WSL agent key"
    }
  }
}
```

## Handoff Skill and Installation

`skills/handoff/SKILL.md` is the canonical cross-harness Handoff skill.
The installer manages Captain, Shipmate compatibility, and Handoff for Codex and Claude.
Managed copies carry ownership and integrity metadata, and the installer refuses unmanaged conflicts unless explicit repair is requested.
MCP registration convergence preserves custom policy and refuses unsafe replacement of customized stale registrations.
Top-level installation rolls back the binary, helpers, registrations, skills, and command wrapper on failure.

The prior unmanaged `~/.claude/commands/handoff.md` was backed up to `~/.claude/commands/handoff.md.pre-t-hub-20260713-200444.bak` with mode `0600`.
Its preserved SHA-256 is `d717284857d7e55a0cb2154cd54c327da9b2ff18eec98592307f95d1cbb23d07`.
The real installer then installed managed Captain, Shipmate compatibility, and Handoff skills for Codex and Claude.
The installed MCP binary matches the release build at SHA-256 `8aa375dcb9ed6dcdcf64cf5820e40f3d283757a86cd9a1c9b9a53b9808042f26`.
`check_environment.sh` reports both harnesses, tmux, the MCP registration, the control handshake, control environment, and skill integrity ready for a capability check.

## Verification Evidence

The integrated source through `e59977f` has the following evidence:

- Rust library: 572 passed and 1 ignored before the final two commits.
- Full Rust workspace, MCP E2E, agent, protocol, and documentation tests passed after each authority follow-up.
- Final targeted identity lifecycle suite: 21 passed.
- Final authenticated socket retirement failure test: passed.
- Frontend: 44 files and 418 tests passed.
- TypeScript typecheck: passed.
- Production frontend build: passed.
- Rust formatting: passed.
- Clippy workspace and all targets with `-D warnings`: passed.
- Codex provisioning test: passed.
- Claude provisioning test: passed.
- Handoff skill test: passed.
- Top-level transactional installer test: passed.
- PowerShell performance contract test: passed through `powershell.exe`.

The production build still reports the known mixed static/dynamic import warnings.
The current build contains a 1.21 MB main JavaScript chunk and a lazy 3.72 MB icon chunk before gzip.
These warnings are tracked as performance work rather than ignored.

GitHub Test workflow `29302705909` completed successfully for `a50e60b`.
Production Release workflow `29302822551` completed successfully for the same exact source, including its quality gate and Windows build.
The release artifact is `t-hub-prod-installer`, artifact ID `8299295908`.
GitHub Test workflow `29304036452` completed successfully for `e2a55c0`.
Production Release workflow `29304129133` completed successfully for the same exact source, including its quality gate and Windows build.
That release artifact is `t-hub-prod-installer`, artifact ID `8299745666`.
GitHub Test workflow `29304997709` completed successfully for `7f395fb`.
Production Release workflow `29308870690` completed successfully for the same exact source, including its quality gate and Windows build.
The final release artifact is `t-hub-prod-installer`, artifact ID `8301430064`.
All external workflow actions are now pinned to immutable commit SHAs, JavaScript actions use Node 24 releases, and CI enforces the pinning contract.

The installed runtime smoke check discovered all four existing terminals, read tabs and repository state, rejected an invalid token, and denied a control-only spawn from this read-only resumed session.
Automated Windows desktop capture returned a black frame in the non-interactive WSL execution context.
The frontend now hydrates authoritative Captain and Crew provider identity after restart and prefers it over stale terminal heuristics.
A commissioned Codex tile cannot expose a stale Claude Session ID, and a Claude tile can recover its authoritative provider session ID before supervision rehydrates.
The full frontend suite passes 45 files and 426 tests with this behavior.
The final packaged smoke discovered all four existing terminals in `0.673s`, rehydrated both Captain records as Codex, rejected a control-only spawn from the read-only resumed session, and confirmed the live PID and rotated endpoint.

## Performance Baseline and Review

The corrected packaged-runtime benchmark pins an exact Windows root PID and creation time, tracks process births and deaths, excludes incomplete CPU intervals from release statistics, and reports duration-weighted CPU.
The older installed four-terminal sample in `artifacts/perf/baseline-0.3.64-4t-v2.json` is gitignored and diagnostic only.
It measured approximately 845.7 MB mean working set, 799.6 MB mean private bytes, and 0.678 of one CPU core across complete intervals.
The WebView subtree accounted for approximately 0.554 of one CPU core.
The run is release-ineligible because process births and deaths occurred and the declared terminal count was not recorded.

The released `a50e60b` build was measured with four declared terminals in `artifacts/perf/t-hub-4t-20260714T032523Z.json`.
It measured approximately 903.7 MB mean working set, 484.2 MB mean private bytes, and 0.788 of one CPU core across complete intervals.
The application process averaged approximately 50.4 MB working set and 0.063 of one CPU core.
The WebView2 subtree averaged approximately 571.1 MB working set and 0.718 of one CPU core.
This run is diagnostic rather than release-acceptance eligible because WebView2 and host-bridge process births or deaths made seven CPU intervals incomplete.
The lower private-byte result is encouraging, but the runs are not directly comparable because the earlier baseline lacked a declared scenario and both runs contain process churn.

Completed low-risk performance changes are:

- Cursor blinking runs only for the visible, foreground, focused terminal region.
- Disabled voice and attention announcements perform no steady Scribe polling.
- Voice settings hydration races no longer transiently arm polling.
- PTY output, state, and exit events use keyed subscribers instead of per-terminal global fanout.
- The UI exposes live terminal resource counters.
- Parked terminals now move through hot, warm, and cold lifecycle states while tmux remains authoritative.
- Lifecycle tests cover cold disposal and rehydration.

The earlier installed `0.3.66` application logged xterm rendering failures involving `loadCell` and `isWrapped` after terminal parking and restoration.
Source commit `6870444` serializes destructive xterm teardown after accepted writes, discards unreplayed pending output during cold teardown, upgrades xterm, and removes the unmaintained canvas renderer.
The fix is installed in the package built from `35fbae2`.
Three Captain terminals were parked past the 30-second cold threshold and rehydrated with readable live output.
The packaged launch produced zero `loadCell`, `isWrapped`, or window errors.
A repeated application launch initially reproduced competing processes and visibly corrupted canvases, so source commit `35fbae2` added the official Tauri single-instance guard as the first plugin.
The rebuilt package kept one PID, preserved the control endpoint, and rendered cleanly when launched again.
Source commit `3b83b9e` gates frontend resize delivery on confirmed remote PTY attachment.
The rebuilt package restored eight live terminals with zero `no live terminal`, xterm, window, or terminal-list errors in the fresh startup diagnostic slice.

The fresh general performance review ranked the next work as:

1. Add packaged 1, 4, 8, and 16 terminal measurements plus in-app resource counters.
2. Implement a hot, warm, and cold terminal lifecycle so parked terminals eventually dispose xterm, CanvasAddon, RemotePty, sockets, readers, and attach processes while tmux stays authoritative.
3. Preserve the stable pool wrapper and rehydrate by subscribing before attach and replaying authoritative capture, avoiding the known canvas DOM-move blanking regression.
4. Preserve bounded Powder event polling for registered Projects without a live Captain so relevant events remain unread until delivery is possible, and cache profile clients, credentials, and HTTP connection pools with explicit refresh behavior.

Source `0.3.100` completes the event-reconciler client cache and Windows console suppression portion of that work.
5. Enable the existing binary PTY protocol and remove the live JSON/base64 encode and decode chain with a tested V1 fallback.
6. Coalesce focus-driven terminal and Git scans, pause low-priority hidden polling, and reduce permanent watchdog cadence after measurement.
7. Lazy-load and prune the non-Lucide icon resolver stack by selected theme.

The hot, warm, and cold terminal lifecycle is now implemented and has passed packaged cold rehydration and application-restart testing.
The full performance matrix and a control-capable input mutation check remain open.

## Remaining Production Work

The canonical gated sequence is [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).

The ordered continuation is:

1. Keep worktree removal suspended until the Phase 2 unified status service consumes Phase 3 B1 ownership and passes its full activation matrix.
2. In parallel against stable shared contracts, complete template and clone preparation plus the shared registration-and-commission resume or rollback transaction, while keeping automatic Powder board creation fail closed until Powder independently provides a non-overwriting create precondition.
3. In parallel through the approved B3 and B4 lanes, implement the provider-neutral History adapter and catalog foundation without changing the Claude-only Recent UI or introducing a substitute durable identity schema.
4. Register the T-Hub codebase, bind it to the `t-hub` Powder board through `n8desktop-wsl`, and commission disposable Codex and Claude Captains.
5. Verify context reset recovery, Crew dispatch into a deliberate shared Workspace, claim renewal, terminal close release, rollback retention, and Powder event delivery against real Powder cards.
6. Complete the native Board's registered and Powder-bound Project success-state acceptance through the Phase 8 flow.
7. Harden generic non-Tauri Vite launch adapters and stale WSL-address recovery for the unified Run and Preview flow.
8. Confirm the Claude terminal-header label interactively in the installed application.
9. Run stable packaged 1, 4, 8, and 16 terminal acceptance measurements, including cold rehydration, input readiness, and canvas rendering.
10. Continue the measured performance tranche with Powder polling, binary PTY transport, focus-scan coalescing, watchdog cadence, and icon loading.

Additional production-readiness gaps remain outside the Captain slice:

- Authenticode signing is absent.
- Tauri CSP hardening is incomplete.
- Security scanning and strict branch protection need completion.
- Packaged Windows, WSL, tmux, Codex, and Claude end-to-end CI is incomplete.
- A 24-hour soak and resource acceptance matrix have not been completed.

## Fresh Context Procedure

1. Read `AGENTS.md`, this file, and the canonical phased plan, then read only the supporting contracts required by the active phase.
2. Run `git status --short` and preserve the user's `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` artifacts.
3. Run `git log --oneline -12` and inspect any commits after this handoff.
4. Confirm the installed Windows executable and PID rather than assuming source is deployed.
5. Treat a right-click pin as visual state only.
6. Use `my_capability` and `captain_bootstrap` before claiming Captain functionality.
7. Never reuse raw tokens to elevate a read-only session.
8. Do not dispatch canonical Crew until the project has a verified Powder binding.
9. Keep Powder authoritative and do not modify Powder to accommodate T-Hub.
10. Re-run relevant gates and commit every verified logical change.

## Resume Point

The application-level Captain authority review is closed with no Critical, High, or Medium finding.
Before the `0.3.94` tranche, the installed Windows process was reverified at PID `23436` and path `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe` during the `0.3.93` packaged review.
That earlier runtime's file and product version were `0.3.93`.
That earlier runtime's installed executable SHA-256 was `4F82CCB76A60B8481FF69601CDB0E7BDCB459CE00B83B31CADA125E688BC5643`.
That earlier installed build was produced from exact detached source `e95eb56` with final standalone executable SHA-256 `725EF0B74E0E2ABBC32064D68785E17F85978A2BE997E865B18E2DE4C21C635D`, NSIS installer SHA-256 `C4CDC28F278815DC2AE826B139A4F1605250B73E48C87D105DD84C06DF3B7F98`, and MSI SHA-256 `AC23DC2414CBC19E174C7EED063CADA3EDFDA85A05F7609AA2250BB8535C51DF`.
That earlier installed executable differs from the final post-bundle standalone hash even though its version, byte length, and build timing align, so byte-for-byte provenance to that final patched standalone is not proven.
The installed `th` CLI is version `0.2.0` from source `07e74f4`.
Source commit `6870444` fixes the reproduced xterm teardown race.
Source commits `585b867`, `70daa67`, and `d8e891e` add clearer Captain vocabulary and preflight, protected Powder profile discovery, a WSL-native folder picker, and Git metadata detection.
Source commit `a00ce7d` adds explicit Git initialization to the shared Project registration transaction.
It never initializes without `initializeGit: true`, refuses a pre-existing `.git` entry, defaults the new repository to `main`, and rolls back only the `.git` directory it created if a later boundary fails.
Source commit `e5948c8` collapses concurrent frontend terminal enumeration into one in-flight request and retries one bounded tmux timeout without retrying unrelated failures.
The `e5948c8` source gate passed 447 frontend tests and TypeScript typechecking.
The preceding runtime and log-retention tranche passed 585 Rust desktop tests with 1 ignored, the Rust workspace suites, MCP end-to-end tests, formatting, warnings-denied Clippy, the production frontend build, and the performance contract.
The latest T-Hub capability probe for this session remained `read`, so no canonical Project mutation, Powder binding, Captain commissioning, or Crew dispatch was attempted.
Source commit `2b7d864` suspends worktree removal fail closed across graphical preflight and the shared backend mutation path until the unified worktree status service can prove canonical Git state, terminals, durable ownership, leases, and Powder claims.
Installed `0.3.86` reproduced the safety failure before that change: direct Tauri removal deleted a disposable linked worktree while tmux session `th_wtrme2e` was rooted inside it, leaving the live pane at a `(deleted)` cwd.
The disposable worktree and session were cleaned after reproduction, and the six canonical sessions were unchanged.
The suspension preserves UI state and invokes no Git operation, and force cannot override it.
Private rollback is limited to worktrees created by the current in-flight `create_worktree` transaction and never removes a worktree after unconfirmed terminal cleanup.
Commit `cfc72b7` first bumped this source to `0.3.87`.
The exact detached Windows build produced all three unsigned local artifacts, but its native removal suite exposed a path-separator-only test failure after proving that the directory remained intact.
Commit `11ccb19` normalizes that cross-platform assertion, and `b8f7309` bumps that source to `0.3.88` under the every-change version policy.
The exact detached `0.3.88` build from `807c271` produced standalone, NSIS, and MSI SHA-256 values `349F899E1938F4C615FC13822FE1DEAEACE47BFB002E90C0C6A108459F4301FE`, `CD4E30FF6A484EF64FAF2E38748C737522A2FED32413F68B65BD5EA6B8AFE973`, and `B135C2D3C8B52B870426B3EC3CB3C84A1737DC5660D4F05F078F1E0EE1C28003`.
Packaging created both installers and then exited only because the updater public key was present without its private signing key, so the artifacts were eligible for local testing but not publication.
The exact detached `0.3.88` native suite then passed the three public refusal cases but exposed that the transaction-rollback fixture registered its worktree with native Windows Git before exercising the production WSL Git path.
Commit `f62f188` makes the fixture use the same WSL Git and host-path conversion boundary as production, and `2c6a429` bumps the corrected source to `0.3.89`.
The exact detached `0.3.89` build from `ac1e2a6` produced standalone, NSIS, and MSI SHA-256 values `440E36512BD95AF56934FEB4B57BED3DF339E2EA64002E432D5FBBF82644967B`, `8144A5CDA2F8CD1378672AC4BE519DD016401D4B2E902BCBA8DAC6A26338545D`, and `B9AF6D760DC15DC0597D4F3FD889F0FA7B34214CCA9635FC833BFBE05C39A2B0`.
Its native suite proved WSL Git fixture creation and the exact public refusal, then exposed a Windows UNC access denial in the host-side existence assertion for a mounted-drive fixture.
Commit `3841c2e` keeps all product operations on WSL Git while checking the retained native fixture path directly, and `e26fe2e` bumps the corrected source to `0.3.90`.
The exact detached `0.3.90` native suite passed all four focused removal tests.
Installed `0.3.90` verified graphical and direct Tauri preflight, normal, and forced refusal against disposable worktrees with live UI and tmux tiles.
Every installed path returned the exact temporary-unavailable error before UI detachment or Git mutation; the tiles, Git registrations, and live pane paths remained intact.
Control, MCP, and CLI parity remains source-test evidence because the installed session retained its read-only capability and did not reuse raw tokens to invoke mutation channels.
The upgrade first launched under debug acceptance as PID `30136`.
The declared one-terminal performance retry ran as isolated PID `49712`, and Windows Explorer separately invalidated the first attempt by launching a normal application process.
The normal installed application was finally restored as PID `20376` without changing the installed hash or any of the six canonical tmux names and pane PIDs.
During the preceding `0.3.90` retry, its live bridge agent reported `t-hub-agent 0.5.0` and had SHA-256 `4BF61DA4DC7BFDBB9AEF8EF464B3AB6E7035D7EF14F715FC5BFE43A78857A706`.
That older binary returned the stable `unsupported` response for `git_info` in a direct protocol 1 capability probe.
The Windows package did not replace that older WSL agent, so packaging the desktop alone could not close the preceding performance blocker.
The retry artifact `artifacts/perf/t-hub-0.3.90-1t-20260715T0044-r2.json` completed 55 samples over 61.05 seconds but observed eight host-bridge births and eight deaths across four incomplete CPU intervals, so `release_acceptance_eligible` is false.
That artifact is diagnostic only, not an accepted baseline, and the 4, 8, and 16 terminal cells remain blocked until the recurring bridge churn is removed and the one-terminal scenario is eligible.
The 29.94-second recurrence matched the visible tile's then-active 30-second full Git-header poll, whose Windows fallback created a fresh WSL process tree on every cache miss.
Source commits `5ced6c2` and `bd0d8dd` implement bridge-first `GitInfo` collection with typed disconnected, unsupported, and command-failure outcomes, a nine-second agent collector bound below the ten-second desktop request bound, and a real matching-agent stdio acceptance test.
The compatibility fallback runs only when the bridge is disconnected or the installed agent explicitly reports `GitInfo` unsupported; agent command failures do not start competing WSL work.
Commit `f821957` bumps the agent and protocol to `0.5.1`, and `8dd94c9` makes successful agent routing one-shot diagnostic evidence while logging every fallback or agent failure.
The full source gate exposed two existing test-isolation failures under load: a lingering attach-churn firehose that could reset the fresh client, and process-global agent environment that leaked across parallel tests.
Commits `a9b7082` and `42de985` quiesce the churn workload, restore the agent test environment, and disconnect the fixture agent before deleting its home.
Commit `d73f9cb` versions desktop `0.3.93` before exact source `e95eb56` was built and installed.
The `0.3.93` source gate passed 56 frontend files and 471 tests, TypeScript, the production frontend build, 633 passed desktop Rust tests with one ignored, the real built-agent stdio round trip, all Rust workspace and MCP end-to-end suites, formatting, warning-denied Clippy, and the performance harness self-tests.
During that `0.3.93` acceptance, the installed bridge agent was PID `1930912` at `/home/natkins/.local/bin/t-hub-agent`, reported `t-hub-agent 0.5.1`, and had SHA-256 `E96DCA57F831451FDA62BE25346B54FF9B495293F0F55095B7ECAC4A891EF04F`.
Installed `0.3.93` passed the graphical same-cwd Git-header proof with one `git_info source=agent` marker and no fallback or agent-error marker.
The isolated packaged retry at PID `20132` completed 55 samples over 60.96 seconds, but host-bridge triplets produced 12 births, 15 deaths, and seven incomplete intervals even though GitInfo stayed on the persistent agent.
Artifact `artifacts/perf/t-hub-0.3.93-1t-20260715T0204-r2.json` is diagnostic and reports `release_acceptance_eligible: false`.
The normal canonical profile was restored as PID `23436`, the disposable performance socket and profiles were removed, and the same six canonical tmux names and pane PIDs remained intact.
Read-only descendant tracing attributed the residual periodic host-bridge lane to terminal reconciliation, which collected tmux sessions and pane metadata through recurring Windows-to-WSL subprocesses.
Source commit `3816bf4` adds the additive `TerminalSnapshot` protocol operation, routes normal terminal reconciliation through the persistent WSL agent, and versions the uninstalled source as desktop `0.3.94` with agent and protocol `0.5.2`.
The compatibility scan is limited to one bootstrap attempt for a disconnected or explicitly unsupported agent, never runs after agent success, and does not run after timeout or agent command failure.
The agent collector bounds each sequential collection step to four seconds, drains output concurrently, and kills and reaps its process group on timeout.
The `0.3.94` source gate passed 471 frontend tests, TypeScript and the production frontend build, 641 desktop Rust tests with one ignored, all Rust workspace and MCP end-to-end suites, warning-denied Clippy, formatting, diff checks, focused timeout regressions, and the performance harness self-tests.
The exact detached `3816bf4` Windows build produced a standalone `0.3.94` executable with SHA-256 `00AA4B113B19B41B2D476E88D9CD5600D42B76F588C294A5D3E06C3B6D59F922`, an NSIS installer with SHA-256 `D9BFC8A94572D1ADEEA8E4494696176D3A49138BEB850D3F90AEE726A2DBE947`, and an MSI with SHA-256 `FF467ECB84AF41C5893E60DBD54B71BF7848E4D89FEEB7829130893C1BAEF54D`.
The build created both bundles and then exited only because the updater public key was present without the private signing key, so these artifacts are for local acceptance and not publication.
The matching detached Linux agent reports `t-hub-agent 0.5.2`, has SHA-256 `813DB68E3DA42A790532258CC89FBBAFC5ABFECFCDD9810FD4D912EB7F14658A`, and passed a real bridge round trip on a disposable socket with exactly one declared session and one pane.
The disposable socket was removed after the proof.
The exact NSIS installed desktop is `0.3.94` with SHA-256 `021E7CAFF58C9A46720A02DD915D09BAC6BFE08235D7E80A8628C1E550223A7E`, and the matching installed agent reports `0.5.2` with SHA-256 `813DB68E3DA42A790532258CC89FBBAFC5ABFECFCDD9810FD4D912EB7F14658A`.
The normal-profile graphical proof emitted one `terminal_snapshot source=agent` marker and one `git_info source=agent` marker with no fallback, timeout, or agent-error marker.
Five polls spanning more than four terminal reconciliation periods kept all six terminal IDs, states, and current working directories stable.
The first isolated 95.09-second artifact `artifacts/perf/t-hub-0.3.94-1t-20260715T1034.json` recorded zero host-bridge births or deaths and 86 complete host-bridge intervals, but a one-second WebView2 helper lifetime made two total intervals incomplete.
The warm repeat `artifacts/perf/t-hub-0.3.94-1t-20260715T1043-r3.json` sampled 95.35 seconds with one declared idle shell, 86 complete total intervals, zero incomplete intervals, zero births or deaths in every category, a stable 17-process tree, and a stable 10-process host bridge.
Its total CPU release statistic is eligible at `0.10637` logical cores over the run.
The disposable tmux socket, database, control files, WebView profile, and shared-layout backup were removed after the proof.
The normal canonical profile is restored as PID `46860` with agent PID `2118725`, and the six original tmux names and pane PIDs remain unchanged.
The eligible one-terminal gate unblocks the serialized 4, 8, and 16 terminal Phase 11 measurements.
The local Powder endpoint and protected agent credential path are operational.
That earlier packaged xterm lifecycle, detach recovery, duplicate-launch, and diagnostic-retention gate passed with eight live tmux sessions preserved.
The installed `a00ce7d` build reproduced one `listTerminals failed` event from the bounded 10-second WSL command timeout before recovery.
After installing `e5948c8`, three consecutive application starts preserved all eight tmux sessions.
Their combined fresh diagnostic slice contains zero `listTerminals failed`, premature-resize, xterm, or window errors, and `th ls` returns all eight sessions.
Installed `0.3.71` completed one warm stress pass and two cold restart passes with zero `loadCell`, `isWrapped`, `replaceCells`, `no live terminal`, window, or unhandled errors in the targeted fresh slices.
Installed `0.3.72` preserved all eight tmux sessions across upgrade and its packaged Captain review reports `Permissions` as `Unrestricted`.
The Codex and Claude harness tests lock the exact unrestricted launch flags used by `commission_captain`.
Installed `0.3.73` preserved all eight tmux sessions and listed all 25 visible canonical Powder boards through `n8desktop-wsl` without free-text entry.
The packaged dialog selected `t-hub`, reported `t-hub via n8desktop-wsl` in preflight, and performed no Project, Powder, Captain, or filesystem mutation during verification.
Source commit `2cf4a42` adds the reviewed empty-codebase transaction, and `00d9207` packages it as `0.3.74`.
Its full gate passed 452 frontend tests, 593 Rust desktop tests with 1 ignored, all Rust workspace and MCP end-to-end tests, warnings-denied Clippy, TypeScript, the production frontend build, and the transactional installer test.
Installed `0.3.74` preserved all eight tmux sessions, displayed the new empty-codebase choice with all 25 Powder boards, reviewed the exact absent destination and unrestricted authority, and left no directory after Cancel.
Packaged Board reproduction on the T-Hub tile resolved the legacy global `http://localhost:4000` setting to `http://192.168.0.102:4000/` instead of using a Project Powder binding or showing the honest unbound state.
Source commit `6c6e4ee` replaces that global iframe with a native Project-scoped read-only Board, and `15ab30f` packages it as `0.3.75`.
The full source gate passed 453 frontend tests, 599 Rust desktop tests with 1 ignored, all Rust workspace and MCP end-to-end tests, warnings-denied Clippy, TypeScript, formatting, and the production frontend build.
The exact detached Windows build produced an executable SHA-256 of `4F43B4B22982B2E09493A7EF331F938675E43405ABE19450081DD6B4DE1153FA` and the installed upgrade preserved all eight tmux sessions.
Packaged WebView verification on the T-Hub tile showed **No registered Project**, zero iframes, and no Board URL input.
The screenshot evidence is `C:\Users\natha\OneDrive\Pictures\Screenshots\T-Hub-0.3.75-Board.png`.
The native Board's registered and bound Project success state remains gated on real Phase 8 acceptance.
The packaged graphical success matrix for new codebases remains open, so Phase 7 remains active on the current Captain-creation sequence.
Installed `0.3.86` reproduced the new-codebase gap: the dialog exposes only an empty Git repository starting point and explicitly defers templates and clones.
The graphical flow sequences separate `register_project` and `commission_captain` backend transactions, so cross-operation resume or rollback and graphical-to-Cortana transaction parity remain open.
Source commit `96998fc` unifies Dev and Preview into one Run and Preview surface and removes all terminal-output URL detection and automatic navigation.
Commit `7ced938` packages that change as `0.3.76`.
The full source gate passed 459 frontend tests, 599 Rust desktop tests with 1 ignored, all Rust workspace and MCP end-to-end tests, warnings-denied Clippy, TypeScript, formatting, and the production frontend build.
The detached Windows build produced executable SHA-256 `2C6FF7C0003852AE95F8968FCC5EF5819744255C1F3F0401150786506D02AF31`, NSIS installer SHA-256 `3798914846CFAE64F3F7FFC79F9BA964E7AD31E2CD64C587D68310F91E6854E0`, and MSI SHA-256 `EEF628D1140DD7AF87D94E6A79450F5124CE440E41352F2B3BA16519DD3B71ED`.
The upgrade preserved the exact same eight tmux session names and pane PIDs.
Packaged WebView verification showed one **Run + Preview** tab, no separate Dev or Preview tab, one unified panel, an empty preview URL, zero iframes, and no detected-URL chips.
The previously offending `http://127.0.0.1:9223/json/list` text remained in the tile's PTY scrollback during that verification.
The screenshot evidence is `C:\Users\natha\OneDrive\Pictures\Screenshots\T-Hub-0.3.76-Run-Preview.png`.
Source commit `61f56ba` implements typed root-package target discovery and generation-safe backend lifecycle snapshots, and `ee5eb66` first packaged it as `0.3.77`.
Source commit `776439a` fixes the reproduced duplicate Codex frame and header-Refresh transcript loss by making tmux attach the only current-screen renderer and removing resize-time transcript clearing.
Commit `b4a1c5d` packages that repair as installed `0.3.78`.
Its full source gate passed 463 frontend tests, 604 Rust desktop tests with 1 ignored, the Rust workspace and MCP end-to-end suites, warnings-denied Clippy, TypeScript, formatting, and the production frontend build.
The exact detached Windows build produced executable SHA-256 `B5786376AA179B08B973940E9D6002D212C958CD470A0E6B43377C586750ED91`, NSIS installer SHA-256 `C174D09E7CAEA3546A57923B3A7CF109867CFD78095994886FC37CA7E607CC71`, and MSI SHA-256 `BB06C0F74B3A46DA1C9D5FD3E69F8AD408BAA41B4E8BEAB9B1CF19C1E5F270A2`.
The upgrade, header Refresh, and full relaunch preserved the same eight tmux pane PIDs and the same T-Hub Codex process chain `1505037 -> 1505453 -> 1505460`.
The active `Run /review on my current changes` draft appeared once after relaunch, and direct visible-pane capture also counted exactly one occurrence.
Screenshot evidence is `C:\Users\natha\OneDrive\Pictures\Screenshots\T-Hub-0.3.78-thub-after-header-refresh.png` and `C:\Users\natha\OneDrive\Pictures\Screenshots\T-Hub-0.3.78-thub-after-full-relaunch-settled.png`.
Packaged Run and Preview acceptance discovered only the four declared T-Hub scripts, started the real Vite `dev` target, detected `http://localhost:1420/`, and stopped the exact run without leaving its Vite descendant alive.
Windows HTTP probing timed out against that WSL loopback-only listener and the preview stayed in its loading state, so reachable-interface ownership is the next Run and Preview implementation boundary.
Source commits `fbacc8f`, `16480b7`, and `19dc3c7` repair that boundary, and `8f5fffa` packages the result as installed `0.3.81`.
Packaged acceptance started the T-Hub root `dev` target, showed Vite `6.4.3`, kept the Preview iframe at `http://localhost:1420/`, returned Windows HTTP 200, and showed both the application and HMR listeners on `0.0.0.0`.
Stop removed ports `1420` and `1421` and the exact managed Vite PID `547880`.
Source commit `9d95fa9` adds a fixed argv-safe WSL supervisor, per-run process groups, stdin lifelines, retained Windows Job Objects, bounded TERM then KILL cleanup, and bounded output-reader joins.
Commit `ec55526` packages that repair as installed `0.3.82`.
The installed package launched the real root `pnpm run dev` as one process group containing pnpm, Vite, and esbuild, returned Windows HTTP 200, and loaded `http://localhost:1420/` in the Preview iframe.
Normal Stop returned in 161 milliseconds, removed all six observed group processes and the run marker, released ports `1420` and `1421`, and a second run reused port `1420` successfully.
Forcing PID `51332` to exit during the second run removed its complete process group, marker, and listeners without changing any of the seven unrelated tmux session names or pane PIDs.
The application then relaunched as installed `0.3.82` PID `48160`.
Installed `0.3.82` also passed representative Next.js acceptance against the real `apps/site` fixture.
The application discovered npm `dev`, started Next `14.2.35`, returned Windows HTTP 200 with both expected page sentinels, loaded `http://localhost:3000/` in Preview, stopped in 161 milliseconds, and removed the full npm and Next process group.
Source commit `5011803` adds a typed static-site target for a regular root `index.html`, an authoritative snapshot URL, and a Windows `127.0.0.1` server with bounded files and strict traversal, hidden-path, symlink, reparse-point, method, and MIME handling.
Packaged `0.3.83` reproduced and exposed a remaining lifecycle defect after successful security acceptance: Stop closed the listener but retained the dead managed URL and iframe as if they were a user URL.
Source commit `3177d81` separates managed and manual Preview URLs so Stop remounts the honest empty state, and `0cd5861` packages the repair as installed `0.3.84`.
The `0.3.84` source gate passed all 469 frontend tests, TypeScript, 613 Rust desktop tests with 612 passed and one ignored, the Rust workspace and MCP end-to-end suites, formatting, and warning-denied Clippy.
Packaged `0.3.84` discovered exactly one package-less static target, auto-loaded `http://127.0.0.1:63437/`, returned the expected HTML sentinel with correct CSS and JavaScript MIME types, and bound only Windows loopback under the T-Hub PID.
Raw, encoded, and double-encoded traversal, backslash, hidden-file, and outside-symlink requests all returned 404, while POST returned 405 and GET and HEAD returned the required security headers.
Stop returned in 406 milliseconds, removed the iframe and URL input, restored the empty Preview state, and closed the listener.
A second run restored its authoritative URL across a Terminal to Run and Preview tab remount, and forced application exit closed that listener while preserving the disposable fixture session and all seven canonical sessions.
The disposable session and files were then removed, and installed `0.3.84` relaunched as PID `17664` with the same seven canonical tmux session names and pane PIDs.
Source commit `1484750` hardens managed Preview concurrency, rejected-start reconciliation, static request admission and deadlines, exact loopback Host validation, and path-race confinement through capability-relative no-follow handles.
Commit `9005117` packaged that source as `0.3.85` and passed 23 focused native Windows Preview tests, including directory-junction rejection.
Its standalone executable, NSIS, and MSI SHA-256 values are `870E71B240B13675F2F717EB786C97F91FDABBD5BFD89E0504E43B2D9E87624D`, `B227A48FDEEF8589E629CA286898FE42ECFCA336C860AE177AC17A6C02A5E121`, and `81A38DB62EE8A68F43DD3F5C3473BC16044F28BF6B4B225FD85630F7315DA454`.
That artifact was not installed because its native test compilation exposed two platform-specific warnings.
Source commit `d05073d` removes those warnings, and `5ea945c` packages the result as installed `0.3.86`.
The exact `0.3.86` source passed 621 Linux Rust library tests with one ignored, warning-denied Clippy, the production frontend build, and the same 23 focused native Windows Preview tests without warnings.
The detached `0.3.86` build produced standalone executable SHA-256 `EF2ED4C1D610F80555A84255F52CC798C1779F24FA9CA79436318D2A5E07B8E1`, NSIS installer SHA-256 `480AAD85F88C20D8105E776E2E84F811096835219F6D97E16999403A6D56714A`, and MSI SHA-256 `4CC8834E283F1639CDA2FD9DD86C34CD703590D7C4AFDC830AC967B95751AAE6`.
Packaged acceptance discovered exactly one typed static target and passed HTML, CSS, JavaScript, nested-path, security-header, hidden-path, symlink, oversize, encoded-traversal, method, missing-Host, mismatched-Host, and duplicate-Host checks.
A nonreading 16 MiB response stopped in 206 milliseconds while a second terminal independently reached `running` with its own run ID and URL.
Restart produced a distinct run ID, stale-run Stop was refused without changing the active run, final Stop cleared the authoritative URL, and final relaunch left no Preview listener.
The six tmux sessions present immediately before installation retained the same names and pane PIDs through installation, acceptance, and final relaunch.
The previously recorded `th_a486c7fc` session was already absent before installation and is not claimed as preserved by this run.
Installed `0.3.86` initially relaunched normally as PID `17712` and was later reverified as PID `53764`, with executable SHA-256 `6C9938814F956E9D2532D1A3E5A020728CFACE302B822DAA033885BD18108D46` unchanged.
The preceding `0.3.81` source review gate passed 55 frontend files and 466 tests, TypeScript, 604 Rust tests with 603 passed and one ignored, MCP end-to-end, and warning-denied Clippy.
The fleet contained eight canonical tmux sessions when the preceding `0.3.81` packaged acceptance began.
During that `0.3.81` review window, the unrelated Scribe session `th_118218d2` and its prior pane PID `3043188` exited, leaving seven live sessions; the managed Stop action owned only the Vite run and did not issue a tmux lifecycle command.
Representative Vite, Next.js, and package-less static packaged acceptance is complete.
The remaining Run and Preview hardening is generic non-Tauri Vite launch adapters and stale WSL-address recovery.
Real Powder acceptance still requires a control-capable Captain session.
The bound Board success state, remaining Run and Preview hardening, Claude header check, packaged performance matrix, and release hardening remain open.
