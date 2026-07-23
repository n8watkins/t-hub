[CmdletBinding()]
param(
    [ValidateSet(1, 4, 8, 16)]
    [int]$DeclaredScenarioTerminals = 1,

    [ValidateSet("idle", "terminal_output", "folder_browsing", "preview_starting", "preview_noisy", "preview_refreshing", "voice_synthesis", "endpoint_recovery", "history_open")]
    [string]$ScenarioKind = "idle",
    [string]$WorkloadVersion = "v1",
    [string]$WorkloadSeed = "default",
    [ValidateRange(1, 3)]
    [int]$Repetition = 1,

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
    [string]$ReferenceBinarySha256 = "",
    [string]$ReferenceSelectionReason = "",
    [string]$SourceCommit = "",
    [string]$InstallerSha256 = "",
    [string]$Package5ManifestPath = "",
    [int]$ProtocolVersion = 2,
    [string]$WslVersion = "",
    [string]$WslDistro = "",
    [int64]$WslMemoryBytes = 0,
    [ValidateRange(0, 16)]
    [int]$ObservedTerminalCount = 0,
    [string]$PowerMode = "",
    [int]$DisplayScale = 0,
    [string]$RuntimeEvidencePath = "",
    [switch]$FunctionsOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0
$script:ArtifactWrittenByThisRun = $false

function Write-DiagnosticArtifact {
    param([string]$Reason)
    if ([string]::IsNullOrWhiteSpace($OutputPath) -or (Test-Path -LiteralPath $OutputPath)) { return }
    try {
        $parent = Split-Path -Parent $OutputPath
        if ($parent.Length -gt 0) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
        $diagnostic = [ordered]@{
            schemaVersion = 3
            candidate = [ordered]@{ sourceCommit = $null; installedBinarySha256 = $null; installerSha256 = $null; protocolVersion = $ProtocolVersion }
            reference = [ordered]@{ installedBinarySha256 = $null; selectionReason = $null }
            host = [ordered]@{ windowsVersion = $null; wslVersion = $null; distro = $null; logicalProcessors = $null; memoryBytes = $null; powerMode = $null; displayScale = $null }
            scenario = [ordered]@{ kind = $ScenarioKind; terminalCount = $DeclaredScenarioTerminals; observedTerminalCount = $null; workloadVersion = $null; workloadSeed = $null; repetition = $Repetition; startedAt = $null; finishedAt = $null }
            resources = [ordered]@{ windows = $null; wslOwned = [ordered]@{ available = $false; reason = "diagnostic artifact" }; webview = @{}; samples = @() }
            operations = @()
            preview = @{}
            voice = @{}
            journal = @{}
            diagnostics = [ordered]@{ errorCode = "collector_exception"; heartbeatStalls = @(); longTasks = @(); resizeObserverErrors = @(); redactionCount = 0 }
            validity = [ordered]@{ eligible = $false; reasons = @("collector_exception"); processBirthIntervalsExcluded = 0 }
            budgets = @()
            decision = "ineligible"
            rawEvidence = @()
            redactionCount = 0
        }
        Write-JsonNoClobber $OutputPath $diagnostic 8
    } catch { }
}

function Write-JsonNoClobber {
    param([string]$Path, [object]$Value, [int]$Depth = 12)
    $json = $Value | ConvertTo-Json -Depth $Depth
    $temp = "$Path.$([guid]::NewGuid().ToString('N')).tmp"
    $stream = $null
    $published = $false
    try {
        $stream = [System.IO.File]::Open($temp, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
        $encoding = New-Object System.Text.UTF8Encoding($false)
        $writer = New-Object System.IO.StreamWriter($stream, $encoding, 1024, $true)
        try { $writer.Write($json); $writer.Flush() } finally { $writer.Dispose() }
        $stream.Flush($true)
        $stream.Dispose()
        $stream = $null
        [System.IO.File]::Move($temp, $Path)
        $published = $true
        $script:ArtifactWrittenByThisRun = $true
    } finally {
        if ($null -ne $stream) { $stream.Dispose() }
        if (-not $published -and (Test-Path -LiteralPath $temp)) { Remove-Item -LiteralPath $temp -Force }
    }
}

trap {
    Write-DiagnosticArtifact $_.Exception.Message
    if ($script:ArtifactWrittenByThisRun) { exit 5 }
    exit 6
}

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

function Assert-Sha256([string]$Value, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Value) -or $Value -notmatch '^[0-9a-fA-F]{64}$') { throw "$Label must be a 64-hex SHA-256" }
    return $Value.ToLowerInvariant()
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
    $wslNames = @("wsl.exe", "wslhost.exe")
    $wslTotals = [ordered]@{
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
        $isWsl = $wslNames -icontains $process.name
        if ($isWsl) {
            $wslTotals.process_count += 1
            $wslTotals.thread_count += $process.thread_count
            $wslTotals.working_set_bytes += $process.working_set_bytes
            $wslTotals.private_bytes += $process.private_bytes
        }

        $key = "{0}|{1}" -f $process.process_id, $process.creation_time_utc
        if ($previousByKey.ContainsKey($key)) {
            $delta = [Math]::Max(0.0, $process.cpu_seconds - $previousByKey[$key].cpu_seconds)
            $cpuDelta += $delta
            $totals.cpu_delta_seconds_observed += $delta
            if ($isWsl) { $wslTotals.cpu_delta_seconds_observed += $delta }
        } else {
            $births += 1
            $totals.process_births += 1
            if ($isWsl) { $wslTotals.process_births += 1 }
        }
    }

    $deaths = 0
    foreach ($process in $PreviousProcesses) {
        $key = "{0}|{1}" -f $process.process_id, $process.creation_time_utc
        if (-not $currentByKey.ContainsKey($key)) {
            $deaths += 1
            $category = Get-ProcessCategory $process $rootIds
            $categoryTotals[$category].process_deaths += 1
            if ($wslNames -icontains $process.name) { $wslTotals.process_deaths += 1 }
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
    $wslTotals.cpu_core_fraction_observed_lower_bound = $wslTotals.cpu_delta_seconds_observed / $ElapsedSeconds
    $wslTotals.cpu_interval_complete = $wslTotals.process_births -eq 0 -and $wslTotals.process_deaths -eq 0
    if ($wslTotals.cpu_interval_complete) { $wslTotals.cpu_core_fraction = $wslTotals.cpu_core_fraction_observed_lower_bound }

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
        wsl_descendants = $wslTotals
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
    $summary.wsl_descendants = [ordered]@{
        process_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.wsl_descendants.process_count })
        thread_count = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.wsl_descendants.thread_count })
        working_set_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.wsl_descendants.working_set_bytes })
        private_bytes = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.wsl_descendants.private_bytes })
        process_births = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.wsl_descendants.process_births })
        process_deaths = Get-Statistics @($Samples | ForEach-Object { [double]$_.totals.wsl_descendants.process_deaths })
        cpu = [ordered]@{
            complete_interval_count = @($Samples | Where-Object { $_.totals.wsl_descendants.cpu_interval_complete }).Count
            incomplete_interval_count = @($Samples | Where-Object { -not $_.totals.wsl_descendants.cpu_interval_complete }).Count
            run_total_core_fraction = $null
        }
    }
    $wslWall = [double]0
    $wslCpu = [double]0
    foreach ($sample in $Samples) {
        if ($sample.totals.wsl_descendants.cpu_interval_complete) {
            $wslWall += [double]$sample.interval_seconds
            $wslCpu += [double]$sample.totals.wsl_descendants.cpu_delta_seconds_observed
        }
    }
    if ($wslWall -gt 0) { $summary.wsl_descendants.cpu.run_total_core_fraction = $wslCpu / $wslWall }
    return $summary
}

