# T-Hub CLI Contract

## Status and Scope

This document defines the public behavior expected from the `th` command-line interface.
It is inspired by AXI's agent-ergonomic principles, but T-Hub does not depend on AXI and does not claim AXI compliance.
T-Hub uses stable JSON rather than requiring TOON.
The current Rust CLI in `apps/cli` remains the implementation foundation and should be normalized rather than replaced.

This contract is the target for later CLI implementation and contract testing.
Documenting the target does not imply that every current command already conforms.

## Architecture

`th` is a thin Rust client of T-Hub's authenticated loopback control protocol.
The control server owns authorization and business operations.
The CLI owns argument validation, transport calls, human rendering, JSON rendering, and process exit status.
The MCP server should remain a thin optional adapter over the same command catalog and backend operations.
CLI and MCP must not implement competing business rules.

## Command Design

- Commands must be non-interactive and must never wait for terminal input.
- Every subcommand must provide concise `--help` with its arguments, flags, defaults, and a small set of examples.
- Unknown commands, positional arguments, and flags must fail before any side effect.
- Command groups should use consistent noun-and-verb organization.
- Existing aliases may remain for compatibility, but new aliases require a compatibility reason.
- Running `th` without arguments should continue to show a useful live fleet view rather than only global help.
- Command parsing, output rendering, transport, and reusable business logic must remain separate.

## Human-Readable Output

- Default output must be concise, readable, deterministic, and useful for the next decision.
- List views should expose only the fields normally needed to choose the next action.
- Empty results must state explicitly that the command succeeded with zero results.
- Long lists and content must be bounded by default.
- Truncated output must report the displayed count, total count, or original size as appropriate.
- Commands should provide `--all` for bounded collections and `--full` for truncated long-form content when practical.
- Cheap derived state that prevents a likely follow-up call should be included when it is relevant.
- Human output must not expose raw internal objects, full dependency responses, stack traces, colors in pipes, spinners, or cursor-control sequences.
- Contextual next-command suggestions are useful for lists, empty states, truncation, and recoverable errors, but should be omitted when the result is self-contained.

## JSON Output

- Commands that return useful data must support `--json`.
- In JSON mode, stdout must contain exactly one valid JSON value on success or failure.
- Progress, warnings, update notices, and diagnostics belong on stderr.
- T-Hub retains its established envelope: `{ "ok", "command", "data", "error" }`.
- A successful response must set `ok` to `true`, provide `data`, and set `error` to `null`.
- A failed response must set `ok` to `false`, set `data` to `null`, and provide a structured `error`.
- Collection data must include an explicit array and a count, including an empty array and a count of zero.
- Collection ordering must be deterministic and documented where it is not self-evident.
- Field names must be descriptive and stable.
- Dates and times must use ISO 8601, with UTC preferred unless local time is part of the operation.
- Machine-readable output must be bounded and must report truncation, total count, pagination, or continuation state explicitly.
- `--full` or another documented bounded retrieval mechanism must be available when a command truncates useful content.

## JSON Compatibility

JSON output is a public API.

- Adding an optional field is normally non-breaking.
- Removing or renaming a field is breaking.
- Changing a field's type or meaning is breaking.
- Changing deterministic collection ordering can be breaking.
- Existing envelope fields and compatibility aliases must not be removed without an explicit migration and versioning decision.
- Important JSON contracts require structural tests rather than broad formatting snapshots.

## Errors

Errors must be actionable, structured, and expressed in T-Hub terminology.
Dependency failures must be translated instead of leaking raw backend responses or stack traces.
Normal output must never contain a stack trace, while an explicit future debug mode may send diagnostic detail to stderr.

T-Hub currently exposes the numeric process exit status as `error.code` and a stable symbolic category as `error.kind`.
That established shape should be extended compatibly with an optional `suggestion` and optional bounded `details` object rather than rewritten in place.
Retired Powder mutation failures use `powder_mutation_<state>` as a historical
compatibility shape, where state is `pending`, `rejected`, `stale`, `conflict`,
`expired`, `unsupported`, `malformed`, or `timeout`.
New agent-session operations do not expose Powder mutation state.

```json
{
  "ok": false,
  "command": "projects show",
  "data": null,
  "error": {
    "code": 4,
    "kind": "project_not_found",
    "message": "Project 'example' was not found.",
    "suggestion": "Run `th projects list` to view saved projects."
  }
}
```

Missing arguments, extra positional arguments, and unknown flags are usage errors.
An error suggestion should identify the specific corrective command when one is known.

## Exit Codes

T-Hub retains its established exit taxonomy because scripts and agents may already branch on it.

```text
0 = success, including an idempotent no-op
2 = invalid command usage, argument, or flag
3 = application unavailable or endpoint discovery/connect failure
4 = operational failure from T-Hub or a local operation
5 = authorization, policy, or confirmation gate
6 = control protocol mismatch or malformed protocol response
```

No unavailable, denied, gated, or failed operation may exit with status zero.
An idempotent mutation that has already reached the requested state must exit zero and report that no change was needed.

