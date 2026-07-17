# T-Hub Phased Production Plan

**Updated:** 2026-07-17.
**Plan source:** `9d7c9f9` on `fix/captain-control-runtime` plus the General-directed scoped-autonomy integration recorded by this change.
**Installed build:** T-Hub `0.3.103` from exact detached source `8654986`, running on the canonical profile as Windows PID `39140` when this plan was refreshed.
**Purpose:** This is the canonical zero-context roadmap for completing T-Hub.

## How to Use This Plan

Read this document before starting a new implementation session.
Use [REVIEW-INDEX.md](./REVIEW-INDEX.md) to distinguish canonical, supporting, historical, and archived documents.
Treat the phase exit gates as product requirements, not suggestions.
Work may proceed in parallel only where the dependency map explicitly permits it.
Do not use a later phase to waive an earlier correctness or safety gate.
Commit each verified logical change separately.
Push, publish, install, create external repositories, spend money, or perform destructive cleanup only with the General's authorization.

The user artifacts `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` must remain untouched unless the General explicitly approves changing their status.

## Product Vocabulary

- **General:** The human user and final authority.
- **Cortana:** The permanent lightweight T-Hub orchestrator identity.
- **Project:** A saved codebase and its canonical repository or main worktree metadata.
- **Assignment:** The durable responsibility given to one Captain within a Project.
- **Captain:** A durable agent identity responsible for an Assignment and any Crew it creates.
- **Workspace:** A coherent workstream or feature grouping controlled by a Captain.
- **Crew:** A bounded worker agent assigned to a Workspace, worktree, and normally a Powder card.
- **Powder board:** The authoritative work ledger containing cards, claims, runs, work logs, input requests, completion evidence, and events.
- **Harness:** The agent runner, initially Codex or Claude Code, with future adapters such as a GLM-compatible runner.
- **Provider:** The model or account provider used by a Harness.
- **History:** The provider-agnostic catalog of resumable and archived agent sessions.
- **Provider limits:** Account-level usage or rate-limit windows, distinct from conversation context and local resource pressure.

## Settled Operating Decisions

1. Cortana always exists as a durable identity.
2. Cortana may change terminal, Harness, Provider, or model without losing identity or checkpoints.
3. Cortana is a lightweight operational coordinator, not a Captain-of-Captains that decomposes implementation work.
4. Multiple Captains may work in one Project.
5. A Captain owns an Assignment, not the entire Project.
6. A Captain may control zero, one, or several Workspaces.
7. A Workspace contains related Crew and worktrees.
8. A Captain terminal does not require a dedicated work Workspace.
9. Completing a Workspace does not retire its Captain.
10. Resetting context does not retire a Captain.
11. Broken terminals trigger recovery rather than retirement.
12. Cortana retires a Captain only through explicit or previously delegated retirement intent and only after safety checks pass.
13. Captains may message other Captains for coordination, requests, and technical help.
14. Peer messaging grants communication but no command authority over another Captain or its Crew.
15. T-Hub should default Cortana, Captains, and Crew to unrestricted execution while displaying that authority clearly.
16. The initial Codex default is the user's configured `gpt-5.6-sol` with medium reasoning effort.
17. The control plane should be CLI-first, with MCP retained as an optional thin adapter over the same operations.
18. History, lifecycle telemetry, voice, notifications, and settings should be provider-agnostic.
19. Powder remains authoritative for card and run execution state, while T-Hub remains authoritative for runtime identity, terminals, Workspaces, and owned resources.
20. Raw CPU, RAM, process, and context samples remain local to T-Hub rather than turning Powder into a telemetry database.
21. Agent work state and runtime health are independent axes governed by [STATUS-MODEL.md](./STATUS-MODEL.md).
22. Worktree identity, ownership, freshness, and cleanup safety are computed once by the backend under [WORKTREE-STATUS-CONTRACT.md](./WORKTREE-STATUS-CONTRACT.md).
23. Agent authority, supervision, evidence, dialogue, escalation, review, and completion follow [AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md](./AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md).
24. This phased plan governs strategy and dependencies, Powder is the executable backlog and work ledger, T-Hub inbox messages carry durable dialogue, lifecycle events carry attention and runtime transitions, and Git plus verification artifacts provide technical proof.
25. Terminal typing is an inspected compatibility path rather than an authoritative message or acknowledgement channel.
26. Every material review finding belongs in this plan even when its implementation is deferred, while only independently executable near-term units belong on the Powder backlog.
27. A change belongs in Powder only when it improves Powder's generic ledger, concurrency, authorization, or retry contract for every client rather than accommodating a T-Hub-specific workflow.
28. T-Hub must fail closed when a required Powder guarantee is unavailable and must not simulate that guarantee with a read-then-write race.
29. Every changed packaged build receives a new version across all authoritative version files before it is built, and one version may identify only one exact source commit.
30. Focused regression tests run during implementation, while the comprehensive quality gate runs at integration, pull-request, packaged-acceptance, and release boundaries.
31. Durable authority role, provider-neutral work profile, and resolved Harness runtime are separate concepts; changing a model never changes General, Cortana, Captain, Crew, or reviewer authority.
32. T-Hub resolves versioned work profiles through provider adapters, records the exact provider, Harness, model, effort, permissions, and fallback used, and never silently changes provider, capability, cost class, or authority.
33. Profile routing is Codex-first for initial delivery, while Claude and future provider mappings remain availability-aware adapter policy rather than permanent product vocabulary.
34. Captain autonomy uses typed scoped delegation rather than a blanket Captain role exception or fleet-wide control.
35. Durable organizational identity, T-Hub capability, scoped grants, and Harness-local execution permission are separate authority axes, and none silently expands another.
36. A live control-capable Captain without a Project may consume only an exact one-time existing-repository bootstrap approval with `maxUses=1` that was issued by the General or by Cortana under an exact pre-existing issuance delegation and binds the Captain's durable identity, current session generation, ship, canonical root, Assignment, protected profile identifier, existing board, operation, request digest, and expiry.
37. `register_project` must not receive a blanket Captain exception because its current transaction can create or initialize Git and update Project metadata or Powder bindings before or after the present role check.
38. Appturnity is the first installed end-user reproduction for the self-bootstrap slice, and no Appturnity Crew may be dispatched until its Project, Captain, Assignment, protected profile, and Powder board link are authoritatively persisted and reread.
39. Named authority profiles are visible templates that expand into explicit T34 grant records; `captain-standard`, `captain-project-builder`, and `captain-delivery` are distinct from T33 Harness and model work profiles, while `captain-production-operator` is never a default standing grant and normally resolves to exact one-time approvals.
40. No standing or delegated Captain profile may authorize protected profile endpoint or credential-command mutation, system-global software installation, or public external repository creation; where supported, only a separately reviewed exact General one-time approval may authorize a narrow instance, while credential reading or export and cross-Project authority remain absolute denials.
41. T35 atomically consumes the one-time bootstrap approval into an immutable operation-owned recovery lease before its first mutation; only the same durable Captain, ship, request digest, root, Assignment, profile, board, and operation may resume it through a verified replacement-generation handoff, and no new or changed request inherits that authority.

## Roadmap, Backlog, and Runtime Evidence

This plan is the consolidated forward roadmap.
Record product decisions, dependencies, phase status, parallelization constraints, test doctrine, and exit gates here.
Do not turn the plan into a high-frequency task log.

Powder is the executable backlog.
Represent independently assignable work as cards with acceptance criteria, proof plans, claims, runs, work logs, blockers, and completion evidence.
Use separate cards and isolated worktrees for parallel lanes, with one declared integration owner.
A Powder card cannot override this plan or another canonical authority contract.

The T-Hub inbox is the durable dialogue layer for instructions, blockers, decisions, permissions, review findings, completion reports, and peer coordination that require delivery or acknowledgement.
Lifecycle events provide event-driven attention and runtime-health transitions without requiring periodic model polling.
Git, tests, review, builds, and packaged acceptance remain the technical proof layer.
The current handoff records the verified resume point across roadmap, backlog, runtime, and technical evidence.

## Review-Derived Change Boundary

The dedicated roadmap, installed-product, and architecture review completed on 2026-07-15.
The review did not mutate source, runtime, terminals, Powder state, or the active Captain integration.
It found one Critical staged completion race, several High durability and orchestration gaps, and deferred product and release work that must remain visible from the earliest planning stage.

The review uses three ownership categories.

- **Powder generic** means Powder must provide a server-enforced capability that is correct for every concurrent or retrying client.
- **T-Hub** means T-Hub owns orchestration policy, durable identity, runtime control, client recovery, user experience, and packaged acceptance.
- **Cross-repository** means Powder supplies the generic primitive and T-Hub consumes it without adding a local workaround.

Agent orchestration exposed the highest-risk races because Captains and Crew create concurrent claims, retries, and ambiguous network outcomes.
The Powder-generic findings are not caused only by agents and remain worthwhile for CI runners, human operators, integrations, and any other concurrent Powder clients.
The T-Hub findings are primarily orchestration-product responsibilities and should not be pushed into Powder.

### Powder-Generic Change Register

| ID | Required generic change | Why it is broadly useful | Orchestration relationship | Dependency and exit condition | Recommended Powder card |
| --- | --- | --- | --- | --- | --- |
| P1 | Add server-enforced run-bound conditional completion that accepts the card, expected current run, operation identity, and proof. | Prevents an old worker or delayed request from completing a newly claimed run. | Exposed by Crew reclamation, but applies to every concurrent worker. | Blocks automated T-Hub completion and Phase 8A exit or shipment, while fail-closed scaffolding may proceed, until run A can never complete run B and replay is idempotent. | `powder-run-bound-conditional-completion` |
| P2 | Add durable idempotency and operation-status recovery for work-log append and completion mutations. | Makes timeouts and lost responses safely retryable without duplicate evidence or effects. | Frequent in agent workflows, but generic distributed-system correctness. | Blocks retry-safe T-Hub work-log and completion claims until response loss can be reconciled by operation ID. | `powder-mutation-idempotency` |
| P3 | Enforce current-run attribution for work logs and return the normalized stored record as the authoritative result. | Prevents stale workers from attaching evidence to another run and makes server redaction compatible with clients. | Crew supply run evidence, but the audit-integrity rule is client-agnostic. | Blocks final evidence acceptance until stale-run append fails and normalized responses are documented and versioned. | `powder-run-bound-work-logs` |
| P4 | Define run-scoped acceptance-criterion review semantics with authenticated reviewer identity and current-run proof. | Prevents stale or arbitrary criterion checks from being presented as current approval. | Captain review motivates the requirement, but any approval workflow needs trustworthy attribution. | Blocks proof-based automated completion until criterion checks are tied to the intended run and authorized reviewer. | `powder-run-scoped-criteria` |
| P5 | Add a versioned create-only or conditional repository operation that never overwrites a concurrently created board. | Provides safe create-if-absent semantics for every provisioning client. | Needed by automatic Project onboarding, but useful to all Powder automation. | Blocks automatic board creation, but not manual selection or binding, until a server-enforced non-overwrite precondition exists. | `powder-conditional-repository-create` |
| P6 | Verify idempotency, operation recovery, and revision preconditions for card create, update, relation, proof-plan, and status mutations, adding generic guarantees where absent. | Prevents duplicate cards and lost concurrent edits after retries, response loss, or multiple writers. | Captains author cards frequently, but every automated backlog client needs the same mutation safety. | T-Hub Captain card authoring may use each existing operation only after that operation satisfies this gate. | `powder-card-authoring-preconditions` |
| P7 | Document the complete versioned Powder event contract, including the existing `work-log-appended` event and its compatibility expectations. | Lets every event consumer distinguish supported events from implementation details. | T-Hub discovered the gap, but it affects every event-stream client. | T-Hub may remain event-name agnostic, but it must not depend on undocumented event-specific behavior. | `powder-card-event-contract-docs` |

These changes must be designed and reviewed in the Powder repository as generic API and storage contracts.
T-Hub must not patch or fork Powder behavior locally to make acceptance pass.

### T-Hub Change Register

