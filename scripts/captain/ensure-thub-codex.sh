#!/usr/bin/env bash
# ensure-thub-codex.sh - register the t-hub MCP server with Codex (codex-cli).
#
# The Codex-harness sibling of the captain-dir `~/.t-hub/captain/ensure-thub-mcp.sh`
# (which provisions Claude). Deploy this to `~/.t-hub/captain/ensure-thub-codex.sh`
# (copy or symlink) so a Codex-harness captain/crew launched on this host gets the
# t-hub tools and runs unblocked. It is idempotent - safe to re-run to backfill.
#
# SCOPE DIFFERENCE from the Claude provisioner (document it, don't "fix" it):
# Codex MCP registration is USER-GLOBAL (`$CODEX_HOME/config.toml`, default
# `~/.codex/config.toml`), NOT per-repo like Claude's `.mcp.json`. Least-privilege
# still holds: the session gets stable discovery plus a durable identity reference.
# Codex only passes named variables to stdio MCP children,
# so this registration declares the two stable T-Hub variable names without storing
# their values. The stable discovery-file path and durable session identity are
# sufficient; rotating addresses and tier credentials are never stored here.
#
# NEVER rewrite config.toml wholesale. The live file carries user-authored
# `[hooks]` and `[hooks.state]` trust blocks that a rewrite could clobber (plan
# finding MED-3). `codex mcp add` establishes the native registration, then this
# script transactionally adds only its unsupported env_vars pass-through field.
#
# VERSION PIN: verified against `codex-cli 0.144.4` on 2026-07-15.
# `codex mcp add/get/remove` are stable, but re-verify on a Codex bump.
#
# The normal binary is the stable WSL-side install produced by
# install-thub-codex.sh. Override it with T_HUB_MCP_BIN only for development or
# an isolated test.
set -euo pipefail

MIGRATE_LEGACY=false
if [ "${1:-}" = "--migrate-legacy-registration" ] && [ "$#" -eq 1 ]; then
  MIGRATE_LEGACY=true
elif [ "$#" -ne 0 ]; then
  echo "usage: ensure-thub-codex.sh [--migrate-legacy-registration]" >&2
  exit 2
fi

BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
BIN="${T_HUB_MCP_BIN:-${BIN_DIR}/t-hub-mcp}"
ATOMIC_HELPER="${T_HUB_ATOMIC_CONFIG_HELPER:-$(cd "$(dirname "$0")" && pwd)/atomic-config.py}"
CONFIG="${CODEX_HOME:-${HOME}/.codex}/config.toml"

if ! command -v codex >/dev/null 2>&1; then
  echo "ensure-thub-codex: codex not on PATH - install codex-cli first" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "ensure-thub-codex: jq is required to verify the complete registration" >&2
  exit 1
fi
if ! command -v flock >/dev/null 2>&1; then
  echo "ensure-thub-codex: flock is required for safe config updates" >&2
  exit 1
fi

install -d -m 700 "$(dirname "$CONFIG")"
exec 9>"${CONFIG}.t-hub.lock"
flock -x 9

verify_cortana_catalog() {
  python3 "$ATOMIC_HELPER" verify-cortana-catalog --executable "$1"
}

if [ ! -x "$BIN" ] || [ ! -f "$BIN" ] || [ -L "$BIN" ]; then
  echo "ensure-thub-codex: t-hub MCP binary must be an executable regular file, not a symlink: $BIN" >&2
  echo "ensure-thub-codex: run install-thub-codex.sh first" >&2
  exit 1
