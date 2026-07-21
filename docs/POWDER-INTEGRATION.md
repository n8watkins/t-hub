# Powder Integration

> Historical compatibility reference: Powder is retired from active T-Hub
> workflows.
> The definitions and API notes below are retained for migration, legacy-data
> interpretation, and compatibility tombstones only.

T-Hub integrates with Powder through Powder's versioned HTTP API and does not modify or duplicate Powder state.
Powder remains authoritative for cards, claims, runs, work logs, input requests, and completion evidence.
T-Hub remains authoritative for projects, ships, terminal identity, Crew liveness, harness selection, checkout paths, and card/run-to-terminal bindings.
The complete relationship between Powder work evidence, T-Hub durable dialogue, lifecycle attention, and technical proof is defined in [AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md](./AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md).
Powder is the executable backlog and evidence ledger rather than the general Captain and Crew chat transport.

## Product Terms

A **codebase** is the WSL folder identified by the Project `rootPath` that the user wants a Captain to manage.
The current API and persisted registry call its durable T-Hub record a **project**.
In normal use, one Project represents one canonical selected root and may have `vcsCapability: "git"` or `vcsCapability: "none"`.
Git Projects separately retain optional `gitMainRoot` metadata.
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

1. Open **Create Captain**.
2. Choose a saved codebase, browse WSL for an existing folder, or configure a new codebase.
3. Let T-Hub canonicalize the selected `rootPath` and validate the explicit display name.
4. Select or confirm the matching Powder board.
5. Keep the protected Powder connection profile under **Advanced** unless more than one valid profile is available.
6. Describe the Captain's assignment.
7. Choose the Captain harness, Codex or Claude.
8. Review a preflight summary covering the codebase, Git state, Powder health and authorization, board match, assignment, and harness.
9. Commission the Captain.

Commissioning creates a control-capability Captain runtime, starts the selected Harness, claims a durable identity, binds it to the Project and Assignment, and provides bootstrap instructions.
The Captain remains visible through the Captains surface without forcing creation of an unrelated work Workspace.
If any required preflight or startup step fails, commissioning fails closed and rolls back the incomplete Captain.

For an existing or new unsaved codebase, the graphical flow calls `register_project` with only the authoritative `rootPath` and explicit display `name`, then calls `commission_captain`.
These are separate backend transactions.
A commissioning failure rolls back incomplete Captain state but preserves the useful Project, Git repository, and any codebase leaf owned by the earlier successful registration.
The product still needs an explicit shared resume-or-rollback contract rather than whole-flow atomicity.

The current source implements the three entry choices, WSL browsing, optional Git capability, protected-profile discovery under **Advanced**, a bounded Powder board selector, current terminology, and a reviewed preflight summary.
The new-codebase path supports an explicitly named empty non-Git Project, while template, clone, and complete packaged success flows remain open.

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

Captain-authorized card authoring is the target contract rather than an installed capability.
Installed `0.3.103` has no sanctioned card-create operation, so the General currently must create routine implementation cards manually.
The T4 plan item adds card create, update, relation, proof-plan, and status operations over Powder's generic API through the shared backend, CLI, MCP, and UI catalog.

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

Projects persist canonical root identity, optional Git metadata, and Powder repository, connection profile name, and event cursor in `~/.t-hub/captains.json`.
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

`register_project` validates and persists an existing or explicitly created canonical `rootPath` without initializing Git.
`initialize_git` is the separate explicit operation that adds Git capability and `gitMainRoot` metadata.
It may also validate and persist the selected Powder binding.
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

Completion, renewal, heartbeat, release, close, cleanup, and reconciler work must be serialized through one per-Crew lifecycle-operation guard.
Automated completion must remain disabled until Powder can enforce the expected current run atomically and T-Hub can reconcile ambiguous results without duplicate effects.

## Event Synchronization

