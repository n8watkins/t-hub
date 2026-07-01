# Native Pivot - Zero-Context Parallel Execution Guide

> **Purpose:** run the remaining pivot tasks (T4-T14 + S16) with fresh, context-free workers, in parallel where the dependency graph allows.
> Each worker gets: §0 (preamble) + §1 (contracts) + its own §3 brief.
> Nothing else from any prior conversation is required.
> Strategy/decision background lives in [NATIVE-RENDER-PIVOT.md](./NATIVE-RENDER-PIVOT.md) (decisions D1-D7, results of T1-T3).

---

## 0. Global preamble (applies to EVERY task)

- **Repo:** `/home/natkins/projects/tools/t-hub/t-hub-app` (WSL). Main branch: `main`.
- **What T-Hub is:** a terminal-multiplexer/agent-cockpit. Today's client is Tauri+React+xterm.js. The pivot replaces it with a native GPUI client. The Rust backend ("server") already serves everything over a TCP control socket; **the native client is just a second client of that socket.**
- **Framework (decided, do not relitigate):** `gpui = "0.2.2"` from crates.io + `alacritty_terminal = "0.26"` + `vte 0.15`. A working example app using exactly these (16 live terminal grids at 90-180fps) is at `C:\Users\natha\spikes\gpui-spike\src\main.rs` - **read it before writing any gpui code.**
- **Branching:** one git worktree + branch per task: `git worktree add ../t-hub-<task> -b native/<task>`. Rebase on `main` before merge; merge in wave order (§2).
- **Commit style:** `feat(native): ...` / `fix(...): ...`, plain dashes (NEVER the em dash character), no co-author lines. Code commits touching `apps/desktop/` must run `apps/desktop/scripts/bump-version.sh` and sync `Cargo.lock` via `cargo check` (never hand-edit). **The new native crate does NOT bump the desktop version** - it is versioned independently.
- **Do-nots:** never restart/kill the user's running T-Hub app or tmux sessions unless the task explicitly says so (sessions survive app restarts, but the running app is the user's live workspace); never send input to tmux sessions you did not create (create disposable `t1-*`-style sessions for testing and kill them after); never touch `t-hub-dev`/`t-hub-localtest` tmux sockets.
- **Live-app facts for testing:** control handshake at `~/.t-hub/control.json` ({addr, token, pid, protocol_version:1}, loopback). tmux sessions on socket `tmux -L t-hub`. The persistent server key `/mnt/c/Users/natha/.t-hub/server-key` == the control token. Optional remote bind: launch app with `T_HUB_CONTROL_BIND=<tailnet-ip>:8790` (8787 is taken by workerd). Port/env gotchas: [T1-REMOTE-VERIFICATION.md](./T1-REMOTE-VERIFICATION.md) §8.
- **Windows builds** (only when a task needs to run the desktop app or native client visually): clone lives at `C:\Users\natha\Projects\Tools\t-hub\t-hub-app` (origin = the WSL repo; pull FROM WSL side with `git -C /mnt/c/... pull origin main`; it carries a local nsis/no-updater tweak in tauri.conf.json - stash/pop around pulls). Build: `powershell.exe -File 'C:\...\t-hub-build-<ver>.ps1'` pattern (see existing ps1 files there). The native crate builds on Windows the same way the spike did (~165s cold).
- **Verification bar:** every task ends with (a) compile clean, (b) its Acceptance demonstrated against the REAL running server (loopback is fine), (c) a short results note appended to this doc's §5 log.

## 1. Frozen contracts (parallel workers build against THESE, not each other)

### 1.1 Crate layout

New standalone crate at `apps/native/` (own `Cargo.toml` + `Cargo.lock`; NOT a cargo workspace with apps/desktop - isolate the pivot from the shipping app):

