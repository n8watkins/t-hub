# Captain Autonomy and Scoped Grants Integration Plan

> Historical planning record: the Powder-binding and Powder-card sections below
> describe pre-retirement design work.
> The active model uses durable agent sessions and local checkpoints as defined
> in [DEPOWDER-MIGRATION-PLAN.md](./DEPOWDER-MIGRATION-PLAN.md).

## Status and intended consumer

This document is a General-requested integration plan for the agent already working on the current T-Hub goal.
It is written for an agent with no access to the conversation that produced it.
It does not replace the active goal, `docs/PHASED-PRODUCTION-PLAN.md`, or any narrower canonical contract.
The consuming agent must compare this plan with the active goal, merge the missing decisions and dependency edges into that goal, and avoid creating duplicate cards for work that is already represented.
When this plan and a canonical document differ, the consuming agent must preserve the safer behavior until the General's decision is recorded in the canonical phased plan.

The source inspection for this plan used T-Hub branch `fix/captain-control-runtime` at commit `a3b136a` on 2026-07-17.
The live reproduction used the installed T-Hub control surface from Captain terminal `d3f3535d`.
No credential, token, or protected Powder profile content is included here.

## Executive decision

Captains need materially more autonomy than the current role-only policy provides.
The intended model is scoped delegation rather than unrestricted fleet authority.
A Captain should be able to administer its own Project, Assignment, Powder board, cards, Crew, worktrees, routine local development, and bounded delivery workflow without returning to the General for every ordinary step.
A Captain must not gain authority over another Project, another Captain, foreign Crew, protected credentials, production data, customer outreach, spending, protected branches, releases, or destructive fleet operations merely because it has T-Hub control capability.

The implementation should therefore introduce typed, durable, visible, audited, revocable grants.
Each grant must be scoped to durable identity and exact resources rather than prose, terminal location, current working directory, or possession of a broad control token.
Named profiles may improve usability, but every profile must expand into explicit operation and constraint records that the backend evaluates.

## Immediate motivating reproduction

The Appturnity ship exposes the current bootstrapping deadlock.

- The canonical WSL Git main worktree is `/home/natkins/projects/appturnity`.
- The checkout was clean at commit `6f3671a25e311a9f9ba31a9f7e6b68cc1d71a842` during reproduction.
- It is one Git repository with `client`, `server`, and `app` application areas.
- It has no nested Git repositories or submodules.
- Powder profile `n8desktop-wsl` already contains the canonical `appturnity` board with zero cards.
- T-Hub has no durable Project record for the Appturnity Git root.
- Captain terminal `d3f3535d` has a live Captain claim, control capability, no `projectId`, and no Crew.
- The Captain called `register_project` with the exact Git root, Project name `appturnity`, profile `n8desktop-wsl`, and Powder board `appturnity`.
- T-Hub rejected the request with `acl: only General/Cortana may register a new project`.
- The rejection occurred before any path-normalization or Powder-binding behavior was exercised.

This proves that the immediate blocker is the new-Project ACL, not monorepo topology.
It also proves a circular dependency in the current Captain workflow.
A Captain cannot attach to a Project until the Project exists, but the Captain cannot register the exact repository in which it was asked to work.

The current operational workaround remains valid while this plan is implemented.
General or Cortana may register the exact Appturnity Project, after which the Captain can attach and continue.
The workaround must not be mistaken for resolution of the product gap.

## Existing authority and roadmap sources

The consuming agent must read these files before changing the active goal or code.

1. `docs/REVIEW-INDEX.md`
2. `docs/PHASED-PRODUCTION-PLAN.md`
3. `docs/CAPTAIN-POWDER-HANDOFF.md`
4. `docs/ORCHESTRATOR-OPERATING-MODEL.md`
5. `docs/AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md`
6. `docs/POWDER-INTEGRATION.md`
7. `docs/cli-contract.md`
8. `docs/WORKTREE-STATUS-CONTRACT.md`
9. `docs/STATUS-MODEL.md`

The following existing roadmap items already own important parts of this work.
This plan must extend or compose with them instead of creating parallel implementations.

| Existing item | Responsibility consumed by this plan |
| --- | --- |
| P5 | Safe create-if-absent Powder board operation that cannot overwrite a concurrent creator. |
| P6 | Idempotency, recovery, authorization, and revision preconditions for Powder card mutations. |
| T4 | Captain-authorized Powder card create, update, relation, proof-plan, and status operations. |
| T5 | Idempotent and resumable Captain creation with stable operation identity. |
| T6 | Canonical board relevance, unambiguous auto-binding, and P5-backed board creation. |
| T7 | Durable Assignment identity and multiple Captains per Project. |
| T8 | Exact Project, Assignment, card, run, Crew, and Workspace event ownership. |
| T9 | Fail-atomic inbox persistence. |
| T10 | Inbox bounds, retention, backpressure, and overflow states. |
| T11 | One actor-aware backend, CLI, MCP, and UI operation catalog with role-filtered discovery. |
| T17 | Authoritative Crew, Captain, claim, terminal, and cleanup state. |
| T18 | Unified worktree status and ownership service. |
| T23 | Durable typed Project run and preview profile. |
| T25 | Control-plane modularization without behavior drift. |
| T26 | Plan status and dependency reconciliation. |
| T33 | Provider-neutral work profiles with explicit Harness, model, effort, and permission resolution. |
| Phase 6 | Durable inbox, typed permission requests, approval decisions, and peer coordination. |
| Phase 7 | Saved, existing, and new codebase onboarding plus safe rollback. |

## Verified current limitations

The following limitations are verified from current source, the installed tool catalog, or canonical documentation.

