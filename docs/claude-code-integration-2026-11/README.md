# How T-Hub was wired into Claude Code and Codex (removed 2026-11-02)

T-Hub was archived on 2026-11-01.
On 2026-11-02 its integration was removed from the owner's machine so idle hooks and servers stop running.
This folder keeps an exact copy of what was installed, for reference or to restore it if T-Hub comes back.

## What was installed

- `claude-settings-thub.json`: the T-Hub entries from `~/.claude/settings.json`.
  The T-Hub agent binary (`~/.local/lib/t-hub/agents/<hash>/t-hub-agent`) was called with `--hook <Event>` on 15 events (CwdChanged, Elicitation, Notification, PermissionRequest, SessionEnd, SessionStart, Stop, StopFailure, SubagentStart, SubagentStop, TaskCompleted, TaskCreated, UserPromptSubmit, WorktreeCreate, WorktreeRemove), each tagged `# __t_hub_managed__`.
  The status line was `t-hub-agent --statusline`, refreshed every 5 seconds.
  Two permission rules pre-approved `mcp__t-hub__close_terminal` and `mcp__t-hub__close_tab`.
- `check-worktree-empty.sh`: a PreToolUse guard on `git worktree remove` that refused while a live T-Hub agent session (tmux socket `t-hub`) was working inside that worktree.
- The T-Hub MCP server, registered for Claude Code at user scope as `t-hub` with command `/home/natkins/.t-hub/bin/t-hub-mcp`.
- `codex-config-thub.toml`: the matching Codex entries from `~/.codex/config.toml` (the `t-hub` MCP server with its per-tool approvals, and the trusted T-Hub project folders).
- `skills/`: the four T-Hub skills that were in `~/.claude/skills` (captain, fleet-orchestrator, handoff-thub, shipmate); `codex-*` folders are the Codex copies where they differed.

## Restoring

1. Reinstall T-Hub so `~/.t-hub/bin/t-hub-mcp` and the agent binary exist (the agent path contains a build hash; use the new one).
2. Merge the `hooks`, `statusLine` and `permissions.allow` entries from `claude-settings-thub.json` into `~/.claude/settings.json`.
3. Run `claude mcp add -s user t-hub /home/natkins/.t-hub/bin/t-hub-mcp`.
4. Append `codex-config-thub.toml` to `~/.codex/config.toml`.
5. Copy the skills back into `~/.claude/skills` (and `~/.codex/skills`) and `check-worktree-empty.sh` into `~/.claude/hooks/`, then add its PreToolUse hook with `"if": "Bash(git worktree remove:*)"`.

The full pre-removal settings are also backed up on the machine at `~/.claude/backup-2026-11-02-thub/`.
