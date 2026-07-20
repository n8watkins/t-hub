# T-Hub UX Reliability and Performance Itinerary

## Status

This is the active outline for the General's July 20, 2026 T-Hub follow-up.
Implementation must proceed serially so each package is reproduced, fixed, reviewed, committed, and accepted before the next package begins.
The Captain remains at orchestration altitude and uses one visible T-Hub Crew workspace with one terminal for the single active implementer.
An independent reviewer may follow the implementer, but implementation packages must not overlap in time.

## Outcomes

The program must deliver the following user-visible outcomes.

1. A commissioned Captain remains durably identifiable as that Captain and safely regains its scoped control capability after T-Hub or its control endpoint restarts.
2. Captain creation works for existing WSL folders whether or not they contain a Git repository.
3. Codebase name is explicit and required, with no visible or implicit "derive from folder" behavior in the new flow.
4. The folder picker distinguishes a real empty folder from a failed or stale directory listing.
5. Terminal, Files, and Preview header controls remain usable as the tile narrows.
6. The product calls the combined surface Preview instead of Run + Preview.
7. Preview can reliably discover, start, display, refresh, stop, and report a project preview.
8. Preview has explicit provider-neutral MCP and CLI operations instead of depending on inference from chat text.
9. Kokoro voice works for supported Codex lifecycle events with the same event semantics as Claude.
10. Merged History, responsive-header, Cortana recovery, and follow-up fixes are proven in a packaged Windows build.
11. The final result meets measured responsiveness, CPU, memory, process, journal, and recovery budgets.

## Confirmed Baseline

The installed production executable is `C:\Users\natha\AppData\Local\T-Hub\t-hub.exe` version `0.3.106`, modified July 19, 2026 at 11:51 PM Pacific.
The installed development executable is version `0.3.105`, modified July 19, 2026 at 9:25 PM Pacific.
The provider-neutral History UI began landing after midnight on July 20.
The responsive tile-header work landed at 2:17 AM on July 20.
The Cortana startup recovery work also landed after the installed production executable was built.
At the time of this audit, 75 desktop-affecting commits on `main` were newer than the production executable.
The installed version number alone cannot prove source provenance because source remained at version `0.3.106` while later commits accumulated.
The merged fixes therefore exist in source and on remote `main`, but they are not present in the currently installed executable.

The existing responsive header source already renders icons and supports full-label, short-label, and icon-only densities in `apps/desktop/src/components/Tile.tsx` and `apps/desktop/src/index.css`.
The source still labels the surface `Run + Preview`, shortens it to `Run`, and describes it accessibly as `Run and Preview`.

Preview currently accepts only a managed runner URL or a manually entered URL.
It intentionally ignores arbitrary URLs printed in an agent terminal.
Chat context does not currently start or navigate Preview.
Preview lifecycle operations exist only behind the desktop UI's Tauri commands.
There is no provider-neutral Preview control surface in the T-Hub control dispatcher, MCP catalog, or CLI.

Kokoro settings are enabled and both local text-to-speech health endpoints responded successfully during the audit.
A real Kokoro request produced a valid WAV file, so synthesis is available.
Interactive Codex is currently launched as telemetry-unobserved and does not produce the normalized permission and question transitions that the voice watcher consumes.
T-Hub already contains a Codex hook normalizer, but it does not install or wire a T-Hub Codex lifecycle hook into interactive Codex sessions.
Voice currently announces permission and question transitions, not completion or arbitrary command events.

The reported Appturnity folder `/home/natkins/appturnity/monorepo-app` exists, is populated, and is a valid Git main worktree.
The backend-equivalent WSL directory-list command currently returns its child folders.
The earlier "No child folders" message was therefore not a truthful description of that folder.

The `repoRoot must be an absolute path` failure has an evidence-backed cross-platform cause.
Git returns a valid POSIX main-worktree path, registration applies host `std::fs::canonicalize`, and Windows retains the POSIX path after canonicalization fails.
Project persistence then applies the Windows host definition of an absolute path and rejects the valid WSL path.
The folder picker also clears entries after a listing error and then renders its empty-state copy alongside the real error, conflating failure with emptiness.

