[CmdletBinding()]
param(
    [ValidateSet(1, 4, 8, 16)]
    [int]$DeclaredScenarioTerminals = 1,

    [ValidateRange(0, 3600)]
    [int]$WarmupSeconds = 30,

    [ValidateRange(1, 86400)]
    [int]$SampleSeconds = 60,

    [ValidateRange(100, 60000)]
    [int]$IntervalMilliseconds = 1000,

    [string]$OutputPath,

    [string]$ExecutablePath = "",
    [string]$ProcessName = "t-hub",
    [ValidateRange(0, 2147483647)]
    [int]$RootProcessId = 0,
    [string]$SetupNote = "",
    [string]$CollectorRepositoryCommit = "unknown",
    [switch]$FunctionsOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

function Get-ProcessSnapshot {
    $rows = @(Get-CimInstance Win32_Process)
    $snapshot = @()
    foreach ($row in $rows) {
        $creation = "unknown"
        if ($null -ne $row.CreationDate) {
            $creation = $row.CreationDate.ToUniversalTime().ToString("o")
        }
        $snapshot += [pscustomobject]@{
            process_id = [int]$row.ProcessId
            parent_process_id = [int]$row.ParentProcessId
            name = [string]$row.Name
            executable_path = [string]$row.ExecutablePath
            creation_time_utc = $creation
            cpu_seconds = ([double]$row.KernelModeTime + [double]$row.UserModeTime) / 10000000.0
            working_set_bytes = [int64]$row.WorkingSetSize
            private_bytes = [int64]$row.PrivatePageCount
            thread_count = [int]$row.ThreadCount
        }
    }
    return @($snapshot)
}

function Test-AppCandidate {
    param($Process)

    if ($ExecutablePath.Length -gt 0) {
        return $Process.executable_path -ieq $ExecutablePath
    }
    return $Process.name -ieq ("{0}.exe" -f $ProcessName)
}

function Get-CandidateRoots {
    param([object[]]$Snapshot)

    $candidateIds = @{}
    foreach ($process in $Snapshot) {
        if (Test-AppCandidate $process) {
            $candidateIds[$process.process_id] = $true
        }
    }

    $roots = @()
    foreach ($process in $Snapshot) {
        if ((Test-AppCandidate $process) -and -not $candidateIds.ContainsKey($process.parent_process_id)) {
            $roots += $process
        }
    }
    return @($roots)
}

function Get-AppTree {
    param(
        [object[]]$Snapshot,
        [int]$PinnedProcessId,
        [string]$PinnedCreationTimeUtc
    )

    $root = @($Snapshot | Where-Object {
        $_.process_id -eq $PinnedProcessId -and $_.creation_time_utc -eq $PinnedCreationTimeUtc
    })
    if ($root.Count -ne 1) {
        throw "Pinned T-Hub root $PinnedProcessId ($PinnedCreationTimeUtc) exited or restarted."
    }

    $children = @{}
    foreach ($process in $Snapshot) {
        $parentKey = [string]$process.parent_process_id
        if (-not $children.ContainsKey($parentKey)) {
            $children[$parentKey] = @()
        }
        $children[$parentKey] += $process.process_id
    }

    $treeIds = @{}
    $queue = New-Object System.Collections.Queue
    $queue.Enqueue($PinnedProcessId)
    while ($queue.Count -gt 0) {
        $processId = [int]$queue.Dequeue()
        if ($treeIds.ContainsKey($processId)) {
            continue
        }
        $treeIds[$processId] = $true
        $childKey = [string]$processId
        if ($children.ContainsKey($childKey)) {
            foreach ($childId in $children[$childKey]) {
                $queue.Enqueue($childId)
            }
        }
    }

    $tree = @($Snapshot | Where-Object { $treeIds.ContainsKey($_.process_id) })
    return [pscustomobject]@{
        roots = @($root)
        processes = $tree
    }
}

function Assert-UnambiguousRootSet {
    param([object[]]$Snapshot, $PinnedRoot, [bool]$ExplicitPid)

    if ($ExplicitPid) {
        $matchingRoots = @(Get-CandidateRoots $Snapshot | Where-Object {
            $_.process_id -eq $PinnedRoot.process_id -and
            $_.creation_time_utc -eq $PinnedRoot.creation_time_utc
        })
        if ($matchingRoots.Count -ne 1) {
            throw "PID $($PinnedRoot.process_id) is no longer the selected T-Hub root."
        }
        return
    }
    $roots = @(Get-CandidateRoots $Snapshot)
    if ($roots.Count -ne 1) {
        throw "Expected exactly one T-Hub root, found $($roots.Count). Pass --pid to select one explicitly."
    }
    if ($roots[0].process_id -ne $PinnedRoot.process_id -or
        $roots[0].creation_time_utc -ne $PinnedRoot.creation_time_utc) {
        throw "The T-Hub root set changed during collection."
    }
}

function Get-ProcessCategory {
    param($Process, [hashtable]$RootIds)

    if ($RootIds.ContainsKey($Process.process_id)) {
        return "application"
    }
    if ($Process.name -ieq "msedgewebview2.exe") {
        return "webview2"
    }
    if (@("wsl.exe", "wslhost.exe", "conhost.exe", "OpenConsole.exe") -icontains $Process.name) {
        return "host_bridge"
    }
    return "other_descendant"
}

function Get-TreeTotals {
    param([object[]]$Processes, [object[]]$PreviousProcesses, [double]$ElapsedSeconds, [object[]]$Roots)

    $previousByKey = @{}
    foreach ($process in $PreviousProcesses) {
        $key = "{0}|{1}" -f $process.process_id, $process.creation_time_utc
        $previousByKey[$key] = $process
    }
    $rootIds = @{}
    foreach ($root in $Roots) {
        $rootIds[$root.process_id] = $true
    }

    $categoryTotals = [ordered]@{}
    foreach ($category in @("application", "webview2", "host_bridge", "other_descendant")) {
        $categoryTotals[$category] = [ordered]@{
            process_count = 0
            thread_count = 0
            working_set_bytes = [int64]0
            private_bytes = [int64]0
            cpu_delta_seconds_observed = [double]0
            cpu_core_fraction = $null
            cpu_core_fraction_observed_lower_bound = [double]0
            process_births = 0
            process_deaths = 0
            cpu_interval_complete = $true
        }
    }

    $currentByKey = @{}
    foreach ($process in $Processes) {
        $key = "{0}|{1}" -f $process.process_id, $process.creation_time_utc
        $currentByKey[$key] = $process
    }

    $cpuDelta = [double]0
    $births = 0
    foreach ($process in $Processes) {
        $category = Get-ProcessCategory $process $rootIds
        $totals = $categoryTotals[$category]
        $totals.process_count += 1
        $totals.thread_count += $process.thread_count
        $totals.working_set_bytes += $process.working_set_bytes
        $totals.private_bytes += $process.private_bytes

        $key = "{0}|{1}" -f $process.process_id, $process.creation_time_utc
        if ($previousByKey.ContainsKey($key)) {
            $delta = [Math]::Max(0.0, $process.cpu_seconds - $previousByKey[$key].cpu_seconds)
            $cpuDelta += $delta
            $totals.cpu_delta_seconds_observed += $delta
        } else {
            $births += 1
            $totals.process_births += 1
        }
    }

    $deaths = 0
    foreach ($process in $PreviousProcesses) {
        $key = "{0}|{1}" -f $process.process_id, $process.creation_time_utc
        if (-not $currentByKey.ContainsKey($key)) {
            $deaths += 1
            $category = Get-ProcessCategory $process $rootIds
            $categoryTotals[$category].process_deaths += 1
        }
    }

    foreach ($category in $categoryTotals.Keys) {
        $totals = $categoryTotals[$category]
        $totals.cpu_core_fraction_observed_lower_bound =
            $totals.cpu_delta_seconds_observed / $ElapsedSeconds
        $totals.cpu_interval_complete =
            $totals.process_births -eq 0 -and $totals.process_deaths -eq 0
        if ($totals.cpu_interval_complete) {
            $totals.cpu_core_fraction = $totals.cpu_core_fraction_observed_lower_bound
        }
    }

    $workingSet = [int64]0
    $privateBytes = [int64]0
    $threadCount = 0
    foreach ($process in $Processes) {
        $workingSet += $process.working_set_bytes
        $privateBytes += $process.private_bytes
        $threadCount += $process.thread_count
    }

    return [ordered]@{
        process_count = $Processes.Count
        thread_count = $threadCount
        working_set_bytes = $workingSet
        private_bytes = $privateBytes
        cpu_delta_seconds_observed = $cpuDelta
        cpu_core_fraction = if ($births -eq 0 -and $deaths -eq 0) { $cpuDelta / $ElapsedSeconds } else { $null }
        cpu_core_fraction_observed_lower_bound = $cpuDelta / $ElapsedSeconds
        process_births = $births
        process_deaths = $deaths
        cpu_interval_complete = $births -eq 0 -and $deaths -eq 0
        categories = $categoryTotals
    }
}

function Get-Statistics {
    param([double[]]$Values)

    if ($Values.Count -eq 0) {
        return $null
    }
    $sorted = @($Values | Sort-Object)
    $sum = [double]0
    foreach ($value in $sorted) {
        $sum += $value
    }
    $middle = [int][Math]::Floor(($sorted.Count - 1) / 2.0)
    $p95Index = [Math]::Max(0, [int][Math]::Ceiling($sorted.Count * 0.95) - 1)
    return [ordered]@{
        min = $sorted[0]
        mean = $sum / $sorted.Count
        p50 = $sorted[$middle]
        p95 = $sorted[$p95Index]
        max = $sorted[$sorted.Count - 1]
    }
}

function Get-CpuSummary {
    param([object[]]$Samples, [string]$Category = "")

    $complete = @($Samples | Where-Object {
        if ($Category.Length -gt 0) {
            $_.totals.categories[$Category].cpu_interval_complete
        } else {
            $_.totals.cpu_interval_complete
        }
    })
    $values = @($complete | ForEach-Object {
        if ($Category.Length -gt 0) {
            [double]$_.totals.categories[$Category].cpu_core_fraction
        } else {
            [double]$_.totals.cpu_core_fraction
        }
    })
    $cpuSeconds = [double]0
    $wallSeconds = [double]0
    foreach ($sample in $complete) {
        $wallSeconds += [double]$sample.interval_seconds
        if ($Category.Length -gt 0) {
            $cpuSeconds += [double]$sample.totals.categories[$Category].cpu_delta_seconds_observed
        } else {
            $cpuSeconds += [double]$sample.totals.cpu_delta_seconds_observed
        }
    }
    return [ordered]@{
        statistics = Get-Statistics $values
        run_total_core_fraction = if ($wallSeconds -gt 0) { $cpuSeconds / $wallSeconds } else { $null }
        complete_interval_count = $complete.Count
        incomplete_interval_count = $Samples.Count - $complete.Count
        complete_wall_seconds = $wallSeconds
        release_acceptance_eligible = $Samples.Count -gt 0 -and $complete.Count -eq $Samples.Count
    }
}

function Get-ArtifactSummary {
    param([object[]]$Samples)

    $summary = [ordered]@{
        process_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.process_count })
        thread_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.thread_count })
        working_set_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.working_set_bytes })
        private_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.private_bytes })
        cpu = Get-CpuSummary $Samples
        process_births = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.process_births })
        process_deaths = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.process_deaths })
        categories = [ordered]@{}
    }
    foreach ($category in @("application", "webview2", "host_bridge", "other_descendant")) {
        $summary.categories[$category] = [ordered]@{
            process_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].process_count })
            thread_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].thread_count })
            working_set_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].working_set_bytes })
            private_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].private_bytes })
            cpu = Get-CpuSummary $Samples $category
            process_births = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].process_births })
            process_deaths = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].process_deaths })
        }
    }
    return $summary
}