function Convert-SafeEvidenceNode {
    param([object]$Node, [string]$Key = "root", [int]$Depth = 0)
    if ($Depth -gt 8) { throw "runtime evidence nesting exceeds the bound" }
    if ($null -eq $Node) { return $null }
    if ($Node -is [bool] -or $Node -is [byte] -or $Node -is [sbyte] -or $Node -is [int16] -or $Node -is [uint16] -or $Node -is [int32] -or $Node -is [uint32] -or $Node -is [int64] -or $Node -is [uint64] -or $Node -is [single] -or $Node -is [double] -or $Node -is [decimal]) {
        if (($Node -is [single] -or $Node -is [double] -or $Node -is [decimal]) -and ([double]::IsNaN([double]$Node) -or [double]::IsInfinity([double]$Node))) { throw "runtime evidence contains a non-finite number" }
        return $Node
    }
    if ($Node -is [string]) {
        $enum = @{
            kind = @("operation", "preview", "voice", "journal", "recovery", "diagnostic", "metric")
            status = @("ok", "ready", "running", "stopped", "failed", "pass", "unavailable", "succeeded", "idle", "busy")
            source = @("tauri", "scribe", "rust", "frontend", "backend", "windows", "linux", "wsl")
            type = @("static", "vite", "nextjs", "monorepo")
            outcome = @("succeeded", "failed", "unavailable", "skipped")
            engine = @("kokoro", "piper")
            device = @("default", "unknown")
            unit = @("ms", "bytes", "count", "fraction", "seconds")
        }
        if (-not $enum.ContainsKey($Key) -or $enum[$Key] -notcontains $Node) {
            if ($Key -eq "version" -and $Node -match '^v[0-9]+(?:\.[0-9]+){0,2}$') { return $Node }
            throw "runtime evidence string field '$Key' is not a canonical enum"
        }
        if ($Node.Length -gt 128 -or $Node -match '[\x00-\x1f]') { throw "runtime evidence string field '$Key' is invalid" }
        return $Node
    }
    if ($Node -is [System.Collections.IEnumerable] -and -not ($Node -is [string])) {
        $items = @()
        foreach ($item in $Node) { $items += Convert-SafeEvidenceNode $item $Key ($Depth + 1) }
        if ($items.Count -gt 256) { throw "runtime evidence array '$Key' exceeds the bound" }
        return @($items)
    }
    $output = [ordered]@{}
    foreach ($property in $Node.PSObject.Properties) {
        if ($property.Name -match '(?i)(token|secret|credential|password|transcript|prompt|argument|payload|raw|content|command|path)') { throw "runtime evidence contains prohibited field '$($property.Name)'" }
        $output[$property.Name] = Convert-SafeEvidenceNode $property.Value $property.Name ($Depth + 1)
    }
    return $output
}

