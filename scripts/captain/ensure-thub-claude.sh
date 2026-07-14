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
if ! command -v flock >/dev/null 2>&1; then
  echo "ensure-thub-claude: flock is required for safe config updates" >&2
  exit 1
fi
if [ ! -x "$BIN" ] || ! "$BIN" --list-tools >/dev/null 2>&1; then
  echo "ensure-thub-claude: t-hub MCP binary is unavailable or invalid: $BIN" >&2
  exit 1
fi

exec 9>"${CONFIG}.t-hub.lock"
flock -x 9

if [ -f "$CONFIG" ] && jq -e --arg bin "$BIN" '
  .mcpServers["t-hub"].type == "stdio" and
  .mcpServers["t-hub"].command == $bin
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: t-hub already points at $BIN; existing args and environment preserved"
  exit 0
fi
if [ -f "$CONFIG" ] && jq -e '.mcpServers["t-hub"] != null' "$CONFIG" >/dev/null \
  && ! jq -e '
    .mcpServers["t-hub"].type == "stdio" and
    (.mcpServers["t-hub"].args // []) == [] and
    (.mcpServers["t-hub"].env // {}) == {}
  ' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: refusing to replace a customized t-hub registration" >&2
  echo "ensure-thub-claude: preserve or remove its args and environment manually before changing the command" >&2
  exit 1
fi

BACKUP=""
HAD_CONFIG=false
EXPECTED_HASH=absent
config_hash() { sha256sum "$CONFIG" | awk '{print $1}'; }
refresh_expected_hash() {
  if [ -f "$CONFIG" ]; then EXPECTED_HASH="$(config_hash)"; else EXPECTED_HASH=absent; fi
}
if [ -f "$CONFIG" ]; then
  HAD_CONFIG=true
  BACKUP="$(mktemp "${CONFIG}.t-hub-backup.XXXXXX")"
  cp -p "$CONFIG" "$BACKUP"
  EXPECTED_HASH="$(config_hash)"
fi
rollback() {
  current_hash=absent
  [ ! -f "$CONFIG" ] || current_hash="$(config_hash)"
  if [ "$current_hash" != "$EXPECTED_HASH" ]; then
    echo "ensure-thub-claude: config changed concurrently; refusing unsafe rollback" >&2
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  if "$HAD_CONFIG"; then
    cp -p "$BACKUP" "$CONFIG"
  else
    rm -f "$CONFIG"
  fi
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
}
trap rollback EXIT

if jq -e '.mcpServers["t-hub"] != null' "$CONFIG" >/dev/null 2>&1; then
  if claude mcp remove -s user t-hub >/dev/null 2>&1; then
    refresh_expected_hash
  else
    refresh_expected_hash
    echo "ensure-thub-claude: failed to remove stale t-hub registration" >&2
    exit 1
  fi
fi
if ! claude mcp add -s user t-hub -- "$BIN" >/dev/null; then
  refresh_expected_hash
  echo "ensure-thub-claude: 'claude mcp add' failed" >&2
  exit 1
fi
refresh_expected_hash
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
