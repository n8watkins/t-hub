#!/usr/bin/env bash
# Linux-runnable contract tests for the Package 6 matrix preflight.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PREFLIGHT="$HERE/package6-matrix-preflight.sh"
fail() { echo "package6-matrix-preflight.test: FAIL - $*" >&2; exit 1; }

bash -n "$PREFLIGHT"
command -v jq >/dev/null 2>&1 || fail "jq is required"

src="0123456789abcdef0123456789abcdef01234567"
hash="$(printf 'a%.0s' {1..64})"
scenarios=(idle terminal_output folder_browsing preview_starting preview_noisy preview_refreshing voice_synthesis endpoint_recovery history_open)
terminal_counts=(1 4 8 16)
repetitions=(1 2 3)
tmp="$(mktemp -d)"
trap '/bin/rm -rf "$tmp"' EXIT

make_manifest() {
  local path="$1"
  jq -n --arg src "$src" --arg hash "$hash" \
    '{schemaVersion:1,candidate:{sourceCommit:$src},artifacts:{installer:{sha256:$hash},installedBinary:{sha256:$hash}}}' \
    > "$path"
}

make_artifact() {
  local path="$1" kind="$2" terminals="$3" repetition="$4"
  jq -n --arg src "$src" --arg hash "$hash" --arg kind "$kind" \
    --argjson terminals "$terminals" --argjson repetition "$repetition" \
    '{schemaVersion:3,candidate:{sourceCommit:$src,installedBinarySha256:$hash,installerSha256:$hash},scenario:{kind:$kind,terminalCount:$terminals,observedTerminalCount:$terminals,repetition:$repetition},validity:{eligible:true,reasons:[]},decision:"pass",budgets:[{id:"absolute.resources",status:"pass"},{id:"paired.regression",status:"pass"},{id:"cleanup.invariant",status:"pass"},{id:"scenario.matrix",status:"pass"}]}' \
    > "$path"
}

make_matrix() {
  local dir="$1"
  mkdir -p "$dir"
  local kind terminals repetition
  for kind in "${scenarios[@]}"; do
    for terminals in "${terminal_counts[@]}"; do
      for repetition in "${repetitions[@]}"; do
        make_artifact "$dir/$kind-${terminals}t-r${repetition}.json" "$kind" "$terminals" "$repetition"
      done
    done
  done
}

run_case() {
  local expected="$1" artifacts="$2" manifest="$3" output="$4"
  set +e
  "$PREFLIGHT" --artifacts-dir "$artifacts" --package5-manifest "$manifest" --source-commit "$src" --output "$output" >/dev/null 2>&1
  local actual=$?
  set -e
  [ "$actual" -eq "$expected" ] || fail "expected exit $expected, got $actual"
}

valid_dir="$tmp/valid"
valid_manifest="$tmp/valid-manifest.json"
make_manifest "$valid_manifest"
make_matrix "$valid_dir"
valid_summary="$tmp/valid-summary.json"
run_case 0 "$valid_dir" "$valid_manifest" "$valid_summary"
jq -e '
  .schemaVersion == 1 and .package == 6 and .decision == "pass" and
  .validity.eligible == true and .validity.reasons == [] and
  .matrix.requiredCellCount == 108 and .matrix.observedCellCount == 108 and
  (.matrix.cells | length) == 108 and
  (.matrix.cells | map([.scenario,.terminalCount,.repetition] | join("|")) | unique | length) == 108 and
  .candidate.sourceCommit == "0123456789abcdef0123456789abcdef01234567" and
  (.candidate.installedBinarySha256 | test("^[a-f0-9]{64}$")) and
  (.candidate.installerSha256 | test("^[a-f0-9]{64}$"))
' "$valid_summary" >/dev/null || fail "valid summary did not prove the complete matrix"
valid_hash="$(sha256sum "$valid_summary" | awk '{print $1}')"
run_case 4 "$valid_dir" "$valid_manifest" "$valid_summary"
[ "$(sha256sum "$valid_summary" | awk '{print $1}')" = "$valid_hash" ] || fail "existing summary was overwritten"

