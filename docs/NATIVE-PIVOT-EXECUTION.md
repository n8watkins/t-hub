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
- PTY plane (v1, default): command `attach_pty` on a dedicated connection; opening frame `{"scrollback":"<b64>"}` (reflowed capture-pane snapshot - approximate; byte-authoritative only from attach forward), then `{"out":"<b64>"}` / `{"exit":code}` outbound; `{"write":"<b64>"}` and `{"resize":{"cols":C,"rows":R}}` inbound.
- PTY plane (v2 binary, T13 - opt-in): send `attach_pty` with arg `"binary": true`. The connection then speaks length-prefixed BINARY frames on the PTY plane (commands + events stay JSON). Negotiation is per-attach and additive: a client that omits `binary` gets v1 unchanged, so the webview is unaffected. The server advertises support via the handshake's `protocol_version` (now `2`); a request `"v"` at or below the server's version is accepted (only a higher, unknown-future version is rejected).
  - **Frame layout:** every frame is `[u8 type][u32 big-endian length][length payload bytes]`. Type tags (mirrored in `pty::binframe`): server->client `0x01` OUT (raw output bytes), `0x02` EXIT (payload = 4 BE bytes of an `i32` exit code, or EMPTY for unknown/signalled - the v1 `null`), `0x03` SCROLLBACK (opening seed, raw bytes), `0x04` ERROR (UTF-8 message, for a pre-stream failure); client->server `0x10` WRITE (raw stdin bytes), `0x11` RESIZE (payload = `[u16 BE cols][u16 BE rows]`). No base64, no JSON envelope on the out/write firehose. Inbound frame length is capped at 16 MiB (`pty::BIN_MAX_FRAME`); an unknown inbound type tag is skipped (forward-compat).
  - **Executable reference client + proof:** `scripts/probes/t13_binframe.py` (drives both v2 binary and v1 fallback against a headless `control_probe_server` example, on a disposable tmux session; measures the wire reduction). T13b's native `wire/` should mirror its binary framing.
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
