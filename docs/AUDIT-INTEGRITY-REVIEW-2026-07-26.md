# Audit integrity implementation review

Date: 2026-07-26

Status: Implemented and validated on PR #80, `fix/audit-integrity-fail-closed`.

## Purpose

This document records the fourteen areas that the first audit-integrity implementation did not handle completely.
It explains the failure mode, operational risk, implemented correction, and verification evidence for each area.
It is intended to make the review findings reusable during future security, persistence, and release work.

The final design is implemented primarily in:

- `apps/desktop/src-tauri/src/audit.rs`
- `apps/desktop/src-tauri/src/control.rs`
- `apps/desktop/src-tauri/src/control/tests.rs`
- `apps/desktop/src-tauri/src/governor.rs`
- `docs/SOCKET-AUTH-DESIGN.md`

## Final contract

Process-changing commands must not produce side effects unless their authorization record and integrity metadata are durable.
Audit state is authenticated with HMAC-SHA256 under a persistent key stored outside the log directory.
The authenticated head manifest commits every retained audit segment, and the protected key state commits the latest manifest generation.
Audit writes use a recoverable journal so startup can roll forward authenticated partial commits.
Startup recovery, live authorization, and on-demand verification fail closed when integrity cannot be established.
Live audit files roll into bounded authenticated segments so reauthentication work is bounded.
Development and production derive isolated audit keys, checkpoints, manifests, and journals.
Authenticated refusal records are rate-limited before durable work to prevent write amplification.

## Review summary

| Area | Primary risk | Resolution |
| --- | --- | --- |
| 1. Recoverable, isolated, rate-limited state | False quarantine, unrecoverable partial commit, verification denial of service | Isolated state paths, commit journal, cached and single-flight verification |
| 2. Recovery gaps and local symlink hygiene | Empty unanchored files, repeated failed scans, accidental local-file tracking | Journal-before-create recovery, failed-result caching, exact ignore rule |
| 3. Live recovery and integrity gaps | Non-durable recovery and undetected current-file tampering | Directory durability and live pre-authorization validation |
| 4. Complete live validation | Historical tampering accepted after writer open | Validate every retained segment with bounded current-file checks |
| 5. Durable directory creation | Entire fresh audit root could disappear after power loss | Sync every newly created directory entry through its parent |
| 6. Retryable durability barriers | A transient sync failure could remain permanently uncorrected | Retry incomplete barriers and cache only successful directory identities |
| 7. Safe append transitions | Concurrent replacement could become a trusted baseline | Prove writer identity, expected growth, and exact append transition |
| 8. Prefix authentication and truncation | Same-length mutation or missing newline could be accepted | Authenticate prior bytes and reject incomplete terminal records |
| 9. Durable recovery with incremental validation | Recovered record could be lost after checkpoint advance | Sync recovered records and retain incremental authenticated state |
| 10. Post-append reauthentication | Advisory locks could not exclude a non-cooperating writer | Reauthenticate content after append before advancing trust |
| 11. Bounded cross-platform reauthentication | Windows lock conflict and quadratic daily work | Validate through the locked handle and rotate bounded segments |
| 12. Collision-free derived paths | Different audit roots could share state files | Injective suffix derivation with authenticated migration |
| 13. Persistent-state and migration authentication | Key-state tampering or ambiguous legacy ownership could be laundered | Revalidate persistent state and require proof of migration ownership |
| 14. Refusal-write amplification control | Read-token caller could exhaust durable audit I/O | Rate-limit refusal audit admission before durable writes |

## 1. Recoverable, isolated, and rate-limited audit state

### Miss

The initial implementation honored `T_HUB_AUDIT_DIR` for log files but derived the key and checkpoint from the production default path.
Development and production could therefore update shared integrity state while writing different logs.
The record, manifest, and checkpoint were also committed as separate writes without a recoverable transaction.
The read-tier `audit_verify` command could repeatedly scan all retained history while holding the audit mutex.

### Risk

Running development and production together could make each instance classify the other instance's valid writes as tampering.
A crash between record, manifest, and checkpoint writes could leave an authenticated but internally inconsistent state that quarantined future commands.
A read-token holder could consume unbounded disk and mutex time through repeated full verification.

### Resolution

Audit state paths are derived from the configured audit directory so development and production are isolated.
The commit path writes an authenticated pending journal and rolls it forward through durable record, manifest, and checkpoint stages.
Verification results are cached and reused when the relevant file-state stamps have not changed.
Concurrent verification requests share the same protected audit state instead of independently forcing unbounded scans.

### Evidence

