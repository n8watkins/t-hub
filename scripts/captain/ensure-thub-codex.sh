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
set -u

BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
BIN="${T_HUB_MCP_BIN:-${BIN_DIR}/t-hub-mcp}"

if ! command -v codex >/dev/null 2>&1; then
  echo "ensure-thub-codex: codex not on PATH - install codex-cli first" >&2
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

# --- Converge the native registration without hand-writing config.toml -------
CURRENT="$(codex mcp get t-hub --json 2>/dev/null || true)"
if [ -n "$CURRENT" ] && printf '%s' "$CURRENT" | grep -Fq "\"command\": \"$BIN\""; then
  echo "ensure-thub-codex: t-hub already points at $BIN"
  exit 0
fi

if [ -n "$CURRENT" ]; then
  if ! codex mcp remove t-hub >/dev/null; then
    echo "ensure-thub-codex: failed to remove stale t-hub registration" >&2
    exit 1
  fi
fi

if codex mcp add t-hub -- "$BIN"; then
  echo "ensure-thub-codex: registered t-hub server via 'codex mcp add' ($BIN)"
else
  echo "ensure-thub-codex: 'codex mcp add' failed" >&2
  exit 1
fi