| ID | Required T-Hub change | Phase | Orchestration-specific | Dependency or exit condition | Recommended T-Hub card |
| --- | --- | --- | --- | --- | --- |
| T1 | Keep automated Powder completion disabled and fail closed until P1 through P4 are available, then send and verify exact card, run, operation, criterion, and proof identity. | 8A | Yes | No card-only completion may ship. | `thub-powder-run-bound-mutations` |
| T2 | Serialize complete, renew, heartbeat, release, close, cleanup, and reconciler actions with one per-Crew lifecycle-operation guard. | 2 and 8A | Yes | Barrier-controlled concurrency tests prove no stale renewal or release can cross completion. | `thub-powder-lifecycle-serialization` |
| T3 | Reconcile ambiguous work-log and completion responses by operation ID, accept Powder's authoritative normalized record, and reject stale-run evidence. | 5 and 8A | Yes | Depends on P2 and P3; retries must never duplicate evidence. | `thub-powder-evidence-recovery` |
| T4 | Expose Captain-authorized Powder card create, update, relation, proof-plan, and status operations through the shared backend, CLI, MCP, and UI catalog. | 5, 7, and 8 | Yes | Each mutation depends on its P6 idempotency, recovery, authorization, and revision-precondition gate. | `thub-captain-card-authoring` |
| T5 | Make Captain creation one idempotent, observable, resumable transaction with a stable request ID and operation-status recovery. | 7 | Yes | Double-click, timeout, lost response, dialog close, refresh, restart, and retry create at most one Project and Captain. | `thub-captain-creation-recovery` |
| T6 | Match Powder boards by canonical repository identity, recommend only relevant boards, auto-bind one unambiguous match, and create only through P5 when none exists. | 7 and 9 | Partly | Unrelated boards remain behind an Advanced path and never become the default. | `thub-board-relevance-auto-bind` |
| T7 | Implement durable Assignment identity and allow multiple Captains in one Project without Workspace, terminal, Crew, or migration collisions. | 3 | Yes | Two distinct Assignments survive restart and retain separate ownership. | `thub-multi-captain-assignment-registry` |
| T8 | Route Powder events by exact Project, Assignment, card, run, Crew, and Workspace ownership instead of selecting the first Captain. | 3, 6, and 8B | Yes | Depends on T7; unowned and conflicting events stay visible and replay remains idempotent. | `thub-powder-event-owner-router` |
| T9 | Make inbox persistence fail atomic so enqueue, delivery, read, acknowledgement, and transition success are returned only after durable storage succeeds. | 6 | Yes | Injected create, write, flush, rename, reopen, and restart failures never lose acknowledged state. | `thub-inbox-fail-atomic-durability` |
| T10 | Bound inbox UTF-8 bodies, standard and Emergency capacity, retention, backpressure, and overflow states. | 6 and 12 | Yes | Emergency priority may not create unbounded memory or disk growth. | `thub-inbox-bounds-retention` |
| T11 | Derive backend, CLI, MCP, and UI operation schemas from one actor-aware catalog with role-filtered discovery and parity tests. | 5 | Yes | CLI remains canonical; MCP stays a thin optional adapter and cannot expose an unfiltered static catalog. | `thub-shared-operation-catalog` |
| T12 | Bound control-server JSON frames before parsing or authentication and test oversized and unterminated requests. | 1 and 12 | No | A local peer cannot cause unbounded allocation or exhaust all connection slots. | `thub-control-frame-bounds` |
| T13 | Reproduce and eliminate the installed callback-ID, IPC fallback, frontend mount, and duplicate background-scan storm, then rerun the duplicate Codex transcript and composer-draft regression. | 1, 9, and 11 | No | A 30-minute packaged stress run has zero missing callbacks, IPC fallbacks from stale callbacks, mount hangs, equivalent duplicate in-flight scans, duplicate transcript frames, or repeated composer text. | `thub-frontend-ipc-scan-storm` |
| T14 | Put Provider limits and History behind singleton stale-while-revalidate services that retain last-known data through transient backend and WSL failure. | 9 and 11 | No | Refresh, remount, and Workspace switching never blank previously valid usage or History. | `thub-usage-history-cache` |
| T15 | Disable unsafe account-level Codex auto-continue and implement exact-thread durable scheduling, deduplication, cancellation, restart recovery, and visible failure. | 4 and 9 | Yes | One provider reset can never submit continuation text to multiple or uncertain Codex sessions. | `thub-codex-exact-thread-continue` |
| T16 | Reproduce and repair Full Board in the installed application, provide a Project-filtered native expansion, and label any external Powder board as global. | 9 | No | Packaged Windows E2E proves browser launch, authentication, focus, unreachable handling, and credential-free URLs. | `thub-full-board-packaged-e2e` |
| T17 | Make Crew completion, Captain waiting or completion, claim release, run release, terminal retirement, and owned-worktree cleanup reflect authoritative evidence. | 2, 8, and 10 | Yes | Completed Crew and Captains do not remain falsely Working and no stale claims or disposable sessions remain. | `thub-captain-crew-terminal-state` |
| T18 | Implement the unified worktree status and ownership service before enabling automatic deletion or retirement cleanup. | 2 and 3 | Yes | Every cleanup decision uses one backend snapshot and fails closed under identity, freshness, or ownership uncertainty. | `thub-worktree-status-service` |
| T19 | Add bounded dual-provider History discovery, collision-safe durable joins, exact resume identity, archive semantics, and user-facing Codex History. | 4 and 9 | Partly | Identical cwd values never merge different conversations and both Harnesses survive restart. | `thub-history-provider-parity` |
| T20 | Normalize Codex and Claude needs-input, completion, failure, and recovery events into one audible voice and notification path. | 4 and 10 | Yes | Real packaged audible tests cover both Harnesses, duplicate suppression, engine failure, and disabled settings. | `thub-provider-voice-parity` |
| T21 | Complete packaged Windows folder-picker acceptance for cancellation, path conversion, inaccessible paths, distro mismatch, junctions, and symlinks. | 7 and 9 | No | The graphical picker and conversational flow resolve the same safe WSL path identity. | `thub-folder-picker-packaged-e2e` |
| T22 | Verify Claude header identity persistence across Refresh, remount, restart, and conversation resume. | 4 and 9 | Yes | Header text remains bound to the same durable session identity. | `thub-claude-header-acceptance` |
| T23 | Add a durable Project run profile with typed package root, command, environment readiness, port policy, and preview URL rules. | 9 | No | Chat or agents may propose a profile, but only validated and explicitly accepted typed configuration can execute. | `thub-run-target-resolution` |
| T24 | Define numeric input, visual-switch, cold-restore, first-feedback, process, memory, and scan-deduplication budgets and extend the packaged 1, 4, 8, and 16 matrix. | 11 | No | Release performance conclusions include input readiness and Workspace switching, not only CPU and process counts. | `thub-performance-budgets-matrix` |
| T25 | Split the control plane into owned modules without changing public behavior or bypassing the shared catalog and lifecycle guards. | 2, 5, and 12 | No | New control-plane growth no longer concentrates in the approximately 21,000-line `control.rs`. | `thub-control-modularization` |
| T26 | Reconcile phase status, dependency edges, reduced test cadence, global version policy, stale handoff state, and accepted messaging-contract integration. | Plan governance | No | A zero-context session can name the earliest unblocked phase and exact active cards without conversation history. | `thub-plan-status-and-cadence-reconcile` |
| T27 | Consolidate devtools and capability reduction, MSRV, license, security, privacy, support, runbook, backup, reset, diagnostics export, corruption, disk-full, and operator support gates. | 12 | No | Production acceptance cannot pass while required operational or public documents and recovery paths are absent. | `thub-release-readiness-consolidation` |
| T28 | Review diagnostic and event-journal retention, including the observed approximately 46.9 MB event journal, and define bounded ownership and cleanup. | 11 and 12 | No | Long-running use has no unbounded log, journal, queue, handle, socket, process, or memory trend. | `thub-journal-retention` |
| T29 | Make `th health --json` report honest Windows and WSL host telemetry, including explicit unsupported or unavailable fields instead of a healthy all-zero skeleton. | 1, 5, and 10 | No | Health output cannot claim success when the requested host metrics were never collected. | `thub-health-host-telemetry` |
| T30 | Make control clients recover stale endpoints promptly with one bounded overall deadline, visible retry state, and no inherited-port delay. | 1 and 5 | No | Capability, health, and list calls recover or return a structured timeout without a 30-to-45-second ambiguous hang. | `thub-control-client-deadline` |
| T31 | Reproduce and eliminate avoidable Workspace-switch terminal restoration delay while preserving bounded terminal resources. | 1, 9, and 11 | No | Packaged warm and cold switches have numeric visual-switch and input-ready budgets; an immediate return never reattaches, and a cold return shows the last authoritative frame immediately while one bounded background attach restores live input. | `thub-workspace-switch-latency` |
| T32 | Make the sidebar show authoritative, legible agent activity for Codex and Claude instead of treating recent terminal output as equivalent to a running agent. | 4, 8, and 9 | Yes | Packaged Codex and Claude runs remain visibly Working through quiet reasoning and tool execution, distinguish needs-input, idle, completed, failed, stale, and unknown states, and expose degraded telemetry rather than a blank status slot. | `thub-sidebar-agent-status` |
| T33 | Implement versioned provider-neutral work profiles with Codex-first model and reasoning routing for Captain commissioning, Crew dispatch, scouting, review, and explicit escalation. | 4, 5, 7, 8, and 11 | Yes | CLI-first preflight and dispatch persist the requested profile and exact resolved runtime, existing sessions remain pinned, unavailable models fail visibly or use one explicitly displayed fallback, and no model choice changes authority or Powder state. | `thub-agent-routing-profiles` |
| T34 | Add a versioned durable scoped-grant contract and one shared evaluator for standing grants, strict delegated subsets, authority templates, approved one-time artifact consumption, revocation, explanation, Project-scoped audit, and bounded decision evidence. | 3, 5, 6, 7, 8, and 12 | Yes | A control token, working directory, prompt, or broad role can never exceed exact identity and grant generation, ship, Project or bootstrap root, Assignment, board, card, run, worktree, branch, environment, external target, expiry, use, and resource constraints; the empty store preserves existing ACL behavior, no authority is inferred from prose or runtime history, and persistence, migration, corruption, revocation races, and redaction fail closed. | `thub-captain-scoped-grants` |
| T35 | Add an exact existing-repository Captain self-bootstrap operation that composes Project registration, existing-board binding, current-Captain attachment, Assignment binding, checkpoint, and operation-status recovery. | 3, 5, 7, 8, and 12 | Yes | The live Appturnity denial is preserved as the first E2E reproduction, and a granted exact request converges through retry and restart on one Project, Captain, Assignment, binding, and checkpoint while every duplicate-root, foreign, ambiguous, stale-generation, or changed-request case fails before mutation. | `thub-captain-existing-project-self-bootstrap` |
| T36 | Add bounded routine delivery grants for owned non-protected branch push, draft pull-request and allowed-reviewer operations, owned-branch CI inspection or retry, bounded preview and non-production resource provisioning and retirement, isolated non-production migration, and non-secret configuration metadata reads. | 5, 9, and 12 | Yes | Only reviewed adapters and exact owned resources may be targeted; secret values, protected branches, production, public repository creation, public release, customer outreach, spending, and destructive cleanup remain separately General-gated. | `thub-captain-routine-delivery-grants` |

The change register is authoritative planning input even before its cards are created.
Powder cards should be created only when their lane is near-term, bounded, dependency-ready, and assigned to an isolated worktree with an explicit exit gate.
Review findings must not disappear merely because a card is deferred, completed, superseded, or cleaned from the active board.

### Captain Autonomy Delta Classification

The General-directed [Captain autonomy and scoped grants integration plan](./CAPTAIN-AUTONOMY-AND-SCOPED-GRANTS-PLAN.md) is integration input for this existing roadmap.
It does not replace the active goal, phase order, current Powder work, or the existing owners named below.
The live Appturnity reproduction returned `acl: only General/Cortana may register a new project` before any Project or Powder binding mutation became observable, but it did not establish which read-only normalization steps ran.
Current source independently confirms that `enforce_project_authority` rejects a new Project for a Captain and that `register_project` reaches filesystem and Git preparation before its Project authority decision.

| Requirement group | Classification | Canonical owner and required delta |
| --- | --- | --- |
| Project, Assignment, ship, Captain, Crew, peer-Captain, and cross-ship isolation | Already covered | T7 and the agent relationship contract remain authoritative; T34 and T35 add negative role and foreign-state non-disclosure tests without weakening those boundaries. |
| Durable identity, T-Hub capability, work profile, and Harness-local permission separation | Already covered | T7 and T33 retain ownership; T34 adds scoped grants as another independent axis, and Crew retain read-capability T-Hub tokens by default. |
| Protected Powder credentials and profile storage | Already covered | The Powder integration contract remains authoritative; a grant may contain an authorized profile identifier only and never an endpoint, key, credential command, raw environment value, or caller-supplied arbitrary profile. |
| General-gated production, protected-branch, destructive, customer, spending, public repository creation, protected profile mutation, system-global installation, publication, release, retirement, and authority-elevation operations | Already covered | The relationship, operating-model, and Phase 12 gates remain authoritative; T34 adds explicit negative grant-evaluator coverage and cannot mint apex authority. |
| Non-overwriting Powder board creation | Already covered | P5 remains the sole generic owner; existing-board self-bootstrap does not depend on P5 and must not create a board. |
| Retry-safe Powder card authoring and Captain card operations | Already covered | P6 and T4 retain ownership; T34 supplies authorization descriptors, while exact board and Assignment scope, revision conflicts, stable criterion identity, Captain attribution, and foreign-card non-disclosure strengthen their acceptance. |
| Captain creation idempotency and operation-status recovery | Requiring stronger acceptance criteria | T5 must carry one canonical request digest and operation identity across Project registration, existing-board binding, current-Captain attachment, Assignment binding, checkpoint, response loss, restart, resume, and explicit partial-state rollback. |
| Canonical board relevance and protected existing-board binding | Requiring stronger acceptance criteria | T6 must use the protected client, exact canonical repository identity, grant-bound profile and board constraints, and explicit ambiguity handling; a name or alias alone never proves repository identity. |
| Assignment identity and multiple Captains | Requiring stronger acceptance criteria | T7 must define a transitional durable unbound-Captain and current session-generation interface that can consume only an exact bootstrap grant before binding one Project and Assignment. |
| Shared actor-aware operation catalog and parity | Requiring stronger acceptance criteria | T11 must describe broad capability, absolute role denials, grant requirements, constraints, effective grant state, dependency availability, provider support, and bounded authorization explanations through equivalent backend, CLI, MCP, and applicable UI results. |
| Durable inbox and typed approval lifecycle | Requiring stronger acceptance criteria | Phase 6 with T9 and T10 exclusively owns approval request, decision, cancellation, persistence, and state transitions; T34 adds standing grants and delegated subsets and may evaluate and atomically consume an approved exact-operation artifact without creating a parallel approval state machine or schema. |
| Provider-neutral dispatch and work profiles | Requiring stronger acceptance criteria | T33 must persist requested and resolved runtime, local permission, exact delegated T-Hub operation subset, grant reference, limits, expiry, provider-native attestation, and visible fallback without elevating Crew capability. |
| Owned worktree and resource lifecycle | Requiring stronger acceptance criteria | T18 and T23 retain canonical snapshot, safety, run-profile, and cleanup ownership; a grant may permit a request but never substitutes for current ownership, liveness, freshness, or cleanup evidence. |
| Saved, existing, and new codebase onboarding | Requiring stronger acceptance criteria | Phase 7, T5, T6, T18, T21, T23, T33, and P5 remain owners; existing-repository Captain self-bootstrap is the narrow new T35 slice, while Project Builder stays a composition rather than a duplicate workstream. |
| Exact existing-repository Captain self-bootstrap | Genuinely new work | T35 owns the grant-evaluated service and Appturnity E2E; it may attach only the calling live Captain to one exact canonical existing root and existing authorized board. |
| Versioned durable scoped grants, standing authority, strict delegated subsets, templates, shared evaluation, explanation, and audit | Genuinely new work | T34 owns the schema, private bounded store, pure policy, approved-artifact consumption and revocation linearization, compatibility migration, decision explanation, Project-scoped audit view, and security matrix while consuming T7 identity, T11 catalog, and Phase 6 approval interfaces. |
| Actor-visible capability discovery | Requiring stronger acceptance criteria | T11 must distinguish granted, grant required, one-time approval required, temporarily unavailable, external dependency blocked, provider unsupported, and absolutely denied states without exposing foreign metadata. |
| Routine non-protected delivery operations | Genuinely new work | T36 owns reviewed adapters for owned branches, draft pull requests, CI, previews, and non-production migrations only after T34, T11, T18, and T23 are stable. |
| Appturnity denial and self-bootstrap E2E | Requiring stronger acceptance criteria | T35 must preserve the live installed denial as immutable first reproduction, use disposable equivalents for mutation tests, pass independent security review, and verify exact installed source, build, installer, binary, operation, Project, Assignment, Captain, profile-name, and board evidence. |
| Role, retry, restart, concurrency, corruption, redaction, and packaged Windows acceptance | Requiring stronger acceptance criteria | Phase 12 and the testing doctrine remain owners; T34 and T35 add autonomy-specific adversarial matrices and no-success-before-durable-reopen gates. |

The T34 Project-scoped audit view is visible only to the General and the owning Captain unless a narrower reviewed role explicitly requires it.
Each audit record carries stable operation and grant identifiers, actor, target, decision, failed constraint, recovery disposition, timestamp, and duration.
Audit records omit prompts, message bodies, credential material, raw environment values, and foreign-Project metadata.

### Captain Autonomy Dependency and Ownership Map

The first vertical slice is T34 followed by T35.
T34 may design its pure schema and policy interfaces alongside T7 and T11, but activation must consume their authoritative durable identity and operation descriptors rather than minting parallel registries or catalogs.
T35 depends on T34, T5, T6, T7, the shared canonical Git identity primitive, and the minimum T11 descriptor and parity surface.
T35 must converge with T18's authoritative canonical-root and ownership model when that interface lands, but the existing-repository slice does not wait for T18 activation and cannot re-enable worktree removal.
T35 does not depend on P5 because Appturnity already has an existing board and this slice must never create one.
T25 owns any control-plane modularization needed to keep grant policy out of the monolithic handler, but a mechanical split cannot delay the safe contract or change behavior.
One integration owner must coordinate the T7 identity, T34 authorization, T5 operation journal, T6 protected-board binding, and T11 catalog boundaries before shared control files change.

Implementation order and exit gates are:

1. Preserve the installed Appturnity ACL denial and current source denial as the immutable end-user reproduction, with no Appturnity mutation or Crew dispatch.
2. Land T34's versioned grant and decision schemas, fail-atomic private store, pure evaluator, absolute denials, standing and one-time modes, strict delegated subsets, revocation and use linearization, compatibility migration, explanation, audit, and exhaustive General, Cortana, owning Captain, peer Captain, foreign Captain, owning Crew, sibling Crew, foreign Crew, unidentified read-capability caller, control-capability caller without valid per-session identity, trusted in-process host, reviewer, unbound-Captain, stale-generation, and foreign-Project role matrix.
3. Make the empty grant store preserve current ACL behavior, infer no authority from prompts, messages, handoffs, Powder cards, shell history, conversation history, or working directories, keep deprecated coarse authorization artifacts readable until verified migration, and preserve host-canonical roots instead of rewriting them for WSL convenience.
4. Require independent security review proving that a control token alone cannot exceed role, ship, exact Project or bootstrap root, Assignment, board, branch, environment, expiry, uses, limits, or current session generation, and that no secret or foreign-state detail appears in persistence, JSON, logs, audit, or errors.
5. Integrate T34 with T11 descriptors and Phase 6 request and decision surfaces while preserving stable bounded CLI JSON, symbolic denials, operation status, MCP thinness, and applicable UI parity.
6. Implement T35 as a dedicated existing-root operation whose grant decision, canonical request digest, approval consumption, and immutable recovery lease are atomically persisted before any filesystem, Git, Project, Powder, Captain, Assignment, terminal, or checkpoint mutation; the Captain cannot issue or delegate its own bootstrap grant.
7. Serialize T35 by the shared canonical Git main-worktree identity and revalidate actor identity, role, ship, session generation or verified recovery handoff, grant version, revocation, expiry, exact root, profile identifier, board identity, request digest, and exclusive operation ownership immediately before each mutation boundary.
8. Make an existing exact Project converge, reject multiple Projects for the same canonical root without mutation or existence leakage, and make identical retries converge on one Project, Captain, Assignment, binding, checkpoint, and result.
9. Make changed-request reuse conflict before side effects and give every committed partial state an explicit operation-owned resume or non-destructive rollback disposition.
10. Permit a replacement generation to assume only that exact in-flight lease when the durable Captain matches, the predecessor is authoritatively dead, no concurrent owner exists, and the grant remains unexpired and unrevoked; this handoff consumes no second use.
11. If revocation or expiry occurs after a partial commit, block further forward mutation, preserve the resumable state, permit only inspection and non-destructive rollback under the operation contract, and require a fresh exact General resume approval before forward progress.
12. Prove crash behavior before consumption, after consumption but before first mutation, and after every committed partial boundary, including restart and replacement convergence, concurrent takeover denial, subdirectory and linked-worktree convergence, POSIX and extended-UNC identity, foreign and ambiguous denial, concurrent General, Cortana, and granted-Captain registration, response loss, dialog close, application restart, WSL restart, corruption, and no credential or foreign-existence leakage through deterministic tests.
13. Run disposable packaged Windows success and denial E2E, then perform an independent identity, authorization, registration, Powder-binding, migration, and rollback review.
14. Link the live Appturnity Captain only after T35 passes the installed gate, then reread and reconcile the exact Project, Captain, Assignment, protected profile name, and `appturnity` board before any card claim or Crew dispatch.

Later integration order is:

- T4 consumes T34 authorization only as each P6 mutation gate passes.
- T33 and Phase 6 consume T34 for visible requested work profiles, exact delegated Crew subsets, standing requests, and one-time decisions.
- T18 and T23 consume T34 only for authorization while retaining authoritative resource and run safety decisions.
- T36 begins only after T34, T11, T18, T23, and reviewed external adapters are stable.
- Optional Project Builder remains Phase 7 plus P5, T5, T6, T18, T21, T23, T33, and T34, with no separate duplicate implementation item.

### Cross-Repository Dependency Order

1. P1 through P4 define safe evidence and completion primitives in Powder.
2. T1 through T3 consume those primitives and keep installed automation fail closed until they pass.
3. P5 optionally enables safe automatic board creation, while T6 continues to support safe selection and binding without it.
4. P6 verifies every Captain-authoring mutation independently, and T4 may expose only the operations whose generic idempotency, recovery, authorization, and revision-precondition gates pass.
5. P7 documents the versioned event contract before T-Hub or another client depends on event-specific behavior.
6. T7 establishes Assignment ownership before T8 activates multi-Captain event routing.
7. T9 and T10 make the inbox safe before Phase 6 messaging activation or Phase 8B messaging acceptance.
8. T11 supplies CLI-first parity before broad Captain card authoring and messaging commands are considered complete.
9. T13 and T14 stabilize the installed frontend before performance or usage-persistence conclusions are accepted.
10. T24 defines the numeric Workspace-switch budgets consumed by T31, while T31 owns lifecycle and rendering changes needed to meet them.
11. T32 consumes the provider-neutral lifecycle model from Phase 4 and the authoritative Captain and Crew terminal-state work from T17; its degraded-state UI may proceed without fabricating missing telemetry.
12. T33 consumes T7 Assignment identity and T11 shared-operation parity; its contract and adapter scaffolding may proceed independently, but control-plane integration must follow the active T2, T17, and T30 ownership sequence and must not delay Phase 8A safety acceptance.
13. T34 consumes T7 durable actor, Assignment, ship, and session-generation identity plus T11 operation descriptors; its schema and pure-policy design may proceed in parallel, but evaluator activation may not create substitute identity or catalog authority.
14. T35 consumes T34 grant evaluation, T5 operation identity and recovery, T6 protected existing-board matching, T7 transitional unbound-Captain identity, the shared canonical Git identity primitive, and minimum T11 parity; it does not depend on P5 or T18 activation, may not create a board, and must converge with T18's model without implementing a parallel ownership service.
15. T34 and the Phase 6 lifecycle route grant and approval attention only after exact ownership resolution, while T8 remains limited to Powder event routing and T17 remains authoritative for Captain, Crew, claim, terminal, and cleanup state; neither event routing nor a grant may substitute for current lifecycle evidence.
16. T33 and Phase 6 consume T34 for exact delegated Crew operation subsets, standing requests, and one-time decisions while preserving Crew read-capability defaults.
17. T36 waits for T34, T11, T18, T23, and reviewed external adapters, and it cannot absorb protected-branch, production, publication, spending, customer, or destructive operations.
18. The optional Project Builder flow remains Phase 7 work composed from P5, T5, T6, T18, T21, T23, T33, and T34 rather than a separate duplicate lane.

### Phase Status and Dependency Summary

| Phase | Current status | Hard dependencies and allowed partial work |
| --- | --- | --- |
| 1 | Original xterm lifecycle gate complete; follow-on control framing, installed IPC, health, and client-recovery regressions open. | T12, T13, T29, and T30 are unblocked follow-on reliability lanes. |
| 2 | Active. | Resource schema and fail-closed scaffolding may proceed; activation depends on T7 identity interfaces and T18. |
| 3 | Earliest unblocked foundational phase. | T7 owns shared identity migrations and the transitional unbound-Captain and session-generation interfaces consumed by T34 and T35; B2 and provider lanes consume the same interfaces. |
| 4 | Partially unblocked. | Adapter fixtures may proceed; durable binding depends on T7. T15 must fail closed immediately. |
| 5 | Active normalization and catalog work. | Strict CLI contract work may proceed; identity-aware expansion depends on T7 and T11, and grant-aware discovery consumes T34 without creating adapter-local policy. |
| 6 | Substrate present, activation blocked. | Depends on T7, T9, T10, T11, and the integrated messaging contract; standing grants and delegated subsets come from T34 while existing typed one-time approval lifecycle remains here. |
| 7 | Active partial product flow. | Existing-repository Captain self-bootstrap depends on T34, T5, T6, T7, the shared canonical Git identity primitive, and minimum T11 parity without waiting for T18 activation; full Project Builder exit additionally depends on T18 cleanup activation and P5 for automatic create-if-absent. |
| 8A | Active immediate Captain vertical slice, blocked from shipment. | Depends on P1 through P4, T1 through T3, and T17 before installed completion acceptance; worktree deletion remains disabled without T18. |
| 8B | Blocked full orchestration acceptance. | Depends on Phase 8A, Phases 2 through 6, and T7 through T11 plus T17 through T18. |
| 9 | Active partial product surfaces. | Individual packaged bugs may proceed; complete exit depends on shared identity, adapter, inbox, and resource contracts. |
| 10 | Blocked for complete parity. | Depends on Phases 3, 4, and 6; fixture and local voice-engine checks may proceed. |
| 11 | Active measurement foundation. | T13, T14, T24, and T28 are required before the final packaged matrix and soak. |
| 12 | Continuous preparation; final gate blocked. | T27 and every preceding phase exit are required for production release; T34, T35, and T36 also require independent security review plus installed role, retry, restart, concurrency, corruption, redaction, and provenance acceptance. |

Phase 2 is the earliest numbered phase with unblocked partial work, but its complete activation requires the Phase 3 B1 identity interface.
Phase 3 is the earliest foundational dependency whose completion unlocks the largest downstream set.
Phase 8A remains the authorized immediate product vertical slice because the installed Captain path already reached its evidence boundary, but it may exit only through the P1 through P4, T1 through T3, and T17 safety gates.

## Current Baseline

The local Powder authority is healthy on WSL at `127.0.0.1:4017`.
Windows reaches Powder privately through Tailscale Serve at `https://n8desktop-wsl.tailae53f1.ts.net`.
The protected Powder profile is `n8desktop-wsl` and authenticated remote operations have passed.
The `t-hub` Powder board and `thub-local-acceptance` card exist.
Project `project-e28c0579-4e78-4de1-b225-d69aab93c143` now registers the T-Hub codebase and binds it to the `t-hub` Powder board through `n8desktop-wsl`.
Installed `0.3.103` has a live control-capability Codex Captain at terminal `c2940be4` in the exact WSL repository directory.
The authorized Stage 1 retry dispatched exactly one Codex Crew, acquired and heartbeated Powder run `run-nO9Ih6F-Dt-E`, preserved the accepted worktree baseline, then released the run and removed the Crew cleanly after a required check failed.
The installed Stage 1 failure first exposed a missing T-Hub integration surface: installed `0.3.103` can claim, heartbeat, renew, release, and read a bounded board snapshot, but it cannot append or expose Powder work-log and completion evidence through a sanctioned Crew operation.
The subsequent architecture review found that merely exposing the existing endpoints is insufficient for safe automated completion.
Powder's current completion operation is card-bound rather than run-bound, while work-log and criterion mutations lack the complete retry, attribution, and current-run guarantees required by P1 through P4.
T-Hub may implement bounded evidence reads and fail-closed client scaffolding now, but automated completion must remain disabled until the generic Powder dependencies pass.

Terminal resource counters and hot, warm, and cold lifecycle states are implemented.
The earlier installed application reproduced xterm `loadCell` and `isWrapped` lifecycle failures.
Source commit `6870444` fixed the teardown race, and the packaged `3b83b9e` build completed cold rehydration with zero `loadCell`, `isWrapped`, or window errors.
Source commit `35fbae2` also prevents a second packaged launch from creating a competing control server and duplicate PTY attachments.
Source commit `3b83b9e` defers frontend resize commands until remote PTY attachment is confirmed, eliminating repetitive startup `no live terminal` diagnostics in packaged testing.
Source commit `a00ce7d` adds explicit Git initialization to the shared Project registration transaction used by Captain creation and MCP.
The transaction atomically reserves a new `.git` directory, initializes `main`, refuses any pre-existing `.git` entry, and removes only the `.git` directory it created when a later registration boundary fails.
Source commit `2cf4a42` adds explicit creation of one absent empty-codebase leaf to the same transaction.
It requires Git initialization, never replaces an existing path, never creates missing parents, and uses non-recursive rollback for the directory it owns.
Diagnostic logs are bounded at startup.
Installed `0.3.68` reduced the retained backup from `135,278,300` bytes to `8,388,493` bytes while preserving complete newest lines.
Installed `0.3.75` replaces the global Board URL and iframe with a native read-only Board resolved from the focused terminal's durable Project and protected Powder binding.
The packaged no-Project path shows an honest state without an iframe or manual Board URL field.
The bound Project success path remains gated on Phase 8A real Project acceptance.
Installed `0.3.76` replaces the separate Dev and Preview tabs with one **Run and Preview** surface and removes terminal-output URL scanning and automatic navigation.
Packaged verification proved that the previously reproduced WebView inspection URL remained in PTY scrollback while the preview URL stayed empty and no iframe was created.
Source commit `61f56ba`, packaged first as `0.3.77`, adds typed root-package target discovery and generation-safe backend lifecycle snapshots.
Packaged `0.3.78` discovered the T-Hub root's four declared scripts, started `pnpm run dev`, detected Vite at `http://localhost:1420/`, and stopped the exact managed run without leaving its Vite descendant alive.
Source commits `fbacc8f`, `16480b7`, and `19dc3c7`, packaged as installed `0.3.81` from `8f5fffa`, make standard Tauri Vite targets bind all WSL interfaces and preserve a localhost URL when Windows can already reach it.
Packaged acceptance started the real T-Hub Vite target, loaded `http://localhost:1420/` in the Preview iframe, returned Windows HTTP 200, bound ports `1420` and `1421` to `0.0.0.0`, and removed both listeners and the exact managed Vite PID on Stop.
Source commit `9d95fa9`, packaged as installed `0.3.82` from `ec55526`, gives each managed package target one WSL process group behind an application-owned stdin lifeline and retained Windows Job Object.
Packaged acceptance proved that Stop returned in 161 milliseconds, removed the run marker and every pnpm, Vite, and esbuild process, released ports `1420` and `1421`, and restarted successfully on the same port.
Forcing the installed application to exit while the restarted target was active also removed the complete process group and both listeners while preserving all seven unrelated tmux sessions and pane PIDs.
Installed `0.3.82` also passed representative Next.js acceptance against `apps/site`: Next `14.2.35` returned Windows HTTP 200, loaded both expected site sentinels in Preview, stopped in 161 milliseconds, and removed the complete npm and Next process group.
Source commit `5011803` adds a typed package-less static target backed by a Windows loopback-only server with traversal, hidden-path, reparse-point, symlink, MIME, method, and file-size protections.
Source commit `3177d81` clears a managed Preview URL and iframe when its run stops, and `0cd5861` packages the final result as installed `0.3.84`.
Packaged static acceptance auto-loaded the authoritative loopback URL, served HTML, CSS, JavaScript, and a nested page, denied raw, encoded, and double-encoded traversal plus hidden and symlink paths, stopped and remounted cleanly, and closed its listener on forced application exit.
The disposable acceptance session was removed afterward, and all seven canonical tmux sessions and pane PIDs remained unchanged.
Source commit `1484750` serializes managed Preview operations per terminal, protects snapshots with operation and generation ownership, rehydrates rejected frontend starts from the authoritative backend snapshot, bounds static request and response work, validates exact loopback Host headers, and serves files through capability-relative no-follow handles.
Commit `9005117` packaged that source as `0.3.85`; its native Windows focused Preview suite passed 23 tests, including directory-junction rejection, and its standalone executable, NSIS, and MSI SHA-256 values are `870E71B240B13675F2F717EB786C97F91FDABBD5BFD89E0504E43B2D9E87624D`, `B227A48FDEEF8589E629CA286898FE42ECFCA336C860AE177AC17A6C02A5E121`, and `81A38DB62EE8A68F43DD3F5C3473BC16044F28BF6B4B225FD85630F7315DA454`.
That `0.3.85` artifact was not installed because native compilation exposed two platform-specific test warnings.
Source commit `d05073d` removes those warnings, and `5ea945c` packages the result as installed `0.3.86`.
The exact `0.3.86` source passed 621 Linux Rust library tests with one ignored, warning-denied Clippy, the production frontend build, and 23 focused native Windows Preview tests without warnings.
Packaged `0.3.86` discovered exactly one typed static target, enforced MIME, method, security-header, traversal, hidden-path, symlink, size, and exact Host rules, preserved unique run ownership across restart and stale Stop, and cleared its authoritative URL on final Stop.
While one terminal had a nonreading 16 MiB response, its Stop completed in 206 milliseconds and a second terminal independently reached `running` with a distinct run ID and URL.
After the final normal relaunch, only the application control listener remained and the six tmux sessions present before installation retained the same names and pane PIDs.
The previously recorded session `th_a486c7fc` was already absent before installation, so it is not claimed as preserved by this acceptance run.
Generic non-Tauri Vite launch adapters and stale WSL-address recovery remain open.
Source commit `776439a`, packaged as `0.3.78` from `b4a1c5d`, removes full tmux capture replay from terminal attachment and stops clearing inline transcript during header Refresh.
The packaged acceptance preserved all eight tmux pane PIDs and the same Codex process chain through install, header Refresh, and full relaunch while the active draft appeared exactly once.
The Codex header identity has been checked interactively, while the Claude header still needs interactive confirmation.

