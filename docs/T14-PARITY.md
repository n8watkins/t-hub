# T14 - Native Client Parity Audit

> **Date:** 2026-07-02 (branch `t14-audit`, native crate at 221 tests, desktop at v0.3.34).
> **Scope:** phase one of T14 - the parity MATRIX and the distribution story.
> The cutover flip itself is the general's call; §5 below is the recommended sequence.
> **Method:** full audit of `apps/desktop/src` + `src-tauri` (the webview spec) against `apps/native/src` (the implementation), plus web research on July-2026 distribution tooling.
> **Statuses:** `present` (daily-drive parity), `degraded` (works with documented deviations), `missing` (not built), `wontport` (deliberately not carried over).
> **Effort:** S = under an hour, M = an hour to a day, L = multi-day.

## 0. Architecture framing (read before the matrix)

The "Tauri app" is TWO things: the webview UI and the Rust server (control socket, tmux, agent bridge, MCP).
The cutover freezes only the webview UI; the Tauri process keeps running as the server until the server-split (M1 in progress) finishes.
Consequences:

- Updater, installer, and tray are NOT cutover blockers - the Tauri shell still provides them for the server process during the parity period.
  The distribution story (§3) is for the post-split world and is gated on the server-split milestone, not on the flip.
- During the parity period both clients CAN attach to one server, but the tab-registry mirror is last-writer-wins (T12 caveat) - the flip must leave exactly one reporter (§5 step 4).

## 1. Parity matrix

### 1.1 Terminal core

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Grid rendering (truecolor, 256, bold, underline, damage-clipped paint) | `Terminal.tsx` / xterm.js | **present** (T5) | - | done |
| Keyboard encoding (xterm full: modifiers, app-cursor, F-keys, AltGr) | `Terminal.tsx:580-668` | **present** (T6) | - | done |
| Kitty keyboard protocol | not in webview either (xterm.js partial) | **degraded** - mode bits tracked, encoder speaks classic xterm; no acceptance TUI needs it | M | none (documented T6 deferral) |
| Mouse reporting (X10/UTF-8/SGR, arbitration, Shift override) | xterm.js | **present** (T6) | - | done |
| Selection (word/line/block, scrollback-offset) + clipboard | xterm.js + clipboard plugin | **present** (T6; WSLg clipboard proven live) | - | done |
| Bracketed paste + injection guard, middle-click paste | xterm.js | **present** (T6) | - | done |
| Scrollback viewport + scrolled-back badge | xterm.js | **present** (T6) | - | done |
| Find-in-terminal (highlights, ordinal/total, wraparound) | xterm search addon | **present** (T6) | - | done |
| URL detection + open (Ctrl+click) | xterm web-links addon | **present** (T6, wrap-aware) | - | done |
| SGR dim | xterm.js | **present** - closed in T14 (`term/mod.rs`, alacritty DIM_FACTOR before inverse swap) | - | done (T14) |
| Ligatures, emoji fallback, combining marks, wide chars, box-drawing/Powerline sprites | webview: font stack + glyphs | **present** (T7; sprites beat WT on E0A0/E0A2) | - | done |
| Missing-font substitution warning | n/a (browser handles) | **present** - closed in T14 (`render/mod.rs` narrow-probe + warn) | - | done (T14) |
| Per-terminal color palette overrides | `theme.ts` termOverrides | **missing** - ANSI 16 hardcoded VS-Code-ish | M | theming task (§4 T-C) |

