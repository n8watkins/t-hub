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

$installerScript = Get-Content -LiteralPath $InstallerScriptPath -Raw
$mainBinaryMatches = [regex]::Matches($installerScript, '(?im)^\s*!define\s+MAINBINARYNAME\s+"([^"]+)"\s*$')
Assert-Contract ($mainBinaryMatches.Count -eq 1) "installer.nsi must define MAINBINARYNAME exactly once."
Assert-Contract ($mainBinaryMatches[0].Groups[1].Value -ceq "t-hub-dev") "installer MAINBINARYNAME must be t-hub-dev."
$sourcePathMatches = [regex]::Matches($installerScript, '(?im)^\s*!define\s+MAINBINARYSRCPATH\s+"([^"]+)"\s*$')
Assert-Contract ($sourcePathMatches.Count -eq 1) "installer.nsi must define MAINBINARYSRCPATH exactly once."
Assert-Contract ($sourcePathMatches[0].Groups[1].Value -match '(?i)(^|[\\/])t-hub-dev\.exe$') "installer payload source must end in t-hub-dev.exe."
Assert-Contract ($installerScript -match '(?im)^\s*!define\s+PRODUCTNAME\s+"T-Hub Dev"\s*$') "installer product marker must be T-Hub Dev."
Assert-Contract ($installerScript -match '(?im)^\s*!define\s+BUNDLEID\s+"com\.t-hub\.dev"\s*$') "installer bundle marker must be com.t-hub.dev."

$resolvedScript = $installerScript.Replace('${MAINBINARYNAME}', "t-hub-dev")
$processChecks = [regex]::Matches($resolvedScript, '(?im)^\s*!insertmacro\s+CheckIfAppIsRunning\b[^\r\n]*$')
Assert-Contract ($processChecks.Count -eq 2) "installer and uninstaller must each call CheckIfAppIsRunning exactly once."
foreach ($processCheck in $processChecks) {
  Assert-Contract ($processCheck.Value -match '(?i)"t-hub-dev\.exe"') "every CheckIfAppIsRunning target must be t-hub-dev.exe."
}
Assert-Contract (-not ($resolvedScript -match '(?im)^\s*!insertmacro\s+CheckIfAppIsRunning\b[^\r\n]*"t-hub\.exe"')) "installer must never target the production t-hub.exe process."
Assert-Contract ($resolvedScript -match '(?im)^\s*File\s+"\$\{MAINBINARYSRCPATH\}"\s*$') "installer must copy the declared development main-binary payload."
Assert-Contract ($resolvedScript -match '(?im)^\s*CreateShortCut\b[^\r\n]*\$INSTDIR\\t-hub-dev\.exe') "installer shortcuts must target t-hub-dev.exe."
Assert-Contract ($resolvedScript -match '(?im)^\s*Delete\s+"\$INSTDIR\\t-hub-dev\.exe"\s*$') "uninstaller must delete t-hub-dev.exe."

foreach ($stateMarker in @("T-Hub Dev", "com.t-hub.dev", "t-hub-dev", ".t-hub-dev")) {
  Assert-Contract ((Get-AsciiMarkerCount -Path $RawBinaryPath -Marker $stateMarker) -gt 0) "raw development binary is missing marker '$stateMarker'."
}

$unknownMarker = "__TAURI_BUNDLE_TYPE_VAR_UNK"
$nsisMarker = "__TAURI_BUNDLE_TYPE_VAR_NSS"
Assert-MarkerCount -Path $RawBinaryPath -Marker $unknownMarker -Expected 1 -Label "raw development binary"
Assert-MarkerCount -Path $RawBinaryPath -Marker $nsisMarker -Expected 0 -Label "raw development binary"
Assert-MarkerCount -Path $ExtractedBinaryPath -Marker $unknownMarker -Expected 0 -Label "installer-extracted development binary"
Assert-MarkerCount -Path $ExtractedBinaryPath -Marker $nsisMarker -Expected 1 -Label "installer-extracted development binary"

$rawHash = Get-Sha256 $RawBinaryPath
$installerHash = Get-Sha256 $InstallerPath
$extractedHash = Get-Sha256 $ExtractedBinaryPath
Assert-Contract ($rawHash -cne $extractedHash) "installer-extracted hash must differ from the raw binary after canonical UNK-to-NSS patching."

$installedHash = $null
if ($InstalledBinaryPath) {
  Assert-MarkerCount -Path $InstalledBinaryPath -Marker $unknownMarker -Expected 0 -Label "installed development binary"
  Assert-MarkerCount -Path $InstalledBinaryPath -Marker $nsisMarker -Expected 1 -Label "installed development binary"
  $installedHash = Get-Sha256 $InstalledBinaryPath
  Assert-Contract ($installedHash -ceq $extractedHash) "installed binary hash must equal the installer-extracted binary hash."
  Assert-Contract ($installedHash -cne $rawHash) "installed binary hash must remain distinct from the raw binary hash."
}

[ordered]@{
  productionMainBinary = $productionBinaryName
  developmentMainBinary = $developmentBinaryName
  rawSha256 = $rawHash
  installerSha256 = $installerHash
  extractedSha256 = $extractedHash
  installedSha256 = $installedHash
  bundleMarkerTransformation = "$unknownMarker -> $nsisMarker"
} | ConvertTo-Json
