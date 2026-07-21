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
NODE_CHANGED=false
BEFORE_NODE=null
POST_NODE=null
EXPECTED_NODE="$(jq -Scn --arg bin "$BIN" '{type:"stdio", command:$bin, args:[], env:{}}')"
config_hash() { sha256sum "$CONFIG" | awk '{print $1}'; }
config_node() {
  if [ -f "$CONFIG" ]; then
    jq -Sc '.mcpServers["t-hub"] // null' "$CONFIG"
  else
    printf 'null\n'
  fi
}
refresh_expected_hash() {
  if [ -f "$CONFIG" ]; then EXPECTED_HASH="$(config_hash)"; else EXPECTED_HASH=absent; fi
}
if [ -f "$CONFIG" ]; then
  HAD_CONFIG=true
  BACKUP="$(mktemp "${CONFIG}.t-hub-backup.XXXXXX")"
  cp -p "$CONFIG" "$BACKUP"
  EXPECTED_HASH="$(config_hash)"
fi
BEFORE_NODE="$(config_node)"
record_expected_post_node() {
  POST_NODE="$1"
  current_node="$(config_node)"
  if [ "$current_node" != "$BEFORE_NODE" ]; then NODE_CHANGED=true; fi
}
rollback() {
  if ! "$NODE_CHANGED"; then
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  current_node="$(config_node 2>/dev/null || printf invalid)"
  if [ "$current_node" != "$POST_NODE" ]; then
    echo "ensure-thub-claude: t-hub registration changed concurrently; refusing unsafe rollback" >&2
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  source_hash=absent
  [ ! -f "$CONFIG" ] || source_hash="$(config_hash)"
  update="$(mktemp "${CONFIG}.t-hub-rollback.XXXXXX")"
  if [ "$BEFORE_NODE" = null ]; then
    jq 'del(.mcpServers["t-hub"]) | if (.mcpServers // {}) == {} then del(.mcpServers) else . end' \
      "$CONFIG" > "$update"
  else
    jq --argjson before "$BEFORE_NODE" '.mcpServers["t-hub"] = $before' "$CONFIG" > "$update"
  fi
  current_hash=absent
  [ ! -f "$CONFIG" ] || current_hash="$(config_hash)"
  if [ "$current_hash" != "$source_hash" ]; then
    echo "ensure-thub-claude: config changed concurrently during rollback; preserving latest file" >&2
    rm -f "$update"
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  if ! "$HAD_CONFIG" && jq -e 'keys == []' "$update" >/dev/null; then
    rm -f "$CONFIG" "$update"
  else
    chmod --reference="$CONFIG" "$update" 2>/dev/null || chmod 600 "$update"
    mv -f "$update" "$CONFIG"
  fi
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
}
trap rollback EXIT

if jq -e '.mcpServers["t-hub"] != null' "$CONFIG" >/dev/null 2>&1; then
  if claude mcp remove -s user t-hub >/dev/null 2>&1; then
    refresh_expected_hash
    record_expected_post_node null
  else
    refresh_expected_hash
    record_expected_post_node null
    echo "ensure-thub-claude: failed to remove stale t-hub registration" >&2
    exit 1
  fi
fi
if ! claude mcp add -s user t-hub -- "$BIN" >/dev/null; then
  refresh_expected_hash
  record_expected_post_node "$EXPECTED_NODE"
  echo "ensure-thub-claude: 'claude mcp add' failed" >&2
  exit 1
fi
refresh_expected_hash
record_expected_post_node "$EXPECTED_NODE"
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