The recovery suite exercises a crash after each durable stage.
Development defaults now point audit state at the development root.
Focused verification tests demonstrate that unchanged state does not trigger repeated full scans.

Primary commits: `49aba563`, `ea39f75a`.

## 2. Recovery gaps and local symlink hygiene

### Miss

A new day file could be created before the recovery journal was durable.
A crash in that window left an empty unanchored JSONL file that startup classified as injected history.
Only successful verification reports populated the cache.
The automated fix workflow also began tracking the machine-local `CLAUDE.md` symlink.

### Risk

A normal crash during first use of a day could permanently quarantine otherwise valid audit state.
After a real integrity failure, repeated `audit_verify` calls could continue rescanning all retained files.
Tracking the local symlink would introduce machine-specific repository state unrelated to the security change.

### Resolution

The pending journal is made durable before a new audit file is created.
Recovery recognizes and completes only journal-authorized creation.
Failed verification reports are cached after the sink is poisoned.
The repository ignores the exact root path `/CLAUDE.md`, while the working-copy symlink remains intact and untracked.

### Evidence

Recovery tests cover the pre-create and empty-file windows.
`failed_verification_is_cached` verifies that a poisoned sink does not repeatedly scan history.
`git ls-files CLAUDE.md` returns no tracked path, and `.gitignore` contains the exact root rule.

Primary commits: `51ec77eb`, `5de29f14`.

## 3. Live audit recovery and integrity gaps

### Miss

Crash recovery could create a missing audit file and sync its contents without syncing the parent directory entry.
Once the current day's writer was open, later process-changing authorization did not revalidate that live file.

### Risk

A second crash could remove the recovered file after the manifest and checkpoint had advanced.
External truncation or modification of the current file could remain undetected while later process-changing commands executed.

### Resolution

Recovery durably syncs the directory entry for every newly created file.
Each process-changing authorization validates durable live state before committing its authorization record.
Unexpected file replacement, truncation, or mutation poisons the sink and refuses dispatch.

### Evidence

`live_process_authorization_rejects_modified_or_truncated_day_file` covers live mutation and truncation.
Recovery tests verify that roll-forward creates and syncs the expected durable file.

Primary commits: `3b2f1c1e`, `a32e3b5b`.

## 4. Complete live audit-file validation

### Miss

The first live check validated only the currently open day file.
Earlier anchored days could be modified or removed after startup without blocking a later command.
The straightforward correction rescanned the complete current-day file before every authorization.

### Risk

Historical tampering could be accepted after the writer was opened.
Full current-day parsing on every command made cumulative work quadratic as the daily record count grew.

### Resolution

Live validation includes every retained authenticated audit file.
Immutable historical segments use identity and metadata stamps to avoid unnecessary content work.
The active segment uses bounded append-transition validation and rotates before its validation cost can grow without limit.

### Evidence

`live_process_authorization_rejects_historical_day_tampering` covers modified and removed historical state.
`cached_verification_avoids_repeated_full_scans` covers unchanged-history reuse.

Primary commit: `b787c721`.

## 5. Durable audit-directory hierarchy creation

### Miss

Creating the audit directory and syncing that directory did not make its own parent entry durable.
On a fresh profile, the complete `.t-hub` directory tree could exist only in volatile filesystem metadata.

### Risk

A power loss could remove the key, checkpoint, manifest, and logs together after a commit had reported success.
Startup could then initialize a clean audit state and lose evidence without detecting a partial rollback.

### Resolution

Directory creation records each missing component.
Every newly created directory is synced, and its parent is synced to make the directory entry durable.
The operation completes only after the complete created hierarchy has crossed its durability barriers.

### Evidence

`persistent_key_creation_durably_creates_nested_directories` covers fresh nested state creation.
The implementation uses `create_dir_all_durable_cached` for audit-owned state paths.

Primary commit: `35914eab`.

## 6. Retryable directory durability barriers

### Miss

If directory creation succeeded but syncing its parent failed, restart saw an existing directory and skipped the failed barrier.

### Risk

A transient I/O failure could leave a directory entry permanently non-durable while later commits reported success.
A subsequent power loss could erase the complete audit root.

### Resolution

Existing directory identities are checked against the cache of completed barriers.
Only successfully completed barriers are cached.
Changed directory identities and previously failed barriers are retried.

### Evidence

`durable_directory_cache_refreshes_changed_identities` verifies that cached barriers do not conceal identity changes.
Focused failure injection verifies that an incomplete barrier is attempted again.

Primary commit: `40f3d750`.

