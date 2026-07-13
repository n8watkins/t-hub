#!/usr/bin/env bash
# Build, atomically install, and register the WSL-side t-hub MCP server for Codex.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
MANIFEST="$REPO_ROOT/apps/desktop/src-tauri/Cargo.toml"
BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
CAPTAIN_DIR="${T_HUB_CAPTAIN_DIR:-${HOME}/.t-hub/captain}"
DEST="$BIN_DIR/t-hub-mcp"

if [ -n "${T_HUB_MCP_SOURCE:-}" ]; then
  SOURCE="$T_HUB_MCP_SOURCE"
else
  if ! command -v cargo >/dev/null 2>&1; then
    echo "install-thub-codex: cargo is required to build t-hub-mcp" >&2
    exit 1
  fi
  cargo build --release -p t-hub-mcp --manifest-path "$MANIFEST"
  SOURCE="$REPO_ROOT/apps/desktop/src-tauri/target/release/t-hub-mcp"
fi

if [ ! -x "$SOURCE" ]; then
  echo "install-thub-codex: source binary is not executable: $SOURCE" >&2
  exit 1
fi

if ! "$SOURCE" --list-tools >/dev/null 2>&1; then
  echo "install-thub-codex: source binary failed its offline catalog probe: $SOURCE" >&2
  exit 1
fi

# Refuse every known skill conflict before replacing the MCP binary or changing
# Codex registration. The installer repeats validation inside its own
# transaction to cover races between preflight and commit.
bash "$HERE/install-captain-skills.sh" --check

install -d -m 700 "$BIN_DIR" "$CAPTAIN_DIR"
TEMP="$(mktemp "$BIN_DIR/.t-hub-mcp.XXXXXX")"
trap 'rm -f "$TEMP"' EXIT
install -m 700 "$SOURCE" "$TEMP"
"$TEMP" --list-tools >/dev/null
mv -f "$TEMP" "$DEST"
trap - EXIT

install -m 700 "$HERE/ensure-thub-codex.sh" "$CAPTAIN_DIR/ensure-thub-codex.sh"
T_HUB_MCP_BIN="$DEST" "$CAPTAIN_DIR/ensure-thub-codex.sh"
bash "$HERE/install-captain-skills.sh"

echo "install-thub-codex: installed $DEST"
echo "install-thub-codex: start new Codex and Claude sessions to load the updated integration"
