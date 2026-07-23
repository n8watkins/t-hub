# Package 6 Performance Closure

## Purpose

Package 6 turns the itinerary's qualitative performance goals into repeatable release decisions.
It measures the exact T-Hub Dev artifact accepted by Package 5.
It covers Windows and owned WSL resources, WebView responsiveness, operation latency, Preview, voice, control recovery, History, and journal behavior.

Performance evidence must not contain transcript content, terminal content, prompt content, tool arguments, command arguments, control credentials, provider credentials, or raw hook payloads.

## Sequencing

Instrumentation that changes application source, benchmark source, or artifact provenance must land before the Package 5 candidate freeze.
The Package 5 build, live acceptance, and Package 6 matrix then use one installed binary hash.
A performance fix creates a new candidate.
The new candidate repeats source review, source gates, packaging, installation, affected live acceptance, and the complete affected performance matrix.

## Required Repetitions

Run every mandatory scenario at one, four, eight, and sixteen visible terminal tiles.
Run three eligible repetitions for every matrix cell.
Use the same disposable workspace snapshot, terminal layout, workload seed, power mode, display scale, foreground state, sample duration, and installed binary for repetitions and paired comparisons.

The collector must observe the terminal count from T-Hub state.
An operator declaration alone is insufficient.

## Absolute Resource Budgets

The values below apply to total owned Windows and WSL processes after the scenario-specific warmup.
CPU is expressed as a fraction of one logical core.
Memory values are binary mebibytes.

| Visible terminals | Idle CPU run total | Idle CPU p95 | Idle private bytes p95 | Idle working set p95 | Stable owned processes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.15 | 0.30 | 700 MiB | 850 MiB | 24 |
| 4 | 0.25 | 0.45 | 1,000 MiB | 1,200 MiB | 36 |
| 8 | 0.40 | 0.70 | 1,500 MiB | 1,800 MiB | 52 |
| 16 | 0.70 | 1.10 | 2,300 MiB | 2,800 MiB | 84 |

The process budget counts the desktop process, WebView descendants, Windows host bridges, and exact owned WSL descendants.
Provider, Preview, and voice processes intentionally started by a scenario are recorded separately and must return to the pre-scenario count after cleanup.
No scenario may leave more than one additional owned process or ten additional threads after a thirty-second cleanup window.
Private bytes and working set must return to within ten percent or 128 MiB, whichever allowance is larger, of the pre-scenario p50 after cleanup.

## Responsiveness Budgets

All latency budgets use monotonic timestamps correlated by one operation identifier.

| Metric | p50 | p95 | Maximum |
| --- | ---: | ---: | ---: |
| Terminal keydown to accepted terminal write | 25 ms | 50 ms | 150 ms |
| Terminal output arrival to painted frame | 33 ms | 100 ms | 250 ms |
| Terminal, Files, or Preview panel open | 100 ms | 250 ms | 750 ms |
| History open to first complete visible result | 200 ms | 500 ms | 1,500 ms |
| Folder selection to loaded result for warm fixture | 250 ms | 750 ms | 2,000 ms |
| Header resize frame while crossing a density tier | 16.7 ms | 50 ms | 150 ms |
| Preview refresh request to committed iframe navigation | 150 ms | 500 ms | 1,500 ms |
| Control endpoint recovery | 500 ms | 1,500 ms | 3,000 ms |

No run may contain a heartbeat stall of 5,000 ms or more.
No run may contain a `ResizeObserver loop` error.
Long tasks of at least 200 ms must average fewer than one per scenario minute and no mandatory action may overlap a long task longer than 500 ms.

## Preview Budgets

Preview timing starts when the typed operation is accepted and stops when the authoritative reachable URL is committed.

| Preview target | Cold ready p95 | Warm restart p95 | Stop and cleanup p95 |
| --- | ---: | ---: | ---: |
| Static | 2 seconds | 1 second | 2 seconds |
| Vite | 8 seconds | 5 seconds | 3 seconds |
| Next.js | 15 seconds | 10 seconds | 5 seconds |
| Configured nested monorepo | 15 seconds | 10 seconds | 5 seconds |

An unreachable probe must back off to no more than one probe per target per second after the first five seconds and no more than one probe per five seconds after thirty seconds.
Concurrent start requests must create one owned process tree.
Preview output retained in UI state must remain within the documented line and byte bounds.
One noisy Preview minute must not cause more than 64 MiB retained-memory growth after cleanup.

## Voice Budgets

Voice timing uses durable attempt identity.
It separates queue wait, synthesis, playback start, playback completion, and outcome persistence.

