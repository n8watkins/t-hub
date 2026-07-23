# Package 5 Packaged Build and Live Acceptance

## Purpose

Package 5 proves that one exact source candidate became one isolated T-Hub Dev installer, one installed binary, and one live-verified product result.
Source tests, browser tests, installer inspection, installation, and live verification are separate delivery states.
No passing state implies a later state.

Production promotion is outside Package 5.
It requires a separate decision from the General after the complete Dev evidence has been reviewed.

## Entry Conditions

Package 5 may start only when all of these conditions hold.

- Packages 0 through 4 are integrated into one exact clean canonical commit.
- The required independent reviewers approved the exact result commits in that ancestry.
- The complete canonical source gates pass at the candidate commit.
- The Captain checkpoint records the candidate, the integrated Package 0 through 4 results, and Package 5 as the next action.
- The authorized Windows builder can resolve the candidate without rebasing, merging, or modifying it.
- The operator has explicit authority for every build, download, installation, provider-configuration, process-changing, and production-observation action in the run.

## Candidate Freeze

The candidate identity is the full Git commit.
The working tree must be clean when the candidate is selected.
The lockfile and build configuration hashes are recorded before the build.
No mutable branch name, version string, workflow label, or timestamp substitutes for the full commit.

Any source change after the freeze creates a new candidate.
The new candidate must repeat source verification, independent review, packaging, installation, and every acceptance row affected by the change.

## Source Gate

Record exact commands, exit codes, durations, and retained logs for the following checks.

- Complete frontend tests.
- Frontend type checking.
- Production frontend build.
- Complete browser suite, including the Package 2 header matrix.
- Relevant complete Rust workspace suites.
- Strict Clippy for every changed Rust crate and the desktop application.
- Rust formatting.
- CLI human and JSON contract suites.
- MCP schema, capability, authorization, restart, and end-to-end suites.
- Persisted-state migration, restart, duplicate, rollback, concurrency, and legacy-read suites.
- Development-build configuration regression.
- Development-installer validator fixture suite.

A skipped test is not a pass.
Every platform-dependent skip must have a corresponding required Windows or WSL execution in the live matrix.

## Artifact Provenance Manifest

Retain one machine-readable manifest named `package-5-evidence.json`.
The manifest must use this minimum shape.

```json
{
  "schemaVersion": 1,
  "candidate": {
    "artifactId": "immutable candidate identity",
    "branch": "exact integrated branch",
    "sourceBaseline": "full baseline Git commit",
    "sourceCommit": "full Git commit",
    "gitTree": "full Git tree",
    "repository": "repository identity",
    "pnpmLockSha256": "sha256",
    "cargoLockSha256": "sha256",
    "appVersion": "version",
    "protocolVersion": 2
  },
  "build": {
    "workflow": "workflow identity",
    "runId": "run identity",
    "runAttempt": 1,
    "runnerImage": "Windows runner identity",
    "windowsVersion": "Windows build",
    "webView2Version": "runtime version",
    "nodeVersion": "version",
    "pnpmVersion": "version",
    "rustVersion": "version",
    "targetTriple": "x86_64-pc-windows-msvc",
    "featureSet": ["devbuild"],
    "tauriConfigSha256": "sha256",
    "tauriOverlaySha256": "sha256",
    "startedAt": "RFC 3339",
    "finishedAt": "RFC 3339"
  },
  "artifacts": {
    "installer": {
      "path": "relative evidence path",
      "sha256": "sha256",
      "signatureStatus": "updater and Authenticode status",
      "reference": "durable artifact reference"
    },
    "rawBinary": { "path": "relative evidence path", "sha256": "sha256" },
    "expectedBinary": { "path": "relative evidence path", "sha256": "sha256" },
    "extractedBinary": { "path": "relative evidence path", "sha256": "sha256" },
    "installedBinary": {
      "path": "absolute installation target",
      "sha256": "sha256"
    },
    "validator": {
      "path": "relative evidence path",
      "sha256": "sha256",
      "passed": true
    }
  },
  "installation": {
    "installedAt": "RFC 3339",
    "productName": "T-Hub Dev",
    "bundleIdentifier": "com.t-hub.dev",
    "executableName": "t-hub-dev.exe",
    "installationTarget": "absolute installation directory"
  },
  "environment": {
    "tHubDistro": "WSL distribution identity",
    "wslVersion": "version",
    "wslKernelVersion": "version",
    "agentVersion": "version",
    "claudeVersion": "version",
    "codexVersion": "version"
  },
  "matrix": [],
  "review": {
    "reviewer": "durable reviewer identity",
    "reviewedAt": "RFC 3339",
    "decision": "approved"
  }
}
```

