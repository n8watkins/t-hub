# Codex harness (Phase 1)

How T-Hub launches, steers, and recalls an OpenAI Codex (`codex-cli`) agent, and the doctrine that keeps it safe.
Phase 1 adds the adapter seam, the spawn presets, MCP provisioning, and resume wiring (PR-A); the `codex exec --json` lifecycle producer (PR-B) and the continuity catalog (PR-C) build on top.
Originally verified against `codex-cli 0.142.5`; MCP provisioning was re-verified against `codex-cli 0.144.3` on 2026-07-13.

## The seam

The launch path is harness-opaque: T-Hub wraps any `startupCommand` string in a login shell (`commands.rs::pane_command`), and the item-3 capability env is injected at the tmux SESSION level regardless of what runs in the pane.
So the harness choice rides the existing opaque command - Phase 1 adds no `SpawnOptions` field and touches none of `commands.rs`/`control.rs`/`plane.rs`/`tmux.rs`/`supervision.rs`/`fleet.rs`.

The `harness/` module (`Harness` enum + `HarnessAdapter` trait) is a pure string builder keyed off the session `provider` string.
The `provider` stays a `String` on the wire and in the DB; `Harness` is a forward-compatible accessor over it.
Unknown, legacy, or empty provider strings resolve to Claude (today's only behavior), test-locked.

## Launching a Codex crew (headless `exec`)

The crew launch pipeline is what `CodexHarness::exec_turn_argv(prompt, None, BypassPermissions)` builds:

```
codex exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check '<prompt>' | t-hub-agent --codex-tap
```

`codex exec` runs one headless turn and EXITS; the `--codex-tap` producer (PR-B) translates the Codex ThreadEvent JSONL into journal entries that flow through the existing agent bridge into the supervision reducer.
The long bypass flag is used everywhere - the `--yolo` alias is not present in the pinned Codex help.

The interactive presets (`SpawnMenu.tsx`) are separate: `codex` for a fresh interactive session, `codex resume` for Codex's own session picker.

## Steer / wake contract (load-bearing - HIGH-1)

Between turns a Codex `exec` crew pane is a plain login shell (the pane execs back into `$SHELL` after `codex exec` exits).
**Steer or wake a Codex crew ONLY by injecting the SHELL COMMAND the adapter builds - NEVER prose.**
Prose sent to a Codex crew tile would be EXECUTED as shell commands on a `--dangerously-bypass`-provisioned workspace.

The steer command is `CodexHarness::exec_turn_argv(next_prompt, Some(thread_id), BypassPermissions)`:

```
codex exec resume '<thread-id>' --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check '<next prompt>' | t-hub-agent --codex-tap
```

The built-in fleet wake is already safe (it targets the ORCHESTRATOR's terminal, `fleet.rs`, harness-agnostic).
The hazard is user-configured `store/rules.ts` send-text rules and captain relays: those must use the shell command above for a Codex crew, not prose.
Phase 2 may add a plane-level guard keyed on client type.

**Captain-dir doctrine to fold (outside this repo):** add this bolded prose-steer rule to the shipmate skill doctrine and the crew-brief escalation block (`~/.t-hub/captain/`).

## Resume + crew migration (D5)

A Codex thread id is a UUIDv7 read from the `thread.started` event and the rollout filename (`~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuidv7>.jsonl`); the producer journals it so the captain roster records it at spawn, exactly like a Claude session UUID.

- Interactive resume: `codex resume '<id>'` (`CodexHarness::resume_argv`).
- Headless resume turn: `codex exec resume '<id>' ...` (`exec_turn_argv` with `Some(id)`).
- **Crew migration doctrine:** move a Codex crew between terminals with `codex resume '<uuid>'` - the mirror of the `claude --resume <uuid>` directive (record the UUID in the roster at spawn; never fresh re-kick).

Explicit non-goal for Phase 1: `db.rs` orphan recovery stays Claude-keyed.

## Permission map

`HarnessAdapter::permission_map` maps a t-hub `PermMode` onto each harness's flags (exact strings test-locked):

| t-hub mode | Codex flags |
|---|---|
| `BypassPermissions` (Crew default) | `--dangerously-bypass-approvals-and-sandbox` |
| `AcceptEdits` (approximate) | `--sandbox workspace-write` (no exact analog; network off by default, so no `git push`) |
| `Default` / read | `--sandbox read-only` |

`BypassPermissions` is the General-authorized default local execution mode for dispatched Codex and Claude Crew in this Captain fleet.
For interactive Codex Crew, T-Hub must launch the provider with the native `--dangerously-bypass-approvals-and-sandbox` flag.
The separate `--skip-git-repo-check` option is appended only to headless `codex exec` turns so they can run in newly created worktrees; it is not part of the permission map.
Bypass is intentional full-worktree local authority without Codex approval prompts, but it does not expand the assigned card, files, worktree, branch, product scope, or external authority.
Crew must still test, log through sanctioned exact-run surfaces, commit verified logical changes separately, and report exact evidence.
Crew must not merge, push, install, deploy, publish, release, alter Powder authority, or decide product, security, destructive, spending, or outward-facing actions without the applicable Captain or General authorization.
Crew must stop the affected path and escalate when scope, credentials, or authority are ambiguous.

Dispatch success requires authoritative post-launch evidence from the foreground provider-native Codex process with the exact expected permission posture.
T-Hub persists and returns that effective Harness permission separately from the Crew's T-Hub control capability and Powder claim.
Missing, stale, conflicting, wrong-provider, wrapper-obscured, unreadable, or changed launch evidence fails closed and transactionally rolls back the terminal, Crew binding, and exact Powder claim.
Before `exec` of today's unmirrored interactive Codex TUI, the owning pane invokes `t-hub-agent --codex-unobserved` so runtime health is visibly degraded, transport is unavailable, and status is unknown instead of falsely Working.
Failure to establish that degraded marker prevents Codex launch.
A future provider-native hook or trusted app-server mirror remains the structured telemetry path, and any later permission posture that is missing, unobserved, or changed must fail closed or remain visibly degraded until authoritative evidence restores confidence.
Harness bypass is not Powder authorization, T-Hub control authority, or General authority.

## Provisioning

`scripts/captain/install-thub-codex.sh` builds the release MCP binary, atomically installs it at `~/.t-hub/bin/t-hub-mcp`, deploys the provisioner, and registers the server.
`scripts/captain/ensure-thub-codex.sh` is the idempotent registration-only entry point and converges an uncustomized stale command path to the installed binary through `codex mcp remove` plus `codex mcp add`.
When a Codex registration has tool allowlists, denylists, timeouts, environment, arguments, or another user-authored policy, provisioning preserves it if the command is already correct and otherwise refuses to repoint it.
Claude registration follows the same preserve-or-refuse rule for custom arguments and environment.
It never hand-writes `config.toml` - `codex mcp add` merges natively and preserves user `[hooks]`/`[hooks.state]` trust blocks byte-for-byte.
Codex MCP registration is user-global (`$CODEX_HOME/config.toml`), not per-repo like Claude's `.mcp.json`; least-privilege still holds because the READ capability token is injected at the tmux session level and inherited by the `t-hub-mcp` child.
Skill and command drift is refused by default.
After inspecting the drift, run `scripts/captain/install-thub-codex.sh --repair-skills` to replace it intentionally, or run `scripts/captain/install-captain-skills.sh --repair` when only skills need repair.
Start a new Codex session after installation so the new MCP tool catalog is loaded.

## Tab naming (MED-4)

Under the `codex exec --json | t-hub-agent --codex-tap` pipeline, tmux's `pane_current_command` may report `t-hub-agent` (the pipeline tail), so `store/clientType.ts`'s title fast path can miss `codex`.
The word-boundary fallback rescues it via the tab label, so name Codex crew tabs `codex-<name>` (shipmate doctrine).
