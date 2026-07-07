# T-Hub - Session Handoff

**Last updated:** 2026-07-07 (Step 4 wave landed, Phase 3 default reverted after an incident) - **Branch:** `main`, fully pushed - **App version on main:** `0.3.47` + the Phase 3 default-off fix on top. **Installed (prod):** `0.3.46` (rolled back - see incident below).

> INCIDENT + ROLLBACK (2026-07-07): 0.3.47 was built + installed, and the Phase 3 default-ON flip BROKE terminal attach for the whole app - every session showed "session detached - reconnecting". ROOT CAUSE: the app's OWN frontend authenticates to the control socket with the token published in `control.json`; Phase 3 downgraded that published token to read-only, so the webview lost control and could not attach terminals. The crew's blast-radius map covered external callers (MCP/CLI/probes) but MISSED the frontend as a consumer of the published token. FIX: rolled prod back to the 0.3.46 installer (`T-Hub_0.3.46_x64-setup.exe`, sessions preserved, terminals healthy again), and reverted the Phase 3 DEFAULT back to OFF on main (branch `fix-phase3-default-off`) - Phase 3 infra stays behind `T_HUB_CONTROL_HARDEN=1`, dormant, until the frontend gets the control token via a trusted internal channel (a Tauri command or spawn-injected env) rather than scraping `control.json`. THAT frontend-token work is the real Phase 3 prerequisite - do it before ever re-enabling the flag.

