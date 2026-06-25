# T-Hub — Performance / Memory Audit (freeze root-cause)

**Status:** Findings from a 4-agent read-only audit. **Symptom:** under increased usage T-Hub (and *only* T-Hub) freezes and shows random RAM spikes, while the rest of Windows stays responsive. **Conclusion:** the freeze is **app-level** (backend Tokio worker-thread exhaustion + frontend main-thread saturation), not OS/WSL memory starvation. Grounded in `file:line`.

## Root cause — two converging mechanisms

### A. Backend: blocking `wsl.exe` spawns exhaust the Tokio worker pool (the freeze)
Most `#[tauri::command] async fn`s do **synchronous blocking work directly on the Tokio executor** (only `usage`/`codex`/`recent`/`db`/`dropin` use `spawn_blocking`). On Windows each `wsl.exe` spawn does `.output()` — it *waits for the child*, pinning a worker thread for the WSL cold-start (tens–hundreds of ms). The default pool ≈ #cores, so a burst saturates every worker → the UI's IPC calls queue behind them → **freeze**. It scales with tile count ("increased usage") and bursts on window focus (all pollers re-fire) → the **RAM/handle spike**.

| Command | Spawns | Cadence | File |
|---|---|---|---|
| `git_info` | **6 sequential `wsl.exe`** | per tile, every 5s + focus | `git.rs:268`, `Tile.tsx:79` |
| `list_terminals` | 2 `wsl.exe` | every 5s + focus | `commands.rs:150`, `Canvas.tsx:190` |
| `host_metrics` | blocking agent round-trip (≤10s) | **every 4s** | `commands_05.rs:45`, `telemetry.ts:102` |
| `claude_usage` | up to **3×** nested `script→wsl→shell→claude -p /usage` | 5min + focus | `usage.rs:43,58` |
| `search_files`/`index_project` (cold) | `wsl.exe rg --files` whole repo | search-as-you-type | `files.rs:1088,1070` |

`git_info` is the dominant offender: **6 × tiles spawns every 5s** (8 tiles ≈ 10 wsl.exe/sec, none off-executor).

### B. Frontend: every output chunk does decode + ANSI-strip + regex on the one JS thread (the freeze)
The pool keeps **every terminal in every tab live + attached** (`TerminalPool.tsx:208`), so every busy terminal's `onOutput` runs synchronously on the WebView2 JS thread (`Terminal.tsx:672`): `decodeBase64` (char-by-char, `client.ts:135`) → `stripAnsi` regex over the whole rolling buffer + `matchAll` URL scan (`Terminal.tsx:646`) → `term.write`. **No batching, no backpressure, DOM renderer (no WebGL), scrollback 20000.** Many busy terminals → decode/render storm → freeze. Status events add O(tiles+rows) selector/regex work per snapshot (`supervision.ts`, `clientType.ts:48`).

## Memory growth (the slow creep + spikes)
- **Backend, never evicted:** `Supervisor.sessions` + `SessionEntry.children` (per session + per subagent, forever; tree is O(children)-cloned + re-emitted on every journal event — `supervision.rs:82,42`); `StatusBridge.latest` (per session, `status.rs:191`). Driven by the 5s statusline stream.
- **Frontend, never evicted:** `supervision.ts` `remove()` is **dead code (0 callers)** → `trees/statuses/snapshots/sessionIdByTmux` grow per session id (fresh UUID per spawn/resume); `sessionContext.ts` has **no prune method at all**; `DevTab` leaks a listener + 2 map entries per terminal; `activity`/`labels`/`userLabels`/`claudeTitles` not pruned on close. The single close-cleanup funnel is `workspace.ts:30 cleanupTileSideState` — extend it to cover all of these.
- **Spikes:** the focus-time `wsl.exe` burst (git×tiles + list_terminals + usage's nested process tree + recent + codex all at once).

## Confirmed healthy (don't touch)
PTY/thread lifecycle (joined on close, `pty.rs`); tmux client cleanup; the new transition log (capped 256); `recent_sessions` (15s TTL + spawn_blocking); control/MCP path (no lock across slow I/O); `wait_for_status` (sleeps outside the lock). Re-enabling WebGL is **out** — it caused the blank-grid bug this build exists to fix.

## Fix plan (prioritized)

### Tier 1 — the freeze ✅ DONE (`8e12fbf`, `b17d922`, `a15416f`)
1. ✅ **`spawn_blocking` the per-poll commands** — `git_info`, `list_terminals`, `host_metrics`, cold `search_files`/`index_project`. Stops Tokio-worker pinning. *(backend)*
2. ✅ **Collapsed `git_info` to one `wsl.exe`** (single `bash -lc` script) **+ per-cwd ~3.5s TTL cache**. Kills the dominant spawn storm. *(backend)*
3. ✅ **Coalesced terminal output** — `onOutput` decodes+enqueues, one rAF flush per frame; `stripAnsi`/URL-scan + activity bump run once per flush; faster base64 decode. *(frontend)*

Verified: cargo build + 159 lib tests + tsc all green. Runtime smoke-test still pending.

### Tier 2 — RAM growth
4. **Evict ended sessions** from `Supervisor.sessions` + `StatusBridge.latest`; **prune completed children**; **self-reap exited terminals** from `TerminalManager` on reader EOF. *(backend)*
5. **Wire the dead `supervision.remove()`** + add `sessionContext.forget()` + fix `DevTab`/`activity`/label maps, all via `cleanupTileSideState`. *(frontend)*

### Tier 3 — smaller
6. Drop `claude_usage` to 1–2 attempts / longer backoff; gate `tlog` behind a debug flag (per-frame `console.log` + `invoke`); consider throttling output processing for hidden-tab terminals.

## Idea — tray "recovery / WSL" submenu (backlog)
A tray affordance to recover from a wedged state without a full reboot, at increasing granularity: **reconnect agent bridge** (cheap) → **restart the `t-hub` tmux server** (kills only T-Hub's terminals) → **reclaim WSL memory now** (`drop_caches`) → **full `wsl --shutdown`** (confirm-gated, nukes all WSL). Useful as a safety hatch; the better fix is removing the *need* for it via Tier 1–2.

## Note on `.wslconfig`
Lowered the WSL cap 24→20 GB (leaves Windows ~11 GB) and added `autoMemoryReclaim=gradual` + `sparseVhd=true`. `memory=` is a *ceiling*, not a reservation; the real "don't hold it all the time" lever is `autoMemoryReclaim`. This is hygiene/headroom — it does **not** fix the app-level freeze (Tier 1 does). Applies on next `wsl --shutdown`.
