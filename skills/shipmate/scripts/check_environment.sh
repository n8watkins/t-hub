#!/usr/bin/env bash
# Print non-secret readiness facts for the Codex Shipmate bootstrap.
set -u

codex_version="unavailable"
if command -v codex >/dev/null 2>&1; then
  codex_version="$(codex --version 2>/dev/null || printf 'unavailable')"
fi

tmux_session="none"
if [ -n "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
  tmux_session="$(tmux display-message -p '#S' 2>/dev/null || printf 'unknown')"
fi

t_hub_mcp="missing"
if command -v codex >/dev/null 2>&1 && codex mcp get t-hub >/dev/null 2>&1; then
  t_hub_mcp="registered"
fi

control_handshake="missing"
if [ -f "${HOME}/.t-hub/control.json" ]; then
  control_handshake="present"
fi

control_env="missing"
if [ -n "${T_HUB_CONTROL_ADDR:-}" ] && [ -n "${T_HUB_CONTROL_TOKEN:-}" ]; then
  control_env="present"
fi

readiness="ready-for-capability-check"
if [ "$tmux_session" = "none" ] || [ "$tmux_session" = "unknown" ]; then
  readiness="not-in-t-hub-tmux"
elif [[ "$tmux_session" != th_* ]]; then
  readiness="not-in-t-hub-session"
elif [ "$t_hub_mcp" != "registered" ]; then
  readiness="needs-t-hub-mcp-provisioning"
elif [ "$control_handshake" != "present" ]; then
  readiness="t-hub-app-not-discoverable"
fi

printf 'codex_version=%s\n' "$codex_version"
printf 'tmux_session=%s\n' "$tmux_session"
printf 't_hub_mcp=%s\n' "$t_hub_mcp"
printf 'control_handshake=%s\n' "$control_handshake"
printf 'control_env=%s\n' "$control_env"
printf 'readiness=%s\n' "$readiness"
