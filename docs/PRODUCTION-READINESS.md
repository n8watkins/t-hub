# T-Hub Production Readiness Plan

**Status:** Active stabilization program.
**Audit baseline:** 2026-07-13, `main` at `e936e33`, source version `0.3.64`.
**Owner:** T-Hub maintainers.

## Executive Decision

T-Hub is a capable personal alpha, but it is not ready to be called stable or production-ready.
The current CI proves that the Linux-compatible Rust and frontend unit suites compile and pass.
It does not prove that the Windows and WSL product works, that a released installer is trustworthy, or that upgrades and recovery are safe.

Feature expansion should pause after the current `0.3.65` integration wave.
The resulting build becomes the stabilization baseline, not the stable release.
Powder expansion, additional orchestration features, and broad UI work resume only after the Beta gate in this document passes.

## Supported Production Scope

The first stable release supports one product shape:

- Windows 11 on x86-64.
- A locally installed per-user T-Hub desktop application.
- WSL2 with Ubuntu, zsh, tmux, Git, and a supported coding-agent CLI.
- One interactive Windows user and one local T-Hub instance.
- Claude Code and Codex workflows that use documented adapters.
- Local and loopback control by default.
- Tailnet access only where the authenticated remote-control design explicitly permits it.

The first stable release does not promise macOS, native Linux, multiple Windows users, arbitrary remote hosts, enterprise administration, or zero-loss process survival across a Windows or WSL shutdown.
Conversation recovery may survive a host restart when the provider transcript remains available, but live processes do not.

## Audit Baseline

### What Is Working

- TypeScript compilation passes.
- The desktop production frontend build completes.
- All 395 frontend tests pass across 39 files.
- The Rust workspace has 636 passing tests and one ignored test.
- The standalone CLI has 28 passing tests.
- The current PR gate runs Rust tests, Clippy, TypeScript, and Vitest.
- `main` blocks force pushes and deletion.
- Tauri updater artifacts are cryptographically signed with the updater key.
- Runtime state includes SQLite WAL persistence, recovery records, diagnostic logging, capability-scoped control tokens, and a control audit trail.

### Stop-Ship Gaps

| Area | Current evidence | Why it blocks stable |
| --- | --- | --- |
| Production-platform testing | CI runs product tests on Ubuntu only. | Windows, WebView2, `wsl.exe`, DPAPI, NSIS, Win32 window behavior, and WSL boundaries are the actual product. |
| Release gating | The tag workflow builds and publishes without depending on the PR test suite. | A release can be built from a commit that never passed the complete gate. |
| Windows trust | The workflow signs Tauri updater artifacts but has no Authenticode certificate configuration. | Windows may treat the executable and installer as an unknown publisher. |
| Browser and packaged-app E2E | There is no Playwright, WebDriver, or packaged Windows automation. | Unit tests do not prove core user workflows or visual integrity. |
| Security automation | Dependabot, secret scanning, push protection, and repository security analysis are disabled. | Known vulnerable dependencies and committed secrets can reach `main` undetected. |
| Dependency health | Desktop audit reports one high and one moderate vulnerability through `seti-icons`; the site reports high and moderate vulnerabilities in Next.js and PostCSS. | Known vulnerable dependency trees cannot ship as stable without triage and a recorded disposition. |
| Webview hardening | Tauri CSP is `null` and release builds include devtools. | The webview has unnecessary attack and debugging surface for a process-capable desktop application. |
| Branch protection | Required checks are not strict, admin enforcement is off, review is not required, and conversations need not be resolved. | The protected branch can accept stale or unreviewed changes and can be bypassed by an administrator. |
| Lint discipline | Clippy completes with 91 library warnings and 96 warnings for test targets. | New warnings can hide inside the existing baseline and CI cannot enforce regression-free code. |
| Toolchain contract | The crate declares Rust 1.77 while using `LazyLock`, which requires Rust 1.80. | A documented supported toolchain can fail to build the product. |
| Repository completeness | There is no license, security policy, support policy, privacy statement, or production runbook. | The project claims to be open source without defining use rights or a vulnerability and support process. |
| Release identity | Source is `0.3.64`, README claims `0.3.58` released, and GitHub's latest public release is `v0.2.0`. | Users and maintainers cannot reliably identify the current supported build. |

### Important Non-Blocking Debt

- `control.rs` is over 12,000 lines and `workspace.ts` is over 2,500 lines.
- Several frontend modules exceed 1,500 lines and mix rendering, persistence, orchestration, and IPC responsibilities.
- Frontend tests emit React `act(...)` warnings and attempt unmocked Tauri IPC calls.
- The production frontend contains a roughly 3.72 MB icon chunk and a 1.20 MB application chunk before compression.
- The CI workflow does not test the CLI, marketing site, shell gates, probe scripts, formatting, coverage, or dependency policy.
- GitHub Actions use movable version tags instead of full commit SHA pins.
- The manual smoke checklist is extensive, mostly unchecked, and not recorded per release.

