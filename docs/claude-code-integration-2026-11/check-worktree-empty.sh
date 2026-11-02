#!/usr/bin/env bash
# User-level PreToolUse guard for `git worktree remove` (applies on every project).
# Refuses the removal while a LIVE t-hub agent session is cwd-inside that worktree
# (the workspace is not empty). Emits a deny decision as JSON; silence = allow.
# On non-t-hub projects this is a safe no-op: no t-hub session is cwd-inside a
# non-t-hub worktree, so it allows. The captain closes the agent first
# (close_terminal), which empties the worktree, and then the removal is permitted.
input=$(cat)
cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // ""')
case "$cmd" in
  *"git worktree remove"*) ;;
  *) exit 0 ;;  # not the guarded command
esac

# The path arg: strip everything up to "remove", drop -f/--force flags, take first token.
path=$(printf '%s' "$cmd" \
  | sed -E 's/.*git[[:space:]]+worktree[[:space:]]+remove[[:space:]]+//' \
  | tr ' ' '\n' | grep -vE '^(-f|--force)$' | head -n1)
[ -z "$path" ] && exit 0
abs=$(readlink -f "$path" 2>/dev/null || printf '%s' "$path")

# Any live t-hub session whose current path is the worktree (or nested under it)?
busy=""
for s in $(tmux -L t-hub list-sessions -F '#{session_name}' 2>/dev/null); do
  p=$(tmux -L t-hub display-message -p -t "$s" '#{pane_current_path}' 2>/dev/null)
  ap=$(readlink -f "$p" 2>/dev/null || printf '%s' "$p")
  case "$ap/" in
    "$abs/"*) busy="$s"; break ;;
  esac
done

if [ -n "$busy" ]; then
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Workspace not empty: live agent %s is cwd-inside %s. Close it first (close_terminal), then remove the worktree."}}' "$busy" "$path"
fi
exit 0
