# Testing T-Hub

T-Hub has two local test profiles so normal iteration does not pay the cost of every real process and installer boundary.

## Fast profile

Run `pnpm test` or `pnpm test:fast` from the repository root.

The fast profile runs these independent lanes concurrently:

- Deterministic Rust library tests across the desktop workspace.
- The standalone `th` CLI contract tests.
- Frontend TypeScript checking.
- All frontend Vitest tests.

The Rust lane excludes `control::tests` and `tmux::tests`.
Those modules contain real process, socket, systemd, and tmux lifecycle tests that share an isolated tmux server and must serialize their ownership transitions.
They remain mandatory in the full profile and in CI.

Use the fast profile during implementation and after focused tests for the code being changed.
If a change touches control or tmux process ownership, run its exact focused tests during implementation and run the full profile before handoff.

## Full profile

Run `pnpm test:full` from the repository root.

The full profile runs these independent lanes concurrently:

- The canonical Rust workspace gate, including real MCP and process integration tests.
- The standalone CLI contract tests.
- Frontend TypeScript checking.
- All frontend Vitest tests.
- The production Vite bundle followed by the Playwright browser suite in one local lane.
- Version, packaged performance, test-profile, and GitHub Actions pinning contracts.

The full profile is the local pre-push and release-candidate gate.
The real Windows installer build and installation remain a separate host-level release validation because they intentionally modify installed Windows state.

## CI layout

CI keeps the complete test coverage.
Frontend unit and build checks run separately from Playwright so browser installation and execution no longer block the faster frontend lane.
The local full profile keeps bundle and browser execution sequential because concurrent Vite build and development-server processes share local Vite state.
TypeScript is checked once, and the production bundle uses `build:bundle` instead of invoking TypeScript a second time.

## Measured baseline

On the WSL development host on July 27, 2026, the frontend Vitest suite took about 19 seconds, typecheck took about 16 seconds, and CLI tests took about 13 seconds.
The complete Rust workspace took about 8 minutes.
The complete local profile is therefore expected to finish in about 8 to 9 minutes because its independent lanes run concurrently.
The fast profile took 22 seconds with warm build artifacts and 39 seconds in an earlier compilation-bearing run.

The slow Rust cost comes from real process and tmux lifecycle coverage rather than obsolete unit-test volume.
Do not delete those tests solely to improve local iteration time.
