# Agent Relationship and Messaging Contract

**Status:** Canonical.
**Scope:** General, Cortana, Captains, peer Captains, Crew, and bounded ephemeral subagents.
**Purpose:** Define authority, supervision, work evidence, durable dialogue, escalation, recovery, and completion without treating terminal text, model memory, or one subsystem as the whole truth.

## Precedence and Related Contracts

The [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md) remains authoritative for product decisions, sequencing, dependencies, testing doctrine, and phase exit gates.
Powder is the executable backlog and work ledger for authorized implementation.
The [CAPTAIN-POWDER-HANDOFF.md](./CAPTAIN-POWDER-HANDOFF.md) records verified runtime evidence and the current zero-context resume point.
This document defines the invariant relationship and messaging rules that the roadmap, Powder cards, T-Hub control plane, CLI, MCP adapter, and user interface must implement consistently.
Canonical precedence is scope-based rather than a single global ordering.
The phased plan wins for product decisions and dependencies, the handoff wins only for verified current runtime facts, the operating model wins for organizational lifecycle, and this contract wins for agent authority and messaging behavior.
No runtime fact or narrower contract may override an explicit product decision or General authorization.
When canonical scopes genuinely conflict, stop the affected action, record the conflict, and update the phased plan with the resolved decision.

The following subsystem contracts remain authoritative within their scopes:

- [ORCHESTRATOR-OPERATING-MODEL.md](./ORCHESTRATOR-OPERATING-MODEL.md) defines Cortana and organizational lifecycle.
- [POWDER-INTEGRATION.md](./POWDER-INTEGRATION.md) defines Powder and T-Hub state ownership.
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

### 2. Powder backlog

Powder is the executable backlog.
Each implementation unit that can be independently assigned must have an authoritative Powder card before Crew dispatch.
The card records scope, acceptance criteria, dependencies, ownership, claim, run, work logs, input requests, proof, and completion state.
Multiple parallel lanes require separate cards, isolated worktrees, non-overlapping ownership, and one declared integration owner.

A Powder card cannot override a product decision, authorization boundary, security rule, or phase dependency from a canonical contract.
When newly discovered work changes roadmap sequencing or an exit gate, update the phased plan as well as the Powder backlog.
Routine implementation progress belongs in Powder rather than the phased plan.

### 3. Runtime communication and evidence

T-Hub provides durable identities, inbox delivery, lifecycle events, terminal bindings, and owned-resource state while work is active.
Git commits, diffs, tests, build artifacts, and packaged acceptance provide technical proof.
The current handoff records the verified resume point across all layers.

## Organizational Relationships

The normal authority path is:

```text
General -> Cortana -> Captain -> Crew
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
Cortana does not decompose a Captain's Assignment, direct that Captain's Crew, approve implementation details, or act as a Captain-of-Captains.
Cortana may surface conflicts and route messages without acquiring another identity's authority.

### Captain

A Captain owns one durable Assignment within one Project.
A Captain may control zero, one, or several coherent Workspaces.
A Captain decomposes its Assignment into Powder cards, selects Crew Harnesses, assigns worktrees and branches, establishes file ownership, defines tests and exit gates, resolves dependencies, reviews evidence, and closes owned work safely.
A Captain has control authority only over its own ship and Crew.
A Captain must remain at orchestration altitude and must not use continuous terminal watching as a substitute for structured evidence.

### Peer Captains

Captains are peers even when they work in the same Project.
They may communicate about dependencies, overlapping files, blockers, shared interfaces, integration order, or technical advice.
Peer messaging grants no authority over another Captain's Assignment, Crew, terminal, worktree, Powder claim, resources, checkpoint, or retirement.
Materially transferred work requires an explicit Assignment change or Powder card ownership transfer.

### Crew

Crew are bounded leaf workers assigned to one card, run, worktree, branch, scope, and Captain.
Crew decide local implementation details and focused test strategy inside that boundary.
Crew do not create durable Crew or manage other Crew.
Work that requires another orchestration layer receives another commissioned Captain rather than an informal Crew hierarchy.

### Bounded ephemeral subagents

A Captain or Crew may use bounded ephemeral subagents only when active policy permits it.
Ephemeral subagents are appropriate for read-only research, mapping, or independent verification that does not require durable ownership.
They do not receive durable Powder claims, worktrees, or authority merely because they were spawned by an authorized identity.

## Authority, Work Profile, and Runtime Resolution

T-Hub separates durable authority role, provider-neutral work profile, and resolved Harness runtime.
General, Cortana, Captain, Crew, reviewer, and ephemeral-subagent authority comes only from durable identity and the applicable Assignment, card, run, and control capability.
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
2. An authorized Captain override inside its Assignment and card boundary.
3. Assignment routing policy.
4. Project routing policy.
5. User-wide T-Hub routing policy.
6. The versioned built-in adapter default.

Before any process starts, T-Hub must show the requested profile, preferred provider policy, resolved provider, Harness, exact model, reasoning effort, effective local permission mode, fallback policy, and any degraded capability.
The Captain or Crew binding must persist the profile-catalog version, requested profile, resolution inputs, exact resolved runtime, fallback outcome, and runtime replacement history.
Existing sessions remain pinned to their resolved runtime until an explicit replacement or recovery operation changes it.

An unavailable model must either fail visibly or use one bounded fallback that was displayed and authorized by policy.
T-Hub must not silently downgrade capability, increase cost class, change provider, weaken permissions, or substitute a conversation identity.
A cross-provider fallback starts a fresh runtime from a durable checkpoint and never treats a Claude conversation ID as a Codex thread ID or the reverse.

Powder remains provider-neutral and requires no model-routing changes.
Powder records scope, acceptance, card and run state, claims, evidence, and completion, while T-Hub owns profile selection, runtime resolution, dispatch, identity, fallback, and display.
Automatic classification from prompt text is deferred until explicit profile selection has produced enough T-Hub-specific evaluation evidence to justify it.

## Source-of-Truth Matrix

| Concern | Authoritative source | Not sufficient by itself |
| --- | --- | --- |
| Product decisions and dependencies | Phased plan and canonical contracts | Conversation memory or an isolated card |
| Executable backlog and card/run execution state | Powder card, claim, run, work log, input request, and completion record | Terminal transcript or Captain self-report |
| Runtime identity and ownership | T-Hub Project, Assignment, Captain, Workspace, Crew, terminal, and resource records | Folder name, tab location, or current working directory |
| Durable dialogue | T-Hub inbox message and acknowledgement state | Unacknowledged terminal typing |
| Agent work state | Structured Harness lifecycle observations under the status model | Output activity or silence |
| Runtime health | T-Hub terminal, process, Harness, and owned-resource evidence | Powder card status |
| Technical correctness | Git commits, diffs, tests, CI, review, builds, and packaged acceptance | Work log or completion message |
| Release identity | Reviewed source commit, build identity, installer hash, installed hash, and runtime evidence | Version string alone |

When authoritative sources disagree, T-Hub must preserve the disagreement and expose a degraded or decision-needed state.
It must not silently choose the most reassuring observation.

## Communication Channels

### Powder work ledger

Powder records durable work facts rather than conversational traffic.
A Crew work log should record start, meaningful milestones, blockers, test outcomes, final evidence, and residual risk.
Work logs must remain concise, attributed, linked to the exact card and run, bounded, and safe to review later.
They must not contain secrets, credentials, hidden reasoning, private chain-of-thought, or large raw logs.

Powder comments may hold low-frequency human-facing context.
Powder input requests represent a durable decision boundary that pauses or blocks authoritative work.
Powder completion represents verified card and run execution state, not merely the end of a model turn.

Powder must not become the general chat transport.
Using work logs for every conversational exchange would create polling latency, audit noise, poor acknowledgement semantics, and an unreadable evidence history.

### T-Hub durable inbox

The T-Hub inbox carries direct dialogue that requires delivery, acknowledgement, response, or later human inspection.
Messages may represent instructions, status requiring attention, blockers, decisions, permissions, review findings, completion reports, lifecycle notices, or peer coordination.
Messages must target durable role identities and use terminal bindings only as delivery routes.

Each message must carry a stable message ID, sender, recipient, type, priority, creation time, and delivery state.
When applicable, it must also reference the Project, Assignment, Workspace, Crew, Powder card, and Powder run.
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
| General | Cortana, any Captain, or any Crew | Instruction, decision, approval request response, clarification, stop, recovery, or emergency | Final authority, but scope and ownership changes must also update the Assignment or Powder card |
| Cortana | General | Fleet status, health, recovery, retirement request, or decision request | No implementation authority |
| Cortana | Captain | Navigation, health, recovery, General relay, context, or retirement coordination | No Crew direction unless relaying an exact General instruction |
| Cortana | Crew | No free-form implementation route | Cortana must communicate through the owning Captain and cannot steer, interrupt, or retire Crew |
| Captain | General | Status, blocker, decision request, completion, risk, or authorization request | General may decide or delegate explicitly |
| Captain | Own Crew | Assignment delivery, follow-up instruction, review finding, decision, recovery, stop, or coordination | Bounded by the Captain's Assignment, card, ship, and delegated authority |
| Captain | Peer Captain | Coordination, dependency, conflict, technical request, or status | No authority transfer or foreign Crew control |
| Captain | Foreign Crew | Denied | Route through the owning Captain |
| Crew | Owning Captain | Status requiring attention, blocker, decision request, permission request, review response, completion, or emergency | No authority expansion |
| Crew | General | Emergency or explicit General-requested dialogue, copied to the owning Captain when safe | No silent scope or ownership change |
| Crew | Same-ship Crew | Dependency coordination or status linked to shared work | No instruction, claim, completion, or control authority over the peer |
| Crew | Foreign Crew or peer Captain | Denied by default | Route through the owning Captain unless the General authorizes a specific coordination route |
| T-Hub lifecycle service | Authorized owning identities | Needs-answer, needs-permission, failure, recovery, completion, context, or resource-risk notice | Attention only, never approval or implementation authority |

Every authorization decision uses durable sender and recipient identity, Project, Assignment, ship, card, and run bindings rather than terminal location or prose claims.
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
A Captain must inspect this evidence before accepting a card as complete.
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
| Assignment | Typed inbox assignment created by dispatch plus Powder card | Crew acknowledges receipt and exact ownership before mutation |
| Follow-up instruction | Inbox linked to card and run | Crew acknowledges the instruction, and the card changes too when scope or acceptance changes |
| Scope or ownership change | Powder card update or transfer plus inbox | Captain revalidates dependencies, authority, worktree ownership, and acceptance before affected work continues |
| Routine progress | Powder work log | No direct message unless attention or coordination is required |
| Blocker | Inbox plus Powder input request or blocked evidence | Captain acknowledges and answers or escalates |
| Product or security decision | Inbox linked to card and run | Crew pauses only the affected path and continues safe unblocked work |
| Permission request | Lifecycle attention plus typed approval request | An authorized approval operation binds one decision to one exact pending operation |
| Review finding | Inbox linked to commit and card | Crew acknowledges, remediates, disputes with evidence, or requests a decision |
| Completion report | Powder final evidence plus inbox notification | Captain verifies Git and tests before Powder completion |
| Peer-Captain coordination | Inbox linked to both Assignments and shared dependency | No authority transfer occurs without an explicit ownership operation |
| Runtime failure or recovery | Lifecycle event plus durable recovery state | Recover the identity rather than inferring retirement |

When a Powder input request pauses an authoritative run, the authoritative answer must be recorded through Powder.
An inbox response may explain or notify, but it does not silently resume the Powder run.

## Assignment Delivery and Acknowledgement

`dispatch_crew` must create a typed assignment message inside the same durable, recoverable dispatch transaction that validates the Project and card, creates the Crew terminal, claims the Powder card, and persists the exact card/run/Crew binding.
The assignment message records the Crew identity, owning Captain, card, run, worktree, branch, Harness, scope digest, acceptance digest, and dispatch request ID.
The launch prompt may transport the assignment into the Harness, but the durable inbox message remains the acknowledgement authority.

Before repository mutation, the Crew acknowledges the exact assignment message and records the start work log against the same run.
An acknowledgement with mismatched card, run, Crew, worktree, branch, or scope digest is rejected and creates Captain attention.

If failure occurs before the claim and binding persist, dispatch rolls back the uncommitted terminal and assignment message.
If failure occurs after the binding persists but before acknowledgement, the Crew remains visibly `starting` or `awaiting-assignment-ack`, and T-Hub recovers or rolls back from the same dispatch request ID rather than creating a second claim or message.
A declined or expired assignment stops affected work and enters the normal claim-release and resource-preservation path.
After restart, T-Hub reconstructs the pending acknowledgement from the durable binding and message state before accepting new work.

## Typed Permission Decisions

An inbox body alone cannot approve a Harness permission, T-Hub control mutation, destructive action, external effect, installation, publication, release, or spending decision.
A typed approval request identifies one stable approval ID, authority domain, requesting identity, owning identity, exact operation, target, arguments digest, requested scope, expiry, and associated card and run.
Only an actor authorized for that domain may call the approval decision operation.

Approval state distinguishes pending, approved, denied, expired, cancelled, and consumed.
The exact pending operation consumes an approval at most once and rejects changed arguments or targets.
A Captain may approve routine local development actions for its own Crew only when repository policy or the General has delegated that class.
A Captain cannot use an approval to elevate a Crew's T-Hub capability, control foreign resources, or authorize an external action reserved to the General.
Crew cannot approve their own request.

## Captain Responsibilities Over Crew

Before dispatch, the Captain must:

1. Confirm the Assignment, Project, Powder binding, card, dependencies, and authorization are unambiguous.
2. Define bounded scope, explicit file ownership, expected branch and worktree, required tests, commit policy, escalation rules, and exit gate.
3. Choose the Harness and effective local execution permission deliberately.
4. Preserve least-privilege T-Hub control authority while allowing the repository's configured local development permission.
5. Dispatch through the sanctioned transaction and verify the card, run, terminal, Harness, worktree, branch, and prompt.

During work, the Captain must:

1. Monitor Powder work evidence, inbox attention, lifecycle status, runtime health, claims, and owned resources as distinct signals.
2. Respond to blockers and decisions promptly through durable messages.
3. Coordinate dependencies and overlapping interfaces without taking over the Crew's implementation loop.
4. Renew or recover work only from authoritative liveness and binding evidence.
5. Continue supervising other unblocked lanes while one lane awaits a decision.
6. Keep a durable checkpoint current when the roster, blocker, decision, or next ordered action materially changes.

Before completion, the Captain must:

1. Verify the exact branch, worktree, commit, changed-file scope, test results, review findings, and residual risk.
2. Require remediation or an explicit General decision for unresolved findings at the applicable severity.
3. Record or verify final Powder evidence against the exact Crew run.
4. Complete the card only after the acceptance criteria pass.
5. Close the Crew and release owned resources only after landed-work and recovery checks pass.
6. Update the checkpoint and handoff before context replacement or retirement.

## Crew Responsibilities

Crew must:

1. Work only inside the assigned card, run, worktree, branch, files, and authorization boundary.
2. Verify the dispatch identity and report a mismatch before mutation.
3. Record start and meaningful progress in the exact Powder run.
4. Send blockers, decisions, and permission needs through the durable inbox when a response is required.
5. Continue safe unblocked work while an escalation is pending.
6. Commit each verified logical change clearly without merging, pushing, installing, publishing, or expanding scope unless authorized.
7. Report exact tests, failed checks, residual risk, and final commit hashes honestly.
8. Request review and completion rather than treating model-turn completion as card completion.
9. Preserve protected files, unrelated changes, credentials, and other Captains' resources.

## Permission and Authority Separation

Local execution permission and T-Hub control-plane capability are separate.
A Crew may run its coding Harness with unrestricted local repository execution while retaining a read-capability T-Hub token that cannot spawn, type into, close, or control foreign terminals.
A Captain may hold T-Hub control capability for its own ship without gaining authority over peer Captains.

A local Codex or Claude approval prompt does not by itself indicate a missing T-Hub role permission.
T-Hub must display both the effective Harness permission mode and the T-Hub control capability clearly.
No message, Powder card, work log, terminal location, or inherited environment variable may silently elevate either authority.

## End-to-End Work Lifecycle

1. The phased plan or an authorized product decision establishes the work and dependencies.
2. Powder receives one or more executable cards with acceptance criteria and proof plans.
3. A Captain assigns separate cards to isolated Crew worktrees and records the dispatch bindings.
4. Each Crew acknowledges the typed assignment message and records a start work log against the same run.
5. Crew record meaningful milestones in Powder without generating conversational noise.
6. Blockers and decisions travel through the durable inbox and remain linked to the card and run.
7. Lifecycle events wake the Captain when attention is required.
8. Crew submit final evidence with commits, tests, failed checks, and residual risk.
9. The Captain reviews the actual artifacts and requests corrections through the inbox when necessary.
10. The Captain completes the Powder card with proof only after acceptance passes.
11. The Captain lands or preserves the work under the applicable authorization, then closes Crew and releases resources safely.
12. Material roadmap changes update the phased plan, and the current runtime state updates the handoff.

## Multiple Captains and Integration Ownership

Multiple Captains may hold distinct Assignments in the same Project.
Each Assignment must have explicit scope and Powder ownership.
Shared interfaces require a declared integration order and one integration owner.
Two Captains must not edit the same registry schema, migration, lifecycle core, or version files concurrently without an agreed boundary.

When overlap is discovered, the Captains exchange a coordination message that identifies the files, interface, dependency, recommended owner, and impact.
They continue non-overlapping work while the ownership decision is pending.
If agreement would materially change either Assignment, the General decides or authorizes a formal transfer.

## Failure and Recovery Rules

Messages, Powder events, dispatch transactions, approvals, and retryable mutations must carry stable idempotency identities.
After an ambiguous timeout, reread authoritative state before retrying.
Never blind-repost a non-idempotent Powder work log or completion mutation.
Automated Powder completion requires a server-enforced expected-current-run precondition so a delayed operation from run A cannot complete run B after release, expiry, or reclaim.
Work-log and criterion mutations require current-run attribution and retry recovery before T-Hub may treat them as authoritative current-run evidence.
When Powder does not provide those generic guarantees, T-Hub must keep the dependent mutation disabled and fail closed rather than approximate it locally.

After T-Hub, WSL, terminal, or Harness restart, recover durable identity and bindings before sending or accepting new work.
Queued messages must survive terminal replacement.
An unknown resume identity blocks terminal steering but does not erase the inbox message, Powder work, or durable Captain or Crew identity.

A runtime failure triggers recovery rather than retirement.
A failed message delivery remains visible and retryable.
A Powder outage blocks new Powder-backed dispatch and completion but does not authorize local emulation of claims or card/run execution state.

## Security, Privacy, and Retention

Message bodies, work logs, proof fields, and audit records must be bounded and scrubbed for known credential shapes.
Machine surfaces must never expose protected Powder endpoints, profiles, tokens, credential commands, or arbitrary card and run substitution fields when durable bindings already supply authority.
Audit records should preserve actor, operation, target, outcome, and safe hashes or summaries rather than sensitive message bodies or proof URLs.

Use thirty days as the provisional recommended default for local inbox message bodies until the General resolves the retention setting.
Keep non-secret delivery metadata longer for recovery and audit.
Keep user-pinned messages until explicitly removed.
Powder retention remains governed by Powder rather than duplicated inside T-Hub.

## CLI, MCP, and UI Contract

The compact JSON `th` CLI is the canonical agent and automation interface.
MCP is a role-filtered thin adapter over the same backend operations and schemas.
The graphical UI consumes the same authoritative state and must not define separate communication semantics.

CLI and MCP must provide equivalent authorized operations for send, list, read, reply, acknowledge, accept, decline, complete, retry, cancel, supersede, Powder evidence, lifecycle attention, typed approval request, approval decision, approval cancellation, and approval status.
JSON output must remain bounded, deterministic, parseable, and free of diagnostics on stdout.
Unknown inputs and forbidden identity substitutions must fail before side effects.

## Activation Tests and Exit Gate

Before this contract is considered implemented, packaged tests must prove:

- Assignment dispatch and acknowledgement survive application and terminal restarts.
- Powder work logs remain tied to the exact card, run, and Crew binding.
- Routine progress does not require terminal polling or direct messages.
- Blocker, decision, permission, review, completion, and peer-coordination messages reach only authorized durable recipients.
- Message retries are idempotent across timeout, crash, reconnect, and duplicate event delivery.
- `send_text` cannot be mistaken for message acknowledgement or card completion.
- Work state, runtime health, Powder status, and technical proof remain separate when they conflict.
- A Captain cannot mutate a peer Captain or foreign Crew through messaging.
- Local unrestricted Harness execution does not grant T-Hub control-plane capability.
- Completion cannot release, renew, or delete stale Crew state after the completion operation begins.
- A delayed run A operation cannot append evidence to, approve criteria for, complete, renew, release, or clean up run B.
- The General can inspect message content, delivery lifecycle, Powder evidence, technical proof, and cleanup outcome without relying on terminal scrollback.
- CLI, MCP, and UI return equivalent results for the same authorized operation.

The contract exits only when one real installed Codex Crew and one real installed Claude Crew each complete the full lifecycle through Powder evidence, durable dialogue, Captain review, completion proof, and safe resource cleanup.

## Current Implementation Boundary

The relationship, authority, channel, evidence, and recovery decisions in this document are settled.
The exact message-body retention duration remains provisional.
Implementation remains incomplete until the phased plan exit gates pass.
The durable inbox substrate exists, but generic send, receive, acknowledgement, message history, and frontend visibility remain open.
Interactive Codex lifecycle parity remains incomplete.
Terminal steering remains a compatibility path and has demonstrated cases where accepted text remained unsubmitted in the interactive composer.
Powder work-log and bounded evidence reads are active T-Hub implementation work for the next packaged version.
Automated completion remains blocked on the generic Powder run-bound, current-run evidence, reviewer-attribution, and idempotent recovery dependencies recorded in the phased plan.