### Hard ACL restrictions

- `enforce_project_authority` rejects a new Project when the caller is a Captain, even when the requested root is the Captain's exact current canonical Git root.
- A Captain may mutate a Project only after an existing Captain record already owns that `projectId` under the same ship.
- `commission_captain` remains General or Cortana only.
- A Captain may attach only its own terminal and cannot attach a different terminal as Captain.
- Cross-ship session access and lifecycle mutation remain denied.

The same-ship and cross-ship boundaries are correct and must remain.
The missing case is a controlled self-bootstrap transition from an active, control-capable, unbound Captain to one exact existing Project.

### Missing Powder planning operations

- Installed T-Hub can list Powder boards and bind an existing Project to one board.
- Installed T-Hub cannot create a Powder board.
- Installed T-Hub has no sanctioned Captain card-create operation.
- Installed T-Hub does not expose Captain card update, relation, proof-plan, general work-log, priority, reopen, cancel, supersede, or status operations.
- `dispatch_crew` requires an existing card in the bound board.
- The General must currently create routine implementation cards manually.

This is explicitly acknowledged in `docs/POWDER-INTEGRATION.md` and represented by P5, P6, T4, and T6.

### Missing durable delegation

- Prose authorization does not change a T-Hub role ACL.
- The current authorization artifact is too coarse to represent routine Project-scoped standing grants safely.
- MCP does not expose the full durable inbox and typed approval lifecycle required by the agent relationship contract.
- `my_capability` reports only broad read or control capability and does not enumerate actor-visible operation grants and constraints.
- The static MCP catalog is not filtered to the exact actor's durable role and grants.
- Same-ship process-changing operations still rely on host confirmation rather than consuming a durable scoped standing grant.

### Missing dispatch and runtime controls

- `dispatch_crew` accepts the Harness but not an explicit work profile or local permission profile.
- The backend currently resolves a fixed Crew permission posture rather than recording a Captain-selected scoped profile in the dispatch contract.
- Dispatch requires an existing worktree and cannot transactionally prepare the card, branch, worktree, grant, Crew binding, claim, terminal, and Harness as one recoverable operation.
- Codex lifecycle and provider-aware recovery are not yet authoritative for every state transition.
- Context pressure, provider limits, and degraded adapter capabilities are not presented as one reliable Captain decision surface.

### Missing owned-resource lifecycle

- Worktree removal is suspended until T18 provides authoritative ownership and safety decisions.
- Managed browser ownership is incomplete.
- Managed development-server and preview ownership is incomplete.
- Project run targets are not represented by an accepted typed profile.
- Terminals, worktrees, browsers, development servers, previews, process trees, temporary profiles, Powder claims, and leases do not yet share one complete resource lifecycle.
- A Captain cannot safely answer whether every owned resource is active, leased, orphaned, dirty, landed, or removable from one authoritative snapshot.

### Missing routine delivery grants

T-Hub does not currently provide a durable Project-scoped grant model for these ordinary delivery operations.

- Push an owned non-protected branch.
- Create or update a draft pull request.
- Request review on an owned draft pull request.
- Inspect or retry CI for an owned branch.
- Create and retire a bounded preview deployment.
- Provision bounded non-production resources.
- Apply isolated development or preview migrations.
- Reference Project environment variables without exposing secret values.

These operations may be performed through other tools when separately authorized, but T-Hub cannot represent a reusable, scoped delegation for them.

## Goals

The integrated goal must produce the following outcomes.

1. A control-capable Captain can safely bootstrap its exact existing Git root into a T-Hub Project when granted that capability.
2. The Captain can bind or auto-bind an existing authorized Powder board whose canonical repository identity matches the Project.
3. The Project registration, board binding, Captain attachment, Assignment binding, checkpoint, and response are idempotent and recoverable.
4. A Captain can author and maintain routine work cards only on its authorized board and Assignment.
5. A Captain can choose and record a permitted Crew work profile at dispatch.
6. A General can issue standing or one-time typed grants with exact scope, limits, expiry, and revocation.
7. A Captain can delegate only an explicit subset of its own routine authority to its own Crew.
8. A Captain can manage its own worktrees and other owned resources through authoritative safety decisions.
9. A Captain can perform bounded non-protected delivery operations when a delivery grant permits them.
10. Every denial is attributed, structured, actionable, and visible without revealing credentials or foreign Project state.
11. Every supported operation has equivalent backend, CLI, MCP, and applicable UI semantics through T11.
12. The installed application passes the complete role, retry, restart, and adversarial E2E matrix.

## Non-goals

This plan does not make Captains fleet-wide administrators.
It does not permit authority based only on a working directory, folder name, terminal tab, prompt text, inherited environment variable, or Powder card body.
It does not permit Captains to mint their own unrestricted grants.
It does not permit Captains to elevate a Crew member's T-Hub token from read to control.
It does not permit cross-Project mutation, foreign Crew control, Captain retirement, or peer Captain seizure.
It does not permit secret disclosure, credential-command disclosure, or protected profile mutation.
It does not permit production data deletion, customer outreach, spending, public repository creation, protected-branch merge, production deployment, publication, or release without a separately authorized grant class.
It does not add a T-Hub-local substitute for a missing Powder concurrency or idempotency primitive.
It does not re-enable worktree removal before T18's authoritative safety service exists.

## Authority model

### Separate the three authority axes

The implementation must keep these axes distinct.

1. Durable organizational identity answers who the actor is and which Project, Assignment, ship, and Crew relationship it owns.
2. T-Hub capability answers whether the session may invoke read, organization, or process-changing control operations at all.
3. Scoped grants answer which otherwise-eligible operations the durable actor may perform against which exact resources and constraints.

