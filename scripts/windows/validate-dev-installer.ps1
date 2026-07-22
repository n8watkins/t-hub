[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$InstallerScriptPath,
  [Parameter(Mandatory = $true)]
  [string]$InstallerPath,
  [Parameter(Mandatory = $true)]
  [string]$RawBinaryPath,
  [Parameter(Mandatory = $true)]
  [string]$ExtractedBinaryPath,
  [string]$InstalledBinaryPath,
  [string]$ExpectedBinaryPath,
  [string]$ProductionConfigPath,
  [string]$DevelopmentConfigPath,
  [string]$CargoManifestPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $ProductionConfigPath) {
  $ProductionConfigPath = Join-Path $repoRoot "apps\desktop\src-tauri\tauri.conf.json"
}
if (-not $DevelopmentConfigPath) {
  $DevelopmentConfigPath = Join-Path $repoRoot "apps\desktop\src-tauri\tauri.dev.conf.json"
}
if (-not $CargoManifestPath) {
  $CargoManifestPath = Join-Path $repoRoot "apps\desktop\src-tauri\Cargo.toml"
}

function Assert-Contract {
  param(
    [bool]$Condition,
    [string]$Message
  )
  if (-not $Condition) {
    throw "Dev installer validation failed: $Message"
  }
}

function Resolve-RequiredFile {
  param(
    [string]$Path,
    [string]$Label
  )
  Assert-Contract (Test-Path -LiteralPath $Path -PathType Leaf) "$Label is missing at '$Path'."
  return (Resolve-Path -LiteralPath $Path).Path
}

function Get-AsciiMarkerCount {
  param(
    [string]$Path,
    [string]$Marker
  )
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $needle = [System.Text.Encoding]::ASCII.GetBytes($Marker)
  $count = 0
  for ($offset = 0; $offset -le $bytes.Length - $needle.Length; $offset++) {
    $matches = $true
    for ($index = 0; $index -lt $needle.Length; $index++) {
      if ($bytes[$offset + $index] -ne $needle[$index]) {
        $matches = $false
        break
      }
    }
    if ($matches) {
      $count++
      $offset += $needle.Length - 1
    }
  }
  return $count
}

function Assert-MarkerCount {
  param(
    [string]$Path,
    [string]$Marker,
    [int]$Expected,
    [string]$Label
  )
  $actual = Get-AsciiMarkerCount -Path $Path -Marker $Marker
  Assert-Contract ($actual -eq $Expected) "$Label must contain '$Marker' exactly $Expected time(s), found $actual."
}