function Read-SafeRuntimeEvidence {
    if ([string]::IsNullOrWhiteSpace($RuntimeEvidencePath)) { return [ordered]@{} }
    if (-not (Test-Path -LiteralPath $RuntimeEvidencePath -PathType Leaf)) { throw "runtime evidence file is missing" }
    if ((Get-Item -LiteralPath $RuntimeEvidencePath).Length -gt 1048576) { throw "runtime evidence file exceeds the 1 MiB bound" }
    $root = Get-Content -LiteralPath $RuntimeEvidencePath -Raw | ConvertFrom-Json
    $allowed = @("operations", "preview", "voice", "journal", "recovery", "diagnostics", "metrics", "paired", "cleanup", "wslOwned")
    $result = [ordered]@{}
    foreach ($property in $root.PSObject.Properties) {
        if ($property.Name -notin $allowed) { throw "runtime evidence root field '$($property.Name)' is not allowlisted" }
        $result[$property.Name] = Convert-SafeEvidenceNode $property.Value $property.Name
    }
    return $result
}

function Assert-BoundedCliString {
    param([string]$Value, [string]$Name, [int]$Maximum = 256, [switch]$RejectSensitive)
    if ($Value.Length -gt $Maximum -or $Value -match '[\x00-\x1f]') {
        throw "$Name exceeds the bounded CLI string contract"
    }
    if ($RejectSensitive -and $Value -match '(?i)\b(token|secret|password|credential|transcript|prompt|payload|content|command|bearer|authorization|api_key|session)\b') {
        throw "$Name contains a prohibited sensitive-content marker"
    }
}

