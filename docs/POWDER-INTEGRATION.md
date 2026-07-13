# Powder Integration

T-Hub integrates with Powder through Powder's versioned HTTP API and does not modify or duplicate Powder state.
Powder remains authoritative for cards, claims, runs, work logs, input requests, and completion evidence.
T-Hub remains authoritative for projects, ships, terminal identity, Crew liveness, harness selection, checkout paths, and card/run-to-terminal bindings.

## Connection Profiles

Projects persist only a Powder repository, connection profile name, and event cursor in `~/.t-hub/captains.json`.
Endpoints and credentials are resolved from `~/.t-hub/powder-profiles.json`.
On Unix, T-Hub refuses to read this file unless it is private, such as mode `0600`.

```json
{
  "schemaVersion": 1,
  "profiles": {
    "production": {
      "baseUrl": "https://powder.example.internal",
      "agentName": "t-hub",
      "apiKeyCommand": "op read 'op://Agents/POWDER_API_KEY__t-hub/credential'"
    }
  }
}
```

`agentName` must match the identity of the agent-scoped Powder API key.
This lets routine Crew operations use an agent key instead of an admin key.
T-Hub maps Powder card and run IDs to exact terminal IDs in its own durable registry, so Powder does not need to accept terminal-ID impersonation.

`apiKeyCommand` is preferred because it resolves the key when T-Hub creates a client and does not persist the key in Captain state.
`apiKeyEnv` may name an environment variable containing the key.
`apiKey` is accepted for protected single-user installations but keeps the secret in the profile file.

The `default` profile can be supplied without a file by setting `POWDER_API_BASE_URL`, `POWDER_AGENT_NAME`, and either `POWDER_API_KEY` or `POWDER_API_KEY_CMD`.
`T_HUB_POWDER_PROFILES_FILE` overrides the profile file location.

## Lifecycle

`register_project` records a canonical Git main worktree.
`bind_project_powder` maps that project to one Powder repository and one connection profile.
`commission_captain` starts Codex or Claude with a control capability, claims a durable ship, and persists its project and assignment.
Commissioning checks Powder health before starting the Captain process.
`captain_bootstrap` reconstructs the project, assignment, Crew roster, Powder mapping, and reset instructions without relying on model memory or terminal location.

`dispatch_crew` validates that the requested checkout belongs to the project and that the Powder card belongs to the bound repository.
It starts a bare read-capability terminal, obtains the authoritative Powder claim, persists the card and run binding, and only then launches the selected harness.
If any step fails, T-Hub closes the terminal, releases the claim when one exists, and removes the uncommitted Crew record.

T-Hub runs a lease reconciler every five minutes.
It renews claims only when tmux proves the Crew terminal is alive.
It releases claims and marks Crew removed when tmux proves the terminal is gone.
It makes no lease mutation when terminal liveness is ambiguous.
Closing a Crew terminal also attempts to release its Powder claim after the process tree is stopped.

## Event Synchronization

T-Hub polls Powder's read-authorized `/api/v1/events/tail` SSE endpoint every 15 seconds by default.
`T_HUB_POWDER_EVENT_POLL_SECS` changes the interval, with a five-second minimum.
Each project stores a monotonic Powder event cursor alongside its repository binding in `captains.json`.
Creating or changing a Powder binding snapshots the current event head so a new Captain does not receive the instance's historical backlog.
An idempotent rebind preserves the existing cursor.
Events from other repositories advance the cursor without being delivered, because Powder sequences are global to the instance.
Events for the bound repository are added to the active Captain's durable T-Hub inbox before the cursor advances.
The wake-up includes the Powder event ID and instructs the Captain to re-read the authoritative card before acting.
If T-Hub stops after enqueueing but before persisting the cursor, the wake-up may be delivered again after restart.
Captains must therefore treat event IDs as idempotency keys.
If the Captain inbox is full or the Powder stream cannot be read, the cursor does not advance past the undelivered event.
T-Hub validates the `powder.card_event.v1` envelope but treats event names and change payloads generically.

## Recovery

After an app restart, the reconciler reads `captains.json` and resumes lease management from the persisted card, run, project, and profile bindings.
After a Codex or Claude reset, the Captain calls `captain_bootstrap` using its ship slug or current terminal ID before accepting work.
No claim is inferred from local state alone, and no new Powder-backed Crew is dispatched while the Powder API is unavailable.

## Upstream Notes

No Powder code changes are required for this integration.
Powder's `work-log-appended` event exists in code but is not listed in `docs/card-events-v1.md`; that documentation gap should be corrected upstream before T-Hub depends on the event contract.
T-Hub does not branch on that event name, so the documentation gap does not affect generic event synchronization.
