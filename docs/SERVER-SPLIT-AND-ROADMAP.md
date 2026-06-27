# T-Hub — Server Split + Roadmap (Design)

> **STATUS (v0.2.0) — the roadmap features ①–⑤ are ALL SHIPPED. This doc is now ONLY about ⑥, the server split (M1→M4).**
>
> The five roadmap items this doc used to scope — ① OS toast notifications, ② prefix keymap + action registry + command palette, ③ git-worktree primitive, ④ `wait_for_status` + coordination/rules bundle, ⑤ native session restore — all landed in **v0.2.0**. Commits + acceptance are tracked in [ROADMAP-PLAN.md](./ROADMAP-PLAN.md) (Wave 0 + Wave 1, WS-1…WS-6). They are kept below only as ✅-marked context so the matrix and sequencing stay readable; **nothing in ①–⑤ remains to build.**
>
> **What's left is ⑥ — and the owner has signaled "remote matters."** So this doc's job changed: it is no longer a survey, it is the **cold-start build guide for the split**. A fresh agent should be able to read it top-to-bottom and start **Milestone 1** with zero prior context. The actionable centerpiece is **§6 (M1→M4)**; settle the **§8 pre-M1 decisions** first. Per §9's owner question, **M1–M2 are pulled forward** (reach-my-agents-from-anywhere is the priority); accept a degraded overlay on remote until M3.

**Status of the BUILD (updated — the split is largely shipped):** **M1, M2a, M2b-core, and all of M3 are SHIPPED** — every overlay/index read (recent · usage · codex · git · host_metrics · file index) plus tiles + events now cross the control socket, all on `main` as `feat(server-split): …` / `fix(server-split): …` commits, each verified live against the running app (socket probes) and reviewed by sub-agents. The design below is now RETROSPECTIVE for M1–M3 and forward-looking for M4. **What's left:** the file BROWSER/READER/EDITOR remoting (M4-gated — arbitrary-path read+write), M2b deferred hardening + a real two-device Tailscale test, then M4 (multi-client). See the §6 table for per-milestone status. The original "design proposal" text is kept for the rationale.

> The one-line bet: **pull T-Hub's "brain" out of the desktop GUI into a headless `t-hub-server` that lives where the agents live (WSL/remote), so any device can connect to one instance and get the full cockpit.** Today brain + face are fused in one Windows process hard-wired to the local WSL.

---

## 1. Where we are today (the monolith)

The Tauri app is **both** the client (xterm tiles, sidebar, React UI) **and** the controller (Rust backend: tmux, agents, supervision, journal, files), in **one process pinned to one Windows machine**. It reaches the agents by shelling into the local WSL via `wsl.exe -e bash`.

**Seams that already exist (this is why the split is an *extraction*, not a rewrite):**
- **The control channel is already a TCP server.** `control.rs` binds `127.0.0.1:0`, authenticates with a per-launch token, and dispatches `{command,args}` → `{ok,result}` as newline-delimited JSON. Discovery via `~/.t-hub/control.json` (addr+token), overridable by `T_HUB_CONTROL_ADDR`/`T_HUB_CONTROL_TOKEN`. It already serves the Read + Organization command surface (`list_terminals`, `get_status`, `supervision_tree`, `read_terminal`, `send_text`, …).
- **The backend is already abstracted away from Tauri** behind an `EventEmitter` trait (`agent/emit.rs`, wired in `lib.rs`) and `#[tauri::command]` wrappers. The stateful core (TerminalManager/`pty.rs`, `AgentBridge`, `Supervisor`, `StatusBridge`, `db.rs`) doesn't *intrinsically* need Tauri.
- **Two transports already cross a process boundary:** core↔agent (NDJSON over stdio across the `wsl.exe` hop; the single seam is `launch_argv` in `agent/mod.rs`) and MCP↔app (the control channel above).