if ($FunctionsOnly) {
    return
}
if ($OutputPath.Length -eq 0) {
    throw "OutputPath is required."
}
Assert-BoundedCliString $WorkloadVersion "workload version" 64 -RejectSensitive
Assert-BoundedCliString $WorkloadSeed "workload seed" 128 -RejectSensitive
Assert-BoundedCliString $ReferenceSelectionReason "reference selection reason" 256 -RejectSensitive
Assert-BoundedCliString $PowerMode "power mode" 128 -RejectSensitive
Assert-BoundedCliString $WslVersion "WSL version" 128 -RejectSensitive
Assert-BoundedCliString $WslDistro "WSL distro" 128 -RejectSensitive
Assert-BoundedCliString $SetupNote "setup note" 256 -RejectSensitive
Assert-BoundedCliString $ProcessName "process name" 128
Assert-BoundedCliString $CollectorRepositoryCommit "collector repository commit" 128
Assert-BoundedCliString $ExecutablePath "executable path" 1024
Assert-BoundedCliString $RuntimeEvidencePath "runtime evidence path" 1024
Assert-BoundedCliString $Package5ManifestPath "Package 5 manifest path" 1024
if ($WorkloadVersion -notmatch '^v[0-9]+$') { throw "workload version must use the canonical vN form" }
if ($WorkloadSeed -notmatch '^[A-Za-z0-9._-]+$') { throw "workload seed must use the canonical token form" }
if ($WslVersion.Length -gt 0 -and $WslVersion -notmatch '^v[0-9]+(?:\.[0-9]+){0,2}$') { throw "WSL version must use the canonical vN form" }
if ($WslDistro.Length -gt 0 -and $WslDistro -notmatch '^[A-Za-z0-9._-]+$') { throw "WSL distro must use the canonical token form" }
if ($PowerMode.Length -gt 0 -and $PowerMode -notmatch '^(ac|dc|balanced|high_performance)$') { throw "power mode must use a canonical enum" }
if ($ReferenceSelectionReason.Length -gt 0 -and $ReferenceSelectionReason -notmatch '^[A-Za-z0-9 ._/-]+$') { throw "reference selection reason contains unsupported text" }

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
        path = $null
        file_version = $item.VersionInfo.FileVersion
        product_version = $item.VersionInfo.ProductVersion
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $firstRoot.executable_path).Hash.ToLowerInvariant()
    }
}
$installedBinarySha256 = if ($binary) { Assert-Sha256 $binary.sha256 "installed binary hash" } else { $null }
if ($ReferenceBinarySha256.Length -gt 0) { $ReferenceBinarySha256 = Assert-Sha256 $ReferenceBinarySha256 "reference binary hash" }
if ($InstallerSha256.Length -gt 0) { $InstallerSha256 = Assert-Sha256 $InstallerSha256 "installer hash" }
if ($SourceCommit.Length -gt 0 -and $SourceCommit -notmatch '^[0-9a-fA-F]{40}$') { throw "source commit must be a full 40-hex Git commit" }
$package5Manifest = $null
if ($Package5ManifestPath.Length -gt 0) {
    if (-not (Test-Path -LiteralPath $Package5ManifestPath -PathType Leaf)) { throw "Package 5 provenance manifest is missing" }
    $package5Manifest = Get-Content -LiteralPath $Package5ManifestPath -Raw | ConvertFrom-Json
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
    $sampleTotals = Get-TreeTotals $currentTree.processes $previousTree.processes $intervalSeconds $currentTree.roots
    $samples += [pscustomobject]@{
        index = $sampleIndex
        elapsed_seconds = $elapsed
        interval_seconds = $intervalSeconds
        metrics = [ordered]@{
            elapsed_ms = [double]($elapsed * 1000.0)
            interval_ms = [double]($intervalSeconds * 1000.0)
            cpu_core_fraction = $sampleTotals.cpu_core_fraction
            working_set_bytes = [double]$sampleTotals.working_set_bytes
            private_bytes = [double]$sampleTotals.private_bytes
            process_count = [int]$sampleTotals.process_count
            thread_count = [int]$sampleTotals.thread_count
            wsl_descendant_process_count = [int]$sampleTotals.wsl_descendants.process_count
        }
        totals = $sampleTotals
    }
    $previousTree = $currentTree
}

