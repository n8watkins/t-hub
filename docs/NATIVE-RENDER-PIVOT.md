# T-Hub - Native Render Pivot (Design + Plan)

> **ARCHIVED 2026-07-05.** The native pivot is paused - the general ended it after Lane N merged ("too much work for efficiency gains I can't quantify").
> The code and this doc's living copy moved to the `native-archive` branch (tag `native-pivot-final`); `apps/native` is removed from main.
> The webview app is the product. Survivors shipped in 0.3.39: attach stability (PR 7), headless server-authoritative organization (PR 8), captain overlay (PR 9).
> Next active work: docs/CAPTAIN-CHAT-PHASES.md.

> **STATUS (2026-07-01) - DECIDED; GATES RUN; FRAMEWORK CONFIRMED.**
> The owner has chosen a full-native client: retire the webview renderer and rebuild the frontend on a native Rust GPU stack.
> Framework: **GPUI - CONFIRMED by the T2 spike (both §6 gates PASSED**; evidence in [T2-GPUI-SPIKE-RESULTS.md](./T2-GPUI-SPIKE-RESULTS.md)); Iced 0.14 remains the documented fallback.
> T1's loopback half also PASSED (see §7); the device-B remote half is recipe-ready in [T1-REMOTE-VERIFICATION.md](./T1-REMOTE-VERIFICATION.md).
> This doc is the cold-start guide for the pivot; the working tracker mirrors §7 as tasks T1-T14.
> Decision rubric, fixed by the owner: **scalability > implementation control > aesthetics**; effort, timeline, and migration risk are explicitly NOT factors; GPL-linked dependencies are acceptable (T-Hub stays open-source/personal).
> Provenance: 29-agent adversarially-verified stack evaluation (2026-07-01); every load-bearing claim below survived or was corrected by that verification.

## 1. The one-line bet

Keep the server as a raw-byte pump and make the client a native GPU app: `alacritty_terminal` parses the PTY stream client-side, and every terminal grid is drawn by one batched GPU scene per window on the discrete GPU - no webview anywhere in the render path.

## 2. Why the webview has to go

