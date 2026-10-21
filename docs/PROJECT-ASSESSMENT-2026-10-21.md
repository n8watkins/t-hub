# T-Hub assessment: stronger models, runtime correctness, and product direction

Assessment date: 2026-10-21.
Source baseline: `88872dce`, desktop version `0.3.163`.
This is an assessment and proposed priority order, not authorization to change product policy or replace the active operating contracts.

## Judgment

T-Hub has a useful core: persistent sessions, cross-provider supervision, worktree ownership, durable assignments, and evidence tied to exact commits.
Its biggest modernization opportunity is the boundary between the agent runtime and the terminal.
The current system often controls bytes and observes processes where it needs to control turns and observe agent state.
More capable models do not remove that problem; longer autonomous runs make incorrect recovery, ambiguous state, and weak delivery acknowledgements more consequential.

Changing model names alone would accomplish little.
The active launch path inherits provider configuration rather than pinning an obsolete model, so a newer default can already flow through.
What is missing is explicit, inspectable execution configuration and evidence about which orchestration approach actually produces better results.

## Evidence and limits

Reviewed the active documentation chain, provider adapters, agent records, dispatch admission, registry reconciliation, automation input, provider usage collection, Captain doctrine, desktop security configuration, and CI/browser coverage.
The marketing site, promotional assets, and every backend command were not exhaustively audited.
No live user sessions were steered, stopped, or modified.
No packaged Windows recovery scenario or paid model benchmark was run.
Static findings below are distinguished from an isolated diagnostic reproduction.

- Frontend suite: 638 tests across 70 files passed.
- TypeScript check and standalone CLI tests passed in the initial standard run.
- Browser profile: production frontend bundle and seven Chromium tests passed.
- A temporary Vitest diagnostic reproduced account-wide auto-continue fan-out using the real watcher installation and timers with mocked provider usage and IPC.
  The probe was removed after execution; no runtime source was changed.
- The initial Rust lane stalled in the configured `sccache` wrapper.
  That specific compiler-wrapper process was terminated and the standard profile was restarted with `RUSTC_WRAPPER=`.
  The rerun passed all standard-profile lanes, including the non-process Rust workspace tests.

## Findings

### 1. High: account-wide quota recovery can inject into unrelated Codex sessions

Evidence: `apps/desktop/src/lib/autoContinueWatch.ts:120`, `:282`, `:321`, and `apps/desktop/src-tauri/src/commands.rs:683`.

All watched Codex terminals use the same account-wide exhausted/reset reading.
The timer path calls recovery without requiring per-session blocked status, an active matching turn, a current foreground-provider check, or a human-input guard.
The backend validates the source label and live PTY connection, then writes the bytes.
Auto-continue is enabled unless each tile opts out.

The isolated reproduction seeded two Codex tiles, no blocked-session evidence, and one expired exhausted account reading.
Installing the actual watcher delivered Escape to both tiles, followed by `continue\r` to both.
This confirms the selection defect; it does not claim an observed live billing action or shell execution.
A session waiting for input or a user editing a prompt can receive unintended automation.
Provider identity can also outlive the foreground provider, so identity alone is insufficient to authorize input.

The same code takes the earliest reset when multiple quota windows are exhausted, although another window can still block work.
Failed usage polls retain the last good reading without a freshness deadline.
The timer marks a window handled before delivery succeeds, making delivery failure another recovery gap.

Recommended acceptance test: one rate-limited session, one working session, one permission prompt, one exited provider, and one user-edited prompt.
Only the intended blocked session may resume, after all applicable limits permit it, with a fresh state check immediately before submission.
Use a provider turn operation where supported; otherwise require conservative state checks and explicit recovery ownership.

### 2. High: durable runtime state conflates process presence with agent activity

Evidence: `apps/desktop/src-tauri/src/control.rs:7331`, `src/control/handlers_agents.rs:438`, and `src/agent_session.rs:124`.

Reconciliation maps a live provider process to `running` and a surviving shell without that provider to `idle`.
It does not use provider turn or permission events to distinguish an idle interactive agent from a working one or a permission-blocked one.
Unknown liveness returns early, preserving the old record without recording that this observation failed.
The serialized durable state is exposed directly by `get_agent` and `list_agents`.

