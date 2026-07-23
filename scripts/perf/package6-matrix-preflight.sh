#!/usr/bin/env bash
# Validate the complete Package 6 evidence matrix without running a collector.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/perf/package6-matrix-preflight.sh \
  --artifacts-dir PATH \
  --package5-manifest PATH \
  --source-commit HEX40 \
  --output PATH

Exit codes:
  0  Complete 9 x 4 x 3 matrix is eligible and passing.
  2  Invalid invocation or argument.
  3  Package 5 manifest, matrix directory, or jq dependency is unavailable.
  4  Matrix cells, identity bindings, or budget decisions fail the gate.
  5  A retained run is ineligible and must be rerun.
  6  Evidence or Package 5 manifest schema is invalid.
EOF
}

die_usage() {
  echo "package6-matrix-preflight: $*" >&2
  usage >&2
  exit 2
}

artifacts_dir=""
package5_manifest=""
source_commit=""
output=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --artifacts-dir)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die_usage "--artifacts-dir requires a value"
      artifacts_dir="$2"
      shift 2
      ;;
    --package5-manifest)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die_usage "--package5-manifest requires a value"
      package5_manifest="$2"
      shift 2
      ;;
    --source-commit)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die_usage "--source-commit requires a value"
      source_commit="$2"
      shift 2
      ;;
    --output)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die_usage "--output requires a value"
      output="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die_usage "unknown argument '$1'"
      ;;
  esac
done

[ -n "$artifacts_dir" ] || die_usage "--artifacts-dir is required"
[ -n "$package5_manifest" ] || die_usage "--package5-manifest is required"
[ -n "$source_commit" ] || die_usage "--source-commit is required"
[ -n "$output" ] || die_usage "--output is required"
[[ "$source_commit" =~ ^[0-9a-fA-F]{40}$ ]] || die_usage "--source-commit must be a full 40-hex commit"

if ! command -v jq >/dev/null 2>&1; then
  echo "package6-matrix-preflight: jq is unavailable" >&2
  exit 3
fi
if [ ! -d "$artifacts_dir" ]; then
  echo "package6-matrix-preflight: artifact directory is unavailable: $artifacts_dir" >&2
  exit 3
fi
if [ ! -f "$package5_manifest" ]; then
  echo "package6-matrix-preflight: Package 5 manifest is unavailable: $package5_manifest" >&2
  exit 3
fi
artifacts_dir="$(cd "$artifacts_dir" && pwd)"
package5_manifest="$(cd "$(dirname "$package5_manifest")" && pwd)/$(basename "$package5_manifest")"

manifest_values=""
if ! manifest_values="$(jq -cer '
  (.schemaVersion == 1 and
  (.candidate.sourceCommit | type == "string" and test("^[0-9a-fA-F]{40}$")) and
  (.artifacts.installer.sha256 | type == "string" and test("^[0-9a-fA-F]{64}$")) and
  (.artifacts.installedBinary.sha256 | type == "string" and test("^[0-9a-fA-F]{64}$"))) as $valid |
  if $valid then [
      .candidate.sourceCommit,
      .artifacts.installer.sha256,
      .artifacts.installedBinary.sha256
    ] | @tsv else error("invalid Package 5 manifest") end
' "$package5_manifest")"; then
  echo "package6-matrix-preflight: Package 5 manifest schema is invalid" >&2
  exit 6
fi
IFS=$'\t' read -r manifest_source manifest_installer manifest_installed <<<"$manifest_values"
manifest_source="${manifest_source,,}"
manifest_installer="${manifest_installer,,}"
manifest_installed="${manifest_installed,,}"
if [ "$manifest_source" != "${source_commit,,}" ]; then
  echo "package6-matrix-preflight: source commit does not match Package 5 manifest" >&2
  exit 4
fi

