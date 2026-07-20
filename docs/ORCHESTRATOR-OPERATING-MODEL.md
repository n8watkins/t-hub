# Cortana Operating Model

## Purpose

Cortana is the General's permanent lightweight T-Hub orchestrator identity.
Cortana helps create or select codebases, commissions Captains, navigates the fleet, surfaces health and attention, recovers broken runtimes, and performs delegated Captain retirement.
Cortana is not to Captains what Captains are to Crew.
Cortana does not decompose Captain Assignments, direct Crew, or resolve implementation conflicts by default.
Agent authority, session evidence, durable dialogue, peer coordination,
escalation, and completion follow
[AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md](./AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md).

Cortana's identity is independent of its current terminal, Harness, Provider, model, or conversation.
Replacing Cortana's runtime must preserve its identity, durable checkpoints, and allowed responsibilities.

## Durable Runtime Reconciliation

T-Hub maintains exactly one durable Cortana identity with at most one authoritative active runtime.
Desktop startup calls one idempotent backend reconciliation operation rather than adopting whichever terminal happens to be visible.
Concurrent startup attempts use a stable operation identity and serialize against the same durable record.
Recovery preserves Cortana's identity and checkpoints while replacing a missing terminal or Harness runtime at a later generation.
When several candidates exist, reconciliation accepts only one deterministic authoritative generation and safely retires only older trusted duplicates.
Equal highest generations, foreign identities, uncertain liveness, and untrusted live candidates fail closed into a visible degraded recovery state.
The runtime governor reserves capacity for Cortana and recovery before admitting ordinary implementation lanes.
The installed runtime obtains provider capacity from a validated `T_HUB_PROVIDER_SESSION_CAPACITY` override when present or from the conservative packaged policy when the override is absent.
The packaged policy is reported as degraded because it is not live provider quota telemetry, while a malformed or unavailable configured override fails closed.
Capacity evidence reports provider limit, provider-consuming live sessions, evidence source, degraded state, and detail separately from total tmux session pressure.
Generic tmux terminals consume machine capacity but not provider capacity unless a Codex or Claude Harness is attested.
Provisioning takes dispatch admission before the Fleet provisioning lock whenever both are required.
Healthy no-spawn Cortana reconciliation inspects under provisioning alone and does not consume spawn admission or rate budget.

## Valid Codebase Entry Paths

Captain creation must support three equally normal starting points.

### 1. Saved codebase

The codebase is already registered as a T-Hub Project.
The General or Cortana selects it, reviews Git and session status, provides an
Assignment and Harness, and commissions a Captain.
More than one Captain may receive distinct Assignments in the same Project.

### 2. Existing codebase not yet saved

The folder or Git repository already exists in WSL but is not registered in T-Hub.
The General or Cortana browses to it, T-Hub detects the canonical main worktree and metadata, and the flow registers it before commissioning.

If the selected folder is not a Git repository, the flow must offer an explicit choice to initialize Git or cancel.
It must not silently initialize or rewrite version control.

### 3. Brand-new codebase

The codebase does not exist yet.
The General may create it through the graphical flow or ask Cortana to prepare it conversationally.

The healthy product flow collects:

- Project name and WSL parent folder.
- Starting source such as empty, template, or clone.
- Initial stack or template options when relevant.
- Git initialization and default branch.
- Whether to create or connect an external remote.
- Initial Captain Assignment and Harness.

T-Hub should then execute one reviewed transaction:

1. Validate the destination and show planned filesystem and external changes.
2. Create or clone the codebase.
3. Initialize and validate the canonical Git main worktree.
4. Register the durable T-Hub Project.
5. Commission the Captain with a distinct Assignment.
6. Offer an initial Workspace only when the Assignment already names a coherent workstream.

If a later step fails, T-Hub should preserve useful local work, report partial state clearly, and offer safe resume or rollback.
It must never delete a pre-existing directory during rollback.

Installed `0.3.86` supports saved-codebase selection, WSL folder browsing, explicit Git initialization for an existing non-repository folder, and one reviewed empty-codebase leaf transaction.
Its **Create new codebase** path offers only **Starting point: Empty Git repository** and explicitly defers template and clone starting points.
The graphical flow currently sequences `register_project` and
`commission_captain` in the frontend, while each backend operation owns only
its own rollback boundary.
Template and clone creation, one shared graphical-and-conversational backend
transaction, explicit cross-operation resume or rollback, and the complete
packaged matrix remain open.

## Healthy Cortana Responsibilities

Cortana should:

- Detect whether a request targets a saved codebase, existing unsaved codebase, or new codebase.
- Ask only for decisions that materially change the result.
- Present a preflight summary before durable or external changes.
- Use the same Project creation and commissioning operations as the graphical UI.
- Commission one or more Captains with clearly distinct Assignments.
- Let the General choose a Harness or use the configured default.
- Navigate or summon Captains.
- Show a lightweight fleet overview.
- Surface Captain health, needs-input states, context pressure, and local resource pressure.
- Recommend a checkpoint or context reset when evidence supports it.
- Recover a broken Captain runtime without replacing its durable identity.
- Retire a Captain when the General explicitly requests it or previously delegates retirement on completion.
- Verify retirement safety before removing runtime and Assignment state.
- Read one bounded authoritative summary directly when it is sufficient for a decision.
- Delegate multi-step investigation, terminal inspection, repository inspection, and administrative mutation to authorized agent sessions.
- Maintain one standing Fleet Admin by default when a live administrative agent and capacity are available.

