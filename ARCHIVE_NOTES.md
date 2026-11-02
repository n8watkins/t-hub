# Archive notes

## Status

T-Hub was archived on 2026-11-01 and is not maintained.
Issues and pull requests will not be reviewed.
The code is kept public as a reference in case parts of it are useful later.

## Last in-progress work

The last active effort was runtime modernization, recorded in `docs/RUNTIME-MODERNIZATION-2026-10-21.md` on the `fix/runtime-modernization` branch.
It followed the project assessment in `docs/PROJECT-ASSESSMENT-2026-10-21.md` on the same branch.
That work removed unsafe automatic quota keystrokes, reconciled agent state with provider lifecycle evidence, added an opt-in structured Codex runtime with durable delivery tracking, persisted provider-reported model provenance, added a desktop CSP, and added provider compatibility CI lanes.
Its remaining acceptance gates were packaged Windows/WSL acceptance, remote CI, and representative delegation trials.
A dependency refresh of the marketing site (`apps/site`, still on Next.js 14) toward Next.js 16 was planned but never started.

## Branches with unfinished work

- `fix/runtime-modernization`: the runtime modernization work above, 28 commits ahead of `main`, never merged or deployed. It also contains 10 docs-only commits that were committed locally on `main` but never pushed.
- `wip/stash-0` through `wip/stash-7`: snapshots of old local stashes from July 2026 (Captain follow-up contract, agent follow-up review, Cortana broker transport, Cortana operation allocation, typed receipts, effect journal, and Captain staged verification). Each is one commit on top of the commit it was stashed from, and the commit message records the original stash name. None of them are reviewed or known to build.
- `native-archive`: an older snapshot from the July 2026 pause (archive README and pivot docs), unrelated to the work above.

## Security note

`apps/site` depends on Next.js 14 (`^14.2.35`), which is out of support and has unpatched security advisories.
Other dependencies were not audited at archive time.
Do not deploy any part of this repository as is.

## How to resume

1. Start from `main`, then review `fix/runtime-modernization` and its `docs/RUNTIME-MODERNIZATION-2026-10-21.md` record before merging it.
2. Upgrade dependencies first, including Next.js in `apps/site`, and run `npm audit` and `cargo audit`.
3. Run the full test profile (Rust `cargo test` plus clippy, and the frontend vitest suite) and get CI green.
4. Work through the "Next acceptance gates" section of the modernization record before shipping a packaged build.
5. Treat the `wip/stash-*` branches as reference material and cherry-pick only what still applies.