$finishedAt = (Get-Date).ToUniversalTime()
$runtimeEvidence = Read-SafeRuntimeEvidence
$runtimeEvidenceHash = if ($RuntimeEvidencePath.Length -gt 0) { (Get-FileHash -LiteralPath $RuntimeEvidencePath -Algorithm SHA256).Hash.ToLowerInvariant() } else { $null }
# A caller-supplied JSON file is redacted evidence, not an authority source.
# Until the T-Hub control/runtime producer writes an attested DTO, none of its
# terminal, WSL, paired, or cleanup claims may influence eligibility.
$trustedObservedTerminalCount = $null
$summary = Get-ArtifactSummary $samples
$validityReasons = @()
if ($null -eq $trustedObservedTerminalCount) { $validityReasons += "observed_terminal_count_unavailable" }
if ($runtimeEvidence.Contains("metrics")) { $validityReasons += "runtime_evidence_attestation_missing" }
if ($samples.Count -eq 0) { $validityReasons += "no_samples" }
if (@($samples | Where-Object { -not $_.totals.cpu_interval_complete }).Count -gt 0) { $validityReasons += "incomplete_cpu_interval" }
if ($installedBinarySha256.Length -eq 0) { $validityReasons += "installed_binary_hash_missing" }
if ($SourceCommit.Length -eq 0) { $validityReasons += "source_commit_binding_missing" }
if ($InstallerSha256.Length -eq 0) { $validityReasons += "installer_hash_missing" }
if ($ReferenceBinarySha256.Length -eq 0) { $validityReasons += "reference_binary_hash_missing" }
if ($ReferenceSelectionReason.Length -eq 0) { $validityReasons += "reference_selection_reason_missing" }
if ($ProtocolVersion -lt 1) { $validityReasons += "protocol_version_missing" }
if ($PowerMode.Length -eq 0) { $validityReasons += "power_mode_missing" }
if ($DisplayScale -le 0) { $validityReasons += "display_scale_missing" }
if ($WslVersion.Length -eq 0) { $validityReasons += "wsl_version_missing" }
if ($WslDistro.Length -eq 0) { $validityReasons += "wsl_distro_missing" }
if ($WslMemoryBytes -le 0) { $validityReasons += "wsl_memory_missing" }
if ($PowerMode.Length -gt 0 -or $DisplayScale -gt 0 -or $WslVersion.Length -gt 0 -or $WslDistro.Length -gt 0 -or $WslMemoryBytes -gt 0) { $validityReasons += "host_context_attestation_missing" }
$manifestCandidate = if ($null -ne $package5Manifest) { $package5Manifest.candidate } else { $null }
$manifestInstalled = if ($null -ne $package5Manifest) { $package5Manifest.artifacts.installedBinary } else { $null }
$manifestInstaller = if ($null -ne $package5Manifest) { $package5Manifest.artifacts.installer } else { $null }
if ($null -eq $package5Manifest) { $validityReasons += "package5_manifest_missing" }
elseif ($null -eq $manifestCandidate -or $manifestCandidate.sourceCommit -ne $SourceCommit) { $validityReasons += "package5_source_binding_mismatch" }
elseif ($null -eq $manifestInstalled -or $manifestInstalled.sha256 -ne $installedBinarySha256) { $validityReasons += "package5_installed_binary_binding_mismatch" }
elseif ($null -eq $manifestInstaller -or $manifestInstaller.sha256 -ne $InstallerSha256) { $validityReasons += "package5_installer_binding_mismatch" }
elseif ($manifestCandidate.protocolVersion -ne $ProtocolVersion) { $validityReasons += "package5_protocol_binding_mismatch" }
$wslOwnedObserved = $false
if (-not $wslOwnedObserved) { $validityReasons += "wsl_owned_evidence_unavailable" }