### 1.2 Cockpit chrome

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Workspace sidebar: create/rename/close/switch | `store/workspace.ts` tabs | **present** (T8) | - | done |
| Auto-grid layout (splitRows semantics) | `Canvas.tsx:54-69` | **present** (T8, exact port) | - | done |
| Manual tile ratios (drag gutters, persisted weights) | `Canvas.tsx:511-567` | **missing** (T8 documented deferral) | M | chrome crews (in flight) |
| Drag-reorder tiles | `Canvas.tsx:569-588` | **missing** (T8 documented deferral) | M | chrome crews (in flight) |
| Tile header: status dot + title + close | `Tile.tsx:549-901` | **present** (T8; dot is semantic via T9 state) | - | done |
| Tile header: git branch + worktree badge + dirty dot | `Tile.tsx` header anatomy | **missing** (data already flows - T11 Files panel shows the same `git_info`) | M | chrome crews (in flight) |
| Tile header: client icon, context meter, editable work-name | `Tile.tsx` header anatomy | **missing** | M | chrome crews (in flight) |
| Fullscreen tile (⤢ + Esc) | `Canvas.tsx:432-452` | **missing** | M | chrome crews (in flight) |
| Close tile = detach (session survives) | webview kills after confirm | **degraded** - native close always detaches, never kills (T8 deviation, safer but not parity) | M | chrome crews / T24 supervision |
| Kill session with busy-confirm (Ctrl+Shift+W + dialog) | `useLifecycleKeybinds.tsx:27-78` | **missing** | M | chrome crews / T24 supervision |
| Spawn terminal locally (+ button, presets, Ctrl+T) | `Canvas.tsx:225-253`, `commands.rs:232-290` | **degraded** - closed in T-B: the sidebar plus row split into "+ workspace" / "+ terminal"; the latter spawns a shell tile via the socket `spawn_terminal` (server-minted id -> apply placement). Presets (Claude/Custom) and Ctrl+T ride T-A's palette/keymap | S (with T-A) | T-B (button); T-A (presets/chord) |
| Multi-window satellites (tear-off/close-back, bounds persist) | `windows.ts` popOutTab | **present** (T10) | - | done |
| Session restore (layout, tabs, active, satellites, per-workspace font) | `workspace.ts:668-716` + SQLite snapshots | **present** (T8/T10, `~/.t-hub/native-layout.json`) | - | done |
| Focused-tile restore across restart | `workspace.ts` focusedId | **missing** (minor; deliberate T8 choice) | S | chrome crews if wanted |
| Layout snapshot history (last-20 ring for recovery) | `db.rs` SNAPSHOT_HISTORY_CAP | **missing** - single JSON file, atomic write | S-M | none (nice-to-have) |
| Sidebar collapse modes (full/rail/hidden) | `App.tsx` sidebar mode | **missing** | M | chrome crews (in flight) |
| Single-instance guard | Tauri default | **present** - closed in T-B: exclusive OS file lock beside the layout (`persist::acquire_instance_lock`); a second instance on the same `THN_LAYOUT` exits with a clear message, distinct layouts coexist | - | done (T-B) |

### 1.3 Keymap, palette, actions

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Action registry (21 commands, typed handlers) | `lib/commands.ts`, `keymapExecutor.ts` | **missing** | L (with keymap below) | **no task** (§4 T-A) |
| Prefix keymap (Ctrl+B leader, 1.5s timeout, double-tap literal, HUD) | `prefixKeyHandler.ts` | **missing** | (in T-A) | no task (§4 T-A) |
| Direct chords (Ctrl+T/W/Tab/1-9/J/K, zoom keys) | `keybindings.ts:43-62` | **missing** - only per-tile terminal keys exist (`render_support.rs:58-80`) | (in T-A) | no task (§4 T-A) |
| Rebindable bindings, conflict handling, persistence | `keybindings.ts` (localStorage v1) | **missing** | (in T-A) | no task (§4 T-A) |
| Command palette (fuzzy, rebind UI, categories) | `CommandPalette.tsx` | **missing** | (in T-A) | no task (§4 T-A) |
| Zoom hotkeys (font size +/-/reset, persisted) | `keybindings.ts` zoom trio | **degraded** - per-workspace font size exists (layout JSON / `THN_FONT`) but no runtime hotkey | S once T-A lands | §4 T-A |
| Workspace jump (Ctrl+1..9), tile cycle (Ctrl+Tab) | `keymapExecutor.ts` | **missing** | (in T-A) | no task (§4 T-A) |