| Metric | p50 | p95 | Maximum |
| --- | ---: | ---: | ---: |
| Kokoro cold synthesis | 2.5 seconds | 5 seconds | 8 seconds |
| Kokoro warm synthesis | 750 ms | 1.5 seconds | 3 seconds |
| Accepted live event to playback start, excluding Scribe hold | 1.5 seconds | 3 seconds | 6 seconds |
| Playback completion to durable outcome | 100 ms | 300 ms | 1 second |
| Scribe stop to held-cue playback start | 700 ms | 1.5 seconds | 3 seconds |

The queue depth must never exceed one active playback plus one pending coalesced cue.
Every displaced, failed, interrupted, succeeded, or application-exit attempt must have one durable terminal outcome.
Disabling voice must stop the sustained 250 ms Scribe poll within one second.
The release target is event-driven Scribe state.
If polling remains because no event source is available, the owning decision must be documented and the enabled idle poll rate must not exceed one request per second.
No voice metric may retain spoken text or provider payload content.

## Journal Budgets

Record journal bytes, parseable entries, retained event identities, compaction count, replay duration, duplicate count, and invalid-entry count before and after every scenario.

- Idle growth must not exceed 256 KiB per hour per live terminal.
- The sixty-second terminal-output scenario must not grow the lifecycle journal unless lifecycle state changes.
- The sixty-second noisy Codex scenario must not exceed 2 MiB of journal growth.
- Duplicate provider events must add zero journal entries.
- Compaction must keep the journal at or below 32 MiB after completion.
- Replay of a 32 MiB compacted journal must complete within five seconds at p95 and ten seconds maximum.
- Replay and compaction must preserve the monotonic high-water sequence and never repeat a voice announcement.
- A malformed entry may produce a bounded error but must not create an unbounded retry or growth loop.

## Paired Regression Budgets

Compare the candidate with the designated reference artifact under identical eligible workloads.
The reference artifact and reason for selecting it must be recorded before collection.

A candidate fails when any p95 resource or latency metric regresses by more than twenty percent and the absolute change is also greater than one of these materiality floors.

- CPU: 0.05 of one logical core.
- Memory: 64 MiB.
- Input or frame latency: 10 ms.
- Panel or folder latency: 50 ms.
- Preview, voice, or endpoint latency: 250 ms.
- Journal growth: 128 KiB per scenario.

Passing an absolute ceiling does not excuse a material regression.
Passing a relative comparison does not excuse an absolute ceiling violation.

## Deterministic Scenario Matrix

Every driver uses a stable workload seed and a disposable development workspace.
Every action carries a correlation identifier into application and collector evidence.

### Idle

Warm for thirty seconds and sample for sixty seconds.
Do not type, resize, switch panels, create processes, or change terminal count.

### Terminal output

Emit ten fixed-width lines per second in every visible terminal for sixty seconds.
Record transport arrival, accepted write, animation-frame flush, painted frame, dropped chunks, and terminal queue depth.
Stop output and observe cleanup for thirty seconds.

### Folder browsing

Alternate twenty times among a loaded-empty folder, a populated non-Git folder, a Git folder, and a forced-listing-error fixture.
Use a 150 ms edit cadence to exercise debounce and cancellation.
Record request count, cancellation count, stale-result count, Git probe count, loaded-state latency, and recursive-walk count.
There must be no recursive walk and no more than one Git probe per accepted selection.

### Preview starting

Start and stop static, Vite, Next.js, and configured nested-monorepo targets.
Run three cold starts and five warm restarts per target.
Record discovery, spawn, listener ownership, probe, authoritative URL, ready, stop, and cleanup timestamps.

### Preview noisy

Run one owned Preview target that emits 500 fixed-width lines per second for sixty seconds.
Keep the Preview panel visible for thirty seconds and hidden for thirty seconds.
Record backend retained output bytes, frontend retained lines, render commits, CPU, memory, and cleanup.

### Preview refreshing

Issue thirty refresh operations at a two-second cadence against one stable owned Preview.
Record request-to-navigation latency, duplicate navigation count, probe count, render count, and memory.

### Voice synthesis

Run one Kokoro cold request followed by ten warm requests.
Run permission, question, completion, and failure policies with deterministic non-sensitive labels.
Exercise one Scribe-held cue, one displaced cue, one synthesis failure, one playback failure, and one interrupted persistence retry.
Record only identities, timestamps, outcome categories, queue depth, and redacted engine metadata.

### Endpoint recovery

Rotate the development listener ten times at a five-second cadence.
After each rotation, issue one read and one Captain-scoped control operation from the same commissioned Captain.
Record discovery generation, reconnect attempts, backoff, identity reauthentication, lease renewal, latency, and result.
No recovery may use a tight loop or create a duplicate Captain, Project, terminal, or Assignment.

### History open

Prepare fixed disposable Claude and Codex history fixtures.
Open and close History twenty times, alternating providers and a combined result.
Record query, first result, complete result, render, process count, memory, and mutation count.
The scenario must not start a provider process or mutate a transcript.

