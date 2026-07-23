#!/usr/bin/env bash
# Deterministic Linux-compatible contract tests for the packaged runtime benchmark.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RUNNER="$HERE/run-thub-benchmark.sh"
COLLECTOR="$HERE/measure-thub.ps1"
COLLECTOR_TEST="$HERE/measure-thub.test.ps1"

fail() { echo "perf-benchmark.test: FAIL - $*" >&2; exit 1; }

bash -n "$RUNNER"
test -r "$COLLECTOR" || fail "PowerShell collector is missing"

for scenario in 1 4 8 16; do
  output="$("$RUNNER" --terminals "$scenario" --warmup-seconds 0 --sample-seconds 1 --interval-ms 100 --dry-run)"
  grep -Fq -- "-DeclaredScenarioTerminals $scenario" <<<"$output" || fail "scenario $scenario was not forwarded"
  grep -Fq -- "-ScenarioKind idle" <<<"$output" || fail "scenario kind was not forwarded"
  grep -Fq -- "-WorkloadSeed default" <<<"$output" || fail "workload seed was not forwarded"
  grep -Fq -- "-CollectorRepositoryCommit" <<<"$output" || fail "collector commit metadata was not forwarded"
done
pid_output="$("$RUNNER" --pid 1234 --dry-run)"
grep -Fq -- "-RootProcessId 1234" <<<"$pid_output" || fail "explicit PID was not forwarded"
reference_hash="$(printf 'a%.0s' {1..64})"
evidence_output="$("$RUNNER" --scenario-kind voice_synthesis --evidence artifacts/perf/evidence.json --reference-sha256 "$reference_hash" --reference-reason 'predeclared baseline' --dry-run)"
grep -Fq -- "-ScenarioKind voice_synthesis" <<<"$evidence_output" || fail "scenario kind override was not forwarded"
grep -Fq -- "-RuntimeEvidencePath" <<<"$evidence_output" || fail "runtime evidence was not forwarded"
grep -Fq -- "-ReferenceBinarySha256 $reference_hash" <<<"$evidence_output" || fail "reference hash was not forwarded"
if "$RUNNER" --reference-sha256 abc --reference-reason bad --dry-run >/dev/null 2>&1; then
  fail "invalid reference SHA-256 was accepted"
fi

if "$RUNNER" --terminals 2 --dry-run >/dev/null 2>&1; then
  fail "invalid terminal scenario was accepted"
fi
if "$RUNNER" --sample-seconds 0 --dry-run >/dev/null 2>&1; then
  fail "zero sample duration was accepted"
fi
if "$RUNNER" --interval-ms 99 --dry-run >/dev/null 2>&1; then
  fail "sub-100ms interval was accepted"
fi
if "$RUNNER" --interval-ms 60001 --dry-run >/dev/null 2>&1; then
  fail "over-60000ms interval was accepted"
fi
if "$RUNNER" --pid 0 --dry-run >/dev/null 2>&1; then
  fail "zero PID was accepted"
fi
if "$RUNNER" --scenario-kind unsupported --dry-run >/dev/null 2>&1; then
  fail "unsupported scenario kind was accepted"
fi
if "$RUNNER" --workload-version "release candidate" --dry-run >/dev/null 2>&1; then
  fail "non-canonical workload version was accepted"
fi
if "$RUNNER" --workload-seed "bearer token" --dry-run >/dev/null 2>&1; then
  fail "sensitive workload seed was accepted"
fi
if "$RUNNER" --workload-seed "Bearer Token" --dry-run >/dev/null 2>&1; then
  fail "uppercase sensitive workload seed was accepted"
fi
if "$RUNNER" --setup-note "Authorization: Bearer abc" --dry-run >/dev/null 2>&1; then
  fail "authorization setup note was accepted"
fi
if "$RUNNER" --power-mode turbo --dry-run >/dev/null 2>&1; then
  fail "non-canonical power mode was accepted"
fi
if "$RUNNER" --source-commit bad --dry-run >/dev/null 2>&1; then
  fail "invalid source commit was accepted"
fi
runner_tmp="$(/bin/mktemp -d)"
trap '/bin/rm -rf "$runner_tmp"' EXIT