Harness local execution permission is a fourth independent axis.
It controls filesystem, subprocess, sandbox, and approval behavior inside Codex, Claude, or a future Harness.
It must never silently expand the session's T-Hub authority.

### Grant record

Define one versioned durable grant record with at least these fields.

| Field | Required meaning |
| --- | --- |
| `schemaVersion` | Versioned public persistence and API contract. |
| `grantId` | Stable non-secret identifier. |
| `issuerIdentity` | App-stamped durable General or authorized delegator identity. |
| `issuerRole` | App-stamped role used for the issuance decision. |
| `subjectIdentity` | Durable Captain, Assignment, or exact Crew identity. |
| `subjectRole` | Expected role at evaluation time. |
| `projectId` | Exact existing Project, when one exists. |
| `bootstrapRoot` | Exact canonical main-worktree identity for a pre-Project bootstrap grant. |
| `assignmentId` | Exact durable Assignment, when applicable. |
| `shipSlug` | Durable ship ownership boundary. |
| `powderProfile` | Protected profile name only, never endpoint or credentials. |
| `powderBoards` | Exact allowed canonical board names or identifiers. |
| `operations` | Explicit versioned operation identifiers. |
| `pathConstraints` | Exact roots or reviewed descendant rules. |
| `branchConstraints` | Exact names or safe reviewed patterns. |
| `environmentConstraints` | Local, test, preview, staging, or production boundary. |
| `resourceLimits` | Crew count, process count, CPU, memory, ports, leases, or preview bounds. |
| `spendLimit` | Optional amount and currency for a separately authorized spending class. |
| `argumentDigest` | Optional exact-arguments binding for a one-time approval. |
| `cardId` | Optional exact Powder card boundary. |
| `runId` | Optional exact Powder run boundary. |
| `validFrom` | Earliest valid time. |
| `expiresAt` | Mandatory expiry for elevated or external-effect grants. |
| `maxUses` | Optional use count, with one as the default for high-consequence approvals. |
| `uses` | Durable consumption evidence. |
| `createdAt` | Creation time. |
| `revokedAt` | Optional revocation time. |
| `revokedBy` | App-stamped revoker identity. |
| `reason` | Human-readable bounded purpose. |
| `sourceAuthorizationId` | Link to the originating typed General decision, when applicable. |

The record must not contain API keys, bearer tokens, credential commands, raw environment values, or secret material.
The persistence layer must use atomic write, flush, rename, reopen, corruption recovery, bounds, and private-file permissions consistent with the identity and inbox stores.

### Grant kinds

Use one underlying schema with distinct evaluated modes.

#### Standing grant

A standing grant covers routine repeatable operations within narrow Project and Assignment constraints.
It remains valid until expiry or revocation.
Examples include own-board card authoring, same-ship Crew lifecycle, routine project-local builds, and pushing owned non-protected branches.

#### One-time approval

A one-time approval binds one exact operation, target, argument digest, and expiry.
It is consumed atomically with the operation.
Examples include a protected merge, production deployment, one customer campaign, or a bounded purchase.

#### Delegated subset

A Captain may delegate only operations it currently holds and only to Crew it owns.
The delegated grant must be a strict subset across operations, Project, Assignment, board, card, run, worktree, branch, environment, resource limits, expiry, and uses.
The Captain may never delegate T-Hub control capability or a General-only operation.

### Named profiles

Profiles are usability templates, not hidden authority.
The preflight must show the expanded operations and constraints before a profile is issued.

#### `captain-standard`

This should be the normal default after Project attachment.

- Read the owned Project, Assignment, board, cards, evidence, resources, and non-secret configuration.
- Create and maintain routine cards on the owned board after P6 and T4 are ready.
- Create owned branches and worktrees.
- Dispatch, steer, heartbeat, recover, review, complete, and safely close own Crew.
- Run routine local build, test, lint, formatting, typecheck, browser, code-generation, and isolated migration work.
- Install repository-local dependencies when repository policy allows it.
- Start and stop Project-owned local development processes through typed run profiles.
- Push owned non-protected branches only when the branch grant includes that pattern.

#### `captain-project-builder`

This optional profile must be explicitly issued for a reviewed parent directory and Powder namespace.

- Create one absent leaf under the approved parent.
- Initialize Git only when the reviewed operation requested it.
- Clone one exact approved remote or instantiate one reviewed template.
- Create one Powder board only through P5's non-overwriting create contract.
- Register, bind, self-attach, and checkpoint through one T5-compatible operation identity.
- Preserve all pre-existing directories and files on failure.

#### `captain-delivery`

This optional profile covers non-production delivery.

- Push owned non-protected branches matching the grant.
- Create or update draft pull requests from those branches.
- Request review from an allowed reviewer set.
- Inspect and retry CI associated with the owned branch.
- Create and retire bounded previews and non-production resources.
- Read secret existence and configuration metadata without reading secret values.

#### `captain-production-operator`

This must never be a default standing profile.
It should normally resolve to one-time approvals with exact targets, argument digests, limits, and expiry.

## Authorization evaluation order

Every mutation must use one shared evaluator in this order.

