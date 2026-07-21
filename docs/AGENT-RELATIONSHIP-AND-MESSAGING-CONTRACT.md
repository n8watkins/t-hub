# Agent Relationship and Messaging Contract

**Status:** Canonical.
**Scope:** General, Cortana, Captains, peer Captains, durable agent sessions, and bounded ephemeral subagents.
**Purpose:** Define authority, supervision, work evidence, durable dialogue, escalation, recovery, and completion without treating terminal text, model memory, or one subsystem as the whole truth.

## Precedence and Related Contracts

The [DEPOWDER-MIGRATION-PLAN.md](./DEPOWDER-MIGRATION-PLAN.md) is the active
post-Powder product decision for durable agent sessions.
The [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md) remains useful for
historical sequencing and evidence, but its Powder-specific sections are not
live product requirements.
This document defines the invariant relationship and messaging rules that the
roadmap, durable agent-session records, T-Hub control plane, CLI, MCP adapter,
and user interface must implement consistently.
Canonical precedence is scope-based rather than a single global ordering.
The phased plan wins for product decisions and dependencies, the handoff wins only for verified current runtime facts, the operating model wins for organizational lifecycle, and this contract wins for agent authority and messaging behavior.
Powder boards, cards, claims, runs, and Crew dispatch are historical compatibility concepts unless a newer contract explicitly reintroduces one.
No runtime fact or narrower contract may override an explicit product decision or General authorization.
When canonical scopes genuinely conflict, stop the affected action, record the conflict, and update the phased plan with the resolved decision.

The following subsystem contracts remain authoritative within their scopes:

- [ORCHESTRATOR-OPERATING-MODEL.md](./ORCHESTRATOR-OPERATING-MODEL.md) defines Cortana and organizational lifecycle.
- [DEPOWDER-MIGRATION-PLAN.md](./DEPOWDER-MIGRATION-PLAN.md) defines the active agent-session state ownership and migration boundary.
- [STATUS-MODEL.md](./STATUS-MODEL.md) defines work state and runtime health as separate axes.
- [cli-contract.md](./cli-contract.md) defines the CLI-first machine and human interface.
- [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md) defines worktree identity, ownership, and cleanup safety.

## Three-Layer Planning Model

T-Hub uses three distinct planning layers.
They must reference one another without duplicating authority.

### 1. Phased plan

The phased plan records product direction, settled decisions, dependencies, parallel lanes, test doctrine, and exit gates.
It is not a high-frequency task tracker.
Update it when a decision, dependency, phase status, critical risk, or exit gate materially changes.

### 2. Durable agent sessions

T-Hub is the durable supervisor for agent sessions, not a task manager.
Each independently started agent receives an explicit assignment, a Captain
relationship, a directory, and a durable session record.
The record contains runtime state, work stage, Git context when detected, and
bounded human-readable checkpoints.
Multiple parallel sessions require isolated worktrees, non-overlapping
ownership, and one declared integration owner.

An agent session record cannot override a product decision, authorization
boundary, security rule, or phase dependency from a canonical contract.
When newly discovered work changes roadmap sequencing or an exit gate, update
the active plan.
Routine implementation progress belongs in bounded session checkpoints rather
than in a task board.

### 3. Runtime communication and evidence

T-Hub provides durable identities, inbox delivery, lifecycle events, terminal bindings, and owned-resource state while work is active.
Git commits, diffs, tests, build artifacts, and packaged acceptance provide technical proof.
The current handoff records the verified resume point across all layers.

## Organizational Relationships

The normal authority path is:

```text
General -> Cortana -> Captain -> agent session
```

This path defines responsibility and control authority.
It does not prohibit direct, durable communication between permitted peers.

### General

The General is the final product, security, destructive-action, installation, release, publication, spending, and retirement authority.
The General may commission multiple Captains with distinct Assignments in one Project.
The General may delegate bounded routine decisions explicitly, but silence never grants new authority.

### Cortana

Cortana is the General's permanent lightweight operational coordinator.
Cortana commissions, locates, monitors, recovers, and retires Captains within the operating model.
Cortana does not decompose a Captain's Assignment, direct that Captain's agent
sessions, approve implementation details, or act as a Captain-of-Captains.
Cortana may surface conflicts and route messages without acquiring another identity's authority.

### Captain

