# T-Hub MCP server

The local **MCP server** lets Claude drive T-Hub — list terminals, read
session status + the supervision tree, check WSL health, search files, and
perform safe organization actions — within the PRD §11.2 permission tiers
(PRD §9.6, §13 "1.5 — Automation and preview").

This document covers the architecture, the control-channel contract, the tool
catalog, the theme-tool contract, registration with Claude Code, and how to run
the end-to-end proof on a dev box.

---

## 1. Architecture

MCP servers are launched **by the client** (Claude) as a short-lived subprocess
that speaks JSON-RPC over **stdio**. Such a process can't share the running
T-Hub app's Tauri-managed state in-process, so T-Hub splits the
responsibility across two pieces joined by a tiny local control channel:

```
  Claude (MCP client)
        │  MCP / JSON-RPC over stdio   (initialize · tools/list · tools/call)
        ▼
  ┌──────────────────────────┐      loopback TCP, NDJSON
  │  t-hub-mcp (binary)    │  ───────────────────────────►  ┌────────────────────────────┐
  │  crates/t-hub-mcp      │   {token, command, args}       │  T-Hub app (Tauri/Rust)  │
  │                          │  ◄───────────────────────────  │  src/control.rs listener   │
  │  • MCP protocol subset   │      {ok, result | error}      │                            │
  │  • static tool catalog   │                                │  • authenticates token     │
  │  • forwards by NAME       │                                │  • dispatches by command   │
  └──────────────────────────┘                                │    name → existing surface │
                                                              │    (tmux · supervision ·   │
   discovers addr + token from                                 │     status · files)        │
   ~/.t-hub/control.json                                     └────────────────────────────┘
```

Key property: **the MCP server has no compile-time knowledge of individual
commands.** `tools/call` takes the tool name + arguments and forwards
`{command: <name>, args: <arguments>}` to the app, which dispatches dynamically.
Adding a tool is a catalog entry on one side and a `match` arm on the other — no
shared types to keep in lockstep.

### Components

| Piece | Path | Role |
| --- | --- | --- |
| MCP server binary | `apps/desktop/src-tauri/crates/t-hub-mcp/` | Speaks MCP JSON-RPC on stdio; owns the tool catalog; forwards `tools/call` to the app. |
| `protocol.rs` | `…/t-hub-mcp/src/protocol.rs` | Minimal JSON-RPC 2.0 framing (request/notification/response/error). |
| `tools.rs` | `…/t-hub-mcp/src/tools.rs` | The static tool catalog + tiers + JSON schemas. |
| `control_client.rs` | `…/t-hub-mcp/src/control_client.rs` | Discovers the app (handshake file / env), forwards one command, and re-reads `control.json` to reconnect once when the endpoint is dead (recovers across an app restart). |
| `server.rs` | `…/t-hub-mcp/src/server.rs` | The stdio loop: `initialize`, `tools/list`, `tools/call`, `ping`. |
| App control listener | `apps/desktop/src-tauri/src/control.rs` | Loopback TCP listener; authenticates the token; dispatches by command name. |
| Registration line | `apps/desktop/src-tauri/src/lib.rs` (`start_control_listener`) | Starts the listener in `setup()` with a supervisor-visitor closure. |

The listener uses **loopback TCP** (not a Unix socket) because T-Hub's primary
target is Windows, where AF_UNIX support is inconsistent; loopback TCP behaves
identically on Windows, WSL, Linux, and macOS.

---

## 2. Control-channel contract

### Transport
Newline-delimited JSON over a loopback TCP connection. One request object per
line, one response object per line.

### Request
```json
{ "token": "<per-launch secret>", "command": "list_terminals", "args": {} }
```

### Response
```json
{ "ok": true, "result": { "...": "..." } }
```
or
```json
{ "ok": false, "error": "human-readable message" }
```

### Discovery + auth (the handshake file)
On startup the app binds `127.0.0.1:0` (ephemeral port), generates a per-launch
token (a UUID), and writes both to a handshake file:

- Path: `$T_HUB_CONTROL_FILE`, else `~/.t-hub/control.json` (mode `0600` on
  unix).
- Contents: `{ "addr": "127.0.0.1:<port>", "token": "<uuid>", "pid": <pid> }`.

