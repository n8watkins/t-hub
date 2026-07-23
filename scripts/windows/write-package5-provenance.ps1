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
  $rootWithSeparator = $root.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
  if ($resolved.Equals($root, [System.StringComparison]::OrdinalIgnoreCase)) {
    return "."
  }
  if ($resolved.StartsWith($rootWithSeparator, [System.StringComparison]::OrdinalIgnoreCase)) {
    return $resolved.Substring($root.Length).TrimStart('\', '/')
  }
  throw "evidence path '$resolved' is outside repository root '$root'"
}

function Git-Output([string[]]$Arguments) {
  $result = & git -C $RepositoryRoot @Arguments 2>&1
  if ($LASTEXITCODE -ne 0) { throw "git $($Arguments -join ' ') failed: $result" }
  if ($null -eq $result) { return "" }
  return ((@($result) -join [Environment]::NewLine).Trim())
}

function Optional-Hash([string]$Path) {
  if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
  return Hash-File $Path
}

function Read-SafeValidation([string]$Path, [hashtable]$Expected) {
  if ([string]::IsNullOrWhiteSpace($Path)) { throw "validator evidence is required" }
  $resolved = Resolve-File $Path "validation evidence"
  $value = Get-Content -LiteralPath $resolved -Raw | ConvertFrom-Json
  $allowed = @("passed", "productionMainBinary", "developmentMainBinary", "rawSha256", "installerSha256", "expectedSha256", "extractedSha256", "installedSha256", "bundleMarkerTransformation")
  $forbidden = '(?i)(token|secret|credential|password|transcript|prompt|tool|argument|payload|hook|control|content|command)'
  foreach ($property in $value.PSObject.Properties) {
    if ($property.Name -match $forbidden) { throw "validation evidence contains prohibited field '$($property.Name)'" }
    if ($property.Name -notin $allowed) { throw "validation evidence contains unknown field '$($property.Name)'" }
    if ($property.Value -is [System.Collections.IEnumerable] -and -not ($property.Value -is [string])) { throw "validation evidence field '$($property.Name)' has an unsupported structure" }
    if ($property.Value -is [System.Management.Automation.PSCustomObject]) { throw "validation evidence field '$($property.Name)' has an unsupported nested structure" }
  }
  if ($value.passed -ne $true) { throw "validator evidence did not report passed=true" }
  foreach ($key in @("rawSha256", "installerSha256", "expectedSha256", "extractedSha256")) {
    if ([string]$value.$key -cne [string]$Expected[$key]) { throw "validator evidence $key does not match computed hash" }
  }
  if ($Expected.ContainsKey("installedSha256") -and [string]$value.installedSha256 -cne [string]$Expected.installedSha256) { throw "validator evidence installedSha256 does not match computed hash" }
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
$sourceResolved = Git-Output @("rev-parse", $SourceCommit)
$head = Git-Output @("rev-parse", "HEAD")
if ($sourceResolved -cne $head) { throw "SourceCommit must equal the checked-out HEAD ($head), got $sourceResolved" }
$tree = Git-Output @("rev-parse", "$SourceCommit`^{tree}")
$dirty = Git-Output @("status", "--porcelain", "--untracked-files=all")
if (-not [string]::IsNullOrWhiteSpace($dirty)) { throw "checked-out tree is dirty; refusing provenance manifest" }
foreach ($tracked in @("pnpm-lock.yaml", "apps/desktop/src-tauri/Cargo.lock", "apps/desktop/src-tauri/tauri.conf.json", "apps/desktop/src-tauri/tauri.dev.conf.json")) {
  $null = Git-Output @("ls-files", "--error-unmatch", $tracked)
}
$rawHash = Hash-File $RawBinaryPath
$installerHash = Hash-File $InstallerPath
$expectedHash = Hash-File $ExpectedBinaryPath
$extractedHash = Hash-File $ExtractedBinaryPath
$installedHash = if ($InstalledBinaryPath) { Hash-File $InstalledBinaryPath } else { $null }
if ($expectedHash -cne $extractedHash) { throw "expected binary hash must equal extracted binary hash" }
if ($installedHash -and $installedHash -cne $expectedHash) { throw "installed binary hash must equal expected/extracted binary hash" }
$expectedValidation = [ordered]@{ rawSha256 = $rawHash; installerSha256 = $installerHash; expectedSha256 = $expectedHash; extractedSha256 = $extractedHash }
if ($installedHash) { $expectedValidation.installedSha256 = $installedHash }
$validation = Read-SafeValidation $ValidationPath $expectedValidation
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
  installation = [ordered]@{
    installedAt = $now
    productName = "T-Hub Dev"
    bundleIdentifier = "com.t-hub.dev"
    executableName = "t-hub-dev.exe"
    installationTarget = if ($InstalledBinaryPath) { $InstalledBinaryPath } else { "uninstalled" }
  }
  environment = [ordered]@{
    tHubDistro = if ($env:THUB_DISTRO) { $env:THUB_DISTRO } else { "unreported" }
    wslVersion = if ($env:WSL_VERSION) { $env:WSL_VERSION } else { "unreported" }
    wslKernelVersion = if ($env:WSL_KERNEL_VERSION) { $env:WSL_KERNEL_VERSION } else { "unreported" }
    agentVersion = if ($env:THUB_AGENT_VERSION) { $env:THUB_AGENT_VERSION } else { "unreported" }
    claudeVersion = if ($env:CLAUDE_VERSION) { $env:CLAUDE_VERSION } else { "unreported" }
    codexVersion = if ($env:CODEX_VERSION) { $env:CODEX_VERSION } else { "unreported" }
  }
  matrix = @()
  review = [ordered]@{ reviewer = if ($env:PACKAGE5_REVIEWER) { $env:PACKAGE5_REVIEWER } else { "pending" }; reviewedAt = if ($env:PACKAGE5_REVIEWED_AT) { $env:PACKAGE5_REVIEWED_AT } else { $null }; decision = if ($env:PACKAGE5_REVIEW_DECISION) { $env:PACKAGE5_REVIEW_DECISION } else { "pending" } }
  validation = $validation
}

$parent = Split-Path -Parent $OutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$json = $manifest | ConvertTo-Json -Depth 12
$temporary = "$OutputPath.$([guid]::NewGuid().ToString('N')).tmp"
[System.IO.File]::WriteAllText($temporary, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $temporary -Destination $OutputPath -Force
$json