## Stability Policy

### Severity

- `P0`: security compromise, destructive data loss, arbitrary unintended process control, broken updater trust, or an unrecoverable install.
- `P1`: core workflow failure, app freeze, lost session continuity, incorrect privilege, startup failure, or repeatable packaged-app crash.
- `P2`: degraded workflow with a safe workaround, material performance regression, or incorrect non-destructive state.
- `P3`: cosmetic, documentation, or low-impact maintenance issue.

### Release Rules

- Stable releases have zero open P0 or P1 defects.
- Beta releases may have documented P2 defects but no open P0 or P1 defects.
- Every release candidate comes from a protected commit that passed all required checks.
- A release is immutable after publication.
- A failed release is superseded by a new version, never silently replaced.
- Every state-format change has forward migration tests and a documented rollback boundary.
- Security-sensitive and process-changing code requires review by someone other than the author when the project has more than one active maintainer.

## Target CI Architecture

The stable gate is a set of independent required checks so a failure names the broken contract.

### Pull Request Checks

| Check | Required work |
| --- | --- |
| Repository policy | Validate version consistency, lockfiles, generated-file policy, workflow syntax, and forbidden untracked release inputs. |
| Frontend quality | Run formatting, ESLint, `tsc --noEmit`, Vitest, and a production Vite build. |
| Frontend tests | Fail on unexpected console errors, React warnings, unhandled rejections, and leaked Tauri calls. |
| Rust quality | Run `cargo fmt --check` and Clippy with `-D warnings` on the declared stable toolchain. |
| Rust tests | Build the real MCP binary and run `cargo test --workspace`. |
| MSRV | Compile the supported crates using the declared minimum Rust version. |
| CLI | Run format, Clippy, and tests for `apps/cli`. |
| Site | Install from its lockfile, build, lint, and run any site tests. |
| Dependency policy | Run Rust advisory and license checks, pnpm audit, npm audit for the site, and GitHub dependency review. |
| Static security | Run CodeQL for Rust, JavaScript, and TypeScript where supported. |
| Browser E2E | Run critical frontend workflows against deterministic mocked IPC with screenshots at supported viewport sizes. |
| Windows compile | Compile and test Windows-specific Rust paths on `windows-latest`. |

Coverage thresholds should begin as a recorded baseline and ratchet upward.
The gate should initially forbid coverage regression on changed modules rather than encourage low-value tests for a global percentage.

### Nightly Checks

- Build a production-like Windows installer.
- Install it silently into an isolated test user profile.
- Launch against a disposable WSL2 distribution or a dedicated Windows and WSL test machine.
- Exercise terminal spawn, text input, resize, detach, kill, worktree create and remove, Claude or fixture-agent supervision, restart recovery, MCP read and control tiers, and updater discovery.
- Run a multi-hour output, resize, spawn, close, and restart soak test.
- Record screenshots, diagnostic logs, process counts, handle counts, memory, CPU, and failure artifacts.
- Run the full manual smoke checklist only for interactions that cannot yet be automated.

### Release Checks

- Require all PR and nightly checks on the exact release commit.
- Require version equality across package metadata, Cargo metadata, Tauri configuration, tag, updater manifest, and release title.
- Build only from a protected tag created from `main`.
- Produce an Authenticode-signed executable and NSIS installer with a trusted timestamp.
- Produce the Tauri updater signature separately.
- Generate SHA-256 checksums, an SBOM, dependency-license inventory, provenance attestation, and build metadata.
- Install and launch the final downloaded artifact, not an intermediate build directory.
- Verify upgrade from the previous stable version and a clean install.
- Verify uninstall behavior and state-retention behavior.
- Publish only after the installed artifact passes the release smoke suite.

## GitHub Protection Target

Before Beta:

- Enable the dependency graph, Dependabot alerts, and Dependabot security updates.
- Enable secret scanning, non-provider patterns, validity checks, and push protection where GitHub makes them available.
- Require branches to be up to date before merging.
- Enforce branch protection for administrators.
- Require conversation resolution.
- Require at least one approving review when a second maintainer is available.
- Dismiss stale approvals after new commits.
- Restrict direct pushes to `main`.
- Delete merged branches automatically.
- Pin third-party Actions to full commit SHAs and document the update process.
- Minimize each workflow's `GITHUB_TOKEN` permissions.