### 1.4 Sidebar overlays and notifications

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Recents list (dedup, open-project filter, worktree hints) | sidebar Recent rows | **present** (T9) | - | done |
| Recents hide/archive | localStorage set + `archive_recent_project` | **present** (durable archive; in-memory hide resets on restart - acceptable) | - | done |
| Resume flow (click -> `claude --resume <id>` in new tile) | `workspace.ts:1085-1120` recall | **present** - closed in T-B end to end: the socket `spawn_terminal` carries `startupCommand` (both server paths: sink forward -> webview IPC pass-through, sink-less -> `commands::pane_command` login-shell wrap + minted-id broadcast), and the `app.rs` host-request worker sends `claude --resume '<id>'` at the recorded cwd; placement rides the apply broadcast | - | done (T-B; live-proven) |
| `resumeStartsClaude` toggle (resume vs plain shell) | `settings.ts` | **missing** (settings task) | S after T-B | §4 T-C |
| Claude/Codex usage meters + cost | sidebar usage | **present** (T9, statusline-first cadences) | - | done |
| Host/WSL metrics | sidebar metrics | **present** (T9) | - | done |
| Supervision tree | sidebar supervision | **present** (T9; fleet view deviation documented) | - | done |
| Toast cards (dedup, warmup, tab-aware suppression, TTL) | `notify.ts` mapping | **present** (T9) | - | done |
| Sound chimes (WebAudio synth: attention/done/error) | `notify.ts:30-113` | **present** - closed in T-B, zero new deps: pure sample synth of the exact notify.ts recipes (`overlays/alerts.rs`) + WAV playback via `paplay`/`pw-play`/`aplay` (unix; WSLg Pulse proven audible) / winmm `PlaySoundW` (Windows). Fires per fresh toast (inherits dedup/warmup/active-tab suppression); `THN_SOUND=0` mutes until the T-C settings hub | - | done (T-B) |
| OS notifications | `notify.ts:115-162` (Tauri plugin) | **present** - closed in T-B, zero new deps: `notify-send` (unix; silent where no daemon exists, e.g. WSLg - toast cards remain the cue) / PowerShell WinRT toast under the PowerShell AppID (Windows, the Tauri plugin's own trick; needs a Windows-build spot-check). Same trigger + `THN_NOTIFY=0` gate | - | done (T-B) |
| Rules engine (status-transition -> notify/sendText/spawn/restart/run) | `store/rules.ts` (WS-5b) | **missing** | L | no task (post-flip, §4 T-C) |
| Auto-continue on usage-limit reset (`autoContinueText`) | settings + client injection | **missing** | M | no task (§4 T-C) |
| Claude hooks install/uninstall panel | ThemeEditor HooksSection | **degraded** - no UI, but the SERVER reconciles managed hooks at boot regardless of client (`lib.rs` spawn_reconcile_managed_hooks) | S (post-flip) | §4 T-C |

### 1.5 Panels

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Files tree + fuzzy search + read-only viewer + git header | Files panel | **degraded** - fully built (T11) but `PanelHost` is NOT MOUNTED in the cockpit; lives only in the `panel-window` demo bin | S-M (mount point is chrome/view.rs) | chrome crews (in flight) |
| File EDIT/save | `write_text_file` (Tauri-only) | **missing** by design - server M4 write gate untouched | gated | server M4 |
| Preview panel (URL list, reachability, title) | Preview iframe | **degraded** - no gpui web element, so embed is **wontport**; probe + external-open shipped (T11) | - | done (documented) |
| `note_session_urls` push (T6 grid scan -> panels) | n/a | **missing** wiring (part of mounting) | S (with mount) | chrome crews |
| Dev runner (spawn, tail, URL detect, stop/kill) | DevTab + devserver.rs | **degraded** - tmux-composed runner shipped (T11); hidden-process runner needs devserver socket commands (documented follow-up) | M (server) | T11 documented follow-up |
| File icon themes (lucide/vscode/seti) | `settings.ts` fileIconTheme | **wontport** - native uses its own minimal glyphs | - | - |

### 1.6 Worktree and MCP flows

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Worktree create via MCP/socket (`create_worktree` -> named tab + placed tile) | `controlBridge.ts` | **present** (T12, cwd-verified) | - | done |
| Worktree create via LOCAL UX (prefix `w`: branch focused repo into sibling worktree) | `workspace.ts:1131-1172` | **missing** (UI trigger only) - the server side is COMPLETE: socket `create_worktree` (T12) does git add + named tab + placed tile. The webview's trigger is itself a keymap chord (prefix `w`), so the native trigger rides T-A's keymap - one action calling `create_worktree` on the focused tile's repo | S (in T-A) | T-A (trigger); server done |
| Worktree list/re-open/remove UX (prefix `l`) | worktrees list dialog | **missing** (UI dialog only) - the server side is now complete: T-B added `list_worktrees` (socket twin of `git_worktree_list`), joining `create_worktree`/`remove_worktree`. The webview trigger is a chord (prefix `l`); the native dialog rides T-A's palette | M (in T-A) | T-A (dialog); server done (T-B) |
| `remove_worktree` native-only (no webview attached) | `workspace.ts:1174-1198` detach-then-remove | **present** - closed in T-B (T12 deviation 2): sink-less removal with socket subscribers broadcasts the detach forward FIRST, then runs `git worktree remove` server-side and CONFIRMS (`removed: true`); subscriber-less stays refused | - | done (T-B; live-proven) |
| MCP organization continuity (move_tile, rename_tab, focus_*, new_tab, spawn_terminal, open_file) | `controlBridge.ts` switch | **present** (T12) | - | done |
| Workspace-tab registry reporting (`report_workspace_tabs`) | Tauri report | **present** (T12; one-reporter rule at flip, §5) | - | done |

### 1.7 Settings and theming

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Settings UI (14 persisted flags, sections, Ctrl+,) | `settings.ts`, ThemeEditor | **missing** - env vars + layout JSON only | L | no task (§4 T-C) |
| Theming (8 presets, 23 chrome tokens, import/export, light mode) | `store/theme.ts` | **missing** - hardcoded dark theme ~= Midnight; colors live in 5 files (`chrome/view.rs:94`, `render/mod.rs:104`, `overlays/view.rs:37`, `panels/view.rs:40`, `app.rs:84`) | L | no task (§4 T-C) |
| Per-workspace identity colors, per-terminal focus rings | `theme.ts` workspaceColors | **missing** | (in theming) | §4 T-C |
| Work names (cwd -> label, surfaces in recents) | `theme.ts` workNames | **missing** | S-M | §4 T-C |
| Server-synced theme (`theme://changed`, `~/.config/t-hub/theme.json`) | ThemeProvider | **missing** (client-side theming first) | (in theming) | §4 T-C |

### 1.8 Shell integration and distribution

| Capability | Webview reference | Native status | Effort | Covered by |
|---|---|---|---|---|
| Auto-updater (signed latest.json, silent install, relaunch) | tauri-plugin-updater + `updateMount.ts` | **missing** in the native binary; Tauri shell still updates the server during parity | M-L | §3 recommendation, post-split |
| Installer (NSIS per-user, shortcuts, uninstall) | tauri.conf.json bundle | **missing**; same framing | M | §3, post-split |
| System tray (Show/Reload/Reconnect/Quit, close-to-tray) | `tray.rs` | **missing**; the native window is the cockpit itself - adopt `tray-icon` post-split | M | §3, post-split |
| Window state: main-window bounds persist | webview does NOT persist either (no plugin) | **present-by-parity** (satellite bounds DO persist, better than webview) | - | done |
| Win11 Snap Layouts hook (`win_snap.rs`) | needed because webview window is frameless | **wontport** - native uses OS decorations; snap works natively | - | - |
| Custom titlebar + auto-hide + titlebar settings (4 flags) | Titlebar.tsx, settings | **wontport** - OS decorations | - | - |
| Drag-drop files onto tile (Windows path -> WSL translate -> paste) | `dropPaste.ts` toWslPath | **missing** - gpui has external-paths drop; needs a `toWslPath` port | M | no task (§4 T-C) |
| Clipboard image paste to temp file + path insert | `dropin.rs` | **missing** | M | no task (low priority) |
| Shell-open external links | shell plugin | **present** (`render::open_url`: rundll32/open/xdg-open) | - | done |
| Dev/prod variant isolation (`T_HUB_*` env) | lib.rs devbuild | **present** - discovery honors `T_HUB_CONTROL_FILE` (closed in T14) + `T_HUB_CONTROL_JSON`/`THN_LAYOUT` | - | done (T14) |

### 1.9 Status tally

| Status | Count |
|---|---|
| present | 33 |
| degraded | 8 |
| missing | 33 |
| wontport | 3 |
| **rows** | **77** |

> Updated by T-B (2026-07-03): resume flow, single-instance guard, chimes, OS notifications, and native-only `remove_worktree` moved to **present**; the local spawn affordance moved to **degraded** (button shipped; presets/Ctrl+T ride T-A). The two worktree UX rows stay missing but are now TRIGGER-only gaps (their server surface is complete, incl. the new `list_worktrees`).

Of the 33 missing: 9 sit with the in-flight chrome crews (incl. the panels URL push that rides their mount), 6 fold into one keymap/palette task (T-A) plus the two worktree trigger rows that now ride it, 12 are post-flip polish (T-C, the settings/theming family), 3 are the distribution items (§3), and 1 is server-gated (M4 file write).

## 2. WSL-boundary checks (T14 brief item)

Checked against the brief's boundary list; two gaps were closed in this task, the rest hold.

1. **Endpoint discovery.** Windows-side client reads `%USERPROFILE%\.t-hub\control.json` (where the app writes it); WSL-side reads the `~/.t-hub/control.json` symlink mirror (memory-noted, works).
   `T_HUB_CONTROL_FILE` (the desktop/devbuild var name) is now honored alongside `T_HUB_CONTROL_JSON` (closed in T14), so a dev-variant server and the native client can share one env block.
2. **Client-local state.** `native-layout.json` resolves per-host home (Windows vs WSL), so driving the client from both sides yields two layouts.
   Acceptable for one daily-drive host; do not run both concurrently against one server (registry reporter fight, §0).
3. **Fonts.** Cascadia Mono is a Windows assumption; the Linux/WSLg run substitutes silently.
   The metrics sanity warning (closed in T14) now flags a proportional substitute instead of drifting silently.
4. **Clipboard.** gpui-native both sides; WSLg -> Windows clipboard sync proven live in T8/T10 acceptances.
5. **URL open.** Per-platform (`rundll32` / `xdg-open`) shipped in T6.
6. **Paths.** All file/worktree paths flow server-side (WSL-native), so the client never translates today.
   The day drag-drop of WINDOWS paths lands (§1.8), it needs the `toWslPath` port - flagged in that row.
7. **Remote/tailnet.** `T_HUB_REMOTE_ADDR`/`T_HUB_REMOTE_TOKEN` bypass the file; remote-peer spawn cwd stays allowlist-scoped server-side (#27) - no client change needed.
8. **Metrics.** `proc_stats` (winstat instrumentation) reads `/proc` and returns zeros on Windows - cosmetic, debug-only.

## 3. Distribution story (updater, installer, tray)

Research date 2026-07-02; sources and version pins in the task log.
Framing per §0: none of this blocks the flip - it is gated on the server split, when the native binary stops riding the Tauri shell.

**Landscape (verified current):**

- `cargo-dist` is alive again UNDER axodotdev (`dist` v0.32.0, May 2026); the astral-sh fork was archived Dec 2025 and points back upstream.
  It generates PowerShell/zip installers and MSI (WiX3), but its MSI is CLI-shaped (bin + PATH, no shortcuts) and its updater (`axoupdater`, v0.10.0) explicitly does NOT work with MSI and needs dist install-receipts.
- `cargo-packager` (CrabNebula, the would-be drop-in Tauri replacement with NSIS + Tauri-style updater) is stale: no release since Nov 2024. Ruled out on maintenance risk.
- **Velopack** is the strong option: Rust crate v1.2.0 (Jun 2026), `GithubSource` against plain GitHub Releases, Setup.exe per-user installer (shortcuts + uninstall entry) plus a portable zip from the same `vpk pack`, zstd DELTA updates (a multi-MB GPUI binary updates cheaply), and `apply_updates_and_restart()` solves the Windows locked-exe problem via its Update.exe helper.
  Cost: `vpk` is a .NET 8 dotnet tool in CI, and `VelopackApp::build().run()` must be first in `main()`.
- `self_update` 1.0.0-rc.1 + `self-replace` 1.5.0 are healthy - the lean plan B (no installer, no deltas, hand-rolled check-on-launch).
- Tray: `tray-icon` 0.24.1 (tauri-apps, actively maintained) works outside Tauri; on Windows it needs a thread with a win32 message pump - gpui pumps standard messages on the main thread, and there is Zed-community precedent for exactly this pairing (zed discussion #40318).
  Fallback: direct `Shell_NotifyIcon` via windows-rs (~300 LoC, zero deps).
- Signing: Azure Artifact Signing (ex Trusted Signing) is GA at $9.99/mo and now open to individuals; unsigned exes re-earn SmartScreen reputation on EVERY release, so this is worth it.

**Recommendation:**

1. **Installer: Velopack Setup.exe + its portable zip.** Answers "msi vs nsis" with NEITHER: dist's MSI can't carry the updater and every NSIS generator outside Tauri is unmaintained.
   The existing `release.yml` gains a .NET 8 setup step + `vpk pack`/`vpk upload` (vpk can publish straight to GitHub Releases); the Tauri NSIS job keeps shipping the server app until the split completes.
2. **Updater: velopack `UpdateManager` + `GithubSource`** replacing the Tauri latest.json flow (check ~6s after boot, honor the same `autoUpdateCheckEnabled`/`autoInstallUpdates` semantics once native settings exist).
   Plan B if the .NET CI dependency offends: `self_update` with the existing latest.json shape.
   Avoid axoupdater unless adopting the whole dist stack; avoid cargo-packager-updater outright.
3. **Tray: adopt, via `tray-icon`,** initialized on the main thread before `app.run()`, events forwarded over a channel into a gpui foreground task.
   Menu parity: Show, Reconnect (maps to ControlClient redial), Quit; "Reload window" is webview-specific - drop it.
   If event-loop interop misbehaves, fall back to Shell_NotifyIcon directly.
4. **Signing:** enroll Azure Artifact Signing now (identity validation can take weeks); wire the official signing action into release.yml when the first native artifact ships.

Estimated total for the Velopack path: 2-3 days of work plus signing enrollment lead time.

## 4. Proposed follow-up tasks (the gaps that need owners)

- **T-A - Keymap + action registry + command palette** (L).
  The single biggest daily-drive gap: port the 21-command registry, prefix keymap (Ctrl+B leader), direct chords, rebind persistence, and the palette.
  Blocked on the chrome crews' input-routing seam; should start immediately after they merge.
- **T-B - Daily-drive flow gaps** - **LANDED 2026-07-03** (branch `tb-flows`; execution-doc §5 entry).
  Shipped: resume wiring end-to-end (server `startupCommand` + `app.rs` arm), local "+ terminal" spawn affordance, native-only `remove_worktree` ordering server-side, `list_worktrees` socket command, sound chimes + OS notifications, single-instance guard.
  Remaining from the original list: the worktree create/list TRIGGERS (keymap chords in the webview too) and the spawn presets/Ctrl+T - all ride T-A's keymap/palette on the now-complete server surface.
- **T-C - Post-flip polish** (L).
  Settings UI + persistence hub, theming (tokens -> one struct -> JSON), work names, rules engine, auto-continue, drag-drop with `toWslPath`, hooks panel.
- **Server attach-churn fix** (pre-flip, desktop crate).
  The CLOSE_WAIT/attach-wedge under client churn (T10 incident, task #27 note) will bite a native daily-driver far sooner than it bit the webview; reap dead attach forwarders and restart the app before the flip.

## 5. Recommended cutover sequence

1. **Merge the two in-flight chrome-UX crews** (headers, ratios/reorder, fullscreen, kill-with-confirm, panels mounting all live there), then this branch.
2. **Fix the server attach-churn wedge and restart the live app** (§4 last item) - a churn-happy native client against the wedged server means daily-drive fallbacks on day one.
3. **Land T-A (keymap/palette) and T-B (flow gaps)** - after these, every remaining `missing` row is post-flip polish or distribution.
4. **Flip:** launch the native client as the default cockpit; keep the Tauri app running as the server with its window CLOSED (close-to-tray) and quiesce the webview's tab reporter so the native client is the sole `report_workspace_tabs` writer (one-line webview gate or simply never showing the window; verify `list_tabs` truthfulness once).
5. **Daily-drive a week, zero-fallback bar** (the §3 T14 acceptance); tag the last webview-default commit for instant revert; freeze `apps/desktop/src` (UI) to bugfix-only.
6. **Post-flip:** T-C polish on the native side; distribution work (§3) starts when the server split delivers a standalone server, ending with the Tauri shell's retirement.
