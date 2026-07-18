# Codex Permission Observability Independent Review Packet

Date: 2026-07-17

Powder card: `thub-codex-permission-observability`

Powder run: `run-wTSwcbUVsq4A`

Integration branch: `review/codex-permission-observability-wave0`

Reviewed coordinator base: `ed21f896f7e582519a02d9d6f32f386579008a8f`

This packet prepares the card for independent review.
It is evidence, not an approval or authorization to merge, push, install, deploy, publish, or release.

## Integration Finding

The exact reviewed coordinator base already contains patch-equivalent versions of every implementation commit from `7cc10f0` through `0c23665`.
`git cherry review/codex-permission-observability-wave0 feat/codex-permission-observability` reports all ten prior commits with `-`, so replaying or cherry-picking that branch would duplicate reviewed behavior and risk regressing later Wave 0 security work.

The equivalent commits in coordinator history are:

| Prior input | Coordinator equivalent | Behavior |
| --- | --- | --- |
| `7cc10f0` | `cc85d07` | Normalize structured Codex permission lifecycle. |
| `a55c411` | `b058a98` | Retain provider-neutral permission needs in supervision. |
| `7f895ab` | `d9b0900` | Route Crew attention only to its owning Captain. |
| `d06a75f` | `0a49b7d` | Cover recorded Codex turn boundaries. |
| `3d496ce` | `bf6c89d` | Keep the Codex tap warning-free. |
| `119f672` | `b047957` | Reject replayed and out-of-order journal entries. |
| `4c707ef` | `0e52d5a` | Parse the current app-server thread identity path. |
| `6c1c24f` | `8bdc14f` | Use credential-safe opaque fallback permission identities. |
| `3715943` | `ec7fba8` | Expose explicit degraded interactive Codex health. |
| `0c23665` | `25a87e2` | Prove the degraded interactive marker through the real agent process and tmux. |

Later integration commits preserve these semantics while binding the degraded marker to the approved Wave 0 launch-attestation contract.
In particular, `59d4000` integrates reviewed Crew launch attestation and `08beabc` restores the marker to `JournalEventType::AgentCommand` with exact tmux provenance.
The assigned base also includes subsequent immutable operation identity, authority-generation, lifecycle serialization, reviewer reconciliation, and fail-closed control remediations.
No prior feature-branch commit was transplanted over those later changes.

## End-User Reproduction

The closest practical structured reproduction runs the real `t-hub-agent --codex-tap` process over the credential-sanitized Codex 0.144.4 app-server lifecycle fixture.
It observes `thread/started`, `turn/started`, `item/commandExecution/requestApproval`, `serverRequest/resolved`, and `turn/completed` in provider order.
The persisted journal contains one provider-neutral `PermissionRequest`, keeps opaque thread, turn, item, and request correlation, and contains neither the command nor the approval reason.

The closest practical interactive reproduction launches the real agent in an isolated tmux server with `--codex-unobserved`.
The marker is written before Codex execution can proceed and contains the exact tmux session name, session id, session creation time, window id, pane id, and pane PID.
The marker reports `unknown` status with degraded, unavailable telemetry and never fabricates `Working`.

The combined real-agent launch gate also passed.
It proves that the exact owning Codex Crew receives its marker before provider execution and that failure to establish the marker remains fail closed.

## Required Behavior Evidence

- The producer normalizes the recorded structured permission request into `JournalEventType::PermissionRequest` with schema `t-hub.permission-request.v1`.
- Provider request, thread, turn, and item identities remain opaque and bounded.
- Missing provider request identities use a versioned non-content-derived opaque identity, so prompts, commands, reasons, paths, and credentials cannot influence the durable id.
- Duplicate structured callbacks are journaled once within the tap stream.
- The agent bridge rejects duplicate and out-of-order journal sequence numbers before reduction, so a replay cannot clear newer permission state.
- A permission request transitions the exact session to `needsPermission` and remains pending across degraded or disconnected telemetry.
- Only an exact structured resolution id clears that request during the same turn.
- New turn, turn completion, turn failure, thread close, and session restart boundaries clear stale permission state.
- Live and replayed reduction produce the same permission and runtime-health state.
- Fleet routing maps the provider session to the exact tmux Crew and wakes only the owning Captain.
- Peer Captains cannot bypass Crew ownership with broad `All` scope or an explicit session subscription.
- Both legacy and durable fleet-wake paths prove the same cross-ship isolation.
- Unsupported, malformed, oversized, disconnected, or unavailable structured telemetry is explicit degraded health rather than false `Working`.
- The terminal-visible fallback is versioned, bounded, credential-safe, and exact-pane-bound without parsing terminal dialogue.

