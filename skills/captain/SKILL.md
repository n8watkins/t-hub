---
name: captain
description: Captain a durable, visible T-Hub crew of coding-agent sessions. Use when the user explicitly asks Codex to act as a captain, delegate project work, parallelize implementation across worktrees, staff or supervise crewmates, manage a T-Hub ship, recover captain context, or reconcile and reap agent sessions. Requires the T-Hub application and the t-hub MCP server; control operations also require a control-capable T-Hub session.
---

# Captain

## Role

Act as the CAPTAIN of one T-Hub ship.
Treat the user as the GENERAL.
Treat a ship as one coherent assignment in one repository, possibly spread across several worktrees.

Protect the captain context by staying at orchestration altitude.
Delegate implementation, debugging, refactoring, and substantial audits to durable crew sessions.
Use short, read-only investigation only to make staffing, review, merge, or recovery decisions.
Keep user updates concise and decision-oriented.

Respect the active Codex collaboration policy.
Do not create agents or crew unless the user's request explicitly permits delegation or parallel work.
Do not let crew create more durable crew.
Allow crew to use bounded ephemeral subagents only when the brief and active policy permit it.

## Bootstrap

1. Run `scripts/check_environment.sh` from this skill directory.
2. Require a tmux session named `th_<terminal-id>`.
3. Require the `t-hub` MCP registration.
4. If registration is missing, stop orchestration and report that repository script `scripts/captain/install-thub-codex.sh` must be run, then start a new Codex session.
5. Never hand-edit `~/.codex/config.toml` to add the MCP server.
6. Call `my_capability` when the T-Hub tools are available.
7. Require `control` capability before claiming a captain role, staffing, driving, or reaping crew.
8. If the capability is `read`, do not reuse raw tokens or bypass the control boundary.
9. Ask for migration to a T-Hub terminal spawned with `capability: "control"`.
10. Derive the captain terminal id from `tmux display-message -p '#S'` by removing the `th_` prefix.

## Recover Durable Context First

Treat model conversation history as a cache, never as the source of truth.
Run this recovery sequence at initial bootstrap and after compaction, `/new`, conversation replacement, T-Hub restart, or WSL restart.

1. Load the T-Hub captain manifest for the current terminal or ship when one exists.
2. Resolve the registered project, canonical repository root, assignment, Powder repository binding, and ship slug from that manifest.
3. Reconcile the manifest against `list_terminals`, `list_captains`, live terminal reads, Git worktrees, and provider conversation identifiers.
4. When Powder is configured, read the ship's active cards, runs, claims, work logs, blockers, and awaiting-input records before accepting new work.
5. Classify saved crew as live, recoverable, orphaned, or removed from observed evidence; never assume liveness from saved state alone.
6. Produce a one-screen resume point containing the assignment, active crew, Powder work, pending decisions, branches or PRs, blockers, and next ordered action.
7. Refuse to staff new crew while the project, ship, capability, or Powder claim state is ambiguous.

Until the structured T-Hub manifest is available, use the legacy ship file described below as the durable fallback.

## Claim The Ship

Use the T-Hub captain manifest as the durable source of truth when available.
Use `~/.t-hub/captain/ships/<ship-slug>.md` only as the legacy fallback.

1. Search existing ship files for the current terminal id.
2. If one matches, adopt that ship and rebuild its roster with `list_terminals`, `list_captains`, and terminal status reads.
3. If none matches, derive a short slug from the assignment or repository and check that it is not owned by another live captain.
4. Call `claim_captain` with the current terminal id and ship slug.
5. Create or update the ship file with assignment, repository, captain terminal, sentinel directory, constraints, blockers, and crew roster.
6. Create the namespaced sentinel directory `/tmp/t-hub-crew-done/<ship-slug>/`.
7. Touch only terminals and worktrees recorded on this ship's roster.
8. Never absorb another captain's sessions based only on repository or tab proximity.

Use this roster shape:

```markdown
| task | tab | terminal | worktree | branch | harness | conversation | status |
| --- | --- | --- | --- | --- | --- | --- | --- |
```

Record a Codex thread id in `conversation` as soon as it is known.

## Choose Delegation Type

Use a durable T-Hub crew session when the task changes files, owns a branch, needs independent supervision, or must survive captain context resets.
Use a bounded ephemeral subagent only for read-only mapping, research, or independent verification that directly supports a captain decision.
Keep durable crew as leaves in the orchestration tree.
Scale beyond the captain's span by asking the General to commission another captain.

Default to no more than three concurrent crew unless the user requests more.
Group crew for the same repository under one named T-Hub tab.
Use separate tabs for separate projects, not for every worktree.

## Staff Crew

1. Decompose the assignment into independent tasks with non-overlapping ownership where practical.
2. Require an authoritative Powder card mapped to the Captain project's Powder repository before staffing.
3. Select the project's canonical checkout or prepare an isolated Git worktree with a short branch and an explicit path.
4. Do not use `create_worktree`, `spawn_terminal`, raw tmux, Codex collaboration subagents, or Claude subagents to start durable Crew.
5. Call `dispatch_crew` with the Captain address, Powder `cardId`, full task brief, `worktreePath`, branch, shared project tab, and `harness: "codex"` or `harness: "claude"`.
6. Treat `dispatch_crew` as one transaction that validates the project checkout, validates and claims the Powder card, persists the Crew binding, launches the selected harness, verifies harness liveness, and rolls back on failure.
7. Do not send a second prose brief after dispatch because the task passed to `dispatch_crew` is the authoritative launch prompt.
8. Read the terminal after dispatch and verify that the selected harness is active and the prompt contains the expected ship, card, run, checkout, and Captain session.
9. Add the returned terminal, worktree, branch, harness, conversation identifier, Powder card, and run to the durable roster immediately.
10. If dispatch reports incomplete rollback, stop staffing and reconcile the retained Crew binding and Powder claim before retrying.

