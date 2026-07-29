#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE="${1:-fast}"
PLAN_ONLY="${2:-}"

usage() {
  cat <<'EOF'
Usage: scripts/ci/local-test.sh <profile> [--plan]

fast            Rust libraries, CLI tests, frontend tests, lint, and typecheck.
standard        All Rust targets except slow process modules, plus the fast lanes.
backend         The standard Rust lane and CLI tests.
frontend        Typecheck, lint, Vitest, and the production frontend bundle.
browser         The production frontend bundle and Playwright browser tests.
process         Real control and tmux process lifecycle tests.
contracts       Portable repository, voice-gate, and skill contracts.
host-contracts  Codex and Claude CLI provisioning and installation contracts.
full            Complete Rust, CLI, frontend, browser, bundle, and portable contracts.
EOF
}

if [[ "$PROFILE" == "-h" || "$PROFILE" == "--help" ]]; then
  usage
  exit 0
fi
case "$PROFILE" in
  fast | standard | backend | frontend | browser | process | contracts | host-contracts | full) ;;
  *)
    usage >&2
    exit 2
    ;;
esac
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

rust_standard() {
  "$ROOT/apps/desktop/scripts/workspace_gate.sh" standard
}

rust_process() {
  "$ROOT/apps/desktop/scripts/workspace_gate.sh" process
}

rust_full() {
  "$ROOT/apps/desktop/scripts/workspace_gate.sh" full
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

# The JavaScript/TypeScript counterpart to the Rust clippy gate. Fails on
# ESLint ERRORS only: warnings (react-hooks/exhaustive-deps and the fast-refresh
# backlog) are reported but do not gate, so `--max-warnings` stays unset.
frontend_lint() {
  cd "$ROOT" && "${PNPM[@]}" --filter t-hub-desktop lint
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

portable_contracts() {
  cd "$ROOT" || return
  bash apps/desktop/scripts/check-version.sh --history &&
    bash apps/desktop/scripts/announce_gate.test.sh &&
    bash scripts/perf/perf-benchmark.test.sh &&
    bash scripts/ci/local-test.test.sh &&
    bash scripts/ci/workflow-actions.test.sh &&
    bash scripts/captain/handoff-skill.test.sh
}

host_contracts() {
  cd "$ROOT" || return
  local missing=0
  local command
  for command in codex claude; do
    if ! command -v "$command" >/dev/null 2>&1; then
      printf '%s is required for the host-contracts profile\n' "$command" >&2
      missing=1
    fi
  done
  if ((missing)); then
    return 1
  fi
  bash scripts/captain/ensure-thub-codex.test.sh &&
    bash scripts/captain/ensure-thub-claude.test.sh &&
    bash scripts/captain/install-thub-codex.test.sh
}

print_plan() {
  case "$PROFILE" in
    fast)
      cat <<'EOF'
rust: cargo test --workspace --lib -- --skip control::tests --skip tmux::tests
cli: cargo test --manifest-path apps/cli/Cargo.toml --locked
frontend-typecheck: pnpm --filter t-hub-desktop typecheck
frontend-lint: pnpm --filter t-hub-desktop lint
frontend-unit: pnpm --filter t-hub-desktop test
EOF
      ;;
    standard)
      cat <<'EOF'
rust: apps/desktop/scripts/workspace_gate.sh standard
cli: cargo test --manifest-path apps/cli/Cargo.toml --locked
frontend-typecheck: pnpm --filter t-hub-desktop typecheck
frontend-unit: pnpm --filter t-hub-desktop test
EOF
      ;;
    backend)
      cat <<'EOF'
rust: apps/desktop/scripts/workspace_gate.sh standard
cli: cargo test --manifest-path apps/cli/Cargo.toml --locked
EOF
      ;;
    frontend)
      cat <<'EOF'
frontend-typecheck: pnpm --filter t-hub-desktop typecheck
frontend-lint: pnpm --filter t-hub-desktop lint
frontend-unit: pnpm --filter t-hub-desktop test
frontend-bundle: pnpm --filter t-hub-desktop build:bundle
EOF
      ;;
    browser)
      cat <<'EOF'
frontend-product: pnpm --filter t-hub-desktop build:bundle, then test:browser
EOF
      ;;
    process)
      cat <<'EOF'
rust-process: apps/desktop/scripts/workspace_gate.sh process
EOF
      ;;
    contracts)
      cat <<'EOF'
contracts: version, voice gate, performance, test profile, workflow pinning, and handoff skill
EOF
      ;;
    host-contracts)
      cat <<'EOF'
host-contracts: Codex provisioning, Claude provisioning, and Codex installation
EOF
      ;;
    full)
      cat <<'EOF'
rust: apps/desktop/scripts/workspace_gate.sh full
cli: cargo test --manifest-path apps/cli/Cargo.toml --locked
frontend-typecheck: pnpm --filter t-hub-desktop typecheck
frontend-lint: pnpm --filter t-hub-desktop lint
frontend-unit: pnpm --filter t-hub-desktop test
frontend-product: pnpm --filter t-hub-desktop build:bundle, then test:browser
contracts: version, voice gate, performance, test profile, workflow pinning, and handoff skill
EOF
      ;;
  esac
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

case "$PROFILE" in
  fast)
    start_lane rust rust_fast
    start_lane cli cli_tests
    start_lane frontend-typecheck frontend_typecheck
    start_lane frontend-lint frontend_lint
    start_lane frontend-unit frontend_tests
    ;;
  standard)
    start_lane rust rust_standard
    start_lane cli cli_tests
    start_lane frontend-typecheck frontend_typecheck
    start_lane frontend-unit frontend_tests
    ;;
  backend)
    start_lane rust rust_standard
    start_lane cli cli_tests
    ;;
  frontend)
    start_lane frontend-typecheck frontend_typecheck
    start_lane frontend-lint frontend_lint
    start_lane frontend-unit frontend_tests
    start_lane frontend-bundle frontend_bundle
    ;;
  browser)
    start_lane frontend-product frontend_product
    ;;
  process)
    start_lane rust-process rust_process
    ;;
  contracts)
    start_lane contracts portable_contracts
    ;;
  host-contracts)
    start_lane host-contracts host_contracts
    ;;
  full)
    start_lane rust rust_full
    start_lane cli cli_tests
    start_lane frontend-typecheck frontend_typecheck
    start_lane frontend-lint frontend_lint
    start_lane frontend-unit frontend_tests
    start_lane frontend-product frontend_product
    start_lane contracts portable_contracts
    ;;
esac

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
