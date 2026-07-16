#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="$repo_root/apps/desktop/src-tauri/Cargo.toml"
target_dir="$repo_root/apps/desktop/src-tauri/target"
agent="$target_dir/debug/t-hub-agent"

command -v tmux >/dev/null
command -v node >/dev/null

CARGO_TARGET_DIR="$target_dir" cargo build --manifest-path "$manifest" -p t-hub-agent
test -x "$agent"

CARGO_TARGET_DIR="$target_dir" T_HUB_REAL_AGENT_BIN="$agent" cargo test \
  --manifest-path "$manifest" \
  --lib \
  'control::tests::dispatch_combined_real_agent_marks_exact_codex_crew_before_provider_exec' \
  -- \
  --exact \
  --ignored \
  --nocapture \
  --test-threads=1
