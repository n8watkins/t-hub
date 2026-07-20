# History Contract

## Purpose

History is the durable, provider-neutral catalog of agent conversations that T-Hub can inspect, resume, recover, archive, or report as incompatible.
History replaces the Claude-only Recent model without changing provider transcripts into T-Hub's source of organizational authority.
The canonical product sequence and exit gate remain in [PHASED-PRODUCTION-PLAN.md](./PHASED-PRODUCTION-PLAN.md).

## Identity

Every History entry has one opaque `historyId` owned by T-Hub.
Version 1 computes it as `history:v1:` followed by the lowercase SHA-256 digest of the length-prefixed canonical Harness name and exact native conversation identity.
The digest input is the big-endian unsigned 32-bit UTF-8 byte length of the Harness name, its UTF-8 bytes, the big-endian unsigned 32-bit UTF-8 byte length of the conversation identity, and its UTF-8 bytes in that order.
Working directory, provider label, title, timestamps, role, and organizational joins never affect that digest.
If two distinct identity tuples ever produce the same digest, the backend fails the conflicting entries closed as recovery-required and reports source degradation instead of selecting one.
Neither a working directory nor a bare provider UUID is globally unique.
Two conversations in the same directory remain distinct, including two conversations from the same Harness.
The same UUID under two Harnesses remains distinct.

`harness` selects lifecycle behavior and currently supports `codex` and `claude`.
`provider` is informational model or account provenance and never selects a resume command.
`conversationId` is the exact Harness-native resume handle.
`providerSessionId` records the provider-native runtime identity when known and may currently equal `conversationId`.
The contract does not require those fields to remain equal in future Harness versions.

## Catalog

The backend exposes a versioned `history_list` operation.
The legacy `recent_sessions` and `archive_recent_project` operations remain Claude-only compatibility surfaces until their callers are retired.

```json
{
  "schemaVersion": 1,
  "generatedAt": "2026-07-15T12:00:00Z",
  "revision": "opaque-revision",
  "entries": [],
  "count": 0,
  "total": 0,
  "truncated": false,
  "sources": [
    {
      "harness": "codex",
      "status": "ready",
      "reason": null
    }
  ]
}
```

`count` always equals `entries.length`.
`total` is the number of matching entries known before the result limit is applied.
A degraded or unavailable source keeps its status visible, and `total` is then the known lower bound rather than a claim of global completeness.
Entries sort by `lastSeenAt` descending and then `historyId` ascending.
Sources sort by canonical Harness name ascending.
`revision` is a SHA-256 digest of the normalized sorted catalog, action compatibility, archive overlay, durable joins, and source statuses, and changes whenever any returned fact changes.

Every schema field is present in every entry.
Unavailable scalar identity and organizational fields are represented as `null` rather than omitted.

One complete entry has this shape:

```json
{
  "historyId": "history:v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "harness": "codex",
  "provider": null,
  "providerSessionId": "019f61f5-b42b-7d01-9602-10def2d72fc0",
  "conversationId": "019f61f5-b42b-7d01-9602-10def2d72fc0",
  "cwd": "/home/natkins/projects/tools/t-hub/t-hub-app",
  "projectId": null,
  "projectName": null,
  "captainId": null,
  "role": null,
  "workspaceId": null,
  "worktreeId": null,
  "branch": null,
  "label": "Repair provider-neutral History",
  "lastText": null,
  "startedAt": "2026-07-15T12:00:00Z",
  "lastSeenAt": "2026-07-15T12:30:00Z",
  "continuityState": "resumable",
  "actions": {
    "focus": { "status": "unavailable", "reason": "Conversation is not active." },
    "resume": { "status": "supported", "reason": null },
    "recover": { "status": "supported", "reason": null },
    "archive": { "status": "supported", "reason": null },
    "unarchive": { "status": "unavailable", "reason": "Conversation is not archived." }
  }
}
```

Each entry contains these fields:

- `historyId`: opaque T-Hub identity.
- `harness`: exact Harness selector.
- `provider`: nullable informational provider name.
- `providerSessionId`: nullable provider-native runtime identity.
- `conversationId`: exact Harness-native resume handle.
- `cwd`: the recorded working directory.
- `projectId` and `projectName`: nullable durable Project join.
- `captainId`: nullable durable Captain identity, never a terminal ID substitute.
- `role`: nullable organizational role.
- `workspaceId`: nullable durable Workspace join.
- `worktreeId` and `branch`: nullable authoritative worktree identity.
- `label`: bounded human-readable title.
- `lastText`: nullable bounded activity preview.
- `startedAt`: nullable ISO 8601 timestamp.
- `lastSeenAt`: required ISO 8601 timestamp.
- `continuityState`: `active`, `resumable`, `archived`, or `recoveryRequired`.
- `actions`: structured focus, resume, recover, archive, and unarchive compatibility.

Each action status is `supported`, `unavailable`, `incompatible`, or `unknown` and may include a bounded reason.
Continuity state is separate from work state and runtime health and never replaces the two axes in [STATUS-MODEL.md](./STATUS-MODEL.md).
History rows render any available work and runtime labels using the shared status precedence.
One unavailable provider source produces a degraded source record instead of a false global empty catalog.
Malformed or future records degrade per entry or source and do not erase healthy entries from another Harness.

## Harness Adapters

Each Harness adapter owns native transcript discovery, compatibility, resume arguments, and archive capability.
The central History service merges normalized adapter entries and joins durable T-Hub identity.
It never infers Project, Captain, role, Workspace, or worktree authority from a path alone.
Durable organizational joins require a unique match on canonical Harness plus exact `providerSessionId` or `conversationId` against the registry.
An ambiguous, duplicate, or cross-Harness match leaves every organizational join null and sets recovery-required compatibility.
The backend never breaks a join tie with cwd, basename, terminal ID, timestamp, or display text.

Claude discovery reads the existing transcript catalog under `~/.claude/projects`.
The transcript filename stem is the Claude conversation identity.
Codex discovery reads date-partitioned `~/.codex/sessions/**/rollout-*.jsonl` records.
The Codex parser selects exactly one `session_meta` whose payload identity matches the rollout filename identity.
This rule prevents an inherited parent `session_meta` in a subagent rollout from replacing the child identity.
No matching record or multiple conflicting matching records cause that rollout to be skipped and the Codex source to be marked degraded with a bounded reason.
The parser never falls back to arbitrary parent metadata.
Filename identity extraction supports only fixture-locked rollout name versions, and an unknown filename format is incompatible rather than guessed.
Codex user-visible labels use normalized `event_msg` user messages rather than duplicated response items.

Catalog bounds apply fairly after Harness and Project grouping.
`HISTORY_ENTRY_LIMIT` is 500, `HISTORY_SOURCE_LIMIT` is 32, `HISTORY_LABEL_MAX_CHARS` is 120, `HISTORY_LAST_TEXT_MAX_CHARS` is 240, and `HISTORY_REASON_MAX_CHARS` is 240.
No chatty directory may evict every other Project or Harness.
Source scanning must remain bounded and must not add sustained hidden-surface CPU or repeated Windows-to-WSL process churn.

## Resume and Recovery

The backend exposes `history_resume` with `historyId`, stable `requestId`, and an optional target tab.
The frontend never supplies an executable command, Harness override, cwd override, or bare conversation identity as authority.
The backend re-resolves `historyId`, verifies compatibility and exact transcript identity, and delegates to the selected Harness adapter.
Claude resumes with the adapter-owned equivalent of `claude --resume <conversationId>`.
Codex resumes with the adapter-owned equivalent of `codex resume <conversationId>`.

An explicit History Resume action always resumes the selected conversation.
The Claude-specific passive `resumeStartsClaude` setting must not silently turn an explicit History action into an ordinary shell.
An unknown, missing, archived, incompatible, or ambiguous identity spawns nothing and returns a structured recovery state.
Each request ID is durably bound to operation name, `historyId`, and normalized arguments.
Reusing a request ID with a different action, entry, or target tab returns a structured conflict rather than replaying another result.
The frontend retains one request ID through ambiguous response recovery and request-status polling.
After process restart or reservation reap, the backend re-probes the exact terminal or archive reality before applying the action again.
In-flight and completed-request identity is never keyed by cwd or bare session ID.