scenarios=(idle terminal_output folder_browsing preview_starting preview_noisy preview_refreshing voice_synthesis endpoint_recovery history_open)
terminal_counts=(1 4 8 16)
repetitions=(1 2 3)
required_cells=$(( ${#scenarios[@]} * ${#terminal_counts[@]} * ${#repetitions[@]} ))

reasons=()
artifact_diagnostics='[]'
cells='[]'
observed_cells=0
schema_error=false
ineligible_error=false
matrix_error=false
declare -A seen_cells=()

add_reason() {
  local reason="$1"
  local existing
  for existing in "${reasons[@]}"; do
    [ "$existing" = "$reason" ] && return 0
  done
  reasons+=("$reason")
}

add_diagnostic() {
  local path="$1" reason="$2"
  artifact_diagnostics="$(jq -c --arg path "$path" --arg reason "$reason" '. + [{path:$path,reason:$reason}]' <<<"$artifact_diagnostics")"
}

mapfile -t artifacts < <(find "$artifacts_dir" -maxdepth 1 -type f -name '*.json' ! -path "$package5_manifest" -print | sort)
if [ "${#artifacts[@]}" -eq 0 ]; then
  add_reason "missing_matrix_cells"
  matrix_error=true
fi

for artifact in "${artifacts[@]}"; do
  metadata=""
  if ! metadata="$(jq -cer --arg source "${source_commit,,}" --arg installer "$manifest_installer" --arg installed "$manifest_installed" '
    def canonical_hash: type == "string" and test("^[0-9a-fA-F]{64}$");
    def canonical_commit: type == "string" and test("^[0-9a-fA-F]{40}$");
    .scenario.kind as $kind |
    .scenario.terminalCount as $terminals |
    (["idle","terminal_output","folder_browsing","preview_starting","preview_noisy","preview_refreshing","voice_synthesis","endpoint_recovery","history_open"] | index($kind)) as $kind_index |
    (.schemaVersion == 3 and
    (.candidate.sourceCommit | canonical_commit) and
    (.candidate.installedBinarySha256 | canonical_hash) and
    (.candidate.installerSha256 | canonical_hash) and
    (.scenario.kind | type == "string") and
    ($kind_index != null) and
    (.scenario.terminalCount | type == "number" and IN(1,4,8,16)) and
    (.scenario.observedTerminalCount | type == "number" and . == $terminals) and
    (.scenario.repetition | type == "number" and IN(1,2,3)) and
    (.validity | type == "object") and
    (.validity.eligible | type == "boolean") and
    (.validity.reasons | type == "array") and
    (.decision | type == "string" and IN("pass","fail","ineligible")) and
    (.budgets | type == "array" and length > 0 and all(.[]; .status | type == "string" and IN("pass","fail","unavailable")))) as $valid |
    if $valid then [
      .candidate.sourceCommit,
      .candidate.installedBinarySha256,
      .candidate.installerSha256,
      .scenario.kind,
      (.scenario.terminalCount | tostring),
      (.scenario.repetition | tostring),
      (.validity.eligible | tostring),
      .decision,
      ([.budgets[] | .status] | join(","))
    ] | @tsv else error("invalid evidence schema") end
  ' "$artifact")"; then
    schema_error=true
    add_reason "invalid_evidence_schema"
    add_diagnostic "$artifact" "invalid_evidence_schema"
    continue
  fi

  IFS=$'\t' read -r artifact_source artifact_installer artifact_installed kind terminals repetition eligible decision budget_statuses <<<"$metadata"
  artifact_source="${artifact_source,,}"
  artifact_installer="${artifact_installer,,}"
  artifact_installed="${artifact_installed,,}"
  cell="$kind|$terminals|$repetition"
  if [ -n "${seen_cells[$cell]+present}" ]; then
    matrix_error=true
    add_reason "duplicate_matrix_cells"
    add_diagnostic "$artifact" "duplicate_matrix_cell:$cell"
  else
    seen_cells[$cell]="$artifact"
    observed_cells=$((observed_cells + 1))
  fi
  if [ "$artifact_source" != "${source_commit,,}" ] || [ "$artifact_installer" != "$manifest_installer" ] || [ "$artifact_installed" != "$manifest_installed" ]; then
    matrix_error=true
    add_reason "identity_binding_mismatch"
    add_diagnostic "$artifact" "identity_binding_mismatch"
  fi
  if [ "$eligible" != true ] || [ "$decision" = "ineligible" ]; then
    ineligible_error=true
    add_reason "ineligible_matrix_run"
    add_diagnostic "$artifact" "ineligible_matrix_run"
  elif [ "$decision" != pass ] || [[ "$budget_statuses" == *fail* ]] || [[ "$budget_statuses" == *unavailable* ]]; then
    matrix_error=true
    add_reason "matrix_budget_failure"
    add_diagnostic "$artifact" "matrix_budget_failure"
  fi
  cells="$(jq -c --arg scenario "$kind" --argjson terminals "$terminals" --argjson repetition "$repetition" --arg path "$artifact" --arg decision "$decision" --arg eligible "$eligible" '. + [{scenario:$scenario,terminalCount:$terminals,repetition:$repetition,path:$path,decision:$decision,eligible:($eligible == "true")}]' <<<"$cells")"
done

for scenario in "${scenarios[@]}"; do
  for terminals in "${terminal_counts[@]}"; do
    for repetition in "${repetitions[@]}"; do
      cell="$scenario|$terminals|$repetition"
      if [ -z "${seen_cells[$cell]+present}" ]; then
        matrix_error=true
        add_reason "missing_matrix_cells"
      fi
    done
  done
done
if [ "$observed_cells" -ne "$required_cells" ]; then
  matrix_error=true
  add_reason "matrix_cell_count_mismatch"
fi

decision=pass
exit_code=0
if [ "$schema_error" = true ]; then
  decision="invalid"
  exit_code=6
elif [ "$ineligible_error" = true ]; then
  decision="ineligible"
  exit_code=5
elif [ "$matrix_error" = true ]; then
  decision="fail"
  exit_code=4
fi

reasons_json='[]'
for reason in "${reasons[@]}"; do
  reasons_json="$(jq -c --arg reason "$reason" '. + [$reason]' <<<"$reasons_json")"
done
manifest_hash="$(sha256sum "$package5_manifest" | awk '{print $1}')"
summary="$(jq -n \
  --argjson cells "$cells" \
  --argjson diagnostics "$artifact_diagnostics" \
  --argjson reasons "$reasons_json" \
  --arg decision "$decision" \
  --arg source "${source_commit,,}" \
  --arg installer "$manifest_installer" \
  --arg installed "$manifest_installed" \
  --arg manifest "$manifest_hash" \
  --argjson required "$required_cells" \
  --argjson observed "$observed_cells" \
  '{schemaVersion:1,package:6,candidate:{sourceCommit:$source,installedBinarySha256:$installed,installerSha256:$installer,package5ManifestSha256:$manifest},matrix:{requiredCellCount:$required,observedCellCount:$observed,cells:$cells},validity:{eligible:($decision=="pass"),reasons:$reasons},decision:$decision,diagnostics:{errorCode:(if $decision=="pass" then "none" else "matrix_preflight_failure" end),artifacts:$diagnostics}}')"

output_parent="$(dirname "$output")"
mkdir -p "$output_parent"
if [ -e "$output" ]; then
  echo "package6-matrix-preflight: refusing to overwrite existing output: $output" >&2
  exit 4
fi
temporary="$output.$$.tmp"
if ! (set -C; printf '%s\n' "$summary" > "$temporary") 2>/dev/null; then
  rm -f "$temporary"
  echo "package6-matrix-preflight: unable to create output" >&2
  exit 4
fi
if ! mv -n "$temporary" "$output" 2>/dev/null; then
  rm -f "$temporary"
  echo "package6-matrix-preflight: output publication collided" >&2
  exit 4
fi

printf '%s\n' "$summary"
exit "$exit_code"
