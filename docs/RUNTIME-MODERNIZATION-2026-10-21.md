# Runtime modernization implementation record

This records changes following [the project assessment](./PROJECT-ASSESSMENT-2026-10-21.md).
The changes are on `fix/runtime-modernization` and are not deployed or pushed.
The assessment remains a historical account of the original baseline.

## Findings and disposition

| Assessment finding | Implemented change | Remaining acceptance |
| --- | --- | --- |
| Unsafe account-wide quota recovery | Removed automatic terminal writes, rejected the legacy recovery source in the backend, and retained fresh session-specific manual reset alerts. | Any future automatic recovery must use explicit session/turn ownership and provider acknowledgement. |
| Process presence reported as agent activity | Reconciled provider lifecycle with tmux evidence, surfaced permission/idle/exited/unavailable states, and retained last-known state separately. | Packaged Windows reconnect and crash scenarios. |
| Terminal writes lack provider acknowledgement | Added an isolated, opt-in app-server pilot with durable request IDs, uncertain-delivery handling, explicit approval responses, and lifecycle replay. | Production integration plus live quota and approval UX acceptance; an authenticated isolated turn now passes. |
| Missing execution provenance | Persisted provider-reported model IDs and model changes in durable agent observations/events; pilot records provider-confirmed thread settings and CLI version. | Legacy terminals may not report reasoning effort or CLI version; these remain explicitly unknown. |
| Delegation policy lacks comparative evidence | Added a matched-trial protocol and scorer requiring all three modes, repeated attempts, usage, interventions, and hashed evidence. | Run representative model trials before changing Captain policy. |
| Agent listing reconciles unbounded history | Reconcile only the requested active page, share process inventory, skip stopped history, and avoid unchanged-observation journal writes. | Continue measuring large real registries; registry snapshots still have costs. |
| Verification omits important runtime coverage | Added lint to the standard profile, provider compatibility lanes, structured runtime subprocess tests, and orchestration scorer contracts. | Execute remote CI and packaged Windows/WSL acceptance. |
| Missing CSP and default release devtools | Added production/development CSP and made release devtools an explicit diagnostic feature. | Confirm the packaged WebView2 application, including local assets and remote previews. |

## Verification evidence

The unsafe recovery regression first failed in Chromium by observing Escape and continue writes to unrelated terminals.
The fixed regression verifies no account-wide terminal writes.
A real-tmux control regression first demonstrated permission state being incorrectly reported as running.
It now verifies permission, working, and completion projections.
The provenance regression first returned no model despite provider telemetry and now verifies persisted model identity, model switching, and unchanged-observation deduplication.

The full local suite passed after the recovery, runtime reconciliation, pagination, and CSP changes.
It included Rust workspace and process tests, standalone CLI tests, typecheck, lint, 637 frontend tests, production bundling, nine Chromium tests, and portable contracts.
Frontend lint initially reported 28 existing warnings and no errors.
A subsequent shared-helper extraction resolved all 28 warnings without suppressions, and the standard lint command now rejects any warnings.
The extraction passed typecheck, all 637 frontend tests, production bundling, nine Chromium tests, and the lint/CI contracts.
Additional provenance changes passed the focused real-tmux regression, typecheck, all 637 frontend tests, and the subsequent Rust gate.
That gate included 412 control tests and 30 tmux tests.
Clippy passed for the workspace and all targets with warnings treated as errors.
The host CLI contract profile passed against the installed providers, including provisioning, interrupted installation recovery, provenance checks, and concurrent installer locking.
Portable contracts passed with the runtime and evaluation suites included.
The runtime subprocess suite passes six tests, including restart replay, lost acknowledgement, exact approval routing, model configuration validation, exclusive state ownership, and oversized-frame rejection.
The evaluation scorer passes four tests covering incomplete comparisons, configuration drift, evidence integrity, and unknown billing cost.

Handshake and model discovery passed against installed Codex 0.163.0 and baseline Codex 0.145.0 without starting a model turn.
The older provider reported an incompatible existing model cache and successfully fell back to its catalog; the runtime did not assume the newer catalog.
A subsequent opt-in authenticated smoke test completed a real turn using Codex 0.163.0 with provider-confirmed `gpt-6.1-sol` and medium reasoning effort.
The final run completed in 3.215 seconds with 29 recorded events, matched the thread and turn identifiers, returned the exact requested marker, and replayed the acknowledged request without submitting it again.
The harness rejects tool use and approval requests and verifies that the disposable workspace remains unchanged.
Its three fixture tests also pass, bringing the runtime test total to nine.
This establishes a narrow authenticated completion path, not comparative model quality or production readiness.

A Chromium accessibility check reproduced unreadable empty-preview text at a contrast ratio of 1.26:1.
The empty preview now uses the tile background, and the regression verifies at least 4.5:1 contrast.
The corrected screenshot was reviewed visually.

Packaged Windows acceptance is in progress using an isolated source copy and dedicated runtime state.
Do not count a successful native build or installer extraction as completed runtime acceptance.

## Next acceptance gates

Use a disposable Windows/WSL environment and the exact candidate development installer for packaged acceptance.
Record the installer and executable hashes, source commit, provider versions, WebView2 version, selected distro, and screenshots.
Exercise terminal launch, provider permission, idle completion, renderer restart, bridge reconnect, provider exit, remote preview, and application restart with durable agent state.
Do not treat browser tests with mocked Tauri IPC or installer contract fixtures as proof of those flows.
Do not shut down the active development distro to simulate recovery.

Run the comparison protocol in `scripts/eval/README.md` with archived prompts and identical acceptance checks.
Do not infer that stronger models make delegation obsolete or that more agents improve quality.
Retain existing policy until measured outcomes justify a change.

The structured runtime remains experimental and separate from production terminals.
Its README documents private state storage, transcript-bearing RPC responses, replay protection, and the absence of automatic retention pruning.
Production adoption needs a reviewed lifecycle and retention policy plus live provider acceptance evidence.
