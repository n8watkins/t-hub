[CmdletBinding()]
param(
    [ValidateSet(1, 4, 8, 16)]
    [int]$ScenarioTerminals = 1,

    [ValidateRange(0, 3600)]
    [int]$WarmupSeconds = 30,

    [ValidateRange(1, 86400)]
    [int]$SampleSeconds = 60,

    [ValidateRange(100, 60000)]
    [int]$IntervalMilliseconds = 1000,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath,

    [string]$ExecutablePath = "",
    [string]$ProcessName = "t-hub",
    [string]$SetupNote = "",
    [string]$RepositoryCommit = "unknown"
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

function Get-AppTree {
    param([object[]]$Snapshot)

    $candidateIds = @{}
    foreach ($process in $Snapshot) {
        if (Test-AppCandidate $process) {
            $candidateIds[$process.process_id] = $true
        }
    }

    $rootIds = @()
    foreach ($process in $Snapshot) {
        if ((Test-AppCandidate $process) -and -not $candidateIds.ContainsKey($process.parent_process_id)) {
            $rootIds += $process.process_id
        }
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
    foreach ($rootId in $rootIds) {
        $queue.Enqueue($rootId)
    }
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
        roots = @($Snapshot | Where-Object { $rootIds -contains $_.process_id })
        processes = $tree
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
            cpu_core_fraction = [double]0
        }
    }

    $cpuDelta = [double]0
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
            $totals.cpu_core_fraction += $delta / $ElapsedSeconds
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
        cpu_core_fraction = $cpuDelta / $ElapsedSeconds
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

function Get-ArtifactSummary {
    param([object[]]$Samples)

    $summary = [ordered]@{
        process_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.process_count })
        thread_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.thread_count })
        working_set_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.working_set_bytes })
        private_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.private_bytes })
        cpu_core_fraction = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.cpu_core_fraction })
        categories = [ordered]@{}
    }
    foreach ($category in @("application", "webview2", "host_bridge", "other_descendant")) {
        $summary.categories[$category] = [ordered]@{
            process_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].process_count })
            thread_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].thread_count })
            working_set_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].working_set_bytes })
            private_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].private_bytes })
            cpu_core_fraction = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.categories[$category].cpu_core_fraction })
        }
    }
    return $summary
}

$initialSnapshot = @(Get-ProcessSnapshot)
$initialTree = Get-AppTree $initialSnapshot
if ($initialTree.roots.Count -eq 0) {
    $selector = "process name '$ProcessName.exe'"
    if ($ExecutablePath.Length -gt 0) {
        $selector = "executable path '$ExecutablePath'"
    }
    throw "No running T-Hub root matched $selector. Start the installed app before benchmarking."
}

$firstRoot = $initialTree.roots[0]
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
$previousTree = Get-AppTree $previousSnapshot
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
    $currentTree = Get-AppTree $currentSnapshot
    if ($currentTree.roots.Count -eq 0) {
        throw "T-Hub exited or no longer matched the process selector during the sample window."
    }
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
    schema_version = 1
    benchmark = "t-hub-packaged-runtime"
    metadata = [ordered]@{
        started_at_utc = $startedAt.ToString("o")
        finished_at_utc = $finishedAt.ToString("o")
        computer_name = $env:COMPUTERNAME
        os_caption = [string]$os.Caption
        os_version = [string]$os.Version
        logical_processor_count = [int]$env:NUMBER_OF_PROCESSORS
        powershell_version = $PSVersionTable.PSVersion.ToString()
        repository_commit = $RepositoryCommit
        installed_binary = $binary
    }
    configuration = [ordered]@{
        scenario_terminals = $ScenarioTerminals
        warmup_seconds = $WarmupSeconds
        requested_sample_seconds = $SampleSeconds
        actual_sample_seconds = $stopwatch.Elapsed.TotalSeconds
        sample_count = $samples.Count
        interval_milliseconds = $IntervalMilliseconds
        process_name = $ProcessName
        executable_path_filter = $ExecutablePath
        setup_note = $SetupNote
        cpu_definition = "CPU seconds consumed divided by wall seconds; 1.0 equals one fully utilized logical core."
    }
    setup_assumptions = @(
        "The installed T-Hub app was already running before collection began.",
        "Exactly the declared number of terminal tiles was prepared manually in one T-Hub window.",
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
