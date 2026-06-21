# T-Hub — Dev build (side-by-side "T-Hub Dev")

> **Naming:** the project is **100% `t-hub`** (renamed 2026-06-20, rollback
> tag `pre-thub-rename`). Brand is `T-Hub`;
> internal tokens are `t-hub` (kebab) / `t_hub` (Rust idents, env vars
> `T_HUB_*`) / `t-hub-desktop` (the pnpm package, distinct from the root
> `t-hub`).

T-Hub ships in **two installable Windows variants that coexist on one machine**:

| | **T-Hub** (production) | **T-Hub Dev** (sandbox) |
|---|---|---|
| Windows app name (`productName`) | `T-Hub` | `T-Hub Dev` |
| Bundle identifier | `com.t-hub.app` | `com.t-hub.dev` |
| tmux socket | `t-hub` | `t-hub-dev` |
| MCP control channel | `~/.t-hub/control.json` | `~/.t-hub-dev/control.json` |
| diag log | `~/.t-hub/diag.log` | `~/.t-hub-dev/diag.log` |
| workspace DB + WebView2 data | `%APPDATA%\com.t-hub.app\…` | `%APPDATA%\com.t-hub.dev\…` |
| window title (frameless; alt-tab/taskbar) | `T-Hub` | `T-Hub Dev` |

**They are separate Windows apps.** Installing one never touches the other:
separate Start-menu entries, separate Add/Remove-Programs entries, separate
install dirs. You can run both at the same time.

**Installing a new T-Hub Dev replaces the previous T-Hub Dev** (same identifier
`com.t-hub.dev` → the NSIS installer recognizes it as the same app and upgrades
in place). Likewise a new T-Hub replaces the previous T-Hub. The two never
replace each other because their identifiers differ.

## What's isolated vs shared

**Isolated** (a dev experiment can NEVER disturb production):
- **tmux sessions** — dev runs on the `t-hub-dev` socket, so its terminals
  never appear in, and can never kill, your production sessions. *(This is the
  safety-critical one.)*
- **MCP control channel** + **diag log** — under `~/.t-hub-dev/`.
- **Workspace DB, cookies/WebView2 data** — Tauri keys its `app_data_dir` on
  the bundle identifier, so `com.t-hub.dev` gets its own directory automatically.

**Shared** (intentional / harmless):
- **App theme** (`~/.config/t-hub/theme.json`) — both variants read the same
  theme. The ~6 themes are stock (compiled in), so nothing is lost; only the
  active selection / any hand-tweaks live in that file.
- **Recents** — derived from your actual `~/.claude` session transcripts (your
  real Claude/Codex sessions), not a T-Hub-owned store.

## How it's built

The variant is a **compile-time Cargo feature** (`devbuild`) plus a tiny Tauri
**config overlay** (`apps/desktop/src-tauri/tauri.dev.conf.json`). The prod
build is byte-for-byte unchanged — `pnpm tauri build` with no flags.

- **Feature `devbuild`** (`apps/desktop/src-tauri/Cargo.toml`): the only thing it
  changes is `apply_devbuild_isolation()` in `src/lib.rs`, which — *before any
  `T_HUB_*`-backed path is first read* — sets `T_HUB_TMUX_SOCKET=t-hub-dev`,
  `T_HUB_CONTROL_FILE=~/.t-hub-dev/control.json`,
  `T_HUB_DIAG_FILE=~/.t-hub-dev/diag.log` (each only if not already overridden).
  It also sets the window title to "T-Hub Dev". No path code was refactored — it
  reuses env hooks that already exist in `tmux.rs`, `control.rs`, and `diag.rs`.
- **Overlay** `tauri.dev.conf.json`: deep-merges over `tauri.conf.json`, changing
  only `productName` → `T-Hub Dev` and `identifier` → `com.t-hub.dev`.

### CI (GitHub Actions) — the normal path

`release.yml` has a `workflow_dispatch` input `variant` (default **dev**):

```bash
# DEV installer (default) → artifact "t-hub-dev-installer"
gh workflow run release.yml --ref main -f variant=dev

# PROD installer → artifact "t-hub-prod-installer"
gh workflow run release.yml --ref main -f variant=prod
```

Tag pushes (`v*`) always build **prod** and publish a release. Manual dispatch
produces a downloadable artifact only (no public release / no `latest.json`).
Download with `gh run download <run-id> -n t-hub-dev-installer -D <dir>`.
The dev installer is named `T-Hub Dev_<version>_x64-setup.exe`.

### Local build (Windows host only)

```bash
cd apps/desktop
pnpm tauri build -f devbuild --config src-tauri/tauri.dev.conf.json   # dev
pnpm tauri build                                                       # prod
```

### Local hot-reload dev instance (WSLg/Linux) — unchanged

The fast iteration loop is still the WSLg instance, which isolates via the same
env hooks:

```bash
cd apps/desktop
T_HUB_TMUX_SOCKET=t-hub-dev pnpm tauri dev
```

Note: WSLg can't exercise Windows-only features (OS file-drop, clipboard-image,
true frameless titlebar) — for those, install the **T-Hub Dev** Windows build.

## Caveat: the updater

Both variants carry the same `plugins.updater` config (endpoint → the prod
repo's `latest.json`). Since dispatch builds publish no `latest.json`, the dev
app's update check is a no-op today. If a prod `v*` release is ever published,
the dev app *could* offer to "update" to it — but because prod's identifier
differs (`com.t-hub.app`), that would install prod **alongside** dev rather than
replacing it. Disable/repoint the updater in the overlay later if it gets noisy.

---

## Canonical `t-hub` identifiers (reference)

| Thing | Value |
|---|---|
| Brand / window title / tray | `T-Hub` |
| Prod bundle id | `com.t-hub.app` |
| Dev bundle id | `com.t-hub.dev` |
| Cargo app crate / lib | `t-hub` / `t_hub_lib` |
| Sub-crates | `t-hub-protocol`, `t-hub-agent`, `t-hub-mcp` |
| pnpm packages | root `t-hub`, desktop `t-hub-desktop`, site `t-hub-site` |
| tmux socket (prod / dev) | `t-hub` / `t-hub-dev` |
| MCP server name | `t-hub` (tools are `mcp__t-hub__*`) |
| State dir (prod / dev) | `~/.t-hub` / `~/.t-hub-dev` |
| Workspace DB | `t-hub.db` (under `app_data_dir`) |
| Hook marker (in `~/.claude/settings.json`) | `__t_hub_managed__` |
| Env hooks | `T_HUB_TMUX_SOCKET`, `T_HUB_CONTROL_FILE`, `T_HUB_DIAG_FILE`, `T_HUB_DB_NAME`, `T_HUB_DISTRO`, `T_HUB_AGENT_BIN` |
| GitHub repo | `n8watkins/t-hub` |
