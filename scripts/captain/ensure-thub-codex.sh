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
# still holds: the item-3 capability env (a READ token by default) is injected at
# the tmux SESSION level and inherited by the `t-hub-mcp` child regardless of
# which harness spawned it (t-hub-mcp resolves `$T_HUB_CONTROL_TOKEN` first).
#
# NEVER hand-write config.toml. The live file carries user-authored `[hooks]` and
# `[hooks.state]` trust blocks that a rewrite could clobber (plan finding MED-3);
# `codex mcp add` MERGES natively and leaves those blocks byte-for-byte intact.
#
# VERSION PIN: verified against `codex-cli 0.144.3` on 2026-07-13.
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
install -d -m 700 "$(dirname "$CONFIG")"
exec 9>"${CONFIG}.t-hub.lock"
flock -x 9

# Read and mutate under the installer lock. Existing policy remains user-owned.
CURRENT="$(codex mcp get t-hub --json 2>/dev/null || true)"
if [ -n "$CURRENT" ] && printf '%s' "$CURRENT" | jq -e --arg bin "$BIN" '
  .transport.type == "stdio" and .transport.command == $bin
' >/dev/null; then
  echo "ensure-thub-codex: t-hub already points at $BIN; existing policy preserved"
  exit 0
fi
if [ -n "$CURRENT" ] && ! printf '%s' "$CURRENT" | jq -e '
  .enabled == true and .disabled_reason == null and
  .transport.type == "stdio" and .transport.args == [] and
  (.transport.env == null or .transport.env == {}) and
  .transport.env_vars == [] and .transport.cwd == null and
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

if [ -n "$CURRENT" ]; then
  if codex mcp remove t-hub >/dev/null; then
    refresh_expected_hash
  else
    refresh_expected_hash
    echo "ensure-thub-codex: failed to remove stale t-hub registration" >&2
    exit 1
  fi
fi

if codex mcp add t-hub -- "$BIN"; then
  refresh_expected_hash
  VERIFIED="$(codex mcp get t-hub --json 2>/dev/null || true)"
  if ! printf '%s' "$VERIFIED" | jq -e --arg bin "$BIN" '
    .enabled == true and .transport.type == "stdio" and
    .transport.command == $bin and .transport.args == [] and
    (.transport.env == null or .transport.env == {}) and
    .transport.env_vars == [] and .transport.cwd == null and
    .enabled_tools == null and .disabled_tools == null and
    .startup_timeout_sec == null and .tool_timeout_sec == null
  ' >/dev/null; then
    echo "ensure-thub-codex: registration verification failed" >&2
    exit 1
  fi
  trap - EXIT
  [ -z "$BACKUP" ] || rm -f "$BACKUP"
  echo "ensure-thub-codex: registered t-hub server via 'codex mcp add' ($BIN)"
else
  refresh_expected_hash
  echo "ensure-thub-codex: 'codex mcp add' failed" >&2
  exit 1
fi
