# Powder Integration

T-Hub integrates with Powder through Powder's versioned HTTP API and does not modify or duplicate Powder state.
Powder remains authoritative for cards, claims, runs, work logs, input requests, and completion evidence.
T-Hub remains authoritative for projects, ships, terminal identity, Crew liveness, harness selection, checkout paths, and card/run-to-terminal bindings.

## Connection Profiles

Projects persist only a Powder repository and connection profile name in `~/.t-hub/captains.json`.
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
`captain_bootstrap` reconstructs the project, assignment, Crew roster, Powder mapping, and reset instructions without relying on model memory or terminal location.

`dispatch_crew` validates that the requested checkout belongs to the project and that the Powder card belongs to the bound repository.
It starts a bare read-capability terminal, obtains the authoritative Powder claim, persists the card and run binding, and only then launches the selected harness.
If any step fails, T-Hub closes the terminal, releases the claim when one exists, and removes the uncommitted Crew record.

T-Hub runs a lease reconciler every five minutes.
It renews claims only when tmux proves the Crew terminal is alive.
It releases claims and marks Crew removed when tmux proves the terminal is gone.
It makes no lease mutation when terminal liveness is ambiguous.
Closing a Crew terminal also attempts to release its Powder claim after the process tree is stopped.

## Recovery

After an app restart, the reconciler reads `captains.json` and resumes lease management from the persisted card, run, project, and profile bindings.
After a Codex or Claude reset, the Captain calls `captain_bootstrap` using its ship slug or current terminal ID before accepting work.
No claim is inferred from local state alone, and no new Powder-backed Crew is dispatched while the Powder API is unavailable.

## Upstream Notes

No Powder code changes are required for this integration.
Powder's `work-log-appended` event exists in code but is not listed in `docs/card-events-v1.md`; that documentation gap should be corrected upstream before T-Hub depends on the event contract.
T-Hub event-tail ingestion and durable event-sequence reconciliation remain a separate integration stage.