missing_dir="$tmp/missing"
cp -R "$valid_dir" "$missing_dir"
/bin/rm "$missing_dir/idle-1t-r1.json"
run_case 4 "$missing_dir" "$valid_manifest" "$tmp/missing-summary.json"
jq -e '.decision == "fail" and (.validity.reasons | index("missing_matrix_cells") != null)' "$tmp/missing-summary.json" >/dev/null || fail "missing cell was not reported"

duplicate_dir="$tmp/duplicate"
cp -R "$valid_dir" "$duplicate_dir"
cp "$duplicate_dir/idle-1t-r1.json" "$duplicate_dir/idle-1t-r1-duplicate.json"
run_case 4 "$duplicate_dir" "$valid_manifest" "$tmp/duplicate-summary.json"
jq -e '.decision == "fail" and (.validity.reasons | index("duplicate_matrix_cells") != null)' "$tmp/duplicate-summary.json" >/dev/null || fail "duplicate cell was not reported"

ineligible_dir="$tmp/ineligible"
cp -R "$valid_dir" "$ineligible_dir"
jq '.validity.eligible=false | .validity.reasons=["observed_terminal_count_unavailable"] | .decision="ineligible"' "$ineligible_dir/idle-1t-r1.json" > "$ineligible_dir/changed.json"
mv "$ineligible_dir/changed.json" "$ineligible_dir/idle-1t-r1.json"
run_case 5 "$ineligible_dir" "$valid_manifest" "$tmp/ineligible-summary.json"
jq -e '.decision == "ineligible" and (.validity.reasons | index("ineligible_matrix_run") != null)' "$tmp/ineligible-summary.json" >/dev/null || fail "ineligible run was not reported"

budget_dir="$tmp/budget"
cp -R "$valid_dir" "$budget_dir"
jq '.decision="fail" | .budgets[0].status="fail"' "$budget_dir/idle-1t-r1.json" > "$budget_dir/changed.json"
mv "$budget_dir/changed.json" "$budget_dir/idle-1t-r1.json"
run_case 4 "$budget_dir" "$valid_manifest" "$tmp/budget-summary.json"
jq -e '.decision == "fail" and (.validity.reasons | index("matrix_budget_failure") != null)' "$tmp/budget-summary.json" >/dev/null || fail "budget failure was not reported"

schema_dir="$tmp/schema"
cp -R "$valid_dir" "$schema_dir"
jq '.schemaVersion=2' "$schema_dir/idle-1t-r1.json" > "$schema_dir/changed.json"
mv "$schema_dir/changed.json" "$schema_dir/idle-1t-r1.json"
run_case 6 "$schema_dir" "$valid_manifest" "$tmp/schema-summary.json"
jq -e '.decision == "invalid" and (.validity.reasons | index("invalid_evidence_schema") != null)' "$tmp/schema-summary.json" >/dev/null || fail "schema failure was not reported"

identity_dir="$tmp/identity"
cp -R "$valid_dir" "$identity_dir"
jq '.candidate.installedBinarySha256=("b" * 64)' "$identity_dir/idle-1t-r1.json" > "$identity_dir/changed.json"
mv "$identity_dir/changed.json" "$identity_dir/idle-1t-r1.json"
run_case 4 "$identity_dir" "$valid_manifest" "$tmp/identity-summary.json"
jq -e '.decision == "fail" and (.validity.reasons | index("identity_binding_mismatch") != null)' "$tmp/identity-summary.json" >/dev/null || fail "identity mismatch was not reported"

set +e
"$PREFLIGHT" >/dev/null 2>&1
invalid_invocation=$?
set -e
[ "$invalid_invocation" -eq 2 ] || fail "invalid invocation returned $invalid_invocation instead of 2"

set +e
"$PREFLIGHT" --artifacts-dir "$tmp/does-not-exist" --package5-manifest "$valid_manifest" --source-commit "$src" --output "$tmp/unavailable.json" >/dev/null 2>&1
missing_dependency=$?
set -e
[ "$missing_dependency" -eq 3 ] || fail "unavailable artifacts returned $missing_dependency instead of 3"

echo "package6-matrix-preflight.test: PASS"
