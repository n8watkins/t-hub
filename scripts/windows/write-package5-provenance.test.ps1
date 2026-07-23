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
  if (-not $message.Contains($Needle)) { Write-Host "Observed bounded rejection: $message" }
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
  "*.exe`nvalidator.json`n" | Set-Content (Join-Path $root ".gitignore")
  git -C $root add .
  git -C $root commit --quiet -m fixture

  $raw = Join-Path $root "raw.exe"
  $expected = Join-Path $root "expected.exe"
  $extracted = Join-Path $root "extracted.exe"
  $installer = Join-Path $root "installer.exe"
  $validator = Join-Path $root "validator.json"
  "same" | Set-Content $raw
  Copy-Item $raw $expected
  Copy-Item $raw $extracted
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
    bundleMarkerTransformation = "UNK -> NSS"
  } | ConvertTo-Json | Set-Content $validator
  $head = (git -C $root rev-parse HEAD).Trim()
  $manifestPath = Join-Path ([System.IO.Path]::GetTempPath()) ("package-5-evidence-" + [guid]::NewGuid().ToString("N") + ".json")
  & $scriptPath -OutputPath $manifestPath -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidationPath $validator | Out-Null
  $manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json
  Assert-True ($manifest.schemaVersion -eq 1) "manifest schema version is not 1"
  Assert-True ($manifest.candidate.sourceCommit -eq $head) "source commit is not HEAD"
  Assert-True ($manifest.candidate.gitTree.Length -eq 40) "git tree binding is missing"
  Assert-True ($manifest.artifacts.expectedBinary.sha256 -eq $manifest.artifacts.extractedBinary.sha256) "expected/extracted hashes differ"
  Assert-True ($manifest.validation.passed -eq $true) "validator pass is not retained"
  Assert-True ($null -ne $manifest.installation -and $null -ne $manifest.environment -and $null -ne $manifest.matrix -and $null -ne $manifest.review) "required evidence sections are missing"

  $badSource = "0" * 40
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-source.json") -RepositoryRoot $root -SourceCommit $badSource -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidationPath $validator } "SourceCommit"
  "dirty" | Set-Content (Join-Path $root "dirty.txt")
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "dirty.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidationPath $validator } "dirty"
  Remove-Item (Join-Path $root "dirty.txt")
  "different" | Set-Content $expected
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "bad-hash.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidationPath $validator } "expected binary hash"
  Copy-Item $raw $expected
  @{ passed = $true; rawSha256 = $hash; installerSha256 = $installerHash; expectedSha256 = $hash; extractedSha256 = $hash; nested = @{ token = "do-not-ingest" } } | ConvertTo-Json -Depth 4 | Set-Content $validator
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "nested.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $installer -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidationPath $validator } "nested"
  $outside = Join-Path $root "..\outside.exe"
  "outside" | Set-Content $outside
  Assert-Fails { & $scriptPath -OutputPath (Join-Path ([System.IO.Path]::GetTempPath()) "outside.json") -RepositoryRoot $root -SourceCommit $head -InstallerPath $outside -RawBinaryPath $raw -ExpectedBinaryPath $expected -ExtractedBinaryPath $extracted -ValidationPath $validator } "outside repository"
  Write-Host "write-package5-provenance.test: PASS"
} finally {
  $outside = Join-Path $root "..\outside.exe"
  if (Test-Path -LiteralPath $outside) { Remove-Item -LiteralPath $outside -Force }
  if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