The current Captain control failure is also evidence-backed.
The WSL discovery file at `/home/natkins/.t-hub/control.json` is a stale regular file that points at retired listener `127.0.0.1:45949`.
The authoritative Windows discovery file at `/mnt/c/Users/natha/.t-hub/control.json` points at live listener `127.0.0.1:56192`.
The two files also contain different credentials.
The Captain MCP first tries its stale spawn-time endpoint, then rereads the stale WSL discovery file and fails its retry with `endpoint_replaced=true`.
Current documentation assumes that the WSL file is a mirror or symlink, but the observed regular file disproves that runtime assumption.
Atomic temp-file replacement can also replace a symlink and permanently split the two discovery paths.
The deeper durability gap is that the shared tier credential is checked before durable session identity, so a real credential rotation strands an otherwise valid commissioned Captain.

## Architectural Decisions

### Captain identity and capability

Captain identity is durable product state, while a control endpoint address is ephemeral transport state.
Commissioning must bind a durable Captain identity to an authenticated session identity and scoped authority.
Endpoint rotation must not silently turn that Captain into a read-only or unauthenticated caller.
The client must rediscover the new endpoint and prove the same session identity before resuming control operations.
It must never copy an ambient read token into a Captain session or treat possession of a broad token as Captain identity.
If identity reauthentication cannot be proven, the Captain must fail closed with a visible recovery action while its durable Captain record remains intact.
Discovery must use one authoritative stable file rather than a replaceable WSL symlink or a frozen address-and-token pair.
The discovery record should carry a listener generation or instance identifier and validate loopback address, protocol version, and freshness.
A session-scoped control lease should be renewable through the durable session credential only after the server verifies an active unrevoked Captain binding, exact terminal, liveness, and current scoped grants.
The renewal operation must derive authority from server state and must never return or persist the shared global control credential.

### Git-optional Projects

T-Hub Project identity must be based on a canonical project root, not on the presence of Git.
Every Project should store a canonical `rootPath` and explicit version-control capability such as `git` or `none`.
Git Projects may additionally store the canonical main-worktree root and Git identity.
Existing `repoRoot` records and API inputs require a compatibility migration that preserves Project IDs, Captains, assignments, checkpoints, and ownership.

Captain commissioning needs only a validated working directory and therefore must work for both Git and non-Git Projects.
Crew worktrees, source-commit baselines, integration provenance, and Git delivery operations remain Git-only.
Those operations must fail early with a stable structured `Git required` capability error for a non-Git Project.
Git initialization remains a separate explicit operation and must never happen implicitly during registration, rollback, or Captain creation.

Codebase name must be explicit and required in the current UI and new machine-readable contracts.
The visible "Derived from folder" behavior must be removed.
A deprecated backend fallback may remain temporarily only for wire compatibility, with telemetry and a documented removal point.
The display name must not become filesystem identity.

### Preview control

Preview process changes must be explicit operations.
An agent may use chat context to decide that a preview is useful, but it must then call a typed MCP or CLI operation.
T-Hub must not start arbitrary commands or navigate an iframe merely because a URL or instruction appeared in conversation text.

One backend service must own Preview discovery, lifecycle, status, URL selection, cleanup, and audit behavior.
The desktop UI, MCP server, and CLI must be thin adapters over that service.
The operation family should cover discovery, status, start, stop, open, and refresh.
Machine-readable CLI output must follow `docs/cli-contract.md` and remain stable.
Read-only operations must remain distinct from organization and process-changing operations.

### Voice events

Voice must consume normalized provider-neutral lifecycle events rather than Claude-specific hook assumptions.
Codex hook integration must redact prompt and tool content before persistence or fanout.
Replay and restart recovery must not repeat already-announced events.
Permission, question, completion, and failure announcements need explicit per-event policy so "voice works" is testable rather than ambiguous.
The implementation should preserve the current attention announcements and add completion or failure settings only through an intentional product contract.

## Serialized Execution

### Package 0 - Captain control continuity

#### Reproduction

1. Commission a Captain through the installed Windows app.
2. Confirm `my_capability` returns `control` and `captain_bootstrap` returns the commissioned identity and Project.
3. Restart or rebind the T-Hub control listener so its address changes.
4. Invoke a read operation and a Captain-scoped control operation from the same running Captain.
5. Record endpoint discovery, authenticated session identity, capability, durable Captain identity, registry state, and any fallback behavior.