The manifest must bind the installed hash to the extracted and expected binary hashes required by `docs/DEV-BUILD.md`.
The manifest must bind `sourceCommit` to `gitTree` using retained `git rev-parse <sourceCommit>^{tree}` output.
The workflow artifact must retain the manifest beside the installer and validator evidence.
The manifest must never contain a control token, read token, session credential, provider credential, transcript content, prompt content, tool arguments, or unredacted hook payload.
Discovery files may be represented only by redacted structural fields and a content hash computed after secret fields are removed.

## Pre-Installation Validation

Follow `docs/DEV-BUILD.md` and reject the artifact unless all of these checks pass.

- Exactly one development installer exists.
- Exactly one expected development executable exists in the extracted payload.
- The NSIS script targets `t-hub-dev.exe` and never terminates `t-hub.exe`.
- The raw binary contains the required unique Tauri bundle marker.
- The expected marker transformation is the only binary difference.
- Raw, expected, extracted, installer, validator, and signature identities are retained.
- The development updater has no endpoints.
- The product name, executable, bundle identifier, database name, state root, control file, journal, voice file, configuration root, WebView profile, tmux socket, and Cortana home match the development isolation contract.

## Production Preservation Baseline

Before installing T-Hub Dev, record the following production evidence.

- Production executable path, version, SHA-256, process ID, and process creation time.
- Production control listener address, listener generation, protocol version, server process identity, and one authenticated read result.
- Redacted and hashed production control discovery structure.
- Production database, state root, tmux socket, journal, voice settings, WebView profile, updater configuration, and Cortana identity.
- Active production terminal and Captain counts.

Installing, updating, or uninstalling T-Hub Dev must not change any production evidence except naturally advancing observational timestamps.
Production credentials and provider credentials must not be copied into the evidence bundle.

## Installed Isolation Matrix

Verify every row against the installed T-Hub Dev binary.

| Surface | Required development identity | Required production observation |
| --- | --- | --- |
| Application | `T-Hub Dev`, `t-hub-dev.exe`, `com.t-hub.dev` | Existing production executable and process remain unchanged |
| State | `~/.t-hub-dev` | `~/.t-hub` remains unchanged |
| Database | `t-hub-dev.db` in development app data | Production database remains unchanged |
| tmux | `t-hub-dev` socket | Production socket and sessions remain unchanged |
| Control | `~/.t-hub-dev/control.json` and distinct listener generation | Production listener remains authenticated and unchanged |
| Journal | `~/.t-hub-dev/journal` | Production journal is not appended by development sessions |
| Voice | `~/.t-hub-dev/voice.json` | Production voice settings remain unchanged |
| Configuration | `~/.t-hub-dev/config` | Production configuration remains unchanged |
| Cortana | `~/.t-hub-dev/orchestrator` and one development identity | Production Cortana remains unchanged |
| WebView | Development bundle profile | Production cookies, cache, and local storage remain unchanged |
| Updater | No endpoints | Production updater configuration remains unchanged |

## Live Acceptance Evidence Row

Every live matrix row records these fields.

- Stable row identifier.
- Requirement and package owner.
- Installed binary path and SHA-256.
- Source commit.
- Start and finish timestamps.
- Exact setup and disposable fixture identity.
- Operator or automation identity.
- User-visible steps.
- Machine-readable calls and redacted results.
- Expected and observed result.
- Screenshot, screen recording, audio observation, log, or state artifact references.
- Cleanup result.
- Pass, fail, blocked, or accepted-exception decision.
- Reviewer identity and review timestamp.

