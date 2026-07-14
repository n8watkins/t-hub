#!/usr/bin/env bash
# Run the packaged Windows T-Hub benchmark from WSL without sampling unrelated processes.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
POWERSHELL_SCRIPT="$HERE/measure-thub.ps1"

terminals=1
warmup_seconds=30
sample_seconds=60
interval_ms=1000
output=""
executable=""
pid=""
setup_note="idle terminals at shell prompts"
dry_run=false

usage() {
  cat <<'EOF'
Usage: scripts/perf/run-thub-benchmark.sh [options]

Options:
  --terminals N       Declared terminal scenario: 1, 4, 8, or 16 (default: 1)
  --warmup-seconds N  Warmup duration before sampling (default: 30)
  --sample-seconds N  Measurement duration (default: 60)
  --interval-ms N     Sample interval, at least 100 ms (default: 1000)
  --output PATH       JSON artifact path (default: artifacts/perf/<timestamp>.json)
  --exe PATH          Exact installed Windows executable path; WSL paths are converted
  --pid PID           Exact T-Hub root PID; required when multiple roots match
  --setup-note TEXT   Workload and tab-layout note stored in benchmark metadata
  --dry-run           Validate arguments and print the PowerShell invocation only
  --help              Show this help
EOF
}

require_value() {
  if [ "$#" -lt 2 ] || [ -z "$2" ]; then
    echo "run-thub-benchmark: $1 requires a value" >&2
    exit 2
  fi
}

to_windows_path() {
  if command -v wslpath >/dev/null 2>&1; then
    wslpath -aw "$1"
  else
    printf '%s\n' "$1"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --terminals) require_value "$@"; terminals="$2"; shift 2 ;;
    --warmup-seconds) require_value "$@"; warmup_seconds="$2"; shift 2 ;;
    --sample-seconds) require_value "$@"; sample_seconds="$2"; shift 2 ;;
    --interval-ms) require_value "$@"; interval_ms="$2"; shift 2 ;;
    --output) require_value "$@"; output="$2"; shift 2 ;;
    --exe) require_value "$@"; executable="$2"; shift 2 ;;
    --pid) require_value "$@"; pid="$2"; shift 2 ;;
    --setup-note) require_value "$@"; setup_note="$2"; shift 2 ;;
    --dry-run) dry_run=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) echo "run-thub-benchmark: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$terminals" in 1|4|8|16) ;; *) echo "run-thub-benchmark: --terminals must be 1, 4, 8, or 16" >&2; exit 2 ;; esac
case "$warmup_seconds" in ''|*[!0-9]*) echo "run-thub-benchmark: --warmup-seconds must be an integer" >&2; exit 2 ;; esac
case "$sample_seconds" in ''|*[!0-9]*) echo "run-thub-benchmark: --sample-seconds must be an integer" >&2; exit 2 ;; esac
case "$interval_ms" in ''|*[!0-9]*) echo "run-thub-benchmark: --interval-ms must be an integer" >&2; exit 2 ;; esac
case "$pid" in ''|*[!0-9]*) [ -z "$pid" ] || { echo "run-thub-benchmark: --pid must be a positive integer" >&2; exit 2; } ;; esac
if [ -n "$pid" ] && [ "$pid" -lt 1 ]; then echo "run-thub-benchmark: --pid must be a positive integer" >&2; exit 2; fi
if [ "$sample_seconds" -lt 1 ]; then echo "run-thub-benchmark: --sample-seconds must be at least 1" >&2; exit 2; fi
if [ "$warmup_seconds" -gt 3600 ]; then echo "run-thub-benchmark: --warmup-seconds must not exceed 3600" >&2; exit 2; fi
if [ "$sample_seconds" -gt 86400 ]; then echo "run-thub-benchmark: --sample-seconds must not exceed 86400" >&2; exit 2; fi
if [ "$interval_ms" -lt 100 ] || [ "$interval_ms" -gt 60000 ]; then
  echo "run-thub-benchmark: --interval-ms must be between 100 and 60000" >&2
  exit 2
fi

if [ -z "$output" ]; then
  output="$REPO_ROOT/artifacts/perf/t-hub-${terminals}t-$(date -u +%Y%m%dT%H%M%SZ).json"
elif [[ "$output" != /* ]]; then
  output="$REPO_ROOT/$output"
fi
if [[ "$executable" == /* ]]; then
  executable="$(to_windows_path "$executable")"
fi

commit="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || printf unknown)"
script_windows="$(to_windows_path "$POWERSHELL_SCRIPT")"
output_windows="$(to_windows_path "$output")"
command=(
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass
  -File "$script_windows"
  -DeclaredScenarioTerminals "$terminals"
  -WarmupSeconds "$warmup_seconds"
  -SampleSeconds "$sample_seconds"
  -IntervalMilliseconds "$interval_ms"
  -OutputPath "$output_windows"
  -SetupNote "$setup_note"
  -CollectorRepositoryCommit "$commit"
)
if [ -n "$executable" ]; then
  command+=( -ExecutablePath "$executable" )
fi
if [ -n "$pid" ]; then
  command+=( -RootProcessId "$pid" )
fi

if "$dry_run"; then
  printf '%q ' "${command[@]}"
  printf '\n'
  exit 0
fi
if ! command -v powershell.exe >/dev/null 2>&1; then
  echo "run-thub-benchmark: powershell.exe is unavailable; run this script from WSL on Windows" >&2
  exit 1
fi
if ! command -v wslpath >/dev/null 2>&1; then
  echo "run-thub-benchmark: wslpath is unavailable; run this script from WSL on Windows" >&2
  exit 1
fi

echo "Benchmark scenario: $terminals terminals"
echo "Do not create, close, or change terminal workloads until collection completes."
"${command[@]}"
