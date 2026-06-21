#!/usr/bin/env bash
# End-to-end proof harness for T-Hub's local MCP server (PRD §9.6).
#
# This produces the round-trip evidence two ways:
#   1. Offline tool catalog — runs the real `t-hub-mcp --list-tools`, which
#      requires no running app (proves the MCP tool surface + tier annotations).
#   2. Live round-trip — runs the `mcp_e2e` integration test with --nocapture,
#      which spawns the REAL `t-hub-mcp` binary, drives it with genuine MCP
#      JSON-RPC over stdio, and forwards each `tools/call` through the REAL app
#      control listener to the REAL command dispatch and back. The `→`/`←` lines
#      it prints are the actual wire transcript.
#
# Usage:  scripts/mcp_proof.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/src-tauri/Cargo.toml"

echo "==> Building t-hub-mcp (release-debug)…"
cargo build -p t-hub-mcp --manifest-path "$MANIFEST" >/dev/null
BIN="$ROOT/src-tauri/target/debug/t-hub-mcp"

echo
echo "==> [1/2] Offline tool catalog ('t-hub-mcp --list-tools')"
echo "    (every tool, its tier, and confirmationRequired flag)"
# Capture the catalog to a temp file, then summarize — avoids a head-closed
# pipe breaking the binary's stdout. Falls back to a raw dump if python3 is
# unavailable.
CATALOG_FILE="$(mktemp)"
trap 'rm -f "$CATALOG_FILE"' EXIT
"$BIN" --list-tools >"$CATALOG_FILE"
if command -v python3 >/dev/null 2>&1; then
  CATALOG_FILE="$CATALOG_FILE" python3 - <<'PY'
import json, os
with open(os.environ["CATALOG_FILE"]) as fh:
    doc = json.load(fh)
for t in doc["tools"]:
    a = t["annotations"]
    print("  - %-18s tier=%-16s confirm=%s" % (t["name"], a["t-hubTier"], a["confirmationRequired"]))
PY
else
  cat "$CATALOG_FILE"
fi

echo
echo "==> [2/2] Live end-to-end round-trip (real binary ⇄ real control listener)"
echo "    Each '→' is an MCP request sent to t-hub-mcp's stdin;"
echo "    each '←' is the JSON-RPC response it wrote to stdout after forwarding"
echo "    the call through the app control channel."
echo
cargo test --manifest-path "$MANIFEST" -p t-hub --test mcp_e2e -- --nocapture --test-threads=1 \
  2>/dev/null | sed -n '/^→/p;/^←/p;/test result/p'

echo
echo "==> Proof complete. The transcript above is a genuine MCP <-> T-Hub round-trip."
