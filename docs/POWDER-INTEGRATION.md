# Powder Integration

T-Hub integrates with Powder through Powder's versioned HTTP API and does not modify or duplicate Powder state.
Powder remains authoritative for cards, claims, runs, work logs, input requests, and completion evidence.
T-Hub remains authoritative for projects, ships, terminal identity, Crew liveness, harness selection, checkout paths, and card/run-to-terminal bindings.

## Product Terms

A **codebase** is the Git checkout on disk that the user wants a Captain to manage.
The current API and persisted registry call its durable T-Hub record a **project**.
In normal use, one project represents one canonical Git repository checkout and its worktrees.
A **Powder board** is the Powder repository namespace containing cards for that codebase.
It is not another Git checkout.
A **Captain Assignment** is one durable responsibility within a project.
Multiple Captains may hold distinct Assignments in the same project.
A **Workspace** is one coherent workstream controlled by a Captain rather than a synonym for a project, Captain, or Crew member.

A project may be registered without Powder, but a commissioned Captain requires a verified Powder binding.
Older pinned Captain tiles may lack a project or Powder binding because pinning is only visual state and does not commission a Captain.

## User-Facing Captain Flow

Captain creation has three valid entry paths:

- Use a saved codebase already registered in T-Hub.
- Choose an existing WSL folder or Git repository and register it.
- Create a brand-new codebase directly or ask Cortana to prepare it.

The complete Cortana contract and new-project transaction are defined in [ORCHESTRATOR-OPERATING-MODEL.md](./ORCHESTRATOR-OPERATING-MODEL.md).

After selecting an entry path, the intended creation flow is:

1. Open **Commission Captain**.
2. Choose a saved codebase, browse WSL for an existing folder, or configure a new codebase.
3. Let T-Hub detect the canonical main worktree, display name, remote, default branch, and existing worktrees.
4. Select or confirm the matching Powder board.
5. Keep the protected Powder connection profile under **Advanced** unless more than one valid profile is available.
6. Describe the Captain's assignment.
7. Choose the Captain harness, Codex or Claude.
8. Review a preflight summary covering the codebase, Git state, Powder health and authorization, board match, assignment, and harness.
9. Commission the Captain.

Commissioning creates a control-capability Captain runtime, starts the selected Harness, claims a durable identity, binds it to the Project and Assignment, and provides bootstrap instructions.
The Captain remains visible through the Captains surface without forcing creation of an unrelated work Workspace.
If any required preflight or startup step fails, commissioning fails closed and rolls back the incomplete Captain.

The current dialog does not yet implement the WSL browser, automatic Git and Powder discovery, simplified terminology, or the preflight summary.
It still exposes manual repository path, project name, Powder repository, and connection profile fields.

## Captain Options and Crew Flow

Commissioning creates only the Captain.
It does not create Crew automatically.

After commissioning, the Captain can:

- Read the project, assignment, Powder board, active Crew, and recovery state through `captain_bootstrap`.
- Decompose the assignment into independent Powder cards.
- Create, name, close, and reconcile coherent Workspaces for related work.
- Dispatch a Crew member for an existing card with a selected Codex or Claude harness.
- Run Crew in the canonical checkout or a validated Git worktree belonging to the project.
- Select a branch and a shared workspace for each Crew member.
- Monitor Crew status, context, questions, blockers, terminal output, Powder claims, and completion evidence.
- Send verified input, focus a Crew terminal, checkpoint recovery state, interrupt its own Crew, and close completed Crew safely.
- Message another Captain for coordination or technical help without gaining authority over that Captain or its Crew.

Six concurrent Crew is the initial operating default for this computer rather than a proven hard limit.
Packaged 1, 4, 8, and 16-session measurements must establish warning and queue thresholds before T-Hub enforces a resource cap.
Durable Crew are leaf workers and do not create more durable Crew.
Work that needs another orchestration layer should receive another commissioned Captain.

Each Crew dispatch requires an existing Powder card belonging to the Project's default or explicitly bound Assignment board.
`dispatch_crew` validates the checkout and card, creates a read-capability terminal, claims the card, records the Crew binding, launches the chosen harness, verifies liveness, and rolls the operation back if any step fails.

## Crew in Workspaces

A Captain always remains reachable through the Captains surface and its durable terminal binding.
A Crew member appears as a normal terminal tile in the workspace selected during dispatch.
Crew working on the same coherent workstream should normally share one named Workspace.
Unrelated work in the same Project should use separate Workspaces even when one Captain controls both.

When dispatch supplies `tabId`, T-Hub places the Crew tile in that existing workspace.
When dispatch supplies `tabName`, T-Hub uses or creates the named workspace.
When neither is supplied, the terminal placement falls back to the currently active workspace and then the first available workspace.
The Captain protocol should therefore always select the destination workspace deliberately.

The same Crew member also appears under the Captain's expandable sidebar row with its live status.
Crew attention states roll up to the Captain row so a permission request or question remains visible while another workspace is active.
Closing a Crew terminal attempts to release its Powder claim and updates the durable roster.

The backend and sidebar support this model, but the desktop application does not yet provide a dedicated graphical **Create Crew** flow or a clear workspace-placement preview.
Crew creation is currently driven by the commissioned Captain through the T-Hub control tools.

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
