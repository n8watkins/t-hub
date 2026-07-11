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
# VERSION PIN: verified against the fleet-pinned `codex-cli 0.142.5` (the recorded
# fixture version). `codex mcp add/get` are stable, but re-verify on a Codex bump.
#
# Override the MCP binary with T_HUB_MCP_BIN if it moves (e.g. a packaged
# sidecar), exactly like ensure-thub-mcp.sh.
set -u

BIN="${T_HUB_MCP_BIN:-/home/natkins/projects/tools/t-hub/t-hub-app/apps/desktop/src-tauri/target/debug/t-hub-mcp}"

if ! command -v codex >/dev/null 2>&1; then
  echo "ensure-thub-codex: codex not on PATH - install codex-cli first" >&2
  exit 1
fi

# --- Idempotency: skip if Codex already registers the t-hub server ----------
if codex mcp get t-hub >/dev/null 2>&1; then
  echo "ensure-thub-codex: codex already registers t-hub (leaving config.toml untouched)"
  exit 0
fi

# --- Register via the native merge (preserves [hooks]/[hooks.state]) --------
if codex mcp add t-hub -- "$BIN"; then
  echo "ensure-thub-codex: registered t-hub server via 'codex mcp add' ($BIN)"
else
  echo "ensure-thub-codex: 'codex mcp add' failed" >&2
  exit 1
fi
