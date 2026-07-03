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
  - **T12 addition:** channel `control://apply` carries every ACCEPTED Organization forward as `{"command":..,"args":..}` (the socket twin of the webview's `control://apply` Tauri event), and the new command `report_workspace_tabs` (`args.tabs: [{id,name,tileIds}]`) lets a socket UI replace the core tab registry the way the webview's Tauri report does. Both additive; a v1 client sees no change.
- PTY plane (v1, default): command `attach_pty` on a dedicated connection; opening frame `{"scrollback":"<b64>"}` (reflowed capture-pane snapshot - approximate; byte-authoritative only from attach forward), then `{"out":"<b64>"}` / `{"exit":code}` outbound; `{"write":"<b64>"}` and `{"resize":{"cols":C,"rows":R}}` inbound.
  - **v1 pre-stream errors (shape changed in T13a):** an attach failure raised before the stream starts (missing/dead session, spawn failure) now arrives as a standard control response `{"ok":false,"error":"..."}` - the same envelope a bad token has always used - NOT a bare `{"error":"..."}` frame. Client authors: treat any opening line with `ok:false` or `error` as attach failure (both `remote_pty.rs` and the native `wire/` already do).
- PTY plane (v2 binary, T13 - opt-in): send `attach_pty` with arg `"binary": true`. The connection then speaks length-prefixed BINARY frames on the PTY plane (commands + events stay JSON). Negotiation is per-attach and additive: a client that omits `binary` gets v1 unchanged, so the webview is unaffected. The server advertises support via the handshake's `protocol_version` (now `2`); a request `"v"` at or below the server's version is accepted (only a higher, unknown-future version is rejected).
  - **Frame layout:** every frame is `[u8 type][u32 big-endian length][length payload bytes]`. Type tags (mirrored in `pty::binframe`): server->client `0x01` OUT (raw output bytes), `0x02` EXIT (payload = 4 BE bytes of an `i32` exit code, or EMPTY for unknown/signalled - the v1 `null`), `0x03` SCROLLBACK (opening seed, raw bytes), `0x04` ERROR (UTF-8 message, for a pre-stream failure); client->server `0x10` WRITE (raw stdin bytes), `0x11` RESIZE (payload = `[u16 BE cols][u16 BE rows]`). No base64, no JSON envelope on the out/write firehose. Inbound frame length is capped at 16 MiB (`pty::BIN_MAX_FRAME`); an unknown inbound type tag is skipped (forward-compat).
  - **Executable reference client + proof:** `scripts/probes/t13_binframe.py` (drives both v2 binary and v1 fallback against a headless `control_probe_server` example, on a disposable tmux session; measures the wire reduction). T13b: the native `wire/` now mirrors this framing (`apps/native/src/wire/mod.rs`; proven by the `binframe-probe` bin).
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
- 2026-07-01 T4 DONE (branch `t4-native-scaffold`): scaffolded `apps/native/` - a standalone lib+bin crate (`t-hub-native`) with `wire/` implementing the §1.3 ControlClient contract, a GPUI debug overlay (`app.rs`, feature `gui`), and a headless acceptance runner (`wire-probe` bin).
  - **Build:** `cargo check --no-default-features` (wire only) AND full `cargo check` (with gpui 0.2.2) both compile CLEAN in WSL - the linux gpui backend (wayland/x11/blade) built without extra system libs, so the GPUI app module is compiler-validated on WSL too, not only on the Windows clone. `cargo test --no-default-features`: 6/6 green.
  - **Acceptance (live, against the running app at 127.0.0.1:57129):** `wire-probe` printed `WIRE-PROBE-OK` - listed 12 real `th_*` sessions via `list_terminals`; received a live `status://snapshot` event; attached a DISPOSABLE `th_t4-wire-check`: seed scrollback carried the marker, `echo` write round-tripped, `resize(90x25)` moved the pane to `90x24`, the session survived detach, then was killed. Only ever touched its own disposable session.
  - **Reconnect-with-backoff:** implemented inside `ControlClient` on all three planes (request redial loop, events resubscribe loop, PTY auto-reattach). Proven deterministically by the unit test `request_redials_after_a_dropped_connection` (mock server drops the first connection; the client redials and succeeds). The full "survives an app restart" path is left to the captain / Windows GUI run, because restarting the user's live app is a §0 do-not.
  - **Deviations from the §1.3 contract (all invisible to consumers or additive):**
    1. `connect(ep)` seeds discovery, but every RECONNECT re-runs `Endpoint::discover()` (falling back to the seed) because the loopback port is ephemeral and changes on app restart - this is what makes reconnect actually reconnect after a restart.
    2. `PtyFrame::Exit(i32)` (contract has no `Option`): a null/absent wire exit code maps to `-1` (unknown/signalled).
    3. On PTY auto-reattach the fresh scrollback is re-emitted as one `PtyFrame::Out` so the consumer's grid re-syncs (stays within the `output` channel semantics).
    4. `write`/`resize`/`detach` keep the exact §1.3 signatures (return `()`; `detach(self)`); I/O errors are logged rather than returned, as the signatures have no `Result`.
    5. Additive, non-breaking helpers: `ControlClient::connect_discovered()`, `list_sessions() -> Vec<SessionInfo>`, and a `push_capped` ring-buffer helper.
    6. gpui is an OPTIONAL cargo feature (`gui`, default on) so `wire/` compiles and unit-tests without a graphics backend (`--no-default-features`); `main.rs` and the `wire-probe` bin work either way.
    7. `crossbeam = "0.5"` provides `crossbeam::channel` exactly as the contract spelled it (it re-exports `crossbeam-channel`).
  - **Version:** the native crate is versioned independently at `0.1.0` (§0/§1.1); the desktop `bump-version.sh` was intentionally NOT run - this commit does not touch `apps/desktop/`.
- 2026-07-01 T5 DONE (branch `t5-render-seam`): swapped the render seam - `PtyHandle` bytes -> `TermSession.advance` -> damage-driven GPU paint, N live grids in one GPUI window.
  - **`term/`** (gpui-free, unit-tested under `--no-default-features`): `TermSession` wraps `alacritty_terminal::Term` + `vte::ansi::Processor` per the frozen §1.4 interface (`new`/`advance`/`resize`/`renderable()->Snapshot`/`take_damage()->Damage`).
  `Snapshot` is plain data (chars + already-resolved RGB + flags); the cell->`TextRun` transform lives in `render/`, so `term/` stays graphics-free and testable in WSL.
  Color mapping (xterm-256 cube, truecolor, inverse) is lifted from the T2 spike.
  - **`render/`** (feature `gui`): `GridView` paints a near-square tile layout (`ceil(sqrt(n))`), sizing each tile's `cols x rows` to its pixel box so a window-resize reflows every terminal and resizes its PTY.
  Damage-clipped paint: `take_damage()` drives which rows get their run vector rebuilt; unchanged rows reuse the cache.
  No custom shaped-line cache - gpui's internal `LineLayoutCache` dedupes shaping (§1.5), confirmed by the spike's Run 4 and by these numbers (a full-repaint run is barely slower than damage on scene time precisely because shaping is already cached; the win shows up in rows-rebuilt).
  `THN_PAINT=full` rebuilds every row every frame for the A/B measurement; the fps logger (reused from the spike) writes fps + scene-ms + rebuilt-vs-total rows to `THN_LOG_DIR/render.log`, and `THN_BENCH_SECS` writes a `SUMMARY` then exits for automated capture.
  A one-line dim per-tile header (id + geometry) is a debug aid, explicitly NOT the T8 cockpit header.
  - **`render_support.rs`** (gpui-free): the pure logic split out of `render/` - key encoding and layout math - so it unit-tests in WSL (the gui-feature test binary can't LINK the gpui graphics backend on this box; `cargo check` still validates the gui code).
  - **Build:** `cargo check` (gui) AND `cargo check --no-default-features` both CLEAN in WSL; `cargo clippy --all-targets` clean on both feature sets; `cargo test --no-default-features` 20/20 green (7 term + 6 wire + 7 render_support).
  Windows release build (`cargo build --release`, MSVC): clean, 2m45s cold, 13 MB binary.
  - **Acceptance (real run, Windows clone, RX 7800 XT @ 180Hz, against the live app):** 12 DISPOSABLE `th_t5-*` sessions (4x `top`/htop busy TUIs, 4x colorized scrollers, 4x idle) attached and rendered in one native window as a 4x3 grid; screenshot in [`T5-render-grid.png`](./T5-render-grid.png) shows crisp monospace, bold runs, truecolor/indexed color, box-drawing and `top`'s colored status bars, all legible.
  Each tile fitted to `81x27` from the provisional `80x24` attach - the content reflowing to 81 cols proves `TermSession::resize` + `PtyHandle::resize` round-tripped to tmux end-to-end (the resize acceptance).
  - **Damage-clipped vs full repaint (LOGGED; the two numbers):** at 12 tiles both pin the 180Hz vsync cap, so the win is CPU scene-build, not fps.
    - `THN_PAINT=damage`: **fps_avg 179.6, scene_avg 1.44 ms, rebuild_pct 7.2 %** (~4,200 of ~58,300 rows/s rebuilt) - full log [`T5-bench-damage.txt`](./T5-bench-damage.txt).
    - `THN_PAINT=full`: **fps_avg 179.9, scene_avg 1.75 ms, rebuild_pct 100 %** (58,300 rows/s) - full log [`T5-bench-full.txt`](./T5-bench-full.txt).
    - Damage clipping does **~1/14th** the per-frame run-build work and cuts scene-build CPU ~18 % (1.44 vs 1.75 ms) while holding full fps; `work_ms_per_s` ~260 vs ~310.
    The modest scene-ms delta (vs the 14x rows delta) is the direct evidence for §1.5: gpui already caches the *shaping*, so damage only saves the cell->run transform - exactly why a bespoke shaped-line cache would be wasted.
  - **Typing:** the key encoder (`render_support::encode`: text via `key_char`, Enter/Backspace/Tab/Shift-Tab/arrows/Home/End/PgUp/PgDn/Ins/Del, Ctrl-letter, Alt-prefix, platform-chord passthrough) is unit-tested (7 cases); the byte path is `PtyHandle::write`, already proven to round-trip live in T4.
  Synthetic key injection into the GPUI window is not automatable headlessly, so live keystroke->echo is the captain's interactive check; the encode tests + T4's proven write path cover the seam.
  - **Deviations from / additions to §1.4 (all additive; the frozen `new`/`advance`/`resize`/`renderable`/`take_damage` signatures are unchanged):**
    1. `Snapshot` is gpui-free plain data (`rows_cells: Vec<Vec<SnapCell>>` + cursor + selection), materialized owned so the terminal lock drops before shaping; the cell->`TextRun` transform is `render/`'s job, keeping the seam one-way and `term/` testable without gpui.
    2. `take_damage() -> Damage` where `Damage` is `Full | Lines(Vec<usize>)` (viewport rows); an empty `Lines` means "nothing changed this interval".
    3. Additive `TermSession` methods for the T5 acceptance: `renderable_rows(&[usize]) -> PartialSnapshot` (damage-clipped rebuild - materializes only the requested rows), `scroll(i32)` / `scroll_to_bottom()` (scrollback viewport), and `cols()`/`rows()` accessors.
    4. Geometry is clamped to >= 1x1 so a mid-layout zero-dimension resize can't panic alacritty.
    5. `render_support.rs` is a new gpui-free module (not named in §1.1) holding key encoding + layout math so they unit-test on WSL; `render/` keeps only the gpui-dependent paint code.
  - **Version:** native crate stays at `0.1.0` (independent; `bump-version.sh` NOT run - `apps/desktop/` untouched).
- 2026-07-01 T13a DONE (branch `t13a-binframe`): server-half BINARY PTY framing, `PROTOCOL_VERSION` 1 -> 2.
  - **What shipped:** `attach_pty` now negotiates framing from arg `"binary": true`. v2 speaks length-prefixed binary frames on the PTY plane (layout documented in §1.2); v1 (base64-NDJSON) is byte-for-byte unchanged for any client that omits the flag. Both directions honor the negotiated framing (scrollback/out/exit/error down, write/resize up). The request-`v` gate was relaxed from `!= PROTOCOL_VERSION` to `> PROTOCOL_VERSION` so a v1 client (the live webview, which sends `v: PROTOCOL_VERSION` from its own build) keeps working against a v2 server. Touched: `control.rs` (attach path, gate, `PROTOCOL_VERSION`, `serve_pty_attach` split into `read_pty_input_v1`/`read_pty_input_v2` + `send_attach_error`) and `pty.rs` (`PtyFraming` enum, `binframe` tags, `write_bin_frame`, `stream_attach_to_sink`/`stream_reader_loop` take a `framing` arg). No native-client changes (that is T13b).
  - **Build/verify:** `cargo check` CLEAN; `cargo test --lib control::` 45/45 + `pty::` 5/5 green (incl. the updated version-gate test, which now also asserts a v1 client is accepted by a v2 server). Added a headless `examples/control_probe_server.rs` (reuses the public `control::ControlContext::with_shared_supervisor` + `*_for_test` constructors) so the probe runs against a REAL listener without touching the user's live app or `~/.t-hub/control.json`.
  - **Harness (`scripts/probes/t13_binframe.py`): green, `T13-BINFRAME-OK`.** On one disposable `th_t13-binframe` tmux session it proves: v2 opens a binary SCROLLBACK frame carrying the seed, a binary WRITE round-trips as binary OUT, a binary RESIZE moves the pane (100x29 -> 90x24); and v1 (regression) still opens `{"scrollback"}`, round-trips `{"write"}`->`{"out"}`, and resizes (100x29 -> 110x39).
  - **Measured reduction (representative `seq 1 50000` firehose):** for the SAME captured output frames (222 frames, 41,680 B of raw PTY payload), the v1 base64+NDJSON wire cost would be 58,390 B (a ~40% tax over the raw bytes) vs the v2 binary wire cost 42,790 B (just the 5-byte headers, ~2.7% over raw) - a **~26.7% wire reduction** (v2 is 73.3% of v1). The headline is stable at ~26-27% across runs (it varies slightly with tmux redraw coalescing). Interpretation: base64 alone inflates raw bytes 33% (4/3), so removing it recovers 25% of the transmitted bytes; dropping the per-frame JSON envelope adds the rest. The raw firehose payload itself is much smaller than 50000 lines because `tmux attach` transmits the rendered/ coalesced pane, not every scrolled line.
- 2026-07-01 T6 DONE (branch `t6-term-ux`): terminal UX completeness on the merged T5 seam - mouse, selection + clipboard, scrollback UX, find-in-scrollback, URL detection.
  Alacritty semantics throughout; zero `app.rs`/`wire/` changes (T8 boundary respected).
  - **`term/`** (gpui-free, unit-tested): `ModeInfo` snapshot of the arbitration mode bits (mouse click/drag/motion, SGR + UTF-8 mouse, alt screen, alternate scroll, app cursor, bracketed paste, focus events);
  selection driving alacritty's own `Selection` (`SelKind` Simple/Block/Semantic/Lines, viewport->grid via `viewport_to_point`, extraction via `selection_to_string`);
  scrollback state (`display_offset`/`history_size`/`scroll_page_up`/`scroll_page_down`/`scroll_to_top`/`scroll_to_line`);
  find over the whole buffer (alacritty `RegexSearch`/`RegexIter`: smart-case literal patterns, `find_next` with wraparound both directions, `visible_search_hits` viewport segments, `match_stats` ordinal/total capped at 999);
  `visible_urls` (viewport scan that joins WRAPLINE-wrapped rows into logical lines and reports wide-char-aware grid-column segments);
  plus the new pure module **`term/scan.rs`** (URL scanner: scheme allowlist, tail-punctuation trim, balanced-paren keep; `search_pattern` smart-case escaping).
  - **`render_support.rs`** (gpui-free, unit-tested): `key_action` routing (Ctrl+Shift+C/V/F copy/paste/find; Shift+PageUp/PageDown/Home/End scrollback paging, handed to the app instead when the alt screen is active - alacritty's `~Alt` gating);
  `encode` upgraded to full xterm coverage (SS3 arrows/Home/End in app-cursor mode, `CSI 1;m` modified cursor keys, tilde keys with modifiers, F1-F12, AltGr-types-text, ctrl+backspace);
  `encode_mouse` (X10, UTF-8 1005 with 2-byte coords, SGR 1006; press/release/motion/wheel; shift/alt/ctrl bits; legacy coord clamp at 223);
  `alt_scroll_bytes` (mode 1007 wheel->arrows), `encode_paste` (bracketed framing + embedded-`ESC[201~` injection guard + CRLF->CR normalization), `search_key` find-bar routing, `cell_from_pixel` hit-testing math.
  - **`render/`** (feature `gui`): click focuses the tile under the pointer (plus mode 1004 focus-in/out reports on focus change);
  selection by drag with double-click word / triple-click line / ctrl+click block / shift+click extend;
  mouse-reporting passthrough with Shift override, per-cell drag motion, buttonless hover motion (1003), and release tracking;
  wheel arbitration in priority order app-wheel-events -> alternate-scroll arrows -> viewport scroll, targeting the tile under the pointer with a fractional accumulator for pixel-delta touchpads;
  a scrolled-back badge (top-right `^ N lines`, click = snap to bottom);
  a per-tile find-bar overlay (all visible matches highlighted dim, the focused match strong, `ordinal/total` readout);
  URL underlines always on with Ctrl+click to open (rundll32 / open / xdg-open per platform);
  middle-click paste; typing and paste snap to the live bottom and clear the selection.
  - **Polish (T5 verifier note):** the unreachable else arm in `paint_grid` is gone - damage now folds into `Option<Vec<rows>>` where `None` == full damage, so `need_full` and the partial path are exhaustive with no dead branch.
  - **Build/verify:** `cargo check` AND `cargo clippy --all-targets` clean on BOTH feature sets; `cargo test --no-default-features` **54/54** green (T5 baseline was 20).
  New: 17 term tests (modes, selection kinds incl. scrollback-offset selection, scroll state, viewport pinning, search wrap/stats/smart-case/segments, URL columns incl. wide-char and wrapped), 8 scan tests, 9 new render_support tests (12 total there).
  - **Deviations / judgment calls:**
    1. The brief's "snap to bottom on new output" is implemented as alacritty semantics: output while scrolled back PINS the viewport to its content (the offset grows - unit-tested), while typing/paste snaps to the bottom.
    A literal output-snap would make scrollback unusable on any busy tile; the badge gives one-click return instead.
    2. URL open is Ctrl+click (consumed, never forwarded to the app); plain click stays selection.
    Ctrl+click on a non-URL cell starts block selection (alacritty parity).
    3. Kitty keyboard protocol deferred: `TermMode` already tracks the kitty flags but the encoder speaks classic xterm; none of the acceptance TUIs require it.
    4. Search is a smart-case literal (regex metachars escaped), not user-supplied regex.
    5. Per-tile palettes (named in the §3 scope line) are NOT here - palette/config plumbing belongs with T7b's per-tile font config; flagged to the captain.
  - **Needs a live Windows check:** real clipboard round-trip (copy/paste/middle-click), mouse-mode TUIs (vim/htop/Claude Code), URL open, wheel feel/multiplier.
  - **Version:** native crate stays `0.1.0` (T8 runs in parallel; not bumping avoids a pointless Cargo.toml conflict). `apps/desktop/` untouched, so `bump-version.sh` intentionally not run.
- 2026-07-01 T13b DONE (branch `t13b-client`): the native `wire/` ControlClient speaks v2 BINARY PTY framing, with automatic v1 fallback.
  - **What shipped (`apps/native/src/wire/mod.rs`):** `attach_pty` opts in to binary framing (`"binary": true`, `"v": 2`) and both directions ride the §1.2 binary frames (SCROLLBACK/OUT/EXIT/ERROR down; WRITE/RESIZE up, writes chunked at the 16 MiB cap).
  The §1.3 consumer API is UNCHANGED - `PtyFrame`/`PtyHandle` are byte-identical to callers; only the wire encoding underneath moved.
  Fallback: the handshake file's `protocol_version` is used as a hint (a server advertising `< 2` gets a straight v1 attach, no wasted probe); when the version is unknown (env-override discovery) the v2 attempt's opening bytes are classified - a binary tag is v2, a JSON `{"scrollback"}` line means a pre-versioning server that ignored the arg (carry on in v1 on the same stream), and a JSON `{"ok":false}` rejection (the old `v != 1` gate answers this way and keeps the connection open) triggers a same-connection v1 retry.
  Reconnects RENEGOTIATE per attach, so an app up/downgrade across a restart is absorbed silently.
  The request/subscribe planes still stamp `v: 1` - their semantics are unchanged since v1 and a v1 server rejects any other version, so this keeps every plane except the opt-in attach compatible in both directions.
  - **Additive (documented §1.3 deviations):** `ControlClient::attach_pty_v1` (forced v1, for the A/B measurement + live regression), `PtyHandle::framing()`, and `PtyHandle::wire_bytes_in()`/`payload_bytes_in()` counters (raw inbound socket bytes vs decoded payload; also useful for a debug overlay); `Endpoint` gained `pub protocol_version: Option<u32>` from the handshake file.
  - **Build/verify:** `cargo check` + `cargo clippy --all-targets` CLEAN on both feature sets (gui and `--no-default-features`); `cargo test --no-default-features` **29/29** (was 20; +9: binary-frame codec round-trips incl. split-read boundaries and the over-cap teardown, and three mock-server negotiation tests - v2 end-to-end byte-for-byte with counter checks, the `ok:false` downgrade, and the handshake-hint skip proving exactly ONE v1 request reaches an advertised-v1 server). Desktop crate (touched by the polish below): `cargo check` clean + `control::`/`pty::` lib tests green. `bump-version.sh` intentionally NOT run - captain reconciles at merge.
  - **E2E (headless `control_probe_server`, one disposable `th_t13b-native` session): `binframe-probe` (new bin) green, `T13B-BINFRAME-PROBE-OK`.** v2 negotiated against the advertised `protocol_version: 2`; the binary SCROLLBACK carried the seed marker; a binary WRITE round-tripped as binary OUT; a binary RESIZE moved the pane 100x29 -> 90x24. The forced-v1 attach against the same server then passed the full v1 regression (b64 scrollback, JSON write -> out, JSON resize 90x24 -> 110x39).
  - **Measured delta (`seq 1 50000` firehose through the real `PtyHandle`, both runs at the same 90x25 geometry):** v2: 252 frames, 52,986 B payload, **54,231 B wire (+2.3% tax)**; pricing v1 for those SAME frames (`{"out":"<b64>"}` = 11 B envelope + 4*ceil(L/3)): 74,736 B -> **27.4% wire reduction** (v2 is 72.6% of v1). Independent measured v1 baseline run: 228 frames, 48,558 B payload, 67,522 B wire (**+39.1% tax**); tax-normalized cross-check: 26.4% reduction. Both agree with T13a's server-side ~26-27%.
  - **Deviation note (fallback proof):** no pre-v2 server binary exists to run (the headless example itself shipped in T13a), so the downgrade is proven at the protocol level - a deterministic mock-server test rejects `v: 2` exactly the way the old gate did (error response, connection left open) and the client transparently completes a v1 attach on the same connection - plus the live forced-v1 run above exercising the whole v1 path against the real server.
  - **Polish riding along (both T13a-verifier LOWs):** `pty.rs write_bin_frame` now uses a checked `u32` length cast (an over-`u32::MAX` payload returns `InvalidInput` instead of silently truncating the header and desyncing the stream); §1.2 documents the v1 pre-stream `{"ok":false,"error"}` attach-error envelope (T13a shape change) - `remote_pty.rs` and the native wire were audited and already handle it, so no consumer change.
  - **Version:** native crate stays at `0.1.0` (independent versioning per §0).
- 2026-07-01 T7 DONE (branch `t7-fonts`): the font subsystem - correct text at terminal fidelity on the merged T5/T6 seam.
  Full findings in [T7-FONT-CATALOGUE.md](./T7-FONT-CATALOGUE.md); screenshots [T7-torture-native.png](./T7-torture-native.png) (native, Windows, Cascadia Mono tile next to Cascadia Code+ligatures tile) vs [T7-torture-windows-terminal.png](./T7-torture-windows-terminal.png) (same bytes in Windows Terminal).
  - **`font/` (new, gpui-free like `term/`/`render_support.rs` - same §1.1 deviation precedent):** `FontSpec` per-tile config (`family:size:lig|nolig`, `THN_FONT` default override); emoji/symbol fallback family selection; **row segmentation** (`segment_cells`) - plain-ASCII runs shape as one line (exact by construction, this is what lets Cascadia Code ligate), while wide/non-ASCII/mark-bearing cells become single-cell segments painted at their own `col * cell_w`, so a fallback glyph's advance error can never push neighbors off the T6 column grid.
  - **`font/sprites.rs` (frozen §1.5 decision):** box drawing U+2500-257F (light/heavy arm table with clean mixed-weight joins, dashed, double lines with correct junction geometry, rounded arcs, diagonals), blocks U+2580-259F (eighths/halves/quadrants, ░▒▓ as alpha), Powerline E0B0-E0B7 + E0A0 branch + E0A2 padlock - all plain rects mapped onto `paint_quad`, never font glyphs.
    Windows Terminal draws ♦ placeholders for E0A0/E0A2; the native client draws real sprites for them.
  - **`term/`:** `SnapCell` gains `width` (WIDE_CHAR) + `zw` (zerowidth combining chars - previously DROPPED, so marks never rendered); `CursorPos.width` makes the cursor cover both columns of a wide char.
  - **`render/`:** rows are painted as merged bg-span quads (grid-exact, replacing `TextRun.background_color`) + positioned segments + precomputed sprite quads (built at damage-rebuild time, not per frame); per-tile `Font` + measured `Metrics` (family/size per tile works today - the torture window runs Mono 13 beside Code 13); `TileSpec.pty`/`client` became `Option` so offline fixture tiles need no server; fixture tiles re-feed on resize instead of reflowing.
  - **Torture harness:** `font/torture.rs` (15-section deterministic ANSI fixture, unit-tested for width discipline) + new bin `font-torture` (`--emit` pipes the same bytes into any other terminal; `THN_TORTURE_FONTS` configures tiles). Added as its own bin per the T7 boundary (app.rs untouched except the TileSpec/GridView::new signature catch-up).
  - **Build/verify:** `cargo check` + `cargo clippy --all-targets` CLEAN on both feature sets; `cargo test --no-default-features` **92/92** (was 63; +29: 13 sprite geometry incl. full-range bounds fuzz, 9 FontSpec/segmentation, 4 fixture, 3 term wide/zw/cursor).
  - **Acceptance (Windows, real Cascadia Mono/Code + Segoe UI Emoji, vs Windows Terminal on the same monitor with the same `--emit` bytes):** all sprite classes render crisp and cell-exact (junction joins, mixed weights, doubles); `|你好世界|` fences align pixel-exact over `|abcdefgh|` (the wide-char acceptance); combining marks compose incl. zalgo/Thai; color emoji render; **Cascadia Code ligates `-> != ===` on the grid while the Mono tile keeps them plain - per-tile ligature config proven**; truecolor ramps and gamma pairs match WT.
    An earlier WSLg run caught the drift bug this task exists for: with the family missing, the platform substituted a proportional font and pure-ASCII rows left the grid while isolated cells stayed on it - root-caused, documented, and the segmentation contains it.
  - **Catalogued deviations vs WT (alacritty grid semantics, intended):** skin-tone modifiers and ZWJ families render as separate cells/emoji (WT grapheme-clusters); flags are RI letter pairs on both; bare `❤` colors like `❤️` (DirectWrite fallback ignores text-presentation default; WT honors it).
  - **Known gaps (small follow-ups):** SGR dim not in `SnapCell` (renders full-bright; WT dims); E0A1/E0B8+ not sprited; diagonal/arc sprites are square-stroked (slightly softer than WT's AA lines); underline under wide fallback glyphs follows glyph advance; a missing font family silently substitutes (a metrics sanity warning is a cheap future guard).
  - **Version:** native crate stays `0.1.0` (T8/T9 run in parallel; same reasoning as T6). `apps/desktop/` untouched; `bump-version.sh` not run.
- 2026-07-01 T9 DONE (branch `t9-overlays`): sidebar overlays, native - recents (+resume flow), Claude/Codex usage + cost, host/WSL metrics, supervision tree, toasts on status transitions.
  Zero `app.rs` changes (T8 boundary respected); everything lives in `apps/native/src/overlays/`.
  - **Layering (the brief's gpui-free rule):** one module per overlay - `recents.rs`, `usage.rs`, `metrics.rs`, `supervision.rs`, `toasts.rs` - each holding its wire-payload parsing (serde), a state struct, reducers, and a plain-data view-model, all unit-testable under `--no-default-features`.
  `mod.rs` composes them into `SidebarState` (+ the cross-id-space `SessionIndex`) with `fold_event` as the single event reducer; `feed.rs` is the I/O pump (`OverlayFeed`: event drain + command polls on background threads, with the pure `PollPlan` cadence scheduler); `view.rs` (feature `gui`) is the ONLY file touching gpui.
  - **Mount contract for T8 / captain (the exported element):**
    ```rust
    let feed = overlays::OverlayFeed::spawn(client.clone());          // Arc<ControlClient>, once per process
    let overlays = cx.new(|_| overlays::OverlaySidebar::new(feed.clone()));
    // in the sidebar shell, below the workspace list:
    div().flex_1().child(overlays.clone())
    // hooks:
    feed.set_active_sessions(ids);      // tab-aware toast suppression (accepts session UUIDs and/or th_* tmux names)
    let host_rx = feed.host_requests(); // HostRequest::ResumeSession{session_id,cwd} -> spawn a tile at cwd running `claude --resume <id>`
    ```
    One-step convenience: `OverlaySidebar::mount(client, cx) -> (Entity<OverlaySidebar>, OverlayFeed)`.
    The element fills whatever box the host gives it (webview sidebar is 180-360px; reads fine across that range).
  - **MERGE CAUTION (captain/T8):** `ControlClient::events()` clones one crossbeam mpmc receiver - cloned receivers COMPETE for frames, they do not fan out.
  `OverlayFeed` must stay the process's single `events()` drainer; if chrome needs live status/supervision too (headers, status rings), read it from the shared `SidebarState` (it already folds statuses, snapshots, trees, titles, and the uuid<->tmux index) or grow a broadcast in `wire/` first.
  - **Data sources:** commands `recent_sessions`, `archive_recent_project`, `claude_usage` (gated fallback), `codex_usage`, `host_metrics` (4s), `list_terminals` (10s, feeds the recents open-project filter), `supervision_session_ids` + `supervision_tree` (one seed pull); channels `status://snapshot`, `session://status`, `supervision://tree`, `agent://title`, `agent://state`.
  Claude usage is statusline-first (freshest non-null reading per window across sessions, the webview's `selectStatuslineUsage`); the expensive `claude_usage` command only runs while NO statusline reading exists (attempt gap 60s, success fresh 1h - webview cadences).
  Codex windows roll over locally past `resetsAt` (webview `advanceWindow`).
  - **Build/verify:** `cargo check` AND `cargo clippy --all-targets` clean on BOTH feature sets; `cargo test --no-default-features` **108/108** green (baseline after T6+T13b was 63; +45 overlay tests: wire-shape parsing fixtures for every payload, recents dedup/filter/hide/resume-gate, usage source-priority/rollover/thresholds, metrics thresholds/staleness, supervision fold/ordering/durations, toast mapping/dedup/warmup/suppression/queue-expiry, `fold_event` integration incl. malformed payloads, `PollPlan` cadences).
  - **Acceptance (live, against the running app at 127.0.0.1:57129):** headless `overlay-probe` printed `OVERLAY-PROBE-OK` - recents loaded with worktree hints and ONE project filtered as open (live filter working), statusline-derived usage meters (weekly 31% used / session 54% used with reset ETAs) plus Codex meters and summed cost, full WSL metrics (RAM/swap/load/procs/distro), 9 active supervision trees with child durations, and **a REAL toast fired on a live `failed` status transition during the 12s window** (the §3 toast acceptance).
  - **Visual check (WSLg, per the wslg-native-gui-verification recipe - no Windows build needed):** `overlay-window` (new gui bin, the brief's demo entry point) screenshots in [`T9-overlay-sidebar.png`](./T9-overlay-sidebar.png) and [`T9-overlay-toasts.png`](./T9-overlay-toasts.png).
  Verified interactively: section collapse/expand toggles, usage meters + provider labels + reset hints, WSL rows with the load row correctly amber (box was genuinely saturated: load 9.00 on 8 cores), and a recent-row click emitting `HostRequest::ResumeSession` with the real session id + cwd (logged by the demo shell).
  `THN_TOAST_DEMO=1` folds three synthetic transitions through the real path (post-warmup) to inspect the toast cards.
  - **Deviations / judgment calls:**
    1. Resume is a `HostRequest` to the embedding shell, NOT a server call: the socket's `spawn_terminal` only forwards to a connected UI apply-sink (today: the webview) and carries no command argument, so it cannot run `claude --resume`.
    The webview's own resume also spawns client-side.
    T8 wires `host_requests()` to its tile-spawn path; the 1.5s double-spawn gate lives in the feed.
    2. Epoch reset hints render as relative ETAs ("resets in 2h 15m") instead of the webview's absolute locale strings - the native crate carries no timezone database.
    The `claude_usage` fallback keeps the server's human reset text verbatim.
    3. The recents open-project filter is live-tmux ∩ snapshot-cwds (the webview filters on open TILES; the native workspace model is T8's).
    Same visible effect once sessions emit statuslines, plus it also hides detached-but-running sessions - safer, since resuming those would fork a live conversation.
    T8 can refine via the shared state if tile-accurate filtering is wanted.
    4. Toasts are VISUAL cards (queue cap 4, TTL 8s, click-to-dismiss) rather than the webview's chimes + OS notifications; sound/OS-notify parity belongs to T14's checklist.
    Same status->notification mapping and titles as `notify.ts`.
    5. Toast warmup re-arms on agent `handshaking`/`replaying` (webview arms once at mount): replay bursts after an agent reconnect seed dedup baselines silently instead of re-toasting.
    The feed's one-time supervision seed also baselines via `seed_status` (never toasts, even on a slow connect).
    6. Tab-aware suppression is the `set_active_sessions` hook (T8 owns tabs); it accepts both id spaces and the fold checks the uuid AND its tmux alias.
    7. The supervision section shows ALL orchestrators with subagent activity (live first, then most-recent), not the webview's one-session detail view (its sidebar variant was removed pre-pivot; a cockpit sidebar wants the fleet view).
    8. Recents hide is in-memory + the durable `archive_recent_project` (the webview also persists its hidden-set in localStorage; native persistence can join T8's layout store).
    9. Additive dep: `serde` (derive) for payload parsing - `serde_json` alone had no derive.
  - **Windows check remaining:** only the `font_family("Segoe UI")` branch (WSLg ran the Linux default font); captain can eyeball it during merge reconcile.
  - **Version:** native crate stays at `0.1.0` (T8 runs in parallel; same reasoning as T6). `apps/desktop/` untouched, so `bump-version.sh` intentionally not run.
- 2026-07-01 T8 DONE (branch `t8-chrome`): cockpit chrome - the native window is a cockpit, not a demo grid.
  A LEFT SIDEBAR lists the workspaces (switch by click, create via the "+ new workspace" row, rename via double- or right-click, close via the row's x, never below one workspace), the active workspace's tiles fill the rest of the window with the webview's REAL auto-grid semantics, and every tile carries the real cockpit header (liveness dot, title, session id, geometry, close x) replacing the T5 debug line.
  - **Course correction honored (from the general, mid-task):** workspace navigation was first built as a top tab strip, then refactored into the left sidebar - the webview's long-standing design.
  Only placement changed; the state machine carried over untouched.
  The sidebar reserves everything below the workspace section for the T9 overlay sections: `chrome::model::SidebarLayout::overlay_mount` is the mount rect (T8 paints a hairline separator and nothing else inside it) - captain should point the T9 crewmate at it.
  - **`chrome/` module (per §1.1):** `model.rs` (gpui-free: the whole chrome state machine - workspaces, active/focused, rename editing, hidden set, `split_rows` + `tile_boxes` + `sidebar_layout` layout math, `reconcile`), `persist.rs` (gpui-free: layout file), `view.rs` (feature `gui`: `CockpitView`, a thin painter/input adapter).
  `cargo test --no-default-features`: 34/34 green (14 new chrome tests).
  - **Auto-grid semantics (the real ones):** `split_rows` mirrors `Canvas.tsx` `splitRows()` exactly - `cols=ceil(sqrt(n))`, `rows=ceil(n/cols)`, extras to the EARLIER rows, every row fully packed so its tiles stretch the full width (5 -> [3,2]; no holes, unlike the T5 uniform grid).
  Manual ratios and drag-reorder are deferred (documented follow-up, not started).
  - **Layout persistence (decide-and-document):** a JSON file, `~/.t-hub/native-layout.json` (`THN_LAYOUT` overrides), written atomically (tmp+rename) on every mutation; shape `{version, tabs:[{name, tiles:[ids]}], active}`.
  SQLite buys nothing at this size.
  Focus and the hidden set are deliberately NOT persisted (a restart re-lists live sessions fresh).
  - **Persistent pool:** every placed tile in every workspace keeps its PTY attached; switching workspaces only changes what is painted, so a switch is instant (verified live: 15 sockets = 13 tiles + request + events, exactly).
  - **Live wiring:** the control socket has NO terminal-lifecycle channel (§1.2), so adds/removals ride a 2s `list_terminals` poll, short-circuited to ~250ms by any `status://snapshot` / `session://status` / `agent://state` / `agent://title` event hint.
  New sessions land in the ACTIVE workspace; dead sessions leave every workspace and the pool.
  - **Tile close = detach, NOT kill (deviation from webview parity):** the webview's close kills the tmux session after a busy-confirm dialog; the native close removes the tile and drops the attach, and the session survives server-side (verified).
  Kill-with-confirm needs the busy signal + dialog UX and belongs with the T24 supervision work; the hidden set keeps a closed tile out of the layout until it dies or the client restarts.
  - **render/ seam for T6:** the per-tile damage/rebuild/row-paint code moved VERBATIM out of `paint_grid`/`paint_tile` into `pub(crate) render::sync_and_paint_content(tile, content_box, ...)`; the T5 grid (`THN_GRID=1`, benches intact) and the chrome both paint tile content exclusively through it.
  T6 owns its internals; T8 only relocated them behind one seam (plus visibility: `Tile::new`, `Metrics`, `PaintMode`, `probe_metrics`, `record_frame`, `paint_tile_frame`).
  - **Polish (T5 verifier):** the bare `.unwrap()`s on `open_window` in app.rs (old lines 363/390) are gone; all three modes print a context line (`t-hub-native: failed to open the ... window: <err>`) and exit 1.
  - **Stretch (liveness cue):** each header's dot goes bright green when the tile produced output within 2s (stamped by the feeder thread), dim gray otherwise; full supervision UX explicitly not built.
  - **Acceptance (live, in WSL via WSLg against the running app at 127.0.0.1:57129):** the linux gpui backend LINKS on this box with a user-local `libxkbcommon-x11.so` symlink shim (`RUSTFLAGS=-L ~/.local/lib-shims`; only the -dev symlink is missing, no sudo needed) and runs under WSLg with `WAYLAND_DISPLAY` unset (gpui 0.2.2's Wayland client rejects WSLg's compositor: `UnsupportedVersion` panic; the X11 backend is clean).
  Screenshot: [`T8-cockpit-sidebar.png`](./T8-cockpit-sidebar.png).
  Verified end-to-end, driving real input via Windows-side `SetCursorPos`/`mouse_event`/`SendKeys` and reading screenshots: 12 real sessions populate on boot; a disposable `th_t8demo1` (marker loop) appeared as a tile in ~2s with a GREEN busy dot and streaming output; a second disposable `th_t8demo2` (idle shell) appeared and its dot stayed dim; clicking a tile focused it (accent title + border) and `echo T8-TYPE-TEST` typed into the native window round-tripped through the PTY plane (echo visible in the tile AND in `tmux capture-pane`); workspace create (+), switch (instant, pool), rename (double-click, backspaces + "ops" + Enter, persisted), and close (refuses the last) all verified against the layout JSON; tile close x removed the tile while the tmux session SURVIVED; killing `th_t8demo1` removed its tile within ~2s ("session t8demo1 gone" logged); the layout (workspaces + active) survived an app restart.
  Real crew sessions spawning mid-test appeared live in the active workspace as they started - the reconcile loop kept up with the churn unprompted.
  - **Boot bug caught by the E2E run (worth recording):** tiles restored from a PERSISTED layout never attached, because pool attachment keyed off reconcile's `added` list (empty when the layout already places every live session).
  Fix: each pass attaches every placed-but-unpooled tile instead.
  A fresh-boot-only test would never have seen it.
  - **Verify:** `cargo check`, `cargo clippy --all-targets`, and tests green on BOTH feature sets (gui and `--no-default-features`); clippy 0 warnings.
  A Windows-native run was not needed for acceptance (WSLg drove the real GUI against the live app), so the Windows build is left to the captain's discretion; the crate builds on Windows the same way T5's did.
  - **Version:** native crate stays `0.1.0` (independent); `apps/desktop/` untouched, so `bump-version.sh` intentionally NOT run.
- 2026-07-02 T8-INTEGRATION (branch `t8-chrome`, merge of main 0.3.33): the T8 cockpit rebased onto the merged T6/T7/T9/T13b world - one integrated cockpit.
  - **render/mod.rs conflict (T8 seam vs T6+T7 in-place evolution):** resolved by taking main's CURRENT internals wholesale and relocating them behind the T8 seam - seam shape won structurally, main's content won substantively.
  `sync_and_paint_content(tile, content_box, ...)` now carries the full T6/T7 pipeline (per-tile FontSpec metrics, fixture reflow, damage-clipped rebuild with bg-span/segment/sprite rows, URL scan, search state, selection/search/URL/cursor/badge/find-bar painting) and returns `(rebuilt, total, badge_rect)`.
  The T6 INPUT logic was likewise extracted into shared per-tile functions (`tile_key_input`, `tile_wheel_dispatch`, `tile_mouse_down_dispatch`, `tile_drag_motion`, `tile_report_release`, `tile_hover_motion`, `notify_focus`) with `GridView` and the chrome as thin adapters, so a tile behaves identically in grid mode and the cockpit (find bar, copy/paste, mouse reporting arbitration, selection kinds, URL ctrl+click).
  Grid mode (`THN_GRID=1`) still works through the seam; the only pixel deviation is the scrolled-back find bar insetting by TILE_PAD (content-relative instead of tile-relative).
  - **T9 mount:** `OverlaySidebar` is composed below the workspace section - the CockpitView is now a flex row (sidebar column: workspace-list canvas on top, `div().flex_1().child(overlays)` below = the model's `overlay_mount`; grid canvas right).
  Live: recents, supervision trees, usage, and WSL metrics all render under the workspace list (screenshot below); `set_active_sessions` is wired on workspace switch/reconcile for tab-aware toast suppression; `host_requests` (resume clicks) are logged - a real tile-spawn path needs `spawn_terminal` semantics the socket does not provide (T11/T12).
  - **Single events drainer:** the chrome no longer holds a `ControlClient::events()` receiver (receivers COMPETE for frames; the T9 `OverlayFeed` must be the sole drainer).
  The reconcile short-circuit now polls `SidebarState::events_folded` every 250ms - any folded event (status/session/title/state, exactly the old hint set) triggers an early re-list, with the 2s hard poll unchanged.
  No wire/ change was needed; no §1.3 deviation.
  - **FontSpec persistence (ride-along):** `Workspace.font: Option<FontSpec>` in the model, serialized as `{family,size,ligatures}` in the layout JSON (absent field = pre-T7 layout, tolerated), applied at attach (`THN_FONT`/default fallback); round-trip + backward-compat tested.
  No settings UI (per the brief) - edit the JSON.
  - **Header dots got semantic:** when the T9 SidebarState knows a tile's session (via the snapshot id index, new additive `SessionIndex::tmux_aliases()` accessor), the dot shows agent status (working/waiting = green, needs question/permission = amber); otherwise the T8 output-recency cue.
  - **Tests:** 151 green under `--no-default-features` = main's 137 + T8's 14 chrome tests, a strict superset (nothing lost); `cargo check` + `clippy --all-targets` clean (0 warnings) on both feature sets.
  - **Live acceptance (WSLg, single instance, against the running app):** sidebar shows workspaces AND the T9 sections below; 11 live tiles render through the seam; a disposable `th_t8int` proved T7 procedural box-drawing sprites (crisp connected lines) and T6 selection in the cockpit: shift+drag painted the translucent selection band and ctrl+shift+c copied the selected cells to the WINDOWS clipboard through WSLg's sync ("tive git:(").
  The shift was REQUIRED because tmux (mouse on) owns unmodified clicks via mouse reporting - i.e. the T6 arbitration behaves correctly inside the cockpit; unmodified clicks go to the app, shift overrides for native selection.
  Liveness dots, live add/remove reconcile, and layout persistence (now with the font field) all still work as in the first T8 pass.
  Screenshot: [`T8-cockpit-integrated.png`](./T8-cockpit-integrated.png).
  - **Debugging note for the record:** the "selection does not paint" hunt was two red herrings deep - stacked background app instances from earlier test runs (clicks landing on one window, logs read from another), then tmux's mouse mode routing unmodified clicks to the app - and zero code bugs; the probes confirmed the relocated pipeline end to end.
- 2026-07-02 T10 DONE (branch `t10-satellites`): multi-window satellites - tear a workspace out into its own OS window, interact, close it back, all over ONE shared ControlClient and ONE attach pool.
  Screenshot (main cockpit + live satellite side by side): [`T10-satellite-windows.png`](./T10-satellite-windows.png).
  - **UX:** each sidebar workspace row gained a tear-off zone left of the `x`: `»` tears the workspace into a satellite window, `«` (accent-colored while torn) brings it home.
  The OS close button on a satellite is the same close-back; clicking a torn-off row's body raises its window; the row's `x` closes workspace AND window.
  Satellite windows are the real cockpit tile experience (headers, T6 input, T7 fonts) minus the sidebar; titles are "T-Hub - <workspace>" and live-retitle on rename.
  - **`chrome/model.rs` (gpui-free):** `Workspace` gained `satellite: bool` + a stable runtime `wsid: u64` (tab indices shift; windows bind by wsid); `tear_off`/`close_back`/`tab_by_wsid`/`satellite_tabs`/`main_tiles`.
  Invariants (T12 note - these hold no matter who mutates the model): `active` never rests on a satellite tab while a main tab exists (`set_active` REFUSES satellite tabs - the view raises the OS window instead); the main window's `focused` is `None` when every workspace is torn off; `reconcile` lands new sessions in a main workspace, CREATING one when all are torn off.
  - **`chrome/windows.rs` (new, gpui-free):** `WindowRegistry` - per-satellite focused tile (each OS window routes its own keys) and live window bounds, plus a re-tear memo so tearing the same workspace twice reopens where the user left it. The brief's "window-registry testable without gpui" module.
  - **`chrome/persist.rs`:** `LayoutTab.satellite: {bounds: {x,y,w,h}}` - additive and tolerant BOTH ways (pre-T10 layouts load into `None`; serde skips unknown fields so an old binary reads a new layout; unit-tested including a future-field fixture). Bounds refresh in the registry every satellite paint; any layout save persists them; main-window close saves once more on the way out.
  - **`chrome/view.rs`:** every per-window bit of `CockpitState` is now keyed by `WinKey::{Main, Sat(wsid)}` - tile hit zones, drag/hover/wheel state, painted-cell counts - because windows repaint on INDEPENDENT schedules and zones written by one window's paint must never be hit-tested by another's click (the pre-T10 single `HitZones` would cross-wire exactly that way).
  One shared per-window tile input core (`tiles_mouse_down/up/move`, `tiles_key`, `tiles_scroll`) drives both `CockpitView` and the new `SatelliteView`; tile behavior is byte-identical across windows because both are thin adapters over the same T6 core.
  gpui gives each window its own renderer + sprite atlas over one shared GPU context (verified in gpui 0.2.2 source: `BladeRenderer`/`BladeAtlas` per window, `gpu::Context` per app).
  - **Sole-events-drainer rule intact:** satellites take an `OverlayFeed` handle and read `SidebarState` (`gather_statuses`) exactly like the main window; ZERO new `events()` receivers; `wire/` untouched.
  **Shared pool intact:** a torn-off tile keeps its ONE `PtyHandle`; tear-off/close-back only changes which window paints it (proven live: fd count and socket count did not move on either transition).
  - **`app.rs`:** persisted satellites reopen at boot at their saved bounds (`cockpit: ... 1 satellite(s)` logged); closing the MAIN window saves the layout and quits the whole app (satellite workspaces STAY satellites in the layout and restore next boot); `THN_SAT_CYCLE=N` harness drives N tear/close cycles through the EXACT click-path functions; `THN_MEM_LOG=1` adds a 5s winstat ticker.
  - **Build/verify:** `cargo check` + `cargo clippy --all-targets` CLEAN on both feature sets; `cargo test --no-default-features` **163/163** (was 151; +12: 8 model satellite tests, 3 registry, 1 persist round-trip/tolerance).
  - **Acceptance (live, WSLg, against the running app at 127.0.0.1:57129, disposable `th_t10a`/`th_t10b` in a pre-placed "t10" workspace):** `»` click tore the workspace into a real OS window ("T-Hub - t10") with both terminals live (marker loop streaming, prompt idle, real headers, 42x41 reflow); clicked the satellite's idle tile and typed - `echo T10-SAT-TYPE-TEST` round-tripped (tmux capture-pane proof); shift+drag selection painted and ctrl+shift+c landed "T10-SAT-MARKER 23:52" on the WINDOWS clipboard through WSLg; OS `x` on the satellite returned the workspace to the main window and activated it (layout JSON verified); re-tear reopened at the remembered bounds (memo); main-window `x` quit cleanly with the satellite persisted, and relaunch RESTORED it as a second window at boot.
  Main-window input after the per-window refactor is proven too (a stray keystroke burst - see incident - went through `tiles_key(Main)` into the main focused tile).
  - **WATCH item (atlas/memory vs windows x cells) - the numbers:** instrumentation = `winstat[event]: windows= visible_cells= rss_mb= fds=` on every lifecycle event (+5s ticks). gpui's atlas is per-window, so window count is the multiplier to watch; measured process-wide (no atlas-size API in gpui 0.2.2).
    10-cycle harness on the ACTIVE workspace (12 live tiles, ~10.4k cells painted in the satellite, dwell 1.5s): steady state (cycles 7-10, after boot attach ramp) **fds 53 -> 53 -> 53 -> 53** and **TCP connections to the server 16 -> 16 -> 16 -> 16** (14 attaches + request + events) - NO session/socket leak; **RSS at tear-off 235.0 / 235.6 / 236.6 / 236.7 MB** (~0.4 MB/cycle, matching the independently measured idle drift of ~0.07 MB/s from 14 LIVE sessions streaming scrollback into their TermSessions - not window-cycle growth); marginal cost of a second window at ~10.4k visible cells ~**2 MB RSS**; `T10-SAT-CYCLE-DONE` logged.
  - **Deviations / judgment calls:**
    1. Tear-off is a sidebar zone click, not a drag-out-of-window gesture (gpui has no cross-window drag surface; a drag gesture can layer on later without model changes).
    2. `close_back` ACTIVATES the returned workspace (you just pulled it home - show it).
    3. `record_frame` (T5 fps bench) stays main-window-only so bench semantics are unchanged; satellite paint cost shows in winstat instead.
    4. Attach-failure no longer evicts the tile from the layout (T8 behavior change): reconcile retries placed-but-unpooled tiles every pass anyway, and the old remove-then-readd path MIGRATED tiles into the active tab - observed live dissolving a satellite workspace during a burst of transient attach failures.
    5. Persisted bounds ride X11 logical coordinates under WSLg; the WM may remap them across monitors on restore (position restore is compositor-advisory; size restores faithfully).
    6. gpui's calloop logs one benign "event for non-existence source" WARN per satellite close (platform layer noticing the destroyed window's source; once in 10 cycles, no misbehavior).
  - **INCIDENT for the captain (server finding, outside the T10 boundary):** early driving mistakes stacked multiple test client instances, and their SIGTERM mid-attach left the LIVE server (v0.3.28-era binary, pid 19092) with **72 CLOSE_WAIT sockets** it never closes and a wedged NEW-attach path: every fresh `attach_pty` fails ("read scrollback frame" / connection reset; later, existence checks report live sessions as missing - `t1_pty.py` reproduces), while the webview's 12 EXISTING attaches keep streaming (user's cockpit unaffected; verified via `tmux list-clients` = exactly 12 healthy clients).
    Needs an app restart to clear, and a server-side fix: reap attach forwarders when a client dies abruptly, and keep the attach path serviceable under client churn - native/satellite clients connect and disconnect FAR more than the webview ever did. Recommend a follow-up task on the desktop crate.
    Also for the record: one SendKeys burst landed in a REAL session's idle zsh prompt (a swallowed focus click meant the workspace never switched under it) and executed a junk "command not found" line - no state change, nothing to clean, but it is a do-not violation and the procedure is now click -> VERIFY (screenshot/log) -> type.
  - **Version:** native crate stays `0.1.0` (independent per §0); `apps/desktop/` untouched, `bump-version.sh` intentionally NOT run.
- 2026-07-02 T12 DONE (branch `t12-mcp-apply`): MCP organization continuity against the native client - `move_tile`, `rename_tab`, `focus_session`, `focus_tab`, `new_tab`, `spawn_terminal`, `open_file`, and `create_worktree` tab placement now manipulate the native cockpit's ChromeModel, exactly as they manipulate the webview.
  Screenshot: [`T12-native-apply.png`](./T12-native-apply.png) (sidebar showing the MCP-created/renamed/worktree workspaces, the apply-focused one active, and a socket-spawned tile placed in it).
  - **Delivery path (the architecture gap this task existed for):** the server's ApplySink only reaches the Tauri webview, so `control.rs` now ALSO broadcasts every accepted Organization forward to event subscribers on a new `control://apply` event channel (§1.2 addition) - `forward_apply` emits sink-first-then-broadcast, `EventFanout::emit_event` returns the delivery count, and `applied` keeps its exact old meaning whenever a sink is wired (webview path byte-identical, proven by the sink-parity unit test); with NO sink, reaching >=1 subscriber counts as delivery.
  The app's own `control://event` forwarder re-emits the new channel under an envelope nothing routes into `applyControl`, so the webview can never double-apply (verified in `controlClient.ts`/`controlBridge.ts`).
  - **Native side, riding the single-drainer architecture:** the T9 `OverlayFeed` event thread (still the process's ONLY `events()` receiver) decodes `control://apply` frames through the new gpui-free `apply/` module onto a `apply_requests()` crossbeam channel; the cockpit worker drains it at the top of every pass (apply frames bump `events_folded`, so the existing hint tick delivers ~250ms steady-state latency) and runs the model mutations plus their side effects (persist, pool detach, mode-1004 focus reports, toast-suppression sync).
  `apply::parse_event`/`apply_model` replicate the `controlBridge.ts` switch arm for arm, including the alias-key tolerance and the total, never-throwing semantics.
  - **Tabs became MCP-addressable:** `Workspace` gained a stable uuid `id` (persisted; a pre-T12 layout mints ids on load), with id-addressed accessors mirroring the webview store (`adopt_tab`, `rename_tab_by_id`, `set_active_by_id`, `move_tile_to_tab` append-without-activate, `reorder_tile` active-tab splice).
  `new_tab` adopts the CORE-minted id verbatim, so the id the MCP caller holds addresses the live native tab.
  - **The registry mirror got its socket write half:** new control command `report_workspace_tabs` (the Tauri report's socket twin) + a native reporter thread fed by every `save_layout` (coalesced, deduped), so `list_tabs` stays truthful whichever client is attached - the acceptance proves it via `rename_tab`, which the server never applies to its own registry (only the native report can make `list_tabs` show the new name, and it does).
  Consistency stays last-writer-wins exactly like the webview's report; if BOTH clients are attached to one server they fight over the mirror (parity-period caveat; T14's cutover leaves one reporter).
  - **spawn_terminal / create_worktree, native path:** the webview "client-side spawn" is really a Tauri IPC into the SAME server process, so with no ApplySink but event subscribers present the server now spawns the tmux session itself (identical id minting to `commands::spawn_terminal`; a `shell` preset stays the pane program verbatim) and carries the minted id in the broadcast - the native apply places the tile (`spawn_terminal`: active tab + focus; `create_worktree`: directly into the adopted named tab).
  With a sink wired the webview keeps owning the spawn (broadcast carries no id; the native adopts via reconcile - for worktrees through a pending cwd-matched placement `reconcile_with_cwds` consumes, segment-boundary semantics lifted from the webview).
  With neither sink nor subscribers, the old refusals/headless behavior are untouched.
  - **Probed first per the brief:** the RUNNING app (a stale pre-#17/pre-T13a binary, handshake `protocol_version: 1`) still answers `spawn_terminal` with the old "gated off in this build" refusal, and it has no apply broadcast at all - so live acceptance ran against a patched headless `control_probe_server` (T13a's harness) with the native cockpit attached under WSLg, driven by the new raw-socket probe `scripts/probes/t12_apply.py`.
  - **Acceptance (live, `T12-APPLY-OK`):** on one run against the real tmux fleet: native boot report reached `list_tabs`; `new_tab` -> tab adopted with the core id (layout JSON + registry); `rename_tab` -> visible + registry round-trip; `spawn_terminal` -> server-minted session placed + focused in the active tab; `move_tile` -> relocated to Workspace 1; `focus_tab`/`focus_session` -> active workspace flips (the latter by tile id via owning-tab activation); `create_worktree` -> named tab opened and the worktree terminal placed IN it with cwd verified `== worktreePath`; `open_file` -> contents served, native layout byte-identical; `remove_worktree` -> documented sink-less refusal; a monitor subscription logged every broadcast; disposable sessions/repos cleaned up (tmux back to baseline).
  - **Verify:** native `cargo check` + `clippy --all-targets` clean on BOTH feature sets, `cargo test --no-default-features` **166/166** (was 151; +8 model incl. pending-placement routing/expiry, +7 apply switch); desktop `cargo check` clean + `control::` **48/48** (+3: broadcast-vs-sink parity, sink-less subscriber delivery, registry replace) + `pty::` 5/5 (desktop clippy count unchanged - the 80 pre-existing doc/MSRV lints are all in untouched code).
  - **Deviations / judgment calls:**
    1. `open_file` intentionally has NO native arm - webview parity: `controlBridge.ts` has no arm either; the server answers the MCP caller with the file contents directly.
    2. `remove_worktree` with no sink still refuses: a socket client cannot own the detach-then-`git worktree remove` ordering; when the webview drives it, the native now hears the broadcast and detaches its own tiles rooted in the dir (cwd segment-boundary match). Full native-only removal belongs to the T14 cutover.
    3. `focus_session` accepts Claude session UUIDs natively (parity-plus): an unmatched id retries through the T9 `SessionIndex` alias before giving up - the webview silently no-ops on UUIDs.
    4. Apply frames raised while the native's event stream is reconnecting are lost (organization commands are point-in-time, the server cannot know a UI was away) - same exposure the webview has when it is not running.
    5. Boot behavior worth knowing: the worker's first reconcile pass attaches every placed tile SEQUENTIALLY, so the FIRST applies after a cold boot can trail by ~15s on a 14-tile fleet; steady state is one hint tick. Parallelizing the attach loop (or draining applies between attaches) is a cheap follow-up but lives in app.rs (T10's file).
    6. Ride-along parity fix: native `add_tab` now names "Workspace N" at the LOWEST FREE index (webview `addTab` / core `auto_name` scheme) instead of len+1.
    7. For-the-record test note: one acceptance run died mid-test with zero code involvement - the WSLg/msrdc window closed and took the client with it (exactly the known gotcha in the crew memory; the monitor subscription proved the server had broadcast correctly, and the rerun passed end to end). Verify the native pid is alive before blaming the apply path.
  - **Version:** native crate stays `0.1.0`; `apps/desktop/` IS touched (control.rs) but `bump-version.sh` was NOT run per the task brief - captain reconciles at merge.
- 2026-07-02 T12-INTEGRATION (branch `t12-mcp-apply`, merge of main with T10 satellites): the two `Workspace` extensions fused - one struct now carries T12's persisted uuid `id` AND T10's runtime `wsid`/`satellite`.
  - **Fusion rules (where the tasks touched the same behavior):** apply-driven mutations respect the T10 satellite invariants.
  `set_active_by_id`/`adopt_tab` refuse to activate a torn-off tab (a `focus_tab` apply on it is a main-grid no-op; the workspace already shows in its own OS window).
  `place_tile` and the `reconcile_with_cwds` no-intent fallback land new sessions in a MAIN workspace, creating one when everything is torn off (T10's arrival rule), while a pending worktree placement still routes into ITS named tab even torn off (that window shows the arrival).
  `reorder_tile` never writes main-window focus while the active tab is a satellite.
  - **Merge fix worth recording:** git's auto-merge spliced T10's `wsid` stamping into T12's `adopt_tab` push (matching struct-literal context lines) and silently dropped it from `add_tab` - caught by auditing every auto-merged region against BOTH parents, not just the conflict markers.
  - **Verify:** native `cargo check` + `clippy --all-targets` clean on both feature sets; `cargo test --no-default-features` **181/181** = T10's 163 + T12's 15 + 3 new satellite-x-apply interaction tests, a strict superset of both; desktop `cargo check` clean, `control::` 48/48 + `pty::` 5/5 unchanged.
- 2026-07-02 T11 DONE (branch `t11-panels`): panels - Files (tree + fuzzy search), Preview (local dev URLs), Dev runner - as one self-contained `panels/` module + the exported `PanelHost` composite, usable as a native tile or side surface.
  Zero `app.rs` changes (T10 owns it this wave); the only touches outside `panels/` are `render::open_url` becoming `pub(crate)` (preview/runner URLs open through T6's existing opener) and the two new bins in Cargo.toml.
  - **Layering (T9 template exactly):** `files.rs` / `preview.rs` / `runner.rs` are gpui-free (serde wire payloads + state structs + reducers + plain-data view-models); `mod.rs` composes them into `PanelsState` (+ the shared project list derived from live session cwds); `feed.rs` is the I/O pump (`PanelsFeed`: one poll/action thread with the pure `PanelPlan` cadence scheduler, short-lived probe threads for URL checks); `view.rs` (feature `gui`) is the ONLY gpui file.
  Demo bins per the brief: `panel-window` (gui) and `panels-probe` (headless acceptance).
  - **Mount contract (captain / T10-T12):**
    ```rust
    let feed = panels::PanelsFeed::spawn(client.clone());    // once per process
    let panels = cx.new(|cx| PanelHost::new(feed.clone(), cx.focus_handle()));
    div().flex_1().child(panels.clone())                     // any box: tile or side surface
    // hooks:
    feed.set_root("/path/to/project");        // bind Files+Run to a tile's cwd (sticky)
    feed.note_session_urls(&session, urls);   // push T6 visible_urls scans (either id space)
    ```
    One-step: `PanelHost::mount(client, cx) -> (Entity<PanelHost>, PanelsFeed)`.
    `PanelHost` is `Focusable`; the host must focus it (or route clicks) for typing - the Files tab types into the fuzzy-search box, the Run tab into the command line (Enter commits). `panel-window` shows the wiring.
    **Single-drainer rule honored:** `PanelsFeed` subscribes to NO `events()` receiver (poll-only: sessions 10s - tightened to 1s while a spawn awaits identification, preview capture-scan 10s, runner tail 1s while active, git 30s, search debounce 120ms), so it composes beside the T9 `OverlayFeed` in one process untouched.
  - **Files:** lazy tree via `list_dir` (expand/collapse, cached, dirs-first server order, client dot-file filter default on, show-ignored toggle = server refetch), fuzzy search via `index_project` once + `search_files` limit 50 (120ms debounce, stale-seq rejection, client-side subsequence highlight spans for DISPLAY - ranking stays server-side), read-only viewer via `open_file` (2 MiB server cap surfaced + a 2000-line render clip), `git_info` header (branch, dirty count, worktree badge).
  **The M4 write gate is untouched:** no server command was added; there is no edit/save path (webview `write_text_file` stays Tauri-only).
  - **Preview (the embed answer):** gpui 0.2.2 has NO web-content element, so true in-window embedding is not feasible (alternatives considered: screenshot-poll needs a browser to render and none exists headless on the box; server-side rendering is new surface).
  Built the best available: per-session local-URL lists (dedup, newest-first, cap 8 - webview parity) from TWO sources - host-pushed `visible_urls` scans (wrap-aware, for attached tiles) and the feed's own `read_terminal` capture scan (covers unattached sessions) - plus CLIENT-side reachability probes (TCP connect + minimal HTTP/1.1 GET over std, no new deps: status code + `<title>`, https = connect-only), `0.0.0.0`/`::` listen-addrs swapped to `localhost` for opening, one-click external open via the platform handler, per-URL re-probe.
  - **Dev runner (the §3 "check what the socket offers" answer):** the socket exposes NOTHING for devserver control - `start_dev_server`/`stop_dev_server`/`probe_tcp`/`preview_host` are Tauri-only `#[tauri::command]`s and `devserver://<id>` events do not ride the control EventFanout. Per the brief that gap stays a FOLLOW-UP (exposing devserver control + its event channel over the socket); NO risky server surface was added.
  Instead the native runner composes commands the socket already audits, making the dev server a first-class tmux session (visible in every client, survives native-client restarts): `spawn_terminal {cwd,name}` (UI-adopted; the response carries no id, so the new session is identified by baseline-diffing `list_terminals` + pane-cwd match) -> an adopt-marker `echo` proves the bound session is OUR fresh shell before anything else is ever typed -> the dev command runs wrapped with a nonce'd exit marker (`cmd; echo EXIT:<code>`, quote-split so the terminal's echo of the TYPING never matches) -> tail + URL detection via `read_terminal` polling (no PTY attach = no resize contention with the user's own tiles) -> stop = `send_keys C-c` + a delayed stop-probe echo (interactive shells abort the `; echo` list on SIGINT, so "the probe marker appears at the prompt" is the termination signal; natural exits report their real code via the exit marker) -> kill = `close_terminal`.
  The machine only ever sends input to sessions it created/bound; nonces make stale scrollback markers inert; spawn/adopt/stop run on timeouts with a fail-fast fold when the server refuses the spawn outright.
  - **Build/verify:** `cargo check` + `cargo clippy --all-targets` CLEAN (0 warnings) on BOTH feature sets; `cargo test --no-default-features` **185/185** (was 151; +34 panels tests: tree fold/flatten/dotfiles, search debounce/stale-seq/clear, highlight spans + segmenting, viewer clip/supersede, URL parse/local-filter/scan, fold dedup/cap/newest-first, probe lifecycle, dead-session URL retention, HTTP response parsing, the full runner happy path + marker nonce/echo guards + wrong-cwd/baseline spawn identification + all three timeouts + session-vanish + default-command detection + edit guard, `PanelPlan` cadences, project fold/sticky-selection/cycling).
  - **Acceptance (headless `panels-probe`, green **`PANELS-PROBE-OK`** TWICE):** (a) against the LIVE app at 127.0.0.1:57129 and (b) against the T13a headless `control_probe_server` running CURRENT source - both on disposable `th_t11*` sessions only.
  Per leg: Files - `index_project` (319 files) -> `search_files "cargotoml"` (6 hits, top `apps/cli/Cargo.toml`, highlight spans) -> `list_dir` tree -> `open_file` README (79 lines) -> `git_info` (branch `t11-panels`, dirty count, linked-worktree), all folded through the real reducers; Preview - a disposable session echoed two localhost URLs, the capture scan folded both, the client probe read a REAL local HTTP server as `Reachable(200)` + title `T11 Probe` and a dead port as `Refused`; Runner - adopt handshake -> Running -> URL detected from live capture -> C-c stop observed via the stop-probe marker -> natural exit observed with code 0 via the exit marker -> `close_terminal` verified the session gone.
  - **Visual check (WSLg, `panel-window`, no Windows build):** screenshots [`T11-panels-files.png`](./T11-panels-files.png) (tree + `⎇ t11-panels ·6 ⧉` git header), [`T11-panels-search.png`](./T11-panels-search.png) (ranked hits, matched chars highlighted, key-file stars), [`T11-panels-preview.png`](./T11-panels-preview.png) (live green URL row `http://localhost:5199/` detected from a `0.0.0.0` listen line, fetched page title, 200), [`T11-panels-run.png`](./T11-panels-run.png) (detected `pnpm dev` default, idle state, Run control, tail box).
  Verified interactively: project-picker cycling, type-to-search + Enter-opens-top-hit + the Escape stack (viewer -> query), tree expand/collapse, tab switching, preview probe/title live against a real `python3 -m http.server`.
  - **Deviations / judgment calls:**
    1. The runner is tmux-composed (above) rather than a stub awaiting devserver socket commands - zero new server surface, and the more product-coherent shape for a terminal multiplexer. The devserver-socket route stays the documented follow-up if hidden-process runners are wanted.
    2. The RUNNING app build (pre-#17) still gates `spawn_terminal` off, so the live spawn leg reports the server's refusal (fail-fast fold verified live via `THN_PROBE_SPAWN=1`); current source forwards to the UI sink (the headless server verified the no-sink refusal). Full spawn->identify->adopt is machine-tested; its first live run needs an updated app build.
    3. Project roots derive from live session pane cwds (`list_terminals`); the selection is STICKY once made - pane cwds churn as agents cd around (a project can vanish from the list while its directory is perfectly valid), and a host-bound root may never appear as a project at all.
    4. Spawn identification matches baseline-diff + cwd; a concurrent user spawn at the SAME cwd inside the ~1s window could be mis-adopted (documented; the adopt marker still proves it is a responsive shell before any command is sent).
    5. Capture-scan URL detection misses URLs that wrap at the pane edge (capture text has no wrap flags); the host-push path (T6 `visible_urls`) is wrap-aware and takes precedence when wired.
    6. The viewer clips rendering at 2000 lines (the element tree rebuilds per frame); the clip is surfaced in-UI. No syntax highlighting (plain monospace).
    7. Preview keeps a dead session's URLs until the list is empty (a dev server often outlives its terminal); the group is labeled "session gone".
  - **Live-app note for the captain:** during acceptance the running app's control socket began resetting NEW connections (the known task-27 attach-churn/CLOSE_WAIT wedge; the listener was confirmed healthy via Windows-side loopback while WSL connects reset). Probe run (a) landed in a good window; run (b) plus the state-machine tests cover everything deterministically. Also: one E2E-driving mishap typed the stray text `cargotoml` into a NON-tmux foreground window (all t-hub / t-hub-dev / t-hub-localtest panes verified clean) - if that string shows up in some composer, it was this task's input injection, not the user.
  - **Version:** native crate stays `0.1.0` (independent); `apps/desktop/` untouched, so `bump-version.sh` intentionally NOT run.
- 2026-07-02 T27 DONE (branch `s27-attach-churn`): the server attach path now survives client churn - an abrupt client death at ANY point in the accept-to-forwarder lifecycle tears the forwarder down promptly, and accumulated dead attaches can no longer wedge the new-attach path (the T10 incident that wedged the general's running app).
  - **Root cause (why 72 CLOSE_WAITs wedged NEW attaches while existing ones streamed):** an attach connection had NO write timeout and NO keepalive.
  A client that stopped draining (SIGTERM'd mid-attach, suspended, or silently vanished) left the forwarder blocked in `write_all` forever: a received FIN never unblocks a blocked write (only an RST does), so the socket sat in CLOSE_WAIT while its handler thread stayed pinned.
  And when the sink write DID fail first, `pty::stream_reader_loop` fell into `child.wait()` on a still-running `tmux attach` client that nobody would ever kill (the connection thread was parked in a read only the dead client could end, so its `detach()` never ran), leaking the thread and its socket clone - the CLOSE_WAITs that never reap.
  Every wedged forwarder also pinned an `ACTIVE_CONNS` slot; at `MAX_CONNS` (256) the accept loop dropped every new connection at accept, so fresh `attach_pty` failed for ALL clients ("connection closed before the scrollback frame" / reset) while established attaches streamed on.
  The later "live sessions reported missing" tail is the same wedge: leaked forwarders hold PTYs/threads/fds, and once spawn/fd pressure bites, the `tmux has-session` subprocess check fails and reads as "session gone".
  - **Fix (`control.rs` + `pty.rs`, defense in depth):**
    1. `SO_SNDTIMEO` (30s, ctx-tunable) is set on every attach connection before the seed; the option lives on the underlying socket, shared by every clone, so one call bounds the scrollback seed AND the forwarder's streaming sink - deaths DURING the seed included, no write can pin a thread anymore.
    2. `stream_reader_loop` now kills the attach client itself when the SINK dies first (it owns the `Child`, so the `wait()` that used to block forever is prompt) and skips the pointless exit frame to a dead sink.
    3. New `on_stream_end` hook on `stream_attach_to_sink`: the forwarder thread shuts the SOCKET down when the stream ends (sink death, or the tmux session exiting under a still-connected client), so the connection's input read unblocks immediately instead of waiting for the client to close; connection-side teardown also shuts down BEFORE joining, so `detach()` can never wait behind a blocked write, and it now runs on the error exits too (RST mid-stream).
    4. TCP keepalive (60s idle + 15s probes, via `socket2` - new direct dep, already in-tree transitively) on EVERY accepted control connection: a half-open socket whose peer vanished with no FIN/RST (powered-off tailnet box, killed WSLg/msrdc window) is reaped in minutes, even in the long-lived modes (attach, event subscribe) that legitimately clear the idle read timeout.
    5. Defensive forwarder-table bound: `MAX_ATTACH_FORWARDERS = 64` (well under `MAX_CONNS` so attach churn can never starve the request/event paths; ~4 full 14-tile cockpits), CAS-exact acquisition, RAII release on every exit path, refusal is an actionable error frame; `attach_forwarder_count()` exposed for diagnostics and the tests.
    6. Ride-along: `serve()` uses `thread::Builder` so a failed handler spawn under resource pressure logs and recovers instead of PANICKING the accept loop (`thread::spawn` panics on spawn failure - the listener would have died under exactly the pressure it exists to survive).
  - **Regression tests (real `serve` accept loop + real tmux, serialized on a private mutex):** `attach_path_survives_abrupt_client_churn` kills clients at every stage - before speaking, mid-request-line, missing-session, pre-seed FIN x3, post-seed RST x3, and the incident's exact shape: a tiny-recv-buffer client that starts a `yes | head -n 1000000` firehose, stops reading, and silently HOLDS its socket open.
  It proves the forwarder table returns to baseline WHILE the wedged client still holds its end (only the write-timeout -> kill -> shutdown chain can achieve that), then proves a FRESH attach succeeds end to end (seed + `S27_CHURN_OK` echo round-trip) and both the forwarder and connection tables drain.
  `attach_forwarder_cap_refuses_then_recovers` proves the bound: at cap the refusal is a clear error then a close (not a park), a client disconnect drains the slot, and the attach path is serviceable again.
  - **Verify:** desktop `cargo check` clean; `cargo test --lib` **216/216** green (+2 new; `control::` and `pty::` suites all green); clippy clean on every touched line (the ~77 pre-existing doc/MSRV lints are all in untouched code).
  - **T12 note 5 (first applies after a cold boot trail ~15s) - explanation, not a server bug:** the apply broadcast rides the separate events connection; `EventFanout::emit_event` writes it the moment the command is accepted, and nothing in the server serializes applies behind attaches (each attach is its own connection + thread and shares no lock with the fanout; T12's own monitor subscription logged broadcasts in real time during the storm).
  The 15s trail is the NATIVE WORKER draining `apply_requests()` only between passes of its first sequential attach loop - exactly where T12's note placed it (`app.rs`, the native crate, T10's file) - so it is outside this desktop-crate task's boundary and unchanged here.
  This task adds no latency to the boot storm either: the 30s write timeout never fires for a draining client, and 14 steady-state forwarders sit far under the 64 cap.
  - **Version:** `apps/desktop/` IS touched but `bump-version.sh` intentionally NOT run per the task brief - captain reconciles at merge.
- 2026-07-02 T14a DONE (branch `t14-audit`): the parity AUDIT half of T14 - matrix + distribution story + small-gap closure. The cutover flip itself (steps in the doc's §5) stays with the general.
  - **Deliverable: [T14-PARITY.md](./T14-PARITY.md)** - 77-row matrix (webview `apps/desktop/src` audited as the spec vs `apps/native/src`): **28 present / 9 degraded / 37 missing / 3 wontport**, each row with effort + owner. Gap concentration: 9 missing rows sit with the in-flight chrome crews; 6 fold into one keymap/palette/action-registry task (T-A, the biggest daily-drive gap); 6 into a flow-gaps task (T-B: local spawn affordance, worktree create/list/remove UX, chimes/OS-notify, single-instance - plus the resume wiring, currently a DEGRADED row, incl. the server-side `spawn_terminal` startup-command arg); 12 are post-flip polish (T-C), 3 distribution, 1 server-gated (M4). WSL-boundary checks all hold (doc §2).
  - **Distribution (researched 2026-07, pinned versions in the doc):** installer = **Velopack Setup.exe + portable zip** (answers msi-vs-nsis with NEITHER: dist/WiX MSI can't carry an updater, non-Tauri NSIS generators are unmaintained); updater = **velopack `UpdateManager` + `GithubSource`** over the existing GitHub Releases (delta updates; `self_update` as lean plan B); tray = **adopt via `tray-icon` 0.24.1** (works beside gpui's main-thread win32 pump, Zed-community precedent); signing = Azure Artifact Signing ($9.99/mo, enroll early). Key framing: NONE of this blocks the flip - the Tauri shell keeps hosting the server (and its updater/tray) until the server split completes.
  - **Small gaps closed (three commits, none touching chrome/view.rs or chrome/model.rs per the crew boundary):** (1) SGR dim renders at alacritty's 66% DIM_FACTOR, applied before the inverse swap (T7 known gap); (2) `ensure_metrics` shapes a narrow-glyph probe beside the M-run and WARNS when a missing family substituted a proportional face (the T7 drift bug's silent mode, now loud); (3) endpoint discovery honors `T_HUB_CONTROL_FILE` (the desktop/devbuild var) after `T_HUB_CONTROL_JSON`, so one env block points a devbuild server and the native client at the same handshake file.
  - **Verify:** `cargo check` + `cargo clippy --all-targets` clean (0 warnings) on BOTH feature sets; `cargo test --no-default-features` **221/221** (was 215; +6: 2 dim, 3 font-probe predicate, 1 discovery precedence). `apps/desktop/` untouched, so `bump-version.sh` intentionally not run.
- 2026-07-02 T24 DONE (branch `ux-cues-panes`): supervision cues, the native slice - a tile now SAYS why it looks frozen instead of reading like a normal quiet pane.
  Screenshots: [`T24-supervision-badges.png`](./T24-supervision-badges.png) (one row showing all four states side by side: idle-live dot + age, streaming-live green dot + `0s ago`, red `EXITED 0` on a dimmed tile, red `DEAD` on a dimmed frozen tile) and [`T24-reconnecting.png`](./T24-reconnecting.png) (every header amber `reconnecting Ns` during a total link outage).
  - **Header age (always on):** every tile header now carries a right-aligned last-output age (`3s ago` / `2m ago` style, coarse by design), stamped by the feeder threads through the new `TilePulse` (output stamp + sticky PTY exit code in one lock-free per-tile cell; the attach scrollback seed counts as output - it IS the content on screen arriving).
  - **`chrome/cues.rs` (new, gpui-free):** age formatting, `TilePulse`, and the `TileLife` state machine - `Live` / `Reconnecting{since}` / `Exited{code,since}` / `Dead{since}` - fed one observation per reconcile pass (`listed` from `list_terminals`, `link_down` from the wire, `exit` from the pulse).
    The distinctions the general kept getting bitten by are now explicit: **session-gone** (delisted without an exit frame -> `DEAD`) vs **process exit** (real exit frame -> `EXITED <code>`, sticky through delisting - the code is more informative than a generic dead) vs **viewer-attach-lost** (listed alive but the attach link is down -> `reconnecting Ns`, amber - work may well continue underneath).
  - **Auto-reattach was already in the wire** (T4: a mid-stream disconnect reattaches with 250ms->5s backoff and re-seeds scrollback) - it was just INVISIBLE by design. Additive wire change: a per-attach `LinkState` flag (`PtyHandle::link_down()`, set when the reader enters its reconnect loop, cleared when a reattach lands). No §1.3 behavior change; consumers of `PtyFrame` see nothing new.
  - **Dead tiles linger instead of vanishing:** reconcile now keeps a died-this-run tile PLACED for 45s (`THN_DEAD_LINGER_MS` overrides) with its badge and a dim scrim over the frozen content (`reconcile_with_cwds_lingering`; the old signature delegates with an empty set, so every prior caller/test is untouched). The attach handle is dropped the moment the death verdict lands (no futile reconnect churn against a gone session - directly relevant to the task-27 CLOSE_WAIT findings), the tile close `x` dismisses early, and dead-on-arrival tiles (died while the app was closed) still prune at boot exactly like pre-T24.
    A delisted id that comes BACK (same tmux name, new session) revives: the stale pool entry is dropped and the normal attach path re-attaches fresh.
  - **The link ticker (the bug the live run caught):** the first live pass showed ages ticking but NO reconnecting badges during a total outage - `ControlClient::request` retries internally with backoff for ~18s before surfacing an error, so a fold-on-list-failure never ran while the cue mattered most. Fix: a 500ms `spawn_link_ticker` folds LINK-ONLY observations (`LifeTracker::observe_link`: defunct verdicts hold, live tiles flip on the wire flag alone - and never `Dead`, because no list means no death verdict). Measured badge onset after a hard link drop: <=2s. A blocked-in-backoff worker no longer delays the cue.
  - **Ride-along wire fix:** the reader's backoff sleep is now stop-interruptible (50ms slices) - dropping a `PtyHandle` mid-backoff (the dead-tile path does this constantly) used to be able to stall its caller up to 5s under the state lock.
  - **Narrow-tile cue clamp (live-run catch #2):** at ~15 header chars the full `reconnecting 15s` cluster overflowed left across the dot/title; the cue now drops the age first, then truncates the badge, so it never collides (screenshot-verified clean at 4-across portrait-monitor widths).
  - **Tests:** 235 green under `--no-default-features` = main's 215 (the T10/T11/T12 union) + 20 new across T24/T26 - T24's: age formatting, pulse semantics, every life transition (attach-loss/recover, exit-sticky-through-delist, delist-without-exit, revive, linger window + release, dead-on-arrival never lingers, link-only folds), lingering reconcile. `cargo check` + `clippy --all-targets` 0 warnings on BOTH feature sets.
  - **Acceptance (live, WSLg, real fleet):** the LIVE app's attach path was wedged (49 CLOSE_WAITs on pid 19092 - the documented task-27 bug, active), so per T11/T12 precedent acceptance ran against the T13a `control_probe_server` (current source, protocol v2) through a killable local TCP proxy, with the real tmux fleet + disposable `th_t24*` sessions and `THN_LAYOUT` pointed at a scratch file (the user's layout untouched).
    Proven end to end: a killed disposable's tile showed the red badge + dim scrim (exit frame delivered -> `EXITED 0`); a session killed WHILE the viewer link was down showed `DEAD` after the link came back (no exit frame, delisted verdict); a hard proxy kill turned every header amber `reconnecting Ns` within ~2s, ages frozen and growing in lockstep; restoring the proxy recovered every tile (fresh scrollback re-seed, `reattached after disconnect` logged) with badges clearing; both defunct tiles pruned on schedule ~45s after their verdicts; a restart pruned dead-on-arrival tiles instantly.
  - **Deviations / judgment calls:**
    1. `tmux kill-session` with a HEALTHY attach yields `EXITED 0` (the server delivers the exit frame before delisting), not `DEAD` - the exit frame is simply the stronger fact. `DEAD` appears exactly when no exit frame ever arrived (viewer detached/link down at kill time, or the server lost the session).
    2. Semantic agent-status dots (T9) yield to life states: a dead session's stale `working` status is noise.
    3. When the session list is unavailable the UI never claims `DEAD` - tiles show `reconnecting` (the wire link is the only honest signal) until a real list returns a verdict.
    4. A placed tile whose FIRST attach keeps failing still paints `attaching…` (no link to be down yet); the reconcile retry loop is its badge.
- 2026-07-02 T26 DONE (branch `ux-cues-panes`): adjustable pane sizes - drag dividers between tiles, per-workspace persisted ratios, double-click resets to auto.
  Screenshot: [`T26-pane-dividers.png`](./T26-pane-dividers.png) (the 10-tile [4,3,3] grid with row 0 dragged to ~47% height - AFTER an app restart, proving the ratios persisted).
  - **Model (`chrome/model.rs`, gpui-free):** `GridRatios { rows: [RowRatio { h, cols }] }` - fractions of the usable (gap-free) extent, rows sum to 1, each row's cols sum to 1. `tile_boxes_ratio` paints them; `tile_boxes` (auto) now delegates with `None`, so the auto grid is byte-identical to before AND to `GridRatios::even`. `divider_zones` derives the hit bands from the SAME dims the boxes came from (12px grabbable band centered on the 6px gap); `divider_extent` + `apply_divider_split` are the pure drag math (position-based: the pointer maps into the neighbor pair's combined extent, clamped so neither pane drops under 80px).
  - **Shape semantics:** ratios describe ONE `split_rows` shape (the tile count they were dragged at). A reflow to a different count paints auto but KEEPS the ratios dormant - session churn that returns to the old count restores the user's sizes; only the divider double-click clears them for good. A reconcile mid-drag stops the drag (never mutates dormant ratios or resizes the wrong pair through a stale id).
  - **Persistence (`chrome/persist.rs`):** `LayoutTab.grid: {rows:[{h, cols:[..]}]}` - additive and tolerant both ways like T10's `satellite` (absent field = auto; unknown fields inside skipped; a hand-edited garbage value sanitizes to auto on load, never an error; auto workspaces serialize WITHOUT the field so a pre-T26 binary sees nothing new).
  - **View (`chrome/view.rs`):** divider zones + grid area ride the per-window `TileZones` (satellite windows share the input core, so their grids drag too - per-WinKey state, same T10 rule); hover shows a half-alpha accent bar in the gap + `ResizeRow`/`ResizeColumn` window cursor; dragging shows the full-accent bar and re-splits live - the reflowed boxes hit the EXISTING `sync_and_paint_content` geometry path, so terminal cols/rows refit and the PTY resize round-trips per paint (T5). Ratios persist once on mouse-up (not per motion event). Double-click a divider = back to auto (`grid: None`, persisted).
  - **Tests (gpui-free):** even-ratios == auto-grid equivalence, ratio boxes, mismatched-shape fallback, sanitize (normalize + reject NaN/zero/negative/empty), divider zone geometry + counts + 0/1-tile cases, extent lookup incl. stale ids, split clamping + normalization-through-drags, persist round-trip + garbage tolerance.
  - **Acceptance (live, WSLg, same probe-server rig, real crew sessions):** hover on a gap painted the accent band (pixel-verified `ha(ACCENT,.5)` blend across the full row width); dragging the [4,3,3] grid's first row divider down 250px resized it live - layout JSON showed `h: 0.4675` (= 1/3 + 250px/1863px usable, exact) and tmux geometry proved the PTY refit: row-0 session `17x52` vs row-1 `23x21`; an app RESTART re-applied the ratios (identical tmux sizes, screenshot above); a column drag pinned its neighbor at exactly the 80px minimum (`0.0978 = 80/818`, the clamp engaging); divider double-click removed `grid` from the JSON and re-equalized every pane (both probes back to 37 rows).
  - **E2E-driving note for future crews (environment, not product):** this box's msrdc RAIL window had stopped forwarding Windows-side synthetic input entirely (SetCursorPos/mouse_event clicks never reached the X app; window resizes never reached gpui) - the T8-era recipe silently no-ops. Working recipe: position the pointer with Windows `SetCursorPos` (WSLg syncs the Windows cursor into the X server) and inject buttons/keys with `python-xlib` XTest INSIDE WSL; pure-XTest motion gets stomped by the WSLg cursor sync, pure-Windows clicks never arrive. `THN_HIT_DEBUG=1` logs every tile-area mouse-down with the live divider zones - added for this hunt, kept (env-gated) for the next one.
  - **Version:** native crate stays `0.1.0` (independent per §0); `apps/desktop/` untouched, so `bump-version.sh` intentionally NOT run.
- 2026-07-03 T-A DONE (branch `ta-keymap`): keymap + action registry + command palette - the §1.3 flip-gating rows of the T14 matrix, ported so a webview user keeps their muscle memory verbatim.
  Screenshots: [`TA-command-palette.png`](./TA-command-palette.png) (palette over the cockpit: fuzzy query `close term`, selected row with description + `Ctrl+W · Ctrl+B X` binding hints + categories, F2/Ctrl+R/Esc footer) and [`TA-prefix-hud.png`](./TA-prefix-hud.png) (the armed-leader pill: `Ctrl+B - waiting for key...`).
  - **Registry (`chrome/actions.rs`, gpui-free):** the webview's 21 commands with ids/labels/descriptions/categories copied verbatim from `lib/commands.ts` (persisted keymaps and palette search transfer 1:1).
    `execute()` is a pure executor over `ChromeModel` returning `Effect`s (persist, focus-notify, tile-closed, raise-satellite, font-changed, PTY literal, host) for the view to run.
    Flows the native client cannot run locally yet (`spawnTerminal`, `newWorktreeWorkspace`, `openWorktreesList` - the T-B daily-drive gaps) still bind, list, and dispatch: they land in `dispatch_host()` with the focused tile's cwd - the ONE seam T-B replaces with its executor (today it logs, exactly like `app.rs` logs `ResumeSession`). No shared edits between the crews.
  - **Keymap (`chrome/keymap.rs`, gpui-free):** the webview's three-tier capture-phase routing, checked before any key reaches a tile: editable guard (native: the focused tile's find bar; rename mode never reaches the keymap) -> prefix mode (ctrl+b leader, 1.5s `PREFIX_TIMEOUT_MS`, bare-key match with modifiers ignored, double-tap types the leader's literal C0 byte, unbound second key disarms and falls through) -> direct chords.
    Defaults are the webview seed verbatim: ctrl+t/w spawn/close, ctrl+tab / ctrl+shift+tab cycle, ctrl+1..9 workspace jump, ctrl+= / ctrl+- / ctrl+0 zoom, ctrl+j focus region, ctrl+k palette; prefixed c/w/t/x/p/o/n/b/l.
    Rebinds are conflict-clearing per tier (assigning a chord strips it from the old owner atomically, webview `setBinding` rule) and persist to `~/.t-hub/native-keymap.json` (`THN_KEYMAP` overrides) in the webview's `{prefixKey, direct, prefixed}` v1 shape - sanitized on load (unknown ids and unparseable chords drop, an unusable prefix falls back to ctrl+b, a corrupt file downgrades to defaults), and the file REPLACES the defaults wholesale, exactly like `coercePersisted`.
    `KeyController` bundles keymap + palette + focus region behind the one `on_key()` entry point, so the full dispatch stack tests headless.
  - **Palette (`chrome/palette.rs` + `chrome/palette_view.rs`):** the webview's `fuzzyScore` ported exactly (case-insensitive subsequence; gap cost + first-hit lateness, lower wins, registry order tie-break) over `label + description + category`; open resets state; enter executes the highlighted command after closing; the 12-row window slides with the selection (no scroll machinery).
    Rebind capture: F2 or Ctrl+R on the highlighted row, next chord binds (lone modifiers wait, Esc cancels), persisted immediately.
    The view layer draws plain state in the `overlays/view.rs` idiom and carries NO listeners - input reaches it only through the controller, so it never fights the canvas hit zones.
  - **View wiring (`chrome/view.rs`, dispatch only):** `on_key` routes through the controller between rename mode and `tiles_key`; consumed keys run `apply_effects` mirroring the mouse paths (save_layout + sync_active_sessions, mode-1004 focus notify, the close-`x` drop path, satellite `activate_window`, per-tile font re-spec, literal PTY write with typed-input semantics, host seam with the focused cwd).
    Any click dismisses the palette (webview backdrop parity) and a tile-area click returns the focus region to the tiles.
    Satellites deliberately keep raw terminal input - the webview keymap only ever lived in its one window (documented deviation; revisit post-flip if satellite muscle memory wants chords).
  - **Zoom (the §1.3 degraded row, closed):** `Tile::set_font_size` (additive in `render/mod.rs`) rebuilds the tile fonts and drops metrics + row cache, so the next `sync_and_paint_content` re-probes and the PTY refits through the EXISTING geometry path (T5/T26 rule: no new reflow code).
    Zoom acts on the active workspace's `font` override (base = `THN_FONT`/default), clamps 6..28 rounded (webview `clampFont`), reset returns to the base size keeping any family/ligature override, and persists per workspace in the layout JSON.
  - **Focus region (ctrl+j):** a minimal keyboard model of the webview's `focusedRegion`: while SIDEBAR holds focus the cycle chords act on workspace tabs, bare arrows step the active workspace, enter/escape return to the tiles, and plain typing is swallowed (never reaches the invisible-to-focus terminal); an amber pill says so.
  - **Tests:** 297 green under `--no-default-features` = the 241 baseline + 56 new - chord build/parse/normalize/format + platform folding, literal-for-prefix, all three dispatch tiers (arm, bare-key with shift ignored, double-tap, unbound fallthrough, timeout expiry + re-arm, guard disarm, lone-modifier keeps the arm), conflict-clearing rebinds both tiers, disk round-trip + sanitize + corrupt-file downgrade, the fuzzy scorer's exact semantics, the palette reducer (filter/clamp/wraparound/execute/close/rebind capture incl. Ctrl+R and cancel), and the executor (jump incl. satellite-raise + out-of-range, global cycle skipping satellites + crossing tabs, sidebar-region tab cycle, close=detach, zoom steps/clamps/reset/active-workspace-only, host commands).
    `cargo check` + `cargo clippy --all-targets` 0 warnings on BOTH feature sets.
  - **Acceptance (live, WSLg, the REAL running server + full session fleet, disposable `th_kmapA/B` sessions, `THN_LAYOUT`/`THN_KEYMAP` scratch files - the user's state untouched):**
    one pane string proves the whole input contract: `hia^Bzxw` in kmapA's `cat -v` = plain typing passed through, `ctrl+b ctrl+b` typed the literal 0x02, an unbound prefix key (`z`) disarmed and fell through, and after a full 1.5s timeout `x` arrived as a plain char with the tile still open (the direct `ctrl+w` close would have needed the chord).
    `ctrl+b c` created + activated a workspace; `ctrl+1`/`ctrl+2` jumped; `ctrl+tab` crossed into kmapB (`echo BB` ran there) and `ctrl+shift+tab` came back.
    Zoom persisted live: two `ctrl+=` wrote `size: 15.0` into the layout JSON mid-run, `ctrl+0` restored 13.0 as a full per-workspace `FontSpec`.
    The palette opened on `ctrl+k`, fuzzy-narrowed `close term` to the right rows with live binding hints, and EXECUTED from the list (enter on "New worktree workspace" -> the T-B seam logged `NewWorktreeWorkspace`); `ctrl+t` logged `SpawnTerminal` with the focused tile's cwd.
    The prefix HUD and SIDEBAR region pills painted (screenshots above); sidebar arrows stepped the active workspace; `ctrl+w`-family close detached the tile and `tmux has-session` proved the session survived.
  - **Deviations / judgment calls:**
    1. The palette is keyboard-only in this slice (arrows + enter; F2/Ctrl+R rebind) - the webview's per-row mouse buttons need palette hit zones, deferred.
       The rebind keystroke itself could not be live-captured this pass: msrdc RAIL swallowed F2 outright (hence the Ctrl+R alias), and by the time the alias shipped the user was actively at the machine, so no further foreground could be borrowed; the capture flow is fully unit-tested and rides the same delivery path every live-proven palette key used. Flag for the daily-drive week.
    2. `closeTerminal` maps to the native close=detach (T8's documented deviation); kill-with-confirm stays with the T24 supervision UX.
    3. WSLg environment (extends the T26 note): keys only deliver while the msrdc window is Windows-FOREGROUND - a fresh launch gets a grace window, `set_input_focus` is ignored by WSLg's WM, and `SetForegroundWindow`/minimize-restore cannot steal it back while the user types elsewhere. Script live passes as one tight burst right after launch.
    4. The scratch client necessarily reported its temp tabs to the server's registry mirror during the pass (T12 last-writer-wins); the webview re-asserts on its next tabs change.
  - **Version:** native crate stays `0.1.0` (independent per §0); `apps/desktop/` untouched, so `bump-version.sh` intentionally NOT run.
