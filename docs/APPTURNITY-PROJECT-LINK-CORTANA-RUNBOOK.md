# Appturnity Project Link Cortana Runbook

## Status and intended reader

This is an operational handoff for the active T-Hub Cortana.
It was requested by the General on 2026-07-17 so Cortana can remove the control-plane block preventing the Appturnity deep review.
It is written for an agent with no access to the conversation that produced it.
It does not replace Cortana's fleet-coordination goal or the T-Hub Captain's active implementation goal.
Cortana must reconcile this work with those goals before assigning or changing T-Hub implementation work.

## Mission

Make the existing Appturnity WSL checkout an authoritative T-Hub Project, bind that Project to the existing Powder `appturnity` repository, attach the already-running Appturnity Captain, and prepare authoritative review cards.

The intended result is:

1. One T-Hub Project represents the canonical WSL Git main worktree at `/home/natkins/projects/appturnity`.
2. That Project is bound to Powder profile `n8desktop-wsl` and repository `appturnity`.
3. Existing Captain terminal `d3f3535d` is attached to the Project under ship `appturnity`.
4. The Captain can recover the Project and board through `captain_bootstrap` and `project_board_snapshot`.
5. Powder contains dependency-ready review cards suitable for sanctioned Crew dispatch.
6. The dirty Windows checkout remains exactly untouched.

## Terminology

Powder already has the Appturnity planning repository or board.
The missing object is a T-Hub Project.

The T-Hub Project owns the canonical local Git identity and stores its Powder binding.
Creating the T-Hub Project must not create a second Powder repository, clone the code, initialize Git, or alter the existing checkout.

## Verified live state

The following facts were observed through the installed T-Hub control surface and local read-only Git commands on 2026-07-17.

### Appturnity Git state

- Canonical WSL checkout: `/home/natkins/projects/appturnity`.
- Canonical branch: `main`.
- Observed commit: `6f3671a25e311a9f9ba31a9f7e6b68cc1d71a842`.
- Observed WSL checkout status: clean.
- Origin: `https://github.com/natkins23/Appturnity.git`.
- The repository has no nested Git repositories or submodules.
- The repository contains multiple application areas, but it is one Git repository and one T-Hub Project.

### Protected Windows checkout

- Protected checkout: `/mnt/c/Users/natha/Projects/Consulting/appturnity`.
- The protected checkout is dirty with many user-owned modifications.
- It is not the registration target.
- It must not be normalized into, copied over, cleaned, reset, staged, committed, registered, or used as a Crew worktree.

### Powder state

- Protected connection profile: `n8desktop-wsl`.
- Powder repository: `appturnity`.
- Repository alias: `natkins23/Appturnity`.
- Tier: active.
- Observed card count: zero.

### T-Hub state

- Existing Projects are `t-hub-app` and `powder` only.
- There is no Appturnity Project.
- Appturnity Captain terminal: `d3f3535d`.
- Appturnity ship slug: `appturnity`.
- The Captain is active and has control capability.
- The Captain has no `projectId` and no Crew.
- Cortana terminal: `a3e518cd`.
- Cortana is active and has the role allowed to register a new Project.
- `captain_bootstrap` for the Appturnity Captain currently returns `Captain is not bound to a registered project`.

### Observed registration failures

An Appturnity Captain registration attempt was denied by the intended role boundary:

```text
acl: only General/Cortana may register a new project
```

Cortana then attempted registration with Windows and POSIX WSL path forms.
The installed runtime rejected those attempts with:

```text
repoRoot must be an absolute path
```

The second error is the immediate technical block.
Cortana already has the required organizational authority.

## Hard constraints

1. Do not create a second Appturnity Captain.
2. Do not create a second Powder `appturnity` repository.
3. Do not dispatch Crew before the Project, Powder binding, Captain attachment, card, and run are authoritative.
4. Do not hand-edit the T-Hub Captain registry or Powder storage.
5. Do not retrieve, print, copy, or pass Powder credentials.
6. Do not use the dirty Windows checkout as the Project root or as a worktree.
7. Do not initialize Git, create a directory, clone a repository, or rewrite the origin.
8. Do not treat a successful source test as proof that the installed Windows runtime is fixed.
9. Do not install, merge, release, publish, or deploy a T-Hub build unless Cortana's current authority covers that action or the General explicitly approves it.
10. Preserve active T-Hub and Powder Captains, Crew, claims, runs, worktrees, and protected untracked files.