## Evidence Schema

Each run produces one JSON artifact with this minimum shape.

```json
{
  "schemaVersion": 3,
  "candidate": {
    "sourceCommit": "full Git commit",
    "installedBinarySha256": "sha256",
    "installerSha256": "sha256",
    "protocolVersion": 2
  },
  "reference": {
    "installedBinarySha256": "sha256",
    "selectionReason": "predeclared reason"
  },
  "host": {
    "windowsVersion": "version",
    "wslVersion": "version",
    "distro": "identity",
    "logicalProcessors": 1,
    "memoryBytes": 1,
    "powerMode": "identity",
    "displayScale": 100
  },
  "scenario": {
    "kind": "idle",
    "terminalCount": 1,
    "observedTerminalCount": 1,
    "workloadVersion": "version",
    "workloadSeed": "seed",
    "repetition": 1,
    "startedAt": "RFC 3339",
    "finishedAt": "RFC 3339"
  },
  "resources": {
    "windows": {},
    "wslOwned": {},
    "webview": {},
    "samples": []
  },
  "operations": [],
  "preview": {},
  "voice": {},
  "journal": {},
  "diagnostics": {
    "heartbeatStalls": [],
    "longTasks": [],
    "resizeObserverErrors": []
  },
  "validity": {
    "eligible": true,
    "reasons": [],
    "processBirthIntervalsExcluded": 0
  },
  "budgets": [],
  "decision": "pass",
  "rawEvidence": []
}
```

Raw evidence references must be content-addressed.
The artifact records redaction counts and rejects prohibited secret or content fields before publication.
Collector commit identity does not prove application source identity.
The installed binary and Package 5 provenance manifest provide that binding.

## Eligibility Rules

A run is ineligible when any of these conditions holds.

- Installed binary, source, scenario driver, workload seed, terminal count, power mode, or display scale differs from the declared matrix cell.
- The observed terminal count differs from the requested count.
- The root application exits, restarts, or changes identity outside the endpoint-recovery scenario.
- An unowned process enters the measured ownership set.
- Required action or metric correlation is missing.
- Process birth or death makes a CPU interval incomplete and the scenario cannot be rerun without that interval.
- A collector, instrumentation, provider, Preview, audio, or control error makes a mandatory metric unavailable.
- Sensitive content or credentials appear in the artifact.
- Cleanup does not reach a stable observation within thirty seconds.

Ineligible runs do not count toward the three required repetitions.
They must be rerun after the eligibility problem is corrected.

## Failure Rules

One mandatory absolute-budget violation fails the matrix cell.
One material paired-regression violation fails the matrix cell.
One five-second UI stall, ownership ambiguity, unbounded loop, queue overflow, duplicate side effect, secret leak, or cleanup failure fails the package immediately.
A p95 is computed only from eligible samples and uses nearest-rank empirical quantiles.
The worst repetition determines the matrix-cell decision.
Missing data is a failure unless the run is declared ineligible and repeated.
Results may not be averaged across terminal counts, scenario kinds, target types, binary identities, or host power modes.

## Runner Exit Codes

The Package 6 runner must use the following stable process exit codes.

| Exit code | Meaning |
| ---: | --- |
| `0` | Every required cell has three eligible passing repetitions and every runner-verifiable schema and evidence check passes |
| `2` | Invalid invocation, unsupported flag, or malformed runner configuration |
| `3` | The exact Package 5 artifact or a required benchmark environment dependency is unavailable |
| `4` | An absolute budget, paired-regression budget, cleanup invariant, evidence requirement, or mandatory scenario assertion failed |
| `5` | The run is invalid because of environment drift, fixture drift, process churn, dropped samples, collector failure, missing correlation, or sensitive-content detection |
| `6` | The result cannot satisfy the required Package 6 evidence schema |

The runner must emit a schema-valid result for exit codes `0`, `4`, and `5`.
The runner must retain a bounded diagnostic artifact for exit codes `2`, `3`, and `6`.
Missing cells, missing repetitions, mixed installed hashes, pooled repetitions, stale evidence, and unreferenced raw artifacts must exit `4`.
An ineligible run exits `5`, remains retained, and does not count toward the three required repetitions.
No retry may overwrite or delete a prior failed or invalid result.

## Exit Gate

Package 6 passes only when every mandatory matrix cell has three eligible passing repetitions.
All absolute and paired-regression budgets must pass.
Every owned process and durable queue must return to its cleanup bound.
The complete evidence set must map to one Package 5 installed binary hash.
An independent benchmark-method reviewer must approve the workload equivalence, collector behavior, redaction, eligibility decisions, quantile calculations, exclusions, and final decision.

After approval, the Captain may report Package 6 as complete and live-verified.
Production remains unchanged until the General separately authorizes promotion.
