# T-Hub Review and Planning Index

## Purpose

This index prevents historical audits, shipped execution plans, and abandoned experiments from being mistaken for current work.
When documents disagree, use the precedence below and verify claims against the current source and installed build.
Historical documents should remain intact as design rationale and evidence unless a separate cleanup explicitly archives them.

Canonical precedence is scope-based, while the numbered lists below are the required reading order.
The current working chain governs what to do next, the phased plan governs product decisions and dependencies, the operating model governs organizational lifecycle, and narrower canonical contracts govern their named subsystem behavior.
Historical handoffs do not provide current runtime facts.
No handoff or subsystem contract may override an explicit phased-plan decision or General authorization.
When canonical scopes genuinely conflict, stop the affected action and record the resolved decision in the phased plan.

Every document in the repository is classified below.
A document that is not listed here has not been triaged; treat it as unverified and say so rather than acting on it.

## Start Here: the current working chain

These are the documents that describe what is actually being worked on, in reading order.
They take precedence over the roadmap and package documents for the question "what do I do next".

1. [NEXT-STEPS-2026-07-31.md](./NEXT-STEPS-2026-07-31.md) is the current zero-context handoff: where things stand, the one unverified Cortana claim, the open optimization phases, and the working rules that came out of six wrong diagnoses.
2. [OPTIMIZATION-PLAN-2026-07-29.md](./OPTIMIZATION-PLAN-2026-07-29.md) is the live performance and correctness plan. Phase 0 is done, Phase 3 was superseded and shipped, and Phases 1, 2, and 4 are open. It also carries the 2026-07-30 branch-triage record.
3. [CORTANA-SIMPLIFICATION-PLAN.md](./CORTANA-SIMPLIFICATION-PLAN.md) is delivered and verified in production; it is retained as the design rationale for the reattach-or-create Cortana singleton and for the four corrections the implementation made to the plan.
4. [PROJECT-REVIEW-2026-07-28.md](./PROJECT-REVIEW-2026-07-28.md) is the most recent full-project review. Its headline finding, roughly 41 GB of stale worktrees, has since been resolved; its structural-debt findings and its warning that `git cherry` is unreliable on this repository still stand.

## Canonical Contracts

These define subsystem behavior and change only when the corresponding implementation changes.

1. [POST-POWDER-ROADMAP.md](./POST-POWDER-ROADMAP.md) defines the agent-session acceptance surface and its next gates.
2. [DEPOWDER-MIGRATION-PLAN.md](./DEPOWDER-MIGRATION-PLAN.md) defines the post-Powder agent-session model and compatibility boundary. The runtime removal is delivered; the persisted-schema retirement remains unscheduled work specified there.
3. [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md) is the long-horizon roadmap and dependency record. Powder-specific sections are historical, and its per-phase status lines were last refreshed 2026-07-27, so verify them against source.
4. [ORCHESTRATOR-OPERATING-MODEL.md](./ORCHESTRATOR-OPERATING-MODEL.md) defines the Cortana, Project, Assignment, Captain, Workspace, and agent-session operating model.
5. [AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md](./AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md) defines authority, supervision, agent-session evidence, durable dialogue, escalation, review, and completion across the agent organization.
6. [cli-contract.md](./cli-contract.md) defines the target public behavior of `th`. `AGENTS.md` requires following it for any CLI change.
7. [STATUS-MODEL.md](./STATUS-MODEL.md) defines the provider-agnostic work-state and runtime-health model.
8. [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md) defines authoritative worktree state and safety decisions across backend, CLI, MCP, and UI.
9. [HISTORY-CONTRACT.md](./HISTORY-CONTRACT.md) defines provider-neutral conversation identity, catalog, resume, recovery, archive, cache, and compatibility behavior.
10. [SOCKET-AUTH-DESIGN.md](./SOCKET-AUTH-DESIGN.md) defines the control-socket authorization tier, fleet spawn budget, and audit trail. Phases 1 through 3 are implemented; the optional phase 3 extensions remain open.

## Current Supporting Specifications

