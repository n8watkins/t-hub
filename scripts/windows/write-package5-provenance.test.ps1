[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$scriptPath = Join-Path $PSScriptRoot "write-package5-provenance.ps1"
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("thub-package5-provenance-" + [guid]::NewGuid().ToString("N"))

function Assert-True([bool]$Condition, [string]$Message) {
  if (-not $Condition) { throw "Assertion failed: $Message" }
}

function Assert-Fails([scriptblock]$Action, [string]$Needle) {
  $failed = $false
  $message = ""
  try { & $Action | Out-Null } catch { $failed = $true; $message = [string]$_.Exception.Message }
  Assert-True $failed "expected failure containing '$Needle'"
  if ($message.IndexOf($Needle, [System.StringComparison]::OrdinalIgnoreCase) -lt 0) { throw "wrong failure: $message" }
}

try {
  New-Item -ItemType Directory -Force -Path (Join-Path $root "apps/desktop/src-tauri") | Out-Null
  "lock" | Set-Content (Join-Path $root "pnpm-lock.yaml")
  "lock" | Set-Content (Join-Path $root "apps/desktop/src-tauri/Cargo.lock")
  '{}' | Set-Content (Join-Path $root "apps/desktop/src-tauri/tauri.conf.json")
  '{}' | Set-Content (Join-Path $root "apps/desktop/src-tauri/tauri.dev.conf.json")
  git -C $root init --quiet
  git -C $root config user.email test@example.invalid
  git -C $root config user.name provenance-test
  "*.exe`nvalidator.json`nother-validator.json`n" | Set-Content (Join-Path $root ".gitignore")
  git -C $root add .
  git -C $root commit --quiet -m fixture

  $raw = Join-Path $root "raw.exe"
  $expected = Join-Path $root "expected.exe"
  $extracted = Join-Path $root "extracted.exe"
  $installer = Join-Path $root "installer.exe"
  $installed = Join-Path $root "installed.exe"
  $validator = Join-Path $root "validator.json"
  "same" | Set-Content $raw
  Copy-Item $raw $expected
  Copy-Item $raw $extracted
  Copy-Item $raw $installed
  "installer" | Set-Content $installer
  $hash = (Get-FileHash $raw -Algorithm SHA256).Hash.ToLowerInvariant()
  $installerHash = (Get-FileHash $installer -Algorithm SHA256).Hash.ToLowerInvariant()
  @{
    passed = $true
    productionMainBinary = "t-hub"
    developmentMainBinary = "t-hub-dev"
    rawSha256 = $hash
    installerSha256 = $installerHash
    expectedSha256 = $hash
    extractedSha256 = $hash
    bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS"
  } | ConvertTo-Json | Set-Content $validator
  $head = (git -C $root rev-parse HEAD).Trim()
  $manifestPath = Join-Path ([System.IO.Path]::GetTempPath()) ("package-5-evidence-" + [guid]::NewGuid().ToString("N") + ".json")
  & $scriptPath -OutputPath $manifestPath -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator | Out-Null
  $manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json
  Assert-True ($manifest.schemaVersion -eq 1) "manifest schema version is not 1"
  Assert-True ($manifest.candidate.sourceCommit -eq $head) "source commit is not HEAD"
  Assert-True ($manifest.candidate.gitTree.Length -eq 40) "git tree binding is missing"
  Assert-True ($manifest.artifacts.expectedBinary.sha256 -eq $manifest.artifacts.extractedBinary.sha256) "expected/extracted hashes differ"
  Assert-True ($manifest.validation.passed -eq $true) "validator pass is not retained"
  Assert-True ($null -ne $manifest.installation -and $null -ne $manifest.environment -and $null -ne $manifest.matrix -and $null -ne $manifest.review) "required evidence sections are missing"
  Assert-True ($manifest.installation.status -eq "not_installed" -and $null -eq $manifest.installation.installedAt -and $null -eq $manifest.installation.installationTarget) "uninstalled manifest fabricated installation evidence"
  Assert-True ($null -ne $manifest.artifacts.validator -and $manifest.artifacts.validator.passed -eq $true) "validator artifact binding is missing"

  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; installedSha256 = $null; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  $nullManifestPath = Join-Path ([System.IO.Path]::GetTempPath()) ("package-5-null-installed-" + [guid]::NewGuid().ToString("N") + ".json")
  & $scriptPath -OutputPath $nullManifestPath -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator | Out-Null
  Assert-True ((Get-Content $nullManifestPath -Raw | ConvertFrom-Json).installation.status -eq "not_installed") "null installed hash should be accepted pre-install"

  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; installedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  $installedManifestPath = Join-Path ([System.IO.Path]::GetTempPath()) ("package-5-installed-" + [guid]::NewGuid().ToString("N") + ".json")
  $installedAt = "2026-07-23T12:00:00.000Z"
  & $scriptPath -OutputPath $installedManifestPath -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -InstalledBinaryPath $installed -InstalledAt $installedAt -ValidatorPath $validator -ValidationPath $validator | Out-Null
  $installedManifest = Get-Content $installedManifestPath -Raw | ConvertFrom-Json
  Assert-True ($installedManifest.installation.status -eq "installed" -and $installedManifest.installation.installedAt -eq $installedAt -and $installedManifest.installation.installationTarget -eq (Split-Path -Parent $installed) -and $installedManifest.artifacts.installedBinary.sha256 -eq $hash) "installed manifest does not bind installation hash"
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-installed-at.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -InstalledBinaryPath $installed -InstalledAt "not-a-timestamp" -ValidatorPath $validator -ValidationPath $validator } "RFC3339"

  $badSource = "0" * 40
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-source.json") -RepositoryRoot $root -SourceCommit $badSource -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "SourceCommit"
  $oldGithubSha = $env:GITHUB_SHA
  $env:GITHUB_SHA = $badSource
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-github-sha.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "GITHUB_SHA"
  if ($null -eq $oldGithubSha) { Remove-Item Env:GITHUB_SHA -ErrorAction SilentlyContinue } else { $env:GITHUB_SHA = $oldGithubSha }
  "dirty" | Set-Content (Join-Path $root "dirty.txt")
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "dirty.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "dirty"
  Remove-Item (Join-Path $root "dirty.txt")
  "different" | Set-Content $expected
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-hash.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "expected binary hash"
  Copy-Item $raw $expected
  @{ passed = $false; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "failed-validator.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "passed=true"
  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = "bad"; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-validator-hash.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "rawSha256"
  @{ passed = $true; productionMainBinary = "wrong"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "wrong-validator-binary.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "productionMainBinary"
  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "bad" } | ConvertTo-Json | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "wrong-validator-marker.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "bundle marker"
  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS"; installedSha256 = "wrong" } | ConvertTo-Json | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "wrong-installed-validator.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -InstalledBinaryPath $installed -InstalledAt $installedAt -ValidatorPath $validator -ValidationPath $validator } "installedSha256"
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "missing-installed-validator.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "not allowed"
  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  Copy-Item $raw $expected
  @{ passed = $true; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; nested = @{ token = "do-not-ingest" } } | ConvertTo-Json -Depth 4 | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "nested.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "nested"
  $otherValidator = Join-Path $root "other-validator.json"
  Copy-Item $validator $otherValidator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "validator-mismatch.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $otherValidator } "must equal"
  @{ passed = $true; productionMainBinary = "t-hub"; developmentMainBinary = "t-hub-dev"; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; bundleMarkerTransformation = "__TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS" } | ConvertTo-Json | Set-Content $validator
  $outside = Join-Path $root "..\outside.exe"
  "outside" | Set-Content $outside
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "outside.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -InstallerScriptPath $outside -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidatorPath $validator -ValidationPath $validator } "outside repository"
  Write-Host "write-package5-provenance.test: PASS"
} finally {
  $outside = Join-Path $root "..\outside.exe"
  if (Test-Path -LiteralPath $outside) { Remove-Item -LiteralPath $outside -Force }
  if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
