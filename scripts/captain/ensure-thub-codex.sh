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

BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
BIN="${T_HUB_MCP_BIN:-${BIN_DIR}/t-hub-mcp}"

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

if [ ! -x "$BIN" ]; then
  echo "ensure-thub-codex: t-hub MCP binary is not executable: $BIN" >&2
  echo "ensure-thub-codex: run install-thub-codex.sh first" >&2
  exit 1
fi

if ! "$BIN" --list-tools >/dev/null 2>&1; then
  echo "ensure-thub-codex: t-hub MCP binary failed its offline catalog probe: $BIN" >&2
  exit 1
fi

CONFIG="${CODEX_HOME:-${HOME}/.codex}/config.toml"
ENV_VARS_JSON='["T_HUB_CONTROL_FILE","T_HUB_SESSION_TOKEN"]'
ENV_VARS_TOML='env_vars = ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]'
install -d -m 700 "$(dirname "$CONFIG")"
exec 9>"${CONFIG}.t-hub.lock"
flock -x 9

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
  exit 0
fi
if "$CANONICAL_TRANSPORT"; then
  echo "ensure-thub-codex: refusing to report a disabled t-hub registration as ready" >&2
  echo "ensure-thub-codex: re-enable it in Codex policy before provisioning" >&2
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
    echo "ensure-thub-codex: config changed concurrently; refusing unsafe rollback" >&2
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
  current_hash=absent
  [ ! -f "$CONFIG" ] || current_hash="$(config_hash)"
  if [ "$current_hash" != "$source_hash" ]; then
    rm -f "$update"
    echo "ensure-thub-codex: config changed concurrently; refusing replacement" >&2
    return 1
  fi
  mv -f "$update" "$CONFIG"
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

trap - EXIT
[ -z "$BACKUP" ] || rm -f "$BACKUP"
if "$LEGACY_MATCH"; then
  echo "ensure-thub-codex: migrated t-hub capability pass-through ($BIN)"
else
  echo "ensure-thub-codex: registered t-hub server via 'codex mcp add' ($BIN)"
fi
