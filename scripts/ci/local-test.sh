#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE="${1:-fast}"
PLAN_ONLY="${2:-}"

usage() {
  cat <<'EOF'
Usage: scripts/ci/local-test.sh [fast|full] [--plan]

fast  Runs deterministic Rust libraries, CLI tests, frontend tests, and typecheck.
full  Runs the complete Rust, CLI, frontend, browser, build, and contract gates.
EOF
}

if [[ "$PROFILE" == "-h" || "$PROFILE" == "--help" ]]; then
  usage
  exit 0
fi
if [[ "$PROFILE" != "fast" && "$PROFILE" != "full" ]]; then
  usage >&2
  exit 2
fi
if [[ -n "$PLAN_ONLY" && "$PLAN_ONLY" != "--plan" ]]; then
  usage >&2
  exit 2
fi

if command -v pnpm >/dev/null 2>&1; then
  PNPM=(pnpm)
elif command -v corepack >/dev/null 2>&1; then
  PNPM=(corepack pnpm)
else
  echo "pnpm is required, either directly or through Corepack" >&2
  exit 1
fi

rust_fast() {
  cargo test \
    --manifest-path "$ROOT/apps/desktop/src-tauri/Cargo.toml" \
    --workspace \
    --lib \
    -- \
    --skip control::tests \
    --skip tmux::tests
}

rust_full() {
  "$ROOT/apps/desktop/scripts/workspace_gate.sh"
}

cli_tests() {
  cargo test --manifest-path "$ROOT/apps/cli/Cargo.toml" --locked
}

frontend_typecheck() {
  cd "$ROOT" && "${PNPM[@]}" --filter t-hub-desktop typecheck
}

frontend_tests() {
  cd "$ROOT" && "${PNPM[@]}" --filter t-hub-desktop test
}

frontend_browser() {
  cd "$ROOT" && "${PNPM[@]}" --filter t-hub-desktop test:browser
}

frontend_bundle() {
  cd "$ROOT" && "${PNPM[@]}" --filter t-hub-desktop build:bundle
}

frontend_product() {
  frontend_bundle && frontend_browser
}

repository_contracts() {
  cd "$ROOT" || return
  bash apps/desktop/scripts/check-version.sh --history &&
    bash scripts/perf/perf-benchmark.test.sh &&
    bash scripts/ci/local-test.test.sh &&
    bash scripts/ci/workflow-actions.test.sh
}

print_plan() {
  if [[ "$PROFILE" == "fast" ]]; then
    cat <<'EOF'
rust: cargo test --workspace --lib -- --skip control::tests --skip tmux::tests
cli: cargo test --manifest-path apps/cli/Cargo.toml --locked
frontend-typecheck: pnpm --filter t-hub-desktop typecheck
frontend-unit: pnpm --filter t-hub-desktop test
EOF
  else
    cat <<'EOF'
rust: apps/desktop/scripts/workspace_gate.sh
cli: cargo test --manifest-path apps/cli/Cargo.toml --locked
frontend-typecheck: pnpm --filter t-hub-desktop typecheck
frontend-unit: pnpm --filter t-hub-desktop test
frontend-product: pnpm --filter t-hub-desktop build:bundle, then test:browser
contracts: version, performance benchmark, test profile, and workflow pinning contracts
EOF
  fi
}

if [[ "$PLAN_ONLY" == "--plan" ]]; then
  print_plan
  exit 0
fi

LOG_DIR="$(mktemp -d)"
trap 'rm -rf "$LOG_DIR"' EXIT

declare -a LANE_NAMES=()
declare -a LANE_PIDS=()

start_lane() {
  local name="$1"
  shift
  LANE_NAMES+=("$name")
  (
    "$@"
  ) >"$LOG_DIR/$name.log" 2>&1 &
  LANE_PIDS+=("$!")
}

if [[ "$PROFILE" == "fast" ]]; then
  start_lane rust rust_fast
else
  start_lane rust rust_full
fi
start_lane cli cli_tests
start_lane frontend-typecheck frontend_typecheck
start_lane frontend-unit frontend_tests
if [[ "$PROFILE" == "full" ]]; then
  start_lane frontend-product frontend_product
  start_lane contracts repository_contracts
fi

printf 'Running %s profile in %d parallel lanes:' "$PROFILE" "${#LANE_NAMES[@]}"
printf ' %s' "${LANE_NAMES[@]}"
printf '\n'

failed=0
for index in "${!LANE_PIDS[@]}"; do
  name="${LANE_NAMES[$index]}"
  if wait "${LANE_PIDS[$index]}"; then
    printf '\n[%s] passed\n' "$name"
    tail -n 12 "$LOG_DIR/$name.log"
  else
    printf '\n[%s] failed\n' "$name" >&2
    cat "$LOG_DIR/$name.log" >&2
    failed=1
  fi
done

if ((failed)); then
  exit 1
fi

printf '\n%s test profile passed\n' "$PROFILE"