The durable inbox substrate implements persistence, ordering, priorities, receipts, crash recovery, sender attribution, and role-based access controls.
It is not yet a complete Captain and Crew communication product.
Agent-to-agent send is not exposed through the normal CLI or MCP catalogs.
Generic delivery, receive, acknowledgement, message history, and frontend visibility remain incomplete.

Claude currently has the strongest T-Hub integration through fifteen lifecycle hooks and a structured status-line bridge.
Codex has a current lifecycle-hook framework, but T-Hub has not integrated it.
Interactive Codex therefore lacks dependable context, supervision, attention, voice, and History parity.

The Windows and WSL TTS endpoints are healthy on ports `7477` and `7478`.
Voice settings are enabled with Kokoro selected and attention announcements enabled.
Automatic spoken announcements currently depend on needs-input status transitions, which interactive Codex does not reliably produce.

The installed `th` CLI reports version `0.2.0`.
Source commit `07e74f4` upgrades the control protocol and recovers from a stale inherited endpoint after application port rotation while preserving the caller's token capability.
Live `health` and `ls` checks now rediscover the packaged application promptly in the normal case.
One post-install `ls` call still hit the bounded 10-second WSL command timeout and succeeded on retry, so transient WSL command latency remains Phase 1 work.
The source documents a newer interface, but the CLI still lacks most Captain, Powder, Workspace, resource, and inbox commands.
The source CLI already has a useful Rust control-client boundary, a no-argument fleet view, deterministic human rendering, a stable JSON envelope, and an established exit-code taxonomy that should be preserved.
The CLI contract audit also found that unknown flags can be accepted silently, per-subcommand help is absent, `worktree rm` has no explicit confirmation gate, some diagnostics can leak into JSON-mode stderr without a structured suggestion, and JSON collections are described as unbounded.
The project-specific target is now defined in `docs/cli-contract.md` and intentionally uses stable JSON without an AXI dependency or a claim of AXI compliance.
Tile headers use authoritative Git branch and dirty data, but the Worktrees dialog exposes only branch, path, and main or linked state.
Recent, Captain, and Workspace rows still contain folder-name worktree heuristics that can disagree with authoritative Git state.
The shared status indicator also combines agent work with terminal lifecycle in some surfaces and can replace exact agent status with a generic terminal tooltip.

## Critical Path

The release critical path maps directly to the numbered phases:

1. Terminal and control reliability.
2. Owned-resource lifecycle safety.
3. Durable identity and organizational model.
4. Provider-agnostic Harness integration.
5. CLI-first control plane.
6. Durable inbox and agent communication.
7. Codebase and Captain creation.
8. Single-Captain safety acceptance followed by full multi-Captain and cross-Harness acceptance.
9. Primary product surfaces.
10. Cortana operations, context, voice, and notifications.
11. Measured runtime efficiency.
12. Security, release, documentation, and production acceptance.

## Phase 1 - Terminal and Control Reliability

**Status:** The original xterm lifecycle gate is complete through installed `0.3.78` from source `b4a1c5d`; follow-on T12 control framing, T13 installed IPC and scan stability, T29 health accuracy, and T30 control-client recovery work is active.
Installed `0.3.67` reproduced the end-user xterm lifecycle failure as `Cannot set properties of undefined (setting 'isWrapped')` during parser line feed.
Installed `0.3.69` then reproduced `Cannot read properties of undefined (reading 'replaceCells')` during rapid Workspace switching and zoom-driven resize.
Source commits `cfa4139` and `cbc558b` serialize resize behind accepted xterm writes and leave xterm's parser callback stack before buffer mutation.
Source commit `1e005e6` converts matching backend detach races into liveness-checked reattachment instead of unhandled `no live terminal` rejections.
One warm stress pass and two cold restart passes on installed `0.3.71` preserved the same eight tmux session IDs and produced zero xterm corruption, detach-race, or unhandled errors.
Duplicate launch retained one Windows PID, `th ls --json` returned all eight sessions after every restart, and diagnostic retention remained within its configured bound.
Installed `0.3.77` reproduced a second user-visible lifecycle regression: cold rehydration replayed a linearized tmux capture and then streamed the attach client's current redraw, visually duplicating an entire Codex frame and its composer draft.
The same path could clear inline transcript during a width-changing header Refresh before the asynchronous backend redraw completed.
Source commit `776439a` makes the attached tmux client the only current-screen renderer and removes the resize-time transcript clear.
Installed `0.3.78` restored the same Codex transcript and one visible draft after header Refresh and full application relaunch, with all eight tmux sessions and the exact Codex process chain unchanged.

### Goal

Make the installed terminal cockpit and its control clients trustworthy before expanding orchestration.

### Work

1. Reproduce the xterm failures through packaged Windows end-user actions.
2. Exercise rapid Workspace switching, terminal parking, warm expiration, cold disposal, restoration, resize, fullscreen, pop-out, and application restart.
3. Correlate each exception with lifecycle state, terminal ID, slot ownership, queued output, replay, resize, and CanvasAddon state.
4. Fix disposal, write, replay, resize, and renderer ordering so callbacks cannot reach disposed state.
5. Preserve subscribe-before-attach and authoritative tmux replay boundaries.
6. Add bounded diagnostic log rotation and retention.
7. Reduce repetitive diagnostics while preserving failure evidence.
8. Upgrade and install the current `th` CLI.
9. Add endpoint rediscovery, stale-pin recovery, bounded timeouts, and non-hanging failure behavior to `th`.
10. Verify that T-Hub owns and closes its Windows supervisor process tree safely.

### Tests and Evidence

- Add a regression test that reproduces each xterm race before implementing its fix.
- Add delayed-output and rapid hot to cold to hot lifecycle tests.
- Add CLI restart, stale endpoint, timeout, protocol mismatch, and malformed-response tests.
- Run frontend tests, Rust workspace tests, TypeScript, formatting, Clippy with warnings denied, and a production frontend build.
- Run packaged Windows interaction tests with at least five terminals.

### Exit Gate

- No `loadCell`, `isWrapped`, blank-canvas, duplicate-attach, or stale-slot failure appears in packaged testing.
- Terminal input works immediately after cold restoration.
- Scrollback and the current prompt match authoritative tmux state after restoration.
- Five terminals survive application restart and remain correctly labeled.
- `th health`, `th ls`, and a mutation-denial test return promptly against live and restarted applications.
- Diagnostic logs remain within the configured retention bound.

## Phase 2 - Unified Owned-Resource Lifecycle

**Status:** Active after Phase 1; managed development-server ownership is partially implemented and packaged `0.3.90` verifies that worktree removal is suspended fail closed, while the unified resource record, browser lifecycle, worktree status service, Resources surface, startup reconciliation, and full exit gate remain open.
The unified worktree snapshot's Captain, Assignment, Workspace, Crew, resource-lease, and Powder ownership fields depend on the Phase 3 B1 durable identity interfaces, so independent Phase 2 work may proceed but the full worktree slice cannot exit before B1 stabilizes.
Installed `0.3.86` reproduced the owned-resource failure by deleting a disposable linked worktree while a live tmux session remained rooted inside it, leaving that pane at a `(deleted)` cwd.
Source `0.3.90` now refuses graphical, direct Tauri, control, MCP, and CLI removal before UI detachment or Git mutation, including with force.
The exact detached `0.3.88` native Windows suite proved the refusal but exposed a test fixture that mixed native Windows Git registration with the production WSL Git removal path.
Commit `f62f188` replaces that fixture with the real production WSL path boundary, and `2c6a429` bumps the corrected source to `0.3.89` under the every-change version policy.
The exact detached `0.3.89` suite then proved the WSL Git fixture creation and public refusal but exposed a Windows UNC access denial in its host-side existence assertion for a mounted-drive fixture.
Commit `3841c2e` checks that same fixture through its retained native host path without changing any production WSL Git operation, and `e26fe2e` bumps the corrected source to `0.3.90`.
The exact detached `0.3.90` native Windows suite passed all four focused removal tests.
Installed `0.3.90` then verified the graphical and direct Tauri preflight, normal, and forced paths against disposable live worktrees: every operation returned the exact temporary-unavailable refusal before UI detachment or Git mutation, the graphical tile remained present, Git registration remained intact, and both live tmux pane paths remained valid.
Source tests cover the same synchronous refusal for control, MCP, and CLI callers; the installed read-only session did not elevate itself with raw credentials to repeat those mutation channels at runtime.
This is a temporary suspension rather than implementation of the unified worktree status service, so full service activation and acceptance remain open.

### Goal

Prevent terminals, browsers, development servers, worktrees, and Powder claims from outliving useful owners without destroying recoverable work.

### Work

1. Define one resource record for terminals, Crew, browsers, development servers, worktrees, Powder claims, temporary profiles, and Windows subprocess trees.
2. Record the owner identity, Project, Assignment, Captain, Workspace, Crew member, Powder card, process root, creation time, last activity, lease, and cleanup state.
3. Route browser creation through a managed T-Hub operation instead of untracked `agent-browser` daemons.
4. Reuse one browser per active verification owner when isolation is unnecessary.
5. Guarantee normal browser and development-server closure through owned cleanup paths.
6. Renew leases only while both owner and resource remain live.
7. Mark resources orphaned when owners disappear and expose a visible grace period before cleanup.
8. Terminate registered process trees gracefully and escalate after a bounded timeout.
9. Remove temporary profiles only after their processes exit.
10. Never target ordinary user Chrome processes.
11. Manage Windows Lighthouse and preview processes with the same ownership contract.
12. Keep Captain and Crew records recoverable until landed-work and claim-release checks pass.
13. Add a Resources view with owner, state, age, activity, and proposed cleanup effect.
14. Add a reviewed **Clean orphaned resources** action.
15. Reconcile owned resources at T-Hub startup and after WSL restart.
16. Implement the unified worktree status service and safety decisions defined in `docs/WORKTREE-STATUS-CONTRACT.md`.

### Tests and Evidence

- Run one hundred managed browser start and stop cycles.
- Kill browser clients, Crew terminals, T-Hub, and the WSL bridge at controlled points.
- Verify that dirty, unmerged, or leased worktrees are never automatically removed.
- Verify that Powder claims release only after confirmed terminal shutdown.
- Verify process ownership against Windows and WSL operating-system evidence.
- Verify that backend, CLI, MCP, and graphical surfaces return equivalent worktree identity, ownership, freshness, and safety decisions.
- Verify that graphical, Tauri, control, MCP, and CLI removal paths all fail closed before UI detachment or Git mutation while the unified service is unavailable, including with force.
- After the unified service exists, verify main, dirty, locked, terminal-owned, leased, claimed, stale, and unknown decisions plus preflight-to-mutation serialization before re-enabling removal.

### Exit Gate

- The browser cycle returns to the original process count.
- Orphaned registered resources disappear after the documented grace period.
- Ordinary Chrome remains untouched.
- Failed claim release remains visible and recoverable.
- The Resources view agrees with operating-system evidence.
- Dirty, leased, main, locked, stale, and unknown worktrees cannot be automatically removed or reused.

## Phase 3 - Durable Identity and Organizational Model

**Status:** Earliest unblocked foundational phase; B1 Assignment identity is required before full Phase 2 ownership activation and the Phase 8B multi-Captain exit gate.

### Goal

Implement permanent Cortana identity, multiple Captains per Project, Assignment ownership, and correct Workspace semantics.

### Work

1. Separate Cortana identity from its current terminal, Harness, Provider, model, and conversation.
2. Auto-recover or recreate Cortana's runtime while preserving its durable identity and last safe checkpoint.
3. Allow explicit Cortana Harness and model changes through a reviewed operation.
4. Replace the one-live-Captain-per-Project registry constraint with multiple durable Assignments per Project.
5. Give each Captain a durable Assignment identity independent of its terminal and provider conversation.
6. Allow a Captain to control zero, one, or several Workspaces.
7. Treat Workspaces as coherent workstreams rather than Project, Captain, or Crew synonyms.
8. Allow Captains to create, name, rename, close, and reconcile their Workspaces.
9. Allow Captains to assign related Crew and worktrees to a Workspace.
10. Keep Captain identity alive after its final Workspace or Crew closes.
11. Add explicit checkpoint, context reset, recovery, and retirement state machines.
12. Implement the settled Cortana retirement policy with cleanup safety gates.
13. Display role, Assignment, Project, Harness, model, context, and unrestricted authority clearly.
14. Migrate legacy pinned and commissioned records without silently granting authority.
15. Define a transitional durable unbound-Captain state that preserves exact ship, terminal, Harness, and session-generation identity without granting Project authority.
16. Allow that state to consume only an exact T34 bootstrap grant and bind one Project and Assignment through T35.
17. Reject stale or replacement terminal generations, conflicting ships or Assignments, and foreign or ambiguous targets without existence leakage.

### Tests and Evidence

- Add registry migration tests from every supported previous schema.
- Commission two Captains in the same Project with distinct Assignments.
- Reset context and replace the Harness runtime without changing durable Captain identity.
- Kill and recover Cortana without creating a second Cortana identity.
- Verify that an empty Workspace does not retire its Captain.
- Verify that idleness and context pressure cannot trigger retirement.
- Verify retirement fails while unsafe Crew, claims, worktrees, browsers, or servers remain.

### Exit Gate

- One Project safely supports multiple live Captains.
- Cortana survives runtime replacement as one permanent identity.
- Captain, Assignment, Workspace, Crew, and Project records remain distinct and understandable.
- Recovery and retirement behavior match the settled policy.

## Phase 4 - Provider-Agnostic Harness Integration

**Status:** Partially unblocked for adapter fixtures and fail-closed Codex auto-continue work; durable identity binding depends on Phase 3 B1.

### Goal

Give Codex, Claude Code, and future Harness adapters one normalized lifecycle contract.

### Normalized Adapter Contract

Each Harness adapter must define:

- Installation and version detection.
- Authentication and readiness checks.
- Start, resume, interrupt, checkpoint, reset, and recover operations.
- Provider session and conversation identity.
- Model and reasoning configuration.
- Permission mode and visible effective authority.
- Turn lifecycle and structured failures.
- Context telemetry.
- Provider-limit telemetry.
- Provider-limit auto-continue scheduling, cancellation, and recovery.
- Tool, task, and subagent lifecycle where available.
- History discovery and resume metadata.
- Hook installation, trust, health, repair, and removal.
- Capability flags for features the provider cannot supply.
- Authoritative and derived inputs for both axes in `docs/STATUS-MODEL.md`.

### Normalized Events

Adapters should map provider events into:

- `session.started`
- `session.ended`
- `turn.started`
- `turn.completed`
- `turn.failed`
- `input.requested`
- `permission.requested`
- `tool.started`
- `tool.completed`
- `context.compacting`
- `context.compacted`
- `subagent.started`
- `subagent.completed`
- `task.created`
- `task.completed`
- `cwd.changed`
- `worktree.created`
- `worktree.removed`

### Work

1. Move Claude-specific supervision assumptions behind the adapter boundary.
2. Integrate current Codex lifecycle hooks with `t-hub-agent`.
3. Add structured telemetry for interactive Codex sessions rather than relying on output activity.
4. Bind Codex thread IDs and Claude session IDs to durable T-Hub identities.
5. Add Codex context telemetry for the outer tile, sidebar, Cortana health, and reset recommendations.
6. Enable and verify the native Codex status line.
7. Apply unrestricted flags to fresh and resumed interactive Codex and Claude sessions.
8. Keep an Advanced override without burdening the normal commissioning flow.
9. Replace **Claude hooks** settings with **Agent integrations**.
10. Show each adapter's installed version, hooks, telemetry, History, permissions, and degraded capabilities.
11. Design the registry so a GLM-compatible adapter can be added without changing History or organizational schemas.
12. Implement Codex auto-continue after provider-limit reset by preserving the exact thread, pending continuation, reset time, and durable Captain or Crew identity.
13. Deduplicate scheduled continuation across app restarts, provider retries, repeated limit events, and simultaneous frontend clients.
14. Allow the General, Captain, or owning Crew policy to cancel or disable a pending continuation before it runs.
15. Replace provider-specific or terminal-output status inference with the two-axis work-state and runtime-health model.

