---
name: handoff
description: Create or update a zero-context project handoff for another coding-agent session. Use when the user asks to hand off, preserve session state, prepare for a context reset, or produce a continuation prompt across Codex and Claude.
---

# Handoff

Create a factual, reset-safe handoff without taking ownership of unrelated work.

## Establish Scope

1. Read the repository instructions and capture `git status --short`, the current branch, recent commits, and remotes before changing anything.
2. Separate this session's work from pre-existing or user-owned changes.
3. Never stage, commit, revert, or rewrite unrelated changes.
4. Never infer permission to push from prior pushes.
   Push only when the user explicitly authorized it for the current work.

## Select The Document

1. Follow a handoff path named by repository instructions or the user.
2. Otherwise, search tracked files for handoff documents and inspect likely candidates before selecting the one whose scope matches the current project or workstream.
3. Do not replace an established scoped handoff with a generic `docs/HANDOFF.md`.
4. If multiple candidates are plausible and none is clearly canonical, report the ambiguity and ask before overwriting one.
5. When no handoff exists, create `docs/HANDOFF.md` if `docs/` is established, otherwise create `HANDOFF.md`.

Never edit generated files, generated sections, changelogs, or roadmap/status documents merely to make the handoff appear current.
Link to authoritative project documents instead.

## Record Evidence

Write for an agent with no conversation history.
Distinguish verified facts, inferences, and unknown external state.
Include:

- Project purpose, relevant architecture, and operating environment.
- Work completed, with commit hashes and intentionally uncommitted files.
- Verification commands and exact outcomes, including failures or skipped checks.
- Runtime and deployment state observed directly, with timestamps or identifiers when relevant.
- External dependencies, credentials, services, approvals, and reachability that were verified or remain blocked.
- Ordered next steps with acceptance criteria and file pointers.
- Decisions already made, constraints, risks, and commands that are known to work.
- A focused file map.

Do not include secrets, tokens, credential commands that reveal secrets, or sensitive terminal output.
Keep each full Markdown sentence on its own physical line unless repository instructions require another format.

## Preserve Durable Agent State

If this is a registered T-Hub Captain or Crew session, use `captain_checkpoint` when the available T-Hub capability permits it.
Store a concise resume point with active agent sessions, branches or PRs, pending decisions, blockers, and the next ordered action.
Include the harness conversation identifier only when it is known from a trusted runtime source.
Do not invent identifiers or claim a checkpoint succeeded when the tool was unavailable or refused it.

When the session has a durable agent session, append a concise progress checkpoint through the authorized T-Hub agent-session surface if one is available.
Never retrieve or pass legacy service credentials in a prompt, command argument, environment dump, handoff document, or checkpoint.
If no sanctioned checkpoint surface is available, record that the durable session checkpoint was not updated and make that limitation part of the handoff's external-state evidence.

## Commit And Report

Follow repository commit requirements, but stage the selected handoff paths explicitly.
Keep handoff-only documentation changes in a dedicated commit when repository policy permits.
Do not commit merely because this skill was invoked if the user requested only an in-chat handoff.

End with:

1. A concise summary for the user.
2. The handoff path and commit, if any.
3. Verification, runtime, checkpoint, and push status.
4. A short kickoff prompt naming the files to read first, the next task, acceptance checks, and repository commit/deploy rules.