```
apps/native/src/
  main.rs      gpui App boot, root window, top-level layout   (T4)
  wire/        ControlClient + PtyHandle (protocol below)     (T4)
  term/        TermSession: alacritty Term + damage + snapshot(T5)
  render/      grid painting: runs, cursor, selection, fonts  (T5 base; T6/T7 extend)
  chrome/      workspace tabs, tile grid, per-tile headers    (T8)
  overlays/    sidebar: recents/usage/metrics/supervision     (T9)
  panels/      files / preview / dev-runner views             (T11)
  apply/       organization-mutation application (MCP path)   (T12)
```

### 1.2 Wire protocol (already shipped server-side; do not change except in T13)

- Transport: TCP, newline-delimited JSON. Request: `{"token":"<key>","command":"<name>","args":{...},"v":1}` -> one-line response `{"ok":true,"result":...}` or `{"ok":false,"error":"..."}`.
- Events: send command `__subscribe_events` on a dedicated connection; ack `{"ok":true,"result":{"subscribed":true,"protocolVersion":1}}`, then frames `{"event":"<channel>","payload":{...}}`.
  Channels observed live: `status://snapshot`, `session://status`, `supervision://tree`, `agent://journal`, `agent://state`.
- PTY plane: command `attach_pty` on a dedicated connection; opening frame `{"scrollback":"<b64>"}` (reflowed capture-pane snapshot - approximate; byte-authoritative only from attach forward), then `{"out":"<b64>"}` / `{"exit":code}` outbound; `{"write":"<b64>"}` and `{"resize":{"cols":C,"rows":R}}` inbound.
- **Executable reference clients: `scripts/probes/t1_*.py`** (connect/auth, commands, subscribe, attach/write/resize). These ran green against v0.3.28. Read them first; they are the protocol documentation.
- Server half lives in `apps/desktop/src-tauri/src/control.rs` (dispatch, EventFanout, `is_allowed_peer`) and `remote_pty.rs` (client half the webview uses today; its `reader_loop` ~line 342 is the seam T5 replicates natively).
- **Two id spaces (do not conflate):** tile/terminal ids are tmux-derived (`th_`-stripped 8-hex); supervision/status keys are Claude session UUIDs. `status://snapshot` payloads carry both (`sessionId` + `tmuxSession`) - chrome maintains the mapping.
- **Two observation planes (decision D5):** screen-derived signals (activity, localhost URLs, cursor) come from the client's own grid; semantic status (cost, context %, needs-input, supervision) ONLY from server events. Never reconstruct semantic state from pixels.

### 1.3 ControlClient interface (T4 delivers; T5/T8/T9/T11/T12 consume)

Pin these shapes; stub them if T4 has not merged yet:

```rust
pub struct Endpoint { pub addr: String, pub token: String }        // discover(): control.json, overridden by T_HUB_REMOTE_ADDR/T_HUB_REMOTE_TOKEN
pub struct ControlClient { /* req/resp conn pool + event conn */ }
impl ControlClient {
    pub fn discover() -> anyhow::Result<Endpoint>;
    pub fn connect(ep: Endpoint) -> anyhow::Result<Self>;
    pub fn request(&self, command: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value>;
    pub fn events(&self) -> crossbeam::channel::Receiver<Event>;    // Event { channel: String, payload: Value }
    pub fn attach_pty(&self, session: &str, cols: u16, rows: u16) -> anyhow::Result<PtyHandle>;
}
pub struct PtyHandle {
    pub scrollback: Vec<u8>,
    pub output: crossbeam::channel::Receiver<PtyFrame>,             // PtyFrame::Out(Vec<u8>) | PtyFrame::Exit(i32)
}
impl PtyHandle { pub fn write(&self, b: &[u8]); pub fn resize(&self, c: u16, r: u16); pub fn detach(self); }
```

(Exact error/async style may be adjusted by T4; consumers must only rely on the SEMANTICS above. Reconnect-with-backoff belongs inside ControlClient, invisible to consumers.)

### 1.4 TermSession interface (T5 delivers; T6/T7 extend)

