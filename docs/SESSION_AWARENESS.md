# Agent-session awareness (0.5 supervision sidebar - LIVE)

This makes the 0.5 supervision sidebar show live Codex and Claude session data.
The orchestrator-to-subagent tree, FR-012 status, WSL health, and provider usage all update from the event spine:

```
Provider hook -> WSL journal -> t-hub-agent -> core (Tauri) -> UI
```

## The live event spine (what was wired)

The frontend subscribes to the bridge event channels below.
The five shared channels are named by `Events05` in `src/ipc/types.ts`, while the workspace store owns the title subscription:

| Channel | Emitted when | Payload (TS) |
| --- | --- | --- |
| `agent://journal` | core consumes a new live journal entry | `JournalEvent` |
| `agent://title` | a session's derived title changes | `{ sessionId, cwd?, title }` |
| `supervision://tree` | a session's subagent tree changes | `SupervisionTree` |
| `session://status` | a session's FR-012 status changes | `SessionStatusEvent` |
| `agent://state` | connection state or journal cursor changes | `AgentStateInfo` |
| `status://snapshot` | a statusline snapshot is ingested | `StatusSnapshot` |

The emit sink is `src-tauri/src/agent/emit.rs` (`EventEmitter` trait plus the control-socket implementation).
It is installed on `AgentBridge` during application setup.
`AgentBridge::consume_journal_entry` is the single ingestion point for committed entries.
Live entries fan out immediately, while cold replay rebuilds backend authority without forwarding each historical `agent://journal` event.
After the verified replay boundary commits, the bridge emits one bounded latest title, status snapshot, supervision tree, and session-status snapshot for each affected session.
Entries that arrive live while replay is in progress are buffered, deduplicated against the committed replay boundary, and published afterward in sequence order.
All state transitions go through `set_state()`, which emits `agent://state`.
Emission is best-effort and a no-op before `set_emitter()` runs, so unit tests are unaffected.

### Status model (FR-012)

`working / waitingOnSubagents / needsQuestion / needsPermission / completed /
failed` are derived by the supervision reducer (`src-tauri/src/supervision.rs`)
from the real hook stream and emitted per session. **`rateLimited` is NOT a
reducer state** — it is a statusline *overlay*: the UI shows `rateLimited` when a
`rate_limits.*.used_percentage` is ≥ 90% **and** the session is otherwise
working/waiting (`displayStatus()` in `src/store/supervision.ts`). The overlay is
applied to the attention queue and the tree badges in the sidebar.

## Installing `t-hub-agent` (required for the bridge to connect)

The core launches the agent over stdio.
Packaged **Windows** builds carry the exact x86-64 Linux helper built from the same source tree as the desktop executable.
Before connecting, packaged startup validates that resource and atomically installs it as `~/.local/lib/t-hub/agents/<sha256>/t-hub-agent` in the configured WSL distro when needed.
The bridge launches that exact digest-versioned path after verifying the installed SHA-256 digest.
Production and development packages with different helper digests can run side by side without replacing one another's verified executable.
If deployment or verification fails, the bridge stays disconnected instead of falling back to another `t-hub-agent` on `PATH`.
The explicit **`T_HUB_AGENT_BIN`** developer override bypasses packaged deployment and is spawned verbatim with the agent arguments on unix and Windows dev builds.
Packaged Windows builds ignore the override and always require the bundled, verified helper.
On a **unix dev box**, the bridge spawns `t-hub-agent --stdio` directly unless that override is set.

### Packaged Windows build

`pnpm tauri build` runs the release resource preparation automatically.
On Windows, the preparation step builds `t-hub-agent` inside WSL with an isolated Cargo target directory, validates the ELF artifact, and copies it into the Tauri resources before packaging.
That default build path requires a Rust toolchain in the target WSL environment.
For an externally built Linux helper, set `T_HUB_AGENT_RESOURCE_SOURCE` to its path before invoking the package build.