**What's hard-wired to the local WSL (the real work of going remote):**
The terminal *tiles* are just PTY bytes — easy to move. But the **overlay** panels are computed by reading the agent machine's filesystem/state, all assuming "WSL is local":

| Panel | Fed by | Local-WSL assumption |
|---|---|---|
| Terminal tiles | PTY bytes (`tmux attach`) | `wsl.exe → tmux` (`tmux.rs`, `pty.rs`) |
| Usage (cost/context %) | reading Claude session `.jsonl` | `\\wsl.localhost` UNC / `wsl.exe` (`usage.rs`, `codex.rs`) |
| Supervision tree | the `t-hub-agent` journal | sidecar runs in WSL, streams over the agent hop (`agent/mod.rs`) |
| Recent sessions | scanning `~/.claude/projects/**` | UNC / native WSL `find` (`recent.rs`) |
| File index/search/reader | indexing repo files | `wsl.exe` / UNC (`files.rs`) |
| Git / worktree / host metrics | running `git` / reading `/proc` | `wsl.exe --cd` (`git.rs`) |

**Takeaway:** *displaying* the sessions remotely is cheap; *the overlay* is the work, because those data sources must be re-pointed from "local WSL" to "the server."

---

## 2. Target architecture

```
        Device A (or a remote box)                 any device(s): B, C, phone…
  ┌─ WSL ──────────────────────────────┐      ┌─ T-Hub GUI (thin client) ─┐
  │  t-hub-server (headless):          │◀────▶│  tiles · sidebar · cost ·  │
  │   tmux/PTY · AgentBridge ·         │  TS  │  supervision · files       │
  │   Supervisor · StatusBridge ·      │ /TCP │  (renders; holds little)   │
  │   journal · db · file index · git  │      └────────────────────────────┘
  └────────────────────────────────────┘            … and B, C, …
        owns ALL state + does the work          one server, many faces
```

- **`t-hub-server`** — a headless daemon that runs **inside WSL** (or any Linux box), owns all state, and does all the work. Crucially, running *inside* WSL turns today's `wsl.exe`/UNC gymnastics into **plain local Linux calls** — a large but *simplifying* change.
- **Thin client** — the Tauri GUI, computing its UI locally (fast, native) but getting all *data* from the server over the network. A client could even run on macOS/Linux (no WSL needed) since the WSL lives on the server.

---

## 3. How a client connects (the connection model)

We **promote the existing control channel** from "local MCP forwarder" to "the primary client↔server protocol." Concretely:

1. **Transport.** Keep newline-delimited JSON request/response, **add** two things it doesn't carry today: (a) a **PTY byte stream** (attach/output/write/resize frames) and (b) an **event subscription stream** (today `terminal://output`, `agent://journal`, `session://status`, `supervision://tree` go out via Tauri `emit` to the in-process GUI; they must instead fan out over the channel).
2. **Bind + reach.** Today it binds `127.0.0.1:0` (loopback only — deliberately, per PRD §11.3). For remote, bind to the **Tailscale interface** (or keep loopback + an SSH tunnel). Tailscale is the recommended path: stable private IP, no public exposure, WireGuard-encrypted.
3. **Auth.** Today: a per-launch shared token in `control.json`. For network exposure that's not enough — move to **per-client keys / a TLS or SSH-tunnel'd channel**. (Loopback token stays fine for same-host clients.)
4. **Discovery + named sessions.** Generalize `control.json` into **named-session namespacing** (herdr's `HERDR_SESSION` pattern): each server instance = a named session with its own socket/handshake (`~/.t-hub/sessions/<name>/control.json`). This *also* kills the "which T-Hub instance is the MCP talking to" class of bug we hit (dev vs prod control channel). A client selects a session by name/host.
5. **Local stays local.** When client and server are the same machine, it's the loopback socket exactly as today — zero UX/latency change. "Remote" is just the same wire bound to a reachable interface.

---

## 4. Impact on client interaction

**What the user notices locally: nothing.** Same app, same speed (loopback). The change is internal: the GUI calls the backend **over the socket** instead of in-process. *(This section is the conceptual model; the concrete build is §6 — M1 is exactly this change on one machine.)*

**What changes conceptually:**
- **`invoke(cmd)` → a request frame; `listen(channel)` → a server event stream.** The frontend already speaks two abstractions (`ipc/client.ts`, `client05.ts`); we add a transport shim so they target the channel. The `ApplySink`/`controlBridge.ts` pattern (today: organization mutations) is the prototype — generalize it to *all* commands + *all* events.
- **Remote adds latency** → design the UI **optimistically**: render the action immediately, reconcile on the server event. (Local is unaffected.)
- **Multi-client (A/B/C on one server)** — the big new behavior:
  - **Shared vs per-client state — a key design decision.** Proposed split: the **server owns the shared truth** (which sessions/agents exist, their status, scrollback, cost, supervision); each **client owns its own view** (tile layout, focus, theme, keymap, which tiles it's rendering). So your phone can arrange the same agents differently from your desktop. This keeps multi-client sane and makes per-client features (layout, keymap, theme) trivially independent.
  - **PTY fan-out:** multiple clients viewing the same tmux pane — tmux supports multiple attached clients, but **resize is shared** (smallest client wins, or we pick a "driving" client). Needs an explicit ownership/resize policy (the code already worries about "two tmux clients interleave," `workspace.ts`).
  - **Mutations broadcast:** a spawn/kill on one client must event-fan-out to the others so all views stay live.
- **Reconnect/offline:** client drops → server keeps running → client reconnects and **re-syncs from server state** (server is the source of truth). This is strictly better than today (where closing the app = closing the controller).

---

## 5. Feature feasibility matrix (placement context — all ✅ shipped)

The organizing axis is **where a feature lives**, because that determines its relationship to the split. The core roadmap below is now **retrospective** (all ✅) — it's kept because the *placement* of each shipped feature is exactly why the split is now an incremental extraction, not a rewrite:

- **Client-only** — lives in whatever client; the split doesn't touch it; each client can differ.
- **Server-side** — backend logic; we built it **on the control channel** (per the forward-compatibility discipline below), so it's already forward-compatible, and the split is what makes its *remote* version possible.
- **Hybrid** — computed server-side, surfaced client-side; needs a small multi-client rule (a M4 concern).

### Core roadmap — ✅ all shipped in v0.2.0 (kept for placement context)

| # | Feature | Lives | Status | Interaction with the split |
|---|---|---|---|---|
| ① | **OS toast notifications** | Hybrid | ✅ **shipped (v0.2.0, WS-2)** | Status is computed server-side; the **toast is raised client-side**. The eventual multi-client rule: server emits the status-change event; each client decides whether to toast (respecting tab-aware suppression). This becomes relevant again only at **M4** (multi-client); single-client behavior is done. |
| ② | **Prefix keymap + action registry (+ command palette)** | **Client-only** | ✅ **shipped (v0.2.0, WS-3)** | **Unaffected by the split** — keymaps are per-client by nature (your phone vs desktop differ). The action registry is pure frontend (`Canvas.tsx` capture handler). Already ports to any future client for free; the split never touches it. |
| ③ | **Git worktree primitive** | Server-side | ✅ **shipped (v0.2.0, WS-4)** | Built as control-channel commands (`create_worktree`/`remove_worktree` in `control.rs`, forwarded to the UI via `ApplySink`) — **so it's already server-ready.** Under the server-in-WSL model the git work becomes native Linux (simpler) with no rework. |
| ④ | **`wait_for_status` (+ coordination/rules bundle)** | Server-side | ✅ **shipped (v0.2.0, WS-5a/WS-5b)** | Built on the `Supervisor`, **already on the control channel** (`wait_for_status` dispatch in `control.rs`, edge-capturing via the transition log). A synchronization primitive that lives server-side and is queryable by any client/agent — exactly the shape the split wants. |
| ⑤ | **Native session restore** | Server-side | ✅ **shipped (v0.2.0, WS-6)** | Server owns persistence + restore: the `db.rs` tile→session catalog is populated from the status bridge and relaunched on boot. In multi-client this stays a pure server concern — clients just see restored tiles appear. |
| ⑥ | **The server split itself** | — | ◐ **REMAINING — this doc** | The keystone. Promote the control channel (PTY + events + network bind + auth + named sessions). Unblocks the *remote* version of every server-side feature above. Phased **M1→M4** in §6. |

### Polish / long-tail (grouped by where they live)

*These are optional nice-to-haves, **not** on the split's critical path. The server-side overlay-remoteization items below are exactly the M3 work (§6); the rest can land anytime, independent of M1→M4.*

- **Client-only (build anytime, split-agnostic):** zoom (`prefix+z`) + indicator · session navigator (`prefix+g`, blocked-filter) · copy-mode + double-click token copy + edge-autoscroll · mouse arbitration (right-click passthrough modifier, drag-borders-over-mouse-apps — directly relevant to Claude/Codex mouse reporting) · drag-reorder tabs/workspaces + right-click tile menu · bare-URL Ctrl+click · custom-command keybindings with injected cwd · config niceties (`new_cwd=follow`, `redraw_on_focus_gained`, `confirm_close`, scrollback budget).
- **Server-side (build on the control channel → forward-compatible):** external custom-agent status injection (`report-agent --custom-status`) · file-index/recent/usage *remote-ization* (these are the overlay re-pointing work of ⑥) · operability (`--default-config` dump, warn-on-unknown-config-keys, **split client/server logs**, live config reload) · **named-session socket namespacing** (folds into ⑥'s connection model and fixes the dev/prod control-channel bug).
- **Hybrid:** notification quality (done-vs-needs-input sounds, per-agent mute, tab-aware suppression) — rides ①.

### What the split does to our **existing** features (do they survive?)

All survive; most are unaffected locally. The ones that need *re-pointing to be available remotely* are exactly the overlay data sources (usage, recent, file index, git, host metrics). Locally they keep working unchanged; remotely they light up only after their reads move server-side (Milestone 3). The terminal tiles work remotely as soon as the PTY stream is on the wire (Milestone 2).

---

## 6. Phased plan — M1→M4 (the actionable centerpiece)

This is the build. Each milestone stands alone and ships value. **M1→M2 are the priority** (per §9). Read §8 first — the shared-vs-per-client split shapes M1, and you must NOT bind to a network interface (M2) until auth-beyond-loopback is settled.

| Milestone | What | Delivers | Status |
|---|---|---|---|
| **M1 — Decouple locally** | Route GUI↔backend traffic for **server-owned state** through the socket on one machine (no remote). Command request/response over the wire (`control_request` — now an **async** `#[tauri::command]` + `spawn_blocking`; as a SYNC command it ran the blocking round-trip on the main UI thread = the v0.3.17 freeze root cause); the event stream forwarded into the webview (`spawn_event_forwarder` → `control://event`). | Proof the wire works end-to-end; zero user-visible change; the foundation. | ✅ **SHIPPED** — `control_client.rs` (`SocketEmitter` is now the SOLE bridge-event sink; the transitional `TeeEmitter` dual-leg has been removed), `EventFanout` in `control.rs`, frontend `ipc/controlClient.ts`. Verified live via socket probes. |
| **M2 — Tiles over the wire** | **M2a:** server-owned PTY streaming — `tmux attach` on the socket (`attach_pty` → `{out}`/`{exit}` frames out, `{write}`/`{resize}` in), client-side `RemotePty`. **M2b:** persistent server key (`~/.t-hub/server-key`), opt-in Tailscale bind, `is_allowed_peer` gate (loopback + 100.64/10 + fd7a:115c::/32), thin-client mode (`T_HUB_REMOTE_ADDR`/`TOKEN`). | **See + drive your remote tmux tiles from device B.** (Overlay still degraded.) The first real "wow." | ✅ **core SHIPPED** — `remote_pty.rs`, `pty.rs` stream-attach, M2b bind+gate. **Deferred hardening:** per-client auth for `attach_pty`, server read/idle timeout, protocol versioning, reconnect re-sync; real two-device Tailscale test + `variant=dev` Windows build. |
| **M3 — Overlay server-side** | Re-point the independent overlay data sources to be **served by the daemon** via shape-identical control commands + sync cores (frontend `controlRequest` flip). | The **full cockpit** remotely. | ✅ **SHIPPED** — recent / claude-usage / codex-usage / git / `host_metrics` (bridge-first, Linux-only local-`/proc` fallback so Windows never zeros) / the file **index** (`index_project` + `search_files`) all over the socket. **Deferred (M4-gated):** the file BROWSER/READER/EDITOR (`list_dir`/`read_text_file`/`write_text_file`) — arbitrary-path read+write is a security-sensitive surface to expose over the network-bindable channel, so it waits for peer-gating/path-scoping. |
| **M4 — Multi-client + hardening** | Named-session namespacing; per-client view vs shared state; PTY resize-ownership; auth beyond the loopback token; split logs. | A/B/C all on one instance, safely. | ☐ **not started** — see §8 decisions (shared-vs-per-client split settled; no network bind until auth-beyond-loopback, satisfied by M2b's `is_allowed_peer`). |

### M1 — Decouple locally (START HERE)

**Goal:** the GUI talks to the backend **over the socket** on one machine, with zero user-visible change. No remote, no Tailscale, no new auth — pure plumbing. This is the keystone: once *all* commands and *all* events cross the socket cleanly on localhost, "remote" is just binding the same wire to a reachable interface (M2).

**The seams already exist — this is why M1 is a generalization, not a rewrite:**

- **The control channel is already a TCP NDJSON server with token auth.** `apps/desktop/src-tauri/src/control.rs` binds `127.0.0.1:0` (`control::start`, line ~209), generates a per-launch token, writes the handshake to `~/.t-hub/control.json`, and dispatches `{token,command,args}` → `{ok,result|error}` newline-delimited JSON (`dispatch_authenticated` → `dispatch`). It already serves the Read + Organization command surface (`list_terminals`, `get_status`, `wait_for_status`, `supervision_tree`, `read_terminal`, `send_text`, `create_worktree`, …). **You are widening this dispatch table to cover the rest of the command surface, not building a new transport.**
- **The backend is already abstracted behind a trait.** Backend→UI events go through the `EventEmitter` trait in `apps/desktop/src-tauri/src/agent/emit.rs` (channels `agent://journal`, `supervision://tree`, `session://status`, `agent://state`, `status://snapshot`, `agent://title`). Today the only impl is `TauriEmitter`, wired in `apps/desktop/src-tauri/src/lib.rs` (`state.agent.set_emitter(...)`, in `setup()`, line ~278). **This trait is the event seam — add a second emitter (or wrap the existing one) that also fans events out over the socket.**
- **The forward-over-the-socket prototype already runs in production.** `control.rs`'s `ApplySink` trait + `AppHandleApplySink` (in `lib.rs`, ~line 159) already forward accepted Organization mutations from the control channel to the UI: the backend emits a `control://apply` Tauri event carrying `{command,args}`, and `apps/desktop/src/ipc/controlBridge.ts` subscribes and dispatches it into the workspace store (`applyControl` switch: `move_tile`, `rename_tab`, `focus_session`, `add_worktree_workspace`, …). **`ApplySink` + `controlBridge` IS the shim you generalize** — today it carries a handful of org mutations one-way; M1 makes it carry the full command + event surface both ways.

**The exact first task (do this slice end-to-end before widening):**
1. Pick ONE command the GUI calls in-process today (a simple read like `get_status` or `list_terminals` is ideal — it's already in `control.rs`'s dispatch table, so the backend half is done).
2. Add a frontend transport shim behind the existing IPC abstractions (`apps/desktop/src/ipc/client.ts` / `client05.ts`) so that `invoke(cmd)` for that one command becomes a **request frame written to the control socket** (reuse the handshake-file discovery: `~/.t-hub/control.json` addr+token; honor `T_HUB_CONTROL_ADDR`/`T_HUB_CONTROL_TOKEN`), and the response is awaited off the socket — instead of a Tauri `invoke`. The `controlBridge` pattern is your reference for "the frontend already speaks to this channel."
3. Add a frontend listener that reads the **event stream** off the same socket and routes frames to the existing `listen(channel)` subscribers — i.e. generalize `controlBridge`'s one-way `control://apply` subscription into "any backend event arrives over the socket and is re-dispatched as if it were a Tauri event." On the backend, add the socket-fanout `EventEmitter` impl so `session://status` etc. also go out over the wire.
4. Round-trip that one command + see live events over the socket on localhost. Then **widen**: bring the remaining commands into `control.rs`'s dispatch and flip the rest of the frontend's `invoke`/`listen` calls onto the shim, command by command.

**Decide before you start (see §8):** the **shared-vs-per-client state split** — proposed: the **server owns** sessions/agents/status/scrollback/cost/supervision; the **client owns** layout/focus/theme/keymap. This determines which `invoke`/`listen` calls move onto the socket (server-owned state) versus stay purely in the client (layout/theme/keymap — they don't cross the wire at all). Confirm this first; it's the boundary the whole M1 refactor draws.

**Do NOT in M1:** bind to anything but loopback; add network auth; touch the overlay data sources (M3); worry about multiple clients (M4). Same-host loopback = today's security boundary (PRD §11.3) and today's zero-latency UX.

**Acceptance:** the GUI round-trips its commands **and** receives its live events over the control socket on localhost, with **zero user-visible change** (same app, same speed). The in-process Tauri `invoke`/`emit` path for the migrated surface is gone or bypassed; the socket is the path.

### M2 — Tiles over the wire

**Goal:** see + drive your remote tmux tiles from device B. The first real "wow."

**Build on M1's wire.** Two additions to the channel:
1. **A PTY byte stream.** Today terminal tiles are PTY bytes from `tmux attach` (`tmux.rs`/`pty.rs`) delivered in-process. Put **attach / output / write / resize** frames on the channel (a binary or framed-NDJSON sub-protocol alongside the request/response and event streams). The output frame fans tmux pane bytes to the client; the write frame carries keystrokes back; resize carries cols/rows.
2. **Network bind.** Change the bind from `127.0.0.1:0` to also reach the **Tailscale interface** (stable private WireGuard IP, no public exposure) — OR keep loopback and front it with an SSH tunnel. The client points at the remote server's session via the handshake (host+addr+token). **GATE: do not do this until §8's auth-beyond-loopback decision is settled** — the loopback-only bind is a deliberate security boundary; a network bind with only the per-launch shared token is not enough.

**Acceptance:** from device B, you can **see and drive** the remote machine's tmux tiles (type into them, they resize, output streams live). The overlay (cost, supervision, recent, files, git) is still degraded on remote — that's M3.

### M3 — Overlay server-side (the bulk — parallelizable)

**Goal:** the full cockpit remotely. This is the heavy milestone and the **embarrassingly-parallel bulk of the split**: there are **5 independent overlay data sources**, each hard-wired to "local WSL" via `wsl.exe`/UNC, and each must be re-pointed to "served by the daemon running natively in Linux." Running the server *inside* WSL turns today's `wsl.exe`/UNC gymnastics into plain local Linux calls — so this is large but *simplifying*. **Run it as 5 agents, one per source** (they touch disjoint files):

| # | Data source | File(s) | Today (local-WSL assumption) | After (served by the daemon) |
|---|---|---|---|---|
| 1 | **Usage** (cost / context %) | `usage.rs`, `codex.rs` | reads Claude session `.jsonl` via `\\wsl.localhost` UNC / `wsl.exe` | native filesystem read inside WSL |
| 2 | **Supervision tree** | `agent/mod.rs` | `t-hub-agent` sidecar runs in WSL, streams over the agent hop | the daemon *is* in WSL — no hop |
| 3 | **Recent sessions** | `recent.rs` | scans `~/.claude/projects/**` via UNC / native `find` | native `find`/walk |
| 4 | **File index / search / reader** | `files.rs` | indexes repo files via `wsl.exe` / UNC | native walk + index |
| 5 | **Git / worktree / host metrics** | `git.rs` | runs `git` / reads `/proc` via `wsl.exe --cd` | native `git` / `/proc` read |

Each agent re-points one source's reads from the WSL-hop path to a native-Linux path, exposes it as a control-channel command/event (so the thin client gets it over the M1/M2 wire), and confirms the overlay panel lights up remotely. **Locally, all five keep working unchanged throughout** — the re-pointing only adds the remote path.

**Acceptance:** the **full cockpit** works from device B — tiles (M2) *plus* live cost/context, supervision tree, recent sessions, file index, and git/host metrics, all served by the daemon.

### M4 — Multi-client + hardening

**Goal:** A/B/C all on one instance, safely. This is where the multi-client design decisions (§8) get paid off in code.

- **Named-session namespacing.** Generalize `control.json` discovery into per-session sockets/handshakes: `~/.t-hub/sessions/<name>/control.json` (herdr's `HERDR_SESSION` pattern). A client selects a server by name/host. This *also* kills the "which T-Hub instance is the MCP talking to" dev-vs-prod control-channel bug.
- **Per-client view vs shared state.** Implement the §8 split for real: server broadcasts shared truth (sessions/agents/status/scrollback) to all clients; each client holds its own layout/focus/theme/keymap. A spawn/kill on one client must **event-fan-out** to the others so all views stay live. Reconnect re-syncs from server state (server is the source of truth — strictly better than today, where closing the app closes the controller).
- **PTY resize ownership.** Multiple clients viewing one tmux pane: tmux supports multiple attached clients, but **resize is shared**. Pick a policy (§8): smallest-client-wins vs a designated "driving" client. The code already worries about "two tmux clients interleave" (`workspace.ts`).
- **Auth beyond loopback.** Move the per-launch shared token → per-client keys / a TLS or SSH-tunnel'd channel for any non-loopback bind. (Loopback token stays fine for same-host clients.) **This is the gate M2 depends on — if M2 shipped behind an SSH tunnel, M4 is where first-class auth lands.**
- **Split logs.** Separate client and server logs so a remote session's diagnostics are legible.

**Acceptance:** two+ clients drive one server instance concurrently; mutations fan out; reconnect re-syncs; resize has a defined owner; non-loopback binds are authenticated.

**The forward-compatibility discipline (already paid off):** *new backend features get added as control-channel commands/events, not in-process-only Tauri calls.* This is why ③④⑤ shipped **server-ready** (worktree/`wait_for_status`/restore all dispatch through `control.rs`). Keep applying it to any new backend feature — M1's generalization assumes the command surface keeps converging on the channel.

---

## 7. Recommended sequencing

1. ~~**Ship the client-only + easy wins now** — ① toasts, ② prefix keymap + action registry + command palette.~~ ✅ **Done (v0.2.0, WS-2/WS-3).** Useful locally today and they port to any future client for free.
2. ~~**Build ③④⑤ as control-channel features** — worktree, `wait_for_status`, session restore.~~ ✅ **Done (v0.2.0, WS-4/WS-5a/WS-5b/WS-6).** They deliver value now *and* shipped server-ready (all dispatch through `control.rs`).
3. **▶ NOW: run the server split** — **M1 (decouple) → M2 (remote tiles) → M3 (overlay) → M4 (multi-client).** Each milestone stands alone; M1→M2 are pulled forward (§9) because "see my agents from another device" is the priority. Start at **§6's M1 cold-start task**, after settling **§8**.
4. **Meanwhile**, use **mosh + tmux + Tailscale** for raw remote access so you're never blocked while the GUI catches up to M2.

**Strategic throughline:** every feature here leans on the three things a terminal-native tool (herdr) structurally can't match — **MCP, cost/context economics, and a real supervision tree.** The split makes those reachable from anywhere; it doesn't change what makes T-Hub *T-Hub*.

---

## 8. Open decisions — settle these BEFORE you start

Three decisions gate the build. **The first one must be settled before M1**; the other two are gates on M2/M4 but should be acknowledged now so M1 doesn't paint into a corner.

**① Shared-vs-per-client state split — SETTLE BEFORE M1 (it draws M1's boundary).**
Proposed: the **server owns the shared truth** — which sessions/agents exist, their status, scrollback, cost, supervision. Each **client owns its own view** — tile layout, focus, theme, keymap, which tiles it's rendering. So your phone can arrange the same agents differently from your desktop, and per-client features (layout/keymap/theme) stay trivially independent. **Why this is a *pre-M1* decision, not an M4 one:** it determines, for every `invoke`/`listen` call, whether it moves onto the socket (server-owned state) or stays purely client-local (never crosses the wire). That's exactly the line M1's refactor draws. Confirm it before flipping the first command.

**② Auth beyond loopback — SETTLE BEFORE M2 (it gates the network bind).**
Today: a per-launch shared token in `control.json`, bound to `127.0.0.1` only — a deliberate security boundary (PRD §11.3). That token is fine for same-host loopback but **not enough for a network-reachable bind.** Options: per-client keys, a TLS-wrapped channel, or keep loopback + an SSH tunnel (Tailscale gives you the private network; the tunnel/keys give you auth). **Do NOT bind the socket to any non-loopback interface until this is decided.** M2 either ships behind an SSH tunnel (auth deferred to the tunnel) or waits for first-class auth — that's the decision. (First-class per-client auth otherwise lands in M4.)

**③ PTY resize ownership — SETTLE BY M4 (multi-client only).**
When several clients attach to one tmux pane, tmux shares the resize: the pane sizes to constraints across all attached clients. Pick a policy: **smallest-client-wins** (simple, every client sees a fully-visible pane) vs a **designated "driving" client** (one client owns the geometry, others letterbox/scroll). The code already worries about "two tmux clients interleave" (`workspace.ts`). Not needed for single-client M1–M3; decide it when M4 lands multi-client.

**Non-decision risks (just be honest about them):**
- **Overlay re-pointing scope (M3)** — `usage.rs`/`codex.rs`/`agent/mod.rs`/`recent.rs`/`files.rs`/`git.rs` all assume local WSL. Moving the server *into* WSL simplifies each (native Linux calls), but re-pointing all five is the bulk of the effort. Parallelizable (§6 M3 table).
- **Effort honesty** — ⑥ is weeks across sessions. M1–M2 get you remote *terminals*; M3 is where the real work is.

---

## 9. Owner priority — SETTLED: remote matters

The single decision that most shapes priority was: **"rich cockpit, locally" vs "reach my agents from anywhere."** With ①–⑤ shipped (the rich local cockpit is *built*), the owner has **chosen "reach my agents from anywhere."**

→ **Pull M1–M2 forward and accept a degraded overlay on remote until M3.** That is the active plan: §7 step 3 is now the head of the queue, and §6's M1 cold-start task is where to begin.

The forward-compatibility discipline (§6) already paid off — ③④⑤ shipped on the control channel, so the split is an extraction from here, not a rewrite.