An accepted exception must identify the approving authority, scope, reason, expiry, and why it does not weaken a release-critical guarantee.

## Cross-Package Evidence Index

| Evidence row | Mandatory evidence | Pass decision |
| --- | --- | --- |
| `P5-P0-CONTINUITY` | Durable Captain, Project, Assignment, terminal, session, listener generation, discovery hash, capability, lease result, timestamps, and redacted request and response records across endpoint, app, WSL, and MCP restarts | The same Captain regains `control`, no durable identity duplicates, stale discovery cannot win, and every invalid identity class fails closed |
| `P5-P3-PREVIEW` | Target, canonical-root fingerprint, request ID, run ID, process tree, bounded output, advertised URL, reachable URL, lifecycle timestamps, screenshots, CLI JSON, MCP results, and cleanup snapshot for all four fixture classes | UI, CLI, and MCP observe one serialized lifecycle, exact owned processes stop, unrelated processes survive, and every adversarial input fails before mutation |
| `P5-P4-VOICE` | Provider version, hook health, before and after configuration hashes, normalized type, journal sequence, provider event identity, replay flag, claim result, durable delivery state, redacted announcement class, synthesis and playback timing, engine health, device result, and redacted journal excerpt | Equivalent events have equivalent behavior, every accepted event delivers at most once, every disabled policy remains silent, replay never redelivers, unrelated configuration remains byte-identical, and every delivery failure is visible |
| `P5-HISTORY-DUAL-HARNESS` | Exact Claude and Codex conversation identities, cwd, timestamps, labels, selected tile, resume and archive results, provider-file hashes, and History screenshots across app and WSL restart | Each conversation resumes exactly, one archive affects only its target, Harness identities remain distinct, and provider files remain intact |
| `P5-CORTANA-EXACTLY-ONCE` | Cortana identity, terminal, tmux target, process identity, listener generation, reconciliation reason and result, event timestamps, and roster snapshots across clean start, repeat reconciliation, app restart, and WSL restart | Exactly one live development Cortana exists, valid recovery reuses its durable identity, and no duplicate terminal or process survives |

## History Matrix

Use disposable Claude and Codex sessions with known identities.

- Close a Claude session and verify it appears once in History with the correct provider, title, directory, and terminal state.
- Close a Codex session and verify the same fields and one History row.
- Verify a closed terminal does not remain in Recent as a live resumable terminal.
- Restart T-Hub Dev and verify the History rows remain correct without duplication.
- Verify opening History does not mutate provider transcripts, recreate a terminal, or start a provider process.
- Verify a malformed or unavailable provider history source produces a bounded visible error without hiding valid rows from the other provider.

## Cortana Matrix

- Start T-Hub Dev from a clean development state and verify exactly one development Cortana is created.
- Restart the application with that valid Cortana alive and verify it is recovered rather than duplicated.
- Exercise the documented invalid-incumbent fixture and verify one replacement is created only after exact ownership validation and retirement.
- Verify concurrent startup requests converge on one Cortana identity.
- Verify production Cortana identity, process, state, and working directory remain unchanged.
- Verify the development Captain sidebar and workspace show only the authoritative development Cortana.

## Package 0 Captain Control Matrix

- Commission one development Captain and record its durable Captain, Project, Assignment, terminal, session, and listener identities.
- Rotate the development control endpoint and verify the same Captain regains its scoped control capability.
- Rotate the credential generation and verify identity-bound lease renewal without exposing the shared control credential.
- Restart T-Hub Dev, WSL, and the MCP process in the documented sequence and verify the same Captain and Assignment recover.
- Verify `captain_bootstrap` returns the same Project, ship, roster, checkpoint, and next action.
- Verify no duplicate Captain, Project, terminal, Assignment, or Crew record appears.
- Verify stale, stolen, ambient, mismatched, released, removed, dead, expired, revoked, foreign, and duplicate identities fail closed.
- Verify a stale WSL shadow discovery file cannot override the authoritative Windows discovery record.
- Record endpoint-recovery latency for Package 6.

