#!/usr/bin/env bash
# Isolated install test using a fake MCP binary and a throwaway Codex config.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/install-thub-codex.sh"
FAILED=0

pass() { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1" >&2; FAILED=1; }

if ! command -v codex >/dev/null 2>&1; then
  echo "SKIP: codex not on PATH"
  exit 0
fi

WORK="$(mktemp -d "${HOME}/.thub-codex-installtest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home"
mkdir -p "$HOME"
export CODEX_HOME="$WORK/codex-home"
mkdir -p "$CODEX_HOME"
export CLAUDE_HOME="$WORK/claude-home"
mkdir -p "$CLAUDE_HOME"

SOURCE="$WORK/source-t-hub-mcp"
cat > "$SOURCE" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--list-tools" ]; then
  printf '[{"name":"list_terminals"},{"name":"claim_captain"}]\n'
  exit 0
fi
exit 1
EOF
chmod 700 "$SOURCE"

BIN_DIR="$WORK/install/bin"
CAPTAIN_DIR="$WORK/install/captain"

if T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BIN_DIR" T_HUB_CAPTAIN_DIR="$CAPTAIN_DIR" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "isolated install exits 0"
else
  fail "isolated install exited non-zero"
fi

if [ -x "$BIN_DIR/t-hub-mcp" ]; then
  pass "installed MCP binary is executable"
else
  fail "installed MCP binary is missing or not executable"
fi

if [ -x "$CAPTAIN_DIR/ensure-thub-codex.sh" ]; then
  pass "deployed provisioner is executable"
else
  fail "deployed provisioner is missing or not executable"
fi
if [ -x "$CAPTAIN_DIR/ensure-thub-claude.sh" ]; then
  pass "deployed Claude provisioner is executable"
else
  fail "deployed Claude provisioner is missing or not executable"
fi

for skill in \
  "$CODEX_HOME/skills/captain/SKILL.md" \
  "$CODEX_HOME/skills/shipmate/SKILL.md" \
  "$CODEX_HOME/skills/handoff/SKILL.md" \
  "$CLAUDE_HOME/skills/captain/SKILL.md" \
  "$CLAUDE_HOME/skills/shipmate/SKILL.md" \
  "$CLAUDE_HOME/skills/handoff/SKILL.md"; do
  if [ -f "$skill" ]; then
    pass "installed skill ${skill#"$WORK/"}"
  else
    fail "missing installed skill ${skill#"$WORK/"}"
  fi
done

if [ -f "$CLAUDE_HOME/commands/handoff.md" ] \
  && [ "$(head -n 1 "$CLAUDE_HOME/commands/handoff.md")" = '---' ] \
  && [ -f "$CLAUDE_HOME/commands/handoff.md.t-hub-managed" ]; then
  pass "installed managed Claude handoff command"
else
  fail "missing or unmanaged Claude handoff command"
fi

if codex mcp get t-hub --json 2>/dev/null | grep -Fq "\"command\": \"$BIN_DIR/t-hub-mcp\""; then
  pass "Codex registration points at the installed binary"
else
  fail "Codex registration does not point at the installed binary"
fi
if jq -e --arg bin "$BIN_DIR/t-hub-mcp" '.mcpServers["t-hub"].command == $bin' "$HOME/.claude.json" >/dev/null; then
  pass "Claude user registration points at the installed binary"
else
  fail "Claude user registration does not point at the installed binary"
fi

if T_HUB_CODEX_SKILLS_DIR="$CODEX_HOME/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$CLAUDE_HOME/skills" \
  bash "$HERE/install-captain-skills.sh" --verify >/dev/null; then
  pass "installed skill hashes match source"
else
  fail "installed skill hash verification failed"
fi

printf '\nlocal modification\n' >> "$CODEX_HOME/skills/captain/SKILL.md"
if CODEX_HOME="$CODEX_HOME" T_HUB_HARNESS=codex \
  "$CODEX_HOME/skills/captain/scripts/check_environment.sh" \
  | grep -Fq 'skill_integrity=drifted'; then
  pass "Captain environment check detects installed skill drift"
else
  fail "Captain environment check missed installed skill drift"
fi

if T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BIN_DIR" T_HUB_CAPTAIN_DIR="$CAPTAIN_DIR" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "repeat install unexpectedly erased skill drift"
elif grep -Fq 'local modification' "$CODEX_HOME/skills/captain/SKILL.md"; then
  pass "repeat install refuses and preserves skill drift"
else
  fail "drift refusal did not preserve the modified skill"
fi
if T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BIN_DIR" T_HUB_CAPTAIN_DIR="$CAPTAIN_DIR" bash "$SCRIPT" --repair-skills >/dev/null 2>&1 \
  && ! grep -Fq 'local modification' "$CODEX_HOME/skills/captain/SKILL.md"; then
  pass "explicit repair replaces skill drift"
else
  fail "explicit skill repair failed"
fi

chmod 600 "$CODEX_HOME/skills/captain/scripts/check_environment.sh"
if T_HUB_CODEX_SKILLS_DIR="$CODEX_HOME/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$CLAUDE_HOME/skills" \
  bash "$HERE/install-captain-skills.sh" --verify >/dev/null 2>&1; then
  fail "integrity verification missed executable-mode drift"
else
  pass "integrity verification detects executable-mode drift"
fi
T_HUB_CODEX_SKILLS_DIR="$CODEX_HOME/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$CLAUDE_HOME/skills" \
  bash "$HERE/install-captain-skills.sh" --repair >/dev/null 2>&1
ln -s /tmp/untrusted "$CODEX_HOME/skills/captain/unexpected-link"
if T_HUB_CODEX_SKILLS_DIR="$CODEX_HOME/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$CLAUDE_HOME/skills" \
  bash "$HERE/install-captain-skills.sh" --verify >/dev/null 2>&1; then
  fail "integrity verification missed added symlink drift"
else
  pass "integrity verification detects added symlink drift"
fi
T_HUB_CODEX_SKILLS_DIR="$CODEX_HOME/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$CLAUDE_HOME/skills" \
  bash "$HERE/install-captain-skills.sh" --repair >/dev/null 2>&1

CONFLICT_WORK="$WORK/conflict"
CONFLICT_CODEX_HOME="$CONFLICT_WORK/codex-home"
CONFLICT_CLAUDE_HOME="$CONFLICT_WORK/claude-home"
CONFLICT_BIN_DIR="$CONFLICT_WORK/install/bin"
CONFLICT_CAPTAIN_DIR="$CONFLICT_WORK/install/captain"
mkdir -p "$CONFLICT_CODEX_HOME" "$CONFLICT_CLAUDE_HOME/skills/shipmate"
printf 'user-owned\n' > "$CONFLICT_CLAUDE_HOME/skills/shipmate/SKILL.md"

if CODEX_HOME="$CONFLICT_CODEX_HOME" \
  CLAUDE_HOME="$CONFLICT_CLAUDE_HOME" \
  T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$CONFLICT_BIN_DIR" \
  T_HUB_CAPTAIN_DIR="$CONFLICT_CAPTAIN_DIR" \
  bash "$SCRIPT" >/dev/null 2>&1; then
  fail "unmanaged late-target conflict unexpectedly succeeded"
else
  pass "unmanaged late-target conflict is refused"
fi

if [ ! -e "$CONFLICT_BIN_DIR/t-hub-mcp" ] \
  && [ ! -e "$CONFLICT_CODEX_HOME/skills/captain" ] \
  && [ ! -e "$CONFLICT_CODEX_HOME/skills/shipmate" ] \
  && [ ! -e "$CONFLICT_CODEX_HOME/skills/handoff" ] \
  && [ ! -e "$CONFLICT_CLAUDE_HOME/skills/captain" ] \
  && [ ! -e "$CONFLICT_CLAUDE_HOME/skills/handoff" ] \
  && [ ! -e "$CONFLICT_CLAUDE_HOME/commands/handoff.md" ] \
  && [ "$(cat "$CONFLICT_CLAUDE_HOME/skills/shipmate/SKILL.md")" = "user-owned" ]; then
  pass "conflict leaves the binary and every skill target unchanged"
else
  fail "conflict left a partial installation"
fi

COMMAND_CONFLICT_WORK="$WORK/command-conflict"
COMMAND_CONFLICT_CODEX_HOME="$COMMAND_CONFLICT_WORK/codex-home"
COMMAND_CONFLICT_CLAUDE_HOME="$COMMAND_CONFLICT_WORK/claude-home"
COMMAND_CONFLICT_BIN_DIR="$COMMAND_CONFLICT_WORK/install/bin"
COMMAND_CONFLICT_CAPTAIN_DIR="$COMMAND_CONFLICT_WORK/install/captain"
mkdir -p "$COMMAND_CONFLICT_CODEX_HOME" "$COMMAND_CONFLICT_CLAUDE_HOME/commands"
printf 'user-owned command\n' > "$COMMAND_CONFLICT_CLAUDE_HOME/commands/handoff.md"

if CODEX_HOME="$COMMAND_CONFLICT_CODEX_HOME" \
  CLAUDE_HOME="$COMMAND_CONFLICT_CLAUDE_HOME" \
  T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$COMMAND_CONFLICT_BIN_DIR" \
  T_HUB_CAPTAIN_DIR="$COMMAND_CONFLICT_CAPTAIN_DIR" \
  bash "$SCRIPT" >/dev/null 2>&1; then
  fail "unmanaged Claude command conflict unexpectedly succeeded"
else
  pass "unmanaged Claude command conflict is refused"
fi

if [ ! -e "$COMMAND_CONFLICT_BIN_DIR/t-hub-mcp" ] \
  && [ ! -e "$COMMAND_CONFLICT_CODEX_HOME/skills/captain" ] \
  && [ ! -e "$COMMAND_CONFLICT_CODEX_HOME/skills/shipmate" ] \
  && [ ! -e "$COMMAND_CONFLICT_CODEX_HOME/skills/handoff" ] \
  && [ ! -e "$COMMAND_CONFLICT_CLAUDE_HOME/skills/captain" ] \
  && [ ! -e "$COMMAND_CONFLICT_CLAUDE_HOME/skills/shipmate" ] \
  && [ ! -e "$COMMAND_CONFLICT_CLAUDE_HOME/skills/handoff" ] \
  && [ "$(cat "$COMMAND_CONFLICT_CLAUDE_HOME/commands/handoff.md")" = "user-owned command" ]; then
  pass "command conflict leaves every managed target unchanged"
else
  fail "command conflict left a partial installation"
fi

if CODEX_HOME="$CONFLICT_CODEX_HOME" codex mcp get t-hub >/dev/null 2>&1; then
  fail "conflict registered an MCP server"
else
  pass "conflict leaves Codex registration absent"
fi

ROLLBACK_WORK="$WORK/rollback"
ROLLBACK_HOME="$ROLLBACK_WORK/home"
ROLLBACK_CODEX_HOME="$ROLLBACK_WORK/codex-home"
ROLLBACK_CLAUDE_HOME="$ROLLBACK_WORK/claude-home"
ROLLBACK_BIN_DIR="$ROLLBACK_WORK/install/bin"
ROLLBACK_CAPTAIN_DIR="$ROLLBACK_WORK/install/captain"
mkdir -p "$ROLLBACK_HOME" "$ROLLBACK_CODEX_HOME" "$ROLLBACK_CLAUDE_HOME" \
  "$ROLLBACK_BIN_DIR" "$ROLLBACK_CAPTAIN_DIR" "$ROLLBACK_WORK/fail-bin"
printf 'old binary\n' > "$ROLLBACK_BIN_DIR/t-hub-mcp"
printf 'old codex config\n' > "$ROLLBACK_CODEX_HOME/config.toml"
printf '{"oldClaudeConfig":true}\n' > "$ROLLBACK_HOME/.claude.json"
cat > "$ROLLBACK_WORK/fail-bin/claude" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = mcp ] && [ "${2:-}" = remove ]; then exit 0; fi
if [ "${1:-}" = mcp ] && [ "${2:-}" = add ]; then exit 29; fi
exit 1
EOF
chmod 700 "$ROLLBACK_WORK/fail-bin/claude"

