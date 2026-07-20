# Two-Axis Agent Status Model

## Purpose

T-Hub must represent what an agent is doing separately from whether its runtime is healthy and reachable.
A single dot must not collapse completed work, a waiting agent, a disconnected terminal, and an unknown provider state into one ambiguous status.
This model is provider-agnostic and applies to Cortana, Captains, Crew, and ordinary agent sessions.

## Axis One: Work State

Work state describes the most recent authoritative turn or attention state.

- `idle`: The session is ready and no turn is active.
- `working`: A turn is actively progressing.
- `waiting-on-subagents`: The parent turn remains active while delegated work is outstanding.
- `needs-answer`: The agent requires user input.
- `needs-permission`: The agent requires an approval decision.
- `rate-limited`: The exact session cannot continue until provider capacity returns or policy changes.
- `completed`: The most recent turn completed successfully.
- `failed`: The most recent turn ended unsuccessfully.
- `unknown`: T-Hub cannot currently establish work state.

`completed` and `failed` describe a turn result and must carry the relevant turn identity and observation time.
They do not mean that the durable Captain or Crew identity has retired.
Silence, lack of terminal output, or a quiet prompt must never prove completion.

## Axis Two: Runtime Health

Runtime health describes whether the bound terminal and Harness can continue work.

- `starting`: The terminal or Harness is being created.
- `ready`: The runtime is live and responsive.
- `restoring`: T-Hub is reattaching or resuming the runtime.
- `degraded`: The runtime remains usable but one or more required signals or dependencies are impaired.
- `disconnected`: The durable identity exists but no usable live attachment is available.
- `stopped`: The runtime ended cleanly and requires an explicit resume or replacement.
- `failed`: The runtime failed and recovery is required.
- `unknown`: T-Hub cannot currently establish runtime health.

Hot, warm, and cold terminal presentation states remain owned-resource lifecycle metadata.
They must not be presented as agent work states or runtime failures when tmux and the Harness remain healthy.

## Delivery and Release Provenance

Work state and runtime health do not prove that a source change was reviewed, accepted, integrated, packaged, installed, or verified in the live application.
T-Hub records those delivery states separately against exact commits and artifacts.

- `implemented`: Code exists at the recorded exact commit.
- `reviewed`: An independent reviewer approved that exact commit for the stated scope.
- `tested`: The required acceptance checks passed on that exact commit.
- `complete`: The stated scope is both independently reviewed and acceptance-tested on the same exact result commit.
- `integrated`: The complete result commit is present in the named canonical baseline.
- `packaged`: An artifact was built from that canonical baseline.
- `installed`: That artifact replaced the intended installation.
- `live-verified`: The required flow passed against the installed application, whether verified by a human or an AI agent.

No surface may collapse `complete`, `integrated`, `packaged`, `installed`, or `live-verified` into one label.
A completed Harness turn remains only a work-state observation and cannot establish any delivery state.
Visible product bugs require packaged graphical end-to-end evidence before `tested` may satisfy `complete`.

## Authority and Freshness

Every status observation must identify its source, observation time, and quality.
Quality is one of `authoritative`, `derived`, `stale`, or `unknown`.
Provider or Harness lifecycle events are authoritative for the state they explicitly report.
T-Hub process and terminal evidence is authoritative for runtime existence and attachment state.
Output activity is only a derived working hint for a plain shell or a session without structured integration.
Transcript parsing, terminal pixels, and folder-name inference must not be treated as authoritative semantic state.

When sources disagree, T-Hub must retain both axes and surface the degraded source rather than silently selecting a reassuring state.
For example, an agent may remain `needs-answer` while its runtime health becomes `disconnected`.

## Normalized Inputs

Harness adapters should map structured provider signals into the normalized events defined in [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).
Codex should use lifecycle hooks for thread, permission, compaction, and subagent boundaries, plus structured app-server events for turns, items, approvals, completion, failure, and cancellation where supported.
Claude should map its hooks and status-line bridge into the same work and runtime transitions.
Unsupported provider signals must remain unavailable or derived rather than fabricated.

## Rendering Rules

The primary indicator represents work state.
Runtime health appears as a secondary badge or warning only when it is not `ready`.
Role, Harness, Provider, model, context, and authority remain separate labels rather than status colors.
Dense surfaces may collapse several attention states to one shape, but accessible text and tooltips must retain the exact state.
Every status must use shape or text in addition to color.
The tile header, sidebar, Captain list, Crew list, History, notifications, voice, and CLI must use the same labels and precedence.

The current generic tooltip pattern must be replaced with exact language.
A `needs-answer` session cannot be labeled only as a live terminal, and a completed turn cannot be labeled only as a terminal state.

## Notification and Voice Rules

Notifications and voice are driven by meaningful work-state and runtime-health transitions, not repeated snapshots.
Needs-answer, needs-permission, failure, recovery, and completion remain separately configurable event classes.
Completion applies to a user-relevant Captain, Crew, or Assignment turn rather than every tool or subagent completion.
Deduplication must survive frontend reconnects and repeated provider events.
Stale or derived status must not trigger destructive automation or confident completion announcements.

## Recovery Rules

A runtime failure triggers recovery, not retirement.
Context reset, terminal replacement, Harness switching, and Provider switching preserve durable identity while creating explicit transition events.
Retirement remains a separately authorized lifecycle operation.
If exact resume identity cannot be proven, T-Hub retains a visible recovery-required state instead of sending input to an uncertain session.

## Tests Required Before Activation

- Test every work state against every relevant runtime-health state.
- Test authoritative, derived, stale, unknown, and conflicting observations.
- Test that output silence cannot produce completion.
- Test runtime replacement without Captain or Crew retirement.
- Test reconnect deduplication for notifications and voice.
- Test equivalent rendering and accessible labels across all product surfaces.
- Test provider capability gaps without inventing unsupported events.
- Test that delivery states remain distinct across backend, CLI, MCP, and graphical surfaces.
- Test that `complete` requires independent review and acceptance tests for the exact result commit.
- Test that visible product bugs cannot become `complete` from source-only tests.