1. Authenticate the control connection and resolve the per-session identity.
2. Resolve the durable role, ship, Project, Assignment, and Crew ownership from authoritative registries.
3. Verify the broad T-Hub capability tier required by the operation.
4. Resolve the operation through the shared actor-aware catalog.
5. Enforce absolute role and cross-ship denials that no ordinary grant may bypass.
6. Resolve unexpired, unrevoked grants for the durable subject.
7. Match the exact Project or reviewed bootstrap root.
8. Match the Assignment, board, card, run, worktree, branch, environment, resource, and external target constraints.
9. Verify the operation arguments or argument digest.
10. Verify remaining uses, concurrency, resource, time, and optional spending limits.
11. Persist a pending operation and idempotency identity before side effects when the mutation is retryable.
12. Execute through the authoritative backend service.
13. Persist grant consumption, operation result, and audit evidence atomically where the contract requires it.
14. Return a bounded structured result or a stable attributed denial.

No UI, CLI, MCP adapter, Powder card, message body, or Harness prompt may implement an independent grant decision.

## Required operation bundles

### Bundle A: Existing-repository self-bootstrap

This is the first user-visible vertical slice and the direct Appturnity fix.

Add an operation such as `captain.bootstrap_existing_project` that accepts or derives:

- The calling Captain identity.
- The calling Captain's live terminal.
- The exact requested WSL path.
- A reviewed display name.
- An optional existing Powder board and protected profile name.
- A durable Assignment description or Assignment identifier.
- One stable operation ID.

The backend must:

1. Require a live control-capable Captain identity.
2. Require a valid bootstrap grant whose `bootstrapRoot` resolves to the same canonical main worktree.
3. Resolve a subdirectory or linked worktree input to the canonical main worktree without accepting a foreign root.
4. Detect an existing matching Project and converge on it instead of creating a duplicate.
5. Refuse multiple Projects resolving to the same canonical main worktree.
6. Refuse a Project already owned by a conflicting ship or Assignment unless a separately authorized transfer exists.
7. List boards only through the protected profile.
8. Auto-bind only one exact canonical repository match.
9. Require explicit reviewed selection when no unambiguous match exists.
10. Never create a board until P5 is available and the grant permits it.
11. Register the Project, bind the board, attach the current Captain, bind the Assignment, and checkpoint through one recoverable operation identity.
12. Preserve a useful registered Project if a later attachment step fails, while returning an explicit resume or rollback state.
13. Never touch a separate dirty checkout merely because its remote or directory name resembles the canonical root.

The current `register_project` handler should not receive a blanket `Captain` exception.
Its authorization should call the shared grant evaluator with an existing-root operation and exact canonical target.

### Bundle B: New Project Builder flow

Compose this bundle with Phase 7, T5, T6, T18, T21, T23, T33, and P5.

The reviewed transaction should support:

- One absent empty-codebase leaf.
- One approved Git clone source.
- One approved template source.
- Explicit Git initialization.
- An exact Project name and Assignment.
- Existing-board selection or P5-backed create-if-absent.
- Harness, model, effort, and permission profile selection.
- Optional initial Workspace only when the Assignment names a coherent workstream.

The transaction must expose preflight, operation status, resume, and safe rollback.
It must never remove or replace a pre-existing directory.
It must never use Powder's unsafe upsert as create-if-absent.

### Bundle C: Captain Powder card authoring

Implement this through T4 only after each P6 mutation gate is satisfied.

The actor-aware catalog should expose bounded operations for:

- Card create.
- Card read and list.
- Title, body, priority, and metadata update.
- Acceptance-criterion create, update, reorder, and delete under stable criterion identity.
- Proof-plan create and update.
- Dependency, blocker, parent, child, duplicate, and related-card relations.
- Status transition.
- Reopen, cancel, supersede, and restore where Powder's generic contract supports them.
- Captain planning and review work-log entries with authenticated Captain attribution.

Every mutation must bind the protected profile and exact authorized board from the Project and Assignment.
Caller-supplied endpoints, credentials, arbitrary profiles, arbitrary boards, and foreign card IDs must be rejected.
Every retryable mutation must use Powder's stable operation identity and revision or precondition contract.

### Bundle D: Crew dispatch and delegated work profiles

Extend the dispatch contract without weakening the existing exact card, run, Crew, worktree, branch, and Harness binding.

Add:

- An explicit requested work-profile identifier.
- The expanded local Harness permission mode.
- The exact T-Hub operations delegated to the Crew.
- Resource limits and expiry.
- A grant or approval reference.
- The stable dispatch operation ID.
- A preflight result that shows effective provider, Harness, model, effort, permission mode, T-Hub capability, and degraded adapter features.

The backend must verify provider-native permission evidence after launch as the current recovery work requires.
The backend must persist the requested and resolved work profile and refuse silent fallback.
Crew should retain read-capability T-Hub tokens unless a separately reviewed future operation proves a need for more.
Local unrestricted Harness execution must not imply T-Hub control authority.

### Bundle E: Standing permission and durable inbox surface

Integrate this bundle with Phase 6, T9, T10, and T11.

Expose CLI and MCP parity for:

- Request grant.
- Issue grant.
- List effective grants.
- Explain an authorization decision.
- Revoke grant.
- Cancel a pending request.
- Inspect grant use and expiry.
- Send, list, read, reply, acknowledge, accept, decline, complete, retry, cancel, and supersede durable messages.
- Request, decide, cancel, consume, and inspect one-time approvals.

The UI should show the Captain's effective profiles and expanded operations.
The Captain should be able to answer why an operation is permitted or denied without seeing foreign Project information.

### Bundle F: Owned resource lifecycle

Do not implement this independently of T18 and `docs/WORKTREE-STATUS-CONTRACT.md`.

Captains need one authoritative Project-owned resource snapshot covering:

- Crew terminals.
- Utility terminals.
- Worktrees and branches.
- Browsers and temporary profiles.
- Development servers and preview processes.
- Windows and WSL subprocess trees.
- Ports and preview URLs.
- Powder claims and runs.
- Leases, ownership, activity, and cleanup eligibility.