The backend exposes `history_focus` with `historyId` for an entry whose continuity state is active.
It re-resolves the exact Harness and conversation identity to one authoritative live terminal and then uses the existing focus operation internally.
It never accepts or infers a terminal target from cwd, bare conversation ID, timestamp, or frontend state.
A stale, missing, ambiguous, legacy, or cross-Harness binding focuses nothing and returns a structured unavailable or recovery-required result.

`history_recover` follows the same identity and idempotency rules.
Recovery is limited to re-scanning adapter evidence, re-reading exact registry identity, reconciling a provable archive overlay, and reporting the resulting compatibility.
It does not create or rebind a Project, Captain, Workspace, worktree, or provider identity.
Organizational mutation continues through its existing separately authorized and reviewed operation.

## Archive

The backend exposes `history_archive` and `history_unarchive` with `historyId` and stable `requestId`.
Archive targets one exact conversation.
It never archives every conversation in a working directory.
It never deletes provider transcripts.

The preferred first implementation is a non-destructive T-Hub archive overlay keyed by `historyId` and persisted atomically in protected T-Hub state.
Archived entries remain visible in the catalog and can be unarchived.
An archived entry cannot resume directly.
Its resume action is unavailable until `history_unarchive` succeeds.
Legacy Claude directories under `~/.claude/projects-archive` are enumerated as per-conversation archived entries even though the old operation moved a whole cwd directory.
The first archive overlay migration records those exact conversations without moving or rewriting their transcripts.
Provider-specific transcript movement may be added only when the adapter can prove exact ownership, reversibility, and compatibility.
An archive failure remains visible and must not become a permanent optimistic local hide.

## Active Conversation Filtering

An active conversation always remains in the catalog with `continuityState: active`.
Its focus action is supported and its resume action is unavailable unless a separately reviewed fork behavior is added later.
An open cwd does not hide any historical conversation from that directory.
A legacy tile without exact provider identity leaves History entries visible with an explicit compatibility state.

## Cache and Migration

The old `th.recent.cache.v1` array is discarded because it has no Harness identity and stores conversation text.
The old `th.recent.hidden.v2` set is not migrated because its cwd-wide meaning would hide unrelated conversations.
The backend remains the catalog cache authority.

If cold rendering later requires a local index, it uses a validated `th.history.index.v1` envelope with schema version, saved time, revision, and non-content identity fields.
Unknown, malformed, expired, or oversized envelopes are discarded atomically.
Local storage must not persist `lastText`, credentials, prompts, executable commands, or provider tokens.

## CLI, MCP, and UI Parity

Graphical, CLI, MCP, and Cortana flows call the same backend catalog and action operations.
The CLI remains noninteractive and follows [cli-contract.md](./cli-contract.md).
Machine-readable responses use the stable envelope and structured error taxonomy.
No surface reconstructs resume commands or archive targets independently.
An unknown future Harness name may remain visible in the catalog, but every executable action is incompatible until a matching adapter is installed.

## Verification

The source gate requires:

- Claude and Codex adapter fixtures for identity, cwd, timestamps, labels, malformed records, missing fields, and future formats.
- A Codex subagent fixture with inherited parent metadata that still resolves the filename-matching child identity.
- Merge tests proving multiple conversations and both Harnesses survive in one cwd.
- Durable join tests that require Harness plus exact conversation identity.
- Exact provider-specific resume integration with no spawn on uncertain identity.
- Active focus tests for exact match, stale terminal, ambiguous binding, legacy tiles, and the same bare identity under different Harnesses.
- Stable request replay and rapid double-click tests.
- Per-conversation archive, unarchive, failure, and restart tests.
- Cache disposal and malformed-cache tests.
- Loading, empty, partial, degraded, success, keyboard, accessibility, narrow-layout, and high-DPI component coverage.
- Scan latency, CPU, and Windows-to-WSL process-churn measurements.

Packaged Windows acceptance creates disposable Claude and Codex conversations in the same repository, records their exact identities, restarts T-Hub and WSL, resumes each exact conversation, archives one without affecting the others, and verifies provider files remain intact.
Phase 9 History is complete only after that packaged dual-Harness acceptance passes.