### Tests and Evidence

- Add adapter contract tests that run against Codex and Claude fixtures.
- Add real interactive start, resume, input request, completion, failure, compaction, and context tests where each provider exposes the event.
- Add explicit degraded-capability tests where one provider lacks an event.
- Verify hook trust and repair behavior without overwriting user-authored hooks.
- Verify provider switching preserves T-Hub identity but never mixes incompatible conversation IDs.
- Verify authoritative, derived, stale, unknown, and conflicting status observations without fabricating unsupported provider events.
- Test Codex auto-continue with real and fixture limit events, exact reset-time scheduling, early retry backoff, app restart, duplicate events, cancellation, missing threads, and already-completed work.
- Verify auto-continue never submits a continuation to a different thread, retired identity, closed Assignment, or manually stopped session.

### Exit Gate

- Codex and Claude both drive dependable working, needs-input, completed, and failed states.
- Both Harnesses expose effective permission mode and provider identity.
- Codex context and resume identity are visible and recoverable.
- Codex auto-continue resumes the exact limited thread once after the provider window resets, or reports a clear recoverable failure.
- A future adapter can implement the normalized contract without changing Project, Captain, Workspace, History, or inbox schemas.
- Work completion, attention, runtime failure, and recovery remain distinct on every supported Harness.

## Phase 5 - CLI-First Control Plane

**Status:** Active for CLI contract normalization and shared-catalog work; identity-aware expansion depends on Phase 3 B1.

### Goal

Make `th` the canonical token-efficient control interface and keep MCP as an optional adapter.

Normalize the existing Rust CLI against `docs/cli-contract.md` before expanding its command surface.
Preserve the existing control-client architecture, JSON envelope, compatible aliases, and exit-code taxonomy unless a separately reviewed versioned migration requires a change.

### Work

1. Normalize the existing argument parser so every command rejects unknown flags and extra positional arguments before side effects.
2. Add concise per-subcommand `--help` with arguments, flags, defaults, and examples.
3. Preserve the stable `{ ok, command, data, error }` JSON envelope and established `0`, `2`, `3`, `4`, `5`, and `6` exit taxonomy.
4. Extend structured errors compatibly with stable symbolic kinds, actionable suggestions, and bounded optional details.
5. Make empty collections explicit, ordering deterministic, and human and JSON output bounded with totals plus `--all` or `--full` escape hatches.
6. Require `--confirm` before destructive effects, retain `--yes` only as a temporary compatibility alias where it already exists, and add `--dry-run` where practical.
7. Define one shared command catalog and schema source for the control server, CLI, and MCP adapter.
8. Add CLI groups for fleet, Projects, Captains, Crew, Workspaces, resources, Powder, History, inbox, context, provider limits, recovery, and retirement.
9. Preserve per-session identity, role, Project, and ownership checks through CLI calls.
10. Add idempotency keys and request-status recovery to every retryable mutation.
11. Add bounded waits and event subscriptions instead of encouraging polling loops.
12. Filter MCP tool exposure by role and capability.
13. Keep MCP for typed clients while avoiding a forty-tool schema burden in every agent context.
14. Add concise agent instructions and command help so agents discover CLI syntax on demand.
15. Ensure CLI and MCP return equivalent results for the same backend operation.
16. Consider `th capabilities --json` only after the expanded catalog makes capability discovery worth its maintenance cost.
17. Make worktree commands consume the unified backend snapshot rather than maintaining separate Git safety logic in the CLI.
18. Include repeated criterion-proof syntax, Powder operation identities, and ambiguous-response status lookup in the shared catalog.
19. Generate actor-visible CLI and MCP catalogs from one descriptor source rather than maintaining separate hard-coded command and tool lists.
20. Report unsupported or unavailable host-health fields honestly rather than returning an all-zero healthy WSL skeleton.
21. Use one bounded control-client deadline with prompt stale-endpoint recovery and structured timeout evidence.
22. Extend each shared operation descriptor with its broad capability, absolute role denials, grant class, exact constraint vocabulary, external dependencies, provider support, and stable explanation kinds.
23. Return actor-visible capability discovery that distinguishes granted, grant required, one-time approval required, temporarily unavailable, dependency blocked, provider unsupported, and absolute denial without foreign or credential detail.

### Tests and Evidence

- Add process-level contract tests for JSON isolation, strict flags and arguments, empty results, exit categories, no-ops, destructive confirmation, deterministic ordering, truncation, and `--full` behavior.
- Add parity tests that execute each shared operation through CLI and MCP.
- Add authorization tests for General, Cortana, Captain, Crew, read-only, and trusted-host callers.
- Measure prompt and tool-schema token overhead before and after role filtering.
- Test restart, timeout, retry, idempotency, and ambiguous-response recovery.
- Prefer structural JSON assertions and use exact-output snapshots only for a small set of intentionally reviewed public contracts.

### Exit Gate

- An agent can operate its allowed T-Hub workflow through `th` without MCP.
- MCP remains functional without defining separate behavior.
- CLI and MCP cannot bypass each other's authorization or identity rules.
- The reduced tool surface demonstrates lower context overhead.
- Unknown input fails before side effects, destructive actions require explicit confirmation, and all supported JSON output remains bounded, parseable, and compatible.

## Phase 6 - Durable Inbox and Agent Communication

**Status:** Durable substrate exists, while product activation is blocked on Phase 3 identity, T9 and T10 durability and bounds, T11 shared-catalog parity, and integration of the messaging contract.

### Goal

Complete a visible, recoverable communication layer for General, Cortana, Captains, and Crew.
Implement the authority and channel boundaries in [AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md](./AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md) without turning Powder into a chat transport or terminal typing into a durable acknowledgement path.

### Work

1. Re-key recipients from temporary terminal IDs to durable role identities with terminal delivery bindings.
2. Expose send, list, read, reply, acknowledge, accept, decline, complete, retry, cancel, supersede, typed approval request, approval decision, approval cancellation, and approval status operations through CLI and MCP.
3. Drain messages at safe provider turn boundaries for every supported recipient role.
4. Add an automatic receive and acknowledgement loop to each Harness adapter.
5. Preserve natural-language bodies alongside structured message types.
6. Support instruction, status, blocker, decision, completion, lifecycle, and coordination messages.
7. Link messages to Projects, Assignments, Workspaces, Crew, and Powder cards where applicable.
8. Implement enqueued, delivering, delivered, read, accepted, declined, completed, failed, retrying, expired, cancelled, and superseded states with authorized transitions.
9. Retain human-readable message history after transport queue compaction.
10. Add unread badges and an on-demand Messages timeline.
11. Allow Captain-to-Captain communication without granting terminal, Crew, or retirement authority.
12. Label cross-Project peer messages clearly.
13. Require transferred implementation work to receive an explicit Assignment or Powder card when ownership changes materially.
14. Add secret redaction and bounded retention controls.
15. Treat Powder as the executable backlog and evidence ledger while linking messages to exact cards and runs when dialogue is required.
16. Make `send_text` an inspected compatibility operation whose acceptance cannot be mistaken for composer submission, recipient acknowledgement, or work completion.
17. Provide event-driven Captain wake-up for blocker, decision, permission, review, completion, runtime failure, and recovery transitions without periodic model polling.
18. Implement the contract's sender-recipient authorization matrix, message transition state machine, typed assignment acknowledgement, and typed approval decisions.
19. Make every inbox mutation fail atomically when durable create, write, flush, rename, reopen, or recovery work fails.
20. Bound UTF-8 body size, standard capacity, Emergency capacity, retention, retry growth, and telemetry while preserving explicit backpressure and overflow states.
21. Prevent any persistence failure from returning an acknowledged delivery, read, acceptance, completion, or approval transition.
22. Reuse the typed approval lifecycle for T34 exact one-time grants, including exact arguments, expiry, cancellation, atomic consumption, and stable operation identity.
23. Add request, issue, list-effective, explain, revoke, use, and expiry communication surfaces for T34 standing grants without treating inbox text as authorization.
24. Preserve strict-subset delegation and make parent revocation deterministically invalidate or suspend every delegated child before any further use, including across restart and concurrent consumption, while keeping grant evaluation and durable grant state inside T34 rather than the inbox implementation.

### Recommended Retention Default

Use thirty days as the provisional recommended default for local message bodies until the General resolves the retention setting.
Keep non-secret delivery metadata longer for recovery and audit.
Keep user-pinned messages until explicitly removed.

### Tests and Evidence

- Test crash recovery across enqueue, delivery, acknowledgement, completion, failure, retry, expiry, cancellation, and supersession transitions.
- Test ordering, priorities, overflow, duplicate acknowledgement, and idempotent reply behavior.
- Test every permitted and denied role pair by message class, Project, Assignment, ship, card, and run relationship.
- Test terminal replacement while messages remain queued.
- Test body redaction and verify that event telemetry does not expose message content implicitly.
- Run packaged Captain-to-Crew, Crew-to-Captain, Cortana-to-Captain, and Captain-to-Captain conversations.

### Exit Gate

- Messages survive application and terminal restarts.
- Each role receives messages only through permitted routes.
- The General can inspect message content and lifecycle without terminal scrollback.
- Peer communication cannot mutate another Captain's authority or resources.

## Phase 7 - Codebase and Captain Creation

**Status:** Active for partial existing-codebase and recovery work; its complete exit depends on Phase 3 B1, T18, T5, T6, and P5 for automatic board creation.

### Goal

Make Captain creation understandable for saved, existing, and completely new codebases.

### Work

1. Replace **Registered project** with **Saved codebase** in user-facing copy.
2. Replace **Register repository** with **Choose existing codebase**.
3. Rename **Powder repository** to **Powder board** or **Work board**.
4. Move protected connection-profile selection under **Advanced** and default it when unambiguous.
5. Add saved codebase, existing WSL folder, and new codebase entry paths.
6. Build a WSL-native folder picker with home and recent shortcuts, breadcrumbs, parent navigation, Git indicators, and manual-path fallback.
7. Detect the canonical main worktree, remote, default branch, dirty state, and existing worktrees.
8. Use the unified worktree status contract for preflight identity, ownership, freshness, and safety decisions.
9. Offer explicit Git initialization for non-repository folders.
10. Add a reviewed new-codebase transaction for empty projects, templates, and clones.
11. Never silently replace a directory or initialize version control.
12. Add Powder board selection and explicit creation when Powder authorization permits it.
13. Add a preflight summary for filesystem changes, Git state, Powder, Assignment, Harness, model, permissions, and external effects.
14. Use the same backend transaction for graphical and Cortana conversational flows.
15. Commission the Captain identity without forcing creation of an unrelated work Workspace.
16. Offer creation of an initial Workspace when the Assignment already names a coherent workstream.
17. Roll back incomplete state while preserving pre-existing directories and useful work.
18. Use one stable operation ID across register, bind, commission, response loss, restart, retry, and recovery.
19. Prevent double-click, backdrop dismissal, refresh, or timeout from creating duplicate Projects, Captains, terminals, or Powder bindings.
20. Recommend and auto-bind only an exact canonical repository match, and keep unrelated Powder boards behind an Advanced path.
21. Expose safe Captain card authoring through the shared operation catalog rather than requiring the General to create routine implementation cards manually.
22. Add the T35 existing-repository Captain self-bootstrap path for one live control-capable unbound Captain with an exact T34 bootstrap grant.
23. Preserve the live Appturnity `register_project` ACL denial as the first installed reproduction and keep Appturnity Crew undispatched until its Project, Captain, Assignment, profile, and board link are authoritative.
24. Resolve subdirectory and linked-worktree input to one canonical main-worktree identity, serialize by that identity, and reject conflicting or ambiguous registrations before mutation.
25. Bind only an existing protected-board match in the first T35 slice, with no board creation and no caller-supplied endpoint, credential, or arbitrary profile.
26. Expose explicit pending, resumable, completed, conflicted, and non-destructive rollback states for a Project preserved after a later attachment or checkpoint failure.

Phase 7 remains active.
Phase 7 item 8 depends on the Phase 2 unified worktree status service, whose durable ownership fields consume the Phase 3 B1 identity interfaces.
The multiple-Captain exit gate also depends on Phase 3 B1 replacing the one-live-Captain-per-Project constraint with durable Assignment identity.
Product-flow work may proceed only against stable shared contracts.
Items 1 through 7, 9, 11, and the existing-codebase portions of 13 through 15 are implemented.
Installed `0.3.72` now launches commissioned Codex and Claude Captains with explicit unrestricted permission flags, and its packaged review screen reports that authority as `Unrestricted`.
Installed `0.3.73` discovers visible canonical boards through the protected Powder profile, exposes bounded pagination through the shared control and MCP operation, and replaces free-text board entry with an accessible selection flow.
Packaged verification listed all 25 real boards from `n8desktop-wsl`, including `t-hub` with its one acceptance card, and preserved the selection in preflight.
Installed `0.3.74` adds a reviewed **Create new codebase** choice for one absent empty-codebase leaf, initializes Git with `main`, and reports the exact filesystem and external effects before creation.
Packaged cancel verification reviewed `/home/natkins/t-hub-cancel-proof-0-3-74`, closed the dialog, and confirmed that no directory was created.
Installed `0.3.86` reproduced the open template and clone gap: **Create new codebase** offers only **Starting point: Empty Git repository** and states that template and clone starting points will be added later.
The graphical flow currently sequences `register_project` and `commission_captain` in the frontend.
Registration transactionally owns its optional directory, Git initialization, Project record, and selected Powder binding, while commissioning separately rolls back only incomplete Captain state.
One reviewed backend transaction shared by graphical and Cortana flows, including explicit cross-operation resume or rollback, remains open.
The complete graphical packaged E2E matrix for existing non-Git success, empty success, template, and clone flows remains open.
The shared registration contract now requires `initializeGit: true` before it changes a non-repository folder, and its Rust integration tests cover success, downstream-failure rollback, pre-existing-file preservation, and refusal to rewrite a pre-existing `.git` entry.
Automatic Powder board creation for a new codebase is blocked by Powder's current repository API contract and P5.
`POST /api/v1/repositories` is an upsert without a create-only or conditional precondition, so a T-Hub read-then-create sequence could overwrite a board created concurrently by another actor.
T-Hub must fail closed rather than use that upsert as create-if-absent, and Powder must not be modified merely to accommodate T-Hub.
This dependency can unblock only when Powder independently exposes the generic versioned non-overwriting create contract or equivalent server-enforced precondition defined by P5.
The current `n8desktop-wsl` profile also has no separately configured repository-admin credential, so positive packaged creation acceptance remains open even after the API dependency is resolved.
The compact new-codebase flow must retain one reviewed operation identity across ambiguous retries and provide an actionable resume path from a Project preserved after a later Powder or binding failure.

### Tests and Evidence

- Add packaged E2E for saved, existing Git, existing non-Git, empty, template, and clone flows.
- Inject failure at every transaction boundary and verify safe resume or rollback.
- Test two Captains commissioned into the same Project.
- Test cancel behavior without residual directories, Project records, boards, or terminals.
- Test graphical and conversational parity.

### Exit Gate

- The user can create a Captain without understanding internal registry or credential-profile details.
- Multiple Captains in one Project receive distinct Assignments.
- No unwanted Workspace is created merely because a Captain exists.
- Preflight and rollback behavior remain understandable at every boundary.

## Phase 8A - Single-Captain Powder Stage 1 Acceptance