Create, stop, reuse, and cleanup operations must serialize against the same snapshot and freshness proof.
Dirty, unmerged, main, locked, leased, claimed, stale, unknown, and foreign resources must fail closed.

### Bundle G: Routine delivery grants

Delivery grants should be implemented only after the grant evaluator and shared operation catalog are stable.

Initial operations should include:

- `git.push_owned_branch`
- `github.create_or_update_draft_pr`
- `github.request_review`
- `ci.inspect_owned_branch`
- `ci.retry_owned_branch`
- `preview.create`
- `preview.inspect`
- `preview.destroy`
- `nonprod.migrate`

Protected branches, production environments, customer messages, releases, public repository creation, and spending must remain separately gated.
The backend should prefer integration adapters that return stable resource identity and operation status rather than treating arbitrary shell text as an authorized action.

### Bundle H: Actor-visible capability discovery

Extend T11 so an agent can discover only the operations it may meaningfully request.

Provide a bounded result that distinguishes:

- Supported and currently granted.
- Supported but requires a grant.
- Supported but requires one-time approval.
- Temporarily unavailable because a safety service is incomplete.
- Blocked on an external dependency such as P5 or P6.
- Unsupported by the selected Harness or provider.
- Absolutely denied for the role.

The result must include constraints and actionable next steps without exposing secret values or foreign Project metadata.

## Absolute General and Cortana boundaries

The following operations must remain General-gated or require a separately explicit typed approval.

- Mutating another Project or Assignment.
- Controlling another Captain or foreign Crew.
- Changing a session's T-Hub capability tier.
- Minting an unrestricted grant.
- Reading or exporting credentials.
- Changing protected Powder profile endpoints or credential commands.
- Installing system-global software.
- Writing or deleting production data.
- Sending customer outreach or public communications.
- Spending or accepting a paid plan.
- Creating a public external repository.
- Merging a protected branch without a documented merge grant.
- Deploying to production.
- Publishing or releasing artifacts.
- Destructive cleanup without authoritative ownership and landed-work proof.
- Retiring a Captain or transferring its durable Assignment without the required authority.

The General may issue a typed grant for a narrow instance of some of these operations.
The grant must never turn the Captain into an apex actor or weaken cross-ship isolation.

## Proposed implementation tranches

The consuming agent should integrate these tranches into the active goal according to its current dependencies.
Do not start all tranches merely because this document exists.

### Tranche 0: Reconcile the active goal

1. Read the current goal, branch, active Powder cards, Crew, and landing state.
2. Compare this plan against P5, P6, T4, T5, T6, T7, T9, T10, T11, T18, T23, T26, T33, Phase 6, and Phase 7.
3. Record which requirements already exist, which need stronger acceptance criteria, and which are genuinely new.
4. Update the canonical phased plan only with the General's requested Captain-autonomy decision and clear dependency edges.
5. Avoid rewriting current runtime evidence in `docs/CAPTAIN-POWDER-HANDOFF.md` unless new runtime evidence was directly verified.
6. Create near-term Powder cards only for dependency-ready, independently executable slices.

### Tranche 1: Scoped grant contract and evaluator

Suggested new card: `thub-captain-scoped-grants`.

1. Define the versioned grant and decision schemas.
2. Define absolute-denial, standing-grant, one-time-approval, and delegated-subset rules.
3. Add pure policy predicates and exhaustive role matrices.
4. Add durable bounded storage and recovery.
5. Add structured authorization explanations and audit events.
6. Integrate the evaluator with T11's actor-aware operation catalog.
7. Preserve current behavior behind an explicit compatibility migration until the new evaluator is proven.

Exit gate:
Every operation in the catalog has one testable authorization descriptor, and an identified Captain cannot exceed Project, Assignment, ship, board, branch, environment, or expiry constraints even with a control token.

### Tranche 2: Existing-repository self-bootstrap

Suggested new card: `thub-captain-existing-project-self-bootstrap`.

1. Reproduce the Appturnity denial through the installed Captain flow.
2. Implement the exact-root bootstrap grant and shared operation.
3. Compose registration, existing-board binding, self-attachment, Assignment binding, checkpoint, and operation-status recovery.
4. Add CLI, MCP, and UI or conversational parity through T11.
5. Prove POSIX and extended-UNC canonical identity behavior without rewriting durable host identity.
6. Run the packaged Appturnity-equivalent E2E against a disposable repository and board.

Exit gate:
A granted Captain bootstraps one exact existing repository, retry and restart converge on one Project and Captain, and every foreign or ambiguous target fails before mutation.

### Tranche 3: Captain card authoring

Use T4 and do not create a duplicate T-Hub workstream.

1. Confirm each Powder mutation satisfies P6.
2. Add shared backend operations and authorization descriptors.
3. Add CLI-first contracts and MCP parity.
4. Add a bounded Captain planning UI or conversational surface after backend parity.
5. Prove idempotent retry, revision conflicts, foreign-board denial, and exact Captain attribution.

Exit gate:
A Captain can turn an Assignment into authoritative cards without General data entry and cannot mutate a card outside its authorized board or Assignment.

### Tranche 4: Dispatch profiles and standing approvals

Compose this with T33 and Phase 6.

1. Add work-profile selection and effective-authority preflight to dispatch.
2. Add durable grant request, decision, status, revocation, and consumption operations.
3. Add Captain subset delegation to own Crew.
4. Add full inbox and typed approval CLI and MCP parity.
5. Add reliable wake and attention transitions.

Exit gate:
The General can grant a routine class once, the Captain can use it repeatedly within constraints without repeated prompts, and changed targets or arguments fail visibly.