The required-check names must be stable and owned by workflows in the default branch.
Stacked pull requests should target `main` before final approval so all production checks run on the actual integration base.

## Product Hardening Workstreams

### PR-0: Close the Current Integration Wave

1. Complete and record the independent review for PR #72.
2. Retrigger and pass required checks for PR #67.
3. Build the combined PR #67 and PR #73 installer.
4. Verify create, no-duplicate, restart-and-restore, and control-capability spawn behavior on Windows and WSL.
5. Merge #67, rebase #73 onto `main`, run the full gate, and merge it.
6. Build `0.3.65` as an internal stabilization baseline.
7. Do not call `0.3.65` stable or publish it to the stable update channel.

Exit gate: the baseline is installed, its version is unambiguous, and all current handoff checks are recorded with artifacts.

### PR-1: Make CI Honest

1. Fix all Clippy warnings and switch CI to `-D warnings`.
2. Add `cargo fmt --check` and a committed Rust toolchain policy.
3. Raise the MSRV to the real minimum or replace APIs that exceed Rust 1.77.
4. Add ESLint and formatting checks for the desktop frontend.
5. Fix all frontend test warnings and fail tests on unexpected stderr or console output.
6. Add CLI and site jobs.
7. Add dependency and license policy checks.
8. Add Windows compilation and Windows-specific tests.
9. Run CI on pushes to `main` as a branch-health backstop.

Exit gate: every required check is clean, warning-free, reproducible from lockfiles, and required by branch protection.

### PR-2: Secure the Build and Repository

1. Enable GitHub security features and dependency updates.
2. Resolve or explicitly remove the vulnerable `seti-icons` dependency path.
3. Upgrade the site to supported, non-vulnerable Next.js and PostCSS versions.
4. Add `cargo-audit` or `cargo-deny` advisory checks and a license allowlist.
5. Pin Actions to immutable SHAs.
6. Add CodeQL and dependency-review workflows.
7. Add `SECURITY.md`, a license, support policy, privacy statement, and responsible-disclosure process.
8. Document every secret used by release automation and its rotation procedure.

Exit gate: no unresolved critical or high vulnerability is present in shipped code, and every exception has an owner, rationale, exposure analysis, and expiry date.

### PR-3: Harden the Desktop Boundary

1. Define and enforce a production CSP.
2. Remove release devtools or place them behind an explicit diagnostic build feature.
3. Audit Tauri capabilities and narrow them by window and command where practical.
4. Threat-model terminal output, clipboard access, shell opening, file reads and writes, remote peers, updater input, control tokens, and agent-provided content.
5. Verify OSC 52, URL handling, path confinement, command argument construction, and untrusted HTML or Markdown rendering.
6. Add negative tests for privilege escalation, cross-ship access, malformed frames, replay, token rotation, and audit-sink failure.
7. Verify DPAPI and key-file permissions in the packaged Windows build.

Exit gate: the threat model has no unowned high-risk finding, and automated tests cover each security boundary.

### PR-4: Build Real End-to-End Coverage

1. Add browser-level UI tests with a deterministic Tauri IPC simulator.
2. Cover first launch, workspace and tab operations, terminal tiles, settings, recovery, errors, focus, keyboard navigation, and captain flows.
3. Add screenshot assertions for desktop minimum, normal, wide, and high-DPI viewports.
4. Add control-plane E2E tests that use real tmux and the real MCP binary.
5. Add a packaged Windows harness using a dedicated Windows and WSL runner.
6. Exercise actual WebView2 rendering, `wsl.exe`, tmux, NSIS install, app restart, and updater behavior.
7. Convert `docs/SMOKE-TEST.md` items into automated cases or release evidence entries.

Exit gate: the stable critical path runs unattended from installer to recovered session, with screenshots and logs retained for failures.

### PR-5: Prove Reliability and Recovery

1. Define schema versions for every persisted format.
2. Test upgrades from the previous three supported releases.
3. Test corrupt, partial, missing, locked, and read-only state.
4. Provide user-visible backup, reset, diagnostics export, and recovery instructions.
5. Rotate and bound diagnostic logs, redact secrets and terminal content, and document retained data.
6. Record clean and unclean shutdown markers.
7. Test app crash, WebView reload, agent death, tmux death, WSL shutdown, Windows restart, network loss, and disk-full behavior.
8. Add soak tests for terminal output, resize churn, tab churn, worktree churn, repeated recovery, and control connections.

Exit gate: no tested failure mode causes silent state loss, indefinite hangs, uncontrolled process growth, or an unrecoverable startup loop.

### PR-6: Make Releases Trustworthy