## 7. Safe append transitions and cached durability barriers

### Miss

The post-commit file stamp could accept a replacement or mutation that occurred between pre-append validation and post-append snapshotting.
The initial durable-directory helper also resynced the complete ancestor hierarchy for every journal, manifest, checkpoint, and temporary-file write.

### Risk

Concurrent tampering could become the next trusted baseline even though the manifest described bytes written through a different file handle.
Repeated high-level directory syncs multiplied synchronous I/O for every authorization.

### Resolution

Append validation proves that the path still identifies the writer's file and that the transition is the exact expected growth.
Trust advances only after the expected append transition is established.
Completed directory barriers are cached by directory identity and retried only after failure or identity change.

### Evidence

`append_transition_rejects_a_replaced_live_path` covers path replacement.
Directory-cache tests cover reuse, failure retry, and identity refresh.

Primary commit: `3fa72d42`.

## 8. Authenticated prefixes and truncated-record rejection

### Miss

File identity and length growth did not detect a same-length in-place modification of bytes that existed before the append.
Verification accepted a nonempty JSONL file whose final record lacked a trailing newline.

### Risk

Modified historical bytes could be incorporated into the trusted post-append snapshot.
The next append could concatenate JSON onto an incomplete record while the head advanced, leaving malformed history.

### Resolution

The append protocol authenticates the preexisting prefix and proves that those bytes remain unchanged across the transition.
Verification treats a nonempty file without a terminal newline as truncated.

### Evidence

`append_transition_rejects_same_length_prefix_tampering` covers same-length mutation.
`append_transition_rejects_prefix_tampering_after_pre_append_validation` covers mutation inside the append window.
`missing_terminal_newline_is_detected_as_truncation` covers incomplete terminal records.

Primary commit: `588461bf`.

## 9. Durable recovery with incremental append validation

### Miss

Recovery treated an already complete pending record as durable without reopening and syncing its file.
The first content-level correction repeatedly reread and hashed the complete current-day prefix.
Reopening an already private file also reapplied mode `0600`, changing ctime and invalidating the trusted stamp.

### Risk

Recovery could durably advance the manifest and checkpoint while the record remained only in filesystem cache.
Repeated prefix hashing restored quadratic daily authorization cost.
An unnecessary metadata mutation could create false tamper failures.

### Resolution

Recovery syncs the record file before advancing the manifest or checkpoint.
The writer retains incremental authenticated state between appends.
Owner-only permissions are changed only when the existing mode is not already correct.

### Evidence

`pending_commit_rolls_forward_after_each_durable_stage` covers the completed-record recovery path.
Focused append tests verify stable metadata and incremental validation behavior.

Primary commit: `e3761cb0`.

## 10. Post-append prefix reauthentication

### Miss

The implementation relied on an exclusive advisory file lock to close the append race.
On Unix, a non-cooperating writer can ignore an advisory lock.

### Risk

A same-user process could modify authenticated prefix bytes after pre-append validation.
The command could execute after the altered file became the accepted post-write baseline.

### Resolution

The writer reauthenticates the relevant prefix after append and before trust advances.
The post-append result must match the previously authenticated state plus the exact new record.

### Evidence

The append-window tamper regression modifies the prefix after pre-append validation and expects fail-closed refusal.

Primary commit: `769b9492`.

## 11. Bounded cross-platform append reauthentication

### Miss

Post-append validation initially opened a second read handle while the Windows writer held an exclusive `LockFileEx` byte-range lock.
The revised full-prefix read again made authorization work grow with the complete current-day history.

### Risk

Windows could reject the second handle's read and quarantine valid audit state after a normal write.
Large daily logs could impose growing command latency while holding the audit mutex.

### Resolution

Read-back validation uses the already locked writer handle.
Audit files rotate into bounded authenticated continuation segments.
Only the bounded active segment requires post-append content reauthentication.
Closed segments remain committed by the authenticated manifest and protected checkpoint.

### Evidence

`full_audit_files_advance_to_bounded_segments` verifies segment rollover.
The cross-platform implementation avoids a second handle inside the Windows lock range.

Primary commit: `b7820565`.

## 12. Collision-free derived audit-state paths

### Miss

Using `Path::with_extension` replaced an existing suffix instead of appending a new one.
Distinct configured directories such as `audit.dev` and `audit.prod` could therefore derive the same head and key paths.
The journal path had the same problem for explicitly configured key paths.

### Risk

Independent instances could overwrite shared manifests, checkpoints, keys, or journals.
Each instance could then quarantine valid state written by the other.

### Resolution