## Why registration currently fails

The source inspection used T-Hub branch `fix/captain-control-runtime` at commit `9d7c9f91a3e4818ddd52d5fadf2f4d700ed14190`.

`register_project` reads `git worktree list --porcelain` through the WSL Git adapter.
The returned main-worktree path is intentionally a POSIX path such as `/home/natkins/projects/appturnity`.
The handler then calls `std::fs::canonicalize` directly on that POSIX string.
In the packaged Windows process, that canonicalization cannot resolve the WSL POSIX path and falls back to the unchanged POSIX string.
`upsert_project` then checks the unchanged string with the Windows host implementation of `Path::is_absolute`.
The Windows predicate does not consider `/home/...` an absolute Windows path, so it emits `repoRoot must be an absolute path`.

This is an evidence-backed source explanation, but installed-runtime reproduction remains authoritative.
The repair must use the shared WSL and host path identity layer rather than adding an Appturnity-specific exception.

Existing T-Hub Project records demonstrate the intended durable identity form.
Their roots are stored as canonical Windows extended UNC paths such as `\\?\UNC\wsl.localhost\Ubuntu-24.04\home\natkins\projects\...`.

## Responsibility split

### Cortana owns

- Fleet reconciliation before the operation.
- Confirming the active T-Hub goal does not already own the same defect.
- Routing the path-normalization defect to the authoritative T-Hub implementation owner.
- Obtaining any required install or deployment authorization.
- Re-running registration through the sanctioned control operation after the fixed runtime is active.
- Binding Powder if registration does not do so atomically.
- Attaching the existing Appturnity Captain.
- Confirming the authoritative Project and Powder state.
- Arranging the initial Powder review cards through a sanctioned Powder surface.
- Recording the completed handoff in Cortana and Appturnity durable checkpoints.

### The T-Hub implementation owner owns

- Reproducing the packaged Windows failure before editing production code.
- Implementing the shared cross-platform path fix.
- Adding unit, integration, authorization, idempotency, and packaged-runtime coverage.
- Obtaining independent review because Project identity and registration are control-plane sensitive.
- Producing a verified installable artifact or approved runtime update.

### The Appturnity Captain owns after unblocking

- Recovering the new Project and board state.
- Confirming the review cards and their dependencies.
- Preparing isolated review worktrees when needed.
- Dispatching and supervising review Crew through `dispatch_crew` only.
- Synthesizing the deep product and engineering review for the General.

## Execution plan

### Phase 0: Recover and reconcile fleet state

Before any mutation, Cortana must:

1. Run the Captain environment check and confirm `my_capability` returns `control`.
2. Read Cortana's durable checkpoint.
3. Call `list_captains`, `list_projects`, `list_powder_boards`, and `list_terminals`.
4. Confirm terminals `a3e518cd` and `d3f3535d` still represent Cortana and the Appturnity Captain.
5. Confirm there is still no Appturnity Project.
6. Confirm Powder profile `n8desktop-wsl` still contains exactly one active `appturnity` repository.
7. Confirm the canonical WSL checkout still resolves to one Git main worktree.
8. Record the WSL checkout HEAD and status.
9. Record the protected Windows checkout status using read-only commands only.
10. Inspect the active T-Hub Captain, active Powder cards, active Crew, branches, and review state before assigning the defect.

Stop if another Project already resolves to the Appturnity root, another Captain claims the ship, the board is ambiguous, or the protected checkout state cannot be distinguished from the canonical WSL checkout.

### Phase 1: Reproduce the path defect through the installed flow

The closest end-user reproduction is a Cortana call to `register_project` against the existing WSL repository.

Use these semantic arguments:

```json
{
  "repoRoot": "/home/natkins/projects/appturnity",
  "name": "appturnity",
  "powderConnectionProfile": "n8desktop-wsl",
  "powderRepository": "appturnity",
  "createDirectory": false,
  "initializeGit": false
}
```

Do not supply `remoteUrl` unless current Git inspection proves the adapter cannot derive it.
Do not change the arguments on an ambiguous response.
After a failure, call `list_projects` before retrying to prove that no partial Project was created.

Capture the bounded error, installed version, Cortana identity, and list-projects result without recording credentials or environment dumps.

### Phase 2: Reconcile the repair with the active T-Hub goal

Cortana must not silently create a parallel implementation stream.

1. Ask the active T-Hub Captain to map this defect against the current goal and `docs/APPTURNITY-PROJECT-LINK-CORTANA-RUNBOOK.md`.
2. Check whether an existing Powder card already owns cross-platform Project-root normalization or registration.
3. If an existing card owns it, expand that card's acceptance criteria with the Appturnity reproduction and dependency.
4. If no existing card owns it, arrange one exact T-Hub Powder card through a sanctioned planning surface.
5. Keep the Appturnity Project registration itself as Cortana's post-fix operational task, not as Crew implementation scope.
6. Preserve the ownership and landing order of active Wave 0 control work.

A suggested new card identifier, only if genuinely absent, is `thub-project-registration-wsl-path-identity`.

### Phase 3: Implement the shared path-identity repair

The implementation must not special-case Appturnity.

The shared behavior must:

1. Accept an absolute POSIX WSL path from the CLI, MCP, or UI on the packaged Windows host.
2. Resolve Git identity inside the configured WSL distribution.
3. Convert the Git-reported POSIX main-worktree root into the canonical host identity before persistence and duplicate lookup.
4. Converge supported POSIX, normal WSL UNC, legacy WSL UNC, and extended WSL UNC spellings onto one Project root.
5. Resolve a linked-worktree input to the canonical main-worktree root.
6. Preserve the configured WSL distribution and reject a foreign distribution.
7. Reject relative, malformed, nonexistent, non-Git, ambiguous, or unsafe paths before persistence.
8. Keep Project IDs and roots unique under concurrent and repeated registration.
9. Preserve existing registered Project identities and Powder bindings.
10. Keep new-Project registration restricted to General and Cortana until the scoped-grant program explicitly changes that policy.
11. Leave the dirty Windows checkout untouched even when its remote resembles the WSL repository.
12. Use the shared path layer rather than independent string replacement in `register_project`.

The likely repair area is:

- `apps/desktop/src-tauri/src/control.rs`, especially `register_project`, `upsert_project`, Project duplicate lookup, and persistence validation.
- `apps/desktop/src-tauri/src/files.rs`, especially shared WSL POSIX, UNC, extended UNC, canonical-host, and POSIX-runtime conversion helpers.
- `apps/desktop/src-tauri/src/git.rs`, especially the documented POSIX `WorktreeInfo.path` contract.
- `apps/desktop/src-tauri/crates/t-hub-mcp/src/tools.rs` only if the public schema or description is inaccurate.
- `apps/cli` only if public command behavior needs parity changes.

Do not weaken `Path` validation without replacing it with an explicit cross-platform absolute and canonical identity contract.

### Phase 4: Verify the repair before installation

The implementation owner must provide at least these tests.

#### Path identity tests

- POSIX `/home/natkins/projects/appturnity` is recognized as an absolute WSL input on Windows.
- Normal `\\wsl.localhost\Ubuntu-24.04\home\natkins\projects\appturnity` converges on the same identity.
- Legacy `\\wsl$\Ubuntu-24.04\home\natkins\projects\appturnity` converges on the same identity.
- Extended `\\?\UNC\wsl.localhost\Ubuntu-24.04\home\natkins\projects\appturnity` converges on the same identity.
- A linked worktree converges on its main worktree.
- Relative and forward-slash UNC-confused inputs fail.
- A foreign WSL distribution fails.
- A Windows drive checkout is not confused with the WSL checkout.
- Symlink, junction, case, separator, trailing-separator, `.` and `..` behavior matches the reviewed canonical identity contract.