fi
BIN_SNAPSHOT_DIR="$(mktemp -d "$(dirname "$CONFIG")/.t-hub-mcp-probe.XXXXXX")"
chmod 700 "$BIN_SNAPSHOT_DIR"
BIN_SELECTION="$BIN_SNAPSHOT_DIR/selected"
BIN_SNAPSHOT="$BIN_SNAPSHOT_DIR/t-hub-mcp"
cleanup_binary_snapshot() {
  if [ -n "${BIN_SELECTION:-}" ] && [ -f "$BIN_SELECTION" ]; then
    rm -f -- "$BIN_SELECTION"
  fi
  if [ -n "${BIN_SNAPSHOT:-}" ] && [ -f "$BIN_SNAPSHOT" ]; then
    rm -f -- "$BIN_SNAPSHOT"
  fi
  if [ -n "${BIN_SNAPSHOT_DIR:-}" ] && [ -d "$BIN_SNAPSHOT_DIR" ]; then
    rmdir -- "$BIN_SNAPSHOT_DIR"
  fi
  BIN_SELECTION=
  BIN_SNAPSHOT=
  BIN_SNAPSHOT_DIR=
}
trap cleanup_binary_snapshot EXIT
if ! BIN_SELECTION_INFO="$(
  python3 "$ATOMIC_HELPER" snapshot-executable \
    --source "$BIN" \
    --destination "$BIN_SELECTION"
)"; then
  echo "ensure-thub-codex: failed to select a verified MCP binary: $BIN" >&2
  exit 1
fi
BIN="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.path)"
BIN_CANONICAL="$BIN"
BIN_DEVICE="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.device)"
BIN_INODE="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.inode)"
BIN_UID="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.uid)"
BIN_GID="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.gid)"
BIN_MODE="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.mode)"
BIN_SIZE="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.size)"
BIN_MTIME_NS="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.mtime_ns)"
BIN_CTIME_NS="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.ctime_ns)"
BIN_DIGEST="$(printf '%s' "$BIN_SELECTION_INFO" | jq -r .source.content_sha256)"
if [ -n "${T_HUB_ENSURE_SOURCE_PAUSE_DIR:-}" ]; then
  printf 'selected\n' > "$T_HUB_ENSURE_SOURCE_PAUSE_DIR/discovered"
  source_wait_count=0
  while [ "$source_wait_count" -lt 1000 ]; do
    [ ! -e "$T_HUB_ENSURE_SOURCE_PAUSE_DIR/resume" ] || break
    sleep 0.01
    source_wait_count=$((source_wait_count + 1))
  done
  if [ ! -e "$T_HUB_ENSURE_SOURCE_PAUSE_DIR/resume" ]; then
    echo "ensure-thub-codex: timed out at the binary-selection test boundary" >&2
    exit 1
  fi
fi
if ! BIN_SNAPSHOT_INFO="$(
  python3 "$ATOMIC_HELPER" snapshot-executable \
    --source "$BIN" \
    --destination "$BIN_SNAPSHOT"
)"; then
  echo "ensure-thub-codex: failed to acquire a verified MCP binary: $BIN" >&2
  exit 1
fi
ACQUIRED_BIN_DEVICE="$(printf '%s' "$BIN_SNAPSHOT_INFO" | jq -r .source.device)"
ACQUIRED_BIN_INODE="$(printf '%s' "$BIN_SNAPSHOT_INFO" | jq -r .source.inode)"
ACQUIRED_BIN_DIGEST="$(printf '%s' "$BIN_SNAPSHOT_INFO" | jq -r .source.content_sha256)"
binary_matches_selection() {
  python3 "$ATOMIC_HELPER" verify-executable \
    --source "$BIN_CANONICAL" \
    --expected-device "$BIN_DEVICE" \
    --expected-inode "$BIN_INODE" \
    --expected-uid "$BIN_UID" \
    --expected-gid "$BIN_GID" \
    --expected-mode "$BIN_MODE" \
    --expected-size "$BIN_SIZE" \
    --expected-mtime-ns "$BIN_MTIME_NS" \
    --expected-ctime-ns "$BIN_CTIME_NS" \
    --expected-digest "$BIN_DIGEST" >/dev/null
}
if [ "$ACQUIRED_BIN_DEVICE" != "$BIN_DEVICE" ] \
  || [ "$ACQUIRED_BIN_INODE" != "$BIN_INODE" ] \
  || [ "$ACQUIRED_BIN_DIGEST" != "$BIN_DIGEST" ] \
  || [ "$(sha256sum "$BIN_SNAPSHOT" | awk '{print $1}')" != "$BIN_DIGEST" ] \
  || ! binary_matches_selection; then
  echo "ensure-thub-codex: t-hub MCP binary changed before its private snapshot was verified: $BIN" >&2
  exit 1
