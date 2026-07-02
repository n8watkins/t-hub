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
| MCP server binary | `src-tauri/crates/t-hub-mcp/` | Speaks MCP JSON-RPC on stdio; owns the tool catalog; forwards `tools/call` to the app. |
| `protocol.rs` | `…/t-hub-mcp/src/protocol.rs` | Minimal JSON-RPC 2.0 framing (request/notification/response/error). |
| `tools.rs` | `…/t-hub-mcp/src/tools.rs` | The static tool catalog + tiers + JSON schemas. |
| `control_client.rs` | `…/t-hub-mcp/src/control_client.rs` | Discovers the app (handshake file / env) and forwards one command. |
| `server.rs` | `…/t-hub-mcp/src/server.rs` | The stdio loop: `initialize`, `tools/list`, `tools/call`, `ping`. |
| App control listener | `src-tauri/src/control.rs` | Loopback TCP listener; authenticates the token; dispatches by command name. |
| Registration line | `src-tauri/src/lib.rs` (`start_control_listener`) | Starts the listener in `setup()` with a supervisor-visitor closure. |

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
| `read_terminal` | Read | allowed | a session's recent visible output via tmux `capture-pane` (plain text; optional scrollback) |
| `focus_session` | Organization | allowed, **audited** | accepted + audited; UI application via the frontend command |
| `move_tile` | Organization | allowed, **audited** | accepted + audited |
| `rename_tab` | Organization | allowed, **audited** | accepted + audited |
| `new_tab` | Organization | allowed, **audited** | mints the tab id **core-side** and returns it (so the tab is addressable by `move_tile`/`focus_tab` and shows in `list_tabs`); forwards that id for the frontend to adopt |
| `focus_tab` | Organization | allowed, **audited** | accepted + audited; UI application via the frontend command |
| `open_file` | Organization | allowed, **audited** | capped text read via the Files reader |
| `create_worktree` | Organization | allowed, **audited** | runs `git worktree add` here, resolves the target tab **by name** (reuse/create by id, never the focused tab), then forwards the tab + spawn-in-worktree to the UI |
| `remove_worktree` | Organization | allowed, **audited** | forwards removal to the UI (which detaches live tiles first, then runs `git worktree remove`); refused if no UI is connected, to avoid orphaning a process |
| `spawn_terminal` | **Process-changing** | **confirmation required** | functional: routes through the same UI adoption path as `create_worktree` so the frontend spawns a real, tracked tile; refused only when no UI is connected to adopt it |
| `send_text` | **Process-changing** | **confirmation required** | types literal text into an existing session via tmux `send-keys -l` (optional trailing Enter); executes |
| `send_keys` | **Process-changing** | **confirmation required** | sends named control keys (e.g. `C-c`, `Up`, `Escape`) to an existing session via tmux `send-keys`; executes |
| `close_terminal` | **Process-changing** | **confirmation required** | kills an existing session + its process tree via tmux `kill-session`; executes |
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
- **Process-changing** tools all carry an explicit `CONFIRMATION REQUIRED` notice
  in their MCP `description`, an `annotations.confirmationRequired:true` boolean,
  and an `annotations.t-hubTier:"process-changing"` string so a permission-aware
  client can gate them — that confirmation contract is the user-facing gate. On
  the app side they split:
  - `spawn_terminal` is **functional** (#17): rather than refusing outright — the
    old behavior, because a raw spawn on the listener would create an untracked
    tmux session the UI never adopts — it routes through the SAME `ApplySink`
    adoption path `create_worktree` uses. The listener forwards a `spawn_terminal`
    command to the frontend, which spawns the tile via the normal `spawnTerminal`
    IPC, so the resulting session is a real, UI-adopted tile tracked like any
    other. It is refused only when no UI is connected (nothing would adopt the
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

### The addressable tab registry (#22)

For any headless tab operation to work — discover a tab id, `move_tile` a terminal
into it, `focus_tab` a known id, or place a `create_worktree` tile in a tab named by
the caller — tabs must be **addressable** over the control API.
The tab layout lives in the frontend store, so the core keeps a small in-memory
**tab registry** (`control::TabRegistry`, one `{id, name, tileIds}` record per tab)
that the control channel reads and writes:

- The frontend is the source of truth. It reports its FULL tab list up on every
  layout change via the `report_workspace_tabs` Tauri command (`controlBridge.ts`),
  which replaces the registry — so `list_tabs` mirrors the live UI, including
  UI-created tabs and real tile membership.
- MCP-driven mutations update the registry **optimistically** using ids the core
  chooses and forwards down: `new_tab` mints the tab id core-side (and returns it),
  `move_tile` moves the tile between records, and `create_worktree` resolves the
  target tab by name (reuse if it exists, else mint an id). Because the forwarded
  id is what the frontend adopts, the two converge on the same id when the frontend
  reports back.

This is deliberately the **minimal** registry that makes named placement +
addressable tabs work; full workspace-tab persistence is still the PRD §8 snapshot
track and out of scope here.

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
      "command": "./src-tauri/target/debug/t-hub-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

- Build the binary first: `cargo build -p t-hub-mcp --manifest-path src-tauri/Cargo.toml`.
- For a **packaged install**, point `command` at the bundled `t-hub-mcp`
  (e.g. the Tauri sidecar binary). On Windows use the `.exe` path.
- The **T-Hub app must be running** for the tools to act on anything; the
  server starts fine regardless and reports a readable tool error when the app
  is down.
- To pin the control endpoint explicitly (skip the handshake file), set
  `env.T_HUB_CONTROL_ADDR` + `env.T_HUB_CONTROL_TOKEN`.

You can also register it imperatively with the Claude CLI:
```
claude mcp add t-hub -- ./src-tauri/target/debug/t-hub-mcp
```

Quick offline sanity check (no app needed) — dump the catalog:
```
./src-tauri/target/debug/t-hub-mcp --list-tools
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
| Tool catalog + tiers | `crates/t-hub-mcp/src/tools.rs` | 8 |
| Control client (forwarding, discovery) | `crates/t-hub-mcp/src/control_client.rs` | 4 |
| MCP server dispatch (initialize/list/call) | `crates/t-hub-mcp/src/server.rs` | 9 |
| App-side control dispatch + tiers (incl. spawn_terminal + tab registry) | `src/control.rs` | 45 |
| End-to-end (real binary ⇄ real listener) | `tests/mcp_e2e.rs` | 1 |

Run them:
```
cargo test --manifest-path src-tauri/Cargo.toml -p t-hub-mcp          # MCP-side units
cargo test --manifest-path src-tauri/Cargo.toml -p t-hub --lib control # app-side units
cargo build -p t-hub-mcp --manifest-path src-tauri/Cargo.toml          # the binary the e2e spawns
cargo test --manifest-path src-tauri/Cargo.toml -p t-hub --test mcp_e2e # end-to-end
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
