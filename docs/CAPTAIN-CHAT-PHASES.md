# Captain Chat - multi-captain phases

Status: outline agreed 2026-07-06; phase 1 SHIPPED 2026-07-06 (PR #10, 0.3.40).
Sidebar fleet surface (slice A of CAPTAIN-SIDEBAR-PRD.md) + voice settings SHIPPED 2026-07-06 (PRs #11-#13, 0.3.41).
Phase 2 GREEN-LIT by the general 2026-07-06 (with slice B of the sidebar PRD riding on it).
Context: the single-captain overlay shipped in 0.3.39 (PR #9): pin one session, Ctrl+B C summons it over any tab, Shift+Esc interrupts it, Esc dismisses.
The fleet doctrine runs one captain per ship and the general runs several ships at once, so the overlay must grow to multiple captains.
Decision from the general: keep ONE overlay panel and ONE chord; multi-captain means fast switching, not simultaneous floating panels.

## Phase 1 - captain list + switcher (UI-only, small) - SHIPPED (PR #10, 0.3.40)

- The captain store becomes a LIST of pinned session ids plus an `activeCaptainId` (most-recently-summoned wins).
- Migration: `t-hub.captain.v1` single id loads as a one-entry list (`t-hub.captain.v2`); never lose an existing pin.
- Pinning is additive: any tile can be pinned while others stay pinned; unpin per tile; killing a session unpins it (existing cleanup path generalizes).
- Ctrl+B C summons the ACTIVE captain; pressing it again while summoned CYCLES to the next pinned captain (MRU order) instead of dismissing; Esc still dismisses.
- The overlay header grows a captain switcher (name + status dot per pinned captain, click to switch); switching reuses the same panel geometry.
- The titlebar anchor gets a count badge and a dropdown listing pinned captains (click = summon that one).
- Keyboard parity: palette entries "Summon captain: <name>" per pinned captain.
- Tests: migration, cycle order, unpin-while-summoned, adoption-drop unpins (extend the PR #9 suites).

## Phase 2 - ship-registry unification (captain identity has ONE source of truth)

- Today captain identity lives in two disconnected places: the UI's localStorage designation and the captain's own ship files (`~/.t-hub/captain/ships/<ship>.md`, `captain-terminal:` line).
- Move the mapping into the SERVER (a captains registry beside the PR #8 TabRegistry): `{ shipSlug, captainSessionId, workspaceTabIds }`, seq'd and synced to the UI exactly like tabs.
- Pinning in the UI becomes CLAIMING captaincy (server mutation, audited, organization tier); ship files remain the captain-side roster but the server registry is what the UI and MCP read.
- Context-aware summon: Ctrl+B C on a tab resolves the captain OWNING that workspace first, falling back to MRU; summoning from an unclaimed tab uses MRU.
- MCP surface: `list_captains` (read tier), `claim_captain`/`release_captain` (organization tier, audited) so captains can self-register on claim instead of hand-editing ship files.
- Survives restarts server-side; localStorage keeps only view state (panel geometry).

## Phase 2.5 - captain-owns-a-workspace + orchestrator-as-home (general's direction 2026-07-06)

- A captain OWNS a home workspace (the tab holding its tile; the mapping is phase 2's `workspaceTabIds`).
The UI's behavior becomes location-aware:
  - On the captain's OWN home workspace ("main view") the summon affordance (anchor dropdown / overlay) HIDES for that captain - you are already with it, so there is nothing to summon.
  - On any OTHER workspace the affordance shows, and navigating to a workspace sets the ACTIVE captain to that workspace's owner (context-aware summon made automatic on tab nav, not just Ctrl+B C).
  - The sidebar CAPTAINS section stays the always-available cross-captain navigator; the overlay is re-scoped to "reach a captain you are away from," not a thing you need at home.
- The ORCHESTRATOR is not a separate dock. It is a designated captain whose home workspace is the default/main view: you talk to it by being on its workspace (and can read its terminal history there when you want), and reach it from elsewhere via overlay or voice (the `speak` tool).
This dissolves the earlier bottom-dock idea - a dock would only duplicate "go to the orchestrator's workspace."
- Depends on phase 2 (the captain to workspace mapping); build as a UI slice on top of the registry. Voice input remains a later, separate add.

## Phase 3 - fleet view (the general's altitude in the UI)

- The titlebar anchor becomes a fleet menu: each ship with its captain, workspace, and a status dot fed by the supervision tree (working / needs-input / failed / idle).
- Needs-input from ANY captain surfaces as an attention badge on the anchor (reuse the existing attention queue); click = summon that captain directly.
- Overlay header shows a one-line ship status (crew count from the roster, open PRs if cheap).
- This is where T24-style supervision cues (native-only today, see the archive) get their webview port - tiles and the fleet menu say WHY something looks stalled.

## Phase 4 - candidates (do not build until asked)

- General broadcast: one message fanned to all captains.
- Captain-to-captain relay affordance in the UI (today relays go through the general or MCP).
- Per-captain chime identity (distinct sounds per ship).

## Standing adjacent goals (not captain-chat, tracked so they are not lost)

- Server split M2-M4: remote/multi-client access - the settled long-term priority (see SERVER-SPLIT-AND-ROADMAP.md).
- MCP parity: `create_worktree`, `remove_worktree`, `wait_for_status` as real MCP tools (today: raw-socket only; `close_tab` shipped in PR #8's catalog).
- Wire read-timeout in any client of the control socket (PR #6's risk note; PR #7 fixed the attach side).