## Model Capacity Finding

The installed `codex-cli 0.144.5` generated experimental app-server schemas include structured capacity signals.
`account/rateLimits/updated` carries a `RateLimitSnapshot` whose `rateLimitReachedType` distinguishes account, workspace-credit, and workspace-usage-limit exhaustion.
Turn errors also expose `codexErrorInfo: "usageLimitExceeded"` through structured error and turn lifecycle payloads.

This proves that capacity exhaustion has a structured protocol representation, but it does not prove that every interactive TUI countdown or pause dialogue is mirrored to the current T-Hub producer.
This card has no captured lifecycle fixture for the exact interactive capacity-pause dialogue, and `--codex-tap` does not currently normalize the account rate-limit notification.
No terminal dialogue parsing, blind terminal text injection, or automatic Continue behavior is added here.
Terminal-only capacity fallback remains a dependency of `thub-codex-capacity-fallback`.

## Verification

The following checks passed on the exact assigned base before this packet was added:

- `cargo test -p t-hub-agent --test codex_tap_e2e -- --nocapture`: 3 passed.
- `cargo test -p t-hub-agent codex::tests -- --nocapture`: 6 passed.
- Focused permission, replay, degraded-health, and Crew-routing library filters: 34 passed in total with no failures.
- `cargo test -p t-hub-agent`: 55 unit tests, 3 Codex tap E2E tests, and 1 exact unobserved-marker E2E test passed.
- `cargo test -p t-hub --lib -- --test-threads=1`: 891 passed, 2 documented ignored, and 0 failed.
- `scripts/captain/verify-codex-permission-integration.sh`: the ignored combined real-agent gate passed when invoked explicitly.

- `cargo fmt --all -- --check`: passed after this packet was added.
- `cargo clippy -p t-hub-agent -p t-hub --all-targets -- -D warnings`: passed after this packet was added.
- The first `cargo test --workspace -- --test-threads=1` run stopped at both MCP E2Es because their documented standalone debug `t-hub-mcp` binary precondition had not been built.
- `cargo build -p t-hub-mcp` completed, and `cargo test -p t-hub --test mcp_e2e -- --test-threads=1` then passed 2 tests with 1 helper ignored.
- `cargo test --workspace --quiet -- --test-threads=1` then passed every workspace target with only the documented ignored tests.
- `git diff --check`: passed after this packet was added.

The initial MCP E2E precondition failure is retained here so the review record does not misrepresent the verification sequence as uniformly green.

## Independent Reviewer Checklist

1. Confirm `HEAD` descends from exact base `ed21f896f7e582519a02d9d6f32f386579008a8f` and that no prior feature-branch commit was replayed over it.
2. Inspect `t-hub-agent` Codex normalization for bounded identities, credential-safe payloads, correct thread and turn correlation, and exact resolution matching.
3. Inspect the agent bridge cursor gate for replay and out-of-order rejection before supervisor reduction.
4. Inspect supervisor clearing behavior at resolution, turn, failure, completion, close, and restart boundaries.
5. Inspect both fleet wake paths for exact owning-Captain routing and peer-Captain isolation.
6. Inspect the unobserved marker for exact tmux provenance, `AgentCommand` compatibility, bounded output, and fail-closed launch ordering.
7. Confirm capacity automation remains out of scope and the residual dependency is reported without a false claim of complete interactive coverage.
8. Re-run the focused tests, combined real-agent gate, formatting, warnings-denied Clippy, and relevant workspace tests.

## Residual Risk

The checked-in permission lifecycle is credential-sanitized and schema-derived rather than a raw credential-bearing capture.
The interactive Codex TUI remains explicitly degraded until a trusted app-server mirror or provider-native lifecycle producer covers it.
Structured capacity signals exist, but the exact interactive capacity-pause flow remains unproven and intentionally unautomated under this card.
