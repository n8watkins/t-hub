[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)] [string]$OutputPath,
  [Parameter(Mandatory = $true)] [string]$RepositoryRoot,
  [Parameter(Mandatory = $true)] [string]$SourceCommit,
  [Parameter(Mandatory = $true)] [string]$InstallerPath,
  [Parameter(Mandatory = $true)] [string]$RawBinaryPath,
  [Parameter(Mandatory = $true)] [string]$ExpectedBinaryPath,
  [Parameter(Mandatory = $true)] [string]$ExtractedBinaryPath,
  [string]$InstalledBinaryPath = "",
  [string]$InstallerScriptPath = "",
  [string]$ValidatorPath = "",
  [string]$Workflow = "",
  [string]$RunId = "",
  [int]$RunAttempt = 1,
  [string]$RunnerImage = "",
  [string]$WindowsVersion = "",
  [string]$WebView2Version = "",
  [string]$NodeVersion = "",
  [string]$PnpmVersion = "",
  [string]$RustVersion = "",
  [string]$TargetTriple = "x86_64-pc-windows-msvc",
  [string[]]$FeatureSet = @("devbuild"),
  [string]$AppVersion = "",
  [int]$ProtocolVersion = 2,
  [string]$ValidationPath = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-File([string]$Path, [string]$Label) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "$Label is missing at '$Path'."
  }
  return (Resolve-Path -LiteralPath $Path).Path
}

function Hash-File([string]$Path) {
  return (Get-FileHash -LiteralPath (Resolve-File $Path "file") -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Relative-EvidencePath([string]$Path) {
  $resolved = (Resolve-Path -LiteralPath $Path).Path
  $root = (Resolve-Path -LiteralPath $RepositoryRoot).Path
  if ($resolved.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
    return $resolved.Substring($root.Length).TrimStart('\', '/')
  }
  return $resolved
}

function Git-Output([string[]]$Arguments) {
  $result = & git -C $RepositoryRoot @Arguments 2>&1
  if ($LASTEXITCODE -ne 0) { throw "git $($Arguments -join ' ') failed: $result" }
  return ([string]$result).Trim()
}

function Optional-Hash([string]$Path) {
  if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
  return Hash-File $Path
}

function Read-SafeValidation([string]$Path) {
  if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
  $resolved = Resolve-File $Path "validation evidence"
  $value = Get-Content -LiteralPath $resolved -Raw | ConvertFrom-Json
  $forbidden = '(?i)(token|secret|credential|password|transcript|prompt|argument|payload|raw.?hook|control.?file)'
  foreach ($property in $value.PSObject.Properties) {
    if ($property.Name -match $forbidden) { throw "validation evidence contains prohibited field '$($property.Name)'" }
  }
  return $value
}

$RepositoryRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$InstallerPath = Resolve-File $InstallerPath "installer"
$RawBinaryPath = Resolve-File $RawBinaryPath "raw binary"
$ExpectedBinaryPath = Resolve-File $ExpectedBinaryPath "expected binary"
$ExtractedBinaryPath = Resolve-File $ExtractedBinaryPath "extracted binary"
if ($InstalledBinaryPath) { $InstalledBinaryPath = Resolve-File $InstalledBinaryPath "installed binary" }
if ($InstallerScriptPath) { $InstallerScriptPath = Resolve-File $InstallerScriptPath "installer script" }
if ($ValidatorPath) { $ValidatorPath = Resolve-File $ValidatorPath "validator" }

$pnpmLock = Join-Path $RepositoryRoot "pnpm-lock.yaml"
$cargoLock = Join-Path $RepositoryRoot "apps\desktop\src-tauri\Cargo.lock"
$tauriConfig = Join-Path $RepositoryRoot "apps\desktop\src-tauri\tauri.conf.json"
$tauriOverlay = Join-Path $RepositoryRoot "apps\desktop\src-tauri\tauri.dev.conf.json"
$tree = Git-Output @("rev-parse", "$SourceCommit`^{tree}")
$sourceResolved = Git-Output @("rev-parse", $SourceCommit)
$now = (Get-Date).ToUniversalTime().ToString("o")

$manifest = [ordered]@{
  schemaVersion = 1
  candidate = [ordered]@{
    artifactId = "t-hub-dev:$sourceResolved"
    branch = if ($env:GITHUB_REF_NAME) { $env:GITHUB_REF_NAME } else { Git-Output @("branch", "--show-current") }
    sourceBaseline = if ($env:GITHUB_SHA) { $env:GITHUB_SHA } else { $sourceResolved }
    sourceCommit = $sourceResolved
    gitTree = $tree
    repository = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "local" }
    pnpmLockSha256 = Hash-File $pnpmLock
    cargoLockSha256 = Hash-File $cargoLock
    appVersion = $AppVersion
    protocolVersion = $ProtocolVersion
  }
  build = [ordered]@{
    workflow = $Workflow
    runId = $RunId
    runAttempt = $RunAttempt
    runnerImage = $RunnerImage
    windowsVersion = $WindowsVersion
    webView2Version = $WebView2Version
    nodeVersion = $NodeVersion
    pnpmVersion = $PnpmVersion
    rustVersion = $RustVersion
    targetTriple = $TargetTriple
    featureSet = @($FeatureSet)
    tauriConfigSha256 = Hash-File $tauriConfig
    tauriOverlaySha256 = Hash-File $tauriOverlay
    startedAt = $now
    finishedAt = $now
  }
  artifacts = [ordered]@{
    installer = [ordered]@{ path = Relative-EvidencePath $InstallerPath; sha256 = Hash-File $InstallerPath; signatureStatus = "unreported"; reference = "workflow-artifact" }
    rawBinary = [ordered]@{ path = Relative-EvidencePath $RawBinaryPath; sha256 = Hash-File $RawBinaryPath }
    expectedBinary = [ordered]@{ path = Relative-EvidencePath $ExpectedBinaryPath; sha256 = Hash-File $ExpectedBinaryPath }
    extractedBinary = [ordered]@{ path = Relative-EvidencePath $ExtractedBinaryPath; sha256 = Hash-File $ExtractedBinaryPath }
    installedBinary = if ($InstalledBinaryPath) { [ordered]@{ path = $InstalledBinaryPath; sha256 = Hash-File $InstalledBinaryPath } } else { $null }
    installerScript = if ($InstallerScriptPath) { [ordered]@{ path = Relative-EvidencePath $InstallerScriptPath; sha256 = Hash-File $InstallerScriptPath } } else { $null }
    validator = if ($ValidatorPath) { [ordered]@{ path = Relative-EvidencePath $ValidatorPath; sha256 = Hash-File $ValidatorPath; passed = $true } } else { $null }
  }
  validation = Read-SafeValidation $ValidationPath
}

$parent = Split-Path -Parent $OutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$json = $manifest | ConvertTo-Json -Depth 12
$temporary = "$OutputPath.$([guid]::NewGuid().ToString('N')).tmp"
[System.IO.File]::WriteAllText($temporary, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $temporary -Destination $OutputPath -Force
$json