fi
if ! verify_cortana_catalog "$BIN_SNAPSHOT"; then
  echo "ensure-thub-codex: t-hub MCP binary lacks the exact cortana_bootstrap catalog contract: $BIN" >&2
  exit 1
fi
if ! binary_matches_selection || ! verify_cortana_catalog "$BIN_SNAPSHOT"; then
  echo "ensure-thub-codex: installed binary changed after its verified catalog snapshot: $BIN" >&2
  exit 1
fi

ENV_VARS_JSON='["T_HUB_CONTROL_FILE","T_HUB_SESSION_TOKEN"]'
ENV_VARS_TOML='env_vars = ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]'
LEGACY_ENV_VARS_JSON='["T_HUB_CONTROL_ADDR","T_HUB_CONTROL_TOKEN","T_HUB_SESSION_TOKEN"]'
BACKUP=""
HAD_CONFIG=false
EXPECTED_HASH=absent
BEFORE_DESCRIPTOR='{"presence":"absent","digest":"absent","recovery":null}'
config_hash() { sha256sum "$CONFIG" | awk '{print $1}'; }
atomic_exchange() {
  local target="$1" candidate="$2" expected="$3" outcome
  if python3 "$ATOMIC_HELPER" exchange --target "$target" --candidate "$candidate" \
    --expected-sha "$expected" --journal "$candidate.journal"; then
    return 0
  fi
  [ -d "$candidate.journal" ] || return 1
  outcome="$(python3 "$ATOMIC_HELPER" recover --journal "$candidate.journal")" || return 1
  [ "$outcome" = committed ]
}
publish_before_state() {
  if [ -z "${T_HUB_INSTALL_STATE_DIR:-}" ]; then return; fi
  BEFORE_DESCRIPTOR="$(python3 "$ATOMIC_HELPER" capture \
    --source "$CONFIG" --recovery "$T_HUB_INSTALL_STATE_DIR/codex-before.bin")"
  state="$(jq -cn --arg target "$CONFIG" --argjson before "$BEFORE_DESCRIPTOR" \
    '{version:1,target:$target,status:"before",before:$before}')"
  python3 "$ATOMIC_HELPER" publish --path "$T_HUB_INSTALL_STATE_DIR/codex-state.json" --value "$state"
}
publish_committed_state() {
  if ! binary_matches_selection || ! verify_cortana_catalog "$BIN_SNAPSHOT"; then
    echo "ensure-thub-codex: installed binary changed before config commit" >&2
    return 1
  fi
  if [ -z "${T_HUB_INSTALL_STATE_DIR:-}" ]; then return; fi
  if [ -f "$CONFIG" ]; then
    post="$(python3 "$ATOMIC_HELPER" describe --path "$CONFIG")"
    post="$(printf '%s' "$post" | jq -c '{presence:"present",digest:.digest,description:.description}')"
  else
    post='{"presence":"absent","digest":"absent"}'
  fi
  state="$(jq -cn --arg target "$CONFIG" --argjson before "$BEFORE_DESCRIPTOR" \
    --argjson post "$post" \
    '{version:1,target:$target,status:"committed",before:$before,post:$post}')"
  python3 "$ATOMIC_HELPER" publish --path "$T_HUB_INSTALL_STATE_DIR/codex-state.json" --value "$state"
}
refresh_expected_hash() {
  if [ -f "$CONFIG" ]; then EXPECTED_HASH="$(config_hash)"; else EXPECTED_HASH=absent; fi
}
begin_config_transaction() {
  if [ -n "${T_HUB_ENSURE_LATE_SOURCE_PAUSE_DIR:-}" ] \
    && [ -z "${LATE_SOURCE_PAUSED:-}" ]; then
    printf 'selected\n' > "$T_HUB_ENSURE_LATE_SOURCE_PAUSE_DIR/discovered"
    late_wait_count=0
    while [ "$late_wait_count" -lt 1000 ]; do
      [ ! -e "$T_HUB_ENSURE_LATE_SOURCE_PAUSE_DIR/resume" ] || break
      sleep 0.01
      late_wait_count=$((late_wait_count + 1))
    done
    if [ ! -e "$T_HUB_ENSURE_LATE_SOURCE_PAUSE_DIR/resume" ]; then
      echo "ensure-thub-codex: timed out at the late binary verification boundary" >&2
      return 1
    fi
    LATE_SOURCE_PAUSED=true
  fi
  if ! binary_matches_selection || ! verify_cortana_catalog "$BIN_SNAPSHOT"; then
    echo "ensure-thub-codex: installed binary changed before config mutation" >&2
    return 1
  fi
  if [ -f "$CONFIG" ]; then
    HAD_CONFIG=true
    BACKUP="$(mktemp "${CONFIG}.t-hub-backup.XXXXXX")"
    cp -p "$CONFIG" "$BACKUP"
    EXPECTED_HASH="$(config_hash)"
  fi
  trap rollback_and_cleanup EXIT
}
rollback() {
  current_hash=absent
  [ ! -f "$CONFIG" ] || current_hash="$(config_hash)"
  if [ "$current_hash" != "$EXPECTED_HASH" ]; then
    echo "ensure-thub-codex: config changed concurrently; refusing unsafe rollback" >&2
    [ -z "$BACKUP" ] || rm -f "$BACKUP"
    return
  fi
  if "$HAD_CONFIG"; then
    if ! atomic_exchange "$CONFIG" "$BACKUP" "$EXPECTED_HASH"; then
      echo "ensure-thub-codex: durable atomic rollback failed" >&2
      return
    fi
  else
    rm -f "$CONFIG"
  fi
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
}
rollback_and_cleanup() {
  rollback
  cleanup_binary_snapshot
}
commit_config_transaction() {
  if ! binary_matches_selection || ! verify_cortana_catalog "$BIN_SNAPSHOT"; then
    echo "ensure-thub-codex: installed binary changed during config mutation" >&2
    return 1
  fi
  trap - EXIT
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
  cleanup_binary_snapshot
}