A Captain owns one durable Assignment within one Project.
A Captain may control zero, one, or several coherent Workspaces.
A Captain decomposes its Assignment into agent-session assignments, selects
Harnesses, assigns worktrees and branches, establishes file ownership, defines
tests and exit gates, resolves dependencies, reviews evidence, and closes owned
work safely.
A Captain has control authority only over its own ship and agent sessions.
A Captain must remain at orchestration altitude and must not use continuous terminal watching as a substitute for structured evidence.
A Captain may read one bounded authoritative summary directly when that is sufficient to decide, prioritize, delegate, review, or escalate.
Multi-step investigations, terminal inspection, repository inspection, worktree maintenance, session cleanup, and other administrative execution belong to authorized agent sessions.

### Ship Admin

A Ship Admin is a durable agent-session role appointed by the owning Captain for one exact ship.
The role persists until revocation, supervisor retirement, or ownership invalidation and carries only an explicit permitted operation set.
A Ship Admin may execute granted status inspection, session maintenance, resource recovery, worktree maintenance, retirement preparation, and exactly approved cleanup inside that ship.
A Ship Admin cannot appoint administrators, re-delegate authority, become Captain, cross ships, direct implementation, or exercise authority the Captain does not possess.
The default operating posture is one standing Ship Admin per active Captain, with additional Ship Admins permitted for genuinely independent administrative work when capacity allows.

### Fleet Admin

A Fleet Admin is a durable agent-session role appointed by Cortana for fleet-level administration within Cortana's existing authority.
The role persists until revocation, Cortana retirement, or Cortana ownership invalidation and carries only an explicit permitted operation set.
A Fleet Admin may inspect Captains, build cross-Captain status reports, recover resources, maintain fleet resources, and prepare retirement when those operations are granted.
A Fleet Admin cannot direct implementation, control Captain-owned agents directly, acquire Captain authority, grant roles, or bypass General-reserved approval.
The default operating posture is one standing Fleet Admin, with more permitted for independent administrative work when capacity allows.

### Peer Captains

Captains are peers even when they work in the same Project.
They may communicate about dependencies, overlapping files, blockers, shared interfaces, integration order, or technical advice.
Peer messaging grants no authority over another Captain's Assignment, agent
session, terminal, worktree, resources, checkpoint, or retirement.
Materially transferred work requires an explicit Assignment change or agent
session ownership transfer.

### Agent sessions

Agent sessions are bounded leaf workers assigned to one Assignment, worktree,
branch, scope, and Captain.
Agents decide local implementation details and focused test strategy inside that
boundary.
Agent sessions do not create durable agent sessions or manage other agents.
Work that requires another orchestration layer receives another commissioned Captain rather than an informal Crew hierarchy.

### Bounded ephemeral subagents

A Captain or agent may use bounded ephemeral subagents only when active policy permits it.
Ephemeral subagents are appropriate for read-only research, mapping, or independent verification that does not require durable ownership.
They do not receive durable assignments, worktrees, or authority merely because
they were spawned by an authorized identity.

## Authority, Work Profile, and Runtime Resolution

T-Hub separates durable authority role, provider-neutral work profile, and resolved Harness runtime.
General, Cortana, Captain, agent, reviewer, and ephemeral-subagent authority
comes only from durable identity, the applicable Assignment, and control
capability.
Provider, Harness, model, reasoning effort, permission mode, latency, price, or context size never grants or removes authority.

The versioned work-profile catalog initially defines:

- `coordination` for fleet navigation, status, prioritization assistance, and Captain commissioning.
- `product_design` for requirements, user experience, product judgment, and architecture restraint.
- `technical_architecture` for demanding repository investigation, decomposition, integration, and cross-cutting technical decisions.
- `exploration` for read-heavy code tracing, dependency inspection, pattern discovery, and bounded research.
- `mechanical_execution` for explicit tests, documentation, types, CRUD, migrations supplied as plans, and repetitive transformations.
- `bounded_implementation` for clear multi-file work with stable interfaces and acceptance criteria.
- `judgment_implementation` for defined outcomes whose safe implementation path still requires substantial local judgment.
- `independent_review` for fresh, read-only verification of requirements, diffs, commits, tests, security, concurrency, migrations, or architecture.
- `frontier_escalation` for exceptional long-running, cross-project, or unusually difficult reasoning after ordinary routing is insufficient.

The initial Codex routing defaults are:

| Work profile | Default Codex runtime |
| --- | --- |
| `coordination` | `gpt-5.6-terra`, medium reasoning |
| `product_design` | `gpt-5.6-sol`, medium reasoning, with high only for a demonstrated difficult decision |
| `technical_architecture` | `gpt-5.6-sol`, medium reasoning, with high only for a demonstrated difficult decision |
| `exploration` | `gpt-5.6-terra`, medium reasoning, read-only by default |
| `mechanical_execution` | `gpt-5.6-luna`, medium reasoning |
| `bounded_implementation` | `gpt-5.6-luna`, high reasoning |
| `judgment_implementation` | `gpt-5.6-terra`, high reasoning |
| `independent_review` | `gpt-5.6-sol`, high reasoning, fresh read-only context |
| `frontier_escalation` | `gpt-5.6-sol` at the highest explicitly authorized supported reasoning level |

These mappings are defaults to evaluate against real T-Hub work, not permanent capability claims about a model family.
Claude and future Harness adapters may map the same profiles to different models without changing the profile meanings or durable identities.
Temporary, promotional, account-specific, preview, or uncertain model availability must not appear in the stable profile vocabulary.

Runtime resolution uses this precedence:

1. An explicit General decision for the exact commissioning or dispatch.
2. An authorized Captain override inside its Assignment boundary.
3. Assignment routing policy.
4. Project routing policy.
5. User-wide T-Hub routing policy.
6. The versioned built-in adapter default.

Before any process starts, T-Hub must show the requested profile, preferred provider policy, resolved provider, Harness, exact model, reasoning effort, effective local permission mode, fallback policy, and any degraded capability.
The Captain or agent binding must persist the profile-catalog version, requested
profile, resolution inputs, exact resolved runtime, fallback outcome, and runtime
replacement history.
Existing sessions remain pinned to their resolved runtime until an explicit replacement or recovery operation changes it.

An unavailable model must either fail visibly or use one bounded fallback that was displayed and authorized by policy.
T-Hub must not silently downgrade capability, increase cost class, change provider, weaken permissions, or substitute a conversation identity.
A cross-provider fallback starts a fresh runtime from a durable checkpoint and never treats a Claude conversation ID as a Codex thread ID or the reverse.

Agent sessions remain provider-neutral and require no model-routing changes.
T-Hub records assignment, runtime, work stage, checkpoints, and Git evidence,
while T-Hub owns profile selection, runtime resolution, identity, fallback, and
display.
Automatic classification from prompt text is deferred until explicit profile selection has produced enough T-Hub-specific evaluation evidence to justify it.

## Source-of-Truth Matrix

| Concern | Authoritative source | Not sufficient by itself |
| --- | --- | --- |
| Product decisions and dependencies | Phased plan and canonical contracts | Conversation memory or an isolated planning artifact |
| Agent assignment and progress | T-Hub agent-session record and bounded checkpoints | Terminal transcript or Captain self-report |
| Runtime identity and ownership | T-Hub Project, Assignment, Captain, Workspace, agent, terminal, and resource records | Folder name, tab location, or current working directory |
| Durable dialogue | T-Hub inbox message and acknowledgement state | Unacknowledged terminal typing |
| Agent work state | Structured Harness lifecycle observations under the status model | Output activity or silence |
| Runtime health | T-Hub terminal, process, Harness, and owned-resource evidence | Work stage or checkpoint text |
| Technical correctness | Git commits, diffs, tests, CI, review, builds, and packaged acceptance | Work log or completion message |
| Release identity | Reviewed source commit, build identity, installer hash, installed hash, and runtime evidence | Version string alone |

When authoritative sources disagree, T-Hub must preserve the disagreement and expose a degraded or decision-needed state.
It must not silently choose the most reassuring observation.

## Communication Channels

### Agent-session evidence

T-Hub records durable work facts rather than conversational traffic.
An agent checkpoint should record meaningful progress, blockers, test outcomes,
handoff context, and residual risk.
Checkpoints must remain concise, attributed, bounded, and safe to review later.
They must not contain secrets, credentials, hidden reasoning, private
chain-of-thought, or large raw logs.

Checkpoint history is not a task board, dependency graph, estimate, priority,
or completion authority.
Using checkpoints for every conversational exchange would create polling
latency, audit noise, poor acknowledgement semantics, and an unreadable
evidence history.

### T-Hub durable inbox

The T-Hub inbox carries direct dialogue that requires delivery, acknowledgement, response, or later human inspection.
Messages may represent instructions, status requiring attention, blockers, decisions, permissions, review findings, completion reports, lifecycle notices, or peer coordination.
Messages must target durable role identities and use terminal bindings only as delivery routes.