#### Implementation contract

- Separate endpoint discovery from durable identity and authorization.
- Replace stale endpoint pins after a transport failure without adopting an ambient lower-capability credential.
- Pass one stable authoritative discovery-file location through supported Codex and Claude MCP environment allowlists without persisting rotating addresses or credential values in global configuration.
- Stop relying on a mutable WSL symlink or shadow copy for Windows-hosted production discovery.
- Reauthenticate the same session identity against the new endpoint.
- Add a narrow, short-lived, identity-bound capability renewal path whose authority comes from the live Captain registry and scoped grants.
- Preserve the Captain record and assignment across listener, app, WSL, and MCP-process restarts.
- Surface a specific recoverable state when endpoint rediscovery succeeds but identity reauthentication fails.
- Ensure new MCP processes inherit variable names or identity references without persisting rotating addresses or secret values in global configuration.
- Keep peer-Captain and Crew isolation unchanged.

#### Acceptance

- The same commissioned Captain returns `control` before and after endpoint rotation.
- `captain_bootstrap` returns the same Captain, Project, assignment, and roster after restart.
- No duplicate Captain, terminal, or Project is created.
- A Crew or unrelated terminal cannot gain Captain authority through endpoint recovery.
- A stolen, stale, ambient, or mismatched token fails closed.
- Recovery works through the packaged Windows app and the registered Codex MCP server.
- Endpoint replacement has bounded latency and does not require recreating the Captain.
- A stale WSL shadow file cannot override the current authoritative Windows handshake.
- Atomic handshake publication cannot sever WSL discovery.
- Credential rotation renews an active Captain's scoped lease without exposing the global control credential.
- Released, removed, dead, expired, revoked, foreign, and duplicate identities cannot renew.

This package is control-plane and security-sensitive and requires an independent sequential review before landing.

### Package 1 - Git-optional Captain creation and WSL path identity

#### Reproduction

1. Use the Create Captain dialog on a populated non-Git WSL folder.
2. Capture the current Git-initialization gate.
3. Use the dialog on `/home/natkins/appturnity/monorepo-app` and capture the absolute-path failure.
4. Force a WSL directory-list failure and compare it with the true empty-folder state.

#### Implementation contract

- Introduce canonical Project root identity with optional Git metadata.
- Use the existing shared Windows-to-WSL path conversion layer instead of host `Path::is_absolute` and host canonicalization for WSL paths.
- Normalize POSIX, normal UNC, legacy UNC, and extended UNC spellings to one Project identity.
- Allow both populated and empty non-Git folders to register and commission a Captain without creating `.git`.
- Remove the Git requirement and Git-initialization checkbox from Captain creation.
- Require an explicit non-empty codebase name and remove folder-derived naming from the UI contract.
- Keep Git initialization available only as a separate explicit operation.
- Separate folder-picker loading, loaded-empty, loaded-populated, error, and stale-response states.
- Skip Git worktree enumeration when the folder is already known to be non-Git.
- Add stable Git-capability errors to worktree, Crew dispatch, baseline, integration, and delivery operations.

#### Acceptance

- Existing Git Project identities and Captain records migrate without duplication or loss.
- A non-Git Captain starts, checkpoints, restarts, and recovers normally.
- Git-only operations fail before mutation with a structured and actionable error.
- The Appturnity folder displays its real children and registers once.
- Relative, nonexistent, foreign-distribution, file, traversal, and unauthorized roots fail before persistence.
- Concurrent equivalent registrations cannot create duplicate Projects.
- Packaged Windows E2E covers Git and non-Git creation through the actual dialog.

This package changes persisted control-plane state and requires migration review and an independent sequential review.

### Package 2 - Responsive Preview header and naming

#### Implementation contract

- Rename every visible, accessible, test, and current-documentation occurrence of Run + Preview or Run and Preview to Preview.
- Use a Preview-appropriate icon rather than retaining a misleading Run-only identity.
- Preserve Terminal, Files, and Preview icons at narrow widths.
- Collapse full labels to icon-only controls before controls overlap, clip, or become inaccessible.
- Preserve tooltips and accessible names in icon-only mode.
- Base layout on the tile's available header width rather than only the application window width.