#### Authorization and persistence tests

- Cortana can register a new exact Project.
- General can register a new exact Project.
- An ungranted Captain remains denied for a new Project.
- A Crew identity remains denied.
- A control token without the required durable role remains denied.
- Repeating the same registration converges on one Project ID.
- Concurrent equivalent path spellings cannot create duplicate Projects.
- Existing T-Hub and Powder Projects retain their current roots and bindings.
- Persistence failure cannot return a false successful registration.

#### Powder binding tests

- Registration can bind the existing `n8desktop-wsl/appturnity` repository.
- An absent board fails without creating a Project that falsely claims a binding.
- An ambiguous or foreign board fails closed.
- Retry after response loss returns the same Project and binding.
- Credentials, endpoints, and protected profile details do not appear in results or errors.

#### Required gates

- Focused Rust tests pass.
- Full relevant workspace tests pass.
- Rust formatting passes.
- Clippy passes for the changed workspace targets.
- CLI and MCP schema tests pass if those surfaces change.
- `git diff --check` passes.
- Independent control-plane review approves the final commit.

### Phase 5: Verify and activate the installed runtime

Source tests do not unblock Appturnity.
The fixed T-Hub runtime used by Cortana must be installed or activated through the repository's approved release or local-acceptance path.

Before installation or activation:

1. Confirm the exact reviewed commit and artifact.
2. Confirm the active T-Hub Captain's landing state.
3. Obtain explicit General approval if install or deployment authority is not already part of Cortana's current goal.
4. Preserve the current registry, Powder profiles, and active fleet state.
5. Follow the repository's packaged Windows acceptance requirements.

After activation:

1. Reconnect through a fresh supported T-Hub MCP session if the runtime requires it.
2. Confirm Cortana still has control capability and its durable identity.
3. Confirm existing Projects and Captains survived unchanged.
4. Repeat the exact installed Appturnity reproduction.

### Phase 6: Register and bind Appturnity

Cortana should call `register_project` with the exact Phase 1 arguments.

Expected behavior:

1. Git validation resolves the WSL repository.
2. The durable Project root becomes the reviewed canonical host identity.
3. One new Project ID is created or an exact existing partial Project is reused.
4. Project name is `appturnity`.
5. Origin and default branch are derived from Git.
6. Powder binding is profile `n8desktop-wsl`, repository `appturnity`.
7. No directory, Git metadata, branch, file, board, card, or protected checkout content changes.

Immediately call `list_projects` after the result.
Require exactly one Project whose canonical root resolves to `/home/natkins/projects/appturnity`.
Require the exact Powder binding in that record.

If Project registration succeeds but the Powder binding is absent, call `bind_project_powder` with the returned `projectId`, connection profile `n8desktop-wsl`, and repository `appturnity`.
Do not create another Project to repair a missing binding.

### Phase 7: Attach the existing Appturnity Captain

Use `attach_captain` with:

- `captainSessionId`: `d3f3535d`.
- `projectId`: the exact Appturnity Project ID returned by registration.
- `shipSlug`: `appturnity`.
- `provider`: `codex`.
- `workspaceTabIds`: `e54319e7-d2e1-40f3-841a-1f16a8c7cb87`.
- `assignment`: comprehensive Appturnity product, customer-interaction, outreach, analytics, UX, architecture, security, reliability, and delivery review.

Do not commission or claim a replacement Captain.
Do not elevate a read-capability terminal.

After attachment, verify:

1. `list_captains` shows Captain `d3f3535d` with the Appturnity `projectId`.
2. `captain_bootstrap` succeeds for `d3f3535d`.
3. The recovered root, Project, Powder profile, Powder repository, ship, Assignment, and empty Crew roster are exact.
4. `project_board_snapshot` for `d3f3535d` resolves the `appturnity` board and reports its current cards honestly.
5. A Captain checkpoint records the new authoritative state.

