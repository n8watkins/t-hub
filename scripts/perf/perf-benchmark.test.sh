#!/usr/bin/env bash
# Deterministic Linux-compatible contract tests for the packaged runtime benchmark.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RUNNER="$HERE/run-thub-benchmark.sh"
COLLECTOR="$HERE/measure-thub.ps1"

fail() { echo "perf-benchmark.test: FAIL - $*" >&2; exit 1; }

bash -n "$RUNNER"
test -r "$COLLECTOR" || fail "PowerShell collector is missing"

for scenario in 1 4 8 16; do
  output="$("$RUNNER" --terminals "$scenario" --warmup-seconds 0 --sample-seconds 1 --interval-ms 100 --dry-run)"
  grep -Fq -- "-ScenarioTerminals $scenario" <<<"$output" || fail "scenario $scenario was not forwarded"
  grep -Fq -- "-RepositoryCommit" <<<"$output" || fail "repository commit metadata was not forwarded"
done

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

grep -Fq 'Get-CimInstance Win32_Process' "$COLLECTOR" || fail "collector does not enumerate Windows processes"
grep -Fq 'parent_process_id' "$COLLECTOR" || fail "collector does not retain process ancestry"
grep -Fq 'cpu_core_fraction' "$COLLECTOR" || fail "collector does not expose normalized CPU"
grep -Fq 'working_set_bytes' "$COLLECTOR" || fail "collector does not expose working set"
grep -Fq 'private_bytes' "$COLLECTOR" || fail "collector does not expose private bytes"
grep -Fq 'thread_count' "$COLLECTOR" || fail "collector does not expose thread counts"
grep -Fq 'schema_version = 1' "$COLLECTOR" || fail "collector schema is not versioned"
grep -Fq 'p95 = $sorted[$p95Index]' "$COLLECTOR" || fail "collector summary does not expose p95"
grep -Fq 'Unrelated WSL, agent-browser, Next.js, and Codex processes are excluded' "$COLLECTOR" \
  || fail "artifact does not state process isolation assumptions"
git -C "$HERE/../.." check-ignore -q artifacts/perf/contract-test.json \
  || fail "machine-specific JSON artifacts are not ignored"

echo "perf-benchmark.test: PASS"
