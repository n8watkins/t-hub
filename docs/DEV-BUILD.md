# T-Hub Dev build

> **Naming:** The project is `t-hub`, the product brand is `T-Hub`, Rust identifiers use `t_hub`, and environment variables use the `T_HUB_*` prefix.

T-Hub ships as two installable Windows variants that can run on the same machine.

| Surface | T-Hub production | T-Hub Dev |
| --- | --- | --- |
| Windows app name | `T-Hub` | `T-Hub Dev` |
| Main executable and process | `t-hub.exe` | `t-hub-dev.exe` |
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
The distinct main executable names ensure that development install, update, and uninstall checks never target the production process.

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
- The development main binary is `t-hub-dev.exe`, and every NSIS process check, payload, shortcut, and uninstall reference must resolve to that name.
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
The overlay changes the product name, main binary name, bundle identifier, and updater endpoints.
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

The development build is split into compile and bundle stages so the unpatched raw binary can be retained for bundle-marker provenance checks.

```powershell
cd apps/desktop
$releaseDir = "src-tauri/target/release"
@(
  "$releaseDir/bundle/nsis",
  "$releaseDir/nsis",
  "$releaseDir/dev-installer-extracted",
  "$releaseDir/dev-installer-evidence",
  "$releaseDir/t-hub-dev.raw.exe"
) | ForEach-Object {
  if (Test-Path -LiteralPath $_) { Remove-Item -LiteralPath $_ -Recurse -Force }
}
pnpm tauri build -f devbuild --config src-tauri/tauri.dev.conf.json --no-bundle
Copy-Item src-tauri/target/release/t-hub-dev.exe src-tauri/target/release/t-hub-dev.raw.exe
pnpm tauri bundle -f devbuild --config src-tauri/tauri.dev.conf.json
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

## Development installer acceptance

Never install or distribute a development installer until the tracked validator accepts its generated NSIS script and extracted payload.
The validator derives hashes from the artifacts under test and does not contain a release-specific expected hash.
The raw binary must contain exactly one canonical Tauri `__TAURI_BUNDLE_TYPE_VAR_UNK` marker.
The installer-extracted and installed binaries must contain exactly one `__TAURI_BUNDLE_TYPE_VAR_NSS` marker and no unknown marker.
The validator constructs the expected binary by replacing only that same-length marker at its unique byte offset.
The extracted and installed binaries must have the raw binary's exact length and must match the expected bytes and SHA-256 without any other alteration.

Run the source configuration regression before building:

```bash
pnpm --dir apps/desktop exec vitest run src/devBuildConfig.test.ts
```

On Windows, extract the generated installer and run the pre-install validator:

```powershell
$nsisDir = "apps/desktop/src-tauri/target/release/bundle/nsis"
$installers = @(Get-ChildItem $nsisDir -Filter "*-setup.exe")
if ($installers.Count -ne 1) { throw "Expected exactly one installer" }
$installer = $installers[0]
$installerScripts = @(Get-ChildItem "apps/desktop/src-tauri/target/release/nsis" -Filter "installer.nsi" -Recurse)
if ($installerScripts.Count -ne 1) { throw "Expected exactly one installer.nsi" }
$installerScript = $installerScripts[0]
$extractDir = "apps/desktop/src-tauri/target/release/dev-installer-extracted"
New-Item -ItemType Directory -Force $extractDir | Out-Null
& 7z x $installer.FullName "-o$extractDir" -y
if ($LASTEXITCODE -ne 0) { throw "7z extraction failed with exit code $LASTEXITCODE" }
$extractedBinaries = @(Get-ChildItem $extractDir -Filter "t-hub-dev.exe" -Recurse)
if ($extractedBinaries.Count -ne 1) { throw "Expected exactly one extracted t-hub-dev.exe" }
$extractedBinary = $extractedBinaries[0]
$evidenceDir = "apps/desktop/src-tauri/target/release/dev-installer-evidence"
New-Item -ItemType Directory -Force $evidenceDir | Out-Null
& scripts/windows/validate-dev-installer.ps1 `
  -InstallerScriptPath $installerScript.FullName `
  -InstallerPath $installer.FullName `
  -RawBinaryPath "apps/desktop/src-tauri/target/release/t-hub-dev.raw.exe" `
  -ExtractedBinaryPath $extractedBinary.FullName `
  -ExpectedBinaryPath "$evidenceDir/t-hub-dev.expected.exe" |
  Set-Content "$evidenceDir/dev-installer-validation.json" -Encoding utf8
Copy-Item "apps/desktop/src-tauri/target/release/t-hub-dev.raw.exe" "$evidenceDir/t-hub-dev.raw.exe"
Copy-Item $installerScript.FullName "$evidenceDir/installer.nsi"
Copy-Item $extractedBinary.FullName "$evidenceDir/t-hub-dev.extracted.exe"
```

