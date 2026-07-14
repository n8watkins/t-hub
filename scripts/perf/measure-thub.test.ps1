$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

$collector = Join-Path $PSScriptRoot "measure-thub.ps1"
. $collector -FunctionsOnly

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function New-ProcessRow {
    param(
        [int]$Id,
        [int]$Parent,
        [string]$Name,
        [string]$Created,
        [double]$Cpu = 0.0
    )
    return [pscustomobject]@{
        process_id = $Id
        parent_process_id = $Parent
        name = $Name
        executable_path = if ($Name -ieq "t-hub.exe") { "C:\T-Hub\t-hub.exe" } else { "" }
        creation_time_utc = $Created
        cpu_seconds = $Cpu
        working_set_bytes = [int64]100
        private_bytes = [int64]80
        thread_count = 2
    }
}

$root = New-ProcessRow 10 1 "t-hub.exe" "root-a" 1.0
$webview = New-ProcessRow 11 10 "msedgewebview2.exe" "web-a" 2.0
$bridge = New-ProcessRow 12 10 "wsl.exe" "bridge-a" 3.0
$other = New-ProcessRow 99 1 "node.exe" "node-a" 10.0
$snapshot = @($root, $webview, $bridge, $other)

$roots = @(Get-CandidateRoots $snapshot)
Assert-True ($roots.Count -eq 1 -and $roots[0].process_id -eq 10) "candidate root selection failed"
$tree = Get-AppTree $snapshot 10 "root-a"
Assert-True ($tree.processes.Count -eq 3) "pinned tree included an unrelated process"

$secondRoot = New-ProcessRow 20 1 "t-hub.exe" "root-b" 0.0
$ambiguous = @($snapshot + $secondRoot)
$threw = $false
try { Assert-UnambiguousRootSet $ambiguous $root $false } catch { $threw = $true }
Assert-True $threw "implicit selection accepted multiple roots"
Assert-UnambiguousRootSet $ambiguous $root $true

$restarted = @((New-ProcessRow 10 1 "t-hub.exe" "root-new" 0.0))
$threw = $false
try { Get-AppTree $restarted 10 "root-a" | Out-Null } catch { $threw = $true }
Assert-True $threw "pinned root accepted PID reuse/restart"

$next = @(
    (New-ProcessRow 10 1 "t-hub.exe" "root-a" 1.5),
    (New-ProcessRow 11 10 "msedgewebview2.exe" "web-a" 2.5),
    (New-ProcessRow 12 10 "wsl.exe" "bridge-a" 3.5)
)
$stable = Get-TreeTotals $next $tree.processes 2.0 @($root)
Assert-True $stable.cpu_interval_complete "stable interval was marked incomplete"
Assert-True ([Math]::Abs($stable.cpu_core_fraction - 0.75) -lt 0.000001) "stable CPU delta was incorrect"

$withBirth = @($next + (New-ProcessRow 13 10 "wsl.exe" "bridge-new" 0.4))
$incomplete = Get-TreeTotals $withBirth $next 1.0 @($root)
Assert-True (-not $incomplete.cpu_interval_complete) "birth interval was marked complete"
Assert-True ($incomplete.process_births -eq 1 -and $null -eq $incomplete.cpu_core_fraction) "birth accounting was incorrect"

$sampleA = [pscustomobject]@{ interval_seconds = 2.0; totals = $stable }
$stableLong = Get-TreeTotals @(
    (New-ProcessRow 10 1 "t-hub.exe" "root-a" 3.5),
    (New-ProcessRow 11 10 "msedgewebview2.exe" "web-a" 4.5),
    (New-ProcessRow 12 10 "wsl.exe" "bridge-a" 5.5)
) $next 3.0 @($root)
$sampleB = [pscustomobject]@{ interval_seconds = 3.0; totals = $stableLong }
$sampleC = [pscustomobject]@{ interval_seconds = 1.0; totals = $incomplete }
$cpu = Get-CpuSummary @($sampleA, $sampleB, $sampleC)
Assert-True ($cpu.complete_interval_count -eq 2 -and $cpu.incomplete_interval_count -eq 1) "CPU completeness summary was incorrect"
Assert-True (-not $cpu.release_acceptance_eligible) "incomplete run was release-eligible"
Assert-True ([Math]::Abs($cpu.run_total_core_fraction - 1.5) -lt 0.000001) "duration-weighted CPU was incorrect"

Write-Host "measure-thub.test: PASS"