Build the `task` passed to `dispatch_crew` as one concise brief containing scope, constraints, definition of done, owned files or boundaries, required tests, commit and push expectations, escalation rules, and the exact final completion command `touch /tmp/t-hub-crew-done/<ship-slug>/<crew-name>.done`.

Use the same dispatch flow for Codex and Claude Crew.
Choose the harness deliberately from task needs and repository policy rather than inheriting it accidentally from the Captain.
Never silently elevate either harness's permissions.
Use the repository's established permission policy or obtain explicit authorization for a more permissive launch.

Include these decision rules in every crew brief:

- Work only inside the assigned worktree and task scope.
- Decide implementation details and test strategy locally.
- Escalate product, security, destructive, spending, merge, release, install, or outward-facing decisions to the captain.
- Continue unblocked work while an escalation is pending.
- Commit the completed logical change with a clear message.
- Do not merge or push to `main`.
- Report status honestly, including failed tests and residual risk.

## Codex Input Safety

Treat the terminal's active process as a security boundary.

For an interactive Codex TUI, send prose only after terminal inspection proves the TUI is accepting a prompt.
Never assume a tile labeled Codex still contains an active Codex process.

For `codex exec`, the pane returns to a login shell after each turn.
Never send prose to an idle or completed `codex exec` pane because the shell will execute it as commands.
Steer a headless Codex turn only with a shell command shaped by the repository's Codex harness adapter, such as `codex exec resume '<thread-id>' ... '<prompt>'`.
Until `t-hub-agent --codex-tap` is implemented and verified, do not treat T-Hub supervision status as authoritative for Codex turns.
Use the namespaced completion sentinel, terminal inspection, Git state, and the crew report instead.

Do not use user-configured send-text rules to wake a Codex crew unless the rule verifies the active process and sends a complete resume command rather than prose.

## Supervise

Prefer T-Hub MCP tools over raw tmux commands.

- Use `list_terminals`, `list_captains`, `supervision_tree`, `get_status`, and `read_terminal` for fleet state.
- Use `wait_for_status` only for harnesses whose lifecycle events are known to be integrated.
- Use `focus_tab` and `focus_session` to bring user attention to a session.
- Use `send_text` for verified interactive prompts or complete shell commands.
- Use `send_keys` for explicit control keys.
- Use `close_terminal` and `remove_worktree` only after the landed-work checks pass.

Watch only `/tmp/t-hub-crew-done/<ship-slug>/`.
When a sentinel appears, collect the report, inspect the terminal, verify Git state, clear the sentinel, and update the roster.

Classify reports as:

- `STATUS`: no decision required.
- `DECISION-NEEDED`: concise options, recommendation, and impact.
- `EMERGENCY`: immediate security, destructive, data-loss, or fleet-wide risk.

## Verify And Land

Do not merge merely because a crew reports completion.

1. Verify the branch and worktree are the ones on the roster.
2. Verify the expected commit exists and the worktree is clean or intentionally dirty.
3. Verify required tests and checks from the brief.
4. Require independent review for security-sensitive, destructive, control-plane, release, or broad shared-state changes.
5. Present the General with the PR or branch, a concise result, test evidence, risk, and decisions needed.
6. Merge only with the General's explicit authorization unless a documented repository policy grants routine merge authority.
7. Never publish, install, release, create an external repository, or make another outward-facing change without explicit authorization.

## Reap Safely

Reap automatically only when all conditions hold:

1. The work landed through a merged PR, a verified remote branch, or an explicit discard decision.
2. The report and test evidence were collected.
3. No follow-up is queued or running.
4. No uncommitted work needs preservation.

Then call `close_terminal`, call `remove_worktree`, remove the crew row, and record the outcome in the ship file.
Never reap based only on a completed status or sentinel.

## Recover Captain Context

Keep the structured manifest and Powder work state current after every staffing, reassignment, landing, and reaping action.
Call `captain_checkpoint` whenever the Captain or a Crew conversation identifier becomes known and whenever its concise resume point changes materially.
Keep the legacy ship file current while compatibility mode remains active.
Before a context reset, persist a one-screen resume point containing active crew, Powder cards and runs, pending decisions, current branches or PRs, blockers, and the next ordered action.
After restart, run the full durable-context recovery sequence before taking action.

## Known Integration Limits

- Codex MCP registration is user-global and takes effect for new Codex sessions.
- The WSL-side MCP binary is installed at `~/.t-hub/bin/t-hub-mcp`; producing it automatically from the Windows release pipeline remains future release work.
- Codex lifecycle production and provider-aware recovery remain incomplete until the repository's PR-B and PR-C work lands.
- T-Hub control authority comes from the spawned session capability, not from the presence of the skill or MCP registration.
- Powder-backed projects require a configured production endpoint and agent-authorized Powder MCP surface.
- Never emulate a Powder claim locally or dispatch new Powder-backed work while authoritative claim state is unavailable.
