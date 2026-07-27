#!/usr/bin/env bash
# Canonical Rust workspace gate.
#
# The mcp_e2e integration tests spawn the real t-hub-mcp binary from the
# Cargo target directory.
# Building that binary explicitly before the standard workspace tests keeps
# this gate deterministic from a clean target directory.
#
# The control and tmux modules exercise one shared process harness.
# Running them after the other targets avoids making unrelated tests contend
# for that harness while preserving the complete workspace coverage.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/src-tauri/Cargo.toml"
MODE="${1:-full}"
PLAN_ONLY="${2:-}"

usage() {
  cat <<'EOF'
Usage: apps/desktop/scripts/workspace_gate.sh [standard|process|full] [--plan]

standard  Builds t-hub-mcp and runs every workspace target except control and tmux.
process   Runs the control and tmux modules sequentially.
full      Runs standard followed by process.
EOF
}

case "$MODE" in
  standard | process | full) ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
if [[ -n "$PLAN_ONLY" && "$PLAN_ONLY" != "--plan" ]]; then
  usage >&2
  exit 2
fi

build_mcp() {
  echo "==> Building t-hub-mcp for mcp_e2e"
  cargo build --manifest-path "$MANIFEST" -p t-hub-mcp
}

run_standard() {
  build_mcp
  echo "==> Running non-process Rust workspace tests"
  cargo test \
    --manifest-path "$MANIFEST" \
    --workspace \
    -- \
    --skip control::tests \
    --skip tmux::tests
}

run_process() {
  echo "==> Running control process tests"
  cargo test \
    --manifest-path "$MANIFEST" \
    -p t-hub \
    --lib \
    control::tests
  echo "==> Running tmux process tests"
  cargo test \
    --manifest-path "$MANIFEST" \
    -p t-hub \
    --lib \
    tmux::tests
}

print_plan() {
  case "$MODE" in
    standard)
      cat <<'EOF'
build: cargo build -p t-hub-mcp
standard: cargo test --workspace -- --skip control::tests --skip tmux::tests
EOF
      ;;
    process)
      cat <<'EOF'
control: cargo test -p t-hub --lib control::tests
tmux: cargo test -p t-hub --lib tmux::tests
EOF
      ;;
    full)
      cat <<'EOF'
build: cargo build -p t-hub-mcp
standard: cargo test --workspace -- --skip control::tests --skip tmux::tests
control: cargo test -p t-hub --lib control::tests
tmux: cargo test -p t-hub --lib tmux::tests
EOF
      ;;
  esac
}

if [[ "$PLAN_ONLY" == "--plan" ]]; then
  print_plan
  exit 0
fi

case "$MODE" in
  standard)
    run_standard
    ;;
  process)
    run_process
    ;;
  full)
    run_standard
    run_process
    ;;
esac