**Status:** Active and blocked from shipment on P1 through P4, T1 through T3, and T17.
Installed `0.3.103` passed Project binding, Captain control capability, WSL cwd, Crew dispatch, durable binding, exact checkout, live Codex Harness, claim, heartbeat, release, rollback, and cleanup checks.
The Stage 1 retry correctly withheld its sentinel because T-Hub has no sanctioned operation to append or read attributed work-log evidence or complete the Crew card with proof.
The next Phase 8A implementation may add bounded evidence reads and the fail-closed T-Hub surfaces, but it must not ship card-only automated completion.
The staged completion implementation verifies an exact run and then sends a card-only completion mutation, which creates a destructive time-of-check/time-of-use race if another run claims the card before Powder applies the mutation.
Phase 8A requires a generic Powder run-bound conditional completion contract, retry-safe work logs, current-run criterion semantics, one per-Crew lifecycle guard, safe ambiguous-response recovery, and authoritative Captain and Crew terminal state.

### Goal

Prove one complete installed Codex Captain and Crew lifecycle against the local Powder authority without claiming full multi-Captain or cross-Harness acceptance.

### Work

1. Reconcile or retire legacy pinned Captain records without losing live terminal state.
2. Register the T-Hub codebase and bind it to the canonical `t-hub` Powder board through `n8desktop-wsl`.
3. Commission one Codex Captain with one bounded Assignment.
4. Create or select one real acceptance card and dispatch exactly one Codex Crew into one isolated Workspace and worktree.
5. Verify checkout, worktree, card ownership, claim acquisition, Harness launch, sidebar visibility, heartbeat, and renewal.
6. Append and read bounded attributed work-log evidence through a retry-safe current-run operation.
7. Complete only through server-enforced card and run preconditions with operation identity, current-run criterion proof, and bounded completion evidence.
8. Serialize completion, renewal, heartbeat, release, close, cleanup, and reconciler actions for the Crew.
9. Verify terminal close, claim release, run release, Crew terminal state, owned-resource cleanup, and safe worktree retention.
10. Remove an acceptance worktree only through T18, or retain it non-destructively with exact ownership and cleanup status when T18 is unavailable.
11. Verify timeout, lost response, reclaimed card, stale run, redacted evidence, duplicate retry, and incomplete-dispatch rollback boundaries.
12. Update the handoff with exact installed source, operation IDs, card, run, work log, proof, terminal state, and cleanup evidence.

### Tests and Evidence

- Use real Powder cards rather than mocks for final acceptance.
- Keep deterministic mocked tests for each failure boundary.
- Use barrier-controlled concurrency tests to prove an old run cannot complete, renew, release, or append evidence after a new run owns the card.
- Capture Project, Captain, Workspace, Crew, terminal, claim, run, work-log, criterion, completion, and cleanup evidence before and after acceptance.
- Verify no raw key appears in logs, prompts, project state, message history, or documentation.

### Exit Gate

- One installed Codex Captain commissions, supervises, recovers, and returns to an authoritative waiting or completed state.
- One Codex Crew claims, works, logs evidence, completes the exact current run, and closes cleanly.
- Run A cannot mutate, complete, release, renew, or attach evidence to run B after a release, expiry, or reclaim boundary.
- Powder and T-Hub agree on the exact card, run, claim, work log, criterion proof, completion proof, terminal state, and cleanup outcome.
- No stale claim, active run, or disposable terminal remains after cleanup.
- An owned acceptance worktree is removed only through T18; otherwise it remains safely retained with an explicit non-cleaned status and resume path.

## Phase 8B - Multi-Captain and Cross-Harness Acceptance

**Status:** Blocked on Phase 8A, Phases 2 through 6, and the T7 through T11 identity, routing, inbox, and shared-catalog gates.

### Goal

Prove the complete multi-Captain, multi-Workspace, Codex, Claude, messaging, recovery, retirement, and owned-resource workflow after the single-Captain safety gate passes.

### Work

1. Commission Codex and Claude Captains with distinct Assignments in the same Project.
2. Verify both terminal headers and durable Harness identities interactively.
3. Create real acceptance cards and dispatch Codex and Claude Crew into deliberate Workspaces.
4. Exercise Captain-to-Captain and Captain-to-Crew messaging around a real dependency.
5. Route Powder card and run events only to the exact owning Captain or Crew.
6. Verify cursor advancement, replay idempotency, conflict visibility, and board filtering.
7. Verify Captain context reset, Cortana recovery, T-Hub restart, WSL restart, terminal replacement, and durable bootstrap recovery.
8. Verify explicit retirement, terminal close, claim and run release, Crew state, owned-resource cleanup, and safe worktree retention.
9. Verify incomplete dispatch and partial recovery rollback at every failure boundary.
10. Clean disposable acceptance state through the owned-resource workflow.

### Tests and Evidence

- Use real Codex and Claude Harnesses plus real Powder cards for final packaged acceptance.
- Retain deterministic identity, routing, messaging, recovery, and cleanup failure fixtures.
- Capture Assignment, Workspace, Crew, terminal, card, run, message, lifecycle, and owned-resource evidence before and after cleanup.
- Prove that peer messages grant no foreign Crew, terminal, completion, or retirement authority.

### Exit Gate

- Both Harnesses commission, recover, supervise, message, and retire cleanly.
- Both Crew Harnesses claim, work, report, and close cleanly.
- Multiple Captains coexist in one Project without identity, event-routing, inbox, Workspace, or resource collisions.
- Powder and T-Hub agree on cards, runs, claims, terminals, messages, and cleanup outcomes.

## Phase 9 - Primary Product Surfaces

**Status:** Active for independently reproducible packaged product bugs, while the complete exit depends on the shared identity, Harness, inbox, and resource contracts.

### Goal

Make Board, Run and Preview, Files, History, Provider limits, Messages, Resources, and settings work without hidden setup knowledge.

### Work

1. Resolve Board from the focused Project's Powder binding rather than `http://localhost:4000`.
2. Display clear unbound, unauthorized, unreachable, and framing-blocked states.
3. Avoid credentials in URLs and frontend-persisted state.
4. Preserve external-browser fallback when framing is blocked.
5. Replace Dev then Preview with one **Run and Preview** flow.
6. Detect package scripts, allow command selection, bind a reachable interface, detect the port, and probe Windows reachability.
7. Show startup output, health, URL, stop, restart, ownership, and failure reasons together.
8. Suspend hidden Board and Preview activity without a visible consumer.
9. Reuse the WSL picker for Files roots.
10. Implement or remove the dead `filesRootDir` setting.
11. Replace **Recent** with provider-agnostic **History**.
12. Group History by durable Project, Captain, role, Harness, and conversation.
13. Support resume, recover, archive, and compatibility states for Codex and Claude.
14. Rename **Usage** to **Provider limits**.
15. Keep conversation context, provider limits, and local resource pressure visually distinct.
16. Add Messages and Resources surfaces with compact unread and warning badges.
17. Add Agent integrations settings and effective unrestricted-permission badges.
18. Add per-session Codex and Claude auto-continue controls with pending, scheduled, cancelled, resumed, and failed states.
19. Show the provider reset time, exact target session, and cancellation action without exposing internal credentials or prompts unnecessarily.
20. Add clear Project, Assignment, Captain, Workspace, Crew, worktree, and board labels.
21. Render work state as the primary status and runtime degradation as a separate secondary signal under `docs/STATUS-MODEL.md`.
22. Replace path-derived worktree labels with authoritative branch and worktree identity wherever current state is available.
23. Verify keyboard access, narrow layouts, high DPI, error states, and visual quality.
24. Reproduce the installed Full Board failure and make the primary expansion Project-filtered, native, authenticated, and packaged-testable.
25. Label an external Powder browser action as a global board and provide actionable authentication, launch, focus, and unreachable states.
26. Put Provider limits and History behind singleton stale-while-revalidate services with bounded scans and last-known-data retention.
27. Add a durable reviewed Project run profile and never execute arbitrary chat text as a command.
28. Allow agents and chat to propose typed run profiles only through validation and explicit acceptance.
29. Preserve current usage, History, transcript, and session identity across Refresh, remount, Workspace switching, and transient backend failure.

Board items 1 through 4 and the Board portion of item 8 are implemented in source commit `6c6e4ee` and packaged as installed `0.3.75` from `15ab30f`.
The backend resolves Captain, Crew, or canonical Git main-worktree identity to one registered Project and returns a bounded repository-filtered Powder snapshot without sending credentials to the frontend.
The native read-only surface covers loading, empty, unbound, unauthorized, unreachable, missing-repository, truncated, and generic error states, with retry and an explicitly unfiltered external full-board fallback where applicable.
Packaged verification on the T-Hub tile showed **No registered Project**, zero iframes, and no Board URL input, correcting the reproduced redirect to `http://192.168.0.102:4000/`.
The registered and bound Project success state still requires the Phase 8A real Powder acceptance flow.
Run and Preview item 5 and the Preview portion of item 8 are implemented in source commit `96998fc` and packaged as installed `0.3.76` from `7ced938`.
The packaged T-Hub tile exposed exactly one combined tab and one unified panel containing both managed runner controls and the empty preview state.
The old inspection endpoint remained present in terminal scrollback, but the preview URL stayed empty with zero iframes and no detected-URL chips.
The typed-target and generation-safe lifecycle portions of items 6 and 7 are implemented in source commit `61f56ba` and packaged in installed `0.3.78`.
The packaged T-Hub root exposed exactly `dev`, `build`, `tauri`, and `typecheck`, started the real Vite target, detected `http://localhost:1420/`, and stopped the managed run and its Vite descendant.
Source commits `fbacc8f`, `16480b7`, and `19dc3c7`, packaged as installed `0.3.81` from `8f5fffa`, complete Windows reachability for the representative standard Tauri Vite target.
The installed package kept the Preview iframe on reachable `http://localhost:1420/`, returned Windows HTTP 200, and removed both listeners and the managed Vite PID on Stop.
Source commit `9d95fa9`, packaged as installed `0.3.82` from `ec55526`, owns the complete managed package process group through normal Stop, same-port restart, natural parent exit, TERM-resistant descendants, and forced application exit.
The installed package removed every pnpm, Vite, and esbuild process plus ports `1420` and `1421` during both normal Stop and forced application exit while preserving the seven unrelated tmux sessions.
Installed `0.3.82` passed the representative Next.js target, including Windows HTTP 200, expected rendered sentinels, exact-run Stop, and full npm and Next process-group cleanup.
Source commits `5011803` and `3177d81`, packaged as installed `0.3.84` from `0cd5861`, add and complete the representative package-less static target.
The installed static server bound only Windows `127.0.0.1`, published its authoritative URL without log parsing, auto-loaded and restored the iframe across tab remount, denied traversal, hidden, and symlink requests, removed the iframe and URL on Stop, restarted cleanly, and closed on forced application exit.
Source commit `1484750`, completed by `d05073d` and packaged as installed `0.3.86` from `5ea945c`, hardens concurrent Start and Stop ownership, authoritative rejected-start recovery, static path-race confinement, exact Host validation, bounded request handling, and bounded shutdown.
Packaged acceptance passed the static HTTP and confinement matrix, stale-run ownership, 206-millisecond nonreading-client Stop, independent cross-terminal Start, listener cleanup, and preservation of the six-session pre-install tmux baseline.
The Run and Preview exit-gate requirement for representative Vite, Next.js, and static projects is complete.
Framework-aware generic Vite arguments and stale WSL-address recovery remain open follow-up hardening.
History items 11 through 13 are governed by [HISTORY-CONTRACT.md](./HISTORY-CONTRACT.md).
The existing Recent implementation is Claude-only, keyed and filtered by cwd, archives an entire Claude project transcript directory, and hardcodes Claude resume behavior.
Codex rows must not be added to that legacy contract because doing so would collapse same-cwd conversations and resume them through the wrong Harness.
Source `0.3.97` at commit `4759df0` preserves a partial Codex session window when the same provider snapshot also contains a recognized weekly window.
It also advances the retained authoritative snapshot across an expired reset boundary before merging a later partial poll, so an old session percentage cannot reappear.
The focused regression suite, all 480 frontend tests, TypeScript, the production frontend build, version consistency, diff checks, and independent review passed.
That fix is included in installed `0.3.100` from exact detached source `8635374`.
Source `0.3.98` at commit `4e264f0` adds the backend-only provider-neutral History identity and transcript parser foundation.
It locks exact Harness-plus-conversation digests, preserves same-cwd and cross-Harness separation, selects filename-matching Codex child metadata, reads the real Codex `model_provider`, normalizes valid timestamps to UTC, degrades malformed records, filters wrapper text, and represents legacy Claude archive entries per conversation.
The foundation exposes no command or UI, leaves Claude-only Recent byte-for-byte unchanged, and marks every not-yet-connected action unavailable.
Its 16 focused tests, full Rust workspace, MCP end-to-end, strict all-feature Clippy, all 480 frontend tests, TypeScript, production build, version consistency, diff checks, and independent review passed.
Source `0.3.99` at commit `3afb521` repairs the reproduced Codex Captain commissioning failure.
Installed `0.3.94` passed the exec-only `--skip-git-repo-check` flag to interactive Codex `0.144.4`, which exited immediately, while the graphical control client timed out before the backend returned its structured rollback error.
The adapter now keeps that flag on `codex exec` Crew turns only, and Captain and Crew orchestration receive a bounded 120-second response window without widening ordinary control requests.
Source `0.3.100` at commit `f8ef9aa` addresses the separately reproduced Powder credential process churn.
The 15-second event reconciler now reuses one resolved client per connection profile for five minutes, invalidates it after a Powder request failure, and starts the Windows credential command with `CREATE_NO_WINDOW`.
The registered T-Hub Project and its `n8desktop-wsl` binding remain durable even though the failed Captain terminal was rolled back, so installed acceptance can retry commissioning without registering a duplicate Project.
Both changes passed 663 desktop Rust tests with one ignored, all Rust workspace and MCP end-to-end suites, strict all-feature Clippy, all 480 frontend tests, TypeScript, the production build, version consistency, diff checks, and independent review.
Exact detached source `8635374` produced the standalone executable, NSIS installer, and MSI with SHA-256 values `950F9C91124CAFBB817FF1A0B1EF496615E9B6222FBC1793D1CABC0D2EAEE8AC`, `85AA44A2A30EB4EF45AC5554E35050A08FD84DBFFDACB1CECB753AA657DCDE53`, and `9BBD43D3A951FFB6E845E7D828AB84AB42437C09B877FE6926CB3BEF9D5D9C6E`.
The NSIS upgrade installed `0.3.100` successfully, and the installed executable has SHA-256 `AC7B6169A638F57FF7E6CA699E7016C3C9715C7E968753D654A3C9095CD944F0`.
All five pre-install tmux session names and pane PIDs survived unchanged.
A 50-second process sample spanning more than three Powder event intervals observed zero PowerShell or cmd children owned by T-Hub PID `14868`.
The preserved Project still requires one trusted graphical Create Captain retry before the Captain and Crew acceptance sequence can continue.
The next History slice must add bounded fair discovery, source statuses, collision handling, durable exact joins, complete revision semantics, and only then expose the versioned `history_list` catalog across control, MCP, CLI, and frontend IPC.

### Tests and Evidence

- Add component tests for every empty, loading, degraded, error, and success state.
- Add cross-surface status tests that assert exact labels, tooltips, accessible text, freshness, and worktree identity.
- Add browser E2E for Board and Preview, including iframe fallback.
- Add History resume tests across Codex and Claude.
- Add auto-continue UI tests for opt-in, opt-out, cancellation, scheduled recovery, duplicate events, and failed exact-thread resume.
- Add packaged Full Board browser-launch tests rather than relying on a mocked `shellOpen` component test.
- Add duplicate-scan, last-known-data, refresh, remount, and Workspace-switch tests for Provider limits and History.
- Test run-profile proposal, validation, acceptance, start, restart, port collision, failure, and exact-process cleanup.
- Add accessibility checks and keyboard-only flows.
- Perform packaged pixel review at representative Windows scaling values.