# This exact under-lock boundary is durable before any registration command can
# mutate the file.  The caller must never infer or adopt an earlier snapshot.
publish_before_state

has_exact_managed_table_shape() {
  awk '
    /^\[mcp_servers\.t-hub\]$/ {
      if (found) bad = 1
      found = 1
      in_target = 1
      next
    }
    /^\[mcp_servers\.t-hub\./ {
      bad = 1
      in_target = 0
      next
    }
    /^\[/ { in_target = 0 }
    in_target && /^[[:space:]]*$/ { next }
    in_target && /^[[:space:]]*command[[:space:]]*=/ {
      commands++
      next
    }
    in_target && /^[[:space:]]*env_vars[[:space:]]*=/ {
      env_vars++
      next
    }
    in_target { bad = 1 }
    END {
      if (found != 1 || commands != 1 || env_vars > 1 || bad) exit 1
    }
  ' "$CONFIG"
}

has_exact_legacy_root_shape() {
  awk '
    /^\[mcp_servers\.t-hub\]$/ {
      if (found) bad = 1
      found = 1
      in_target = 1
      next
    }
    /^\[/ { in_target = 0 }
    in_target && /^[[:space:]]*($|#)/ { next }
    in_target && /^[[:space:]]*(command|args|env|env_vars)[[:space:]]*=/ { next }
    in_target { bad = 1 }
    END { if (found != 1 || bad) exit 1 }
  ' "$CONFIG"
}

# Read and mutate under the installer lock. Existing policy remains user-owned.
CURRENT="$(codex mcp get t-hub --json 2>/dev/null || true)"
CANONICAL_TRANSPORT=false
if [ -n "$CURRENT" ] && printf '%s' "$CURRENT" | jq -e --arg bin "$BIN" '
  .transport.type == "stdio" and .transport.command == $bin and
  .transport.args == [] and (.transport.env == null or .transport.env == {}) and
  .transport.env_vars == [
    "T_HUB_CONTROL_FILE",
    "T_HUB_SESSION_TOKEN"
  ] and .transport.cwd == null
' >/dev/null; then
  CANONICAL_TRANSPORT=true
fi

if "$CANONICAL_TRANSPORT" && printf '%s' "$CURRENT" | jq -e '
  .enabled == true and .disabled_reason == null
' >/dev/null; then
  echo "ensure-thub-codex: t-hub already points at $BIN with capability pass-through; existing policy preserved"
  publish_committed_state
  exit 0
fi
if "$CANONICAL_TRANSPORT"; then
  echo "ensure-thub-codex: refusing to report a disabled t-hub registration as ready" >&2
  echo "ensure-thub-codex: re-enable it in Codex policy before provisioning" >&2
  exit 1
fi

LEGACY_REGISTRATION=false
if [ -n "$CURRENT" ] && printf '%s' "$CURRENT" | jq -e \
  --arg bin "$BIN" --argjson legacy_env_vars "$LEGACY_ENV_VARS_JSON" '
  .enabled == true and .disabled_reason == null and
  .transport.type == "stdio" and .transport.command == $bin and
  .transport.args == [] and (.transport.env == null or .transport.env == {}) and
  .transport.env_vars == $legacy_env_vars and .transport.cwd == null and
  .enabled_tools == null and .disabled_tools == null and
  .startup_timeout_sec == null and .tool_timeout_sec == null
' >/dev/null; then
  LEGACY_REGISTRATION=true
fi
if "$LEGACY_REGISTRATION" && ! has_exact_legacy_root_shape; then
  LEGACY_REGISTRATION=false
fi

migrate_legacy_registration() {
  source_hash="$(config_hash)"
  env_line="$(awk '
    /^\[mcp_servers\.t-hub\]$/ { in_target = 1; next }
    /^\[/ { in_target = 0 }
    in_target && /^[[:space:]]*env_vars[[:space:]]*=/ { print NR }
  ' "$CONFIG")"
  if [ -z "$env_line" ] || [[ "$env_line" == *$'\n'* ]] ||
    ! sed -n "${env_line}p" "$CONFIG" | grep -Eq '^[[:space:]]*env_vars[[:space:]]*=[[:space:]]*\[[[:space:]]*"T_HUB_CONTROL_ADDR"[[:space:]]*,[[:space:]]*"T_HUB_CONTROL_TOKEN"[[:space:]]*,[[:space:]]*"T_HUB_SESSION_TOKEN"[[:space:]]*\][[:space:]]*(#.*)?$'; then
    echo "ensure-thub-codex: legacy root env_vars declaration is not uniquely replaceable" >&2
    return 1
  fi

  update="$(mktemp "${CONFIG}.t-hub-update.XXXXXX")"
  {
    if [ "$env_line" -gt 1 ]; then head -n "$((env_line - 1))" "$CONFIG"; fi
    printf '%s\n' "$ENV_VARS_TOML"
    tail -n "+$((env_line + 1))" "$CONFIG"
  } > "$update"
  chmod --reference="$CONFIG" "$update"

  if ! atomic_exchange "$CONFIG" "$update" "$source_hash"; then
    rm -f "$update"
    echo "ensure-thub-codex: config changed concurrently; refusing replacement" >&2
    return 1
  fi
  python3 "$ATOMIC_HELPER" discard --path "$update"
  refresh_expected_hash

  verified="$(codex mcp get t-hub --json 2>/dev/null || true)"
  post_verification_hash="$(config_hash)"
  if [ "$post_verification_hash" != "$EXPECTED_HASH" ]; then
    echo "ensure-thub-codex: config changed concurrently during migration verification" >&2
    return 1
  fi
  if ! printf '%s' "$verified" | jq -e --arg bin "$BIN" --argjson env_vars "$ENV_VARS_JSON" '
    .enabled == true and .disabled_reason == null and
    .transport.type == "stdio" and .transport.command == $bin and
    .transport.args == [] and (.transport.env == null or .transport.env == {}) and
    .transport.env_vars == $env_vars and .transport.cwd == null
  ' >/dev/null; then
    echo "ensure-thub-codex: migrated registration verification failed" >&2
    return 1
  fi
  before_semantics="$(printf '%s' "$CURRENT" | jq -Sc '
    .transport.env = (.transport.env // {}) | del(.transport.env_vars)
  ')"
  after_semantics="$(printf '%s' "$verified" | jq -Sc '
    .transport.env = (.transport.env // {}) | del(.transport.env_vars)
  ')"
  if [ "$before_semantics" != "$after_semantics" ]; then
    echo "ensure-thub-codex: migration changed registration semantics beyond env_vars" >&2
    return 1
  fi
}

if "$MIGRATE_LEGACY"; then
  if ! "$LEGACY_REGISTRATION"; then
    echo "ensure-thub-codex: refusing migration because t-hub is not the exact enabled legacy registration" >&2
    exit 1
  fi
  if [ -L "$CONFIG" ]; then
    echo "ensure-thub-codex: refusing to mutate symlinked config: $CONFIG" >&2
    exit 1
  fi
  begin_config_transaction
  if ! migrate_legacy_registration; then
    exit 1
  fi
  publish_committed_state
  commit_config_transaction
  echo "ensure-thub-codex: migrated legacy t-hub capability pass-through ($BIN)"
  exit 0
fi
if "$LEGACY_REGISTRATION"; then
  echo "ensure-thub-codex: legacy t-hub registration requires --migrate-legacy-registration" >&2
  exit 1
fi

LEGACY_MATCH=false
if [ -n "$CURRENT" ] && printf '%s' "$CURRENT" | jq -e --arg bin "$BIN" '
  .enabled == true and .disabled_reason == null and
  .transport.type == "stdio" and .transport.command == $bin and
  .transport.args == [] and (.transport.env == null or .transport.env == {}) and
  .transport.env_vars == [] and .transport.cwd == null
' >/dev/null; then
  LEGACY_MATCH=true
fi

if [ -L "$CONFIG" ]; then
  echo "ensure-thub-codex: refusing to mutate symlinked config: $CONFIG" >&2
  exit 1
fi
if [ -n "$CURRENT" ] && ! "$LEGACY_MATCH" && ! has_exact_managed_table_shape; then
  echo "ensure-thub-codex: refusing to replace a customized t-hub registration" >&2
  echo "ensure-thub-codex: the managed table contains unknown fields or nested policy" >&2
  exit 1
fi
if [ -n "$CURRENT" ] && ! "$LEGACY_MATCH" && ! printf '%s' "$CURRENT" | jq -e '
  .enabled == true and .disabled_reason == null and
  .transport.type == "stdio" and .transport.args == [] and
  (.transport.env == null or .transport.env == {}) and
  (.transport.env_vars == [] or .transport.env_vars == [
    "T_HUB_CONTROL_FILE",
    "T_HUB_SESSION_TOKEN"
  ] or .transport.env_vars == [
    "T_HUB_CONTROL_ADDR",
    "T_HUB_CONTROL_TOKEN",
    "T_HUB_SESSION_TOKEN"
  ]) and .transport.cwd == null and
  .enabled_tools == null and .disabled_tools == null and
  .startup_timeout_sec == null and .tool_timeout_sec == null
' >/dev/null; then
  echo "ensure-thub-codex: refusing to replace a customized t-hub registration" >&2
  echo "ensure-thub-codex: preserve or remove its policy manually before changing the command" >&2
  exit 1
fi

begin_config_transaction

insert_env_vars() {
  source_hash="$(config_hash)"
  update="$(mktemp "${CONFIG}.t-hub-update.XXXXXX")"
  if ! cp -p "$CONFIG" "$update"; then
    rm -f "$update"
    return 1
  fi

  env_line="$(awk '
    /^\[mcp_servers\.t-hub\]$/ { in_target = 1; next }
    /^\[/ { in_target = 0 }
    in_target && /^[[:space:]]*env_vars[[:space:]]*=/ { print NR }
  ' "$update")"
  command_line="$(awk '
    /^\[mcp_servers\.t-hub\]$/ { in_target = 1; next }
    /^\[/ { in_target = 0 }
    in_target && /^[[:space:]]*command[[:space:]]*=/ { print NR }
  ' "$update")"

  if [ -n "$env_line" ]; then
    if [[ "$env_line" == *$'\n'* ]] ||
      ! sed -n "${env_line}p" "$update" | grep -Eq '^[[:space:]]*env_vars[[:space:]]*=[[:space:]]*\[[[:space:]]*\][[:space:]]*(#.*)?$'; then
      echo "ensure-thub-codex: refusing to rewrite a customized env_vars declaration" >&2
      rm -f "$update"
      return 1
    fi
    edit="${env_line}c\\${ENV_VARS_TOML}"
  else
    if [ -z "$command_line" ] || [[ "$command_line" == *$'\n'* ]]; then
      echo "ensure-thub-codex: could not locate the managed t-hub command" >&2
      rm -f "$update"
      return 1
    fi
    edit="${command_line}a\\${ENV_VARS_TOML}"
  fi

  if ! sed -i "$edit" "$update"; then
    rm -f "$update"
    return 1
  fi
  if ! atomic_exchange "$CONFIG" "$update" "$source_hash"; then
    rm -f "$update"
    echo "ensure-thub-codex: config changed concurrently; refusing replacement" >&2
    return 1
  fi
  python3 "$ATOMIC_HELPER" discard --path "$update"
}

POLICY_BEFORE=""
if "$LEGACY_MATCH"; then
  POLICY_BEFORE="$(printf '%s' "$CURRENT" | jq -c '{
    enabled, disabled_reason, enabled_tools, disabled_tools,
    startup_timeout_sec, tool_timeout_sec
  }')"
elif [ -n "$CURRENT" ]; then
  if codex mcp remove t-hub >/dev/null; then
    refresh_expected_hash
  else
    refresh_expected_hash
    echo "ensure-thub-codex: failed to remove stale t-hub registration" >&2
    exit 1
  fi
fi

if ! "$LEGACY_MATCH" && codex mcp add t-hub -- "$BIN"; then
  refresh_expected_hash
elif ! "$LEGACY_MATCH"; then
  refresh_expected_hash
  echo "ensure-thub-codex: 'codex mcp add' failed" >&2
  exit 1
fi

if ! insert_env_vars; then
  echo "ensure-thub-codex: failed to declare capability environment pass-through" >&2
  exit 1
fi
refresh_expected_hash

VERIFIED="$(codex mcp get t-hub --json 2>/dev/null || true)"
if ! printf '%s' "$VERIFIED" | jq -e --arg bin "$BIN" --argjson env_vars "$ENV_VARS_JSON" '
  .enabled == true and .transport.type == "stdio" and
  .transport.command == $bin and .transport.args == [] and
  (.transport.env == null or .transport.env == {}) and
  .transport.env_vars == $env_vars and .transport.cwd == null
' >/dev/null; then
  echo "ensure-thub-codex: registration verification failed" >&2
  exit 1
fi

if "$LEGACY_MATCH"; then
  POLICY_AFTER="$(printf '%s' "$VERIFIED" | jq -c '{
    enabled, disabled_reason, enabled_tools, disabled_tools,
    startup_timeout_sec, tool_timeout_sec
  }')"
  if [ "$POLICY_BEFORE" != "$POLICY_AFTER" ]; then
    echo "ensure-thub-codex: existing Codex policy changed during migration" >&2
    exit 1
  fi
else
  if ! printf '%s' "$VERIFIED" | jq -e '
    .disabled_reason == null and .enabled_tools == null and
    .disabled_tools == null and .startup_timeout_sec == null and
    .tool_timeout_sec == null
  ' >/dev/null; then
    echo "ensure-thub-codex: registration policy verification failed" >&2
    exit 1
  fi
fi

publish_committed_state
commit_config_transaction
if "$LEGACY_MATCH"; then
  echo "ensure-thub-codex: migrated t-hub capability pass-through ($BIN)"
else
  echo "ensure-thub-codex: registered t-hub server via 'codex mcp add' ($BIN)"
fi
