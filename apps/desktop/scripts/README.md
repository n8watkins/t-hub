# T-Hub desktop helper scripts

Operator/host-side shell helpers for the T-Hub desktop app.
These are thin clients of the running app; they are not built or bundled.

| Script | Purpose |
| --- | --- |
| `announce.sh` | Captain voice announcements (TTS) with the canonical Scribe dictation gate. |
| `mcp_proof.sh` | End-to-end proof harness for the local MCP server. |
| `bump-version.sh` | Release version bump helper. |

## announce.sh

Speaks a line of text through the local TTS servers (Kokoro `127.0.0.1:7478`, Piper `127.0.0.1:7477`), falling back to Windows SAPI when the selected engine's server is down.
Settings come from `~/.t-hub/captain/voice.json` (`enabled`, `engine`, `voice`, `volume`, `sapiRate`) - the same file the app reads (see `src-tauri/src/voice.rs`).

```
announce.sh "crew one is blocked" [voice]
```

### Scribe voice-gate (canonical)

Before speaking, `announce.sh` asks the T-Hub app's **authoritative** `scribe_status` over the control socket - the exact same source of truth the in-app voice watcher (`src/lib/voiceAnnounce.ts`) polls.
The app (`src-tauri/src/scribe.rs`) decides "is the general dictating?" from Scribe's live status file **with pid-liveness plus a staleness backstop**, and already **fails open** (reports not-listening) whenever it cannot positively confirm active dictation.
So this script does not re-read the status file or keep its own weaker copy of the gate logic; it consults the one gate.

Behavior:

- **While the general dictates:** defer (wait, bounded), then speak shortly after they stop - mirroring the in-app hold-and-flush so a cue is never dropped.
- **Fail open (speak now):** if the control socket / app is unreachable, or the answer is anything but a confident "listening" - voice is never lost when the app or Scribe is closed.
- **Deliberate divergence from the in-app path:** the in-app watcher coalesces held cues to a single latest announcement; this shell path is one process per cue and cannot coalesce across invocations, so multiple deferred cues each speak (roughly together) once dictation stops. For the low-frequency captain path this is acceptable and is preferred over dropping.

The control-socket handshake is discovered from `~/.t-hub/control.json`.
`scribe_status` is a Read-tier command, so the script uses the least-privilege `read_token`.

Env overrides:

| Var | Default | Meaning |
| --- | --- | --- |
| `SCRIBE_GATE_DISABLE` | `0` | `1` = never gate (always speak now). |
| `SCRIBE_DEFER_MAX_S` | `120` | Hard cap on deferral; at the cap the cue speaks anyway (fail open) so it is never lost. |
| `SCRIBE_POLL_S` | `0.3` | Re-poll interval while deferring. |
| `SCRIBE_TAIL_S` | `0.5` | Quiet tail after dictation stops before speaking. |
| `T_HUB_CONTROL_FILE` / `T_HUB_CONTROL_ADDR` / `T_HUB_CONTROL_TOKEN` | - | Override control-socket discovery (see `control.json`). |

### Install

The captain voice path runs `~/.t-hub/captain/announce.sh`.
Point it at this tracked copy so both voice paths share one reviewable script (single source of truth):

```sh
ln -sf "$PWD/apps/desktop/scripts/announce.sh" ~/.t-hub/captain/announce.sh
```

Run that from the repo root of your primary checkout (not a throwaway git worktree, so the symlink target is stable).

### Verifying the gate

`scripts/probes/scribe_gate_e2e.py` drives the real Scribe status file and checks the live app's `scribe_status` answer (pid-liveness + fail-open) over the control socket.
The in-app watcher's hold/coalesce/flush logic is covered by `src/lib/voiceAnnounce.test.ts`.
