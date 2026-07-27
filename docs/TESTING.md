# Testing T-Hub

T-Hub has layered local test profiles so normal iteration does not pay the cost of every real process, browser, host CLI, and installer boundary.

## Profile summary

| Command | Coverage | Warm baseline |
|---|---|---:|
| `pnpm test` or `pnpm test:fast` | Rust libraries, CLI, typecheck, and Vitest | 25 seconds |
| `pnpm test:standard` | Every Rust target except the slow process modules, plus the fast frontend and CLI lanes | About 1 minute |
| `pnpm test:backend` | Standard Rust and CLI lanes | About 1 minute |
| `pnpm test:frontend` | Typecheck, Vitest, and the production bundle | Under 1 minute |
| `pnpm test:browser` | Production bundle and Playwright | Under 1 minute |
| `pnpm test:contracts` | Version, voice-gate, performance, test-profile, workflow-pinning, and skill contracts | About 30 seconds |
| `pnpm test:host-contracts` | Real Codex and Claude provisioning and installation contracts | About 7 minutes |
| `pnpm test:process` | Real control and tmux process lifecycle tests | About 4 to 5 minutes |
| `pnpm test:full` | Complete Rust, CLI, frontend, browser, bundle, and portable contracts | About 4 to 6 minutes |

Run a focused test first when changing one behavior.
Then choose the narrowest profile that crosses the boundaries affected by the change.

## Fast profile

The fast profile runs these independent lanes concurrently:

- Deterministic Rust library tests across the desktop workspace.
- The standalone `th` CLI contract tests.
- Frontend TypeScript checking.
- All frontend Vitest tests.

The Rust lane excludes `control::tests` and `tmux::tests`.
It also selects library targets, so binary and integration targets belong to the standard profile.

Use the fast profile during implementation and after focused tests for the code being changed.

## Standard and targeted profiles

The standard profile adds every Rust binary and integration target while still excluding the two slow process modules.
It builds the real `t-hub-mcp` binary before running the workspace so MCP integration tests remain deterministic from a clean target directory.

Use the backend profile for Rust or CLI-only changes.
Use the frontend profile for TypeScript, state, and component changes.
Use the browser profile when layout, interaction, rendering, or the production bundle can change.
Use the contracts profile for repository scripts, voice gating, performance evidence, workflow pinning, and skill packaging.
Use the host-contracts profile when Codex or Claude provisioning and installation can change.

The host-contracts profile fails when either real CLI is missing.
It never silently treats a skipped compatibility test as coverage.
The contract scripts use isolated temporary homes and do not mutate the operator's real Codex or Claude configuration.

## Process and full profiles

The process profile runs only `control::tests` and `tmux::tests`.
Those modules exercise real process, socket, shell, systemd, and tmux lifecycle behavior.
They share isolated infrastructure and serialize ownership transitions, which is why they dominate runtime.
The canonical Rust gate runs the standard targets before the process modules so unrelated tests do not contend for that shared infrastructure.

Run the process profile after changes to control dispatch, terminal ownership, tmux, history resume, supervision, or process evidence.

The full profile runs these independent lanes concurrently:

- The canonical Rust workspace gate, including real MCP and process integration tests.
- The standalone CLI contract tests.
- Frontend TypeScript checking.
- All frontend Vitest tests.
- The production Vite bundle followed by the Playwright browser suite in one local lane.
- Portable version, voice-gate, performance, test-profile, workflow-pinning, and handoff-skill contracts.

The full profile is the local pre-push gate for changes that cross several areas.
The real Windows installer build and installation remain a separate host-level release validation because they intentionally modify installed Windows state.

## CI layout

CI keeps the complete portable coverage and runs the real Codex and Claude compatibility contracts in a dedicated pinned-CLI job.
Frontend unit and build checks run separately from Playwright so browser installation and execution no longer block the faster frontend lane.
The local full profile keeps bundle and browser execution sequential because concurrent Vite build and development-server processes share local Vite state.
TypeScript is checked once, and the production bundle uses `build:bundle` instead of invoking TypeScript a second time.

CI also runs Rust formatting, Clippy, the minimum supported Rust version, and Windows installer contracts.
The native Windows installer build, installation, launch, hook, voice, and state-preservation smoke test remains a release workflow boundary.

## Measured baseline

On the WSL development host on July 27, 2026, the normal profile completed in 25.38 seconds.
The standard Rust lane completed in 49.74 seconds, so the parallel standard profile is expected to remain close to one minute.
The process profile completed in 4 minutes 42 seconds.
The earlier monolithic Rust workspace invocation took about 8 minutes because unrelated targets contended with the process harness.
The partitioned complete Rust gate completed in 4 minutes 8 seconds.
The exact complete local profile completed in 6 minutes 2 seconds in a later run.
The complete local profile is therefore expected to finish in about 4 to 6 minutes because its independent product lanes run concurrently and its Rust classes run sequentially.

Cold builds can take longer because Cargo must compile and link every native Tauri test target.
The slow Rust cost comes from real process and tmux lifecycle coverage rather than obsolete unit-test volume.
Do not delete those tests solely to improve local iteration time.
