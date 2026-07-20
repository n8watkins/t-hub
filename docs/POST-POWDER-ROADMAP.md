# Post-Powder Agent-Session Roadmap

This is the active roadmap for T-Hub after Powder retirement.

The product authority is the local T-Hub registry, the owning Captain, the durable agent-session record, the checkpoint stream, and the event cursor.

Powder is not a runtime dependency, an authorization source, a network service, or an MCP tool catalog entry.
Legacy Powder fields remain readable only for schema compatibility.

## Current acceptance surface

- `start_agent` creates the durable record before launch, applies the spawn governor, launches Codex or Claude, and binds the spawned identity to the owning Captain ship.
- `agent_checkpoint` permits the exact agent to checkpoint its own bounded progress and stage.
- `list_agents` excludes stopped agents by default and exposes stopped history through `state: "removed"` with cursor pagination.
- `agent_events` exposes lifecycle and checkpoint deltas with bounded batches and cursor-expiry signaling.
- Restart recovery reads the registry before provider activity and preserves explicit work stages.
- MCP advertises the agent-session tools and no retired Powder tools.

## Next gates

1. Reconcile provider conversation identity from Codex and Claude launch evidence after startup and restart.
2. Reconcile runtime state to running, idle, needs-permission, exited, or unavailable from provider and terminal evidence.
3. Add process round-trip coverage for CLI, MCP, the start-agent dialog, both providers, safe close, restart, and removed-history pagination.
4. Keep legacy schema fixtures and zero-network Powder assertions in the release suite.
5. Build matching Windows and WSL components from one reviewed release commit before activation.

## Authority rules

The authenticated exact agent may write only its own checkpoint and may select only `working`, `needsInput`, or `readyForReview`.

The owning Captain may supervise its agents and advance lifecycle state.

General or Cortana authority may recover and inspect fleet state according to the control capability contract.

No caller may infer authority from a working directory, terminal name, provider prompt, historical Powder field, or external record.