### Exit Gate

- Board opens the correct Project board without manual URL configuration.
- Run and Preview starts and stops representative Vite, Next.js, and static projects.
- Files and Captain creation use the same canonical WSL path semantics.
- History resumes Codex and Claude sessions accurately.
- Codex and Claude auto-continue state is visible, controllable, and bound to the correct session.
- Full Board opens the intended Project context or clearly labels the external global Powder board.
- Provider limits and History retain last-known valid data through bounded transient failures without duplicate equivalent scans.
- Run and Preview executes only a validated typed Project target or an explicitly reviewed temporary target.
- Hidden surfaces produce no sustained CPU activity.

## Phase 10 - Cortana Operations, Context, Voice, and Notifications

**Status:** Blocked for complete provider parity on Phases 3, 4, and 6, while local voice-engine checks and adapter fixtures may proceed.

### Goal

Give Cortana lightweight operational awareness and make attention cues provider-independent.

### Work

1. Add `fleet_health`, `captain_health`, `context_status`, `resource_summary`, and `list_owned_resources` operations.
2. Add `navigate_to_captain`, `recover_captain`, `checkpoint_captain`, and `retire_captain` operations.
3. Generate threshold events in T-Hub rather than making Cortana continuously poll with model tokens.
4. Derive liveness from terminals, Harness processes, and lifecycle events rather than Captain self-report alone.
5. Generate context-reset recommendations after safe turn boundaries and meaningful Assignment milestones.
6. Require a durable checkpoint and unresolved-decision review before reset recommendations.
7. Preserve Captain identity, Workspaces, and Crew across context resets.
8. Feed Codex and Claude attention states into the same chime, desktop-notification, and voice paths.
9. Separate controls for needs-input, completion, failure, recovery, and retirement cues.
10. Preserve Scribe talk-over protection and voice-engine fallback visibility.
11. Attribute cues to the correct Cortana, Captain, or Crew identity.
12. Consider per-Captain chime or voice identity only after the common cue path is reliable.
13. Verify Claude and Codex header identity persistence across Refresh, remount, restart, and exact conversation resume.
14. Fail visibly when a Harness cannot prove a needs-input transition rather than silently claiming voice parity.

### Tests and Evidence

- Test threshold generation without any model process running.
- Verify that idle, empty, or high-context Captains are not retired automatically.
- Verify that reset recommendations do not appear while unsafe work or unanswered decisions remain.
- Test voice and notification transitions for both Harnesses.
- Test Scribe hold and delayed delivery behavior.
- Test TTS failure, fallback, recovery, and user-disabled states.

### Exit Gate

- Cortana can inspect and recover the fleet without implementation-level control over Crew.
- Context recommendations are useful, safe, and provider-independent.
- Codex and Claude produce equivalent user-facing attention cues for equivalent states.
- Voice failures are visible rather than silent.

## Phase 11 - Measured Runtime Efficiency

**Status:** Active; exact desktop `0.3.94` and agent `0.5.2` deployment, graphical agent routing, and the repeated packaged one-terminal baseline are eligible, while the 4, 8, and 16 terminal matrix cells remain pending.
The first isolated attempt was invalidated when Windows Explorer launched a separate normal T-Hub process.
The documented retry pinned installed `0.3.90` PID `49712` and completed 55 samples over 61.05 seconds with one visible idle shell tile, but the host bridge produced eight births and eight deaths across four incomplete CPU intervals.
The artifact therefore reports `release_acceptance_eligible: false` and is diagnostic evidence rather than an accepted baseline.
The 29.94-second recurrence matched the visible tile's then-active 30-second full Git-header poll, whose Windows fallback created a new WSL process tree on every cache miss.
Source commits `5ced6c2` and `bd0d8dd` add the `GitInfo` protocol operation, route full snapshots through the persistent agent, cap the agent collector below the desktop request timeout, distinguish disconnected, unsupported, and command-failure outcomes, and add a real stdio round-trip test against the matching agent.
The one-shot Windows fallback remains only for a disconnected bridge or an explicitly unsupported older agent; an agent command failure returns degraded Git state without starting competing fallback work.
Commit `8dd94c9` emits one successful agent-source marker per process and keeps every exceptional route visible for packaged acceptance.
Commits `a9b7082` and `42de985` also remove full-suite attach-test interference by quiescing the churn workload and restoring process-global agent integration-test environment before deleting its fixtures.
Commit `d73f9cb` versions desktop `0.3.93`, with agent and protocol `0.5.1` from `f821957`; exact source `e95eb56` was then built and installed for packaged acceptance.
Installed `0.3.93` with agent `0.5.1` passed the graphical same-cwd Git-header proof, emitted one `git_info source=agent` marker, and emitted no fallback or agent-error marker in the proof window.
The isolated packaged retry at PID `20132` completed 55 samples over 60.96 seconds with one visible idle shell tile and the same exclusive agent route, but recurring host-bridge triplets produced 12 births, 15 deaths, and seven incomplete intervals.
Artifact `artifacts/perf/t-hub-0.3.93-1t-20260715T0204-r2.json` therefore remains diagnostic and reports `release_acceptance_eligible: false`.
Read-only descendant tracing attributed the residual periodic host-bridge lane to terminal reconciliation, which collected tmux sessions and pane metadata through recurring Windows-to-WSL subprocesses.
Source commit `3816bf4` adds the additive `TerminalSnapshot` protocol operation and routes normal `list_terminals` reconciliation through the persistent WSL agent.
The compatibility scan is limited to one bootstrap attempt for a disconnected or explicitly unsupported agent, is never resumed after agent success, and does not run after a timeout or agent command failure.
The agent collector bounds each of its two sequential steps to four seconds, drains both output pipes concurrently, and kills and reaps the collector process group on timeout.
The source versions are desktop `0.3.94` with agent and protocol `0.5.2`.
Its source gate passed 471 frontend tests, TypeScript and the production frontend build, 641 desktop Rust tests with one ignored, all Rust workspace and MCP end-to-end suites, warning-denied Clippy, formatting, diff checks, focused inherited-pipe and large-output timeout regressions, and the performance harness self-tests.
The exact detached `3816bf4` Windows build produced standalone, NSIS, and MSI SHA-256 values `00AA4B113B19B41B2D476E88D9CD5600D42B76F588C294A5D3E06C3B6D59F922`, `D9BFC8A94572D1ADEEA8E4494696176D3A49138BEB850D3F90AEE726A2DBE947`, and `FF467ECB84AF41C5893E60DBD54B71BF7848E4D89FEEB7829130893C1BAEF54D`.
The matching detached Linux agent reports `0.5.2` with SHA-256 `813DB68E3DA42A790532258CC89FBBAFC5ABFECFCDD9810FD4D912EB7F14658A`.
A direct real-agent round trip on a disposable isolated socket returned exactly one declared session and one pane, then cleanup preserved all six canonical session names and pane PIDs.
The exact NSIS installed desktop is `0.3.94` at `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe` with SHA-256 `021E7CAFF58C9A46720A02DD915D09BAC6BFE08235D7E80A8628C1E550223A7E`, and the matching installed agent reports `0.5.2` with SHA-256 `813DB68E3DA42A790532258CC89FBBAFC5ABFECFCDD9810FD4D912EB7F14658A`.
The normal-profile graphical proof emitted one `terminal_snapshot source=agent` marker and one `git_info source=agent` marker with no fallback, timeout, or agent-error marker, while five polls kept all six terminal IDs, states, and current working directories stable.
The first isolated 95.09-second packaged run in `artifacts/perf/t-hub-0.3.94-1t-20260715T1034.json` recorded zero host-bridge births or deaths and 86 complete host-bridge intervals, proving the targeted recurring churn was removed, but a one-second WebView2 helper lifetime made two total intervals incomplete.
The warm repeat in `artifacts/perf/t-hub-0.3.94-1t-20260715T1043-r3.json` sampled 95.35 seconds with exactly one declared idle shell, 86 complete total intervals, zero incomplete intervals, zero births or deaths in every category, a stable 17-process tree, and a stable 10-process host bridge.
Its total CPU release statistic is eligible at `0.10637` logical cores over the run.
The disposable tmux socket, database, control files, WebView profile, and shared-layout backup were removed after the proof.
The canonical profile was restored as PID `46860` with all six original tmux names and pane PIDs unchanged.
The eligible one-terminal gate unblocks the 4, 8, and 16 terminal scenarios, which are the next serialized Phase 11 measurements.

### Goal

Reduce steady CPU, memory, process, and startup cost using packaged measurements rather than intuition.

### Work

1. Capture clean packaged baselines with 1, 4, 8, and 16 declared sessions.
2. Include hot, warm, cold, Board, Preview, Captain, Crew, browser, inbox, and voice scenarios.
3. Attribute WebView2 CPU to renderer work, GPU work, xterm, animation, polling, or repaint scheduling.
4. Stop unnecessary animation frames, canvas redraws, cursor work, and layout measurement on hidden or unchanged surfaces.
5. Preserve bounded Powder event polling for registered Projects without a live Captain so relevant events remain unread until delivery is possible.
6. Cache Powder profiles, credentials, clients, and HTTP connection pools with explicit refresh behavior; source `0.3.100` completes the event-reconciler client cache and Windows console suppression, while broader shared caching remains open.
7. Enable binary PTY framing with a tested version fallback.
8. Remove the live JSON and base64 terminal-output path.
9. Coalesce terminal, focus, Git, History, usage, resource, and pane scans.
10. Pause low-priority polling for hidden windows, cold terminals, inactive panels, and disabled features.
11. Reduce watchdog cadence when event-driven diagnostics can prove health.
12. Lazy-load and prune icon resolvers by selected theme.
13. Reduce the main and icon JavaScript chunks.
14. Measure process birth and death, handles, threads, sockets, relays, and memory recovery.
15. Run a twenty-four-hour packaged soak.
16. Measure warm Workspace visual switch, input-ready latency, cold terminal visual restore, cold input readiness, and Captain first feedback.
17. Define reviewed numeric budgets for every release statistic before treating a matrix cell as passing.
18. Track duplicate equivalent scans, frontend callback failures, IPC fallbacks, mount hangs, WebView heap, GPU activity, and WSL descendant churn.

### Provisional Budget Candidates

These values are review candidates rather than settled release requirements until the measurement harness, hardware profile, and representative scenarios are accepted.

- Warm Workspace visual switch p95 at or below 100 milliseconds.
- Input-ready after a warm switch p95 at or below 150 milliseconds.
- Cold terminal visual restore p95 at or below 1 second.
- Input-ready after cold restore p95 at or below 1.5 seconds.
- Captain overlay first feedback p95 at or below 100 milliseconds.
- Zero duplicate equivalent provider, History, Git, terminal, resource, or pane scan while the same request is already in flight.

### Tests and Evidence

- Keep all scenarios scripted and record exact source, installed hash, PID, terminal count, and interval completeness.
- Reject performance conclusions from runs with unexplained process churn or incomplete CPU intervals.
- Measure input latency and cold restoration, not only memory.
- Record numeric pass or fail results for visual-switch, input-ready, restore, first-feedback, process, memory, callback, IPC, and scan-deduplication budgets.
- Compare before and after artifacts for each optimization.

### Exit Gate

- Hidden and cold terminals create no sustained rendering CPU.
- Closing resources returns process and memory counts toward baseline.
- The 1, 4, 8, and 16 session matrix meets documented budgets.
- The packaged stress window contains no missing callback IDs, stale-callback IPC fallback, frontend mount hang, or duplicate equivalent in-flight scan.
- The soak shows no growing process, handle, socket, log, queue, or memory trend.

## Phase 12 - Security, Release, Documentation, and Production Acceptance

**Status:** Continuous preparation is allowed, while the final production gate remains blocked on every preceding phase and T27.

### Goal

Make the validated product safe, traceable, installable, and understandable.

### Work

1. Document the expected Windows-to-WSL Tailscale route and the nonessential WSL self-hairpin limitation.
2. Resolve repeated Tailscale DNS or duplicate-bind warnings that affect supportability.
3. Complete Tauri Content Security Policy hardening for app, Board, and Preview surfaces.
4. Add Authenticode signing for the executable and installer.
5. Add dependency, secret, vulnerability, and license scanning.
6. Complete strict branch protection and required status checks.
7. Keep external workflow actions pinned to immutable revisions.
8. Add packaged Windows, WSL, tmux, Codex, Claude, Powder, messaging, Board, Preview, voice, and cleanup E2E coverage.
9. Validate installer upgrade, rollback, state migration, and uninstall behavior.
10. Verify protected Powder permissions and credential redaction on every path.
11. Produce an SBOM and retain source commit, build identity, installer hash, and installed-binary hash.
12. Update user documentation for all settled product vocabulary and workflows.
13. Mark historical design documents superseded only with explicit approval.
14. Preserve Lavish and deck artifacts as instructed by the General.
15. Update the zero-context handoff with exact source, runtime state, tests, measurements, and remaining risks.
16. Keep `docs/REVIEW-INDEX.md` current so historical and archived reviews cannot silently become active backlog.
17. Bump the desktop version for every changed packaged build and reject reuse of a version already tagged to different source.
18. Build and install the signed production artifact from the exact reviewed commit.
19. Push and publish only when the General requests it.
20. Disable release devtools and reduce Tauri capabilities to the minimum reviewed production set.
21. Pin and verify the Rust toolchain and minimum supported Rust version across local and CI builds.
22. Add and review license, security policy, privacy, support, contribution, and operator-runbook documents.
23. Implement user-owned backup, reset, and diagnostics-export workflows with explicit scope and confirmation.
24. Test registry, inbox, History, journal, Project, and resource corruption plus disk-full and partial-write recovery.
25. Publish the supported Windows, WSL, tmux, Harness, Provider, Powder, and upgrade matrix.
26. Bound diagnostic and event-journal retention and prove cleanup never removes user-owned evidence silently.
27. Add native Windows automated tests to complement Ubuntu CI before calling the packaged product release-ready.
28. Independently review the T34 grant evaluator, storage migration, revocation and consumption linearization, foreign-state non-disclosure, and every absolute denial.
29. Independently review the T35 identity transition, canonical-root uniqueness, operation journal, Project and board convergence, partial-state ownership, and rollback boundaries.
30. Run installed Windows T34 and T35 matrices for role, stale generation, retry, response loss, application and WSL restart, concurrency, corruption, redaction, exact build provenance, and no-success-before-durable-reopen behavior.

### Tests and Evidence

- Run the entire automated quality gate on the exact release source.
- Run final interactive Captain and Crew acceptance on the installed Windows build.
- Verify version, PID, executable hash, sessions, Powder, Tailscale, Board, Preview, History, messaging, voice, resources, and cleanup.
- Audit the working tree for generated files, secrets, and preserved user artifacts.

### Exit Gate

- No Critical, High, or unresolved Medium finding remains under the documented threat model.
- The signed installer upgrades without losing identities, sessions, Workspaces, History, inbox state, or protected bindings.
- Documentation matches visible behavior and terminology.
- The installed version and binary hash match the release artifact.
- The handoff names the next action without relying on conversation history.

## Parallel Implementation Map

Parallel work should use isolated worktrees, explicit file ownership, separate Powder cards, and one integration owner.
Parallel lanes must not edit the same registry schema, migration, or core lifecycle file without an agreed boundary.

### Tranche P - Generic Powder Contract Dependencies

These lanes belong in the Powder repository and must be designed for every Powder client rather than for T-Hub alone:

- **P1 Run-bound completion:** server-enforced expected-run precondition, durable operation identity, idempotent replay, and no effect on a reclaimed card.
- **P2 Mutation recovery:** append and completion operation-status lookup after timeout or response loss.
- **P3 Work-log attribution:** current-run enforcement and authoritative normalized stored responses.
- **P4 Criterion integrity:** current-run review scope and authenticated reviewer identity.
- **P5 Conditional repository creation:** create-only or equivalent non-overwriting precondition.
- **P6 Card-authoring safety:** verify idempotency, recovery, authorization, and revision preconditions for create, update, relation, proof-plan, and status mutations.
- **P7 Event documentation:** publish the complete versioned card-event contract, including `work-log-appended`.

