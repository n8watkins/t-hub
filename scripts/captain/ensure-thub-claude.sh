#!/usr/bin/env bash
# Register the stable T-Hub MCP binary in Claude Code's user scope.
set -euo pipefail

BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
BIN="${T_HUB_MCP_BIN:-${BIN_DIR}/t-hub-mcp}"
CONFIG="${HOME}/.claude.json"

if ! command -v claude >/dev/null 2>&1; then
  echo "ensure-thub-claude: claude not on PATH - install Claude Code first" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "ensure-thub-claude: jq is required to verify the complete registration" >&2
  exit 1
fi
if [ ! -x "$BIN" ] || ! "$BIN" --list-tools >/dev/null 2>&1; then
  echo "ensure-thub-claude: t-hub MCP binary is unavailable or invalid: $BIN" >&2
  exit 1
fi

if [ -f "$CONFIG" ] && jq -e --arg bin "$BIN" '
  .mcpServers["t-hub"] == {
    "type": "stdio", "command": $bin, "args": [], "env": {}
  }
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: t-hub already points at $BIN"
  exit 0
fi

BACKUP=""
HAD_CONFIG=false
if [ -f "$CONFIG" ]; then
  HAD_CONFIG=true
  BACKUP="$(mktemp "${CONFIG}.t-hub-backup.XXXXXX")"
  cp -p "$CONFIG" "$BACKUP"
fi
rollback() {
  if "$HAD_CONFIG"; then
    cp -p "$BACKUP" "$CONFIG"
  else
    rm -f "$CONFIG"
  fi
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
}
trap rollback EXIT

claude mcp remove -s user t-hub >/dev/null 2>&1 || true
if ! claude mcp add -s user t-hub -- "$BIN" >/dev/null; then
  echo "ensure-thub-claude: 'claude mcp add' failed" >&2
  exit 1
fi
if ! jq -e --arg bin "$BIN" '
  .mcpServers["t-hub"] == {
    "type": "stdio", "command": $bin, "args": [], "env": {}
  }
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: registration verification failed" >&2
  exit 1
fi

trap - EXIT
[ -z "$BACKUP" ] || rm -f "$BACKUP"
echo "ensure-thub-claude: registered t-hub user server ($BIN)"