if ($FunctionsOnly) {
    return
}
if ($OutputPath.Length -eq 0) {
    throw "OutputPath is required."
}

$initialSnapshot = @(Get-ProcessSnapshot)
$candidateRoots = @(Get-CandidateRoots $initialSnapshot)
$explicitPid = $RootProcessId -gt 0
if ($explicitPid) {
    $selected = @($initialSnapshot | Where-Object { $_.process_id -eq $RootProcessId })
    if ($selected.Count -ne 1) {
        throw "No running process has PID $RootProcessId."
    }
    $firstRoot = $selected[0]
    Assert-UnambiguousRootSet $initialSnapshot $firstRoot $true
} elseif ($candidateRoots.Count -ne 1) {
    $selector = "process name '$ProcessName.exe'"
    if ($ExecutablePath.Length -gt 0) {
        $selector = "executable path '$ExecutablePath'"
    }
    throw "Expected exactly one running T-Hub root for $selector, found $($candidateRoots.Count). Pass --pid to select one explicitly."
} else {
    $firstRoot = $candidateRoots[0]
}
$initialTree = Get-AppTree $initialSnapshot $firstRoot.process_id $firstRoot.creation_time_utc
$binary = $null
if ($firstRoot.executable_path.Length -gt 0 -and (Test-Path -LiteralPath $firstRoot.executable_path)) {
    $item = Get-Item -LiteralPath $firstRoot.executable_path
    $binary = [ordered]@{
        path = $firstRoot.executable_path
        file_version = $item.VersionInfo.FileVersion
        product_version = $item.VersionInfo.ProductVersion
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $firstRoot.executable_path).Hash.ToLowerInvariant()
    }
}

