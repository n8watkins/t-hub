# T-Hub — Server Split + Roadmap (Design)

**Status:** Design / proposal. No code yet. Captures (1) the full feature backlog, (2) how each feature interacts with a client/server split, (3) what the split does to our *existing* features, (4) the impact on client interaction, and (5) how a client connects to a `t-hub-server`. Grounded in the current code; file references are real.

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

**What the user notices locally: nothing.** Same app, same speed (loopback). The change is internal: the GUI calls the backend **over the socket** instead of in-process.

**What changes conceptually:**
- **`invoke(cmd)` → a request frame; `listen(channel)` → a server event stream.** The frontend already speaks two abstractions (`ipc/client.ts`, `client05.ts`); we add a transport shim so they target the channel. The `ApplySink`/`controlBridge.ts` pattern (today: organization mutations) is the prototype — generalize it to *all* commands + *all* events.
- **Remote adds latency** → design the UI **optimistically**: render the action immediately, reconcile on the server event. (Local is unaffected.)
- **Multi-client (A/B/C on one server)** — the big new behavior:
  - **Shared vs per-client state — a key design decision.** Proposed split: the **server owns the shared truth** (which sessions/agents exist, their status, scrollback, cost, supervision); each **client owns its own view** (tile layout, focus, theme, keymap, which tiles it's rendering). So your phone can arrange the same agents differently from your desktop. This keeps multi-client sane and makes per-client features (layout, keymap, theme) trivially independent.
  - **PTY fan-out:** multiple clients viewing the same tmux pane — tmux supports multiple attached clients, but **resize is shared** (smallest client wins, or we pick a "driving" client). Needs an explicit ownership/resize policy (the code already worries about "two tmux clients interleave," `workspace.ts`).
  - **Mutations broadcast:** a spawn/kill on one client must event-fan-out to the others so all views stay live.
- **Reconnect/offline:** client drops → server keeps running → client reconnects and **re-syncs from server state** (server is the source of truth). This is strictly better than today (where closing the app = closing the controller).

---

## 5. Feature feasibility matrix (the heart)

The organizing axis is **where a feature lives**, because that determines its relationship to the split:

- **Client-only** — lives in whatever client; the split doesn't touch it; each client can differ. Build anytime.
- **Server-side** — backend logic; building it **on the control channel now** makes it forward-compatible, and the split is what makes its *remote* version possible at all.
- **Hybrid** — computed server-side, surfaced client-side; needs a small multi-client rule.

### Core roadmap

| # | Feature | Lives | Effort | Interaction with the split |
|---|---|---|---|---|
| ① | **OS toast notifications** | Hybrid | Easy | Status is computed server-side; the **toast is raised client-side**. Multi-client rule: server emits the status-change event; each client decides whether to toast (respecting tab-aware suppression). Plugin install + the existing `notify.ts` lights it up today; the multi-client rule is a small addition. |
| ② | **Prefix keymap + action registry (+ command palette)** | **Client-only** | Med | **Unaffected by the split** — keymaps are per-client by nature (your phone vs desktop differ). The action registry is pure frontend (`Canvas.tsx` capture handler). Build now; it ports to any client for free. |
| ③ | **Git worktree primitive** | Server-side | Med-Hard | A backend op → becomes a **server command** (`git_worktree_add/remove/list`). Under the server-in-WSL model the git work is native Linux (simpler). Build it as a control-channel command now and it's server-ready; the `WorktreeCreate` journal enum is already reserved. |
| ④ | **`wait_for_status` (+ coordination bundle)** | Server-side | Med | **Best-case fit** — a synchronization primitive *should* live in the server and be queryable by any client/agent. Build on the `Supervisor` via the control channel. Bundle: `wait_for_output` (unwrapped match), `--no-focus` spawn flag, `recent-unwrapped` read mode, external custom-status injection, `agent.explain`. |
| ⑤ | **Native session restore** | Server-side | Med-Hard | Server owns persistence + restore. Add a `tile_sessions` table (`db.rs`), populate from the status bridge, relaunch via the existing `recall`/`--resume` path. In multi-client this is purely a server concern — clients just see restored tiles appear. |
| ⑥ | **The server split itself** | — | Hard (phased) | The keystone. Promote the control channel (PTY + events + network bind + auth + named sessions). Unblocks the *remote* version of every server-side feature above. |

### Polish / long-tail (grouped by where they live)

- **Client-only (build anytime, split-agnostic):** zoom (`prefix+z`) + indicator · session navigator (`prefix+g`, blocked-filter) · copy-mode + double-click token copy + edge-autoscroll · mouse arbitration (right-click passthrough modifier, drag-borders-over-mouse-apps — directly relevant to Claude/Codex mouse reporting) · drag-reorder tabs/workspaces + right-click tile menu · bare-URL Ctrl+click · custom-command keybindings with injected cwd · config niceties (`new_cwd=follow`, `redraw_on_focus_gained`, `confirm_close`, scrollback budget).
- **Server-side (build on the control channel → forward-compatible):** external custom-agent status injection (`report-agent --custom-status`) · file-index/recent/usage *remote-ization* (these are the overlay re-pointing work of ⑥) · operability (`--default-config` dump, warn-on-unknown-config-keys, **split client/server logs**, live config reload) · **named-session socket namespacing** (folds into ⑥'s connection model and fixes the dev/prod control-channel bug).
- **Hybrid:** notification quality (done-vs-needs-input sounds, per-agent mute, tab-aware suppression) — rides ①.

### What the split does to our **existing** features (do they survive?)

All survive; most are unaffected locally. The ones that need *re-pointing to be available remotely* are exactly the overlay data sources (usage, recent, file index, git, host metrics). Locally they keep working unchanged; remotely they light up only after their reads move server-side (Milestone 3). The terminal tiles work remotely as soon as the PTY stream is on the wire (Milestone 2).

---

## 6. Phased plan

| Milestone | What | Delivers | Effort |
|---|---|---|---|
| **M1 — Decouple locally** | Route a slice of GUI↔backend **through the socket** on one machine (no remote). Generalize `ApplySink`/`controlBridge` to carry commands + events. | Proof the wire works end-to-end; zero user-visible change; the foundation. | Med |
| **M2 — Tiles over the wire** | Put PTY attach/output/write/resize on the channel; bind to Tailscale; client points at a remote server. | **See + drive your remote tmux tiles from device B.** (Overlay still degraded.) The first real "wow." | Med |
| **M3 — Overlay server-side** | Move usage / supervision / recent / file-index / git reads to be served by the daemon (native Linux in WSL). | The **full cockpit** remotely. | Hard |
| **M4 — Multi-client + hardening** | Named-session namespacing; per-client view vs shared state; PTY resize-ownership; auth beyond the loopback token; split logs. | A/B/C all on one instance, safely. | Med-Hard |

**The forward-compatibility discipline (start this immediately, even before M1):** *new backend features get added as control-channel commands/events, not in-process-only Tauri calls.* That way ③④⑤ and the server-side polish are built **server-ready from day one**, and the split happens incrementally as we ship features rather than as one scary cutover.

---

## 7. Recommended sequencing

1. **Ship the client-only + easy wins now** — ① toasts, ② prefix keymap + action registry (unlocks a command palette), and the client-only polish (zoom, navigator, copy-mode, mouse arbitration). These are useful **locally today** and port to any future client for free.
2. **Build ③④⑤ as control-channel features** (not in-process) — worktree, `wait_for_status`, session restore. They deliver value now *and* are server-ready.
3. **Run the server split as a parallel track**: M1 (decouple) → M2 (remote tiles) → M3 (overlay) → M4 (multi-client). Each milestone stands alone; M2 already scratches "see my agents from another device."
4. **Meanwhile**, use **mosh + tmux + Tailscale** for raw remote access so you're never blocked while the GUI catches up.

**Strategic throughline:** every feature here leans on the three things a terminal-native tool (herdr) structurally can't match — **MCP, cost/context economics, and a real supervision tree.** The split makes those reachable from anywhere; it doesn't change what makes T-Hub *T-Hub*.

---

## 8. Risks / open decisions

- **Multi-client session ownership** — PTY resize policy when several clients view one pane (smallest-wins vs a designated driver). Real, needs a decision in M4.
- **Auth for network exposure** — loopback token → per-client keys / TLS / SSH-tunnel. Don't bind to a network interface until this is settled (today's loopback-only is a deliberate security boundary, PRD §11.3).
- **Shared vs per-client state** — proposed: server owns sessions/agents/status; client owns layout/focus/theme/keymap. Confirm this split early; it shapes M1.
- **Overlay re-pointing scope (M3)** — `files.rs`/`recent.rs`/`usage.rs`/`codex.rs`/`git.rs` all assume local WSL. Moving the server *into* WSL simplifies this, but it's the bulk of the effort.
- **Effort honesty** — ⑥ is weeks across sessions. M1–M2 get you remote *terminals*; M3 is where the real work is.

---

## 9. Open question for the owner

The single decision that most shapes priority: **is "rich cockpit, locally" the thing you love most, or is "reach my agents from anywhere" the top priority?**
- If the former → do §7 steps 1–2 (features now), treat the split as a slow background track.
- If the latter → pull M1–M2 forward and accept a degraded overlay on remote until M3.

Either way, the **forward-compatibility discipline (§6) costs nothing and keeps both doors open** — so we adopt it regardless.
