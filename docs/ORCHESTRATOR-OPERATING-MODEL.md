# Orchestrator Operating Model

## Purpose

The orchestrator is the General's fleet-level coordinator.
It turns broad intent into projects and Captain assignments, watches the fleet, routes decisions, and keeps durable state coherent.
It is not the default implementation worker, a hidden administrator, or a substitute for a project Captain.

The orchestrator may prepare or register a project because project registration is an apex operation available to the General or Cortana.
Once a project has a commissioned Captain, normal implementation ownership moves to that Captain.

## Valid Project Entry Paths

Captain creation must support three equally normal starting points.

### 1. Saved codebase

The codebase is already registered in T-Hub.
The user or orchestrator selects it, reviews its Git and Powder status, provides an assignment and harness, and commissions the Captain.

### 2. Existing codebase not yet saved

The folder or Git repository already exists in WSL but is not registered in T-Hub.
The user or orchestrator browses to it, T-Hub detects the canonical Git main worktree and metadata, and the flow registers it before commissioning.

If the selected folder is not a Git repository, the flow must offer an explicit choice to initialize Git or cancel.
It must not silently initialize or rewrite version control.

### 3. Brand-new codebase

The codebase does not exist yet.
The user may create it directly or ask the orchestrator to prepare it.

The healthy product flow collects:

- Project name and WSL parent folder.
- Starting source: empty, template, or clone from an existing remote.
- Initial stack or template options when relevant.
- Git initialization and default branch.
- Whether to create or connect an external remote.
- Powder board creation or selection.
- Initial Captain assignment and Codex or Claude harness.

T-Hub should then execute one reviewed transaction:

1. Validate the destination and show the planned filesystem and external changes.
2. Create or clone the codebase.
3. Initialize and validate the canonical Git main worktree.
4. Register the durable T-Hub project.
5. Create or select the Powder board and verify authorization.
6. Bind the project to that board.
7. Commission the Captain in the reserved Captains workspace.
8. Create a deliberate project workspace for future Crew.

If a later step fails, T-Hub should preserve useful local work, clearly report partial state, and offer a safe resume or rollback.
It must never delete a pre-existing directory during rollback.

The current implementation does not provide this transaction.
`register_project` requires an existing Git main worktree, and the T-Hub Powder integration does not currently create Powder boards.

## Healthy Orchestrator Responsibilities

The orchestrator should:

- Translate the General's broad objective into one or more clearly named projects.
- Detect whether the request targets a saved codebase, an existing unsaved codebase, or a new codebase.
- Ask only for decisions that materially change the result, such as destination, template, remote visibility, destructive replacement, spending, or publication.
- Present a preflight summary before creating durable or external state.
- Use the same project creation and commissioning operations exposed in the graphical UI.
- Create concise Captain assignments with scope, constraints, success criteria, and escalation rules.
- Choose Codex or Claude deliberately rather than inheriting a harness accidentally.
- Commission no more than one live Captain for the same project under the current registry contract.
- Watch Captain status and Powder events, surface decisions, and recover fleet state after resets or restarts.
- Commission another Captain when work belongs to a separate project or exceeds one Captain's healthy span.

The orchestrator should not:

- Perform routine project implementation after a Captain is commissioned.
- Create or manage a Captain's Crew directly.
- Skip the Captain to steer or abort individual Crew members.
- Treat a pinned terminal as a commissioned Captain.
- Invent a Powder board mapping or dispatch work while Powder state is unavailable or ambiguous.
- Create a public remote, publish, deploy, install, spend money, or perform destructive replacement without explicit authorization.
- Auto-spawn itself with elevated or permissionless settings.

## Captain and Crew Boundary

The orchestrator owns the fleet of Captains.
A Captain owns one project assignment and its Crew.
Crew own bounded Powder cards and validated checkouts or worktrees.

The normal command path is:

```text
General -> Orchestrator -> Captain -> Crew
```

Status and decisions travel back up the same path.
The orchestrator may focus or wake a Captain, but it should not create skip-level control over that Captain's Crew.

After commissioning, the Captain decides how to decompose the assignment into Powder cards, which harness each Crew member uses, which validated worktree it owns, and which shared project workspace receives its tile.
The orchestrator remains responsible for cross-project priority, Captain health, conflicts between projects, and decisions that exceed a Captain's authority.

## Expected User Experience

The main action should be **Create Captain**, followed by a first choice:

- **Use saved codebase**
- **Choose existing WSL folder**
- **Create new codebase**

When invoked through Cortana, the orchestrator should drive the same flow conversationally and leave a visible preflight card for the General to review.
The graphical and conversational paths must call the same backend operations so they cannot drift in safety or behavior.

Successful completion should leave the user with:

- A registered project with understandable codebase metadata.
- A verified Powder board binding.
- One commissioned Captain visible in the Captains workspace.
- One named project workspace ready for Crew.
- A concise summary of what was created, what remains local, and what external state changed.
