# T-Hub Review and Planning Index

## Purpose

This index prevents historical audits, shipped execution plans, and abandoned experiments from being mistaken for current work.
When documents disagree, use the precedence below and verify claims against the current source and installed build.
Historical documents should remain intact as design rationale and evidence unless a separate cleanup explicitly archives them.

## Canonical Current Documents

1. [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md) is the authoritative forward roadmap, dependency map, testing doctrine, and exit-gate definition.
2. [CAPTAIN-POWDER-HANDOFF.md](./CAPTAIN-POWDER-HANDOFF.md) is the current runtime evidence and zero-context resume handoff.
3. [ORCHESTRATOR-OPERATING-MODEL.md](./ORCHESTRATOR-OPERATING-MODEL.md) defines the current Cortana, Project, Assignment, Captain, Workspace, and Crew operating model.
4. [cli-contract.md](./cli-contract.md) defines the target public behavior of `th`.
5. [STATUS-MODEL.md](./STATUS-MODEL.md) defines the provider-agnostic work-state and runtime-health model.
6. [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md) defines authoritative worktree state and safety decisions across backend, CLI, MCP, and UI.

## Current Supporting Specifications

- [PRODUCTION-READINESS.md](./PRODUCTION-READINESS.md) supplies release-quality gates that remain applicable where the phased plan has not superseded their sequencing.
- [PERFORMANCE-BENCHMARK.md](./PERFORMANCE-BENCHMARK.md) defines the packaged runtime measurement procedure.
- [POWDER-INTEGRATION.md](./POWDER-INTEGRATION.md) describes the Powder integration boundary and protected profiles.
- [HISTORY-CONTRACT.md](./HISTORY-CONTRACT.md) defines provider-neutral conversation identity, catalog, resume, recovery, archive, cache, and compatibility behavior.
- [MCP.md](./MCP.md) documents the existing MCP and control-channel implementation, while the CLI-first roadmap governs its future surface.
- [WORKTREE-WORKFLOW.md](./WORKTREE-WORKFLOW.md) remains the interaction and path-convention design, while the unified worktree contract governs status and safety.
- [SESSION_AWARENESS.md](./SESSION_AWARENESS.md) records the existing Claude-oriented event spine, while the two-axis status model governs provider-neutral semantics.
- [SMOKE-TEST.md](./SMOKE-TEST.md) is a useful regression checklist, but version-specific assertions must be checked against the current phased plan before use.

## Historical Reviews and Shipped Plans

These documents preserve rationale and prior findings but are not active backlogs.

- [AUDIT.md](./AUDIT.md) is a read-only audit of version `0.1.16` and is explicitly historical.
- [FEATURE-PLAN.md](./FEATURE-PLAN.md) records a feature wave that is marked shipped.
- [HERDR-PARITY.md](./HERDR-PARITY.md) records an earlier competitor-parity analysis whose main work is marked shipped.
- [PLAN.md](./PLAN.md) is the largely shipped `0.5` through `2.0` design-rationale record.
- [ROADMAP-PLAN.md](./ROADMAP-PLAN.md) is the earlier server-split execution plan and shipped-wave record.
- [SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) preserves the server-split design and earlier multi-client decisions.
- [PERF-AUDIT.md](./PERF-AUDIT.md) is explicitly superseded for the original freeze diagnosis.
- [PERF-AND-DRAG-WORKLOG.md](./PERF-AND-DRAG-WORKLOG.md) is the historical master worklog for the earlier drag and freeze investigation.
- [HANDOFF.md](./HANDOFF.md) is an older handoff and must not replace the Captain and Powder handoff.
- [CAPTAIN-CHAT-PHASES.md](./CAPTAIN-CHAT-PHASES.md) and [CAPTAIN-SIDEBAR-PRD.md](./CAPTAIN-SIDEBAR-PRD.md) preserve earlier Captain UI slices that now feed the broader phased plan.

## Archived or Abandoned Experiments

The native-client pivot is not the active product direction.
The webview application remains the product unless the General makes a new explicit decision.

- [NATIVE-FINISH-PLAN.md](./NATIVE-FINISH-PLAN.md) is explicitly archived.
- [NATIVE-PIVOT-EXECUTION.md](./NATIVE-PIVOT-EXECUTION.md) is execution evidence from the paused pivot.
- [NATIVE-RENDER-PIVOT.md](./NATIVE-RENDER-PIVOT.md) is earlier native-render planning.
- [T14-PARITY.md](./T14-PARITY.md) is the native-client parity audit and is not a current webview checklist.
- [T2-GPUI-SPIKE-RESULTS.md](./T2-GPUI-SPIKE-RESULTS.md) and [T7-FONT-CATALOGUE.md](./T7-FONT-CATALOGUE.md) are native-render experiment evidence.

## Drafts and User-Owned Artifacts

`docs/DECK-AGENTS-DESIGN.md` and `.lavish/` are user-owned artifacts and remain untouched unless the General explicitly authorizes changing their status.
Draft documents may inform product discussion, but no draft overrides a canonical document without an explicit decision recorded in the phased plan.

## Review Procedure

1. Start with this index and the canonical phased plan.
2. Read the current handoff for installed-runtime facts.
3. Use supporting specifications for subsystem detail.
4. Use historical reviews to understand rationale, then verify every proposed action against current source.
5. Record a supersession or status banner when a new review replaces an existing current document.
6. Do not delete historical evidence merely to simplify the active backlog.