### Tranche 5: Owned resources and routine delivery

Compose this with T18, T23, and the selected integration adapters.

1. Complete the owned-resource snapshot and safety service.
2. Re-enable worktree cleanup only through that service.
3. Add typed run, browser, development-server, preview, and cleanup operations.
4. Add non-protected branch, draft PR, CI, and preview grants.
5. Keep production and destructive operations separately gated.

Exit gate:
Captains can carry routine work from card through reviewed non-protected delivery while every resource and external effect remains attributable, bounded, recoverable, and safely cleanable.

### Tranche 6: Optional Project Builder

Compose this with P5, T5, T6, T18, T21, T23, and T33.

1. Add reviewed empty, clone, and template sources.
2. Add safe Powder board create-if-absent.
3. Add one transaction and operation-status recovery across filesystem, Git, Project, board, Assignment, Captain, and checkpoint state.
4. Complete the packaged graphical and conversational matrix.

Exit gate:
A Project Builder Captain can create exactly one reviewed codebase under its grant, while every pre-existing path, unrelated board, and external resource remains unchanged.

## Suggested source ownership map

The exact module boundaries may change under T25, but the consuming agent should start with this map.

| Area | Current files and responsibilities |
| --- | --- |
| Pure policy | `apps/desktop/src-tauri/src/acl.rs` for role and relationship predicates. |
| Durable identity | `apps/desktop/src-tauri/src/identity.rs` and the Captain, Project, Assignment, and Crew registries currently concentrated in `control.rs`. |
| Authorization artifacts | `apps/desktop/src-tauri/src/authz.rs`. |
| Durable dialogue | `apps/desktop/src-tauri/src/inbox.rs` and its control handlers. |
| Control enforcement | `apps/desktop/src-tauri/src/control.rs`. |
| Git identity and worktrees | `apps/desktop/src-tauri/src/git.rs` plus T18's future shared service. |
| Filesystem creation boundaries | `apps/desktop/src-tauri/src/files.rs`. |
| Powder protected client | `apps/desktop/src-tauri/src/powder.rs`. |
| Harness profiles and attestation | `apps/desktop/src-tauri/src/harness/mod.rs`, `harness/codex.rs`, and `harness/claude.rs`. |
| CLI transport | `apps/desktop/src-tauri/src/control_client.rs` and `apps/cli/src/main.rs`. |
| MCP schemas | `apps/desktop/src-tauri/crates/t-hub-mcp/src/tools.rs`. |
| Project IPC | `apps/desktop/src/ipc/projects.ts`. |
| Captain onboarding UI | `apps/desktop/src/components/CaptainCommissionDialog.tsx` and its tests. |
| Board UI | `apps/desktop/src/components/BoardPanel.tsx` and its tests. |
| Canonical public contracts | `docs/PHASED-PRODUCTION-PLAN.md`, `docs/ORCHESTRATOR-OPERATING-MODEL.md`, `docs/AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md`, `docs/POWDER-INTEGRATION.md`, `docs/cli-contract.md`, and `docs/WORKTREE-STATUS-CONTRACT.md`. |

Do not add further policy directly to `control.rs` if T25 can provide a small owned module without delaying the safe vertical slice.
Do not split modules mechanically without preserving behavior and parity tests.

## Required schemas and public operations

The shared catalog should eventually include versioned descriptors for at least these operations.

### Project and Assignment

- `project.bootstrap_existing`
- `project.create_empty`
- `project.clone`
- `project.create_from_template`
- `project.bind_powder_existing`
- `project.create_and_bind_powder`
- `captain.attach_self`
- `assignment.create_or_bind`
- `project.operation_status`

### Grants and approvals

- `grant.request`
- `grant.issue`
- `grant.list_effective`
- `grant.explain`
- `grant.revoke`
- `approval.request`
- `approval.decide`
- `approval.cancel`
- `approval.status`

### Powder planning

- `powder.card.create`
- `powder.card.update`
- `powder.card.relate`
- `powder.card.update_proof_plan`
- `powder.card.transition`
- `powder.card.append_captain_log`
- `powder.card.operation_status`

### Crew and resources

- `crew.dispatch`
- `crew.recover`
- `crew.reassign`
- `crew.heartbeat`
- `crew.complete`
- `crew.close`
- `resource.list_owned`
- `resource.preflight_cleanup`
- `resource.cleanup`
- `run.start`
- `run.stop`
- `preview.create`
- `preview.destroy`

### Delivery

- `git.push_owned_branch`
- `github.create_or_update_draft_pr`
- `github.request_review`
- `ci.inspect_owned_branch`
- `ci.retry_owned_branch`
- `nonprod.migrate`

Operation names may change during shared-catalog design.
The semantic boundaries, identity binding, and parity requirements must remain.

## End-to-end reproduction and acceptance matrix

Bug fixes and policy changes must start with the closest practical end-user E2E reproduction.
The Appturnity sequence is the required first reproduction for self-bootstrap.
Use disposable repositories and boards for mutation tests after proving the live denial.

### Existing-repository self-bootstrap success cases

- Captain with a valid exact-root grant registers its current existing WSL Git main worktree.
- Captain starts in a subdirectory and converges on the same canonical main worktree.
- Captain starts in a linked worktree and converges on the registered main-worktree identity.
- Exact canonical Powder match auto-binds.
- Explicit reviewed board selection binds when no unique match exists.
- Retry with the same operation ID returns the same Project, Captain, Assignment, and checkpoint.
- Response loss after commit is recovered by operation-status lookup.
- Application and WSL restart resume the same partial operation.
- Existing matching Project converges without duplicate creation.