```powershell
$env:T_HUB_AGENT_RESOURCE_SOURCE = "C:\path\to\t-hub-agent"
pnpm tauri build
```

The distro is `Ubuntu-24.04` by default.
Override it with `T_HUB_DISTRO`, which is read by `lib.rs::default_distro`.
The packaged bridge launches through `wsl.exe` without a shell and does not depend on the distro's `PATH`:

```text
wsl.exe -d <distro> --cd ~ -e \
  /home/<user>/.local/lib/t-hub/agents/<sha256>/t-hub-agent --stdio
```

### Dev box (this repo, run inside WSL/Linux directly)

```sh
cd apps/desktop
cargo build --manifest-path src-tauri/Cargo.toml -p t-hub-agent
install -m 0755 src-tauri/target/debug/t-hub-agent ~/.local/bin/t-hub-agent
command -v t-hub-agent      # /home/<you>/.local/bin/t-hub-agent
```

Now `pnpm tauri dev` will connect.
The bridge spawns `t-hub-agent --stdio`, handshakes (`Hello`/`Ready`), replays the journal, and goes live.
Connection lifecycle operations are serialized, and a reconnect fully retires the current helper before starting its replacement.
The replacement becomes live only after the protocol version matches and any required replay reaches its verified durable boundary.
Handshake errors, timeouts, malformed frames, incomplete replay, and replacement failure terminate the candidate helper and leave the bridge failed rather than publishing partial state.
Replay-only frames received after a verified replay commit fail the live transport without publishing their contents.
Use the developer override for a one-off without touching `PATH`:

```sh
T_HUB_AGENT_BIN=$PWD/src-tauri/target/debug/t-hub-agent pnpm tauri dev
```

## Installing provider hooks (consent-gated)

The hooks populate the journal.
The Claude and Codex panels live in **Settings -> Hooks**.
The Claude panel merges into `~/.claude/settings.json`, preserves existing hooks and non-hook keys, makes a one-time `settings.json.t-hub-bak`, and removes only T-Hub's marker-tagged entries on uninstall.
It installs handlers for the 15 verified lifecycle hooks (`SessionStart`, `Stop`, `SubagentStart`, `SubagentStop`, `Elicitation`, and the other supported events), each invoking `<resolved-agent-path> --hook <EVENT>`.
On packaged Windows builds, install and startup repair use the same digest-versioned helper path that the bridge deployed and verified.
Codex hook ownership, trust, repair, and privacy constraints are documented in [CODEX-HARNESS.md](./CODEX-HARNESS.md#lifecycle-hooks).

> Each hook is a separate short-lived process that appends to the journal file.
> The long-lived `--stdio` agent's tail thread observes the file's growth (`Journal::head_seq_on_disk`) about every 200 ms, not just its own in-memory head, so externally appended hook events stream live instead of waiting for reconnect and replay.

## Demo / verification

`live_emit_demo_hook_sequence_to_supervision_tree` (a gated integration test in
`src-tauri/src/agent/connection.rs`) drives the **real** binary through the
production hook entrypoint and asserts both emit paths:

- **replay** — `SessionStart → UserPromptSubmit → SubagentStart → Stop` fired
  before connect → agent replays on handshake → core emits `supervision://tree`
  `{status: waitingOnSubagents}`;
- **live tail** — `SubagentStop` fired *after* connect → agent tail streams it →
  core emits `session://status` `{status: completed}`.

```sh
cargo build --manifest-path src-tauri/Cargo.toml -p t-hub-agent   # build first
cargo test  --manifest-path src-tauri/Cargo.toml --lib live_emit_demo \
  -- --nocapture --test-threads=1
# → live_emit_demo: replay path emitted waitingOnSubagents ✓
# → live_emit_demo: live tail path emitted completed ✓
```

The test skips gracefully when the binary isn't built, so CI never fails
spuriously.