$absoluteLimit = switch ($DeclaredScenarioTerminals) {
    1 { [ordered]@{ cpuRun = 0.15; cpuP95 = 0.30; privateBytes = 700MB; workingSet = 850MB; processes = 24 } }
    4 { [ordered]@{ cpuRun = 0.25; cpuP95 = 0.45; privateBytes = 1000MB; workingSet = 1200MB; processes = 36 } }
    8 { [ordered]@{ cpuRun = 0.40; cpuP95 = 0.70; privateBytes = 1500MB; workingSet = 1800MB; processes = 52 } }
    16 { [ordered]@{ cpuRun = 0.70; cpuP95 = 1.10; privateBytes = 2300MB; workingSet = 2800MB; processes = 84 } }
}
$absoluteMetricsAvailable = $null -ne $summary.cpu.run_total_core_fraction -and $null -ne $summary.cpu.statistics -and $null -ne $summary.private_bytes -and $null -ne $summary.working_set_bytes -and $null -ne $summary.process_count
$absoluteStatus = if (-not $absoluteMetricsAvailable) { "unavailable" } elseif ($summary.cpu.run_total_core_fraction -gt $absoluteLimit.cpuRun -or $summary.cpu.statistics.p95 -gt $absoluteLimit.cpuP95 -or $summary.private_bytes.p95 -gt $absoluteLimit.privateBytes -or $summary.working_set_bytes.p95 -gt $absoluteLimit.workingSet -or $summary.process_count.p95 -gt $absoluteLimit.processes) { "fail" } else { "pass" }
$scenarioStatus = if ($null -eq $trustedObservedTerminalCount -or $trustedObservedTerminalCount -ne $DeclaredScenarioTerminals -or $Repetition -lt 1 -or $Repetition -gt 3) { "unavailable" } else { "pass" }
$pairedStatus = "unavailable"
$cleanupStatus = "unavailable"
$budgets = @(
    [ordered]@{ id = "absolute.resources"; kind = "absolute"; status = $absoluteStatus; observed = $summary; limits = $absoluteLimit },
    [ordered]@{ id = "paired.regression"; kind = "paired"; status = $pairedStatus; observed = $null; limits = $null },
    [ordered]@{ id = "cleanup.invariant"; kind = "cleanup"; status = $cleanupStatus; observed = $null; limits = $null },
    [ordered]@{ id = "scenario.matrix"; kind = "scenario"; status = $scenarioStatus; observedTerminalCount = $trustedObservedTerminalCount; declaredTerminalCount = $DeclaredScenarioTerminals; repetition = $Repetition }
)
$budgetFailure = @($budgets | Where-Object { $_.status -eq "fail" }).Count -gt 0
$budgetUnavailable = @($budgets | Where-Object { $_.status -eq "unavailable" }).Count -gt 0
if ($budgetUnavailable) { $validityReasons += "budget_evidence_unavailable" }
$eligible = $validityReasons.Count -eq 0
$decision = if ($budgetFailure) { "fail" } elseif (-not $eligible) { "ineligible" } else { "pass" }
$evidenceSection = {
    param([string]$Name)
    if ($runtimeEvidence.Contains($Name)) { return $runtimeEvidence[$Name] }
    return [ordered]@{}
}
$rawEvidence = @()
if ($runtimeEvidenceHash) { $rawEvidence += [ordered]@{ kind = "redacted_runtime_metrics"; sha256 = $runtimeEvidenceHash; redactionCount = 0 } }
$rootMetadata = @($initialTree.roots | ForEach-Object {
    [ordered]@{
        process_id = $_.process_id
        name = $_.name
        executable_path = $null
        creation_time_utc = $_.creation_time_utc
    }
})
$artifact = [ordered]@{
    schemaVersion = 3
    schema_version = 3
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
        binary_provenance_note = "The collector repository commit does not prove which source commit produced the installed binary; use installed_binary.sha256 and the Package 5 provenance manifest for identity."
        installed_binary = $binary
        reference_binary_sha256 = if ($ReferenceBinarySha256.Length -gt 0) { $ReferenceBinarySha256.ToLowerInvariant() } else { $null }
    }
    candidate = [ordered]@{
        sourceCommit = $SourceCommit
        installedBinarySha256 = $installedBinarySha256
        installerSha256 = if ($InstallerSha256.Length -gt 0) { $InstallerSha256 } else { $null }
        protocolVersion = $ProtocolVersion
    }
    reference = [ordered]@{
        installedBinarySha256 = if ($ReferenceBinarySha256.Length -gt 0) { $ReferenceBinarySha256 } else { $null }
        selectionReason = if ($ReferenceSelectionReason.Length -gt 0) { $ReferenceSelectionReason } else { $null }
    }
    host = [ordered]@{
        windowsVersion = [string]$os.Version
        wslVersion = if ($WslVersion.Length -gt 0) { $WslVersion } else { $null }
        distro = if ($WslDistro.Length -gt 0) { $WslDistro } else { $null }
        logicalProcessors = [int]$env:NUMBER_OF_PROCESSORS
        memoryBytes = if ($WslMemoryBytes -gt 0) { $WslMemoryBytes } else { $null }
        powerMode = if ($PowerMode) { $PowerMode } else { $null }
        displayScale = if ($DisplayScale -gt 0) { $DisplayScale } else { $null }
    }
    scenario = [ordered]@{
        kind = $ScenarioKind
        terminalCount = $DeclaredScenarioTerminals
        observedTerminalCount = $trustedObservedTerminalCount
        workloadVersion = $WorkloadVersion
        workloadSeed = $WorkloadSeed
        repetition = $Repetition
        startedAt = $startedAt.ToString("o")
        finishedAt = $finishedAt.ToString("o")
    }
    resources = [ordered]@{
        windows = $summary
        windowsWslBridges = $summary.wsl_descendants
        wslOwned = if ($runtimeEvidence.Contains("wslOwned")) { $runtimeEvidence["wslOwned"] } else { [ordered]@{ available = $false; reason = "authoritative Linux-side ownership evidence was not supplied" } }
        samples = $samples | ForEach-Object { $_.metrics }
    }
    operations = & $evidenceSection "operations"
    preview = & $evidenceSection "preview"
    voice = & $evidenceSection "voice"
    journal = & $evidenceSection "journal"
    diagnostics = & $evidenceSection "diagnostics"
    validity = [ordered]@{ eligible = $eligible; reasons = $validityReasons; processBirthIntervalsExcluded = @($samples | Where-Object { -not $_.totals.cpu_interval_complete }).Count }
    budgets = $budgets
    redactionCount = 0
    decision = $decision
    rawEvidence = $rawEvidence
    configuration = [ordered]@{
        declared_scenario_terminals = $DeclaredScenarioTerminals
        scenario_kind = $ScenarioKind
        workload_version = $WorkloadVersion
        workload_seed = $WorkloadSeed
        repetition = $Repetition
        observed_terminal_count = $trustedObservedTerminalCount
        observed_terminal_metadata = if ($runtimeEvidence.Contains("metrics")) { $runtimeEvidence["metrics"] } else { $null }
        warmup_seconds = $WarmupSeconds
        requested_sample_seconds = $SampleSeconds
        actual_sample_seconds = $stopwatch.Elapsed.TotalSeconds
        sample_count = $samples.Count
        interval_milliseconds = $IntervalMilliseconds
        process_name = $ProcessName
        executable_path_filter = $null
        selected_root_process_id = $firstRoot.process_id
        selected_root_creation_time_utc = $firstRoot.creation_time_utc
        setup_note = $null
        cpu_definition = "CPU seconds consumed divided by wall seconds; 1.0 equals one fully utilized logical core. Intervals with process births or deaths are incomplete and excluded from CPU release statistics. Their observed lower bound is diagnostic only."
        quantile_definition = "p50 and p95 use the nearest-rank empirical quantile: sorted[ceil(p*n)-1]."
    }
    setup_assumptions = @(
        "The installed T-Hub app was already running before collection began.",
        "The terminal scenario count is eligible only when redacted trusted runtime evidence reports the observed T-Hub count; the CLI declaration is never treated as authoritative.",
        "Terminal creation, closure, and workload changes were avoided during warmup and sampling.",
        "Unrelated WSL, agent-browser, Next.js, and Codex processes are excluded unless they are descendants of the selected T-Hub root.",
        "WSL descendant metrics include only Windows WSL bridge descendants visible from the pinned T-Hub root; Linux-side process metrics require a separate redacted runtime evidence source."
    )
    roots = $rootMetadata
    samples = $samples
    summary = $summary
    evidence = $runtimeEvidence
}

$parent = Split-Path -Parent $OutputPath
if ($parent.Length -gt 0) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
}
Write-JsonNoClobber $OutputPath $artifact 12
Write-Host "Wrote benchmark artifact: $OutputPath"
if ($budgetFailure) { exit 4 }
if (-not $eligible) { exit 5 }