`t-hub-mcp` reads that file to learn where to connect and which token to
present. Two env vars override discovery (used by the proof harness and packaged
installs):

- `T_HUB_CONTROL_ADDR` + `T_HUB_CONTROL_TOKEN` — pin the endpoint directly.
- `T_HUB_CONTROL_FILE` — point at a non-default handshake path.

The token gates every request: a bad token is rejected **before** any command
runs, and the listener only accepts loopback peers.

### Recovering from an app restart

The app rebinds to a **fresh ephemeral port and a new token on every launch**,
rewriting `control.json`. A running `t-hub-mcp`, though, may have captured the
old `addr` + `token` in its env at spawn time - the app injects
`T_HUB_CONTROL_ADDR` / `T_HUB_CONTROL_TOKEN` down the spawn tree - so after a
restart that pin points at the dead pre-restart endpoint. Rather than reporting
"T-Hub is not running" until the MCP client itself is relaunched,
`resolve_and_call` recovers transparently:

- A `tools/call` whose round-trip fails at the **transport** level (connect
  refused, or the stream dies mid-exchange - exactly how a restarted app on a
  new port looks) triggers a **re-read of `control.json`**, dropping the stale
  env pair, and **one retry** against the `addr` + `token` the running app just
  wrote.
- An **app-level** rejection (bad token, unknown command, a governor refusal) is
  not a moved endpoint, so it surfaces verbatim - no re-read, no retry.
- If `control.json` names the same endpoint (or is gone/unreadable), there is
  nothing fresher to retry, so the original transport error stands - a
  genuinely-down app is still reported as a tool error, never silently swallowed.

---

## 3. Tool catalog and permission tiers (PRD §11.2)