Each message must carry a stable message ID, sender, recipient, type, priority, creation time, and delivery state.
When applicable, it must also reference the Project, Assignment, Workspace, and
agent session.
Retryable delivery must use the stable message ID as an idempotency key.

The message lifecycle distinguishes enqueued, delivering, delivered, read, accepted, declined, completed, failed, retrying, expired, cancelled, and superseded states.
A successful socket write or terminal injection does not prove that the recipient read or accepted the message.

### Message state machine

`enqueued` is the initial durable state after validation and persistence.
The delivery service moves `enqueued` or `retrying` to `delivering`, then records `delivered` or `failed` with an attempt time and outcome.
The recipient moves `delivered` to `read`, then may move `read` to `accepted` or `declined` when the message requests a response.
The recipient moves `accepted` to `completed` only after the accepted instruction or coordination obligation is finished.
For an informational message whose schema requires no response, an authorized recipient acknowledgement moves `read` directly to `completed`.

`declined`, `completed`, `expired`, `cancelled`, and `superseded` are terminal states.
`failed` is nonterminal only when policy allows another bounded delivery attempt.
An authorized retry moves `failed` to `retrying` while preserving the same message ID and immutable payload.
When retry policy is exhausted, the delivery service moves `failed` to `expired` and creates attention for the sender or owning identity.
An authorized sender may cancel or supersede a nonterminal message, and a correction must use a new message ID that references the superseded message.

Every transition records the actor, timestamp, prior state, next state, and safe outcome metadata.
The message payload and authority references become immutable after `enqueued`.
An attempted transition by the wrong actor, from the wrong prior state, or after a terminal state fails without side effects.
Retention may remove an expired body later, but it must preserve enough non-secret metadata to explain delivery and acknowledgement history.

## Sender and Recipient Authorization

The following matrix defines the normal direct-message routes.
System-generated lifecycle notices are a separate trusted producer and never grant implementation authority.

| Sender | Recipient | Allowed message classes | Authority effect |
| --- | --- | --- | --- |
| General | Cortana, any Captain, or any agent | Instruction, decision, approval request response, clarification, stop, recovery, or emergency | Final authority, but scope and ownership changes must also update the Assignment or agent session |
| Cortana | General | Fleet status, health, recovery, retirement request, or decision request | No implementation authority |
| Cortana | Captain | Navigation, health, recovery, General relay, context, or retirement coordination | No agent direction unless relaying an exact General instruction |
| Cortana | Agent session | No free-form implementation route | Cortana must communicate through the owning Captain and cannot steer, interrupt, or retire an agent |
| Cortana | Fleet Admin | Exact administrative assignment, status request, recovery request, or revocation | Execution authority only inside the durable grant |
| Captain | General | Status, blocker, decision request, completion, risk, or authorization request | General may decide or delegate explicitly |
| Captain | Own agent sessions | Assignment delivery, follow-up instruction, review finding, decision, recovery, stop, or coordination | Bounded by the Captain's Assignment, ship, and delegated authority |
| Captain | Own Ship Admin | Exact administrative assignment, status request, maintenance request, cleanup approval, or revocation | Execution authority only inside the durable grant and exact ship |
| Captain | Peer Captain | Coordination, dependency, conflict, technical request, or status | No authority transfer or foreign agent control |
| Captain | Foreign agent | Denied | Route through the owning Captain |
| Agent | Owning Captain | Status requiring attention, blocker, decision request, permission request, review response, completion, or emergency | No authority expansion |
| Agent | General | Emergency or explicit General-requested dialogue, copied to the owning Captain when safe | No silent scope or ownership change |
| Agent | Same-Assignment agent | Dependency coordination or status linked to shared work | No instruction or control authority over the peer |
| Agent | Foreign agent or peer Captain | Denied by default | Route through the owning Captain unless the General authorizes a specific coordination route |
| Ship Admin | Owning Captain | Administrative result, evidence, blocker, or escalation | No implementation direction or authority expansion |
| Fleet Admin | Cortana | Fleet administrative result, evidence, blocker, or escalation | No implementation direction or Captain authority |
| T-Hub lifecycle service | Authorized owning identities | Needs-answer, needs-permission, failure, recovery, completion, context, or resource-risk notice | Attention only, never approval or implementation authority |

