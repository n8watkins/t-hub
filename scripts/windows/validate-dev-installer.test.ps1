$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$validator = Join-Path $PSScriptRoot "validate-dev-installer.ps1"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("t-hub-dev-installer-validator-" + [guid]::NewGuid().ToString("N"))

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Message
  )
  if (-not $Condition) {
    throw "Assertion failed: $Message"
  }
}

function Write-AsciiFixture {
  param(
    [string]$Path,
    [string]$Content
  )
  [System.IO.File]::WriteAllBytes($Path, [System.Text.Encoding]::ASCII.GetBytes($Content))
}

function Invoke-Validator {
  param(
    [string]$ScriptPath,
    [string]$RawPath,
    [string]$ExtractedPath,
    [string]$InstalledPath
  )
  return & $validator `
    -InstallerScriptPath $ScriptPath `
    -InstallerPath (Join-Path $fixtureRoot "T-Hub Dev_0.0.0_x64-setup.exe") `
    -RawBinaryPath $RawPath `
    -ExtractedBinaryPath $ExtractedPath `
    -InstalledBinaryPath $InstalledPath `
    -ProductionConfigPath (Join-Path $repoRoot "apps\desktop\src-tauri\tauri.conf.json") `
    -DevelopmentConfigPath (Join-Path $repoRoot "apps\desktop\src-tauri\tauri.dev.conf.json") `
    -CargoManifestPath (Join-Path $repoRoot "apps\desktop\src-tauri\Cargo.toml")
}

function Assert-ValidatorFails {
  param(
    [string]$Name,
    [scriptblock]$Action,
    [string]$ExpectedMessage
  )
  try {
    & $Action | Out-Null
    throw "Validator unexpectedly accepted negative fixture '$Name'."
  } catch {
    Assert-True ($_.Exception.Message -like "*$ExpectedMessage*") "negative fixture '$Name' failed for the wrong reason: $($_.Exception.Message)"
  }
}

try {
  New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
  $validScriptPath = Join-Path $fixtureRoot "installer.nsi"
  $rawPath = Join-Path $fixtureRoot "raw-t-hub-dev.exe"
  $extractedPath = Join-Path $fixtureRoot "extracted-t-hub-dev.exe"
  $installedPath = Join-Path $fixtureRoot "installed-t-hub-dev.exe"
  $installerPath = Join-Path $fixtureRoot "T-Hub Dev_0.0.0_x64-setup.exe"

  $validScript = @'
!define PRODUCTNAME "T-Hub Dev"
!define MAINBINARYNAME "t-hub-dev"
!define MAINBINARYSRCPATH "C:\build\t-hub-dev.exe"
!define BUNDLEID "com.t-hub.dev"
Section Install
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  File "${MAINBINARYSRCPATH}"
  CreateShortCut "$SMPROGRAMS\T-Hub Dev.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
SectionEnd
Section Uninstall
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  Delete "$INSTDIR\${MAINBINARYNAME}.exe"
SectionEnd
'@
  Set-Content -LiteralPath $validScriptPath -Value $validScript -Encoding UTF8
  Write-AsciiFixture $installerPath "fixture installer"
  $binaryPrefix = "T-Hub Dev|com.t-hub.dev|t-hub-dev|.t-hub-dev|"
  Write-AsciiFixture $rawPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_UNK|raw")
  Write-AsciiFixture $extractedPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|raw")
  Copy-Item -LiteralPath $extractedPath -Destination $installedPath

  $result = (Invoke-Validator $validScriptPath $rawPath $extractedPath $installedPath | Out-String) | ConvertFrom-Json
  Assert-True ($result.productionMainBinary -ceq "t-hub") "production binary result is incorrect."
  Assert-True ($result.developmentMainBinary -ceq "t-hub-dev") "development binary result is incorrect."
  Assert-True ($result.rawSha256 -cne $result.extractedSha256) "raw and extracted fixture hashes must differ."
  Assert-True ($result.extractedSha256 -ceq $result.installedSha256) "extracted and installed fixture hashes must match."
  $preInstallResult = (Invoke-Validator $validScriptPath $rawPath $extractedPath $null | Out-String) | ConvertFrom-Json
  Assert-True ($null -eq $preInstallResult.installedSha256) "pre-install validation must not invent an installed hash."

  $oldFaultyScriptPath = Join-Path $fixtureRoot "old-faulty-installer.nsi"
  Set-Content -LiteralPath $oldFaultyScriptPath -Value ($validScript.Replace('t-hub-dev', 't-hub')) -Encoding UTF8
  Assert-ValidatorFails "old production-binary installer" {
    Invoke-Validator $oldFaultyScriptPath $rawPath $extractedPath $installedPath
  } "MAINBINARYNAME must be t-hub-dev"

  $productionTargetScriptPath = Join-Path $fixtureRoot "production-process-target.nsi"
  $productionTargetScript = $validScript.Replace(
    '!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"',
    '!insertmacro CheckIfAppIsRunning "t-hub.exe" "${PRODUCTNAME}"'
  )
  Set-Content -LiteralPath $productionTargetScriptPath -Value $productionTargetScript -Encoding UTF8
  Assert-ValidatorFails "production process target" {
    Invoke-Validator $productionTargetScriptPath $rawPath $extractedPath $installedPath
  } "every CheckIfAppIsRunning target must be t-hub-dev.exe"

  $duplicateRawPath = Join-Path $fixtureRoot "duplicate-marker.exe"
  Write-AsciiFixture $duplicateRawPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_UNK|__TAURI_BUNDLE_TYPE_VAR_UNK")
  Assert-ValidatorFails "duplicate raw bundle marker" {
    Invoke-Validator $validScriptPath $duplicateRawPath $extractedPath $installedPath
  } "exactly 1 time(s), found 2"

  $wrongInstalledPath = Join-Path $fixtureRoot "wrong-installed.exe"
  Write-AsciiFixture $wrongInstalledPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|different")
  Assert-ValidatorFails "installed hash mismatch" {
    Invoke-Validator $validScriptPath $rawPath $extractedPath $wrongInstalledPath
  } "installed binary hash must equal"

  Write-Host "PASS: Dev installer validator accepted the isolated fixture and rejected four unsafe fixtures."
} finally {
  if (Test-Path -LiteralPath $fixtureRoot) {
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
  }
}
