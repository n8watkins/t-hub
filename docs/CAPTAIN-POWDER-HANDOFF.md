# Captain, Crew, Powder, and Performance Handoff

## Canonical Planning Note

The runtime evidence in this handoff remains the baseline for the installed `0.3.66` build.
The authoritative forward roadmap is [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).
That plan now includes the settled permanent Cortana identity, multiple Captains per Project, Assignment-based ownership, provider-agnostic Harness integration, CLI-first control, durable messaging, History, voice parity, and parallel implementation lanes.
The CLI-first phase is governed by [cli-contract.md](./cli-contract.md), which preserves the existing Rust client architecture and stable JSON contract while scheduling strict parsing, safety, bounded output, help, and contract tests for later implementation.
Where the narrower ordered list in this handoff differs from the phased plan, follow the phased plan.

**Updated:** 2026-07-14.
**Repository:** `/home/natkins/projects/tools/t-hub/t-hub-app`.
**Branch:** `main`.
**Source head before this handoff update:** `a4fd704`.
**Installed Windows build:** locally built T-Hub `0.3.66` from `a4fd704`.

## Executive Status

The Captain, Crew, cross-harness provisioning, Handoff skill, authority hardening, and initial performance work are implemented and committed on `main`.
The final independent authority review reports no remaining Critical, High, or Medium application-level finding under the documented same-user threat model.
The exact integrated source passed Rust workspace tests, MCP end-to-end tests, frontend tests, TypeScript, the production frontend build, formatting, warning-free Clippy, installer tests, and the PowerShell performance contract test.

The final production artifact is installed and running from `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe`.
The installed executable SHA-256 is `3192f4f18a637dfde6b5e74358b504d05839b19452f94644a08d15c73a3dd141`.
It is running as PID `13092`.

The local Powder authority is running as a WSL user service on `127.0.0.1:4017` and is reachable from Windows through Tailscale Serve at `https://n8desktop-wsl.tailae53f1.ts.net`.
The protected `n8desktop-wsl` profile retrieves an agent-scoped key from WSL, and an authenticated remote write has passed.
The `t-hub` Powder board and `thub-local-acceptance` card exist.
No T-Hub project is currently registered, and the two visible legacy Captain records have no project or Powder binding.
Real commissioned Captain and Crew acceptance therefore remains incomplete rather than externally blocked.

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

The installed application is logging current xterm rendering failures involving `loadCell` and `isWrapped`.
Treat this as a lifecycle race until reproduced and disproved.
It is the highest-priority correctness issue before further lifecycle optimization.

The fresh general performance review ranked the next work as:

1. Add packaged 1, 4, 8, and 16 terminal measurements plus in-app resource counters.
2. Implement a hot, warm, and cold terminal lifecycle so parked terminals eventually dispose xterm, CanvasAddon, RemotePty, sockets, readers, and attach processes while tmux stays authoritative.
3. Preserve the stable pool wrapper and rehydrate by subscribing before attach and replaying authoritative capture, avoiding the known canvas DOM-move blanking regression.
4. Skip Powder event polling when no active Captain can receive events and cache profile clients, credentials, and HTTP connection pools with explicit refresh behavior.
5. Enable the existing binary PTY protocol and remove the live JSON/base64 encode and decode chain with a tested V1 fallback.
6. Coalesce focus-driven terminal and Git scans, pause low-priority hidden polling, and reduce permanent watchdog cadence after measurement.
7. Lazy-load and prune the non-Lucide icon resolver stack by selected theme.

The hot, warm, and cold terminal lifecycle is now implemented, but it remains the highest-risk performance change.
Terminal visibility, scrollback recovery, input readiness, and canvas reattachment still require packaged end-to-end testing, and the current xterm errors must be resolved before the lifecycle is accepted for production.

## Remaining Production Work

The canonical gated sequence is [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).

The ordered continuation is:

1. Reproduce and fix the installed xterm lifecycle rendering errors.
2. Replace manual Captain repository entry with saved, existing WSL folder, and new codebase paths, simplify project terminology, and add a commissioning preflight summary.
3. Register the T-Hub codebase, bind it to the `t-hub` Powder board through `n8desktop-wsl`, and commission disposable Codex and Claude Captains.
4. Verify context reset recovery, Crew dispatch into a deliberate shared workspace, claim renewal, terminal close release, rollback retention, and Powder event delivery against real Powder cards.
5. Wire the Board panel to the focused project's Powder profile instead of the global `http://localhost:4000` default.
6. Replace the unclear Dev then Preview sequence with a guided Run and Preview flow.
7. Confirm the Claude terminal-header label interactively in the installed application.
8. Run stable packaged 1, 4, 8, and 16 terminal acceptance measurements, including cold rehydration, input readiness, and canvas rendering.
9. Continue the measured performance tranche with Powder polling, binary PTY transport, focus-scan coalescing, watchdog cadence, and icon loading.

Additional production-readiness gaps remain outside the Captain slice:

- Authenticode signing is absent.
- Tauri CSP hardening is incomplete.
- Security scanning and strict branch protection need completion.
- GitHub Actions dependencies should be pinned to immutable revisions.
- Packaged Windows, WSL, tmux, Codex, and Claude end-to-end CI is incomplete.
- A 24-hour soak and resource acceptance matrix have not been completed.

## Fresh Context Procedure

1. Read `AGENTS.md`, this file, `skills/captain/SKILL.md`, `docs/POWDER-INTEGRATION.md`, `docs/PERFORMANCE-BENCHMARK.md`, and `docs/PRODUCTION-READINESS.md`.
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
The WSL skill migration, authority hardening, immutable CI action migration, provider identity fix, push, CI, production release, Windows installation, and four-terminal packaged measurement are complete through `7f395fb`.
The live smoke reproduced a five-second tmux timeout against a healthy `4.3s` Windows-to-WSL command, widened the bounded default to ten seconds, and verified the final packaged request in `0.673s`.
The source is ahead of `origin/main` by five commits through `a4fd704`, and the locally installed Windows build is `0.3.66`.
The local Powder endpoint and protected agent credential path are operational.
The immediate action is to fix the xterm lifecycle race, then improve Captain creation and run real Powder-backed Captain and Crew acceptance.
The Board endpoint, Preview workflow, Claude header check, packaged performance matrix, and release hardening remain open.