function Get-Sha256 {
  param([string]$Path)
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-BytesSha256 {
  param([byte[]]$Bytes)
  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {
    return ([System.BitConverter]::ToString($sha256.ComputeHash($Bytes))).Replace("-", "").ToLowerInvariant()
  } finally {
    $sha256.Dispose()
  }
}

function Find-ByteSequenceOffsets {
  param(
    [byte[]]$Bytes,
    [byte[]]$Needle
  )
  $offsets = @()
  for ($offset = 0; $offset -le $Bytes.Length - $Needle.Length; $offset++) {
    $matches = $true
    for ($index = 0; $index -lt $Needle.Length; $index++) {
      if ($Bytes[$offset + $index] -ne $Needle[$index]) {
        $matches = $false
        break
      }
    }
    if ($matches) {
      $offsets += $offset
      $offset += $Needle.Length - 1
    }
  }
  return @($offsets)
}

function Get-UniqueNsisDefine {
  param(
    [string]$Script,
    [string]$Name
  )
  $matches = [regex]::Matches($Script, "(?im)^\s*!define\s+$([regex]::Escape($Name))\s+`"([^`"]+)`"\s*$")
  Assert-Contract ($matches.Count -eq 1) "installer.nsi must define $Name exactly once."
  return $matches[0].Groups[1].Value
}

function Resolve-NsisDefines {
  param([string]$Script)
  $definitions = @{}
  foreach ($match in [regex]::Matches($Script, '(?im)^\s*!define\s+([A-Za-z0-9_]+)\s+"([^"]*)"\s*$')) {
    $name = $match.Groups[1].Value
    if (-not $definitions.ContainsKey($name)) {
      $definitions[$name] = $match.Groups[2].Value
    }
  }
  $resolved = $Script
  for ($pass = 0; $pass -lt 20; $pass++) {
    $before = $resolved
    foreach ($name in $definitions.Keys) {
      $resolved = $resolved.Replace(('${' + $name + '}'), [string]$definitions[$name])
    }
    if ($resolved -ceq $before) {
      return $resolved
    }
  }
  throw "Dev installer validation failed: NSIS definitions did not resolve within 20 passes."
}

function Get-UniqueNsisSection {
  param(
    [string]$Script,
    [string]$Name
  )
  $pattern = '(?ims)^\s*Section\s+(?:/o\s+)?(?:"([^"\r\n]+)"|([^\r\n]+))\s*$([\s\S]*?)^\s*SectionEnd\b'
  $matches = @()
  foreach ($match in [regex]::Matches($Script, $pattern)) {
    $sectionName = if ($match.Groups[1].Success) { $match.Groups[1].Value } else { $match.Groups[2].Value.Trim() }
    if ($sectionName -ceq $Name) {
      $matches += $match
    }
  }
  Assert-Contract ($matches.Count -eq 1) "installer.nsi must contain exactly one $Name section."
  return $matches[0].Groups[3].Value
}

function Get-OptionalProperty {
  param(
    [object]$Object,
    [string]$Name
  )
  $property = $Object.PSObject.Properties[$Name]
  if ($null -eq $property) {
    return $null
  }
  return $property.Value
}

$InstallerScriptPath = Resolve-RequiredFile $InstallerScriptPath "generated installer.nsi"
$InstallerPath = Resolve-RequiredFile $InstallerPath "NSIS installer"
$RawBinaryPath = Resolve-RequiredFile $RawBinaryPath "raw development binary"
$ExtractedBinaryPath = Resolve-RequiredFile $ExtractedBinaryPath "installer-extracted development binary"
$ProductionConfigPath = Resolve-RequiredFile $ProductionConfigPath "production Tauri config"
$DevelopmentConfigPath = Resolve-RequiredFile $DevelopmentConfigPath "development Tauri config"
$CargoManifestPath = Resolve-RequiredFile $CargoManifestPath "Cargo manifest"
if ($InstalledBinaryPath) {
  $InstalledBinaryPath = Resolve-RequiredFile $InstalledBinaryPath "installed development binary"
}

$productionConfig = Get-Content -LiteralPath $ProductionConfigPath -Raw | ConvertFrom-Json
$developmentConfig = Get-Content -LiteralPath $DevelopmentConfigPath -Raw | ConvertFrom-Json
$cargoManifest = Get-Content -LiteralPath $CargoManifestPath -Raw
$cargoNameMatch = [regex]::Match($cargoManifest, '(?m)^name\s*=\s*"([^"]+)"')
Assert-Contract $cargoNameMatch.Success "Cargo package name is missing."
$cargoBinaryName = $cargoNameMatch.Groups[1].Value
$productionMainBinary = Get-OptionalProperty $productionConfig "mainBinaryName"
$developmentMainBinary = Get-OptionalProperty $developmentConfig "mainBinaryName"
$productionBinaryName = if ($productionMainBinary) {
  [string]$productionMainBinary
} else {
  $cargoBinaryName
}
$developmentBinaryName = if ($developmentMainBinary) {
  [string]$developmentMainBinary
} elseif ($productionMainBinary) {
  [string]$productionMainBinary
} else {
  $cargoBinaryName
}

Assert-Contract ($productionBinaryName -ceq "t-hub") "production main binary must resolve to t-hub, got '$productionBinaryName'."
Assert-Contract ($developmentBinaryName -ceq "t-hub-dev") "development main binary must resolve to t-hub-dev, got '$developmentBinaryName'."
Assert-Contract ($developmentBinaryName -cne $productionBinaryName) "production and development main binaries must be distinct."
Assert-Contract ($developmentConfig.productName -ceq "T-Hub Dev") "development productName must be T-Hub Dev."
Assert-Contract ($developmentConfig.identifier -ceq "com.t-hub.dev") "development bundle identifier must be com.t-hub.dev."
$developmentEndpoints = $developmentConfig.plugins.updater.endpoints
Assert-Contract ($developmentEndpoints.Count -eq 0) "development updater endpoints must remain disabled."

$installerScript = (Get-Content -LiteralPath $InstallerScriptPath -Raw).Replace("`r`n", "`n").Replace("`r", "`n")
$mainBinaryName = Get-UniqueNsisDefine $installerScript "MAINBINARYNAME"
$mainBinarySourcePath = Get-UniqueNsisDefine $installerScript "MAINBINARYSRCPATH"
$productName = Get-UniqueNsisDefine $installerScript "PRODUCTNAME"
$bundleId = Get-UniqueNsisDefine $installerScript "BUNDLEID"
Assert-Contract ($mainBinaryName -ceq "t-hub-dev") "installer MAINBINARYNAME must be t-hub-dev."
Assert-Contract ($mainBinarySourcePath -match '(?i)(^|[\\/])t-hub-dev\.exe$') "installer payload source must end in t-hub-dev.exe."
Assert-Contract ($productName -ceq "T-Hub Dev") "installer product marker must be T-Hub Dev."
Assert-Contract ($bundleId -ceq "com.t-hub.dev") "installer bundle marker must be com.t-hub.dev."

$resolvedScript = Resolve-NsisDefines $installerScript
$activeLines = ($resolvedScript -split "`n" | Where-Object { -not $_.TrimStart().StartsWith(";") }) -join "`n"
$productionReference = '(?i)(?<![A-Za-z0-9_-])t-hub\.exe(?![A-Za-z0-9_.-])'
Assert-Contract (-not ($activeLines -match $productionReference)) "installer contains a production t-hub.exe reference."
$installSection = Get-UniqueNsisSection $resolvedScript "Install"
$uninstallSection = Get-UniqueNsisSection $resolvedScript "Uninstall"
$allProcessChecks = [regex]::Matches($resolvedScript, '(?im)^\s*!insertmacro\s+CheckIfAppIsRunning\b[^\r\n]*$')
$installProcessChecks = [regex]::Matches($installSection, '(?im)^\s*!insertmacro\s+CheckIfAppIsRunning\b[^\r\n]*"t-hub-dev\.exe"[^\r\n]*$')
$uninstallProcessChecks = [regex]::Matches($uninstallSection, '(?im)^\s*!insertmacro\s+CheckIfAppIsRunning\b[^\r\n]*"t-hub-dev\.exe"[^\r\n]*$')
Assert-Contract ($allProcessChecks.Count -eq 2) "installer.nsi must contain exactly two CheckIfAppIsRunning calls."
Assert-Contract (($installProcessChecks.Count + $uninstallProcessChecks.Count) -eq $allProcessChecks.Count) "all CheckIfAppIsRunning calls must be confined to the Install and Uninstall sections."
Assert-Contract ($installProcessChecks.Count -eq 1) "Install section must contain exactly one t-hub-dev.exe CheckIfAppIsRunning call."
Assert-Contract ($uninstallProcessChecks.Count -eq 1) "Uninstall section must contain exactly one t-hub-dev.exe CheckIfAppIsRunning call."
foreach ($processLine in [regex]::Matches($activeLines, '(?im)^.*(?:KillProcess|taskkill)[^\r\n]*$')) {
  if ($processLine.Value -match '(?i)\.exe') {
    Assert-Contract ($processLine.Value -match '(?i)(?<![A-Za-z0-9_-])t-hub-dev\.exe(?![A-Za-z0-9_.-])') "kill/process commands may target only t-hub-dev.exe."
  }
}
$mainPayloads = [regex]::Matches($installSection, '(?im)^\s*File\s+"[^"\r\n]*t-hub-dev\.exe"\s*$')
Assert-Contract ($mainPayloads.Count -eq 1) "Install section must copy exactly one t-hub-dev.exe main payload."
$shortcutLines = [regex]::Matches($resolvedScript, '(?im)^\s*CreateShortCut\b[^\r\n]*$')
Assert-Contract ($shortcutLines.Count -gt 0) "installer must create at least one development shortcut."
foreach ($shortcutLine in $shortcutLines) {
  Assert-Contract ($shortcutLine.Value -match '(?i)\$INSTDIR\\t-hub-dev\.exe') "every executable shortcut must target t-hub-dev.exe."
}
$mainDeletes = [regex]::Matches($uninstallSection, '(?im)^\s*Delete\s+"\$INSTDIR\\t-hub-dev\.exe"\s*$')
Assert-Contract ($mainDeletes.Count -eq 1) "Uninstall section must delete t-hub-dev.exe exactly once."
$mainBinaryRegistryWrites = [regex]::Matches($installSection, '(?im)^\s*WriteRegStr\b[^\r\n]*"MainBinaryName"\s+"t-hub-dev\.exe"\s*$')
Assert-Contract ($mainBinaryRegistryWrites.Count -eq 1) "Install section must write MainBinaryName as t-hub-dev.exe exactly once."

foreach ($stateMarker in @("T-Hub Dev", "t-hub-dev", ".t-hub-dev", "t-hub-dev.db")) {
  Assert-Contract ((Get-AsciiMarkerCount -Path $RawBinaryPath -Marker $stateMarker) -gt 0) "raw development binary is missing marker '$stateMarker'."
}

$unknownMarker = "__TAURI_BUNDLE_TYPE_VAR_UNK"
$nsisMarker = "__TAURI_BUNDLE_TYPE_VAR_NSS"
$unknownBytes = [System.Text.Encoding]::ASCII.GetBytes($unknownMarker)
$nsisBytes = [System.Text.Encoding]::ASCII.GetBytes($nsisMarker)
Assert-Contract ($unknownBytes.Length -eq $nsisBytes.Length) "canonical Tauri bundle markers must have equal byte length."
$rawBytes = [System.IO.File]::ReadAllBytes($RawBinaryPath)
$unknownOffsets = @(Find-ByteSequenceOffsets -Bytes $rawBytes -Needle $unknownBytes)
Assert-Contract ($unknownOffsets.Count -eq 1) "raw development binary must contain '$unknownMarker' exactly 1 time(s), found $($unknownOffsets.Count)."
Assert-MarkerCount -Path $ExtractedBinaryPath -Marker $unknownMarker -Expected 0 -Label "installer-extracted development binary"

$rawHash = Get-Sha256 $RawBinaryPath
$installerHash = Get-Sha256 $InstallerPath
$extractedHash = Get-Sha256 $ExtractedBinaryPath
$expectedBytes = New-Object byte[] $rawBytes.Length
[System.Array]::Copy($rawBytes, $expectedBytes, $rawBytes.Length)
[System.Array]::Copy($nsisBytes, 0, $expectedBytes, $unknownOffsets[0], $nsisBytes.Length)
$expectedHash = Get-BytesSha256 $expectedBytes
$extractedLength = (Get-Item -LiteralPath $ExtractedBinaryPath).Length
Assert-Contract ($extractedLength -eq $expectedBytes.Length) "installer-extracted binary must have the same byte length as the raw binary."
Assert-Contract ($extractedHash -ceq $expectedHash) "installer-extracted binary must equal the exact canonical UNK-to-NSS patch of the raw binary."
if ($ExpectedBinaryPath) {
  $expectedParent = Split-Path -Parent $ExpectedBinaryPath
  Assert-Contract ([string]::IsNullOrWhiteSpace($expectedParent) -or (Test-Path -LiteralPath $expectedParent -PathType Container)) "expected binary parent directory is missing."
  [System.IO.File]::WriteAllBytes($ExpectedBinaryPath, $expectedBytes)
}

$installedHash = $null
if ($InstalledBinaryPath) {
  Assert-MarkerCount -Path $InstalledBinaryPath -Marker $unknownMarker -Expected 0 -Label "installed development binary"
  $installedHash = Get-Sha256 $InstalledBinaryPath
  $installedLength = (Get-Item -LiteralPath $InstalledBinaryPath).Length
  Assert-Contract ($installedLength -eq $expectedBytes.Length) "installed binary must have the same byte length as the raw binary."
  Assert-Contract ($installedHash -ceq $expectedHash) "installed binary must equal the exact canonical UNK-to-NSS patch of the raw binary."
}

[ordered]@{
  productionMainBinary = $productionBinaryName
  developmentMainBinary = $developmentBinaryName
  rawSha256 = $rawHash
  installerSha256 = $installerHash
  expectedSha256 = $expectedHash
  extractedSha256 = $extractedHash
  installedSha256 = $installedHash
  bundleMarkerTransformation = "$unknownMarker -> $nsisMarker"
} | ConvertTo-Json