assert_runner_diagnostic() {
  local artifact="$1" expected_code="$2"
  [ -s "$artifact" ] || fail "runner case did not publish a diagnostic artifact"
  /usr/bin/jq -e --arg code "$expected_code" '
    .schemaVersion == 3 and
    (.candidate | type == "object") and
    (.candidate.sourceCommit | type == "string") and
    (.candidate.installedBinarySha256 == null) and
    (.candidate.installerSha256 == null) and
    (.candidate.protocolVersion | type == "number") and
    (.reference | type == "object") and
    (.reference.installedBinarySha256 == null) and
    (.reference.selectionReason == null) and
    (.host | type == "object") and
    (.host.windowsVersion | type == "string") and
    (.host.wslVersion | type == "string") and
    (.host.distro | type == "string") and
    (.host.logicalProcessors | type == "number") and
    (.host.memoryBytes | type == "number") and
    (.host.powerMode | type == "string") and
    (.host.displayScale | type == "number") and
    (.scenario | type == "object") and
    (.scenario.kind | type == "string") and
    (.scenario.terminalCount | type == "number") and
    (.scenario.observedTerminalCount == null) and
    (.scenario.workloadVersion | type == "string") and
    (.scenario.workloadSeed | type == "string") and
    (.scenario.repetition | type == "number") and
    (.scenario.startedAt == null) and
    (.scenario.finishedAt == null) and
    (.resources | type == "object") and
    (.resources.windows | type == "object") and
    (.resources.wslOwned | type == "object") and
    (.resources.wslOwned.available | type == "boolean") and
    (.resources.wslOwned.reason | type == "string") and
    (.resources.webview | type == "object") and
    (.resources.samples | type == "array") and
    (.operations | type == "array") and
    (.preview | type == "object") and
    (.voice | type == "object") and
    (.journal | type == "object") and
    (.diagnostics | type == "object") and
    (.diagnostics.errorCode | type == "string") and
    (.diagnostics.heartbeatStalls | type == "array") and
    (.diagnostics.longTasks | type == "array") and
    (.diagnostics.resizeObserverErrors | type == "array") and
    (.diagnostics.redactionCount | type == "number") and
    (.validity | type == "object") and
    (.validity.eligible | type == "boolean") and
    (.validity.reasons | type == "array") and
    (.validity.processBirthIntervalsExcluded | type == "number") and
    (.budgets | type == "array") and
    (.decision | type == "string") and
    (.rawEvidence | type == "array") and
    (.redactionCount | type == "number") and
    (.diagnostics.errorCode == $code)
  ' "$artifact" >/dev/null || fail "runner diagnostic schema/types or error code are invalid"
}

run_runner_case() {
  local name="$1" expected_code="$2"; shift 2
  local artifact="$runner_tmp/$name.json" actual
  set +e
  "$RUNNER" --output "$artifact" "$@" >/dev/null 2>&1
  actual=$?
  set -e
  [ "$actual" -eq "$expected_code" ] || fail "runner $name returned exit $actual instead of $expected_code"
  assert_runner_diagnostic "$artifact" "runner_failure"
  printf '%s\n' "$artifact"
}

run_runner_case malformed 2 --terminals 2 >/dev/null
run_runner_case missing 2 --terminals >/dev/null
run_runner_case unknown 2 --unknown-flag >/dev/null
dependency_artifact="$runner_tmp/dependency.json"
set +e
PATH="$HERE" /usr/bin/bash "$RUNNER" --output "$dependency_artifact" --terminals 1 >/dev/null 2>&1
dependency_exit=$?
set -e
[ "$dependency_exit" -eq 3 ] || fail "missing PowerShell dependency returned exit $dependency_exit instead of 3"
assert_runner_diagnostic "$dependency_artifact" "runner_failure"