P1 through P4 may run in parallel with T-Hub client scaffolding, lifecycle serialization, deterministic mocks, and read-only evidence surfaces.
T-Hub automated completion remains disabled until their exact reviewed Powder contracts are deployed and accepted.
P5 does not block manual board selection or binding.
P6 permits T-Hub to expose each Captain card-authoring mutation whose existing Powder contract independently passes its gate.
P7 does not block generic event-name handling, but event-specific behavior cannot depend on an undocumented event.

### Tranche A - Immediate Foundation

These lanes may proceed in parallel:

- **A1 Terminal correctness:** xterm race reproduction, lifecycle fixes, and packaged tests.
- **A2 CLI reliability:** upgrade `th`, fix restart recovery, add timeout tests, preserve protocol compatibility, and do not expand the command catalog yet.
- **A3 Resource schema design:** specify ownership records and reconciliation without enabling cleanup yet.
- **A4 Provider event research and fixtures:** capture Codex and Claude lifecycle fixtures without changing the live reducer.
- **A5 Documentation and terminology:** keep canonical definitions synchronized without changing historical artifacts.
- **A6 Control framing:** bound unauthenticated and authenticated control frames and connection cleanup.
- **A7 Installed IPC diagnosis:** reproduce callback loss, custom-protocol fallback, mount hangs, and duplicate scans before fixing them.

Integration order is A1 and A2 first, followed by the safe activation of A3.
A3 must implement worktree ownership and safety from `docs/WORKTREE-STATUS-CONTRACT.md` rather than introducing a resource-only approximation.
A3 may land Git-only suspension and safety scaffolding before B1, but it must consume B1 for Captain, Assignment, Workspace, and Crew ownership rather than creating a parallel identity model.

### Tranche B - Identity, Providers, and Control

These lanes may proceed in parallel after the Phase 1 control contract is stable:

- **B1 Cortana and multi-Captain registry:** identity schemas, migrations, Assignment records, and retirement state.
- **B2 Workspace model:** Captain-to-Workspace control and Crew membership.
- **B3 Codex adapter:** hooks, interactive telemetry, context, History, and permission launch behavior.
- **B4 Claude adapter normalization:** move existing hooks and status telemetry behind the shared contract.
- **B5 CLI contract and shared catalog:** first normalize the existing CLI to `docs/cli-contract.md`, then add shared schemas, role filtering, command groups, and CLI-to-MCP parity tests.
- **B6 Inbox identity and UI data model:** implement `docs/AGENT-RELATIONSHIP-AND-MESSAGING-CONTRACT.md` through durable recipients, message states, card and run links, retention, acknowledgements, and read APIs.
- **B7 Powder lifecycle safety:** per-Crew serialization and retry-safe integration against P1 through P4.
- **B8 Scoped grant contract:** add the T34 schema, bounded private store, pure evaluator, standing and one-time modes, strict delegated subsets, compatibility migration, explanation, audit, and role matrix against explicit T7 and T11 interfaces.

B1 owns shared identity migrations.
B3 and B4 must consume B1's identity interfaces rather than each introducing provider-specific durable fields.
B3 and B4 must emit the two independent status axes defined in `docs/STATUS-MODEL.md`.
B5 owns command definitions.
B5 must land contract behavior and process-level tests before broad command generation so new commands inherit the correct interface.
B6 owns messaging schema and must not bypass B1 authority.
B7 may build mocks and fail-closed scaffolding before P1 through P4 land, but may not ship automated completion against a card-only Powder mutation.
B8 owns authorization artifacts and must not introduce a parallel identity registry, operation catalog, inbox approval state machine, resource verdict, or provider work-profile model.
B8 pure-policy and schema work may proceed alongside B1 and B5 only with explicit interfaces, while evaluator activation waits for authoritative B1 identity and B5 descriptors.

### Tranche C - Product Flows

These lanes may proceed in parallel after the identity and adapter contracts stabilize:

- **C1 Codebase picker and preflight UI.**
- **C2 New-codebase and rollback transaction.**
- **C3 Board binding and authentication states.**
- **C4 Run and Preview lifecycle.**
- **C5 History and Provider limits UI.**
- **C6 Messages and Resources UI.**
- **C7 Cortana health and recovery commands.**
- **C8 Voice and notification parity.**
- **C9 Captain creation recovery and card authoring.**
- **C10 Full Board packaged repair and relevant-board defaults.**
- **C11 Provider-limit and History singleton caching.**
- **C12 Existing-repository Captain self-bootstrap:** compose T35 with T34, T5, T6, T7, T11, and the shared canonical Git identity primitive, preserve the Appturnity denial, and activate only after the backend and parity gates pass without waiting for T18 activation.
- **C13 Routine delivery grants:** start T36 only after T34, T11, T18, T23, and reviewed external adapters are stable.

Each lane must use shared Project, identity, resource, and adapter APIs.
No lane may create a local substitute for a missing backend contract.
C12 does not depend on P5 or T18 activation, may bind only an existing authorized board, and must converge with T18 without implementing a parallel ownership service.
C13 may cover owned non-protected delivery only and cannot absorb protected profile endpoint or credential-command mutation, system-global installation, public repository creation, production, protected-branch, publication, spending, customer, or destructive authority.

### Tranche D - Acceptance and Hardening

These lanes may proceed in parallel after real Powder acceptance passes:

- **D1 Final packaged performance matrix and soak:** measurement-harness and diagnostic work may proceed earlier, while the final 1, 4, 8, and 16 acceptance matrix and soak wait for real Powder acceptance and a stable installed build.
- **D2 Security and credential audit.**
- **D3 Packaged cross-Harness E2E.**
- **D4 Accessibility and visual quality.**
- **D5 Installer, signing, update, and rollback.**
- **D6 Documentation, handoff, and release evidence.**

Release integration waits for every Phase 12 gate.

## Testing Doctrine

1. Reproduce user-visible bugs in the packaged product before fixing them.
2. Add a failing automated regression at the closest reliable layer.
3. Test pure state transitions with unit tests.
4. Test adapter and protocol contracts with fixtures from real Harness output.
5. Test registry, authorization, migration, and rollback through Rust integration tests.
6. Test UI state, accessibility, and error presentation through component tests.
7. Test complete user workflows through packaged Windows E2E.
8. Use real Powder only for final acceptance while retaining deterministic mock failure tests.
9. Test every mutation at success, explicit rejection, timeout, crash, retry, and rollback boundaries.
10. Run the focused regression, formatter, and directly affected static checks before each logical commit.
11. Run lint, warnings-denied compilation, frontend tests, Rust workspace tests, TypeScript, production builds, contract parity, and security checks at integration, pull-request, packaged-acceptance, and release boundaries.
12. Record interactive checks that cannot yet be automated and convert stable checks into automation later.
13. Do not declare provider parity based only on both terminals launching.
14. Test authoritative, derived, stale, unknown, and conflicting state explicitly rather than collapsing uncertainty into a healthy default.

## Claude and Codex Parity Matrix

The matrix describes current T-Hub support, not the provider's theoretical capabilities.

| Capability | Claude Code today | Codex today | Required outcome |
| --- | --- | --- | --- |
| Interactive launch | Supported | Supported | Apply explicit unrestricted defaults and identity labels to both |
| Interactive unrestricted permissions | Inherited or flag-dependent | Inherited or flag-dependent | Apply and display the effective bypass mode consistently |
| Provider session identity | Strong through `SessionStart` hooks | Partial for interactive sessions | Bind both to durable T-Hub identities |
| Turn lifecycle | Strong hook coverage | Headless tap plus weak interactive inference | Normalize structured interactive events |
| Needs-question detection | `Elicitation` and filtered notifications | No complete T-Hub bridge | Derive from Codex hooks or app-server events |
| Permission-request detection | Hooked | Provider hook exists but is not integrated | Feed both into one attention path |
| Completion detection | `Stop` hook | Provider `Stop` hook exists but is not integrated | Feed both into one reducer |
| Failure detection | `StopFailure` and session-end evidence | No exact `StopFailure` hook | Derive from turn events, process result, and structured errors |
| Context telemetry | Structured status-line bridge | Native footer only for the user | Add structured Codex context telemetry |
| Provider limits | Supported through status line and fallback | Account usage strip exists | Normalize global quota display |
| Subagent supervision | Hooked | Provider hooks exist but are not integrated | Normalize start and stop events |
| Task lifecycle | Claude-specific task hooks | No direct equivalent confirmed | Mark capability and derive only when reliable |
| Worktree lifecycle | Claude-specific hooks | No direct equivalent confirmed | Use T-Hub-owned worktree operations as the common authority |
| Directory changes | Claude `CwdChanged` hook | No direct equivalent confirmed | Use terminal or T-Hub process evidence where necessary |
| Compaction lifecycle | Not currently integrated by T-Hub | Codex has pre and post compact hooks | Add normalized context compaction events where available |
| Tool lifecycle | Not currently part of T-Hub supervision | Codex has pre and post tool hooks | Keep optional and avoid noisy default UI |
| History and resume | Claude-only Recent implementation | No unified History | Build adapter-backed History for both |
| Context meter in tiles | Claude-only | Missing | Make provider-independent |
| Auto-continue after provider limit | Implemented through the Claude-specific flow | Missing | Build durable exact-thread Codex scheduling, cancellation, deduplication, and recovery |
| Voice attention announcements | Works when Claude status transitions arrive | Usually absent because interactive status is weak | Drive voice from normalized events |
| Chimes and OS notifications | Stronger through Claude events | Degraded | Drive both from normalized events |
| Hook installation UI | Claude-only | Missing | Replace with Agent integrations |
| Hook trust model | Claude settings merge | Codex requires explicit hook review and hash trust | Surface provider-specific trust without hiding it |
| Native agent voice input | Enabled in the current Claude configuration | No equivalent T-Hub-managed Codex setting | Prefer provider-agnostic Scribe input rather than require native parity |
| Provider-native notifications | Claude notification hooks feed T-Hub | Codex TUI notifications exist outside T-Hub | Normalize important events inside T-Hub and leave native notifications optional |
| Provider plugins and marketplaces | Claude plugins and marketplaces are configured separately | Codex plugins use a different configuration system | Show integration health without trying to force one provider's plugin model onto another |
| MCP provisioning | Installed and tested | Installed and tested | Prefer CLI-first shared operations and role-filter MCP |
| CLI control | Available but incomplete | Available but incomplete | Make Harness-independent and canonical |
| Model and reasoning display | Partial | Configured externally | Display effective model and reasoning without requiring selection each launch |
| Harness switching | Not a durable identity operation | Not a durable identity operation | Preserve T-Hub identity across reviewed runtime replacement |

## Intentional Provider Differences

Provider parity means equivalent T-Hub outcomes, not identical provider internals.
Claude may continue to provide unique task, notification, worktree, and directory hooks.
Codex may continue to provide unique compaction and tool lifecycle hooks.
T-Hub should expose optional detail where useful while keeping the common Captain and Crew workflow consistent.
Unsupported events must be labeled as unavailable or derived rather than fabricated.

## Outstanding Considerations and Recommended Defaults

### Same-User Isolation

The current application boundary does not protect against a malicious process running as the same WSL user.
Strong isolation requires separate OS users, containers, or a broker that keeps tmux and credentials outside agent-readable state.
Recommended initial decision: document the same-user trust boundary and defer hard isolation until the core workflow passes acceptance.

### Powder Board Cardinality

A Project may eventually need more than one Powder board.
Recommended schema: support one default board plus optional Assignment-specific bindings without forcing the UI to expose multiple boards initially.

### Multi-Captain Git Coordination

Multiple Captains in one Project increase branch, worktree, shared-file, and landing conflicts.
Recommended policy: every Crew member owns one validated worktree, every Captain Assignment has a branch namespace, Powder claims carry work ownership, and overlapping Captains coordinate through visible messages.

### Cross-Project Captain Messaging

Captains may need expertise from another Project.
Recommended policy: allow explicitly addressed cross-Project messages, label them clearly, grant no file or terminal access, and require explicit work transfer before implementation ownership changes.

### Offline and Partial Failure

T-Hub, Powder, Tailscale, the Harness, and the model Provider can fail independently.
Recommended policy: preserve read and recovery functions offline, fail authority-dependent mutations safely, and show which subsystem is unavailable.

### Secrets and Retention

Inbox bodies, terminal captures, History, logs, and Powder references can contain sensitive material.
Recommended policy: redact known secret shapes, avoid implicit body logging, use bounded local retention, and provide explicit deletion and pinning.

### Provider Limits and Auto-Continue

Provider limit behavior differs across services and can change.
Expose provider limits globally and keep context per conversation.
Implement auto-continue as a normalized adapter capability with an explicit per-session setting.
Codex auto-continue must persist the exact thread ID, intended continuation, earliest reset time, owning T-Hub identity, cancellation state, and idempotency key.
If Codex cannot resume safely, T-Hub must retain the pending recovery visibly rather than sending input to an uncertain shell or conversation.

### Model and Harness Switching

A runtime switch can strand a provider conversation or introduce incompatible identifiers.
Recommended policy: require a checkpoint, stop the old runtime, start the replacement, bind the new conversation, and retain the old conversation in History.

### Resource Budget

The six-concurrent-Crew idea is an initial operational default rather than a proven hardware limit.
Recommended policy: do not enforce a hard limit until packaged 1, 4, 8, and 16-session measurements establish warning and queue thresholds.

## Outstanding Questions

No product question blocks Phase 1 or Phase 2.
The following questions can be resolved before their dependent phases:

1. Should the first UI expose Assignment-specific Powder boards, or support them only in the schema and Advanced settings?
2. Should message-body retention default to thirty days, or should the General choose a different local retention period?
3. Does the General want hard same-user isolation before public distribution, or is the documented local trust boundary acceptable for the first production release?
4. Which GLM Harness or OpenAI-compatible runner should become the third adapter after Codex and Claude parity is complete?
5. Should completion voice announcements remain opt-in and separate from needs-input speech?
6. Should Codex interactive telemetry combine lifecycle hooks with app-server or structured turn events for states the hooks cannot prove?
7. Should provider-specific capabilities appear in an Advanced detail view while the normal UI presents the shared workflow?

Recommended answers are already recorded above so implementation need not pause unless the General wants different policy.
The recommended Codex telemetry answer is to use hooks for lifecycle boundaries and a structured Codex event source for context, failures, and any missing turn detail.
The recommended UI answer is to preserve provider-specific detail in Agent integrations while keeping the normal Captain and Crew workflow common.

## Zero-Context Resume Checklist

1. Load the active workspace `AGENTS.md` instructions supplied to the session and read this document.
2. Read `docs/CAPTAIN-POWDER-HANDOFF.md`, `docs/ORCHESTRATOR-OPERATING-MODEL.md`, `skills/captain/SKILL.md`, `docs/POWDER-INTEGRATION.md`, and `docs/PERFORMANCE-BENCHMARK.md` for the active phase.
3. Run `git status --short --branch` and preserve `.lavish/` plus `docs/DECK-AGENTS-DESIGN.md`.
4. Run `git log --oneline -12` and inspect work after this plan.
5. Confirm the installed Windows PID, executable path, version, and hash rather than assuming source is deployed.
6. Confirm the active phase and its dependencies.
7. Reproduce the relevant user-visible behavior before editing a bug fix.
8. Use an isolated worktree and Powder card for parallel implementation.
9. Run the phase-specific tests and global quality gates.
10. Commit the verified logical change with a clear message and no automatic co-author line.
11. Update this plan only when product decisions, dependencies, or phase status materially change.