> STEP 4 STATUS (2026-07-07): (a) socket-gate Phase 3 - LANDED (PR #25) but DEFAULT REVERTED to OFF after the incident above; safe/dormant behind the flag. (b) Cortana UI parity - orchestrator renders as "Cortana" through a shared `AgentRow` (full captain status parity) with a crown icon (PR #26) - GOOD, not implicated. (c) stray placeholder fixed - duplicate reserved Captains tab from the registry echo path, filtered in `adoptRegistry` + self-healed in `ensureReservedCaptainsTab` (PR #27) - GOOD, not implicated. (d) NEW item from the general (captains showing as un-closeable BOTTOM workspace tiles; should be top agents list only) - crew was mid-flight (`fix-captain-workspace-dupes` worktree) when the process exited; NOT landed, re-staff. NEXT: build a "T-Hub Dev" (`com.t-hub.dev`, side-by-side) from the fixed sources so the general can live-view (b)+(c) against the same agents; re-staff (d); once green, cut a clean prod build (0.3.48) with Phase 3 default-off.

> Zero-context handoff. Read this in full, plus [CAPTAIN-CHAT-PHASES.md](./CAPTAIN-CHAT-PHASES.md) and the orders at `~/.t-hub/captain/orders/t-hub-2026-07-06.md`.
> Ship identity: you are captain of ship **t-hub** (ship file `~/.t-hub/captain/ships/t-hub.md`, captain terminal `e4348ddf`). The old "t-hub-native" slug is retired; the native pivot is archived.
> Working dir: `/home/natkins/projects/tools/t-hub/t-hub-app`.

---

## 1. What this is

T-Hub is a Tauri 2 desktop command-center for running and supervising many Claude Code / Codex agent sessions.
Rust backend (`apps/desktop/src-tauri`) + React/TS/Tailwind frontend (`apps/desktop/src`) with xterm.js terminals.
On Windows it drives WSL tmux (`tmux -L t-hub`, sessions `th_<id>`); a loopback control socket (`control.rs`, handshake `~/.t-hub/control.json`) is the spine every crew/captain/orchestrator/MCP command flows through.

## 2. What landed this wave (0.3.40 -> 0.3.45, all on main)

- **Multi-captain + sidebar** (earlier): captain list/switcher, CAPTAINS sidebar section, identity rows.
- **Voice** (0.3.41-0.3.43): Settings > Voice (engine selector), **Kokoro** TTS added as a selectable engine (local server `~/projects/extensions/kokoro-tts`, `127.0.0.1:7478`, 54 voices, detached from crews - restart via its `start.sh`); Piper still on `7477`. Announce-on-attention with a Scribe voice-gate (holds announcements while the general dictates - Scribe writes `%LOCALAPPDATA%\com.natkins.scribe\status.json`, T-Hub reads it, `scribe_status` MCP tool). Voice-title bug fixed (speaks the captain identity, not the typed input).
- **Identity fix** (0.3.44, PR #19): `stableCaptainIdentity` prefers cwd basename over tab name (fixed captains all showing "appturnity" when sharing a tab).
- **Captains surface** (0.3.44 -> 0.3.45): built a custom deck (PR #20), then RETIRED it per the general's correction (PR #22) in favor of a **reserved "Captains" workspace tab** (id `captains-reserved`) - a normal workspace tab holding captain + orchestrator tiles as ordinary terminals, kept out of the work tabs; net -494 lines. Polish (PR #24): last-work-tab close guard + keep work spawns out of Captains.
- **Orchestrator** (Cortana): a `fleet-orchestrator` skill session (terminal `e05764f5`, cwd `~/.t-hub/orchestrator`, ship slug `fleet`, file `~/.t-hub/captain/ships/fleet.md`), **adopt-only** (T-Hub adopts + designates an existing orchestrator on launch; NO permissionless auto-spawn - an adversarial audit killed auto-spawn as premature at 1-ship scale). It commands captains via the raw control socket. General drives it directly; it relays orders to captains (this handoff came through it).
- **SECURITY - socket-gate** (design `docs/SOCKET-AUTH-DESIGN.md`):
  - **Phase 1** (PR #21, on 0.3.45): fleet spawn governor (concurrent cap 64 from live tmux, spawn rate 20/min burst 8, hard ceiling 128, destructive throttle 15/min) + tamper-evident audit log (`~/.t-hub/audit/`, `send_text` content redacted to a hash, hash-chained). Refuses runaway/injection fan-out; normal orchestration bursts pass.
  - **Phase 2 + 2b** (PR #23, merged - in 0.3.46): capability-scoped tokens. `control.json` `token` = full-power `control_token` (backward-compatible; every current caller keeps working); a `read_token` (Read tier only) added. Gate at `dispatch_authenticated` maps token -> capability -> command tier. Elevate-by-default (crews keep control unless explicitly opted read-only, fail-safe to control). Remote (Tailscale) peers capped to read. `create_worktree`/`remove_worktree` are control-tier.
  - **Phase 3** (PARKED - built, flag OFF): `T_HUB_CONTROL_HARDEN` env, default false. Flipping it stops publishing `control_token` to `control.json` so the control capability only flows via the spawn-tree env injection - closes the "scrape control.json -> full power" hole. **Step 4a below is to do Phase 3 in this fresh context.**

## 3. PARKED / next steps (Step 4 of the orders - resume here)

Per `~/.t-hub/captain/orders/t-hub-2026-07-06.md`, after the 0.3.46 install + fresh context, delegate these to crew worktrees (reproduce E2E first, review, test, merge, bump):

- **(a) Socket-gate Phase 3** - implement/flip the final lockdown (stop publishing the full-power token; control capability only via the spawn tree). It is built behind `T_HUB_CONTROL_HARDEN` (default off). The general green-lit doing it now. Verify probes/MCP/app all still spawn once the flip is on (the app must inject `control_token` into elevated sessions - Phase 2b - for this to keep working).
- **(b) Cortana (orchestrator) UI parity** - in the app, rename the "orchestrator" entity to **Cortana**. Give it the SAME status/sidebar treatment as the captains (a peer entry in the sidebar / Captains tab with the same live status/context display captains have) PLUS a special icon marking it as the orchestrator. Cortana's live terminal is `e05764f5`. The `orchestratorId` designation + `ensureOrchestrator` adopt-only already exist in `store/captain.ts` - build the naming + icon + status-parity on that.
- **(c) Workspace stray-terminal bug** - repro: creating a NEW workspace shows a "new terminal" placeholder on the back of the workspace even when that workspace already has terminals. Expected: do not show the placeholder when terminals already exist. Reproduce E2E, fix, land.

Also parked (lower priority): the deck's one-click "start orchestrator" affordance for the rare cold-start (deferred since the orchestrator persists in tmux).

## 4. Conventions and gotchas (hard-won)

- **Version bump every code commit** (`apps/desktop/scripts/bump-version.sh`, then `cargo check` in `src-tauri` to sync `Cargo.lock` - never hand-edit the lock). One bump per landed change; the captain/orchestrator bumps at merge, crews never bump.
- **Local Windows build**: clone at `C:\Users\natha\projects\Tools\t-hub\t-hub-app` - do NOT `git merge` into it; rsync `apps/desktop/src` + `src-tauri/src` + Cargo.toml/lock + package.json over it, `sed` its `tauri.conf.json` version, then `powershell.exe ... pnpm tauri build`. Output `...\bundle\nsis\T-Hub_<v>_x64-setup.exe`; install `/S`; relaunch `%LOCALAPPDATA%\T-Hub\t-hub.exe`. Verify "Finished 1 bundle". Sanity-grep a new symbol in the synced sources before trusting the installer.
- **Session-kill-on-install regression**: the 0.3.40 install killed 5 of 7 tmux sessions; later installs (0.3.41-0.3.45) preserved ALL sessions (attached ones survive). Still a latent risk - this handoff exists because the install may kill the captain session.
- **Crew orchestration**: spawn via the control socket `create_worktree` (reference client `scripts/probes/t1_lib.py`); crews run `claude --dangerously-skip-permissions --model opus` (the auto-mode classifier requires the general's fresh authorization per launch - it will block, ask the general). Crew brief ends with `touch /tmp/t-hub-crew-done/<ship>/<name>.done`; captain arms a background watcher over that dir. Review pattern: focused Opus finder agents + adversarial verify before presenting; fix rounds on the same branch.
- **Fable model is walled** (usage credits) - crews + subagents run **Opus** (`--model opus`, `/model opus`, `model: "opus"`).
- **Sparse/vertical tmux render** on some crew tiles is a WSLg capture glitch, not a stuck crew - verify via `tmux capture-pane` process check, not the garbled pane.

## 5. Key files

- `apps/desktop/src/store/captain.ts` - orchestratorId designation, ensureOrchestrator adopt-only, captain pins.
- `apps/desktop/src/store/workspace.ts` - reserved Captains tab (`CAPTAINS_TAB_ID`), tile placement (moveTileToCaptainsTab / placeWorkTile), tab close guards.
- `apps/desktop/src/components/CaptainsList.tsx`, `Sidebar.tsx` - the agents/captains sidebar list (where Cortana status parity + icon go, Step 4b).
- `apps/desktop/src/components/CaptainOverlay.tsx` - `stableCaptainIdentity`, status dot.
- `apps/desktop/src-tauri/src/control.rs` - the control socket, capability gate (`dispatch_authenticated`, `resolve_capability`, `required_tier`), `T_HUB_CONTROL_HARDEN` Phase 3 flag.
- `apps/desktop/src-tauri/src/governor.rs`, `audit.rs` - Phase 1 spawn budget + audit log.
- `docs/SOCKET-AUTH-DESIGN.md` - the full socket-auth design (Phases 1-3).
- `~/.t-hub/captain/ships/t-hub.md` - this ship's roster + landed log. `~/.t-hub/captain/ships/fleet.md` - Cortana's roster.

## 6. Kickoff prompt for the next context

> You are the captain of ship **t-hub** (terminal `e4348ddf`), resuming after a `/clear`. Read `docs/HANDOFF.md` in full, then `~/.t-hub/captain/orders/t-hub-2026-07-06.md`. State: main at 0.3.45 + PR #23/#24 (Phase 2 tokens + polish) merged; 0.3.46 is being/was installed. Resume Step 4: delegate to crew worktrees, reproduce E2E first, review + test + merge + bump each, and report each land back to Cortana (orchestrator, terminal `e05764f5`): **(a)** socket-gate Phase 3 - implement/flip the `T_HUB_CONTROL_HARDEN` final token lockdown (built, flag-off); **(b)** rename the orchestrator entity to **Cortana** in the app with the same sidebar/status treatment as captains plus a special orchestrator icon (Cortana terminal `e05764f5`); **(c)** fix the new-workspace stray "new terminal" placeholder bug (don't show it when the workspace already has terminals). One warm crew may exist at `0d179f8e` (worktree `.claude/worktrees/anchor-dropdown-portal`); the orchestrator `e05764f5` lives in the reserved Captains tab.
