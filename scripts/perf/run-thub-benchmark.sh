#!/usr/bin/env bash
# Run the packaged Windows T-Hub benchmark from WSL without sampling unrelated processes.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
POWERSHELL_SCRIPT="$HERE/measure-thub.ps1"

terminals=1
scenario_kind=idle
workload_version=v1
workload_seed=default
repetition=1
warmup_seconds=30
sample_seconds=60
interval_ms=1000
output=""
executable=""
pid=""
runtime_evidence=""
reference_binary_sha256=""
reference_selection_reason=""
source_commit=""
installer_sha256=""
package5_manifest=""
observed_terminals=""
protocol_version=2
wsl_version=""
wsl_distro=""
wsl_memory_bytes=""
power_mode=""
display_scale=""
setup_note="idle terminals at shell prompts"
dry_run=false

usage() {
  cat <<'EOF'
Usage: scripts/perf/run-thub-benchmark.sh [options]

Options:
  --terminals N       Declared terminal scenario: 1, 4, 8, or 16 (default: 1)
  --scenario-kind K   Matrix scenario kind (default: idle)
  --workload-version V Stable workload definition (default: v1)
  --workload-seed S   Stable workload seed (default: default)
  --repetition N      Eligible repetition number (default: 1)
  --warmup-seconds N  Warmup duration before sampling (default: 30)
  --sample-seconds N  Measurement duration (default: 60)
  --interval-ms N     Sample interval, at least 100 ms (default: 1000)
  --output PATH       JSON artifact path (default: artifacts/perf/<timestamp>.json)
  --exe PATH          Exact installed Windows executable path; WSL paths are converted
  --pid PID           Exact T-Hub root PID; required when multiple roots match
  --evidence PATH     Optional redacted numeric runtime evidence JSON
  --reference-sha256 H Reference installed binary SHA-256 for paired comparison
  --reference-reason T Predeclared reference selection reason
  --source-commit H   Full source Git commit bound to the installed artifact
  --installer-sha256 H Package 5 installer SHA-256
  --package5-manifest PATH Package 5 provenance manifest for exact app binding
  --observed-terminals N Authoritative observed terminal count (required for eligibility)
  --wsl-version V      Observed WSL version (required for eligibility)
  --wsl-distro NAME    Observed WSL distro identity (required for eligibility)
  --wsl-memory-bytes N Observed WSL memory bytes (required for eligibility)
  --power-mode NAME   Observed host power mode (required for eligibility)
  --display-scale N   Observed display scale percent (required for eligibility)
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

validate_text() {
  local name="$1" value="$2" maximum="$3"
  if [ "${#value}" -gt "$maximum" ] || [[ "$value" =~ [[:cntrl:]] ]]; then
    echo "run-thub-benchmark: $name exceeds its bounded text contract" >&2
    exit 2
  fi
  if [[ "$value" =~ (^|[^[:alnum:]])(token|secret|password|credential|transcript|prompt|payload|content|command)([^[:alnum:]]|$) ]]; then
    echo "run-thub-benchmark: $name contains a prohibited sensitive-content marker" >&2
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
    --scenario-kind) require_value "$@"; scenario_kind="$2"; shift 2 ;;
    --workload-version) require_value "$@"; workload_version="$2"; shift 2 ;;
    --workload-seed) require_value "$@"; workload_seed="$2"; shift 2 ;;
    --repetition) require_value "$@"; repetition="$2"; shift 2 ;;
    --warmup-seconds) require_value "$@"; warmup_seconds="$2"; shift 2 ;;
    --sample-seconds) require_value "$@"; sample_seconds="$2"; shift 2 ;;
    --interval-ms) require_value "$@"; interval_ms="$2"; shift 2 ;;
    --output) require_value "$@"; output="$2"; shift 2 ;;
    --exe) require_value "$@"; executable="$2"; shift 2 ;;
    --pid) require_value "$@"; pid="$2"; shift 2 ;;
    --evidence) require_value "$@"; runtime_evidence="$2"; shift 2 ;;
    --reference-sha256) require_value "$@"; reference_binary_sha256="$2"; shift 2 ;;
    --reference-reason) require_value "$@"; reference_selection_reason="$2"; shift 2 ;;
    --source-commit) require_value "$@"; source_commit="$2"; shift 2 ;;
    --installer-sha256) require_value "$@"; installer_sha256="$2"; shift 2 ;;
    --package5-manifest) require_value "$@"; package5_manifest="$2"; shift 2 ;;
    --observed-terminals) require_value "$@"; observed_terminals="$2"; shift 2 ;;
    --wsl-version) require_value "$@"; wsl_version="$2"; shift 2 ;;
    --wsl-distro) require_value "$@"; wsl_distro="$2"; shift 2 ;;
    --wsl-memory-bytes) require_value "$@"; wsl_memory_bytes="$2"; shift 2 ;;
    --power-mode) require_value "$@"; power_mode="$2"; shift 2 ;;
    --display-scale) require_value "$@"; display_scale="$2"; shift 2 ;;
    --setup-note) require_value "$@"; setup_note="$2"; shift 2 ;;
    --dry-run) dry_run=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) echo "run-thub-benchmark: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$terminals" in 1|4|8|16) ;; *) echo "run-thub-benchmark: --terminals must be 1, 4, 8, or 16" >&2; exit 2 ;; esac