#### Acceptance

- Exact breakpoint tests cover one, two, four, eight, and sixteen tiles.
- Tests cover long terminal titles, Git chips, status badges, narrow windows, and display scaling.
- No header action is clipped, overlapped, or ambiguous.
- Keyboard focus and screen-reader names remain correct.
- Header adaptation does not create resize loops or measurable layout thrashing.

### Package 3 - Preview runtime and explicit automation

#### Reproduction

1. Reproduce Preview with the user's actual project and the current installed executable.
2. Repeat against the source-based development build.
3. Capture discovery result, selected package or static target, launch command, process identity, output, detected URL, reachability, iframe behavior, and cleanup.

#### Implementation contract

- Discover configured targets and nested monorepo applications rather than only root `package.json` scripts and root `index.html`.
- Persist an explicit selected preview target per Project or Workspace.
- Make the backend snapshot own the authoritative preview URL.
- Recognize safe loopback representations including `localhost`, `127.0.0.1`, `0.0.0.0`, IPv6 loopback, and validated WSL-host mappings.
- Invalidate stale WSL addresses after network or WSL restart.
- Add typed backend operations for discover, status, start, stop, open, and refresh.
- Expose matching MCP tools and `th preview` CLI commands with stable JSON output and documented exit codes.
- Require explicit confirmation and authorization for start and stop.
- Keep arbitrary conversation URLs and commands untrusted until passed through the typed operation contract.
- Prove process-tree ownership and cleanup so stopping Preview does not kill an unrelated server.

#### Acceptance

- Preview works for representative Vite, Next.js, static, and configured monorepo targets.
- Start is idempotent and concurrent starts serialize safely.
- Status reports starting, running, unreachable, stale, failed, and stopped distinctly.
- Iframe, open externally, refresh, stop, restart, and app-restart recovery work in the packaged Windows app.
- An MCP or CLI caller can execute the full lifecycle without relying on UI clicks or chat inference.
- Invalid hosts, path traversal, command injection, port confusion, and unrelated process ownership fail closed.

### Package 4 - Codex voice parity

#### Reproduction

1. Use Settings Test Voice to separate synthesis and playback from lifecycle-hook behavior.
2. Trigger equivalent permission and question states in Claude and interactive Codex.
3. Compare normalized events, journal entries, frontend status transitions, synthesis requests, playback outcome, and deduplication.
4. Decide and document whether completion and failure should speak by default or through separate settings.

#### Implementation contract

- Wire the existing Codex hook normalizer into supported interactive Codex lifecycle hooks.
- Add install, health, repair, and uninstall handling that is symmetrical with the supported Claude integration while respecting Codex configuration ownership.
- Preserve unrelated user Codex hooks and never rewrite secrets or provider credentials.
- Emit provider-neutral permission, question, completion, failure, and session-end events with stable identities.
- Redact sensitive command, prompt, and tool content before journal persistence.
- Deduplicate before journal, status-store, and voice fanout.
- Show synthesis, playback, and device failures instead of silently losing a cue.

#### Acceptance

- Test Voice is audible through the packaged Windows app.
- Equivalent Claude and Codex attention events produce one announcement with equivalent wording and timing.
- Restart and journal replay do not repeat an announcement.
- Disabled voice, disabled attention, and per-event settings are respected.
- Hook install and uninstall preserve unrelated Codex configuration.
- The journal remains bounded under noisy Codex tool activity.

This package affects provider configuration and potentially sensitive event data and requires independent sequential review.

### Package 5 - Packaged build and live acceptance

The first build should be an isolated T-Hub Dev build, not an immediate production replacement.
Building before Packages 0 through 4 are integrated would prove that merged History and responsive-header source works, but it would become stale as soon as the remaining fixes land.
The preferred sequence is one source-level reproduction baseline, serialized fixes, full automated verification, one isolated Dev build, live acceptance, and then an explicit production promotion decision.

#### Acceptance

