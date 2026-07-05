# Native Finish Plan - from parity to sole cockpit

Status snapshot date: 2026-07-05, main at 0.3.37.
This is the execution plan to FINISH the native pivot: close the flip-gating gaps, flip the default cockpit, land post-flip polish, and end with a distributable native app.
It supersedes the open items in [T14-PARITY.md](./T14-PARITY.md) §4-5 and picks up where [NATIVE-PIVOT-EXECUTION.md](./NATIVE-PIVOT-EXECUTION.md) §5 left off.

## 0. What is ALREADY done (do not re-plan these)

- T4-T13 all merged: wire client, render seam, terminal UX, fonts, chrome, overlays, satellites, panels (built, unmounted), MCP apply, binary framing.
- T-A merged (#28): 21-command action registry, prefix keymap (Ctrl+B leader), direct chords, command palette, rebinds.
- T-B merged (#29): resume end-to-end via spawn `startupCommand`, spawn button, worktree server commands, chimes + OS notifications, single-instance guard.
- T24 supervision cues + T26 adjustable pane ratios merged (#24/#26).
- S27 attach-churn fix merged AND the live app restarted on 0.3.37 (2026-07-05), so the pre-flip server-health prerequisite from T14-PARITY §5 step 2 is satisfied.
- Spawn latency: tmux calls batched into 2 command sequences (0.3.37), benefiting both frontends.

## 1. Lane N - flip blockers (the last chrome-UX gaps)

These are the only rows standing between today and the flip.
All are native-crate work in `apps/native/src/chrome/`; the webview implementations are the spec.

### N1 - Tile header anatomy (M)

- **Objective:** the real cockpit tile header: status dot + folder + git branch + worktree badge + dirty dot + client icon + context meter + editable work-name + fullscreen and close buttons.
- **Spec:** `apps/desktop/src/components/Tile.tsx` header (~lines 549-901).
- **Data:** already on the socket: `git_info` poll, `status://snapshot` (context meter, client), §1.2 id mapping. No server work.
- **Files:** `chrome/view.rs` (header render), `chrome/model.rs` (work-name state, persisted).
- **Acceptance:** headers live-update on a real session (branch within one `git_info` poll, status ring within 2s); work-name edit persists across restart.

### N2 - Tile drag-reorder gesture (S-M)

- **Objective:** drag a tile by its header to reorder within the grid.
- **Note:** the model API exists and is tested (`chrome/model.rs:841 reorder_tile`); only the gesture + drop-target highlight are missing.
- **Spec:** `Canvas.tsx` reorder UX (~lines 569-588).
- **Files:** `chrome/view.rs` (header hit-region drag arbitration - must not collide with T26 divider drags or terminal selection drags).
- **Acceptance:** drag tile A onto tile B splices it at B's slot; layout persists; divider drag and text selection still work.

### N3 - Tile fullscreen (S)

- **Objective:** one tile takes the whole grid; Esc or the header button restores.
- **Spec:** `Canvas.tsx` (~lines 432-452).
- **Files:** `chrome/model.rs` (fullscreen: Option<session-id> on the tab), `chrome/view.rs`, `chrome/actions.rs` (palette command + chord, reuse the webview binding).
- **Acceptance:** fullscreen resizes the PTY to the full grid, restore reflows; state survives tab switch; palette entry works.

### N4 - Kill session with confirm (M)

- **Objective:** Ctrl+Shift+W (and palette "Kill session") actually kills the tmux session, with the busy-process confirm dialog.
- **Spec:** `apps/desktop/src/hooks/useLifecycleKeybinds.tsx` (~27-78) + Tile.tsx dialog.
- **Note:** native close stays detach-by-default (T8 decision, safer); kill is the explicit escalation.
- **Files:** `chrome/actions.rs` (new CommandId), `chrome/view.rs` (confirm overlay), wire: existing `close_terminal` socket command with kill semantics (verify server arg; add if detach-only).
- **Acceptance:** killing a busy session prompts; confirming kills the tmux session (verify `list_terminals`); a disposable session only.

### N5 - Mount the panels (S-M)

- **Objective:** the T11 `PanelHost` (Files / Preview / Dev-runner) reachable from the cockpit - as a toggleable side surface or a tile type, matching how the webview exposes panels.
- **Note:** panels are DONE and demoed in the `panel-window` bin; this is pure mounting.
- **Files:** `chrome/view.rs` (mount point), `chrome/actions.rs` (toggle command + palette entries).
- **Acceptance:** file tree browse + fuzzy search + preview open against the live server from inside the cockpit.

**Parallelization:** N1+N4 as one crew assignment (both live in the header/dialog area of `chrome/view.rs`), N2+N3 as a second (both are layout gestures), N5 as a third.
Merge order: whoever is ready first; later merger owns `chrome/view.rs` conflicts (execution-guide §4 rule).

## 2. Lane F - the flip (serial, after Lane N)

Follows T14-PARITY §5 steps 4-5 verbatim:

1. Quiesce the webview tab reporter so native is the sole `report_workspace_tabs` writer (one-line gate or keep the Tauri window closed-to-tray); verify `list_tabs` truthfulness once.
2. Make the native client the daily default: Tauri app runs as server in tray, native is the cockpit (Start Menu shortcut exists as of 2026-07-05).
3. Daily-drive one week with a ZERO-fallback bar; log every fallback with its cause in this doc.
4. Tag the last webview-default commit for instant revert; freeze `apps/desktop/src` (UI) to bugfix-only.

## 3. Lane P - post-flip polish (T-C family, parallel with Lane D)

In priority order; sizes from T14-PARITY:

- **P1 Settings hub (M):** one native settings surface + persistence for the ~14 flags (sounds `THN_SOUND`, notifications `THN_NOTIFY`, fonts, ligatures, scrollback...). Kills the env-var gates from T-B.
- **P2 Theming (M-L):** the 23 chrome tokens + ANSI palette into one struct -> JSON at `~/.t-hub/`; port the 8 presets; subscribe to the server `theme://changed` event for cross-client sync; per-terminal palette overrides ride this.
- **P3 Crash-recovery review (M):** on relaunch after an unclean exit, offer per-session restore choices (webview parity, PRD §6.6).
- **P4 Rules engine + auto-continue (L):** port `store/rules.ts` status-transition automation and the usage-limit `autoContinueText`.
- **P5 Drag-drop file paths (M):** gpui drop handler + `toWslPath` translation -> paste into the focused terminal.
- **P6 Hooks management UI (S-M):** view/install/uninstall managed hooks (server already reconciles at boot).

## 4. Lane D - server split + distribution (the endgame)

Distribution is gated on the server outliving the Tauri shell ([SERVER-SPLIT-AND-ROADMAP.md](./SERVER-SPLIT-AND-ROADMAP.md) M1-M4).
M1 first slice is already shipped (loopback `control_client.rs`, Tee emitter, event forwarder).

- **D1 Finish M1 (L):** every remaining Tauri-IPC command the native client needs goes over the control socket; the server core builds as a standalone `t-hub-server` binary with no Tauri dependency.
- **D2 Server as a service (M):** standalone server start/stop/health; native client can launch it if absent (today the Tauri app must be running - this removes that).
- **D3 Installer + updater (M-L):** Velopack `Setup.exe` + portable zip for `t-hub-native` + `t-hub-server` (T14-PARITY §3 recommendation); CI release lane beside the existing Tauri one.
- **D4 Tray (S-M):** `tray-icon` crate + message-pump thread; Show / Reconnect / Quit.
- **D5 Signing (S, external):** Azure Artifact Signing enrollment ($9.99/mo), wire into the release lane.
- **D6 Retire the webview (S):** remove the Tauri UI, keep or fold the shell; the Tauri updater's last act is shipping the native installer.

## 5. Sequence summary

```
Lane N (3 parallel crews)  ->  Lane F flip + 1-week soak  ->  Lane P polish
                                                          \->  Lane D split + distribution
Lane P and Lane D run in parallel after the flip; D6 retires the webview last.
```

Rough shape: Lane N is days of crew work, not weeks.
The week-long Lane F soak is the schedule anchor.
P1/P2 are the first post-flip tasks because they remove the last "webview is nicer" reasons; D1-D3 are the long pole to a real installer.

## 6. Results log (append per task, execution-guide convention)

- (empty)
