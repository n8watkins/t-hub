# T-Hub Dev build

> **Naming:** The project is `t-hub`, the product brand is `T-Hub`, Rust identifiers use `t_hub`, and environment variables use the `T_HUB_*` prefix.

T-Hub ships as two installable Windows variants that can run on the same machine.

| Surface | T-Hub production | T-Hub Dev |
| --- | --- | --- |
| Windows app name | `T-Hub` | `T-Hub Dev` |
| Bundle identifier | `com.t-hub.app` | `com.t-hub.dev` |
| tmux socket | `t-hub` | `t-hub-dev` |
| T-Hub state root | `~/.t-hub` | `~/.t-hub-dev` |
| MCP control channel | `~/.t-hub/control.json` | `~/.t-hub-dev/control.json` |
| Agent journal | `~/.t-hub/journal` | `~/.t-hub-dev/journal` |
| Cortana working directory | `~/.t-hub/orchestrator` | `~/.t-hub-dev/orchestrator` |
| Windows app data and WebView data | `%APPDATA%\com.t-hub.app\...` | `%APPDATA%\com.t-hub.dev\...` |
| Workspace database | `t-hub.db` | `t-hub-dev.db` |
| Updater endpoint | Production release manifest | Disabled |

Installing a new T-Hub Dev build replaces only the previous T-Hub Dev installation.
Installing a production build replaces only the previous production installation.
The distinct bundle identifiers provide separate install directories, Start menu entries, application data, WebView profiles, and uninstall records.

## Isolation contract

The `devbuild` Cargo feature applies isolation before any lazy runtime path is resolved.
Each development default is installed only when the operator has not supplied an explicit environment override.
Production builds do not run this setup and retain their historical defaults.

The following T-Hub-owned mutable surfaces are isolated:

- tmux sessions use the `t-hub-dev` socket.
- The MCP handshake, diagnostics log, Captain registry, identity store, authorization store, and delegated administration grants live under `~/.t-hub-dev`.
- Durable inbox queues, audit logs, control keys, read keys, voice settings, and Powder compatibility profiles live under `~/.t-hub-dev`.
- The agent bridge and every tmux-launched agent inherit `T_HUB_AGENT_JOURNAL_DIR=.t-hub-dev/journal`.
- Relative agent journal paths are resolved against the WSL user's home directory.
- Cortana uses `~/.t-hub-dev/orchestrator` and development Crew use `~/.t-hub-dev/crew-gh-empty` for the empty GitHub CLI profile.
- Theme and portable workspace layout files use `~/.t-hub-dev/config` through `T_HUB_CONFIG_DIR`.
- The workspace database uses the development filename inside the already separate development application data directory.
- The development bundle identifier isolates frontend local storage, cookies, cache, and WebView data.
- Automatic Claude settings reconciliation is disabled in development builds.
- The development updater has no endpoints and cannot consume a production release manifest.

The concrete environment defaults are:

| Variable | Development default |
| --- | --- |
| `T_HUB_TMUX_SOCKET` | `t-hub-dev` |
| `T_HUB_CONTROL_FILE` | `~/.t-hub-dev/control.json` |
| `T_HUB_DIAG_FILE` | `~/.t-hub-dev/diag.log` |
| `T_HUB_CAPTAINS_FILE` | `~/.t-hub-dev/captains.json` |
| `T_HUB_IDENTITIES_FILE` | `~/.t-hub-dev/identities.json` |
| `T_HUB_AUTHORIZATIONS_FILE` | `~/.t-hub-dev/authorizations.json` |
| `T_HUB_DELEGATED_ADMIN_FILE` | `~/.t-hub-dev/delegated-admin.json` |
| `T_HUB_INBOX_DIR` | `~/.t-hub-dev/inbox` |
| `T_HUB_AUDIT_DIR` | `~/.t-hub-dev/audit` |
| `T_HUB_SERVER_KEY_FILE` | `~/.t-hub-dev/server-key` |
| `T_HUB_SERVER_READ_KEY_FILE` | `~/.t-hub-dev/server-read-key` |
| `T_HUB_VOICE_FILE` | `~/.t-hub-dev/voice.json` |
| `T_HUB_POWDER_PROFILES_FILE` | `~/.t-hub-dev/powder-profiles.json` |
| `T_HUB_CONFIG_DIR` | `~/.t-hub-dev/config` |
| `T_HUB_DB_NAME` | `t-hub-dev.db` |
| `T_HUB_AGENT_JOURNAL_DIR` | `.t-hub-dev/journal` relative to WSL home |
| `T_HUB_CORTANA_HOME` | `.t-hub-dev/orchestrator` relative to WSL home |

## Intentionally shared external surfaces

Development T-Hub still operates real coding harnesses and real project repositories.
These external surfaces remain shared by design:

- Recents are read from the user's actual Claude and Codex transcripts.
- Claude and Codex provider configuration, credentials, and session stores remain available to the harnesses they launch.
- Scribe control and status sources are observed read-only.
- User-selected repositories and worktrees are real targets of explicit terminal and product actions.
- Explicit Claude hook install or uninstall actions still operate on the user's selected Claude configuration and require their existing consent gates.
- Explicit environment overrides can point a development process at another location and therefore transfer responsibility to the operator.

No automatic development startup path writes `~/.claude/settings.json`.

## How it is built

The variant combines the compile-time Cargo feature `devbuild` with `apps/desktop/src-tauri/tauri.dev.conf.json`.
The overlay changes the product name, bundle identifier, and updater endpoints.
The production build uses neither the feature nor the overlay.

### CI

The release workflow accepts a `variant` input and defaults manual dispatches to development.

```bash
# Build the development installer.
gh workflow run release.yml --ref main -f variant=dev

# Build the production installer.
gh workflow run release.yml --ref main -f variant=prod
```

Tag pushes matching `v*` always build production and publish a release.
Manual dispatch produces a downloadable artifact without publishing `latest.json`.

```bash
gh run download <run-id> -n t-hub-dev-installer -D <dir>
```

The development installer is named `T-Hub Dev_<version>_x64-setup.exe`.

### Local Windows build

```bash
cd apps/desktop
pnpm tauri build -f devbuild --config src-tauri/tauri.dev.conf.json
```

The unchanged production command is:

```bash
cd apps/desktop
pnpm tauri build
```

### Local WSLg hot reload

The WSLg development loop must also compile the `devbuild` feature and load the overlay.

```bash
cd apps/desktop
pnpm tauri dev -f devbuild --config src-tauri/tauri.dev.conf.json
```

WSLg cannot exercise Windows-only behavior such as operating-system file drop, clipboard images, or the native frameless title bar.
Install the T-Hub Dev Windows build for those checks.

## Canonical identifiers

| Thing | Value |
| --- | --- |
| Production brand and window title | `T-Hub` |
| Development brand and window title | `T-Hub Dev` |
| Production bundle identifier | `com.t-hub.app` |
| Development bundle identifier | `com.t-hub.dev` |
| Cargo app crate and library | `t-hub` and `t_hub_lib` |
| Subcrates | `t-hub-protocol`, `t-hub-agent`, `t-hub-mcp` |
| pnpm packages | root `t-hub`, desktop `t-hub-desktop`, site `t-hub-site` |
| MCP server name | `t-hub` |
| Hook marker | `__t_hub_managed__` |
| Repository | `n8watkins/t-hub` |
