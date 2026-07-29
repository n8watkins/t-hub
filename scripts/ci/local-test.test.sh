#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/ci/local-test.sh"
WORKFLOW="$ROOT/.github/workflows/test.yml"

fast_plan="$(bash "$SCRIPT" fast --plan)"
grep -Fq -- "--skip control::tests" <<<"$fast_plan"
grep -Fq -- "--skip tmux::tests" <<<"$fast_plan"
grep -Fq -- "--lib" <<<"$fast_plan"
grep -Fq "frontend-lint" <<<"$fast_plan"
if grep -Fq "frontend-product" <<<"$fast_plan"; then
  echo "fast plan must not run browser tests" >&2
  exit 1
fi
# The lint lane reports warnings without gating on them. Pinning `--max-warnings`
# to 0 would turn the react-hooks/exhaustive-deps backlog into a merge blocker,
# so the contract asserts the flag stays absent until that backlog is cleared.
if grep -Fq -- "--max-warnings" <<<"$fast_plan"; then
  echo "lint lane must not gate on warnings yet" >&2
  exit 1
fi

standard_plan="$(bash "$SCRIPT" standard --plan)"
grep -Fq "workspace_gate.sh standard" <<<"$standard_plan"

backend_plan="$(bash "$SCRIPT" backend --plan)"
grep -Fq "workspace_gate.sh standard" <<<"$backend_plan"
if grep -Fq "frontend-" <<<"$backend_plan"; then
  echo "backend plan must not run frontend lanes" >&2
  exit 1
fi

frontend_plan="$(bash "$SCRIPT" frontend --plan)"
grep -Fq "frontend-typecheck" <<<"$frontend_plan"
grep -Fq "frontend-lint" <<<"$frontend_plan"
grep -Fq "frontend-unit" <<<"$frontend_plan"
grep -Fq "frontend-bundle" <<<"$frontend_plan"
if grep -Fq "cargo " <<<"$frontend_plan"; then
  echo "frontend plan must not run Rust or CLI lanes" >&2
  exit 1
fi

browser_plan="$(bash "$SCRIPT" browser --plan)"
grep -Fq "frontend-product" <<<"$browser_plan"
grep -Fq "build:bundle" <<<"$browser_plan"
grep -Fq "test:browser" <<<"$browser_plan"

process_plan="$(bash "$SCRIPT" process --plan)"
grep -Fq "workspace_gate.sh process" <<<"$process_plan"

contracts_plan="$(bash "$SCRIPT" contracts --plan)"
grep -Fq "voice gate" <<<"$contracts_plan"
grep -Fq "handoff skill" <<<"$contracts_plan"

host_contracts_plan="$(bash "$SCRIPT" host-contracts --plan)"
grep -Fq "Codex provisioning" <<<"$host_contracts_plan"
grep -Fq "Claude provisioning" <<<"$host_contracts_plan"
grep -Fq "Codex installation" <<<"$host_contracts_plan"

full_plan="$(bash "$SCRIPT" full --plan)"
grep -Fq "workspace_gate.sh full" <<<"$full_plan"
grep -Fq "frontend-lint" <<<"$full_plan"
grep -Fq "frontend-product" <<<"$full_plan"
grep -Fq "build:bundle" <<<"$full_plan"
grep -Fq "voice gate" <<<"$full_plan"
grep -Fq "handoff skill" <<<"$full_plan"
if grep -Fq "tsc --noEmit && vite build" <<<"$full_plan"; then
  echo "full plan must not repeat typecheck inside the bundle lane" >&2
  exit 1
fi

if bash "$SCRIPT" obsolete --plan >/dev/null 2>&1; then
  echo "unknown profiles must fail" >&2
  exit 1
fi

grep -Fq "bash apps/desktop/scripts/announce_gate.test.sh" "$WORKFLOW"
# The server-side gate must run ESLint, mirroring the local frontend-lint lane.
grep -Fq "pnpm --filter t-hub-desktop lint" "$WORKFLOW"
grep -Fq "bash scripts/captain/handoff-skill.test.sh" "$WORKFLOW"
grep -Fq "@openai/codex@0.145.0" "$WORKFLOW"
grep -Fq "@anthropic-ai/claude-code@2.1.220" "$WORKFLOW"
grep -Fq "bash scripts/captain/ensure-thub-codex.test.sh" "$WORKFLOW"
grep -Fq "bash scripts/captain/ensure-thub-claude.test.sh" "$WORKFLOW"
grep -Fq "bash scripts/captain/install-thub-codex.test.sh" "$WORKFLOW"

workspace_gate="$ROOT/apps/desktop/scripts/workspace_gate.sh"
standard_rust_plan="$(bash "$workspace_gate" standard --plan)"
grep -Fq "cargo build -p t-hub-mcp" <<<"$standard_rust_plan"
grep -Fq -- "--skip control::tests" <<<"$standard_rust_plan"
grep -Fq -- "--skip tmux::tests" <<<"$standard_rust_plan"

process_rust_plan="$(bash "$workspace_gate" process --plan)"
grep -Fq "control::tests" <<<"$process_rust_plan"
grep -Fq "tmux::tests" <<<"$process_rust_plan"

full_rust_plan="$(bash "$workspace_gate" full --plan)"
grep -Fq "cargo build -p t-hub-mcp" <<<"$full_rust_plan"
grep -Fq "control::tests" <<<"$full_rust_plan"
grep -Fq "tmux::tests" <<<"$full_rust_plan"

if bash "$workspace_gate" obsolete --plan >/dev/null 2>&1; then
  echo "unknown Rust workspace profiles must fail" >&2
  exit 1
fi

echo "local test profile contract passed"