The separate supervision subsystem has richer lifecycle evidence, and this finding does not claim that every UI badge is wrong.
The problem is disagreement between authoritative surfaces that a supervising agent may use to decide whether to wait, wake, or recover.
The active post-Powder roadmap already identifies this missing runtime reconciliation gate.

Unify the projection from trusted provider events, with separate process state, turn state, evidence source, and observation time.
Keep unknown state explicit and preserve the last-known value separately.
Test permission, completion, idle prompt, process exit, and bridge failure through both the UI and control API.

### 3. High architectural priority: automation still depends on terminal bytes

Evidence: `apps/desktop/src-tauri/src/plane.rs:1`, `src/commands.rs:683`, and `src/control/handlers_captains.rs:1066`.

The automation plane attributes writes but has no durable queue, receipt state, or human-typing exclusion.
The default Crew path launches interactive provider TUIs; Codex starts with an explicit degraded-observability marker.
A successful PTY write does not establish which turn consumed the message or whether a provider accepted it.
A 120 ms delay between Escape and submission is a timing heuristic, not a provider acknowledgement.

Do not confuse this with the entire communication system being non-durable.
`AgentFollowup` already has durable intent and semantic idempotency, which should be retained.
The missing guarantee is at the final runtime delivery boundary.

Current official Codex App Server documentation exposes thread/turn operations, streaming lifecycle events, approvals, interruption, and steering with an expected turn ID.
The installed `codex-cli 0.163.0` also exposes `app-server` tooling.
That is a concrete alternative to controlling a TUI with keystrokes, subject to testing against the installed protocol.
[Codex App Server](https://learn.chatgpt.com/docs/app-server).

Claude's Agent SDK provides a programmable agent loop with sessions, permissions, hooks, and streaming interfaces.
It is a separate adapter candidate, not an assumption that the providers have identical APIs or authentication semantics.
[Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview).

Introduce a narrow runtime interface for start, resume, steer, interrupt, approval, events, and effective configuration.
Implement one disposable Codex vertical slice before migrating production sessions.
Retain terminals for human interaction and compatibility; replace automation incrementally.
Prove restart reconnection, event replay, duplicate-delivery handling, and approval behavior before promoting the new path.

### 4. Medium: model upgrades are invisible in durable execution provenance

Evidence: `apps/desktop/src-tauri/src/control/handlers_spawn.rs:712`, `:904`, and `src/agent_session.rs:842`.

The strict `start_agent` schema selects a harness but accepts no model or reasoning-effort field.
The durable session record stores provider, assignment, and delivery provenance but not requested/effective model, reasoning effort, or harness version.
The generated command inherits provider defaults.
Two nominally identical assignments can consequently run with different configurations without that difference appearing in their durable record.

Add optional execution configuration with provider capability validation and persist the effective configuration returned by the runtime.
Do not force a model override when the user intentionally wants provider defaults.
Record changes after an in-session model switch as events rather than overwriting history.
Discover supported models and effort values instead of hard-coding a new model list.
Codex documents `model/list` for this purpose; account availability still needs verification.
[Codex model discovery](https://learn.chatgpt.com/docs/app-server#models).

### 5. Medium: mandatory organizational overhead has no measured quality baseline

Evidence: `skills/captain/SKILL.md:16`, `:44`, `:131`, and `docs/PERFORMANCE-BENCHMARK.md`.

The Captain doctrine delegates every multi-step investigation and forbids direct routine evidence gathering beyond a bounded summary.
Those are deliberate operating rules, not accidental implementation defects.
They nevertheless encode an assumption that delegation and summary boundaries are better than a capable primary agent doing the investigation itself.
The repository performance benchmark measures app/process behavior; it does not establish agent task success, coordination cost, or whether additional roles improve outcomes.

My recommendation is a testable hypothesis, not a claim that newer models make multi-agent work obsolete.
Compare a strong single agent, that agent with bounded native helpers, and the durable Captain/Crew workflow on the same representative tasks and baselines.
Measure accepted-result rate, regressions, human interventions, wall time, context/usage where available, and merge conflicts.
Use repeated trials and exact-commit acceptance tests.
Include cross-subsystem bugs, focused changes, genuine independent workstreams, and interrupted-session recovery.

Keep durable Crew for independent work, persistence, and ownership boundaries where they earn their cost.
Consider allowing direct bounded investigation by the primary agent if the results support it.
Changing this doctrine is a separate product-policy decision.

### 6. Medium: listing a small page reconciles the entire matching agent history

Evidence: `apps/desktop/src-tauri/src/control/handlers_agents.rs:36`, `src/control.rs:7331`, `src/tmux.rs:1026`, and `src/control/captains_registry.rs:2290`.

`list_agents` gathers every agent matching the Captain/project and reconciles each before applying active/removed filtering and pagination.
Each reconciliation snapshots the registry and probes tmux; `harness_liveness` repeats the session check and requests pane information.
Thus `limit: 1` does not bound reconciliation work, and stopped history participates even for an active-only query.
Repeated full snapshots also make the in-memory work grow with retained history.
On Windows these process boundaries are especially worth measuring.

This is a source-confirmed scaling problem, not a measured latency claim for the installed app.
Batch liveness once per reconciliation cycle, maintain event-driven state, and serve lists from a consistent snapshot.
Benchmark realistic retained history, not only the number of visible tiles.

### 7. High release-confidence gap: green suites do not establish the primary product flow

Evidence: `.github/workflows/test.yml`, `apps/desktop/e2e/tile-header.spec.ts:65`, and `docs/PRODUCTION-READINESS.md`.

The seven browser tests exercise real Chromium with mocked Tauri IPC and focus on layout, density, panels, keyboard behavior, and resize.
They are useful and passed, but do not prove live provider launch, permission handling, delivery, or recovery.
Windows CI runs contract scripts; release builds and installer inspection add evidence, but they are not an installed Windows/WSL lifecycle test.
The standard local profile explicitly skips control and tmux process modules and does not run lint.
A green standard run is therefore a narrower claim than a green complete release gate.

CI installs Codex `0.145.0` and Claude Code `2.1.220`; this environment has Codex `0.163.0` and Claude Code `2.1.306`.
That proves a compatibility-coverage mismatch, not that the newer versions are broken.
Keep pinned reproducibility and add a supported-current compatibility lane with event fixtures and disposable real-provider smoke tests.

Gate release candidates on packaged startup, launch, approval, follow-up, provider exit, UI reload, WSL restart, and conversation recovery using a disposable profile.
Do not run destructive restart scenarios against the user's active fleet.

### 8. Medium hardening gap: production webview defenses remain incomplete

Evidence: `apps/desktop/src-tauri/tauri.conf.json:26` and `src-tauri/Cargo.toml:55`.

Production CSP remains `null`, and the Tauri dependency enables `devtools` unconditionally.
These are current source facts that survived the earlier readiness audit.
They are defense-in-depth gaps for a desktop application with process and clipboard capabilities, not proof of a reachable injection vulnerability.

Define a CSP around actual asset and preview requirements and make release devtools an explicit diagnostic build choice.
Validate both embedded preview behavior and remote navigation boundaries before enforcing the policy.
Keep this work separate from a claim that model upgrades themselves introduced a security vulnerability.

## What to preserve

- Durable assignments, checkpoints, cursor-based events, and idempotent follow-up intent.
- Exact-commit review/test provenance and explicit delivery stages.
- Worktree ownership, conflict admission, and clean-baseline checks.
- Capability-scoped control identities and separation of role from authorization.
- Persistent tmux terminals and the cross-provider user experience.
- Existing UI layout tests and explicit degraded states when evidence is unavailable.

Default Crew permission bypass is documented as intentional user-authorized policy.
It is not being reported as an accidental regression.
However, worktree/lane ownership is coordination, not an OS sandbox: it cannot technically confine an unrestricted provider process to assigned files.
Keep that distinction explicit when evaluating unattended execution.

## Recommended order

1. Reproduce the auto-continue selection defect through a disposable end-user flow, then gate recovery on fresh per-session state.
2. Unify durable runtime state with provider events and add effective model/harness provenance.
3. Add a current-provider compatibility lane and the missing packaged lifecycle acceptance tests.
4. Build one structured Codex runtime slice behind an explicit feature flag and test replay, approvals, and steering.
5. Compare single-agent, native-helper, and durable-Crew execution on representative tasks before changing the hierarchy.
6. Remove list-time full-history reconciliation and close the webview hardening gaps.

The product opportunity is reliable supervision of work: intent, execution, evidence, review, and recovery across providers.
The terminal grid remains useful, but should become a view into that system rather than the automation protocol itself.
