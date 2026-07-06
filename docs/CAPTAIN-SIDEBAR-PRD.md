# Captain sidebar section - PRD

Status: draft 2026-07-06, written after the general's review of the anchor dropdown fix (PR #11).
Author: crew, ship t-hub-native, worktree anchor-dropdown-portal.
Scope: PRD only - no code, no branch; delivery is sliced below so the general can green-light increments.

## Problem

The general supervises several captains at once, but the UI's only captain surfaces are transient: the anchor dropdown (a click-to-open list) and the overlay (one panel at a time).
There is no persistent, glanceable answer to "what are my captains doing right now, and does any of them need me".
Testing the fixed dropdown surfaced the concrete asks: larger rows with more context per captain (crew count, workspace), and ideally a dedicated CAPTAINS view in the sidebar with details of what crew or workspaces each captain controls.
The RECENT sidebar section also takes more vertical space than it earns (up to 38vh), crowding out exactly the kind of persistent surface captains need.

## Relationship to CAPTAIN-CHAT-PHASES.md

This is NOT a separate track.
The sidebar CAPTAINS section is the sidebar embodiment of phase 3 (fleet view - the general's altitude in the UI), and its data spine is phase 2 (ship-registry unification).
Two positioning changes to the phases doc fall out of this PRD:

- Phase 3 currently says "the titlebar anchor becomes a fleet menu".
  This PRD amends that: the persistent fleet surface moves to the sidebar; the anchor dropdown stays a thin switcher (rationale below) and the anchor keeps only the phase 3 attention badge.
- The spawnedBy crew-link plumbing (the one real data gap, below) belongs in the phase 2 server registry, not in a UI-side patch.

Phase 2 must still not start without the general's explicit go; that is why this PRD carves out a UI-only slice that ships value before phase 2.

## What the section shows

A new CAPTAINS section in the sidebar, above WORKSPACES (it is the command view; workspaces are the terrain view).
It reuses the existing `Section` pattern (chevron collapse persisted to localStorage, count badge, `th-scroll` body).
One row per pinned captain, MRU order matching the store, with:

- Status dot: the existing `CaptainStatusDot` (terminal state + bound session status + output activity), same semantics as tiles and the dropdown.
- Name: `useCaptainDisplayLabel` (user label first, then derived command and directory), identical to the overlay switcher and dropdown so a captain reads the same everywhere.
- Workspace: the tab the captain's tile lives in (already derivable - the liveness check does this lookup today); dimmed "tile not available" when the tab is popped out, same affordance as the dropdown rows.
- Crew summary: "3 running - 1 done", plus an amber outstanding-tasks badge when `outstandingTasks > 0`.
  Sourced from the supervision tree for the captain's session (see data notes for what "crew" means before and after phase 2).
- Attention roll-up: the row pulses amber while the captain's own session is in needs-permission or needs-question, even when the general is on another workspace tab.
  Slice A scope note: subagent children cannot express needs-input today (SubagentNode carries only running/completed), so the roll-up covers the captain session's own status; child and crew attention states join the roll-up in slice B once the registry links crew sessions.
  This is the single highest-value cue a persistent surface adds over the dropdown, and it is the webview port of the T24-style supervision cues phase 3 already promises.
- Context meter: a thin bar fed by the per-session `StatusSnapshot` (context percent, rate-limit state); rate-limited renders amber.
- Interactions: click a row = `summonCaptain` (same path as the dropdown, MRU front + focus); a chevron on the row expands the existing `SupervisionTree` component inline underneath (it already renders the orchestrator header plus per-subagent child rows - it just is not mounted anywhere near the captain list today).

## The dropdown stays a switcher

The anchor dropdown and the sidebar section serve different moments and should not be merged:

- The dropdown is a switcher: transient, MRU-ordered, one click or Ctrl+B C away, and it works even when the sidebar is collapsed to rail or hidden.
- The sidebar section is a supervision surface: persistent, glanceable, detailed, and expandable.

The dropdown just shipped working (PR #11) and costs nothing to keep.
Dropdown polish requested by the general - slightly taller rows and a second line with the workspace name (optionally the crew summary once it is cheap) - is a small UI-only change that rides with slice A, kept out of PR #11 so that stays a clean bug fix.

## Data notes and the spawnedBy gap

Available today with zero new plumbing:

- Pinned captains, MRU order, active id: captain store (`t-hub.captain.v2`).
- Captain terminal to session: `sessionNameForTerminal` + the supervision store's `sessionIdByTmux` index.
- Per-session status, context percent, cost, rate-limit state: supervision store `statuses` + `snapshots`.
- Orchestrator to children tree with running/completed state and `outstandingTasks`: supervision store `trees`, already rendered by `SupervisionTree.tsx`.
- Captain to workspace tab: workspace store `tabs[].order` membership (the existing liveness lookup).

The gap: the supervision tree's children are SUBAGENTS (Task-tool spawns reported by the session).
Crew in the org model are separate MCP-spawned sessions in their own worktrees, and nothing records "session X was spawned by captain Y" for those.
So the slice A crew summary honestly counts subagents, not crewmates.

Fix location: phase 2's server captains registry.
When a captain spawns a session through the control socket (`spawn_terminal` / `create_worktree`), the server records the requesting captain as `spawnedBy` and carries crew membership in the registry snapshot beside `{ shipSlug, captainSessionId, workspaceTabIds }`, seq'd and synced to the UI exactly like the PR #8 TabRegistry.
That gives the sidebar real crew counts, per-crewmate rows under an expanded captain, and "controlling workspaces" from `workspaceTabIds` - and it gives MCP `list_captains` the same truth, which is the whole point of phase 2's one-source-of-truth rule.
Crew-session attention states then join the roll-up (a crewmate stuck on a permission prompt bubbles amber onto its captain's row).

## RECENT section change

Cap the RECENT body at about 3 rows (a fixed pixel height of roughly 3 row-heights instead of today's `maxHeight: 38vh`), keeping the existing internal scroll and the persisted collapse toggle.
Trivial change; it frees the vertical space the CAPTAINS section moves into and matches how the general actually uses RECENT (a short scrollable tail, not a wall).

## Delivery slices

Slice A - UI-only (no server changes, can ship before phase 2 starts):

- RECENT capped to about 3 rows.
- CAPTAINS sidebar section with everything derivable today: status dot, name, workspace, subagent-based activity summary plus outstanding-tasks badge, attention roll-up from the captain session's own needs-permission and needs-question statuses (subagent nodes cannot express needs-input yet - child and crew attention arrive in slice B), context meter, inline SupervisionTree expansion, click to summon.
- Label the summary honestly at this stage (subagent activity, not crew) so the UI never claims data it does not have.
- Dropdown polish: taller rows, workspace second line.
- Tests: section render from seeded stores, summon wiring, attention roll-up precedence, RECENT cap.

Slice B - phase 2 dependent (needs the general's explicit go for phase 2):

- Server captains registry with `spawnedBy` recorded at the spawn path; registry snapshot sync to the UI; pinning becomes claiming.
- Sidebar crew summary switches from subagent counts to real crew membership; expanded captain lists crewmates with their own status dots; "controlling workspaces" from `workspaceTabIds`.
- Crew sessions join the attention roll-up.

Slice C - phase 3 alignment:

- Anchor attention badge (any captain needs input = badge on the anchor; click = summon that captain), per the existing phase 3 line.
- WHY-stalled cues on rows (the T24 supervision-cue port), ship slugs as row identity once the registry names ships.
- Amend CAPTAIN-CHAT-PHASES.md phase 3 wording: sidebar is the fleet surface; the anchor menu stays a switcher.

Recommended first green-light: slice A.
It is small, entirely inside existing stores and components, delivers the persistent surface and the attention cue immediately, and loses nothing when slice B upgrades the data underneath it.
