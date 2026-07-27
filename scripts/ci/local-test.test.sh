#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/ci/local-test.sh"

fast_plan="$("$SCRIPT" fast --plan)"
grep -Fq -- "--skip control::tests" <<<"$fast_plan"
grep -Fq -- "--skip tmux::tests" <<<"$fast_plan"
if grep -Fq "frontend-browser" <<<"$fast_plan"; then
  echo "fast plan must not run browser tests" >&2
  exit 1
fi

full_plan="$("$SCRIPT" full --plan)"
grep -Fq "workspace_gate.sh" <<<"$full_plan"
grep -Fq "frontend-product" <<<"$full_plan"
grep -Fq "build:bundle" <<<"$full_plan"
if grep -Fq "tsc --noEmit && vite build" <<<"$full_plan"; then
  echo "full plan must not repeat typecheck inside the bundle lane" >&2
  exit 1
fi

if "$SCRIPT" obsolete --plan >/dev/null 2>&1; then
  echo "unknown profiles must fail" >&2
  exit 1
fi

echo "local test profile contract passed"