Cortana should not:

- Decompose a Captain's Assignment into implementation tasks.
- Create or manage a Captain's Crew directly.
- Skip the Captain to steer, interrupt, or retire individual Crew members.
- Treat a pinned terminal as a commissioned Captain.
- Retire a Captain merely because it is idle, high in context, empty of Crew, or has no open Workspace.
- Invent task-board state or dispatch work outside the durable agent-session contract.
- Resolve Project-level implementation conflicts unless the General explicitly asks for coordination help.
- Create a public remote, publish, deploy, install, spend money, or perform destructive replacement without authorization.
- Perform routine multi-step investigation or administrative execution when a delegated administrator can carry it out within an exact scope.

## Delegated Administrative Roles

A Ship Admin is a durable agent-session role appointed by the owning Captain and retained until explicit revocation, supervisor retirement, or ownership invalidation.
The Ship Admin may inspect status and perform only the granted session, resource, retirement-preparation, or worktree-maintenance operations inside that exact ship.
A Ship Admin cannot appoint administrators, re-delegate authority, become Captain, cross ships, direct implementation, or exercise authority the Captain does not possess.
Each active Captain should maintain one standing Ship Admin by default when a suitable live agent and reserved capacity are available.

A Fleet Admin is a durable agent-session role appointed by Cortana and retained until explicit revocation, Cortana retirement, or Cortana ownership invalidation.
A Fleet Admin may inspect and administer Captains across the fleet only through its explicit operation set and Cortana's existing authority.
Fleet Admins support cross-Captain status, recovery, resource maintenance, and retirement preparation.
Multiple Fleet Admins may exist concurrently, but the default reservation is one standing Fleet Admin.
A Fleet Admin cannot direct implementation, acquire Captain authority, grant roles, control Captain-owned agents directly, or bypass General-reserved approval.

The outer control capability only permits a caller to reach control operations.
The durable delegated role, grant generation, delegating supervisor identity, exact ship or fleet scope, permitted operation set, current supervisor generation, and revocation state determine effective authority.
Every authorized operation is attributed to both the acting administrator and the delegating supervisor.
Revocation remains effective across reconnects and restarts.
Destructive session cleanup additionally requires one exact supervisor approval that binds the grant, actor, operation, and target and is consumed at most once.
Worktree removal and reuse remain unavailable until the unified worktree safety service supplies an authoritative mutation-time verdict.

## Captain, Workspace, and Crew Boundary

A Captain owns one durable Assignment within a Project.
Multiple Captains may have Assignments in the same Project.
A Captain may control zero, one, or several Workspaces.
A Workspace represents one coherent workstream rather than an entire Project or one Captain terminal.
Agent sessions own bounded work through their durable assignment, checkpoint,
runtime, and Git evidence inside one Workspace.

The normal command path is:

```text
General -> Cortana -> Captain -> Crew
```

This hierarchy describes authority, not a ban on useful peer communication.
Captains may message other Captains for coordination, blockers, overlapping work, or technical help.
Peer messaging does not grant access to another Captain's terminal, Crew, files, claims, Assignment, or retirement controls.

After commissioning, a Captain decides which agent sessions to start, the
assignment and Harness for each session, which worktree it uses, and which
Workspace receives it.
Cortana may surface conflicts or navigate to the relevant Captains, but it does not become their implementation manager.

## Context and Recovery

T-Hub should derive runtime liveness from the terminal, Harness process, provider lifecycle, and owned-resource state.
Captains should not need to spend model turns sending periodic liveness messages.

Cortana may receive event-driven notifications when:

- A Captain needs input.
- A Captain runtime fails.
- Context pressure crosses a threshold.
- An owned resource becomes orphaned.
- A delegated Assignment reports completion.
- Retirement safety checks require attention.

A context reset recommendation should follow a safe turn boundary and require a durable checkpoint.
Resetting context preserves the Captain, Assignment, Workspaces, and agent
session records.

## Retirement Policy

Cortana may retire a Captain when:

- The General explicitly requests retirement.
- The General previously delegates retirement after Assignment completion.
- The Captain reports completion and requests retirement under that delegated authority.
- The General cancels or supersedes the Assignment.

Cortana must verify:

- A durable final checkpoint exists.
- No active agent sessions remain.
- No unresolved input requests remain.
- No unsafe dirty, leased, or unmerged worktree remains.
- No owned browser or development-server process remains.
- No unread completion, blocker, or decision message remains.

A Captain cannot autonomously destroy its durable identity.
A broken terminal is a recovery event rather than evidence that the Assignment is complete.

## Expected User Experience

The main action should be **Create Captain**, followed by:

- **Use saved codebase**
- **Choose existing WSL folder**
- **Create new codebase**

When invoked through Cortana, the same flow should run conversationally and leave a visible preflight card for review.
The graphical and conversational paths must call the same backend operations.

Successful commissioning should leave the General with:

- An understandable saved codebase record.
- One commissioned Captain with a clear Assignment.
- No forced work Workspace unless one is useful immediately.
- A concise summary of local changes, external changes, permissions, Harness, and remaining work.