### Phase 8: Prepare Powder review cards

The observed Appturnity board has zero cards.
`dispatch_crew` requires an authoritative card in the bound board.

If the installed T-Hub catalog still lacks Captain card creation, Cortana or the General must arrange the cards through a sanctioned Powder planning surface.
Do not bypass protected profiles or write directly to Powder storage.

Create only cards whose scope and dependencies are ready.
A recommended review decomposition is:

1. `appturnity-product-capability-review`
   Review current and missing product capabilities, target customers, differentiation, packaging, pricing, conversion paths, and prioritized feature gaps.
2. `appturnity-customer-journey-outreach-review`
   Review acquisition channels, calls to action, consultation scheduling, forms, email, SMS, lifecycle messaging, follow-up, retention, referral, consent, and unsubscribe behavior.
3. `appturnity-analytics-crm-interaction-review`
   Review customer identity, event taxonomy, attribution, CRM integration, lead routing, funnel and cohort reporting, campaign tracking, interaction history, support history, privacy, and data quality.
4. `appturnity-ux-accessibility-seo-performance-review`
   Review desktop and mobile UX, visual quality, accessibility, forms, error states, SEO, structured metadata, performance, content credibility, and conversion friction.
5. `appturnity-architecture-security-reliability-review`
   Review application architecture, data model, APIs, authentication, authorization, secrets, dependencies, tests, CI, observability, deployment, resilience, backups, and operational risk.
6. `appturnity-review-synthesis-roadmap`
   Combine the evidence into a deduplicated capability inventory, gap register, recommended architecture, dependency map, prioritized roadmap, success metrics, and phased execution plan.

Each card must include:

- A bounded read-only or documentation scope.
- Exact repository and worktree rules.
- Required evidence and file pointers.
- Customer and business questions where applicable.
- Security, privacy, legal, spending, outreach, and production escalation rules.
- Definition of done.
- Review criteria.
- Expected final report shape.
- Commit expectations if the review produces a repository document.

The synthesis card must depend on the review cards whose evidence it consumes.
Do not dispatch the synthesis card before its dependencies are complete or explicitly waived by the General.

### Phase 9: Hand control back to the Appturnity Captain

Cortana should checkpoint its own coordination state and the Appturnity ship state.
The Appturnity Captain then performs the normal recovery sequence and confirms:

1. Project identity is authoritative.
2. Powder binding is authoritative.
3. Cards and dependencies are visible.
4. No claim or run is already active unexpectedly.
5. Candidate worktrees are safe and do not include the dirty Windows checkout.
6. The General has permitted the planned delegation level.

Only then may the Appturnity Captain call `dispatch_crew` for the review cards.

## End-to-end acceptance criteria

The unblock is complete only when all of these checks pass.

1. `list_projects` contains exactly one Appturnity Project.
2. Its root resolves to the canonical main worktree `/home/natkins/projects/appturnity`.
3. Its Powder binding is exactly `n8desktop-wsl/appturnity`.
4. Retrying registration returns the same Project ID.
5. The existing `t-hub-app` and `powder` Projects remain unchanged.
6. Captain `d3f3535d` is attached to the Appturnity Project.
7. `captain_bootstrap` succeeds and returns the correct Project, Assignment, ship, board, and Crew roster.
8. `project_board_snapshot` resolves the exact Powder board.
9. Review cards exist with correct dependencies and no foreign-board mutations.
10. The canonical WSL checkout remains clean at the expected commit unless the General separately authorizes a later change.
11. The dirty Windows checkout's status and content remain unchanged.
12. No duplicate Project, Powder repository, Captain, claim, run, branch, or worktree exists.
13. No credential or protected profile detail appears in logs, checkpoints, errors, or plan documents.
14. The Appturnity Captain can perform a sanctioned Crew dispatch preflight against an exact card without Project or board ambiguity.

## Failure and recovery rules

### Registration response is ambiguous

Do not retry immediately with changed arguments.
Call `list_projects` and search by canonical root, Powder binding, and derived remote.
If one matching Project exists, converge on it.
If no matching Project exists, use the same semantic arguments for the retry.
If multiple candidates exist, stop and escalate before mutation.