Every authorization decision uses durable sender and recipient identity, Project,
Assignment, ship, and agent-session bindings rather than terminal location or
prose claims.
A denied route must not fall back to raw terminal injection.

### Lifecycle events and attention

Harness and T-Hub lifecycle events communicate work-state and runtime-health transitions.
They wake the appropriate owner for needs-answer, needs-permission, failure, recovery, completion, context pressure, or resource-risk transitions.
They are not free-form messages and must not carry hidden message content.

Captains should be event-driven rather than spending model turns polling or sending periodic liveness requests.
Silence and terminal output activity remain non-authoritative for semantic work state.

### Git, tests, review, and packaged evidence

Git and verification artifacts prove what changed and whether it passed the required gate.
A completion report must identify exact commits, changed-file scope, tests, failed checks, residual risk, and proof artifacts.
A Captain must inspect this evidence before advancing an agent work stage.
Security-sensitive, destructive, control-plane, release, or broad shared-state changes require independent review.

### Terminal steering

Terminal typing is an interactive fallback rather than the durable communication system.
Use it only after inspection proves the target process is the expected interactive Harness or a complete safe resume command is required.
Never send prose to an idle headless Codex pane that has returned to a shell.

`send_text` acceptance does not prove composer submission, model receipt, or message acknowledgement.
Until exact submitted-message acknowledgement is implemented and packaged, the sender must verify the target state or use the durable inbox.
Terminal text must never be the only record of an Assignment, decision, blocker, approval, or completion.

## Message Classes and Required Behavior

| Message class | Primary channel | Required behavior |
| --- | --- | --- |
| Assignment | Typed inbox assignment plus durable agent-session record | Agent acknowledges receipt and exact ownership before mutation |
| Follow-up instruction | Inbox linked to the agent session | Agent acknowledges the instruction, and the Assignment changes too when scope changes |
| Scope or ownership change | Assignment change plus inbox | Captain revalidates dependencies, authority, worktree ownership, and acceptance before affected work continues |
| Routine progress | Agent checkpoint | No direct message unless attention or coordination is required |
| Blocker | Inbox plus agent checkpoint or lifecycle attention | Captain acknowledges and answers or escalates |
| Product or security decision | Inbox linked to the Assignment and agent session | Agent pauses only the affected path and continues safe unblocked work |
| Permission request | Lifecycle attention plus typed approval request | An authorized approval operation binds one decision to one exact pending operation |
| Review finding | Inbox linked to commit and agent session | Agent acknowledges, remediates, disputes with evidence, or requests a decision |
| Completion report | Agent checkpoint plus inbox notification | Captain verifies Git and tests before advancing the work stage |
| Peer-Captain coordination | Inbox linked to both Assignments and shared dependency | No authority transfer occurs without an explicit ownership operation |
| Runtime failure or recovery | Lifecycle event plus durable recovery state | Recover the identity rather than inferring retirement |

When an agent checkpoint or lifecycle event pauses an authoritative work path,
the authoritative answer must be recorded through the durable inbox or the
typed control operation.
An inbox response may explain or notify, but it does not silently change an
agent's Assignment or work stage.

### Durable Captain follow-up operation

`agent_followup` is the typed control operation for an active Captain to deliver a follow-up instruction to one existing owned agent session.
The request names one stable `requestId`, `captainSessionId`, `shipSlug`, `projectId`, `agentSessionId`, and non-empty message.
T-Hub authenticates the exact active owning Captain and verifies every named ownership binding before enqueueing.
Foreign, missing, stopped, or exited agent sessions are rejected without an inbox or Assignment mutation.
The durable inbox recipient is the `agentSessionId`, while any terminal binding remains only a later delivery route.
The inbox persists the request ID and a signature of the complete typed request semantics so an identical retry after restart returns the original sequence and a changed message, scope, identity, or Assignment is rejected.
When a follow-up replaces Assignment metadata, T-Hub persists the inbox record as held, commits the Assignment replacement, and only then activates the message for delivery.
A failure before activation must leave the instruction non-deliverable and allow the same authenticated request to converge on retry.
The optional `replacementAssignment` field is the only follow-up field that replaces durable agent Assignment metadata.
Omitting `replacementAssignment` means the follow-up is inside the existing scope and must leave Assignment metadata unchanged.
This operation must not fall back to `send_text`, terminal typing, or a foreign Captain route.

## Assignment Delivery and Acknowledgement