case "$scenario_kind" in idle|terminal_output|folder_browsing|preview_starting|preview_noisy|preview_refreshing|voice_synthesis|endpoint_recovery|history_open) ;; *) echo "run-thub-benchmark: unsupported --scenario-kind '$scenario_kind'" >&2; exit 2 ;; esac
case "$repetition" in ''|*[!0-9]*) echo "run-thub-benchmark: --repetition must be an integer" >&2; exit 2 ;; esac
if [ "$repetition" -lt 1 ] || [ "$repetition" -gt 3 ]; then echo "run-thub-benchmark: --repetition must be between 1 and 3" >&2; exit 2; fi
case "$warmup_seconds" in ''|*[!0-9]*) echo "run-thub-benchmark: --warmup-seconds must be an integer" >&2; exit 2 ;; esac
case "$sample_seconds" in ''|*[!0-9]*) echo "run-thub-benchmark: --sample-seconds must be an integer" >&2; exit 2 ;; esac
case "$observed_terminals" in ''|*[!0-9]*) [ -z "$observed_terminals" ] || { echo "run-thub-benchmark: --observed-terminals must be an integer" >&2; exit 2; } ;; esac
case "$wsl_memory_bytes" in ''|*[!0-9]*) [ -z "$wsl_memory_bytes" ] || { echo "run-thub-benchmark: --wsl-memory-bytes must be an integer" >&2; exit 2; } ;; esac
case "$display_scale" in ''|*[!0-9]*) [ -z "$display_scale" ] || { echo "run-thub-benchmark: --display-scale must be an integer" >&2; exit 2; } ;; esac
case "$interval_ms" in ''|*[!0-9]*) echo "run-thub-benchmark: --interval-ms must be an integer" >&2; exit 2 ;; esac
case "$pid" in ''|*[!0-9]*) [ -z "$pid" ] || { echo "run-thub-benchmark: --pid must be a positive integer" >&2; exit 2; } ;; esac
if [ -n "$pid" ] && [ "$pid" -lt 1 ]; then echo "run-thub-benchmark: --pid must be a positive integer" >&2; exit 2; fi
if [ "$sample_seconds" -lt 1 ]; then echo "run-thub-benchmark: --sample-seconds must be at least 1" >&2; exit 2; fi
if [ "$warmup_seconds" -gt 3600 ]; then echo "run-thub-benchmark: --warmup-seconds must not exceed 3600" >&2; exit 2; fi
if [ "$sample_seconds" -gt 86400 ]; then echo "run-thub-benchmark: --sample-seconds must not exceed 86400" >&2; exit 2; fi
if [ -n "$observed_terminals" ] && { [ "$observed_terminals" -lt 1 ] || [ "$observed_terminals" -gt 16 ]; }; then echo "run-thub-benchmark: --observed-terminals must be between 1 and 16" >&2; exit 2; fi
if [ -n "$wsl_memory_bytes" ] && [ "$wsl_memory_bytes" -lt 1 ]; then echo "run-thub-benchmark: --wsl-memory-bytes must be positive" >&2; exit 2; fi
if [ -n "$display_scale" ] && { [ "$display_scale" -lt 1 ] || [ "$display_scale" -gt 500 ]; }; then echo "run-thub-benchmark: --display-scale must be between 1 and 500" >&2; exit 2; fi
if [ -n "$reference_binary_sha256" ] && [[ ! "$reference_binary_sha256" =~ ^[0-9a-fA-F]{64}$ ]]; then echo "run-thub-benchmark: --reference-sha256 must be 64 hex characters" >&2; exit 2; fi
if [ -n "$reference_binary_sha256" ] && [ -z "$reference_selection_reason" ]; then echo "run-thub-benchmark: --reference-reason is required with --reference-sha256" >&2; exit 2; fi
if [ -n "$installer_sha256" ] && [[ ! "$installer_sha256" =~ ^[0-9a-fA-F]{64}$ ]]; then echo "run-thub-benchmark: --installer-sha256 must be 64 hex characters" >&2; exit 2; fi
if [ -n "$source_commit" ] && [[ ! "$source_commit" =~ ^[0-9a-fA-F]{40}$ ]]; then echo "run-thub-benchmark: --source-commit must be a full 40-hex commit" >&2; exit 2; fi
if [ "$interval_ms" -lt 100 ] || [ "$interval_ms" -gt 60000 ]; then
  echo "run-thub-benchmark: --interval-ms must be between 100 and 60000" >&2
  exit 2