1. Acquire and protect a Windows Authenticode signing identity.
2. Configure Tauri or the release workflow to sign and timestamp the executable and installer.
3. Separate alpha, beta, and stable update channels.
4. Make the release workflow depend on the exact commit's complete CI result.
5. Generate checksums, SBOM, license inventory, provenance, and release notes.
6. Test clean install, upgrade, downgrade boundary, uninstall, and rollback.
7. Publish a release support matrix and known-issues section.
8. Align README, source versions, tags, manifests, and GitHub Releases.

Exit gate: a new machine can verify publisher identity, install, launch, upgrade, recover, and uninstall using only published artifacts and documentation.

### PR-7: Reduce Structural Risk

1. Add characterization tests around `control.rs` and `workspace.ts` before moving behavior.
2. Split control transport, authentication, authorization, audit, request lifecycle, terminal operations, organization operations, and orchestration into owned modules.
3. Split workspace persistence, terminal lifecycle, tab layout, recovery, worktrees, and orchestration into focused stores and services.
4. Break large UI modules into state controllers and presentational components without duplicating global state.
5. Add dependency-boundary rules so extracted modules do not collapse back into god files.
6. Measure build size and remove the full icon payload or generate a used-icon subset.

Exit gate: process-changing code has clear module boundaries, isolated tests, and no single general-purpose file is the default destination for new behavior.

## Operational Readiness

Stable requires an operator-facing runbook that covers:

- Supported Windows, WebView2, WSL, Ubuntu, tmux, Git, Claude Code, and Codex versions.
- Installation prerequisites and automatic prerequisite detection.
- Log locations, rotation, redaction, and diagnostics export.
- Health checks for T-Hub, tmux, the agent bridge, MCP, voice engines, and updater.
- Backup, restore, reset, and uninstall procedures.
- Token and signing-key rotation.
- Vulnerability intake and emergency release procedure.
- Failed update recovery and rollback limits.
- Known data-loss boundaries, especially WSL shutdown and provider transcript retention.
- A reproducible incident template with version, commit, environment, logs, and steps.

Remote telemetry is not required for the first stable release.
If telemetry or crash reporting is introduced, it must be opt-in, disclose transmitted fields, exclude terminal content and secrets, and provide a local-only mode.

## Measurable Release Gates

### Alpha Gate

- The `0.3.65` integration baseline is installed and manually verified.
- CI is warning-free and runs on pull requests and `main`.
- Known high-severity dependency findings are resolved.
- Version metadata is consistent.
- No open P0 defect exists.

### Beta Gate

- Branch and repository security controls are enabled.
- A production CSP is active and release devtools are disabled.
- Browser E2E covers all core UI workflows.
- Packaged Windows and WSL smoke automation covers spawn, input, resize, detach, kill, restart, restore, MCP, and install.
- Upgrade tests pass from the previous supported release.
- A 24-hour soak completes without a P0 or P1 failure, uncontrolled memory growth, process leakage, or UI hang.
- License, security, privacy, support, and operator documentation exist.
- No open P0 or P1 defect exists.

### Stable Gate

- The exact release artifact passes clean-install, upgrade, recovery, and uninstall tests.
- Executable and installer Authenticode signatures validate with a trusted timestamp.
- Updater signature, checksums, SBOM, provenance, and release metadata validate.
- All required CI, nightly, security, and release checks pass on the release commit.
- The release candidate completes a representative seven-day dogfood period without a P0 or P1 regression.
- Rollback and emergency patch procedures have been exercised once.
- Public documentation names the supported version and known limitations accurately.

## Execution Order

The dependency order is:

1. Close the current integration wave and establish `0.3.65` as the internal baseline.
2. Make CI honest and enable repository security controls.
3. Resolve known dependency and desktop-boundary security gaps.
4. Build browser and packaged Windows E2E coverage.
5. Prove recovery, migration, soak, and failure behavior.
6. Build the trusted release chain and enter Beta.
7. Reduce architectural concentration while Beta evidence accumulates.
8. Cut Stable only when every Stable gate is evidenced by an artifact or test result.

This order deliberately builds the measurement and trust system before broad refactoring.
Without that foundation, large structural changes would increase uncertainty while the project still lacks production-platform regression detection.

## Evidence Record

Each candidate release should add a short record under `docs/releases/<version>.md` containing:

- Commit and tag.
- CI and nightly run links.
- Installer checksum and signing identity.
- Clean-install, upgrade, recovery, and uninstall results.
- Automated E2E result and retained artifact location.
- Manual checks that remain and the person who performed them.
- Open P2 and P3 defects with links.
- Rollback boundary and previous supported release.

No unchecked Markdown checklist is release evidence by itself.
Evidence must identify the tested artifact, environment, result, and date.