runner_diag="$runner_tmp/collision.json"
set +e
"$RUNNER" --output "$runner_diag" --unknown-flag >/dev/null 2>&1
runner_exit=$?
set -e
[ "$runner_exit" -eq 2 ] || fail "collision baseline returned exit $runner_exit instead of 2"
assert_runner_diagnostic "$runner_diag" "runner_failure"
runner_before="$(/usr/bin/sha256sum "$runner_diag" | /usr/bin/awk '{print $1}')"
set +e
"$RUNNER" --output "$runner_diag" --unknown-flag >/dev/null 2>&1
collision_exit=$?
set -e
[ "$collision_exit" -eq 2 ] || fail "collision retry returned exit $collision_exit instead of 2"
runner_after="$(/usr/bin/sha256sum "$runner_diag" | /usr/bin/awk '{print $1}')"
[ "$runner_before" = "$runner_after" ] || fail "runner collision clobbered the original artifact"
tmp_count="$(/usr/bin/find "$runner_tmp" -maxdepth 1 -type f -name '*.tmp' | /usr/bin/wc -l)"
stale_count="$(/usr/bin/find "$runner_tmp" -maxdepth 1 -type f -name 'collision.json.*' | /usr/bin/wc -l)"
collision_count="$(/usr/bin/find "$runner_tmp" -maxdepth 1 -type f -name 'collision.runner-*.json' | /usr/bin/wc -l)"
[ "$tmp_count" -eq 0 ] || fail "runner diagnostic left temporary artifacts"
[ "$stale_count" -eq 0 ] || fail "runner diagnostic left stale wildcard artifacts"
[ "$collision_count" -eq 1 ] || fail "runner collision did not create exactly one unique artifact"
assert_runner_diagnostic "$(/usr/bin/find "$runner_tmp" -maxdepth 1 -type f -name 'collision.runner-*.json' -print -quit)" "runner_failure"

if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoProfile -NonInteractive -File "$COLLECTOR_TEST"
elif command -v powershell.exe >/dev/null 2>&1 && command -v wslpath >/dev/null 2>&1; then
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
    -File "$(wslpath -aw "$COLLECTOR_TEST")"
else
  fail "PowerShell is required to execute collector behavior tests"
fi

grep -Fq 'Get-CimInstance Win32_Process' "$COLLECTOR" || fail "collector does not enumerate Windows processes"
grep -Fq 'parent_process_id' "$COLLECTOR" || fail "collector does not retain process ancestry"
grep -Fq 'cpu_core_fraction' "$COLLECTOR" || fail "collector does not expose normalized CPU"
grep -Fq 'working_set_bytes' "$COLLECTOR" || fail "collector does not expose working set"
grep -Fq 'private_bytes' "$COLLECTOR" || fail "collector does not expose private bytes"
grep -Fq 'thread_count' "$COLLECTOR" || fail "collector does not expose thread counts"
grep -Fq 'schema_version = 3' "$COLLECTOR" || fail "collector schema is not versioned"
grep -Fq 'schemaVersion = 3' "$COLLECTOR" || fail "camelCase schemaVersion is missing"
for required_field in candidate reference host scenario resources operations preview voice journal diagnostics validity budgets decision rawEvidence; do
  grep -Fq "$required_field" "$COLLECTOR" || fail "schema field $required_field is missing"
done
grep -Fq 'wsl_descendants' "$COLLECTOR" || fail "collector does not expose WSL descendant metrics"
grep -Fq 'Read-SafeRuntimeEvidence' "$COLLECTOR" || fail "collector does not sanitize runtime evidence"
grep -Fq 'preview' "$COLLECTOR" || fail "collector does not retain preview evidence section"
grep -Fq 'voice' "$COLLECTOR" || fail "collector does not retain voice evidence section"
grep -Fq 'journal' "$COLLECTOR" || fail "collector does not retain journal evidence section"
grep -Fq 'recovery' "$COLLECTOR" || fail "collector does not retain recovery evidence section"
grep -Fq 'p95 = $sorted[$p95Index]' "$COLLECTOR" || fail "collector summary does not expose p95"
grep -Fq 'Unrelated WSL, agent-browser, Next.js, and Codex processes are excluded' "$COLLECTOR" \
  || fail "artifact does not state process isolation assumptions"
git -C "$HERE/../.." check-ignore -q artifacts/perf/contract-test.json \
  || fail "machine-specific JSON artifacts are not ignored"

echo "perf-benchmark.test: PASS"
