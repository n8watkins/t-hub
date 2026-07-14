# Packaged Runtime Performance Benchmark

This benchmark measures the installed Windows T-Hub process tree without attributing unrelated WSL, agent-browser, Next.js, or Codex processes to the app.
It establishes a reproducible baseline before performance changes are made.

## Metrics

Each sample records total and per-category values for the selected T-Hub roots and their descendants:

- Working set bytes.
- Private bytes.
- CPU as a fraction of one logical core, where `1.0` is one fully utilized logical core.
- Process count.
- Thread count.

The categories are `application`, `webview2`, `host_bridge`, and `other_descendant`.
The JSON also records Windows version, logical processor count, installed binary version and SHA-256 when available, the collector repository commit, benchmark timing, the pinned process root, and setup notes.
The collector commit identifies the scripts that produced the artifact; it does not prove which source commit produced the installed binary.
Use the installed binary SHA-256 as the packaged-build identity.

CPU intervals containing process births or deaths are incomplete because snapshot polling cannot recover the full lifetime CPU of transient processes.
Their observed CPU is retained only as a lower bound, they are excluded from CPU summary statistics, and any run containing one is marked ineligible for release CPU acceptance.
The run-total CPU value is total observed CPU seconds divided by total wall seconds across complete intervals, rather than an unweighted mean of interval ratios.
The p50 and p95 fields use nearest-rank empirical quantiles (`sorted[ceil(p*n)-1]`).

## Scenario Setup

Run the `1`, `4`, `8`, and `16` terminal scenarios separately.
Before each run, prepare exactly that many terminal tiles in one installed T-Hub window.
The scenario count is operator-declared and is recorded as `declared_scenario_terminals`; the current collector does not claim it observed the in-app terminal count.
Use the same tab layout and terminal workload for every comparison.
For the initial idle baseline, leave each terminal at an idle shell prompt, focus the T-Hub window, and avoid typing, resizing, changing tabs, or creating and closing terminals during warmup and sampling.

The harness deliberately does not create or close terminals.
Automating that operation against the current installed app could modify live user sessions, and the packaged app does not yet expose a dedicated disposable benchmark workspace lifecycle.

## Run From WSL

The default run warms up for 30 seconds and samples for 60 seconds at one-second intervals:

```bash
scripts/perf/run-thub-benchmark.sh --terminals 1
scripts/perf/run-thub-benchmark.sh --terminals 4
scripts/perf/run-thub-benchmark.sh --terminals 8
scripts/perf/run-thub-benchmark.sh --terminals 16
```

Use an exact executable path if more than one T-Hub variant is running:

```bash
scripts/perf/run-thub-benchmark.sh \
  --terminals 4 \
  --exe '/mnt/c/Users/natha/AppData/Local/T-Hub/T-Hub.exe' \
  --setup-note 'four idle PowerShell terminals, 2x2 grid'
```

The collector requires exactly one matching T-Hub root.
When multiple instances use the same executable, select one by PID:

```bash
scripts/perf/run-thub-benchmark.sh --terminals 4 --pid 18228
```

The root is pinned by PID and creation time, and collection aborts if it exits or restarts.

Inspect the invocation without requiring T-Hub or PowerShell to be available:

```bash
scripts/perf/run-thub-benchmark.sh --terminals 4 --dry-run
```

Artifacts are written under `artifacts/perf/` by default and are gitignored.
Keep representative artifacts with test reports or release evidence outside Git rather than committing machine-specific results.

## Compare Runs

Compare runs only when terminal count, workload, window state, installed build, warmup duration, sample duration, and host power conditions match.
Use `summary.cpu.run_total_core_fraction` for steady-state CPU only when `release_acceptance_eligible` is true.
Use p95 for recurring high-load behavior and maxima to identify isolated spikes.
Inspect category totals before attributing growth to the Rust application, WebView2, or WSL host bridge.

This first slice measures the Windows process tree only.
It does not measure GPU memory, WebView heap allocation, terminal frame latency, input latency, or processes inside WSL after the Windows `wsl.exe` boundary.
Those require separate ETW, WebView DevTools, and in-app workload instrumentation.