- The Dev installer identifies the exact source commit and binary hash.
- T-Hub Dev uses its isolated state, database, tmux socket, control channel, journal, voice settings, WebView profile, and updater configuration.
- History shows closed Codex and Claude sessions correctly.
- Cortana starts or recovers exactly once.
- Captain control survives restart.
- Git and non-Git Captain creation pass.
- Header, Preview, and voice acceptance pass in the real Windows WebView and audio path.
- Production is not replaced until the General authorizes promotion after reviewing Dev evidence.

## Performance Program

Performance is a release gate for every package rather than a final cleanup task.

### Budgets and instrumentation

- Endpoint recovery must be bounded, cancellable, and free of tight reconnect loops.
- Folder browsing must debounce manual edits, cancel stale requests, avoid recursive scans, and avoid duplicate Git probes.
- Canonical path identity must be computed once per selection or registration and reused.
- Header responsiveness must use stable width thresholds without repeated synchronous measurements or resize feedback loops.
- Preview discovery must cache by canonical root and relevant file fingerprint rather than rerun on every panel mount.
- Preview output must be batched or coalesced instead of copying up to 2,000 lines and rerendering for every output event.
- Preview URL probing must be asynchronous, bounded, deduplicated, and backed off while unreachable.
- Voice should move away from sustained 250 ms polling where an event-driven watch can provide equivalent correctness.
- Kokoro cold and warm synthesis latency, queue depth, cancellation, and playback failure must be visible.
- Codex hook integration must avoid recording noisy tool events that do not contribute to lifecycle or user attention.
- Journal size, retention, replay time, and deduplication must be measured because the current production journal was approximately 49.7 MB during this audit.

### Packaged benchmark matrix

Run the existing packaged benchmark process from `docs/PERFORMANCE-BENCHMARK.md` at one, four, eight, and sixteen terminals.
Measure idle, terminal-output, folder-browsing, Preview-starting, Preview-noisy, Preview-refreshing, voice-synthesis, endpoint-recovery, and History-open scenarios.
Record process-tree working set, private bytes, CPU, process count, thread count, input latency, panel-open latency, Preview-ready latency, voice latency, endpoint-recovery latency, and journal growth.
Compare only identical workloads and installed binary identities.
Any interval with process birth or death must follow the benchmark document's eligibility rules.

## Verification Gates Per Package

Each package must pass its focused E2E reproduction before implementation and the same E2E flow after implementation.
Each logical change must be committed separately after verification.
Frontend changes require focused tests, the complete frontend suite, type checking, and production build.
Rust changes require focused tests, the complete relevant Rust suites, strict Clippy, and formatting.
CLI changes require contract tests for human and JSON modes, stable error categories, and documented exit codes.
MCP changes require schema, capability-tier, authorization, restart, and E2E coverage.
Persisted-state changes require migration, restart, duplicate, rollback, concurrency, and legacy-read coverage.
Control-plane, provider-hook, and persisted-schema packages require independent sequential review before landing.
No package is considered live until verified against an exact packaged Windows binary.

## Recommended Crew Sequence

| Order | Package | Implementer specialty | Required sequential reviewer |
| --- | --- | --- | --- |
| 0 | Captain control continuity | Rust control, MCP, identity, recovery | Security and control-plane reviewer |
| 1 | Git-optional Captain creation | Rust project model, WSL paths, React dialog | Persistence and cross-platform reviewer |
| 2 | Responsive Preview header and naming | React UI and accessibility | UI and packaged-layout reviewer |
| 3 | Preview runtime and automation | Rust process lifecycle, MCP, CLI, React | Security and lifecycle reviewer |
| 4 | Codex voice parity | Harness hooks, journal, frontend audio | Privacy and provider-integration reviewer |
| 5 | Dev build and acceptance | Windows packaging and E2E | Release evidence reviewer |
| 6 | Performance closure | Packaged profiling and regression analysis | Benchmark-method reviewer |

Only one implementation package may be active at a time.
The next package starts only after the previous commit, test evidence, independent review where required, and Captain checkpoint are complete.

## Open Product Decision

The General reported that Kokoro is silent for Codex commands.
The current policy speaks only permission and question events, so successful command completion is silent by design.
The recommended contract is to keep permission and question announcements enabled by the existing attention setting and add separate configurable completion and failure announcements with conservative defaults.
The live acceptance matrix must test whichever policy the General selects.
