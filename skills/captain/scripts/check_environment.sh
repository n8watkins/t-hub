#!/usr/bin/env bash
# Print non-secret readiness facts for a Codex or Claude Captain bootstrap.
set -u

codex_version="unavailable"
if command -v codex >/dev/null 2>&1; then
  codex_version="$(codex --version 2>/dev/null || printf 'unavailable')"
fi

claude_version="unavailable"
if command -v claude >/dev/null 2>&1; then
  claude_version="$(claude --version 2>/dev/null || printf 'unavailable')"
fi

harness="${T_HUB_HARNESS:-}"
if [ -z "$harness" ]; then
  if [ -n "${CODEX_THREAD_ID:-}" ]; then
    harness=codex
  elif [ -n "${CLAUDE_SESSION_ID:-}" ]; then
    harness=claude
  elif [ -n "${CLAUDE_CODE_ENTRYPOINT:-}" ]; then
    harness=claude
  else
    harness=codex
  fi
fi

tmux_session="none"
if [ -n "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
  tmux_session="$(tmux display-message -p '#S' 2>/dev/null || printf 'unknown')"
fi

t_hub_mcp="missing"
case "$harness" in
  codex)
    if command -v codex >/dev/null 2>&1 && codex mcp get t-hub >/dev/null 2>&1; then
      t_hub_mcp="registered"
    fi
    ;;
  claude)
    if command -v claude >/dev/null 2>&1 && claude mcp get t-hub >/dev/null 2>&1; then
      t_hub_mcp="registered"
    fi
    ;;
  *) t_hub_mcp="unsupported-harness" ;;
esac

skill_integrity="unknown"
skill_root="$(cd "$(dirname "$0")/.." && pwd)"
marker="$skill_root/.t-hub-managed"
if [ -f "$marker" ] && command -v sha256sum >/dev/null 2>&1; then
  expected="$(sed -n 's/^source-sha256=//p' "$marker")"
  actual="$(
    (
      cd "$skill_root"
      find . -mindepth 1 ! -name .t-hub-managed -print0 \
        | sort -z \
        | while IFS= read -r -d '' entry; do
            if [ -L "$entry" ]; then
              printf 'l\0%s\0%s\0%s\0' "$entry" "$(stat -c '%a' "$entry")" "$(readlink "$entry")"
            elif [ -d "$entry" ]; then
              printf 'd\0%s\0%s\0' "$entry" "$(stat -c '%a' "$entry")"
            elif [ -f "$entry" ]; then
              printf 'f\0%s\0%s\0' "$entry" "$(stat -c '%a' "$entry")"
              sha256sum "$entry"
            else
              printf 'o\0%s\0%s\0' "$entry" "$(stat -c '%f' "$entry")"
            fi
          done
    ) | sha256sum | awk '{print $1}'
  )"
  if [ -n "$expected" ] && [ "$actual" = "$expected" ]; then
    skill_integrity="verified"
  else
    skill_integrity="drifted"
  fi
elif [ -f "$marker" ]; then
  skill_integrity="unverifiable"
else
  skill_integrity="source-tree"
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
elif [ "$skill_integrity" = "drifted" ]; then
  readiness="needs-t-hub-skill-reinstall"
elif [ "$control_handshake" != "present" ]; then
  readiness="t-hub-app-not-discoverable"
fi

printf 'codex_version=%s\n' "$codex_version"
printf 'claude_version=%s\n' "$claude_version"
printf 'harness=%s\n' "$harness"
printf 'tmux_session=%s\n' "$tmux_session"
printf 't_hub_mcp=%s\n' "$t_hub_mcp"
printf 'control_handshake=%s\n' "$control_handshake"
printf 'control_env=%s\n' "$control_env"
printf 'skill_integrity=%s\n' "$skill_integrity"
printf 'readiness=%s\n' "$readiness"