$os = Get-CimInstance Win32_OperatingSystem
$startedAt = (Get-Date).ToUniversalTime()
if ($WarmupSeconds -gt 0) {
    Write-Host "Warming up for $WarmupSeconds seconds..."
    Start-Sleep -Seconds $WarmupSeconds
}

$previousSnapshot = @(Get-ProcessSnapshot)
Assert-UnambiguousRootSet $previousSnapshot $firstRoot $explicitPid
$previousTree = Get-AppTree $previousSnapshot $firstRoot.process_id $firstRoot.creation_time_utc
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$lastElapsed = [double]0
$samples = @()
$sampleIndex = 0
Write-Host "Sampling the T-Hub process tree for $SampleSeconds seconds..."
while ($stopwatch.Elapsed.TotalSeconds -lt $SampleSeconds) {
    Start-Sleep -Milliseconds $IntervalMilliseconds
    $currentSnapshot = @(Get-ProcessSnapshot)
    $elapsed = $stopwatch.Elapsed.TotalSeconds
    $intervalSeconds = $elapsed - $lastElapsed
    $lastElapsed = $elapsed
    Assert-UnambiguousRootSet $currentSnapshot $firstRoot $explicitPid
    $currentTree = Get-AppTree $currentSnapshot $firstRoot.process_id $firstRoot.creation_time_utc
    $sampleIndex += 1
    $samples += [pscustomobject]@{
        index = $sampleIndex
        elapsed_seconds = $elapsed
        interval_seconds = $intervalSeconds
        totals = Get-TreeTotals $currentTree.processes $previousTree.processes $intervalSeconds $currentTree.roots
    }
    $previousTree = $currentTree
}