## Package 1 Captain Creation Matrix

Run the complete actual-dialog contract in `docs/PACKAGE-1-WINDOWS-E2E-REQUIREMENTS.md`.
Include populated non-Git, empty non-Git, valid Git, Appturnity, directory failure, stale response, invalid root, concurrent equivalent registration, Git-required denial, explicit Git initialization, restart, and identity-preservation cases.

## Package 2 Header Matrix

Run the complete installed Windows matrix in `docs/PACKAGE-2-WINDOWS-E2E-REQUIREMENTS.md`.
Retain every required scale, tile-count, breakpoint, keyboard, accessibility, resize, and screenshot result.

## Package 3 Preview Matrix

Use disposable registered Projects for Vite, Next.js, static content, and a configured nested monorepo target.

- Discover targets and verify the selected target persists across panel and application restart.
- Exercise status, start, open, refresh, stop, and restart from the desktop UI.
- Exercise the same lifecycle through MCP and CLI JSON contracts without relying on chat inference.
- Verify starting is idempotent and concurrent starts serialize.
- Verify starting, running, unreachable, stale, failed, and stopped states.
- Verify the authoritative URL in the backend snapshot reaches the iframe and external-open surfaces.
- Verify valid localhost, IPv4 loopback, wildcard bind, IPv6 loopback, and current validated WSL-host mappings.
- Restart WSL or change its host mapping and verify stale mappings are invalidated.
- Verify stop and application recovery affect only the exact owned process tree.
- Verify invalid host, path traversal, command injection, port confusion, foreign Project, unrelated listener, and ambiguous ownership cases fail before mutation.
- Retain process identities, listener ownership, URLs, bounded output, cleanup state, CLI JSON, MCP results, and screenshots.

## Package 4 Voice Matrix

The packaged policy contract is exact.
Permission, question, completion, and failure are independent settings.
Fresh or missing configuration defaults the master voice switch and all four event policies off.
An enabled legacy attention value migrates permission and question to enabled, while completion and failure remain off.
The legacy attention field is otherwise only a compatibility projection of permission or question.

- Use Test Voice with Kokoro and verify a valid synthesis response reaches the selected Windows audio device.
- Record cold and warm synthesis plus playback latency for Package 6.
- Trigger equivalent Claude and Codex permission events and verify one announcement with equivalent wording and timing.
- Trigger equivalent Claude and Codex question events and verify one announcement with equivalent wording and timing.
- Verify fresh or missing configuration leaves permission, question, completion, and failure silent.
- Verify the enabled legacy attention projection enables permission and question without enabling completion or failure.
- Enable and disable each policy separately and verify exactly one matching announcement or silence for each provider and event.
- Disable the master switch and each individual event policy and verify the corresponding cue remains silent.
- Restart during replay and verify historical events are not announced.
- Exercise synthesis, playback, audio-device, interrupted, and application-exit outcomes and verify each is visible and durable.
- Verify held cues remain session-bound, respect Scribe state, and recover or interrupt durably.
- Verify hook install, health, repair, and uninstall preserve unrelated Codex and Claude configuration.
- Snapshot and restore shared provider configuration around the run under explicit authority.
- Verify journal records and evidence contain no prompt, command, tool argument, provider credential, or other sensitive content.
- Run noisy Codex lifecycle activity and retain journal growth, compaction, replay, and deduplication evidence for Package 6.

## Full Matrix Exit Rule

Package 5 passes only when all mandatory rows pass against one installed binary hash.
A failed mandatory row blocks Package 5.
A blocked row blocks Package 5 unless the missing external prerequisite is restored and the same row is rerun.
An inconclusive observation is a failure, not a pass.
Evidence from a different source commit, installer, installed hash, state root, or unrecorded configuration is invalid.

The independent release-evidence reviewer must verify provenance, isolation, evidence completeness, secret redaction, cleanup, and every row decision.
Only after that approval may the Captain report Package 5 as live-verified and ask the General whether to consider production promotion.