`start_agent` must create a typed assignment message inside the same durable,
recoverable launch transaction that validates the Project and Assignment,
creates the agent terminal, and persists the exact agent-session binding.
The assignment message records the agent identity, owning Captain, Project,
worktree, branch, Harness, scope digest, and request ID.
The launch prompt may transport the assignment into the Harness, but the durable inbox message remains the acknowledgement authority.

Before repository mutation, the agent acknowledges the exact assignment
message and records a start checkpoint against the same session.
An acknowledgement with mismatched agent, worktree, branch, or scope digest is
rejected and creates Captain attention.

If failure occurs before the session binding persists, start rolls back the
uncommitted terminal and assignment message.
If failure occurs after the binding persists but before acknowledgement, the
agent remains visibly `starting` or `awaiting-assignment-ack`, and T-Hub recovers
or rolls back from the same request ID rather than creating a second session.
A declined or expired Assignment stops affected work and preserves the session
record for recovery.
After restart, T-Hub reconstructs the pending acknowledgement from the durable binding and message state before accepting new work.

## Typed Permission Decisions

An inbox body alone cannot approve a Harness permission, T-Hub control mutation, destructive action, external effect, installation, publication, release, or spending decision.
A typed approval request identifies one stable approval ID, authority domain,
requesting identity, owning identity, exact operation, target, arguments digest,
requested scope, expiry, and associated Assignment and agent session.
Only an actor authorized for that domain may call the approval decision operation.

Approval state distinguishes pending, approved, denied, expired, cancelled, and consumed.
The exact pending operation consumes an approval at most once and rejects changed arguments or targets.
A Captain may approve routine local development actions for its own agent
sessions only when repository policy or the General has delegated that class.
A Captain cannot use an approval to elevate an agent's T-Hub capability,
control foreign resources, or authorize an external action reserved to the
General.
Agents cannot approve their own request.

## Captain Responsibilities Over Agent Sessions

Before dispatch, the Captain must:

1. Establish and record one exact clean source commit for every agent assignment.
2. Preserve unrelated dirty user work outside the dispatch baseline.
3. Commit shared interfaces before dependent lanes begin.
4. Confirm the Assignment, Project, dependencies, and authorization are unambiguous.
5. Define bounded scope, explicit file ownership, expected branch and worktree, required tests, commit policy, escalation rules, and exit gate.
6. Declare one stable lane identity, its explicit dependency set, its mutable file, schema, and interface claims, and any required integration ordering contract.
7. Choose the Harness and effective local execution permission deliberately.
8. Run adaptive dispatch preflight and preserve capacity for Cortana, standing administrators, and recovery.
9. Start through the sanctioned transaction and verify the agent session, terminal, Harness, worktree, branch, source commit, prompt, and recorded capacity decision.

During work, the Captain must:

1. Monitor agent checkpoints, inbox attention, lifecycle status, runtime health, and owned resources as distinct signals.
2. Respond to blockers and decisions promptly through durable messages.
3. Coordinate dependencies and overlapping interfaces without taking over the agent's implementation loop.
4. Renew or recover work only from authoritative liveness and binding evidence.
5. Continue supervising other unblocked lanes while one lane awaits a decision.
6. Keep a durable checkpoint current when the roster, blocker, decision, or next ordered action materially changes.

Before completion, the Captain must:

1. Verify the exact branch, worktree, commit, changed-file scope, test results, review findings, and residual risk.
2. Require remediation or an explicit General decision for unresolved findings at the applicable severity.
3. Record or verify final Git and test evidence against the exact agent session.
4. Advance the work stage only after the acceptance criteria pass.
5. Close the agent session and release owned resources only after landed-work and recovery checks pass.
6. Update the checkpoint and handoff before context replacement or retirement.

Completion and release reporting must use these separate states:

- `implemented` means code exists at an exact commit.
- `reviewed` means an independent reviewer approved that exact commit.
- `tested` means the required acceptance checks passed on that exact commit.
- `complete` means the stated scope is both independently reviewed and acceptance-tested on the same exact result commit.
- `integrated` means the complete result is present in the named canonical baseline.
- `packaged` means an artifact was built from that baseline.
- `installed` means that artifact replaced the intended installation.
- `live-verified` means the required flow passed against the installed application through a human or AI-agent verifier.

Visible product bugs require packaged graphical end-to-end evidence before `tested` may satisfy `complete`.
Integration must record the dispatch baseline and the exact agent commits that produced the canonical result.

## Agent Responsibilities