if HOME="$ROLLBACK_HOME" \
  CODEX_HOME="$ROLLBACK_CODEX_HOME" \
  CLAUDE_HOME="$ROLLBACK_CLAUDE_HOME" \
  PATH="$ROLLBACK_WORK/fail-bin:$PATH" \
  T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$ROLLBACK_BIN_DIR" \
  T_HUB_CAPTAIN_DIR="$ROLLBACK_CAPTAIN_DIR" \
  bash "$SCRIPT" >/dev/null 2>&1; then
  fail "injected Claude registration failure unexpectedly succeeded"
else
  pass "injected Claude registration failure is reported"
fi

if [ "$(cat "$ROLLBACK_BIN_DIR/t-hub-mcp")" = "old binary" ] \
  && [ "$(cat "$ROLLBACK_CODEX_HOME/config.toml")" = "old codex config" ] \
  && [ "$(cat "$ROLLBACK_HOME/.claude.json")" = '{"oldClaudeConfig":true}' ] \
  && [ ! -e "$ROLLBACK_CAPTAIN_DIR/ensure-thub-codex.sh" ] \
  && [ ! -e "$ROLLBACK_CAPTAIN_DIR/ensure-thub-claude.sh" ] \
  && [ ! -e "$ROLLBACK_CODEX_HOME/skills" ] \
  && [ ! -e "$ROLLBACK_CLAUDE_HOME/skills" ]; then
  pass "top-level rollback restores binary, helpers, configs, and skills"
