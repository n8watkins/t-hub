# T-Hub — Dev build (side-by-side "T-Hub Dev")

T-Hub ships in **two installable Windows variants that coexist on one machine**:

| | **T-Hub** (production) | **T-Hub Dev** (sandbox) |
|---|---|---|
| Windows app name (`productName`) | `T-Hub` | `T-Hub Dev` |
| Bundle identifier | `com.termhub.dev` | `com.termhub.devbuild` |
| tmux socket | `termhub` | `termhub-dev` |
| MCP control channel | `~/.termhub/control.json` | `~/.termhub-dev/control.json` |
| diag log | `~/.termhub/diag.log` | `~/.termhub-dev/diag.log` |
| workspace DB + WebView2 data | `%APPDATA%\com.termhub.dev\…` | `%APPDATA%\com.termhub.devbuild\…` |
| window title (frameless; alt-tab/taskbar) | `T-Hub` | `T-Hub Dev` |

**They are separate Windows apps.** Installing one never touches the other:
separate Start-menu entries, separate Add/Remove-Programs entries, separate
install dirs. You can run both at the same time.

**Installing a new T-Hub Dev replaces the previous T-Hub Dev** (same identifier
`com.termhub.devbuild` → the NSIS installer recognizes it as the same app and
upgrades in place). Likewise a new T-Hub replaces the previous T-Hub. The two
never replace each other because their identifiers differ.

## What's isolated vs shared

**Isolated** (a dev experiment can NEVER disturb production):
- **tmux sessions** — dev runs on the `termhub-dev` socket, so its terminals
  never appear in, and can never kill, your production sessions. *(This is the
  safety-critical one.)*
- **MCP control channel** + **diag log** — under `~/.termhub-dev/`.
- **Workspace DB, theme-per-window state, cookies/WebView2 data** — Tauri keys
  its `app_data_dir` on the bundle identifier, so `com.termhub.devbuild` gets
  its own directory automatically.

**Shared** (intentional / harmless):
- **App theme** (`~/.config/termhub/theme.json`) — both variants read the same
  theme. Editing the theme in dev also changes production's theme. Low-harm;
  not isolated to keep the change small. (Set `XDG_CONFIG_HOME` differently per
  launch if you ever need them split — but don't set it process-wide for the
  packaged app, since spawned WSL/Claude children would inherit it.)
- **Recents** — derived from your actual `~/.claude` session transcripts, i.e.
  your real Claude/Codex sessions. Both variants surface the same list because
  they *are* your sessions, not a T-Hub-owned store.

## How it's built

The variant is a **compile-time Cargo feature** (`devbuild`) plus a tiny Tauri
**config overlay** (`apps/desktop/src-tauri/tauri.dev.conf.json`). The prod
build is byte-for-byte unchanged — `pnpm tauri build` with no flags.

- **Feature `devbuild`** (`apps/desktop/src-tauri/Cargo.toml`): the only thing it
  changes is `apply_devbuild_isolation()` in `src/lib.rs`, which — *before any
  `TERMHUB_*`-backed path is first read* — sets `TERMHUB_TMUX_SOCKET=termhub-dev`,
  `TERMHUB_CONTROL_FILE=~/.termhub-dev/control.json`,
  `TERMHUB_DIAG_FILE=~/.termhub-dev/diag.log` (each only if not already
  overridden). It also sets the window title to "T-Hub Dev". No path code was
  refactored — it reuses env hooks that already existed in `tmux.rs`,
  `control.rs`, and `diag.rs`.
- **Overlay** `tauri.dev.conf.json`: deep-merges over `tauri.conf.json`, changing
  only `productName` → `T-Hub Dev` and `identifier` → `com.termhub.devbuild`.

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

The fast iteration loop is still the WSLg instance, which already isolates via
the same env hooks (it predates this feature):

```bash
cd apps/desktop
TERMHUB_TMUX_SOCKET=termhub-dev pnpm tauri dev
```

Note: WSLg can't exercise Windows-only features (OS file-drop, clipboard-image,
true frameless titlebar) — for those, install the **T-Hub Dev** Windows build.

## Caveat: the updater

Both variants carry the same `plugins.updater` config (endpoint → the prod
repo's `latest.json`). Since dispatch builds publish no `latest.json`, the
dev app's update check is a no-op today. If a prod `v*` release is ever
published, the dev app *could* offer to "update" to it — but because prod's
identifier differs (`com.termhub.dev`), that would install prod **alongside**
dev rather than replacing it. Disable/repoint the updater in the overlay later
if this becomes noisy.

---

## Every `termhub` identifier in the project (and why it stays)

You asked whether "termhub" still appears anywhere. **User-visible brand
strings are all "T-Hub"** (window title, productName, tray, all in-app panel
text, the marketing site). What remains as `termhub` is **technical plumbing,
kept on purpose** — renaming any of these would break the things noted:

| Identifier | Where | Why it stays `termhub` |
|---|---|---|
| `com.termhub.dev` | `tauri.conf.json` (prod bundle id) | Changing it breaks NSIS in-place upgrade matching for existing installs. |
| `com.termhub.devbuild` | `tauri.dev.conf.json` (dev bundle id) | Distinct-but-stable so dev installs replace each other; must not collide with prod. |
| crate `termhub` / lib `termhub_lib` | `Cargo.toml` | Rust package name; cosmetic-only, churns every path. |
| `termhub-protocol`, `termhub-agent`, `termhub-mcp` | `src-tauri/crates/*` | Workspace crate names + the WSL agent binary + MCP binary. |
| `tmux -L termhub` / `-L termhub-dev` | `tmux.rs` | The isolated tmux socket; the WSL-side server key. |
| MCP server key `termhub` | `.mcp.json` | The registered MCP server name Claude Code calls. |
| `~/.termhub/` (`~/.termhub-dev/`) | `control.rs`, `diag.rs` | Control-channel + diag-log home. |
| `%APPDATA%\com.termhub.*\termhub.db` | `db.rs` (`TERMHUB_DB_NAME`) | Workspace SQLite DB filename. |
| `__termhub_managed__` | `claude/hooks.rs` | Marker in `~/.claude/settings.json`; uninstall/idempotency keys on it. |
| `th_<terminalId>` | tmux session names | Per-terminal session prefix. |
| `TERMHUB_TMUX_SOCKET`, `TERMHUB_CONTROL_FILE`, `TERMHUB_DIAG_FILE`, `TERMHUB_DB_NAME`, `TERMHUB_DISTRO` | various | Env hooks (the dev variant rides these). |
| `n8watkins/t-hub` repo, `termhub-site.vercel.app`, npm `termhub-site` | git remote, site | Repo was renamed to `t-hub`; the Vercel/npm names are deploy identifiers (GitHub redirects the old URLs). |
| Code comments / docstrings mentioning "TermHub" | throughout | Developer-facing prose, not shown to users. |

**Bottom line:** there is no user-*visible* "TermHub" left; every remaining
`termhub` is an internal identifier that is deliberately stable.