Agents must:

1. Work only inside the assigned Assignment, worktree, branch, files, and authorization boundary.
2. Verify the start identity and report a mismatch before mutation.
3. Record start and meaningful progress in bounded checkpoints.
4. Send blockers, decisions, and permission needs through the durable inbox when a response is required.
5. Continue safe unblocked work while an escalation is pending.
6. Commit each verified logical change clearly without merging, pushing, installing, publishing, or expanding scope unless authorized.
7. Report exact tests, failed checks, residual risk, and final commit hashes honestly.
8. Request review and stage advancement rather than treating model-turn completion as work completion.
9. Preserve protected files, unrelated changes, credentials, and other Captains' resources.
10. Stop the affected path and escalate when scope, credentials, authority, product intent, security posture, destructive impact, or outward-facing impact is ambiguous.

## Permission and Authority Separation

Local execution permission and T-Hub control-plane capability are separate.
A session may run its coding Harness with unrestricted local repository execution while retaining a read-capability T-Hub token that cannot spawn, type into, close, or control foreign terminals.
A Captain may hold T-Hub control capability for its own ship without gaining authority over peer Captains.

`BypassPermissions` is the explicitly authorized default local execution mode for started Codex and Claude agent sessions in this Captain fleet.
This default is intentional and grants the Harness full local execution authority inside the agent's assigned worktree without provider approval prompts.
It does not expand the agent's file, worktree, branch, or product scope.
Agents remain responsible for testing, bounded checkpoints, separate verified commits, and honest reporting of commits, checks, failures, and residual risks.
Agents must not merge, push, install, deploy, publish, release, or make product, security, destructive, spending, or outward-facing decisions unless the applicable Captain or General authority explicitly authorizes that exact action.

Every agent launch must use the selected provider's native bypass flag, attest the authoritative foreground provider process after launch, and persist and return the verified effective Harness permission mode.
T-Hub must fail closed and transactionally roll back the terminal and agent binding when permission evidence is missing, stale, conflicting, wrong-provider, wrapper-obscured, unreadable, or changes before durable launch acceptance.
When structured provider telemetry is unavailable or the permission posture cannot remain observed, T-Hub must expose a degraded or unknown state rather than implying that the agent is safely working.
An observed permission posture change after launch must fail closed or degrade visibly until authoritative provider-native evidence re-establishes the expected posture.

A local Codex or Claude approval prompt does not by itself indicate a missing T-Hub role permission.
T-Hub must display both the effective Harness permission mode and the T-Hub control capability clearly.
No message, checkpoint, terminal location, or inherited environment variable may silently elevate either authority.
Harness bypass, T-Hub control capability, Assignment scope, and General authority are independent authority axes.
Harness bypass is not T-Hub control authority and cannot mutate another session's durable record.

## End-to-End Work Lifecycle

1. The active plan or an authorized product decision establishes the work and dependencies.
2. A Captain starts one or more agent sessions with explicit assignments and proof plans.
3. Each agent receives an isolated worktree when needed and records the start binding.
4. Each agent acknowledges the typed assignment message and records a start checkpoint.
5. Agents record meaningful milestones in bounded session checkpoints without generating conversational noise.
6. Blockers and decisions travel through the durable inbox and remain linked to the Assignment and agent session.
7. Lifecycle events wake the Captain when attention is required.
8. Agents submit final evidence with commits, tests, failed checks, and residual risk.
9. The Captain reviews the actual artifacts and requests corrections through the inbox when necessary.
10. The Captain advances the agent work stage only after acceptance passes.
11. The Captain lands or preserves the work under the applicable authorization, then closes the agent session and releases resources safely.
12. Material roadmap changes update the phased plan, and the current runtime state updates the handoff.

## Multiple Captains and Integration Ownership

Multiple Captains may hold distinct Assignments in the same Project.
Each Assignment must have explicit scope and agent-session ownership.
Shared interfaces require a declared integration order and one integration owner.
Two Captains must not edit the same registry schema, migration, lifecycle core, or version files concurrently without an agreed boundary.

There is no fixed three-agent, four-agent, or other policy cap on independent implementation lanes.
Every genuinely independent lane with explicit ownership and dependencies may proceed when the runtime governor, machine health, Provider limits, worktree availability, and integration-collision checks admit it.
Parallel lanes that share mutable files, schemas, or interfaces require one declared integration owner and an explicit ordering contract.
The governor must retain capacity for Cortana, standing administrators, and recovery rather than filling every available session slot with implementation work.