else
  fail "top-level rollback left a partial installation"
fi

POST_SKILL_WORK="$WORK/post-skill-rollback"
POST_SKILL_HOME="$POST_SKILL_WORK/home"
POST_SKILL_CODEX_HOME="$POST_SKILL_WORK/codex-home"
POST_SKILL_CLAUDE_HOME="$POST_SKILL_WORK/claude-home"
POST_SKILL_BIN_DIR="$POST_SKILL_WORK/install/bin"
POST_SKILL_CAPTAIN_DIR="$POST_SKILL_WORK/install/captain"
mkdir -p "$POST_SKILL_HOME" "$POST_SKILL_CODEX_HOME" "$POST_SKILL_CLAUDE_HOME" \
  "$POST_SKILL_BIN_DIR" "$POST_SKILL_CAPTAIN_DIR"
printf 'old binary\n' > "$POST_SKILL_BIN_DIR/t-hub-mcp"

if HOME="$POST_SKILL_HOME" \
  CODEX_HOME="$POST_SKILL_CODEX_HOME" \
  CLAUDE_HOME="$POST_SKILL_CLAUDE_HOME" \
  T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$POST_SKILL_BIN_DIR" \
  T_HUB_CAPTAIN_DIR="$POST_SKILL_CAPTAIN_DIR" \
  T_HUB_SKILL_FAIL_AFTER_INSTALL=1 \
  bash "$SCRIPT" >/dev/null 2>&1; then
  fail "injected post-skill failure unexpectedly succeeded"
else
  pass "injected post-skill failure is reported"
fi
if [ "$(cat "$POST_SKILL_BIN_DIR/t-hub-mcp")" = "old binary" ] \
  && [ ! -e "$POST_SKILL_CODEX_HOME/config.toml" ] \
  && [ ! -e "$POST_SKILL_HOME/.claude.json" ] \
  && [ ! -e "$POST_SKILL_CODEX_HOME/skills/captain" ] \
  && [ ! -e "$POST_SKILL_CLAUDE_HOME/skills/captain" ] \
  && [ ! -e "$POST_SKILL_CLAUDE_HOME/commands/handoff.md" ]; then
  pass "post-skill failure rolls back binary, configs, skills, and command"
else
  fail "post-skill failure left a partial installation"
fi

if [ "$FAILED" -eq 0 ]; then
  echo "install-thub-codex.test: PASS"
else
  echo "install-thub-codex.test: FAIL" >&2
fi
exit "$FAILED"