```rust
pub struct TermSession { /* alacritty_terminal Term + vte Processor */ }
impl TermSession {
    pub fn new(cols: u16, rows: u16) -> Self;
    pub fn advance(&mut self, bytes: &[u8]);                        // feed PtyFrame::Out
    pub fn resize(&mut self, cols: u16, rows: u16);
    pub fn renderable(&self) -> Snapshot;                           // cells + cursor + selection, via Term::renderable_content()
    pub fn take_damage(&mut self) -> Damage;                        // Term::damage() + reset_damage()
}
```

Feed pattern (proven in the spike): `vte::ansi::Processor::advance(&mut term, bytes)`; render transform mirrors alacritty's `display/content.rs`.

### 1.5 Rendering facts (from the T2 spike - reuse, do not rediscover)

- One gpui window scene handles 16+ live grids; paint rows via `window.text_system().shape_line(...)` + `ShapedLine::paint`/`paint_background` inside a `canvas` element; drive with `request_animation_frame`.
- gpui's internal `LineLayoutCache` already dedupes repeat shaping - do not build a custom shaped-line cache.
- Monospace font: "Cascadia Mono" is present on the Windows box.
- Box-drawing/Powerline glyphs must eventually be procedural sprites, not font glyphs (T7).

## 2. Wave plan (what actually runs in parallel)

| Wave | Tasks | Parallel-safe because | Gate to start |
|---|---|---|---|
| **A** (now) | **T4** (scaffold+wire) · **S16** (wsl.exe sweep) · **T13a** (binary framing, server half) · **T7a** (font spike, standalone) | Disjoint codebases: apps/native (new) vs desktop misc files vs control/remote_pty vs the C:\ spike crate | none |
| **B** | **T5** (seam swap) · **T8** (chrome) · **T9** (overlays) | Different modules (term+render vs chrome vs overlays), all consuming §1.3 semantics; T8/T9 may stub ControlClient if starting before T4 merges | T4 merged (or stub against §1.3) |
| **C** | **T6** (terminal UX) · **T7b** (font integration) · **T10** (satellites) · **T11** (panels) · **T12** (MCP apply) · **T13b** (client adopts v2 framing) | T6/T7b extend term+render; T10/T11/T12 extend chrome; T13b touches wire only | parents merged (T5 for T6/T7b/T13b; T8 for T10/T11/T12) |
| **D** | **T14** (parity + cutover) | serial by nature | all of C |

Merge order within a wave: whoever is ready first; conflicts are structurally unlikely (module boundaries above).

## 3. Task briefs

### T4 - Scaffold the native client crate (Wave A, critical path)

- **Objective:** `apps/native/` boots a gpui window titled "T-Hub Native", connects to the running server via §1.3 `ControlClient` (discovery, auth, request, events, attach_pty), and proves it live.
- **Inputs:** §1.2/§1.3; `scripts/probes/t1_*.py` (port their logic to Rust); spike `main.rs` on C:\ for gpui boilerplate; `remote_pty.rs` for the attach framing the server expects.
- **Deliverables:** crate + `wire/` module implementing §1.3 exactly (adjusting style is allowed; document deviations in this file); a debug overlay or log listing sessions and streaming events; reconnect-with-backoff inside ControlClient.
- **Acceptance:** against the live app: lists the real sessions; receives live `status://snapshot` events; `attach_pty` to a DISPOSABLE tmux session streams the seed + output, write+resize round-trip (mirror `t1_pty.py`/`t1_resize.py`); survives an app restart via reconnect.
- **Verify:** `cargo check` in WSL (best effort - if the linux gpui backend needs system libs, document and rely on Windows); real run on the Windows clone.

### T5 - Swap the render seam (Wave B)

