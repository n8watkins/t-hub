# Claude-session awareness (0.5 supervision sidebar — LIVE)

This makes the 0.5 supervision sidebar show **live** Claude-session data: the
orchestrator→subagent tree, FR-012 status, WSL health, and statusline usage all
update as Claude runs, driven by the event spine

```
Claude hook → WSL journal → t-hub-agent → core (Tauri) → UI
```

## The live event spine (what was wired)

The frontend already subscribed to five Tauri event channels (`Events05` in
`src/ipc/types.ts`); the backend now actually **emits** on them:

| Channel               | Emitted when                                   | Payload (TS)         |
| --------------------- | ---------------------------------------------- | -------------------- |
| `agent://journal`     | core consumes a journal entry (stream/replay)  | `JournalEvent`       |
| `supervision://tree`  | a session's subagent tree changes              | `SupervisionTree`    |
| `session://status`    | a session's FR-012 status changes              | `SessionStatusEvent` |
| `agent://state`       | connection state / journal cursor changes      | `AgentStateInfo`     |
| `status://snapshot`   | a statusline snapshot is ingested              | `StatusSnapshot`     |

The emit sink is `src-tauri/src/agent/emit.rs` (`EventEmitter` trait +
`TauriEmitter`). It is installed onto the `AgentBridge` in `lib.rs`'s `setup()`
(after the `AppHandle` exists). `AgentBridge::consume_journal_entry` — the single
ingestion point — fans the events out; all state transitions go through
`set_state()`, which emits `agent://state`. Emission is best-effort and a no-op
before `set_emitter()` runs, so unit tests are unaffected.

### Status model (FR-012)

`working / waitingOnSubagents / needsQuestion / needsPermission / completed /
failed` are derived by the supervision reducer (`src-tauri/src/supervision.rs`)
from the real hook stream and emitted per session. **`rateLimited` is NOT a
reducer state** — it is a statusline *overlay*: the UI shows `rateLimited` when a
`rate_limits.*.used_percentage` is ≥ 90% **and** the session is otherwise
working/waiting (`displayStatus()` in `src/store/supervision.ts`). The overlay is
applied to the attention queue and the tree badges in the sidebar.

## Installing `t-hub-agent` (required for the bridge to connect)

The core launches the agent over stdio. On **Windows** it runs inside WSL via
`wsl.exe -d <distro> -- t-hub-agent --stdio`; on a **unix dev box** it spawns
`t-hub-agent --stdio` directly. Either way the binary must be resolvable, or
you can point at it explicitly with **`T_HUB_AGENT_BIN`** (overrides argv[0]).

### Build it

```sh
cargo build --manifest-path src-tauri/Cargo.toml -p t-hub-agent
# → src-tauri/target/debug/t-hub-agent
```

### Windows path: install into the WSL distro (so `wsl.exe … t-hub-agent` finds it)

The agent runs **inside** the distro, so install the Linux build onto the
distro's `PATH`. From the WSL distro shell:

```sh
# Build the linux binary inside WSL (or copy a prebuilt one in), then:
install -m 0755 src-tauri/target/debug/t-hub-agent ~/.local/bin/t-hub-agent
#   ~/.local/bin is on PATH in a default Ubuntu login shell. /usr/local/bin
#   (sudo) also works and is visible to non-login `wsl.exe -- …` invocations.
command -v t-hub-agent     # must print a path
t-hub-agent --version      # t-hub-agent 0.5.x
```

If `~/.local/bin` is not on the non-interactive `wsl.exe` `PATH`, prefer
`/usr/local/bin`, or set the escape hatch on the Windows side:

```powershell
setx T_HUB_AGENT_BIN "wsl.exe"   # not typical; usually just install on PATH
```

The distro is `Ubuntu-24.04` by default; override with the `T_HUB_DISTRO` env
var (read in `lib.rs::default_distro`).

### Dev box (this repo, run inside WSL/Linux directly)

```sh
install -m 0755 src-tauri/target/debug/t-hub-agent ~/.local/bin/t-hub-agent
command -v t-hub-agent      # /home/<you>/.local/bin/t-hub-agent
```

Now `pnpm tauri dev` will connect: the bridge spawns `t-hub-agent --stdio`,
handshakes (Hello/Ready), replays the journal, and goes `live`. Escape hatch for
a one-off without touching PATH:

```sh
T_HUB_AGENT_BIN=$PWD/src-tauri/target/debug/t-hub-agent pnpm tauri dev
```

## Installing the Claude hooks (consent-gated)

The hooks are what *populate* the journal. The **HookInstallPanel** lives in
**Settings → Hooks** (mounted in `ThemeEditor.tsx`, not the sidebar): a consent
checkbox → Install. It is non-destructive: it merges into
`~/.claude/settings.json`, preserves your existing hooks + non-hook keys, makes a
one-time `settings.json.t-hub-bak`, and ships a clean uninstall that removes
only T-Hub's marker-tagged entries. It installs handlers for the 15 verified
lifecycle hooks (`SessionStart … Stop … SubagentStart/Stop … Elicitation …`),
each a `t-hub-agent --hook <EVENT>` one-liner.

> Each hook is a separate short-lived process that appends to the journal file.
> The long-lived `--stdio` agent's tail thread observes the **file's** growth
> (`Journal::head_seq_on_disk`) every ~200 ms — not just its own in-memory head —
> so externally-appended hook events stream live (not only on reconnect+replay).

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