- [TESTING.md](./TESTING.md) defines the ten local test profiles and their measured costs. Verified against `package.json` on 2026-08-01.
- [MCP.md](./MCP.md) documents the MCP architecture, control-channel contract, and tool catalog, while the CLI-first roadmap governs its future surface.
- [DEV-BUILD.md](./DEV-BUILD.md) defines the side-by-side production and Dev variant boundary, including the separate socket, state root, and bundle identity.
- [CODEX-HARNESS.md](./CODEX-HARNESS.md) documents the Codex adapter seam, spawn presets, MCP provisioning, and resume wiring.
- [WORKTREE-WORKFLOW.md](./WORKTREE-WORKFLOW.md) remains the interaction and path-convention design, while the unified worktree contract governs status and safety.
- [SESSION_AWARENESS.md](./SESSION_AWARENESS.md) records the Claude-oriented event spine, while the two-axis status model governs provider-neutral semantics.
- [CAPTAIN-AUTONOMY-AND-SCOPED-GRANTS-PLAN.md](./CAPTAIN-AUTONOMY-AND-SCOPED-GRANTS-PLAN.md) is the General-directed integration specification for scoped Captain grants, existing-repository self-bootstrap, and bounded routine delivery; the phased plan remains authoritative for ownership, order, and activation.
- [BACKBURNER.md](./BACKBURNER.md) is the deliberately deferred list. Items there are parked with a reason, not forgotten.
- [PERFORMANCE-BENCHMARK.md](./PERFORMANCE-BENCHMARK.md) defines the process-tree measurement procedure used as one input to Package 6.
- [PACKAGE-5-LIVE-ACCEPTANCE.md](./PACKAGE-5-LIVE-ACCEPTANCE.md) and [PACKAGE-6-PERFORMANCE-CLOSURE.md](./PACKAGE-6-PERFORMANCE-CLOSURE.md) remain the release-evidence and performance-budget standards for a packaged candidate, independent of the itinerary that produced them.
- [PACKAGE-1-WINDOWS-E2E-REQUIREMENTS.md](./PACKAGE-1-WINDOWS-E2E-REQUIREMENTS.md) and [PACKAGE-2-WINDOWS-E2E-REQUIREMENTS.md](./PACKAGE-2-WINDOWS-E2E-REQUIREMENTS.md) define deferred packaged-Windows verification that source and browser tests cannot satisfy.
- [PRODUCTION-READINESS.md](./PRODUCTION-READINESS.md) supplies Alpha, Beta, and Stable release gates that have never been passed. Its 2026-07-13 measurements and its recommended feature pause are historical.
- [UX-RELIABILITY-AND-PERFORMANCE-ITINERARY.md](./UX-RELIABILITY-AND-PERFORMANCE-ITINERARY.md) is retained for its Package 0 through Package 6 requirements; its serialized execution program no longer governs sequencing.
- [SMOKE-TEST.md](./SMOKE-TEST.md) is a useful regression checklist, but its version-specific assertions must be checked against current source before use.
- [AUDIT-INTEGRITY-REVIEW-2026-07-26.md](./AUDIT-INTEGRITY-REVIEW-2026-07-26.md) records the fourteen audit-integrity gaps found while validating PR #80, their implemented corrections, regression evidence, and the accepted external rollback boundary.
- [WORKTREE-FIRST-IDE.md](./WORKTREE-FIRST-IDE.md) is a positioning and product-thesis essay rather than an execution plan.
- [POWDER-INTEGRATION.md](./POWDER-INTEGRATION.md) is a compatibility reference for retired Powder data and profiles.

## Completed Work Records

These record finished efforts. They are accurate about what happened and are not backlogs.

- [SIMPLIFICATION-PROGRESS.md](./SIMPLIFICATION-PROGRESS.md) tracks the `control.rs` and `workspace.ts` decomposition. All in-plan work is complete; further shrinkage would need a new decision.
- [CI-HEALTH-PLAN.md](./CI-HEALTH-PLAN.md) records a `main` CI outage and its resolution on 2026-07-24.
- [ORCHESTRATOR-WAKE-DESIGN.md](./ORCHESTRATOR-WAKE-DESIGN.md) is the design behind the shipped `watch_fleet` and `unwatch_fleet` tools.
- [AGENT-SESSION-SMOKE-0.3.106.md](./AGENT-SESSION-SMOKE-0.3.106.md) is a version-specific release smoke procedure superseded by the Package 5 and Package 6 evidence standards.
- [DEPOWDER-MIGRATION-IMPLEMENTATION-REPORT.md](./DEPOWDER-MIGRATION-IMPLEMENTATION-REPORT.md) is a point-in-time snapshot from the migration; its branch and ancestry gates are resolved.

## Historical Reviews and Shipped Plans

These preserve rationale and prior findings but are not active backlogs.
Branch names, terminal ids, installed versions, and worktree paths in this section are all stale.

