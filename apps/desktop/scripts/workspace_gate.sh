#!/usr/bin/env bash
# Canonical Rust workspace gate.
#
# The mcp_e2e integration tests spawn the real t-hub-mcp binary from the
# Cargo target directory.  Building that binary explicitly before the
# workspace test keeps this gate deterministic from a clean target directory.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/src-tauri/Cargo.toml"

echo "==> Building t-hub-mcp for mcp_e2e"
cargo build --manifest-path "$MANIFEST" -p t-hub-mcp

echo "==> Running the Rust workspace tests"
cargo test --manifest-path "$MANIFEST" --workspace