### Project exists but binding is absent

Preserve the Project.
Use `bind_project_powder` against the exact Project ID.
Do not create a replacement Project.

### Project and binding exist but attachment fails

Preserve the Project and binding.
Reconcile the Captain identity, terminal capability, ship ownership, and existing assignments.
Do not commission another Captain or rewrite the registry.

### Runtime fix fails packaged acceptance

Do not use source-only success to proceed.
Keep Appturnity unbound and Crew undispatched.
Return the exact failed gate to the implementation owner and preserve recoverable state.

### Powder card creation is unavailable

The Project and Captain attachment may still be completed.
Record card creation as the remaining blocker.
Use only a sanctioned Powder operator or agent-authorized planning surface.
Do not emulate cards locally.

### Protected checkout changes unexpectedly

Stop immediately.
Do not clean, reset, stash, commit, or repair it.
Report the observed delta to the General and preserve both checkouts.

## Required Cortana completion report

Cortana's final report should include:

- Installed T-Hub version and reviewed source commit.
- Path-fix Powder card and run, if created.
- Fix commit, review decision, and packaged acceptance evidence.
- Appturnity Project ID.
- Stored canonical Project root.
- Powder profile and repository names without protected connection details.
- Existing Captain terminal and successful attachment evidence.
- `captain_bootstrap` and `project_board_snapshot` outcomes.
- Created review card IDs and dependency order.
- WSL checkout HEAD and final status.
- Confirmation that the dirty Windows checkout was untouched.
- Any skipped checks, remaining blockers, or residual risks.
- The next ordered action for Captain `d3f3535d`.

## Focused file map

- `docs/APPTURNITY-PROJECT-LINK-CORTANA-RUNBOOK.md`: this operational handoff.
- `docs/CAPTAIN-AUTONOMY-AND-SCOPED-GRANTS-PLAN.md`: longer-term Captain grant and self-bootstrap program.
- `docs/REVIEW-INDEX.md`: canonical document reading order.
- `docs/PHASED-PRODUCTION-PLAN.md`: canonical T-Hub roadmap and dependency ownership.
- `docs/CAPTAIN-POWDER-HANDOFF.md`: verified T-Hub and Powder execution contracts.
- `docs/POWDER-INTEGRATION.md`: current Powder integration limits.
- `docs/cli-contract.md`: public CLI rules.
- `apps/desktop/src-tauri/src/control.rs`: Project registration, authority, persistence, attachment, and checkpoint handlers.
- `apps/desktop/src-tauri/src/files.rs`: shared WSL and host path conversion helpers.
- `apps/desktop/src-tauri/src/git.rs`: WSL Git adapter and main-worktree path contract.
- `/home/natkins/projects/appturnity`: canonical Appturnity WSL checkout.
- `/mnt/c/Users/natha/Projects/Consulting/appturnity`: protected dirty Windows checkout that must remain untouched.

## Kickoff prompt for Cortana

Read `docs/APPTURNITY-PROJECT-LINK-CORTANA-RUNBOOK.md` after recovering your durable Cortana checkpoint and the canonical documents in `docs/REVIEW-INDEX.md`.
Reconcile it with the active T-Hub Captain goal and current Powder cards before assigning work.
Your immediate task is to get the installed cross-platform Project registration defect reproduced, owned, fixed, independently reviewed, and accepted in the packaged Windows runtime.
Then register `/home/natkins/projects/appturnity` as one T-Hub Project bound to `n8desktop-wsl/appturnity`, attach existing Captain `d3f3535d`, arrange the dependency-ready deep-review cards, checkpoint both ships, and hand control back to the Appturnity Captain.
Do not create duplicate Projects, boards, Captains, or cards, and do not touch `/mnt/c/Users/natha/Projects/Consulting/appturnity`.
Follow `AGENTS.md`, commit each verified logical change separately, and do not merge, push, install, deploy, publish, or release without the authority applicable to your current goal.
