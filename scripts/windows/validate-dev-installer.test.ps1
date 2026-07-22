param(
  [ValidateSet("LF", "CRLF")]
  [string]$LineEndingMode
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $LineEndingMode) {
  foreach ($mode in @("LF", "CRLF")) {
    & $PSCommandPath -LineEndingMode $mode
  }
  return
}

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

function Write-NsisFixture {
  param(
    [string]$Path,
    [string]$Content
  )
  $lineEnding = if ($LineEndingMode -ceq "CRLF") { "`r`n" } else { "`n" }
  Write-AsciiFixture $Path ($Content -replace "`r`n|`r|`n", $lineEnding)
}

function ConvertTo-MixedLineEndings {
  param([string]$Content)
  $lines = $Content -split "`r`n|`r|`n"
  $builder = New-Object System.Text.StringBuilder
  for ($index = 0; $index -lt $lines.Count; $index++) {
    [void]$builder.Append($lines[$index])
    if ($index -lt $lines.Count - 1) {
      [void]$builder.Append($(if ($index % 2 -eq 0) { "`r`n" } else { "`n" }))
    }
  }
  return $builder.ToString()
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
    -ExpectedBinaryPath (Join-Path $fixtureRoot "expected-t-hub-dev.exe") `
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
  WriteRegStr HKCU "Software\T-Hub Dev" "MainBinaryName" "${MAINBINARYNAME}.exe"
SectionEnd
Section Uninstall
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  Delete "$INSTDIR\${MAINBINARYNAME}.exe"
SectionEnd
'@
  # PowerShell preserves the host platform's line endings in here-strings.
  # Normalize before constructing mutations so every negative fixture is
  # materially unsafe on both Windows and Unix hosts.
  $validScript = $validScript.Replace("`r`n", "`n").Replace("`r", "`n")
  Write-NsisFixture $validScriptPath $validScript
  Write-AsciiFixture $installerPath "fixture installer"
  $binaryPrefix = "T-Hub Dev|t-hub-dev|.t-hub-dev|t-hub-dev.db|__TAURI_BUNDLE_TYPE_VAR_NSS|"
  Write-AsciiFixture $rawPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_UNK|raw")
  Write-AsciiFixture $extractedPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|raw")
  Copy-Item -LiteralPath $extractedPath -Destination $installedPath

  $result = (Invoke-Validator $validScriptPath $rawPath $extractedPath $installedPath | Out-String) | ConvertFrom-Json
  Assert-True ($result.productionMainBinary -ceq "t-hub") "production binary result is incorrect."
  Assert-True ($result.developmentMainBinary -ceq "t-hub-dev") "development binary result is incorrect."
  Assert-True ($result.rawSha256 -cne $result.extractedSha256) "raw and extracted fixture hashes must differ."
  Assert-True ($result.expectedSha256 -ceq $result.extractedSha256) "expected and extracted fixture hashes must match."
  Assert-True ($result.extractedSha256 -ceq $result.installedSha256) "extracted and installed fixture hashes must match."
  $preInstallResult = (Invoke-Validator $validScriptPath $rawPath $extractedPath $null | Out-String) | ConvertFrom-Json
  Assert-True ($null -eq $preInstallResult.installedSha256) "pre-install validation must not invent an installed hash."

  $mixedScriptPath = Join-Path $fixtureRoot "mixed-installer.nsi"
  Write-AsciiFixture $mixedScriptPath (ConvertTo-MixedLineEndings $validScript)
  $mixedResult = (Invoke-Validator $mixedScriptPath $rawPath $extractedPath $installedPath | Out-String) | ConvertFrom-Json
  Assert-True ($mixedResult.expectedSha256 -ceq $mixedResult.extractedSha256) "mixed-line-ending installer script must validate."

  $oldFaultyScriptPath = Join-Path $fixtureRoot "old-faulty-installer.nsi"
  Write-NsisFixture $oldFaultyScriptPath ($validScript.Replace('t-hub-dev', 't-hub'))
  Assert-ValidatorFails "old production-binary installer" {
    Invoke-Validator $oldFaultyScriptPath $rawPath $extractedPath $installedPath
  } "MAINBINARYNAME must be t-hub-dev"

  $productionTargetScriptPath = Join-Path $fixtureRoot "production-process-target.nsi"
  $productionTargetScript = $validScript.Replace(
    '!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"',
    '!insertmacro CheckIfAppIsRunning "t-hub.exe" "${PRODUCTNAME}"'
  )
  Write-NsisFixture $productionTargetScriptPath $productionTargetScript
  Assert-ValidatorFails "production process target" {
    Invoke-Validator $productionTargetScriptPath $rawPath $extractedPath $installedPath
  } "production t-hub.exe reference"

  $wrongBundleIdScriptPath = Join-Path $fixtureRoot "wrong-bundle-id.nsi"
  $wrongBundleIdScript = $validScript.Replace(
    '!define BUNDLEID "com.t-hub.dev"',
    '!define BUNDLEID "com.t-hub.app"'
  )
  Write-NsisFixture $wrongBundleIdScriptPath $wrongBundleIdScript
  Assert-ValidatorFails "wrong generated NSI bundle ID" {
    Invoke-Validator $wrongBundleIdScriptPath $rawPath $extractedPath $installedPath
  } "installer bundle marker must be com.t-hub.dev"

  $duplicateRawPath = Join-Path $fixtureRoot "duplicate-marker.exe"
  Write-AsciiFixture $duplicateRawPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_UNK|__TAURI_BUNDLE_TYPE_VAR_UNK")
  Assert-ValidatorFails "duplicate raw bundle marker" {
    Invoke-Validator $validScriptPath $duplicateRawPath $extractedPath $installedPath
  } "exactly 1 time(s), found 2"

  $wrongInstalledPath = Join-Path $fixtureRoot "wrong-installed.exe"
  Write-AsciiFixture $wrongInstalledPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|rax")
  Assert-ValidatorFails "installed hash mismatch" {
    Invoke-Validator $validScriptPath $rawPath $extractedPath $wrongInstalledPath
  } "installed binary must equal the exact canonical"

  $tamperedExtractedPath = Join-Path $fixtureRoot "tampered-extracted.exe"
  $tamperedInstalledPath = Join-Path $fixtureRoot "tampered-installed.exe"
  Write-AsciiFixture $tamperedExtractedPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|ARBITRARY-TAMPERED-PAYLOAD")
  Copy-Item -LiteralPath $tamperedExtractedPath -Destination $tamperedInstalledPath
  Assert-ValidatorFails "altered extracted payload" {
    Invoke-Validator $validScriptPath $rawPath $tamperedExtractedPath $tamperedInstalledPath
  } "same byte length"

  $alteredByteExtractedPath = Join-Path $fixtureRoot "altered-byte-extracted.exe"
  $alteredByteInstalledPath = Join-Path $fixtureRoot "altered-byte-installed.exe"
  Write-AsciiFixture $alteredByteExtractedPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|rax")
  Copy-Item -LiteralPath $alteredByteExtractedPath -Destination $alteredByteInstalledPath
  Assert-ValidatorFails "same-length altered extracted byte" {
    Invoke-Validator $validScriptPath $rawPath $alteredByteExtractedPath $alteredByteInstalledPath
  } "exact canonical UNK-to-NSS patch"

  $appendedExtractedPath = Join-Path $fixtureRoot "appended-extracted.exe"
  $appendedInstalledPath = Join-Path $fixtureRoot "appended-installed.exe"
  Write-AsciiFixture $appendedExtractedPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|raw|APPENDED")
  Copy-Item -LiteralPath $appendedExtractedPath -Destination $appendedInstalledPath
  Assert-ValidatorFails "appended extracted payload" {
    Invoke-Validator $validScriptPath $rawPath $appendedExtractedPath $appendedInstalledPath
  } "same byte length"

  $truncatedExtractedPath = Join-Path $fixtureRoot "truncated-extracted.exe"
  $truncatedInstalledPath = Join-Path $fixtureRoot "truncated-installed.exe"
  Write-AsciiFixture $truncatedExtractedPath ($binaryPrefix + "__TAURI_BUNDLE_TYPE_VAR_NSS|r")
  Copy-Item -LiteralPath $truncatedExtractedPath -Destination $truncatedInstalledPath
  Assert-ValidatorFails "truncated extracted payload" {
    Invoke-Validator $validScriptPath $rawPath $truncatedExtractedPath $truncatedInstalledPath
  } "same byte length"

  $misplacedChecksScriptPath = Join-Path $fixtureRoot "misplaced-checks.nsi"
  $misplacedChecksScript = $validScript.Replace(
    "Section Uninstall`n  !insertmacro CheckIfAppIsRunning `"`${MAINBINARYNAME}.exe`" `"`${PRODUCTNAME}`"",
    "  !insertmacro CheckIfAppIsRunning `"`${MAINBINARYNAME}.exe`" `"`${PRODUCTNAME}`"`nSection Uninstall"
  )
  Assert-True ($misplacedChecksScript -cne $validScript) "misplaced process-check fixture mutation must take effect."
  Write-NsisFixture $misplacedChecksScriptPath $misplacedChecksScript
  Assert-ValidatorFails "both process checks in install section" {
    Invoke-Validator $misplacedChecksScriptPath $rawPath $extractedPath $installedPath
  } "all CheckIfAppIsRunning calls must be confined"

  $directKillScriptPath = Join-Path $fixtureRoot "direct-production-kill.nsi"
  $directKillScript = $validScript.Replace(
    "Section Install",
    "Section Install`n  KillProcessCurrentUser `"t-hub.exe`""
  )
  Write-NsisFixture $directKillScriptPath $directKillScript
  Assert-ValidatorFails "direct production process kill" {
    Invoke-Validator $directKillScriptPath $rawPath $extractedPath $installedPath
  } "production t-hub.exe reference"

  $indirectKillScriptPath = Join-Path $fixtureRoot "indirect-production-kill.nsi"
  $indirectKillScript = $validScript.Replace(
    '!define BUNDLEID "com.t-hub.dev"',
    "!define BUNDLEID `"com.t-hub.dev`"`n!define PROCESSALIAS `"t-hub.exe`""
  ).Replace(
    "Section Install",
    'Section Install' + "`n" + '  KillProcessCurrentUser "${PROCESSALIAS}"'
  )
  Write-NsisFixture $indirectKillScriptPath $indirectKillScript
  Assert-ValidatorFails "indirect production process kill" {
    Invoke-Validator $indirectKillScriptPath $rawPath $extractedPath $installedPath
  } "production t-hub.exe reference"

  $extraProductionRefsScriptPath = Join-Path $fixtureRoot "extra-production-refs.nsi"
  $extraProductionRefsScript = $validScript.Replace(
    '  File "${MAINBINARYSRCPATH}"',
    '  File "${MAINBINARYSRCPATH}"' + "`n" +
      '  File "/oname=t-hub.exe" "C:\build\t-hub.exe"' + "`n" +
      '  CreateShortCut "$DESKTOP\Production.lnk" "$INSTDIR\t-hub.exe"' + "`n" +
      '  WriteRegStr HKCU "Software\T-Hub Dev" "MainBinaryName" "t-hub.exe"'
  ).Replace(
    '  Delete "$INSTDIR\${MAINBINARYNAME}.exe"',
    '  Delete "$INSTDIR\${MAINBINARYNAME}.exe"' + "`n" + '  Delete "$INSTDIR\t-hub.exe"'
  )
  Write-NsisFixture $extraProductionRefsScriptPath $extraProductionRefsScript
  Assert-ValidatorFails "extra production executable references" {
    Invoke-Validator $extraProductionRefsScriptPath $rawPath $extractedPath $installedPath
  } "production t-hub.exe reference"

  $workflowPath = Join-Path $repoRoot ".github\workflows\release.yml"
  $workflow = Get-Content -LiteralPath $workflowPath -Raw
  $validatorTestIndex = $workflow.IndexOf("- name: Test development installer validator")
  $buildIndex = $workflow.IndexOf("- name: Build installers")
  $validationIndex = $workflow.IndexOf("- name: Validate development installer isolation")
  $uploadIndex = $workflow.IndexOf("- name: Upload installers as workflow artifacts")
  Assert-True ($validatorTestIndex -ge 0 -and $validatorTestIndex -lt $buildIndex) "validator fixtures must run before the installer build."
  Assert-True ($buildIndex -lt $validationIndex -and $validationIndex -lt $uploadIndex) "real installer validation must run after build and before upload."
  foreach ($requiredWorkflowContract in @(
    '"$releaseDir/bundle/nsis"',
    '"$releaseDir/nsis"',
    '"$releaseDir/dev-installer-extracted"',
    '$installers.Count -ne 1',
    '$installerScripts.Count -ne 1',
    '$extractedBinaries.Count -ne 1',
    '$LASTEXITCODE -ne 0',
    '-ExpectedBinaryPath',
    'dev-installer-evidence/*'
  )) {
    Assert-True ($workflow.Contains($requiredWorkflowContract)) "release workflow is missing '$requiredWorkflowContract'."
  }

  Write-Host "PASS: Dev installer validator accepted $LineEndingMode and mixed-line-ending fixtures and rejected thirteen unsafe fixtures."
} finally {
  if (Test-Path -LiteralPath $fixtureRoot) {
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
  }
}