## Safety

- Destructive commands must require an explicit non-interactive `--confirm` flag before any side effect.
- Existing `--yes` behavior may remain temporarily as a documented compatibility alias while `--confirm` becomes canonical.
- Destructive or wide-reaching commands should support `--dry-run` when practical.
- Confirmation must be validated before endpoint discovery, dependency calls, or local mutation.
- Retired `th powder` commands must return the structured `powder_retired` error
  before endpoint discovery or mutation.
- `--force` may alter a safety policy only where documented and must not substitute for confirmation.
- Unknown or misspelled flags must never be ignored.
- Dry runs must report the proposed effects using the same stable vocabulary as execution results.

## Configuration

Configuration precedence must be documented by each command when it introduces command-specific configuration.
The target precedence is:

```text
CLI flags
environment variables
user configuration or handshake files
defaults
```

T-Hub does not currently define a project-level CLI configuration layer, so one must not be implied.
Endpoint discovery must remain compatible with the documented control environment variables and user handshake file while later reliability work adds stale-endpoint rediscovery.

## Active Supervisory Workflow Commands

The supervisory workflow is active through the shared control operation catalog.

- `th agents preflight` evaluates an exact source commit, lane identity, dependencies, mutable files, schemas, interfaces, integration contracts, and reserved runtime capacity without launching an agent.
- Capacity output preserves `providerSessionLimit`, `providerLiveSessions`, `providerHeadroom`, and `providerCapacityStatus` with its source, degraded state, and optional detail.
- `th agents start` requires the same exact dispatch baseline and concurrency contract and rejects a dirty checkout, abbreviated commit, stale commit, resource collision, missing ordering contract, or exhausted reserved capacity.
- `th agents delivery` records evidence for implementation, independent review, acceptance testing, integration, packaging, installation, and live verification without collapsing those states.
- `th admin list`, `appoint`, `revoke`, `approve-session`, `approve-worktree`, `cleanup-session`, `maintain-session`, `recover-resource`, `prepare-retirement`, and `maintain-fleet-resource` expose durable delegated administration through the same authorization service used by MCP and control clients.
- `th admin approve-session` sends only the exact session ID, and the backend derives the target kind, ship, and ownership from the authoritative fleet registry.
- `th admin cleanup-session` requires both an exact unconsumed approval ID and `--confirm` before endpoint discovery or mutation.
- `th admin maintain-session` performs bounded non-destructive maintenance on one exact live T-Hub session after revalidating the current grant, actor, supervisor, and target ownership.
- `th admin recover-resource` accepts a session, ship, or worktree target and either performs bounded maintenance or records an authoritative recovery plan when direct mutation is unsafe.
- `th admin prepare-retirement` accepts a session, ship, or worktree target and records a deterministic readiness plan without performing a destructive action.
- `th admin maintain-fleet-resource` accepts a fleet, ship, or Captain-session target and remains restricted to Fleet Admin grants without exposing Crew implementation direction.
- `th worktree prune` is reporting-only even when the compatibility `--yes` flag is supplied.

Worktree removal and reuse remain unavailable until one authoritative worktree safety service can prove ownership, integration, retention, and removability for the exact target.

The CLI and MCP adapters serialize the same request and response schemas and do not make independent authorization decisions.

Delivery output must preserve `implemented`, `reviewed`, `tested`, `complete`, `integrated`, `packaged`, `installed`, and `live-verified` as separate facts.
Every new `integrated` evidence object must include the authenticated integration owner and the ordered, bounded, unique lane inputs whose exact baseline and resulting commits produced the canonical integration.
Legacy integration evidence without a manifest remains readable but must not report the `integrated` state.
Every new `packaged` evidence object must include the branch, exact source baseline and source commit, exact Git tree, version, installer SHA-256, build time, signature status, artifact ID, and reference.
The artifact `recordedAt` field is the server audit time, while `manifest.builtAt` is the asserted build completion time in Unix epoch milliseconds and must fall between the integration and artifact record times, inclusive.
Legacy artifact evidence without a complete manifest remains readable but must not report the `packaged` state.

## Testing Requirements

The implementation uses the existing Rust test approach and process-level CLI contract coverage.
The suite must prove parseable and isolated JSON stdout, strict usage failures, explicit empty collections, stable exit categories, idempotent no-ops, destructive confirmation before mutation, deterministic ordering, bounded output, and complete `--full` retrieval where supported.
It must also test endpoint restart, stale discovery state, timeout, malformed response, protocol mismatch, retry, and ambiguous mutation recovery.
CLI and MCP parity tests should be generated from the shared operation catalog after that catalog exists.

## Explicitly Deferred

This contract does not authorize adding TOON, an AXI dependency, an AXI fork, a standalone CLI framework, automatic context injection, session-start hooks, a generated skill, a new MCP server, OpenAPI, a universal schema-version field, or a multi-model benchmark harness.
A future `th capabilities --json` command should be considered only after the expanded shared command catalog makes discovery materially useful.
