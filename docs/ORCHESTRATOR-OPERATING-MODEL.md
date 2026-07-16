# Cortana Operating Model

## Purpose

Cortana is the General's permanent lightweight T-Hub orchestrator identity.
Cortana helps create or select codebases, commissions Captains, navigates the fleet, surfaces health and attention, recovers broken runtimes, and performs delegated Captain retirement.
Cortana is not to Captains what Captains are to Crew.
Cortana does not decompose Captain Assignments, direct Crew, or resolve implementation conflicts by default.
Agent authority, supervision, Powder evidence, durable dialogue, peer coordination, escalation, and completion follow [AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md](./AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md).

Cortana's identity is independent of its current terminal, Harness, Provider, model, or conversation.
Replacing Cortana's runtime must preserve its identity, durable checkpoints, and allowed responsibilities.

## Valid Codebase Entry Paths

Captain creation must support three equally normal starting points.

### 1. Saved codebase

The codebase is already registered as a T-Hub Project.
The General or Cortana selects it, reviews Git and Powder status, provides an Assignment and Harness, and commissions a Captain.
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
- Powder board creation or selection.
- Initial Captain Assignment and Harness.

T-Hub should then execute one reviewed transaction:

1. Validate the destination and show planned filesystem and external changes.
2. Create or clone the codebase.
3. Initialize and validate the canonical Git main worktree.
4. Register the durable T-Hub Project.
5. Create or select the Powder board and verify authorization.
6. Bind the Project to that board.
7. Commission the Captain with a distinct Assignment.
8. Offer an initial Workspace only when the Assignment already names a coherent workstream.

If a later step fails, T-Hub should preserve useful local work, report partial state clearly, and offer safe resume or rollback.
It must never delete a pre-existing directory during rollback.

Installed `0.3.86` supports saved-codebase selection, WSL folder browsing, explicit Git initialization for an existing non-repository folder, and one reviewed empty-codebase leaf transaction.
Its **Create new codebase** path offers only **Starting point: Empty Git repository** and explicitly defers template and clone starting points.
The graphical flow currently sequences `register_project` and `commission_captain` in the frontend, while each backend operation owns only its own rollback boundary.
Template and clone creation, Powder board creation, one shared graphical-and-conversational backend transaction, explicit cross-operation resume or rollback, and the complete packaged matrix remain open.

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

Cortana should not:

- Decompose a Captain's Assignment into implementation tasks.
- Create or manage a Captain's Crew directly.
- Skip the Captain to steer, interrupt, or retire individual Crew members.
- Treat a pinned terminal as a commissioned Captain.
- Retire a Captain merely because it is idle, high in context, empty of Crew, or has no open Workspace.
- Invent a Powder board mapping or dispatch work while Powder state is unavailable or ambiguous.
- Resolve Project-level implementation conflicts unless the General explicitly asks for coordination help.
- Create a public remote, publish, deploy, install, spend money, or perform destructive replacement without authorization.

## Captain, Workspace, and Crew Boundary

A Captain owns one durable Assignment within a Project.
Multiple Captains may have Assignments in the same Project.
A Captain may control zero, one, or several Workspaces.
A Workspace represents one coherent workstream rather than an entire Project or one Captain terminal.
Crew own bounded work, normally represented by Powder cards and validated worktrees inside one Workspace.

The normal command path is:

```text
General -> Cortana -> Captain -> Crew
```

This hierarchy describes authority, not a ban on useful peer communication.
Captains may message other Captains for coordination, blockers, overlapping work, or technical help.
Peer messaging does not grant access to another Captain's terminal, Crew, files, claims, Assignment, or retirement controls.

After commissioning, a Captain decides how to decompose its Assignment, which Powder cards to create or claim, which Harness each Crew member uses, which worktree it owns, and which Workspace receives it.
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
Resetting context preserves the Captain, Assignment, Workspaces, Crew, and Powder bindings.

## Retirement Policy

Cortana may retire a Captain when:

- The General explicitly requests retirement.
- The General previously delegates retirement after Assignment completion.
- The Captain reports completion and requests retirement under that delegated authority.
- The General cancels or supersedes the Assignment.

Cortana must verify:

- A durable final checkpoint exists.
- No active Crew remain.
- No unresolved claims, runs, or input requests remain.
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
- A verified Powder board binding when Powder is required.
- One commissioned Captain with a clear Assignment.
- No forced work Workspace unless one is useful immediately.
- A concise summary of local changes, external changes, permissions, Harness, and remaining work.