T-Hub polls Powder's read-authorized `/api/v1/events/tail` SSE endpoint every 15 seconds by default.
`T_HUB_POWDER_EVENT_POLL_SECS` changes the interval, with a five-second minimum.
Each project stores a monotonic Powder event cursor alongside its repository binding in `captains.json`.
Creating or changing a Powder binding snapshots the current event head so a new Captain does not receive the instance's historical backlog.
An idempotent rebind preserves the existing cursor.
Events from other repositories advance the cursor without being delivered, because Powder sequences are global to the instance.
The current implementation adds events for the bound repository to one active Captain's durable T-Hub inbox.
That compatibility behavior is insufficient for multiple Assignment-owning Captains and must not be treated as the target routing contract.

Target routing resolves an event's exact card and run binding to its Crew, owning Captain, Assignment, and Project before delivery.
Routine progress updates the authoritative Powder-backed Board state without creating conversational inbox history.
Actionable input, blocker, claim-conflict, completion, failure, or recovery events create typed lifecycle attention for the owning identities and instruct them to reread the authoritative card.
An event with no exact owner remains a visible unbound Project event for the General or Cortana rather than being assigned to an arbitrary Captain.
An event with conflicting owners enters a visible routing-conflict state and does not advance into any Captain's dialogue history.

The routed event includes the Powder event ID and uses it as the idempotency key.
If T-Hub stops after enqueueing but before persisting the cursor, the wake-up may be delivered again after restart.
Captains must therefore treat event IDs as idempotency keys.
The cursor advances only after the event is durably stored and either routed to exact owners or retained as an unbound or conflicting Project event.
If durable routing storage is unavailable or the Powder stream cannot be read, the cursor does not advance past the event.
T-Hub validates the `powder.card_event.v1` envelope but treats event names and change payloads generically.

## Recovery

After an app restart, the reconciler reads `captains.json` and resumes lease management from the persisted card, run, project, and profile bindings.
After a Codex or Claude reset, the Captain calls `captain_bootstrap` using its ship slug or current terminal ID before accepting work.
No claim is inferred from local state alone, and no new Powder-backed Crew is dispatched while the Powder API is unavailable.

## Generic Powder Dependencies

The initial integration could use Powder's existing read, claim, heartbeat, renew, and release operations without changing Powder.
The dedicated architecture review found that safe automated evidence and completion need generic Powder guarantees that the current API does not provide.
These are concurrency and audit-integrity improvements for every Powder client rather than T-Hub-specific accommodations.

Powder must provide:

- Server-enforced run-bound conditional completion with a durable operation identity and idempotent replay.
- Operation-status recovery for ambiguous work-log and completion responses.
- Current-run work-log attribution with the normalized stored record returned authoritatively.
- Current-run acceptance-criterion scope with authenticated reviewer identity.
- A versioned create-only or equivalent non-overwriting repository operation before any client implements create-if-absent.
- Idempotency, operation recovery, authorization, and revision preconditions for card create, update, relation, proof-plan, and status mutations where the existing contracts do not already provide them.
- Complete versioned event documentation, including the existing `work-log-appended` event and its compatibility expectations.

T-Hub owns the policy and integration around those primitives.
T-Hub must preserve exact Project, Assignment, Captain, Crew, card, run, terminal, and worktree authority, bound responses, role-filtered access, retry recovery, packaged UX, and fail-closed behavior.
T-Hub must not implement a read-then-write approximation for a missing Powder precondition.
The authoritative dependency IDs, acceptance gates, and proposed cards are recorded under **Review-Derived Change Boundary** in [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).

## Upstream Notes

No Powder changes are required for T-Hub identity, terminals, Workspaces, Harness adapters, inbox messaging, board relevance, UI, performance, or runtime cleanup.
Generic Powder changes are required for the safe conditional and retry contracts listed above before T-Hub enables automated completion or create-if-absent board provisioning.
Powder's `work-log-appended` event exists in code but is not listed in `docs/card-events-v1.md`; that documentation gap should be corrected upstream before T-Hub depends on the event contract.
T-Hub does not branch on that event name, so the documentation gap does not affect generic event synchronization.