### Self-bootstrap denial cases

- No grant.
- Expired grant.
- Revoked grant.
- Wrong subject identity.
- Wrong role.
- Crew attempts to use a Captain grant.
- Foreign ship.
- Foreign repository root.
- Path outside the reviewed root.
- Different WSL distribution.
- Ambiguous canonical root.
- Conflicting existing Project.
- Conflicting Assignment or ownership.
- Powder board does not exist.
- Powder board belongs to a different canonical repository.
- Multiple matching aliases are ambiguous.
- Powder profile is unavailable or unauthorized.
- Caller supplies an endpoint, credential, or arbitrary profile not present in the grant.
- Argument digest differs from a one-time approval.

Every denial must occur before Project, board, Captain, Assignment, terminal, or checkpoint mutation unless the documented operation state explicitly records a resumable earlier success.

### Grant durability and security cases

- Atomic create failure returns no grant.
- Write, flush, rename, reopen, and restart failures never acknowledge an unpersisted grant.
- Duplicate issuance with the same idempotency identity converges.
- Conflicting reuse of an operation identity fails closed.
- Expiry is enforced across clock and restart boundaries.
- Revocation prevents subsequent use.
- One-time approval is consumed exactly once with the matching arguments.
- Concurrent use cannot exceed `maxUses` or resource limits.
- Captain delegation is a strict subset of its current authority.
- Revoking the parent grant invalidates or suspends delegated children according to the reviewed contract.
- No secret appears in persistence, JSON output, logs, audit events, or errors.
- Corrupt grant storage enters an honest degraded mode and prevents unsafe mutation.

### Card authoring cases

- Captain creates, updates, relates, and transitions cards only on the exact authorized board.
- Assignment-specific restrictions are enforced.
- Idempotent retry creates one card.
- Revision mismatch returns a structured conflict with no lost update.
- Foreign card and board identifiers are denied without existence leakage beyond permitted metadata.
- Captain planning logs are attributed to the Captain, not a Crew run.
- P6 operation-status recovery handles ambiguous responses.

### Crew dispatch cases

- Preflight displays requested and resolved Harness, model, effort, permission mode, T-Hub capability, grant, worktree, branch, card, and run.
- Provider-native launch evidence matches the requested permission mode.
- Missing, stale, malformed, wrapper-obscured, wrong-provider, and conflicting permission evidence fails and rolls back.
- Crew receives only the delegated subset and cannot invoke a Captain-only operation.
- Dispatch retry converges on one card claim, Crew binding, terminal, and Harness.
- Expired or revoked work profiles prevent new dispatch without seizing already running work silently.

### Resource and delivery cases

- Dirty, unmerged, main, locked, leased, claimed, stale, unknown, and foreign worktrees cannot be removed.
- A safely landed disposable worktree can be removed after UI and process detachment.
- Ordinary user browsers and processes are never targeted.
- Preview limits and expiry are enforced.
- Owned non-protected branch push succeeds when granted.
- Protected branch push or merge is denied without the required separate approval.
- Draft PR and CI retry are limited to the owned branch and repository.
- Production, customer outreach, spending, and public release operations remain denied under standard and delivery profiles.

### Role matrix

At minimum, test every relevant operation as:

- General.
- Cortana.
- Owning Captain.
- Peer Captain in the same Project with a different Assignment.
- Foreign Captain.
- Owning Crew.
- Sibling Crew.
- Foreign Crew.
- Read-capability unidentified caller.
- Control-capability caller with no valid per-session identity.
- Trusted in-process host.

Possession of a control token must not make a denied identity pass.

## CLI, MCP, and UI parity

Follow `docs/cli-contract.md` for every new CLI command.

- Validate all flags and positional arguments before side effects.
- Preserve clean JSON stdout and stable exit categories.
- Use bounded deterministic output with explicit totals and pagination.
- Return structured denial, conflict, expired, revoked, unavailable, retryable, and ambiguous-result errors.
- Require stable operation identities for retryable mutations.
- Provide operation-status lookup.
- Keep MCP a thin adapter over the same backend operation.
- Generate actor-visible tools from the shared catalog rather than extending the static list independently.
- Keep graphical preflight and results semantically equivalent to CLI and MCP.
- Never expose protected profile credentials to frontend code.

## Migration and compatibility

The plan must preserve current registered Projects, Captains, Crew, Powder bindings, claims, and sessions.

1. Add a versioned grant store with a safe empty default.
2. Treat absence of a grant as current behavior unless an existing explicit repository policy is migrated through a reviewed compatibility rule.
3. Do not infer grants from historical prose, Powder card text, shell history, or unstructured handoff text.
4. Preserve General and Cortana authority while moving their operations through the same catalog and audit path where practical.
5. Preserve current same-ship Captain control over its Crew.
6. Preserve Crew read-capability defaults.
7. Keep deprecated coarse authorization artifacts readable until replacement records and consumers are verified.
8. Provide explicit migration status and rollback for persisted schema changes.
9. Do not rewrite host-canonical Project roots merely to simplify WSL execution paths.
10. Keep credential profiles separate from Project and grant persistence.

## Observability and audit requirements

Every grant and evaluated mutation should emit bounded events containing:

- Stable operation ID.
- Grant or approval ID.
- Actor identity handle and role.
- Ship, Project, Assignment, and target type.
- Operation identifier.
- Permit or deny result.
- Stable denial kind.
- Constraint that failed, without foreign or secret detail.
- Retry, replay, or recovery disposition.
- Resource or Powder identity where the actor is authorized to see it.
- Timestamp and duration.

