#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="$repo_root/apps/desktop/src-tauri/Cargo.toml"
agent="$repo_root/apps/desktop/src-tauri/target/debug/t-hub-agent"

cargo build --manifest-path "$manifest" -p t-hub-agent
test -x "$agent"

T_HUB_REAL_AGENT_BIN="$agent" cargo test \
  --manifest-path "$manifest" \
  --lib \
  'control::tests::dispatch_combined_real_agent_marks_exact_codex_crew_before_provider_exec' \
  -- \
  --exact \
  --ignored \
  --nocapture \
  --test-threads=1