$finishedAt = (Get-Date).ToUniversalTime()
$rootMetadata = @($initialTree.roots | ForEach-Object {
    [ordered]@{
        process_id = $_.process_id
        name = $_.name
        executable_path = $_.executable_path
        creation_time_utc = $_.creation_time_utc
    }
})
$artifact = [ordered]@{
    schema_version = 2
    benchmark = "t-hub-packaged-runtime"
    metadata = [ordered]@{
        started_at_utc = $startedAt.ToString("o")
        finished_at_utc = $finishedAt.ToString("o")
        computer_name = $env:COMPUTERNAME
        os_caption = [string]$os.Caption
        os_version = [string]$os.Version
        logical_processor_count = [int]$env:NUMBER_OF_PROCESSORS
        powershell_version = $PSVersionTable.PSVersion.ToString()
        collector_repository_commit = $CollectorRepositoryCommit
        binary_provenance_note = "The collector repository commit does not prove which source commit produced the installed binary; use installed_binary.sha256 for identity."
        installed_binary = $binary
    }
    configuration = [ordered]@{
        declared_scenario_terminals = $DeclaredScenarioTerminals
        observed_terminal_count = $null
        observed_terminal_metadata = $null
        warmup_seconds = $WarmupSeconds
        requested_sample_seconds = $SampleSeconds
        actual_sample_seconds = $stopwatch.Elapsed.TotalSeconds
        sample_count = $samples.Count
        interval_milliseconds = $IntervalMilliseconds
        process_name = $ProcessName
        executable_path_filter = $ExecutablePath
        selected_root_process_id = $firstRoot.process_id
        selected_root_creation_time_utc = $firstRoot.creation_time_utc
        setup_note = $SetupNote
        cpu_definition = "CPU seconds consumed divided by wall seconds; 1.0 equals one fully utilized logical core. Intervals with process births or deaths are incomplete and excluded from CPU release statistics. Their observed lower bound is diagnostic only."
        quantile_definition = "p50 and p95 use the nearest-rank empirical quantile: sorted[ceil(p*n)-1]."
    }
    setup_assumptions = @(
        "The installed T-Hub app was already running before collection began.",
        "The terminal scenario count is declared by the operator and was not verified through T-Hub control.",
        "Terminal creation, closure, and workload changes were avoided during warmup and sampling.",
        "Unrelated WSL, agent-browser, Next.js, and Codex processes are excluded unless they are descendants of the selected T-Hub root."
    )
    roots = $rootMetadata
    samples = $samples
    summary = Get-ArtifactSummary $samples
}

$parent = Split-Path -Parent $OutputPath
if ($parent.Length -gt 0) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
}
$artifact | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $OutputPath -Encoding UTF8
Write-Host "Wrote benchmark artifact: $OutputPath"