| Tool | Tier | Default | Backed by |
| --- | --- | --- | --- |
| `list_terminals` | Read | allowed | tmux `list-sessions` on the isolated `t-hub` socket |
| `get_status` | Read | allowed | supervision status + statusline snapshot for a `sessionId` |
| `wait_for_status` | Read | allowed | long-polls the supervision reducer until a session reaches a target FR-012 status (or a timeout) |
| `supervision_tree` | Read | allowed | orchestrator→subagent tree for a `sessionId` |
| `wsl_health` | Read | allowed | host metrics from `/proc` (+ supervised-session count) |
| `search_files` | Read | allowed | fuzzy file-index search (names + metadata only, never contents) |
| `list_tabs` | Read | allowed | the live workspace tabs (`{id, name, tileIds}`) from the core's addressable tab registry — the frontend reports its layout up so this mirrors the UI |
| `list_captains` | Read | allowed | the claimed captains (`{shipSlug, captainSessionId, workspaceTabIds, crew}` + revision) from the server captains registry — the one source of truth the UI and MCP share |
| `read_terminal` | Read | allowed | a session's recent visible output via tmux `capture-pane` (plain text; optional scrollback) |
| `list_fleet_watches` | Read | allowed | the armed orchestrator wakes (who gets woken, for which sessions + states) from the fleet-watch registry |
| `scribe_status` | Read | allowed | whether the general is dictating right now - Scribe's v1 status endpoint (discovered via `~/.scribe/control.json`; `status.json` file fallback with pid + 15s TTL); `listening` mirrors Scribe's level-triggered `busy` flag and fails open to `false` when it can't tell |
| `focus_session` | Organization | allowed, **audited** | accepted + audited; UI application via the frontend command |
| `move_tile` | Organization | allowed, **audited** | applies to the authoritative server registry FIRST (unknown `tabId` is a hard error), then forwards the registry snapshot; lands even when the target tab is hidden or the window is unfocused |
| `rename_tab` | Organization | allowed, **audited** | registry-first + strict (unknown `tabId` is an error), then forwards the snapshot |
| `new_tab` | Organization | allowed, **audited** | mints the tab id **core-side** and returns it (so the tab is addressable by `move_tile`/`focus_tab` and shows in `list_tabs`); the tab is created in the BACKGROUND (the user's active tab is not switched - use `focus_tab`) |
| `focus_tab` | Organization | allowed, **audited** | the one organization command that intentionally moves the user's view; strict (the tab must exist) and mirrored into the registry's `activeTabId` |
| `close_tab` | Organization | allowed, **audited** | closes a workspace tab headlessly; refused for the LAST tab, and for a non-empty tab unless `force: true` (see the tab-lifecycle policy below) |
| `claim_captain` | Organization | allowed, **audited** | claims captaincy of a ship in the server captains registry (registry-first + strict: one captain per ship), then forwards the captains snapshot (`sync_captains`); a captain self-registers with its own session id instead of hand-editing ship files (see the captains-registry policy below) |
| `release_captain` | Organization | allowed, **audited** | releases a claim by `captainSessionId` or `shipSlug` (unknown claims are refused), then forwards the snapshot |
| `watch_fleet` | Organization | allowed, **audited** | arms a server-side push that re-invokes THIS orchestrator's loop (injects a prompt into its terminal) when a watched session (default: any captain) goes idle / needs-input / completes; idempotent (re-arming replaces the prior watch) |
| `unwatch_fleet` | Organization | allowed, **audited** | disarms the orchestrator wake previously armed with `watch_fleet` |
| `open_file` | Organization | allowed, **audited** | capped text read via the Files reader |
| `create_worktree` | Organization | allowed, **audited** | runs `git worktree add` here, resolves the target tab **by name** (reuse/create, never the focused tab), spawns the worktree terminal **server-side**, places it in the registry, and forwards the snapshot; returns `tabId` + `terminalId` synchronously; optional `spawnedBy` records the worktree terminal as a captain's crew |
| `remove_worktree` | Organization | allowed, **audited** | forwards removal to the UI (which detaches live tiles first, then runs `git worktree remove`); refused if no UI is connected, to avoid orphaning a process |
| `spawn_terminal` | **Process-changing** | **confirmation required** | the server spawns the session itself, resolves `tabName`/`tabId` against the registry (reuse-or-create, WITHOUT switching the user's active tab), places the tile, and returns the real `id` synchronously; refused only when no UI is connected at all; optional `spawnedBy` records the session as a captain's crew |
| `commission_captain` | **Process-changing** | **confirmation required** | starts a new control-capability Codex or Claude Captain and binds it to a registered, Powder-verified project |
| `attach_captain` | **Process-changing** | **confirmation required** | attaches an existing live harness only when it already holds the current control capability; read-only terminals are refused without changing their token |
| `send_text` | **Process-changing** | **confirmation required** | types literal text into an existing session via tmux `send-keys -l` (optional trailing Enter); executes |
| `send_keys` | **Process-changing** | **confirmation required** | sends named control keys (e.g. `C-c`, `Up`, `Escape`) to an existing session via tmux `send-keys`; executes |
| `close_terminal` | **Process-changing** | **confirmation required** | kills an existing session + its process tree via tmux `kill-session`; also drops the dead tile from the server registry and pushes a `sync_tabs` snapshot, so the tile leaves its tab even when that tab is hidden |
| `get_theme` / `set_theme` | Theme | forwarded by name | see the theme contract below |

### How tiers are enforced
- **Read** tools dispatch directly and return live data.
- **Organization** tools are accepted and **audited** (PRD §11.2: "allowed with
  visible audit event"). Those whose effect is a pure UI mutation
  (`focus_session`, `move_tile`, `rename_tab`, `new_tab`, `focus_tab`) return an
  audit acknowledgement (`{accepted, audited:true, applied, …}`) and forward the
  mutation to the frontend (`control://apply`); `open_file` has a real
  side-effect-free backing (the reader) and returns file contents. The two
  worktree tools (`create_worktree`, `remove_worktree`) are also Organization
  tier: `create_worktree` runs `git worktree add` here and forwards a new tab +
  spawn to the UI, while `remove_worktree` forwards the removal to the UI (which
  detaches any live tiles before `git worktree remove`) and refuses outright when
  no UI is connected, so a running process is never orphaned.
  The captain tools (`claim_captain`, `release_captain`) are Organization tier
  too: they mutate the authoritative captains registry FIRST (strict - one
  captain per ship, an unknown release is a hard error) and forward the captains
  snapshot to the UI under `args.sync` on a `sync_captains` `control://apply`, the
  captains twin of the tab-registry sync contract (see the captains-registry
  policy below).
- **Process-changing** tools all carry an explicit `CONFIRMATION REQUIRED` notice
  in their MCP `description`, an `annotations.confirmationRequired:true` boolean,
  and an `annotations.t-hubTier:"process-changing"` string so a permission-aware
  client can gate them — that confirmation contract is the user-facing gate. On
  the app side they split:
  - `spawn_terminal` is **functional** (#17, headless-org): the SERVER spawns the
    tmux session itself (same id minting + pane wrap as the Tauri spawn), places
    the tile in the registry (resolving `tabName`/`tabId`; default is the user's
    active tab), and forwards the id + snapshot for the UI to adopt.
    The real terminal id is returned synchronously.
    It is refused only when no UI is connected at all (nothing would render the
    tile). Its confirmation contract (description + annotations) is unchanged.
  - `send_text`, `send_keys`, and `close_terminal` **execute**: they act only on
    an existing `th_*` session the app already owns, driving tmux directly
    (`send-keys -l` / `send-keys` / `kill-session`). They return an audited
    acknowledgement (`{accepted, audited:true, …}`); `send_text`/`send_keys`
    return a clear "no such session" error if the target session does not exist.
- **Destructive / secret-bearing** tools (PRD §11.2 lower tiers) are simply not
  in the catalog and not dispatchable — the listener's `match` has no arm for
  them, so an unknown command is refused.

Tool failures (a gated tool, or "T-Hub is not running") come back as MCP
**tool results with `isError: true`**, not transport errors — that's how MCP
surfaces tool-level failures to the model.

### The authoritative tab registry (#22, headless-org)

For any headless tab operation to work - discover a tab id, `move_tile` a terminal into it, `focus_tab` a known id, or place a `create_worktree`/`spawn_terminal` tile in a tab named by the caller - tabs must be **addressable** over the control API, and the operation must survive the target tab being hidden or the window being minimized.
The core therefore owns the tab organization in an in-memory **tab registry** (`control::TabRegistry`: a monotonic revision `seq`, the mirrored `activeTabId`, and one `{id, name, tileIds}` record per tab).
The SERVER is the source of truth for organization:

- Every organization mutation applies to the registry FIRST (an invalid target is a hard error, never a silent accept-then-lose), then the command forwards the full registry snapshot to the UI under `args.sync` on `control://apply`.
  The UI renders FROM the snapshot (`adoptRegistry` in the workspace store), so a hidden tab or an unfocused/minimized window cannot lose the update.
- Control-originated placement NEVER switches the user's active tab or steals focus.
  `focus_tab`/`focus_session` are the explicit, intentional view switches.
- The frontend up-syncs USER-originated layout changes (drag, UI close, UI rename) via `report_workspace_tabs`, carrying the active tab and the last revision it applied (`baseSeq`).
  A report whose `baseSeq` is stale - a server mutation the UI has not applied yet - is REJECTED and answered with the authoritative snapshot to adopt, closing the lost-update race where a UI report clobbered a headless `move_tile`.
- Satellite (popped-out) windows neither apply organization forwards nor report: they hold a single tab, and reporting it would collapse the registry.

**Tab lifecycle policy (headless close).**
`close_terminal` kills the session AND drops its tile from the registry, pushing a `sync_tabs` snapshot so the tile leaves its (possibly hidden) tab immediately.
An auto-created tab (from `tabName` placement) that becomes empty is NOT reaped implicitly - an agent staging a workspace may empty and refill it - so closing is always an explicit `close_tab`.
`close_tab` refuses the last tab, and refuses a non-empty tab unless `force: true`; a force-closed tab's still-live sessions are re-adopted into the active tab by the UI reconciler, never orphaned.

The registry is still in-memory, per app run; the frontend persists layout for restarts and seeds the registry with its first report.
Full workspace-tab persistence remains the PRD §8 snapshot track and out of scope here.

### The authoritative captains registry (captain-chat phase 2)

Captain identity used to live in two disconnected places: the UI's localStorage designation and the captain's own ship files (`~/.t-hub/captain/ships/<ship>.md`).
Phase 2 moves the mapping into the SERVER - a **captains registry** (`control::CaptainsRegistry`) beside the tab registry, holding one `{shipSlug, captainSessionId, workspaceTabIds, crew}` record per claim plus a monotonic revision `seq`.
It is the ONE source of truth the UI and MCP both read; ship files remain the captain-side roster only.
The contract mirrors the tab registry, with one difference (it is **persistent**):

- **Overlay pinning is visual only.** A pin changes the local summon overlay without claiming authority, changing capability, or moving the terminal.
- **Commissioning and attachment create authority explicitly.** `commission_captain` starts a new control-capability harness, while `attach_captain` accepts only an existing harness that already holds the current control token. Both require a registered project and a successful protected Powder preflight.
- **Claims remain registry-first.** `claim_captain`, commissioning, attachment, and release mutate the registry before forwarding the authoritative claim snapshot under `args.sync` on a `sync_captains` `control://apply`. The UI keeps overlay MRU membership as independent view state.
- **`spawnedBy` links crew.** `spawn_terminal` and `create_worktree` accept a `spawnedBy` captain session id and record the spawned session under that captain's `crew` (an unclaimed `spawnedBy` never fails the spawn - `crewRecorded: false` tells the caller to `claim_captain` first). This is what makes the sidebar's crew counts and per-crewmate rows real rather than a count of Task-tool subagents.
- **Lifecycle cleanup is server-side.** `close_terminal` drops the dead session from the registry - a captain's death releases its claim, a crewmate's death leaves every crew list. `close_tab` prunes the closed tab from every captain's `workspaceTabIds` (the claim survives; a captain can control zero tabs). Each cleanup forwards a `sync_captains` snapshot.
- **Survives restarts.** Unlike tabs (which the frontend re-seeds on boot), the captains registry is written through to `~/.t-hub/captains.json` (override `T_HUB_CAPTAINS_FILE`; the dev build isolates it under `~/.t-hub-dev`) on every mutation, including the revision, and reloaded on launch. localStorage keeps only view state (overlay geometry, MRU order). A missing or corrupt file starts empty and heals on the first write.

At boot the UI fetches `list_captains` and adds live commissioned Captains to the overlay without turning unrelated local overlay pins into claims.

---

## 4. The theme-tool contract

`get_theme` and `set_theme` are **forwarded by name, verbatim** over the control
channel — `t-hub-mcp` and `control.rs` do not depend on the theme
implementation compiling. The contract for the parallel theme track:

- **Command names:** `get_theme` (no args) and `set_theme` (`{ "theme": "<id>" }`).
- The theme track adds the `get_theme` / `set_theme` Tauri commands and a
  `theme://changed` event, then wires control-channel handlers that call them.
- Until those handlers land, the control listener returns a clear, theme-specific
  error (`"… the theme command handler is not wired in this build yet …"`) for
  both commands — distinct from the generic "unknown command" path, so the
  forward seam is observable. The MCP tool surface already advertises both tools.

When the handlers land, no change is needed in `t-hub-mcp`: the names already
forward.

---

## 5. Registering with Claude Code

`.mcp.json` at the repo root registers the server:

```json
{
  "mcpServers": {
    "t-hub": {
      "command": "./apps/desktop/src-tauri/target/debug/t-hub-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

- Build the binary first: `cargo build -p t-hub-mcp --manifest-path apps/desktop/src-tauri/Cargo.toml`.
- For a **packaged install**, point `command` at the bundled `t-hub-mcp`
  (e.g. the Tauri sidecar binary). On Windows use the `.exe` path.
- The **T-Hub app must be running** for the tools to act on anything; the
  server starts fine regardless and reports a readable tool error when the app
  is down.
- To pin the control endpoint explicitly (skip the handshake file), set
  `env.T_HUB_CONTROL_ADDR` + `env.T_HUB_CONTROL_TOKEN`.

You can also register it imperatively with the Claude CLI:
```
claude mcp add t-hub -- ./apps/desktop/src-tauri/target/debug/t-hub-mcp
```

Quick offline sanity check (no app needed) — dump the catalog:
```
./apps/desktop/src-tauri/target/debug/t-hub-mcp --list-tools
```

---

## 6. End-to-end proof (dev box)

`scripts/mcp_proof.sh` produces the round-trip evidence two ways:

1. **Offline tool catalog** — runs `t-hub-mcp --list-tools` (no app needed),
   printing every tool + tier + `confirmationRequired`.
2. **Live round-trip** — runs the `mcp_e2e` integration test with `--nocapture`,
   which:
   - seeds a real `Supervisor` (an orchestrator with a running subagent, then a
     `Stop` → `waitingOnSubagents`) + a real `StatusBridge` (a statusline
     snapshot at 42% context),
   - starts a **real** `control.rs` listener on a loopback port,
   - creates a real tmux session on the `t-hub` socket,
   - spawns the **real** `t-hub-mcp` binary and drives it with genuine MCP
     JSON-RPC over stdio,
   - asserts the full round-trip for `initialize`, `tools/list`, and
     `tools/call` of `wsl_health`, `get_status`, `supervision_tree`,
     `search_files`, `list_terminals`, and the gated `spawn_terminal`.

```
scripts/mcp_proof.sh
```

### Sample transcript (real output, abbreviated)

`tools/call get_status` for the seeded session — note the derived
`waitingOnSubagents` status and the `contextUsedPct` from the statusline
snapshot, both fetched live through the control channel:

```
→ {"id":4,"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_status","arguments":{"sessionId":"sess-e2e"}}}
← {"jsonrpc":"2.0","id":4,"result":{"isError":false,"structuredContent":{
     "sessionId":"sess-e2e",
     "status":"waitingOnSubagents",
     "snapshot":{"sessionId":"sess-e2e","contextUsedPct":42.0,"rateLimitsPresent":false,"ingestedAtMs":1000}
   }, "content":[{"type":"text","text":"…"}]}}
```

`tools/call spawn_terminal` against the **headless** e2e listener (no UI wired) —
functional but refused because there's no frontend to adopt the tile (against the
running app it instead forwards the spawn and a real tile appears):

```
→ {"id":8,"jsonrpc":"2.0","method":"tools/call","params":{"name":"spawn_terminal","arguments":{"cwd":"/tmp"}}}
← {"jsonrpc":"2.0","id":8,"result":{"isError":true,"content":[{"type":"text",
     "text":"spawn_terminal: no UI is connected to adopt the new terminal tile …"}]}}
```

---

## 7. Tests

| Scope | Where | Count |
| --- | --- | --- |
| MCP protocol framing | `crates/t-hub-mcp/src/protocol.rs` | 4 |
| Tool catalog + tiers | `crates/t-hub-mcp/src/tools.rs` | 12 |
| Control client (forwarding, discovery, restart recovery) | `crates/t-hub-mcp/src/control_client.rs` | 12 |
| MCP server dispatch (initialize/list/call) | `crates/t-hub-mcp/src/server.rs` | 9 |
| App-side control dispatch + tiers (incl. spawn_terminal + tab registry) | `src/control.rs` | 122 |
| End-to-end (real binary ⇄ real listener) | `tests/mcp_e2e.rs` | 1 |

Run them:
```
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml -p t-hub-mcp          # MCP-side units
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml -p t-hub --lib control # app-side units
cargo build -p t-hub-mcp --manifest-path apps/desktop/src-tauri/Cargo.toml          # the binary the e2e spawns
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml -p t-hub --test mcp_e2e # end-to-end
```

---

## 8. Security notes (PRD §11.3)

- The control channel binds **loopback only** and gates every request on a
  **per-launch token** written to a `0600` handshake file. A bad token is
  rejected before dispatch.
- Read + Organization commands are dispatchable. The process-changing tier is
  **confirmation-gated** at the client (description + annotations) and executes on
  the app side: `spawn_terminal` forwards to the UI, which spawns a real, adopted
  tile (refused only when no UI is connected, so it never creates an untracked
  process); `send_text`, `send_keys`, and `close_terminal` act only on a `th_*`
  session the app already owns (typing into / interrupting / killing an existing
  session via tmux), never spawning an untracked process.
  Their MCP descriptions carry the `CONFIRMATION REQUIRED` contract a permission-
  aware client gates on. Any command with no `match` arm (a tool not in the
  catalog) is refused outright, independent of what an MCP client advertises.
- `search_files` returns **names + metadata only, never file contents**
  (PRD §11.1, FR-023). `open_file` reads through the same size-capped,
  binary-rejecting reader the UI uses.
- No secrets, `.env` values, or transcript content are exposed by any tool.
```