fi
validate_text "workload version" "$workload_version" 64
validate_text "workload seed" "$workload_seed" 128
validate_text "reference reason" "$reference_selection_reason" 256
validate_text "power mode" "$power_mode" 128
validate_text "WSL version" "$wsl_version" 128
validate_text "WSL distro" "$wsl_distro" 128
validate_text "setup note" "$setup_note" 256

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
  -ScenarioKind "$scenario_kind"
  -WorkloadVersion "$workload_version"
  -WorkloadSeed "$workload_seed"
  -Repetition "$repetition"
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
if [ -n "$runtime_evidence" ]; then
  command+=( -RuntimeEvidencePath "$(to_windows_path "$runtime_evidence")" )
fi
if [ -n "$reference_binary_sha256" ]; then
  command+=( -ReferenceBinarySha256 "$reference_binary_sha256" )
fi
if [ -n "$reference_selection_reason" ]; then
  command+=( -ReferenceSelectionReason "$reference_selection_reason" )
fi
if [ -n "$source_commit" ]; then
  command+=( -SourceCommit "$source_commit" )
fi
if [ -n "$installer_sha256" ]; then
  command+=( -InstallerSha256 "$installer_sha256" )
fi
if [ -n "$package5_manifest" ]; then
  command+=( -Package5ManifestPath "$(to_windows_path "$package5_manifest")" )
fi
if [ -n "$observed_terminals" ]; then
  command+=( -ObservedTerminalCount "$observed_terminals" )
fi
if [ -n "$wsl_version" ]; then
  command+=( -WslVersion "$wsl_version" )
fi
if [ -n "$wsl_distro" ]; then
  command+=( -WslDistro "$wsl_distro" )
fi
if [ -n "$wsl_memory_bytes" ]; then
  command+=( -WslMemoryBytes "$wsl_memory_bytes" )
fi
if [ -n "$power_mode" ]; then
  command+=( -PowerMode "$power_mode" )
fi
if [ -n "$display_scale" ]; then
  command+=( -DisplayScale "$display_scale" )
fi

if "$dry_run"; then
  printf '%q ' "${command[@]}"
  printf '\n'
  exit 0
fi
if ! command -v powershell.exe >/dev/null 2>&1; then
  echo "run-thub-benchmark: powershell.exe is unavailable; run this script from WSL on Windows" >&2
  exit 3
fi
if ! command -v wslpath >/dev/null 2>&1; then
  echo "run-thub-benchmark: wslpath is unavailable; run this script from WSL on Windows" >&2
  exit 3
fi

echo "Benchmark scenario: $terminals terminals"
echo "Do not create, close, or change terminal workloads until collection completes."
set +e
"${command[@]}"
status=$?
set -e
if [ "$status" -ne 0 ]; then
  case "$status" in
    4|5|6) exit "$status" ;;
    *) exit 5 ;;
  esac
fi
exit 0
