# Orchestrator wake - design

Order: `~/.t-hub/captain/orders/t-hub-2026-07-08-idle-notifications.md` (Cortana, on behalf of the general).

## Problem

The orchestrator (Cortana) has no push signal when a supervised session - especially a captain - goes idle, needs input, or completes.
It must POLL the control socket to notice, so captains get stranded "waiting for the orchestrator" and the orchestrator does not know it is being waited on.
This stranded the Appturnity captain today.

## Reproduction (2026-07-08, against the live app)

Two independent root causes, both verified:

1. **The poll path is broken for captains.**
   The captains registry keys a captain by its 8-char **tile id** (`CaptainRecord.captain_session_id`, e.g. `280699db`).
   The supervision reducer keys by the **Claude session UUID** (e.g. `568bf956-...`).
   `get_status` / `supervision_tree` / `wait_for_status` pass the caller id straight to the supervisor, so calling any of them with a captain's `captainSessionId` returns `status:"unknown"` / `null`.
   The orchestrator cannot even poll a captain's status by the id `list_captains` gives it.
   Verified: `get_status {sessionId:"280699db"} -> {status:"unknown"}`, `get_status {sessionId:"568bf956-..."} -> {status:"needsQuestion"}`.

2. **There is no push path.**
   Status transitions fan out centrally on the `session://status` event (server-split M1), but the only callers of `tmux::send_text` / `send_keys` are the explicit `send_text` / `send_keys` control commands.
   Nothing converts a transition into a wake.
   An idle orchestrator sits at its prompt and is never re-invoked.

## Key facts about the architecture

- Session status lives in `Supervisor` (`supervision.rs`): an event-driven reducer over the agent journal.
  On each actual status change it appends `(seq, uuid, status)` to a bounded transition log and emits `session://status` (`{session_id: uuid, status}`) + `supervision://tree`.
- The **id bridge already exists in memory**: `StatusBridge` (`claude/status.rs`, held as `ControlContext.status`) maps `uuid -> StatusSnapshot`, and each snapshot carries `tmux_session` (`th_<tile>`) + `cwd`.
  So `uuid -> tile` is a live reverse lookup; the durable twin is the `tile_sessions` DB table.
- The **captains registry** (`CaptainsRegistry`, shared `Arc` in `ControlContext.captains`) is authoritative + persistent and keyed by tile id.
- **Injection** is `tmux::send_text(th_<tile>, text, enter=true)` - types a line into a Claude Code session and submits it, re-invoking its agent loop. This is the only thing that wakes an idle Claude Code loop.
- There is **no stored orchestrator identity**: nothing records "session X supervises captains A,B,C". The orchestrator is whoever calls `list_captains`.

## Design

Three pieces. All wake behaviour is **opt-in**: it fires only for an orchestrator that has explicitly armed a watch, so a fleet with no armed orchestrator sees zero behaviour change (this is the safety boundary, in place of a global default-off flag - cf. the Phase 3 rollback lesson).

### 1. Fix the id bridge (prerequisite)

`get_status` / `supervision_tree` / `wait_for_status` resolve their `sessionId` through a new helper: if the id is not a known supervisor key but IS a live tile, map `tile -> uuid` via `StatusBridge` and use that.
Adds `StatusBridge::session_for_terminal(tile) -> Option<uuid>` and `terminal_for_session(uuid) -> Option<tile>`.
After this, the orchestrator can poll a captain's status by its `captainSessionId`.

### 2. FleetWatchRegistry + `watch_fleet` / `unwatch_fleet`

A new shared `Arc<FleetWatchRegistry>` (wired like `captains`), holding one `FleetWatch` per orchestrator:

```
FleetWatch {
  orchestrator_tile_id: String,   // where to inject the wake
  scope: Captains | All | Sessions(Vec<tile>),  // default Captains
  states: Vec<SessionStatus>,     // default the actionable set
}
```

- `watch_fleet {scope?, states?}` - the calling session (identified by its tile id) arms a watch. Requires a live terminal (like `claim_captain`).
- `unwatch_fleet {}` - disarm.
- Actionable set (default): `Completed`, `NeedsQuestion`, `NeedsPermission`, `Failed`, `RateLimited`, `Expired` - i.e. the order's idle/turn-complete, needs-input, completed/exited buckets. Not `Working` / `WaitingOnSubagents` / `Detached` / `Restoring` / `Unknown`.

### 3. FleetNotifier (server-side push)

Wired in `lib.rs setup()`. Observes every session status **edge** via a lightweight observer callback added to `AgentBridge::emit_session` (which already computes `(uuid, status)` on the journal path).
It holds `Arc` handles to the watch registry, captains registry, and status bridge.

On each edge `(uuid, status)`:

1. Resolve `uuid -> tile` (StatusBridge). Look up whether `tile` is a captain (captains registry) -> `shipSlug`, `captainSessionId`.
2. For each armed `FleetWatch` whose scope includes `tile` and whose `states` includes `status`, and where `tile != orchestrator_tile_id` (never wake on your own transition):
   build a payload `{sessionId: tile, captainSessionId, shipSlug, state, uuid, seq, ts}` and enqueue a pending wake for that orchestrator.
3. **Gate + coalesce**: if the orchestrator is idle, flush immediately by injecting a wake line via `tmux::send_text`.
   If the orchestrator is mid-turn (`Working` / `WaitingOnSubagents`), hold; flush (coalesced into one message listing all pending captains) when the orchestrator's own next idle edge is observed.
   A per-orchestrator min-interval rate-limit prevents storms on idle flapping.
4. Also emit `fleet://wake` on the event stream (UI badge / voice cue - the bonus, secondary to the machine wake).

Injected wake line (a compact, routable prompt the fleet-orchestrator skill consumes):

```
[T-HUB FLEET WAKE] captain "ship-280699db" (280699db) -> needsQuestion. Supervise it. (seq 42, 2026-07-08T20:15:03Z)
```

## Status + live validation

Landed on branch `feat/orchestrator-wake` (0.3.50 -> 0.3.54), fully unit- + E2E-tested (see below).
The running app when this was built was the older 0.3.49 binary, so the machine-consumable path was proven in a faithful harness (real tmux), not yet against the live app + real Claude sessions.
Final live validation (rebuild + relaunch the Tauri app, arm `watch_fleet`, drive a real captain idle, watch Cortana's terminal get the injected turn) belongs to the merge/release step the general owns - the app cannot be relaunched from inside its own supervised session without killing that session.

Test coverage proving the full chain:
- `agent::tests::status_observer_fires_on_the_journal_consume_path` - journal event -> supervisor -> `emit_session` -> observer `(uuid, status)`, terminal edge `Completed`.
- `fleet::tests::*` (7) - the notifier: routing, orchestrator-idle gating, coalescing, one-wake-per-idle-window, non-captain scope, failed-injection retry, never-wake-self.
- `fleet::tests::wake_lands_in_a_real_orchestrator_pane_e2e` - the REAL tmux injection: a captain going idle types the wake line into a live orchestrator pane, read back via capture-pane. Injected line observed:
  `[T-HUB FLEET WAKE] captain "ship-e2e" (e2ecap01) -> completed. Supervise it (get_status / read_terminal, then act).`
- `control::tests::get_status_resolves_a_captain_tile_id_to_its_claude_uuid` + `claude::status::tests::reverse_lookups_bridge_tile_and_session_ids` - the id bridge.
- `tools::tests::fleet_wake_tools_are_exposed_with_the_right_tiers` - the MCP surface.

## Separate gap: captain crew-spawn tools (surfaced, not solved here - per the order)

Captains are NOT missing the tools: `spawn_terminal` / `create_worktree` are in the MCP catalog, and a T-Hub-spawned session is injected the FULL control token by default (`elevation_env`, control.rs ~2110), so the server-side capability tier is satisfied.
The blocker is one (or more) of these gates, in likelihood order - a definitive root-cause needs reproducing the Appturnity captain's spawn attempt, which is out of scope for this order:

1. **Claude Code permission-confirmation gate (most likely).**
   The t-hub MCP marks process-changing tools (`spawn_terminal`, `send_text`, `send_keys`, `close_terminal`) as confirmation-required.
   That confirmation is a Claude Code permission prompt - an autonomous captain with no human at its terminal cannot self-approve it, so the tool call never reaches the server.
   Fix: a captain-scoped auto-approve (allowlist those tools in the captain's Claude Code settings when it is claimed), or a distinct lower-gate captain crew-spawn tool.
2. **Server-side UI-presence gate.**
   `spawn_terminal` refuses when no UI is connected to adopt the tile (`apply_sink.is_none() && fanout.subscriber_count()==0`, control.rs ~3735).
   In a running app with the webview this is satisfied, so it is a blocker only for headless/CLI contexts.
3. **`spawnedBy` linkage.** Crew is only recorded as a captain's crew if the spawn passes `spawnedBy: <captainTileId>`; without it the session spawns but is not addressable as that captain's crew.

**Verdict: FOLLOW-ON effort, independent of this notification work.**
It is a capability/permission-binding problem, not a missing tool, and touches the Claude Code permission model + the server spawn gate rather than the supervisor/notifier path this order changed.
Rough size: small-to-moderate (a captain-scoped tool allowlist is small; a clean headless-spawn redesign is moderate).