Derived state paths append an injective suffix without replacing the configured path's existing extension.
Existing legacy locations are considered through an explicit authenticated migration path.

### Evidence

`derived_state_paths_preserve_existing_extensions` proves distinct inputs remain distinct.
`state_paths_migrate_legacy_files_before_use` covers compatible legacy migration.

Primary commit: `4279ee0e`.

## 13. Persistent key-state and legacy migration authentication

### Miss

After loading the persistent key and checkpoint, authorization compared the head only with the in-memory checkpoint.
Deletion, replacement, corruption, or rollback of the persistent key-state file could be overwritten by the next commit.
Legacy paths created by the old collision-prone derivation could not identify which configured audit directory owned shared state.

### Risk

The next valid write could launder a live persistent-state integrity failure.
Multiple destinations could copy the same legacy state and cause at least one instance to adopt another directory's checkpoint.

### Resolution

Every authorization identity-stamps and validates persistent key state before advancing its checkpoint.
Unexpected deletion, replacement, corruption, or rollback fails closed.
Legacy state migrates only when its key, manifest, checkpoint, and logs fully authenticate the current audit directory.
Ambiguous ownership copies nothing, preserves the legacy files, and returns an actionable fail-closed migration error.

### Evidence

`live_write_rejects_deleted_persistent_key_state` covers deletion.
`live_write_rejects_corrupted_persistent_key_state` covers corruption.
`live_write_rejects_rolled_back_persistent_checkpoint` covers rollback.
`state_path_migration_rejects_ambiguous_legacy_ownership` covers collision ambiguity.

Primary commit: `06f26e33`.

## 14. Refusal-audit write-amplification control

### Miss

A holder of the published read token could repeatedly call Organization or ProcessChanging commands that were guaranteed to fail authorization.
Each refusal still forced synchronous journal, log, manifest, checkpoint, and directory work.

### Risk

A local read-token holder could generate unbounded durable I/O, fill the audit volume, or poison the sink.
Legitimate process-changing commands would then fail closed because the audit sink was unavailable.

### Resolution

Refusal-producing requests pass through a dedicated token bucket before durable audit work.
Admitted refusal attempts remain audited.
Excess refusal attempts return without multiplying persistent writes.
The limiter is separate from command and spawn quotas so an audit refusal does not consume legitimate execution capacity.

### Evidence

`refusal_audit_rate_limit_bounds_durable_writes` verifies bounded persistent records under repeated refusals.
`audit_refusal_refunds_governor_admission` verifies that an audit failure does not consume spawn or destructive-rate quota.

Primary commit: `a07dd2ef`.

## Accepted threat boundary

The implementation detects record forgery, link breaks, tail truncation, whole-segment removal, head-manifest modification, and rollback of the head relative to the persistent checkpoint.
It cannot detect restoration of the entire audit state, checkpoint, and key material from one internally consistent older filesystem snapshot.
Detecting that rollback requires a genuinely external monotonic authority such as TPM-backed state, a remote transparency service, or another non-rollbackable counter.
Unix owner-only key permissions also do not protect against a same-user attacker who can read the key.
These boundaries are explicit and must not be described as solved by the local HMAC design.

## Validation completed

The focused audit suite passed 36 tests after the final migration and persistent-state fixes.
The no-mistakes review completed after fourteen fix rounds with no remaining findings.
The pipeline test and documentation stages passed.
GitHub CI passed the Rust workspace gate, CLI workspace tests, warning-denied Clippy, Rust 1.89 MSRV check, frontend type checking and tests, Playwright suite, production frontend build, Windows packaging contracts, and GitGuardian checks.

## Review lessons

Security-sensitive persistence must be reviewed as a state machine across every crash boundary, not as a sequence of successful writes.
Tamper evidence must cover collection membership, ordering, live file identity, persistent key state, and migration ownership.
Filesystem durability requires syncing directory entries as well as file contents.
Fail-closed behavior must be placed before every side effect and must release reservations and quota on refusal.
Performance is part of security because an unbounded verifier or refusal path can make the audit sink unavailable.
Cross-platform file-lock semantics must be exercised explicitly rather than inferred from one operating system.
Migration must prove ownership before copying security state.
Automated review fixes must preserve repository scope and must not track machine-local files.

## Release guidance

PR #80 should remain the single review unit for the implementation and this review record.
The branch should pass the complete validation pipeline again after this document is committed.
The production installer should be built from merged `main`, with the standard one-time build version bump applied at build time.
The installed Windows executable must be verified by version, source commit, path, SHA-256, process identity, and a live authenticated control round trip.