- xterm.js wants a GPU context per tile; WebView2 caps live WebGL contexts (~16) and hard-evicts the least-recent, which forced T-Hub onto the slower Canvas-2D addon (`Terminal.tsx` header comment) and produced a trail of pain: `docs/PERF-AND-DRAG-WORKLOG.md`, `docs/STRUCTURAL-FREEZE-ANALYSIS.md`, and DnD rebuilt around canvas (`dropTarget.ts`).
- WebView2 cannot force the high-performance GPU, and Chromium cannot composite across GPUs on Windows (WebView2Feedback #5072), so the discrete GPU sits idle while the integrated chip paints the terminals.
- The product's core promise is "many terminals, all live, all the time" - exactly the workload a browser engine is structurally worst at and a native renderer is built for.

## 3. Decision record

| # | Decision | Notes |
|---|---|---|
| D1 | **Full-native client** (drop the webview; rebuild the frontend) | Owner accepts the frontend rebuild if the result "looks good and is fast". |
| D2 | **VT parser lives client-side** (`alacritty_terminal`); the server stays a byte pump | Already the shipping runtime: `commands.rs::spawn_terminal` spawns no in-process PTY, `attach_terminal` goes through `RemotePty` over the control socket, and `Cargo.toml` has zero VT-parser deps. |
| D3 | **GPUI leading, Iced fallback**, decided by the §6 spike gates | The GPL-3.0 question (zed #55470) is accepted under the open-source intent, so it does not block GPUI. |
| D4 | **tmux stays** as the durable session server | Persistence + multi-client attach are tmux's job; "max control" means owning the render pipeline, not reinventing tmux. |
| D5 | **Two observation planes, never conflated** | Screen-derived signals (activity, localhost URLs, cursor) are client-authoritative from the grid; semantic status (cost, context %, needs-input, supervision) is server-computed and arrives as `session://status` / `supervision://tree`. Never reconstruct semantic state from pixels. |
| D6 | **Future phone/Mac clients parse their own bytes** (Model A) | A server-rendered grid would put a parser server-side and break D2. `alacritty_terminal` compiles for mobile targets; SwiftTerm/xterm.js are acceptable phone-side parsers. |
| D7 | **React cockpit retires at cutover** | The Tauri app stays the reference implementation and fallback until T14, then is frozen for revert. |

## 4. Target architecture

```
        t-hub server (today: in-app backend; target: headless in WSL/remote)
  ┌───────────────────────────────────────────────┐
  │ tmux -L t-hub · PTYs · supervision · journal  │
  │ usage · recents · file index · git · metrics  │
  │ control socket: NDJSON commands + events      │
  │  + PTY frames {out}/{write}/{resize}          │
  │    (binary framing lands at T13)              │
  └───────────────┬───────────────────────────────┘
                  │ raw bytes + events (loopback or tailnet)
  ┌───────────────▼───────────────────────────────┐
  │ native client (Windows now; Mac later)        │
  │  alacritty_terminal per session (parse+damage)│
  │  one batched GPU scene per window,            │
  │  shared glyph atlas, discrete GPU             │
  │  native cockpit chrome: tiles · headers ·     │
  │  workspaces · sidebar · satellites            │
  └───────────────────────────────────────────────┘
```

The native client is simply the second client implementation of the already-shipped server split (see `SERVER-SPLIT-AND-ROADMAP.md`).
Its existence is the strongest argument for finishing that doc's M4 hardening (multi-client, auth, named sessions).

## 5. Why this is a one-seam swap, not a rewrite

The reader loop in `remote_pty.rs` (~line 342) today decodes each `{out}` frame and emits `terminal://output` into the webview.
The native client replaces that emit with `Term::advance(&bytes)` into a client-side `alacritty_terminal`, then draws from the damage.
Connect, handshake, `{write}`, `{resize}`, detach, and the `{scrollback}` seed are untouched.
The existing `COALESCE_WINDOW` / `MAX_BATCH_BYTES` batching converts directly into the damage feed.

## 6. Framework decision: GPUI vs Iced

**GPUI (leading).**
The only stack already shipping T-Hub's shape: Zed is a wall of `alacritty_terminal` grids plus rich panel chrome composited into one batched GPU scene per window (adjacent same-styled cells merged into batched text runs, backgrounds as rect quads, one shared per-window atlas: R8 alpha-mask tinted by fg color, RGBA sampled directly for emoji), against a documented 8ms/120fps frame budget.
Highest aesthetic ceiling of any option; Zed is the proof.
Verified caveats, all landing on the primary Windows target:

- The Windows backend is GPUI's youngest and uses native DX11; Zed's cross-platform wgpu migration is Linux-only (PR #46758), and the Windows-wgpu effort lives in the actively maintained community fork `gpui-ce`.
- Open hybrid-GPU adapter-selection bug (zed #36798): may pick the integrated GPU on integrated+discrete machines.
- Zed publicly deprioritized general-purpose/community GPUI for 2026; GPUI is consumed as git/platform crates, not a stable published crate.
- Unadjudicated GPL-3.0 transitive static-link question (zed #55470: `sum_tree -> ztracing`); accepted under D3.

**Iced 0.14 (fallback; runner-up).**
Same architectural ceiling on a cleaner substrate: a custom `iced_wgpu::Primitive` gives the terminal renderer its own wgpu pipeline/shader (verified), `pane_grid` is literal tmux-style tiling, daemon mode shares one Engine/device across windows, MIT license, DX12 on Windows with standard `PowerPreference::HighPerformance` adapter selection.
Every load-bearing Iced claim survived adversarial verification.
Costs: you write the terminal renderer and text stack yourself (glyphon), and the proven aesthetic top-end is lower.

**Rejected.**
egui: aesthetic ceiling explicitly below requirement.
Dioxus/Freya: Skia caps GPU exploitation (2D compositing only, no custom pipelines, no first-class discrete-GPU selection, single-threaded across windows); `freya-terminal` remains a useful reference for observation signals.
Hybrid winit+wgpu+wry: keeps React at the cost of a permanent two-render-tree wart and a documented-broken Windows composition path; its only differentiator is one the owner waived.

**Spike gates (T2, run on the hybrid-GPU Windows 11 box):**

1. 12-16 terminal elements, each fed a synthetic firehose (`yes | cat`, `cat /dev/urandom | xxd`), hold frame budget with all grids scrolling at once (GPUI repaints the whole window with no per-tile damage regions - this is the claim under stress).
2. The "Using GPU" log selects the discrete adapter, or it can be forced (zed #36798).

Fail either gate → flip to Iced (T3) with no loss of architectural ceiling.

**RESULT (2026-07-01): both gates PASSED - GPUI is the confirmed framework.**
Full evidence in [T2-GPUI-SPIKE-RESULTS.md](./T2-GPUI-SPIKE-RESULTS.md); highlights:

- G1: 16 real `alacritty_terminal` grids, worst-case renderer (no damage tracking, full reshape every frame): rock-steady ~90 fps on a 180Hz panel, never below 82, zero seconds under 55; 12 grids ran 150-180 fps.
  GPU utilization 3.5-7.3% (the 7800 XT nearly idle) - enormous headroom; the spike's CPU-side brute force is the ceiling, and a production damage-driven renderer only improves it.
- G2: discrete RX 7800 XT selected by default (gpui log + 100% of GPU-engine samples on the discrete LUID); no forcing needed on this box (the zed #36798 hybrid concern applies to laptop-style setups; OS-level per-app "High performance" is the lever there).
- Buildability: **`gpui = "0.2.2"` exists on crates.io** and a standalone app built in 165s with zero workarounds - this CORRECTS the §6 caveat below about GPUI having no published crate (true only of git-main's newer split).
- The zed #55470 GPL chain is absent from published gpui 0.2.2 (name-level check).

## 7. Phased plan (mirrored as tracker tasks T1-T14)

Dependency spine: T2 → T3 → T4 (T1 also gates T4); T4 → {T5, T8, T9}; T5 → {T6, T7, T13}; T8 → {T10, T11, T12}; {T6, T7, T9, T10, T11, T12, T13} → T14.

### Phase 0 - Gates (T1, T2 run in parallel)

| Task | What | Acceptance |
|---|---|---|
| **T1** | **Verify the server split against a second device** (M2b has never been tested): loopback regression; Tailscale-bind thin client from device B (`T_HUB_REMOTE_ADDR`/`T_HUB_REMOTE_TOKEN` - NOT `T_HUB_CONTROL_TOKEN`, which is the server-side override); non-tailnet LAN peer rejected by `is_allowed_peer`; kill/reconnect re-sync. | **✅ DONE 2026-07-01.** Loopback PASSED end-to-end (commands, version gate, auth, event fanout, PTY seed/out/write/resize, drop + re-sync); remote bind live via explicit `T_HUB_CONTROL_BIND` on the tailnet IP; peer gate observed rejecting non-tailnet sources pre-auth; **positive device-B round-trip confirmed from an Android phone over the tailnet**. Full findings + gotchas (PATH no-op, mirrored-networking hairpin) in [T1-REMOTE-VERIFICATION.md](./T1-REMOTE-VERIFICATION.md). Found en route: the `git_info` `wsl.exe -e` bug (fixed, v0.3.28). |
| **T2** | **The decisive GPUI spike**: minimal GPUI app (or fork the standalone gpui-terminal / instrument a Zed nightly), one window, 12-16 firehose-fed `alacritty_terminal` Terms; measure sustained fps and the selected GPU adapter; `cargo deny check licenses` for the GPL chain (informational). | Recorded fps + adapter evidence; written pass/fail against both §6 gates. **✅ DONE 2026-07-01 - both gates PASSED** ([results](./T2-GPUI-SPIKE-RESULTS.md)); spike source at `C:\Users\natha\spikes\gpui-spike\`. |
| **T3** | **Record the framework decision** in §6; if T2 failed a gate, run the equivalent Iced spike first (custom `iced_wgpu::Primitive` + glyphon grids, `pane_grid` sanity, daemon multi-window smoke, adapter check). | This doc updated with the choice + evidence. **✅ DONE 2026-07-01 - GPUI confirmed** (§6 RESULT block); pin `gpui = "0.2.2"` for T4. |

### Phase 1 - Terminal path

| Task | What | Acceptance |
|---|---|---|
| **T4** | **Native client skeleton**: new workspace crate; window + event loop + render surface; control-socket client (discovery via `~/.t-hub/control.json`, env overrides, token auth, request/response, event-stream subscription). | Opens, connects to a running server, lists sessions, logs live events. |
| **T5** | **The one-seam swap** (§5): `attach_pty` per session; `{scrollback}`+`{out}` → `Term::advance`; damage-clipped batched rendering of N grids in one window; `{write}`/`{resize}` round-trip. | 12+ live tmux-backed tiles render, scroll, type, and resize in one native window. |
| **T6** | **Terminal UX completeness**: full keyboard encoding (modifiers, app-cursor, bracketed paste), mouse modes + arbitration, selection + clipboard, scrollback viewport, search, URL detection from the client grid (replaces the JS output scan), per-tile palettes. | Side-by-side parity with a Tauri tile for daily Claude/Codex use. |
| **T7** | **Font subsystem**: shaping + ligatures on a mono grid, color-emoji fallback, grapheme/combining marks, wide chars, box-drawing/Powerline as procedural sprites (not font glyphs), sRGB/linear blending; GPUI text system or glyphon per T3. | A torture-test script (emoji, ligatures, Powerline, CJK, combining marks, box-drawing TUI) renders correctly. |

### Phase 2 - Cockpit rebuild

| Task | What | Acceptance |
|---|---|---|
| **T8** | **Chrome rebuild**: workspaces as hideable tabs; tile grid (auto near-square + manual ratios + drag-reorder); per-tile headers (status ring, folder, git branch + worktree badge + dirty dot, client icon, context meter, editable work-name, fullscreen, close); theming. | The daily-driver layout is fully operable in the native client. |
| **T9** | **Sidebar overlays**: recents + resume flow, usage/cost, host/WSL metrics, supervision tree, toasts with tab-aware suppression - all consumed from the shipped M3 socket surface (semantic plane, D5). | Sidebar parity against the same running server. |
| **T10** | **Multi-window satellites**: tear-off = separate OS window with its own surface + glyph atlas, shared socket client, per-window layout state. | A workspace tears off, renders live terminals, closes back cleanly. |
| **T11** | **Panels**: file tree/search (`index_project`/`search_files` over the socket; arbitrary-path read/write stays M4-gated), preview (fed by T6 grid URL scan), dev runner. | All three tile panel views work natively. |
| **T12** | **MCP organization continuity**: socket mutations (`move_tile`, `rename_tab`, `focus_session`, `new_tab`, `spawn_terminal`, `open_file`, ...) apply to the native workspace model, replacing the webview `ApplySink`/`controlBridge` path. | The full audited MCP tool surface works end-to-end from a Claude session. |

### Phase 3 - Protocol + cutover

| Task | What | Acceptance |
|---|---|---|
| **T13** | **Binary PTY framing**: bump `PROTOCOL_VERSION` to 2; length-prefixed binary frames for PTY out/write (JSON stays for commands/events); version negotiation so V1 (webview) clients keep working. | Measured bandwidth drop on a firehose session; V1 clients unaffected. |
| **T14** | **Parity audit + cutover**: checklist vs the Tauri app (prefix keymap, palette, session restore, worktree flow, notifications, theming); distribution/updater story for the native binary (Tauri updater/tray/installer are lost); then daily-drive and freeze the webview client for revert. | A week of daily-driving with no fallback, then the webview path is frozen. |

## 8. Cross-cutting truths (hold under any framework)

- **The font subsystem is the real cost center.**
  `alacritty_terminal` is parser + grid + damage only - zero font, rasterization, atlas, or GPU code.
  The expensive parts: grapheme/combining-mark correctness (Zed has open bugs here), ligatures across a mono grid, color-emoji fallback, wide chars, box-drawing/Powerline drawn procedurally as sprites (font-sourced glyphs cause seams; incomplete codepoint tables are a recurring bug class), and sRGB/linear blending.
  Off-GPUI, `glyphon` (cosmic-text + swash + etagere) is the turnkey wgpu text engine.
- **`alacritty_terminal` API notes (verified).**
  `Term::renderable_content()` returns a `RenderableContent` struct exposing a `GridIterator<Cell>` plus cursor/selection/colors/mode; you transform `Cell` into render cells yourself, as the alacritty binary does in `display/content.rs`.
  Damage is real: `Term::damage()` / `TermDamage` / `reset_damage()` (per-line dirty).
  Zed consumes a pinned git fork (rev 4c12966, "0.26.1-dev"), not crates.io.
- **Protocol tax.**
  base64 + NDJSON adds roughly 33% plus framing per `{out}` frame - fine on loopback, a real tax over the tailnet with many hot terminals; hence T13.
- **tmux geometry.**
  `window-size latest` (`pty.rs:78`) means two clients attached to the same pane collapse to the most-recent geometry; phone+desktop on one pane needs grouped sessions, separate windows, or accepted mirroring.
  Unsolved, and predates this pivot.
- **The scrollback seed is an approximation.**
  The `{scrollback}` frame is a reflowed `capture-pane -e` snapshot; the client grid is byte-authoritative only from attach forward.
- **Two id spaces (verified in the T1 run).**
  `list_terminals`/`attach_pty`/`read_terminal` address tmux tile ids (`th_`-stripped), while supervision/`get_status` key by Claude session UUID; a native client must maintain the tile-to-session-UUID mapping to render status.
  Also `tmux_target` truncates a bare sessionId to 8 chars (`tmux.rs:272-278`) - pass the full `th_`-prefixed name for long-named sessions.
- **Default remote port 8787 can collide** (occupied by `workerd` in WSL on the dev box, and WSL mirrored networking shares the port space); set `T_HUB_CONTROL_PORT` when enabling the remote bind.

## 9. Risks and open questions

- **M2b is untested against a second device.**
  T1 exists because betting the client architecture on an unexercised wire would be building on sand.
- **GPUI substrate risk**: youngest-on-Windows, community-fork dynamics (`gpui-ce`), no stable published crate - the price of the 120fps proof.
  Iced remains viable at every point as the pressure-release valve.
- **Input-encoding completeness (T6) is a long tail** (kitty keyboard protocol, mouse modes); budget for chasing edge cases against real TUIs (Claude Code, Codex, vim, htop).
- **Tauri conveniences are lost at cutover**: updater, tray, installer packaging; T14 owns the replacement story.
- **Accessibility regresses**: native grids leave the DOM a11y tree; accepted for a personal tool, noted honestly.
- **Satellites multiply atlases** (per-window); memory scales with windows × visible cells; fine at realistic counts, watch it in T10.

## 10. References

- Zed/GPUI: zed #36798 (hybrid-GPU adapter selection), zed #55470 (GPL-3.0 transitive link), Zed PR #46758 (Linux-only wgpu migration), community fork `gpui-ce`, `gpui-component`, standalone `gpui-terminal`.
- Crates: `alacritty_terminal` (Apache-2.0), `iced` 0.14 (`iced_wgpu` custom `Primitive`, `pane_grid`, daemon multi-window), `glyphon`/`cosmic-text`/`swash`/`etagere`, `portable-pty`.
- WebView2: WebView2Feedback #5072 (cannot force the high-performance GPU).
- Internal: `docs/SERVER-SPLIT-AND-ROADMAP.md` (the wire this pivot rides; M4 is now motivated by this client), `docs/PERF-AND-DRAG-WORKLOG.md` + `docs/STRUCTURAL-FREEZE-ANALYSIS.md` (the webview pain record), `apps/desktop/src-tauri/src/remote_pty.rs` (the seam), `apps/desktop/src-tauri/src/pty.rs:78` (`window-size latest`), `.mcp.json` (the MCP surface T12 must keep working).