- [AUDIT.md](./AUDIT.md) is a read-only audit of version `0.1.16`.
- [FEATURE-PLAN.md](./FEATURE-PLAN.md) records a feature wave that is marked shipped, and `docs/lanes/` holds its three per-lane kickoff briefs.
- [HERDR-PARITY.md](./HERDR-PARITY.md) records an earlier competitor-parity analysis whose main work is marked shipped.
- [PLAN.md](./PLAN.md) is the largely shipped `0.5` through `2.0` design-rationale record.
- [ROADMAP-PLAN.md](./ROADMAP-PLAN.md) is the earlier parity execution plan and shipped-wave record.
- [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) preserves the server-split design. M1 through M3 shipped; M4 and the M2b hardening tail are deferred, not scheduled.
- [PERF-AUDIT.md](./PERF-AUDIT.md) and [STRUCTURAL-FREEZE-ANALYSIS.md](./STRUCTURAL-FREEZE-ANALYSIS.md) are both explicitly superseded for the freeze root cause.
- [PERF-AND-DRAG-WORKLOG.md](./PERF-AND-DRAG-WORKLOG.md) is the master worklog for the earlier drag and freeze investigation.
- [HANDOFF.md](./HANDOFF.md) is an older historical handoff and must not replace the active contracts.
- [CAPTAIN-CHAT-PHASES.md](./CAPTAIN-CHAT-PHASES.md) and [CAPTAIN-SIDEBAR-PRD.md](./CAPTAIN-SIDEBAR-PRD.md) preserve earlier Captain UI slices that now feed the broader phased plan.
- [CODEX-SESSION-WORKFLOW-REVIEW-2026-07-20.md](./CODEX-SESSION-WORKFLOW-REVIEW-2026-07-20.md) is a read-only workflow review snapshot.
- [T1-REMOTE-VERIFICATION.md](./T1-REMOTE-VERIFICATION.md) is the never-run device-B recipe for the M2b remote wire.

### Powder-era records

Each of these carries its own historical banner and describes the retired Powder integration.
None authorizes current registration, dispatch, installation, or deployment.

- [CAPTAIN-POWDER-HANDOFF.md](./CAPTAIN-POWDER-HANDOFF.md), [LOCAL-POWDER-CAPTAIN-EXECUTION-PLAN.md](./LOCAL-POWDER-CAPTAIN-EXECUTION-PLAN.md), and [CAPTAIN-ACTIVE-ASSIGNMENT.md](./CAPTAIN-ACTIVE-ASSIGNMENT.md).
- [APPTURNITY-PROJECT-LINK-CORTANA-RUNBOOK.md](./APPTURNITY-PROJECT-LINK-CORTANA-RUNBOOK.md).
- [T1-DISPATCH-RECOVERY-REVIEW.md](./T1-DISPATCH-RECOVERY-REVIEW.md) and [T1-DISPATCH-RECOVERY-EXECUTION-DIRECTIVE.md](./T1-DISPATCH-RECOVERY-EXECUTION-DIRECTIVE.md).
- [WAVE0-CONTROL-INTEGRATION-SECURITY-REVIEW.md](./WAVE0-CONTROL-INTEGRATION-SECURITY-REVIEW.md) and [CODEX-PERMISSION-OBSERVABILITY-REVIEW.md](./CODEX-PERMISSION-OBSERVABILITY-REVIEW.md).
- Powder-era material in [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md) is historical unless the section explicitly points to an active agent-session contract.

## Archived or Abandoned Experiments

The native-client pivot is not the active product direction.
The webview application remains the product unless the General makes a new explicit decision.

- [NATIVE-FINISH-PLAN.md](./NATIVE-FINISH-PLAN.md) is explicitly archived.
- [NATIVE-PIVOT-EXECUTION.md](./NATIVE-PIVOT-EXECUTION.md) is execution evidence from the paused pivot.
- [NATIVE-RENDER-PIVOT.md](./NATIVE-RENDER-PIVOT.md) is earlier native-render planning.
- [T14-PARITY.md](./T14-PARITY.md) is the native-client parity audit and is not a current webview checklist.
- [T2-GPUI-SPIKE-RESULTS.md](./T2-GPUI-SPIKE-RESULTS.md) and [T7-FONT-CATALOGUE.md](./T7-FONT-CATALOGUE.md) are native-render experiment evidence.

## Documents Outside `docs/`

- `README.md` is the repository overview and build guide.
- `AGENTS.md` and `CLAUDE.md` are the repository agent instructions and are binding on every change.
- `PRD.md` is the product requirements document and `REVIEW.md` is the technical review of it, including the verified Claude Code integration facts. Both date from 2026-06-20 and predate the agent-session model.
- `apps/cli/README.md` documents the `th` CLI as shipped, alongside the target behavior in `cli-contract.md`.
- `skills/captain/SKILL.md` is the canonical Captain protocol, `skills/shipmate/SKILL.md` is its compatibility alias, and `skills/handoff/SKILL.md` is the cross-harness handoff skill.

## Drafts and User-Owned Artifacts

`docs/DECK-AGENTS-DESIGN.md` and `.lavish/` are user-owned artifacts and remain untouched unless the General explicitly authorizes changing their status.
`docs/DECK-AGENTS-DESIGN.md` is Git-excluded rather than tracked.
Draft documents may inform product discussion, but no draft overrides a canonical document without an explicit decision recorded in the phased plan.

## Review Procedure

1. Start with this index and the current working chain.
2. Use the canonical contracts for subsystem detail, and the phased plan for product decisions and dependencies.
3. Use historical reviews to understand rationale, then verify every proposed action against current source.
4. Record a supersession or status banner when a new review replaces an existing current document.
5. Add any new document to this index in the same commit that creates it.
6. Do not delete historical evidence merely to simplify the active backlog.