- **Objective:** N live grids in the native window: `PtyHandle` bytes -> `TermSession.advance` -> damage-driven paint. This is the pivot's core.
- **Inputs:** §1.4/§1.5; spike renderer; `alacritty_terminal` docs (`Term::renderable_content()`, `TermDamage`).
- **Deliverables:** `term/` + `render/` modules; a hardcoded 2x2-to-4x4 grid of real attached sessions (chrome comes in T8); per-frame budget instrumentation (reuse spike's fps logger).
- **Acceptance:** 12+ real tmux-backed tiles render, scroll, type, resize correctly in one native window; a busy Claude TUI session renders legibly at full speed; damage-clipped paint measurably beats full repaint (log both).
- **Verify:** side-by-side with a Tauri tile on the same session (create disposable sessions for input tests).

### T6 - Terminal UX completeness (Wave C, after T5)

- **Objective:** everything a webview tile does today, natively.
- **Scope:** full keyboard encoding (modifiers, app-cursor, bracketed paste, kitty protocol as needed), mouse reporting modes + arbitration, selection + clipboard, scrollback viewport, find-in-terminal, URL detection from the grid (client plane, D5), per-tile palettes.
- **Inputs:** today's behavior in `apps/desktop/src/components/Terminal.tsx` (keyboard handler ~lines 580-668) as the spec; alacritty's input encoding as reference impl.
- **Acceptance:** daily-drive parity checklist against a Tauri tile using real TUIs (Claude Code, Codex, vim, htop); each item demonstrated in a disposable session.

### T7 - Font subsystem (T7a spike: Wave A · T7b integration: Wave C)

- **Objective:** correct text at terminal fidelity: ligatures on a mono grid, color-emoji fallback, grapheme/combining marks, wide chars, box-drawing/Powerline as procedural sprites, sRGB blending.
- **T7a (standalone, now):** extend the C:\ spike crate with a torture-test screen (emoji, ligature fonts, CJK, combining marks, Powerline prompt, box-drawing TUI frames); catalogue what gpui 0.2.2 gets right/wrong; prototype procedural box-drawing sprites over `paint_quad`.
- **T7b (after T5):** port the working pieces into `render/`; wire per-tile font config.
- **Acceptance:** the torture-test script renders correctly in the native client; visual diff vs WezTerm/Windows Terminal on the same content noted in §5.

### T8 - Cockpit chrome (Wave B)

- **Objective:** the moat UI, native: workspaces as hideable tabs; tile grid (auto near-square + manual ratios + drag-reorder); per-tile headers (status ring, folder, git branch + worktree badge + dirty dot, client icon, context meter, editable work-name, fullscreen, close).
- **Inputs:** current behavior/specs in `apps/desktop/src/components/Tile.tsx` (header anatomy ~lines 549-901), `Canvas.tsx` (`splitRows()` layout math ~54-69), `store/workspace.ts` (tab/tile model); data via §1.3 (`git_info`, `status://snapshot`, `supervision://tree`; id mapping per §1.2).
- **Deliverables:** `chrome/` module; layout persisted (own SQLite or JSON file - decide and document; the SERVER keeps owning sessions, the CLIENT owns layout per D5/§8-of-pivot-doc).
- **Acceptance:** multi-workspace, mixed-tile daily layout fully operable: create/close/reorder/resize tiles, hide/show workspaces, headers live-update (status ring within 2s of a session status event; branch within one `git_info` poll).

### T9 - Sidebar overlays (Wave B)

- **Objective:** recents (+resume flow), Claude/Codex usage + cost, host/WSL metrics, supervision tree, toasts on status transitions with tab-aware suppression.
- **Inputs:** every data source is ALREADY a socket command/event (M3 shipped): `recent`/usage/`host_metrics`/`supervision_tree` commands + the §1.2 channels; current UI in `apps/desktop/src` sidebar components as visual spec.
- **Parallel note:** pure consumer of §1.3 - the most stub-friendly task; can develop against recorded event fixtures (capture with `t1_subscribe.py > fixtures.ndjson`).
- **Acceptance:** sidebar parity against the same running server; toast fires on a real session status transition.

### T10 - Multi-window satellites (Wave C, after T8)

- **Objective:** tear a workspace into its own OS window; per-window gpui surface/atlas; shared ControlClient; per-window layout state; clean close-back.
- **Watch:** atlas memory scaling with windows x visible cells - instrument and log in §5.
- **Acceptance:** tear off, render live terminals, interact, close back; no leaks after 10 cycles.

### T11 - Panels (Wave C, after T8)

- **Objective:** Files (tree + fuzzy search via `index_project`/`search_files` socket commands; arbitrary-path read/write stays M4-gated server-side - do NOT add it), Preview (localhost URLs from T6's grid scan), Dev runner.
- **Acceptance:** all three panel views work in a native tile against the live server.

### T12 - MCP organization continuity (Wave C, after T8)

- **Objective:** organization mutations arriving on the control socket (`move_tile`, `rename_tab`, `focus_session`, `focus_tab`, `new_tab`, `spawn_terminal`, `open_file`, ...) apply to the NATIVE client's workspace model.
- **Inputs:** today's webview path: `ApplySink` in `control.rs` -> `control://apply` -> `apps/desktop/src/ipc/controlBridge.ts` (`applyControl` switch) - replicate the switch in `apply/`; note `spawn_terminal` is gated off in some builds (probe first).
- **Acceptance:** from a Claude session with the t-hub MCP connected, each audited tool visibly manipulates the native client.

### T13 - Binary PTY framing, PROTOCOL_VERSION 2 (T13a server: Wave A · T13b client: Wave C)

- **Objective:** kill the ~33% base64+NDJSON tax on PTY frames: length-prefixed binary frames for out/write on attach connections; JSON stays for commands/events; version negotiated at attach so V1 (webview) clients keep working.
- **T13a (now):** server side in `control.rs`/`pty.rs` attach path + a python test harness proving V2 frames + V1 fallback (extend `scripts/probes/`); regression: the live webview client still works (V1).
- **T13b (after T5):** native `wire/` speaks V2; measure bandwidth delta on a firehose session, log in §5.
- **Acceptance:** V1 client unaffected (manual tile check); V2 harness green; measured reduction recorded.

### T14 - Parity audit + cutover (Wave D, serial, LAST)

- **Objective:** checklist vs the Tauri app (prefix keymap + action registry, command palette, session restore, worktree flow, notifications, theming); distribution/updater story for the native binary (Tauri updater/tray/installer are lost - pick replacements, e.g. cargo-dist or a custom updater); WSL-boundary checks; then flip the default, daily-drive a week, freeze the webview client (tag for revert).
- **Acceptance:** a week of daily-driving with zero fallbacks; webview path frozen.

### S16 - wsl.exe `--` sweep (Wave A, independent, desktop app)

- **Objective:** audit every remaining `wsl.exe` call site using bare `--` (which re-joins the tail through the user's default shell, zsh): `files.rs:361/1182`, `tmux.rs` (sites other than the already-fixed `pane_info_command`), `pty.rs:141`, `recent.rs:227/736/781`, `agent/mod.rs:97/817`, `devserver.rs:194`.
- **Rule:** args that can ever carry `$`/backticks/quotes/user data -> switch to `-e`; provably-constant argv -> leave with a one-line comment referencing the `tmux.rs pane_info_command` note. Reference fix: `git.rs` as of v0.3.28.
- **Acceptance:** each site dispositioned in the commit message; `cargo check` clean; version bumped; spot-verify one converted site E2E (mirror the reproduction in `docs/T1-REMOTE-VERIFICATION.md` style: old form vs new form output).

## 4. Integration and merge order

1. Wave A merges as ready (disjoint).
2. T4 merges -> unblocks real (non-stub) B work; B tasks rebase, replace stubs with `wire::ControlClient`.
3. T5 then T8 then T9 (or any order - modules are disjoint; first merged rebases the others).
4. Wave C as parents land.
5. T14 alone, on the assembled result.

If two tasks must touch the same file (expected only at `main.rs` layout composition), the LATER merger owns the conflict and keeps both features working.

## 5. Results log (append per task)

- 2026-07-01 T1/T2/T3: done pre-guide; see [T1-REMOTE-VERIFICATION.md](./T1-REMOTE-VERIFICATION.md) and [T2-GPUI-SPIKE-RESULTS.md](./T2-GPUI-SPIKE-RESULTS.md).