The pre-install result must show production `t-hub`, development `t-hub-dev`, the canonical `UNK -> NSS` transformation, and raw, installer, expected, and extracted SHA-256 values.
After installing into the development install directory, run the same command with `-InstalledBinaryPath "$env:LOCALAPPDATA\T-Hub Dev\t-hub-dev.exe"`.
Record the raw binary, installer, extracted binary, and installed binary SHA-256 values with the build evidence.
Archive the retained raw binary, generated `installer.nsi`, expected patched binary, extracted binary, and validation JSON together so the exact acceptance decision can be audited later.
Do not substitute the post-bundle `target/release/t-hub-dev.exe` for the retained `t-hub-dev.raw.exe`, because Tauri patches the bundle marker during bundling.

Run the Windows validator fixture suite whenever its contract changes:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/windows/validate-dev-installer.test.ps1
```

## Package 1 Windows end-to-end acceptance

Package 1 is not accepted from source checks or installer inspection alone.
It requires a real Windows install, update, and uninstall sequence with a running production fixture.

Before the development install, record the production process ID, process start time, installed executable SHA-256, control-file path and contents, listener address and instance identity, and the result of one authenticated live control call.
Install the validated T-Hub Dev installer and verify that production retains the same process ID, start time, executable hash, control-file evidence, listener evidence, and successful live call.
Record the development process ID, start time, installed `t-hub-dev.exe` hash, `~/.t-hub-dev/control.json` evidence, listener identity, and one authenticated development live call.
Update T-Hub Dev with a second validated installer and verify that only the development process ID and start time change while production evidence remains byte-for-byte and identity-for-identity stable.
Uninstall T-Hub Dev and verify that only the development process exits and development install files are removed while production evidence and its live call remain unchanged.
The install, update, and uninstall logs must show that both `CheckIfAppIsRunning` calls target only `t-hub-dev.exe` and never `t-hub.exe`.

### Legacy development installer migration

Development installers built before the distinct main binary contract may have installed a development executable named `t-hub.exe`.
That legacy process name is indistinguishable from production by image name, so automation must never use `taskkill /IM t-hub.exe` as migration cleanup.
Before the first fixed upgrade, explicitly exit the legacy T-Hub Dev window by its development installation identity and confirm that production remains running.
The fixed installer can then migrate the same `com.t-hub.dev` installation to `t-hub-dev.exe`; verify the complete Windows end-to-end contract before relying on normal update behavior.

## Canonical identifiers

| Thing | Value |
| --- | --- |
| Production brand and window title | `T-Hub` |
| Development brand and window title | `T-Hub Dev` |
| Production main executable | `t-hub.exe` |
| Development main executable | `t-hub-dev.exe` |
| Production bundle identifier | `com.t-hub.app` |
| Development bundle identifier | `com.t-hub.dev` |
| Cargo app crate and library | `t-hub` and `t_hub_lib` |
| Subcrates | `t-hub-protocol`, `t-hub-agent`, `t-hub-mcp` |
| pnpm packages | root `t-hub`, desktop `t-hub-desktop`, site `t-hub-site` |
| MCP server name | `t-hub` |
| Hook marker | `__t_hub_managed__` |
| Repository | `n8watkins/t-hub` |
