#!/usr/bin/env bash
# Register the stable T-Hub MCP binary in Claude Code's user scope.
set -euo pipefail

BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
BIN="${T_HUB_MCP_BIN:-${BIN_DIR}/t-hub-mcp}"
CONFIG="${HOME}/.claude.json"
ATOMIC_HELPER="${T_HUB_ATOMIC_CONFIG_HELPER:-$(cd "$(dirname "$0")" && pwd)/atomic-config.py}"

publish_committed_state() {
  if [ -z "${T_HUB_INSTALL_STATE_DIR:-}" ]; then return; fi
  hash="$(sha256sum "$CONFIG" | awk '{print $1}')"
  structure="$(jq -Sc '{parent_present:has("mcpServers"),key_present:(.mcpServers|has("t-hub")),value:.mcpServers["t-hub"]}' "$CONFIG")"
  state="$(jq -cn --arg hash "$hash" --argjson structure "$structure" '{presence:"present",hash:$hash,structure:$structure}')"
  python3 "$ATOMIC_HELPER" publish --path "$T_HUB_INSTALL_STATE_DIR/claude-state.json" --value "$state"
}

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

if [ -f "$CONFIG" ] && ! jq -e '
  (has("mcpServers") | not) or (.mcpServers | type) == "object"
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: refusing malformed non-object mcpServers parent" >&2
  exit 1
fi
if [ -f "$CONFIG" ] && jq -e '
  (.mcpServers | type) == "object" and
  (.mcpServers | has("t-hub")) and
  (.mcpServers["t-hub"] | type) != "object"
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: refusing malformed non-object t-hub registration" >&2
  exit 1
fi

if [ -f "$CONFIG" ] && jq -e --arg bin "$BIN" '
  .mcpServers["t-hub"].type == "stdio" and
  .mcpServers["t-hub"].command == $bin
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: t-hub already points at $BIN; existing args and environment preserved"
  publish_committed_state
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
BEFORE_STATE='{}'
POST_STATE='{}'
EXPECTED_NODE="$(jq -Scn --arg bin "$BIN" '{type:"stdio", command:$bin, args:[], env:{}}')"
config_hash() { sha256sum "$CONFIG" | awk '{print $1}'; }
config_state() {
  if [ -f "$CONFIG" ]; then
    jq -Sc '{
      parent_present: has("mcpServers"),
      parent_type: (if has("mcpServers") then (.mcpServers | type) else "absent" end),
      key_present: (if (.mcpServers | type) == "object" then (.mcpServers | has("t-hub")) else false end),
      value: (if (.mcpServers | type) == "object" and (.mcpServers | has("t-hub")) then .mcpServers["t-hub"] else null end)
    }' "$CONFIG"
  else
    printf '{"parent_present":false,"parent_type":"absent","key_present":false,"value":null}\n'
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
BEFORE_STATE="$(config_state)"
record_remove_result() {
  actual_state="$(config_state)"
  if [ "$actual_state" != "$BEFORE_STATE" ]; then NODE_CHANGED=true; fi
  POST_STATE="$actual_state"
}
record_add_result() {
  actual_state="$(config_state)"
  actual_value="$(printf '%s' "$actual_state" | jq -Sc '.value')"
  post_value="$(printf '%s' "$POST_STATE" | jq -Sc '.value')"
  if [ "$actual_value" = "$EXPECTED_NODE" ]; then
    POST_STATE="$actual_state"
    NODE_CHANGED=true
  elif [ "$actual_value" != "$post_value" ]; then
    return
  fi
}
rollback() {
  if ! "$NODE_CHANGED"; then
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  current_state="$(config_state 2>/dev/null || printf invalid)"
  if [ "$current_state" != "$POST_STATE" ]; then
    echo "ensure-thub-claude: t-hub registration changed concurrently; refusing unsafe rollback" >&2
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  source_hash=absent
  [ ! -f "$CONFIG" ] || source_hash="$(config_hash)"
  update="$(mktemp "${CONFIG}.t-hub-rollback.XXXXXX")"
  before_parent_present="$(printf '%s' "$BEFORE_STATE" | jq -r '.parent_present')"
  before_key_present="$(printf '%s' "$BEFORE_STATE" | jq -r '.key_present')"
  if [ "$before_key_present" = true ]; then
    before_value="$(printf '%s' "$BEFORE_STATE" | jq -c '.value')"
    jq --argjson before "$before_value" '.mcpServers["t-hub"] = $before' "$CONFIG" > "$update"
  elif [ "$before_parent_present" = true ]; then
    jq 'del(.mcpServers["t-hub"])' "$CONFIG" > "$update"
  else
    jq 'del(.mcpServers["t-hub"]) | if (.mcpServers // {}) == {} then del(.mcpServers) else . end' "$CONFIG" > "$update"
  fi
  remove_restored_config=false
  if ! "$HAD_CONFIG" && jq -e 'keys == []' "$update" >/dev/null; then
    remove_restored_config=true
  fi
  if ! python3 "$ATOMIC_HELPER" exchange --target "$CONFIG" --candidate "$update" \
    --expected-sha "$source_hash"; then
    echo "ensure-thub-claude: config changed concurrently during rollback; preserving latest file" >&2
    rm -f "$update"
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  if "$remove_restored_config"; then
    python3 "$ATOMIC_HELPER" discard --path "$CONFIG"
    python3 "$ATOMIC_HELPER" discard --path "$update"
  else
    python3 "$ATOMIC_HELPER" discard --path "$update"
  fi
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
}
trap rollback EXIT

if jq -e '.mcpServers["t-hub"] != null' "$CONFIG" >/dev/null 2>&1; then
  if claude mcp remove -s user t-hub >/dev/null 2>&1; then
    refresh_expected_hash
    record_remove_result
  else
    refresh_expected_hash
    record_remove_result
    echo "ensure-thub-claude: failed to remove stale t-hub registration" >&2
    exit 1
  fi
fi
if ! claude mcp add -s user t-hub -- "$BIN" >/dev/null; then
  refresh_expected_hash
  record_add_result
  echo "ensure-thub-claude: 'claude mcp add' failed" >&2
  exit 1
fi
refresh_expected_hash
record_add_result
if ! jq -e --arg bin "$BIN" '
  .mcpServers["t-hub"] == {
    "type": "stdio", "command": $bin, "args": [], "env": {}
  }
' "$CONFIG" >/dev/null; then
  echo "ensure-thub-claude: registration verification failed" >&2
  exit 1
fi

publish_committed_state
trap - EXIT
[ -z "$BACKUP" ] || rm -f "$BACKUP"
echo "ensure-thub-claude: registered t-hub user server ($BIN)"