When overlap is discovered, the Captains exchange a coordination message that identifies the files, interface, dependency, recommended owner, and impact.
They continue non-overlapping work while the ownership decision is pending.
If agreement would materially change either Assignment, the General decides or authorizes a formal transfer.

## Failure and Recovery Rules

Messages, agent-session events, start transactions, approvals, and retryable mutations must carry stable idempotency identities.
After an ambiguous timeout, reread authoritative state before retrying.
Never blindly repost a non-idempotent checkpoint or session mutation.
Agent-session mutations require the expected current session and request identity
so a delayed operation cannot update a replacement session.

After T-Hub, WSL, terminal, or Harness restart, recover durable identity and bindings before sending or accepting new work.
Queued messages must survive terminal replacement.
An unknown resume identity blocks terminal steering but does not erase the inbox message, session history, or durable Captain or agent identity.

A runtime failure triggers recovery rather than retirement.
A failed message delivery remains visible and retryable.
An unavailable external service does not block ordinary agent-session supervision
and never authorizes a local emulation of another subsystem's authority.

## Security, Privacy, and Retention

Message bodies, checkpoints, proof fields, and audit records must be bounded and scrubbed for known credential shapes.
Machine surfaces must never expose protected credentials, unrestricted endpoints, or arbitrary identity substitution fields when durable bindings already supply authority.
Audit records should preserve actor, operation, target, outcome, and safe hashes or summaries rather than sensitive message bodies or proof URLs.

Use thirty days as the provisional recommended default for local inbox message bodies until the General resolves the retention setting.
Keep non-secret delivery metadata longer for recovery and audit.
Keep user-pinned messages until explicitly removed.
Historical provider data remains governed by its original retention policy and is
not duplicated into the active T-Hub agent-session model.

## CLI, MCP, and UI Contract

The compact JSON `th` CLI is the canonical agent and automation interface.
MCP is a role-filtered thin adapter over the same backend operations and schemas.
The graphical UI consumes the same authoritative state and must not define separate communication semantics.

CLI and MCP must provide equivalent authorized operations for send, list, read,
reply, acknowledge, accept, decline, checkpoint, retry, cancel, lifecycle
attention, typed approval request, approval decision, approval cancellation, and
approval status.
JSON output must remain bounded, deterministic, parseable, and free of diagnostics on stdout.
Unknown inputs and forbidden identity substitutions must fail before side effects.

## Activation Tests and Exit Gate

Before this contract is considered implemented, packaged tests must prove:

- Assignment dispatch and acknowledgement survive application and terminal restarts.
- Agent checkpoints remain tied to the exact durable session binding.
- Routine progress does not require terminal polling or direct messages.
- Blocker, decision, permission, review, completion, and peer-coordination messages reach only authorized durable recipients.
- Message retries are idempotent across timeout, crash, reconnect, and duplicate event delivery.
- `send_text` cannot be mistaken for message acknowledgement or work-stage advancement.
- Work stage, runtime health, and technical proof remain separate when they conflict.
- A Captain cannot mutate a peer Captain or foreign agent through messaging.
- Local unrestricted Harness execution does not grant T-Hub control-plane capability.
- Completion cannot release, renew, or delete stale agent state after the completion operation begins.
- A delayed session A operation cannot append evidence to or clean up replacement session B.
- The General can inspect message content, delivery lifecycle, session checkpoints, technical proof, and cleanup outcome without relying on terminal scrollback.
- CLI, MCP, and UI return equivalent results for the same authorized operation.

The contract exits only when one real installed Codex agent session and one real
installed Claude agent session each complete the full lifecycle through durable
dialogue, Captain review, completion proof, and safe resource cleanup.

## Current Implementation Boundary

The relationship, authority, channel, evidence, and recovery decisions in this document are settled.
The exact message-body retention duration remains provisional.
Implementation remains incomplete until the phased plan exit gates pass.
The durable inbox substrate exists, but generic send, receive, acknowledgement, message history, and frontend visibility remain open.
Interactive Codex lifecycle parity remains incomplete.
Terminal steering remains a compatibility path and has demonstrated cases where accepted text remained unsubmitted in the interactive composer.
Agent-session listing, checkpoint, event, and recovery reads are the active T-Hub
implementation boundary.
Historical Powder data remains readable only through compatibility migration and
is not part of active authorization, health, or completion workflows.