Do not emit prompt bodies, message bodies, credentials, raw environment variables, or unbounded command arguments by default.
Provide a Project-scoped audit view so the General and owning Captain can explain consequential actions.

## Threat model and failure rules

The implementation must explicitly cover these threats.

- Prompt injection attempts to use a standing grant outside its operation set.
- A control-capability token is stolen by a different session.
- A Captain tries to use a grant after moving to another Project or Assignment.
- A stale terminal ID points at a replacement session.
- A linked worktree path is confused with a foreign main worktree.
- A board alias is used to bind the wrong canonical repository.
- Concurrent actors register the same root or create the same board.
- Lost responses cause duplicate Projects, cards, Crew, pushes, previews, or deployments.
- A revoked or expired grant races an in-flight operation.
- A Captain delegates a wider scope than it owns.
- A Crew tries to use its Captain's environment token or grant reference.
- A retry changes operation arguments while reusing the identity.
- A storage failure returns a false successful grant, approval, or mutation.
- Logs or errors reveal credentials or foreign Project existence.
- Cleanup removes recoverable work or a live user's process.

The safe default is refusal with preserved recoverable state.
Uncertainty is never permission to seize, overwrite, merge, deploy, release, or delete.

## Documentation changes expected during implementation

The implementation agent should update canonical documentation only as each contract lands and is verified.

- Add the General's scoped Captain-autonomy decision and new dependency edges to `docs/PHASED-PRODUCTION-PLAN.md`.
- Update `docs/ORCHESTRATOR-OPERATING-MODEL.md` with the self-bootstrap and Project Builder boundaries.
- Update `docs/AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md` with grant issuance, subset delegation, and typed standing approval semantics.
- Update `docs/POWDER-INTEGRATION.md` with board and card operation availability only after the installed surfaces exist.
- Update `docs/cli-contract.md` only when public command behavior needs a compatible contract extension.
- Update `docs/WORKTREE-STATUS-CONTRACT.md` only through T18's reviewed ownership contract.
- Update `docs/REVIEW-INDEX.md` if this plan becomes a current supporting specification rather than a temporary integration packet.
- Do not edit generated changelogs.
- Do not rewrite historical handoffs to make them appear current.

## Definition of done for the integrated program

This program is complete only when all of the following are true.

1. The canonical phased plan contains the approved Captain-autonomy decision, ownership, dependencies, cards, and exit gates.
2. The actor-aware catalog and durable grant evaluator are the only authorization path for covered operations.
3. Appturnity's exact existing-repository scenario passes through a granted Captain without General or Cortana data entry.
4. Retry, response loss, dialog close, app restart, and WSL restart converge on one Project, Assignment, Captain, and board binding.
5. Captain card authoring passes P6 and T4 acceptance.
6. Dispatch records the requested and effective work profile and provider-native permission evidence.
7. Typed standing grants and one-time approvals survive restart, expire, revoke, consume, and explain correctly.
8. Captain delegation to Crew is a strict subset and cannot elevate the Crew's T-Hub capability.
9. Worktree and owned-resource cleanup uses T18 and preserves every dirty, unmerged, leased, claimed, stale, unknown, or foreign resource.
10. Routine non-protected delivery works only under an explicit delivery grant.
11. General-gated production, customer, spending, protected-branch, destructive, retirement, and authority-elevation operations remain denied without exact approval.
12. Backend, CLI, MCP, and applicable UI parity tests pass.
13. The complete role, retry, restart, concurrency, corruption, redaction, and packaged Windows E2E matrix passes.
14. Independent security review approves the grant evaluator and self-bootstrap control path.
15. Installed-runtime acceptance confirms the shipped behavior rather than relying only on source tests.

## Ordered next actions for the consuming agent

1. Read the current goal and all canonical files listed above.
2. Inspect the current branch, active T-Hub and Powder cards, and any in-flight Crew before editing planning state.
3. Treat this file as General-directed integration input, not permission to abandon the current goal.
4. Produce a delta table mapping every requirement here to an existing roadmap item, an acceptance-criteria expansion, or a genuinely new item.
5. Recommend the smallest dependency-correct first slice.
6. The recommended first slice is the scoped grant contract plus existing-repository self-bootstrap design, with the Appturnity denial as the required E2E reproduction.
7. Keep P5, P6, T4, T5, T6, T7, T9, T10, T11, T18, T23, and T33 ownership intact.
8. Ask the General only for decisions that materially change the scope or safety boundary.
9. Create Powder cards only when a slice is independently executable and its external dependencies are ready.
10. Require independent review for grant evaluation, identity, Project registration, Powder binding, protected delivery, and destructive resource code.
11. Commit each verified logical change separately with clear messages and no agent co-author.
12. Do not merge, push, install, deploy, publish, or release without the authority applicable to the consuming agent's current goal.

## Kickoff prompt for the consuming agent

Read `docs/CAPTAIN-AUTONOMY-AND-SCOPED-GRANTS-PLAN.md` after the canonical files in `docs/REVIEW-INDEX.md`.
Integrate its General-directed Captain-autonomy requirements into your existing goal without replacing that goal or duplicating P5, P6, T4, T5, T6, T7, T9, T10, T11, T18, T23, T26, T33, Phase 6, or Phase 7 work.
Start by producing an evidence-backed delta and dependency map.
Use the live Appturnity `register_project` ACL denial as the first E2E reproduction for the self-bootstrap slice.
Preserve cross-Project isolation, Crew read-capability defaults, credential boundaries, protected-branch and production gates, and every current rollback guarantee.
Follow `AGENTS.md`, `docs/cli-contract.md`, the repository's every-change commit policy, and all installed-runtime acceptance requirements.
