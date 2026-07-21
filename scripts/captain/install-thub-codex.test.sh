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

MIGRATION_WORK="$WORK/migration"
MIGRATION_HOME="$MIGRATION_WORK/home"
MIGRATION_CODEX_HOME="$MIGRATION_WORK/codex-home"
MIGRATION_CLAUDE_HOME="$MIGRATION_WORK/claude-home"
MIGRATION_BIN_DIR="$MIGRATION_WORK/install/bin"
MIGRATION_CAPTAIN_DIR="$MIGRATION_WORK/install/captain"
mkdir -p "$MIGRATION_HOME" "$MIGRATION_CODEX_HOME" "$MIGRATION_CLAUDE_HOME"
cat > "$MIGRATION_CODEX_HOME/config.toml" <<EOF
[mcp_servers.t-hub]
command = "$MIGRATION_BIN_DIR/t-hub-mcp"
args = []
env = {}
env_vars = ["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"]

[mcp_servers.t-hub.tools.list_terminals]
approval_mode = "approve"
EOF
if HOME="$MIGRATION_HOME" CODEX_HOME="$MIGRATION_CODEX_HOME" \
  CLAUDE_HOME="$MIGRATION_CLAUDE_HOME" T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$MIGRATION_BIN_DIR" T_HUB_CAPTAIN_DIR="$MIGRATION_CAPTAIN_DIR" \
  bash "$SCRIPT" --repair-skills --migrate-legacy-registration >/dev/null 2>&1 \
  && CODEX_HOME="$MIGRATION_CODEX_HOME" codex mcp get t-hub --json | jq -e '
    .transport.env_vars == ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]
  ' >/dev/null 2>&1 \
  && grep -Fq 'approval_mode = "approve"' "$MIGRATION_CODEX_HOME/config.toml" \
  && jq -e --arg bin "$MIGRATION_BIN_DIR/t-hub-mcp" \
    '.mcpServers["t-hub"].command == $bin' "$MIGRATION_HOME/.claude.json" >/dev/null; then
  pass "installer composes skill repair with Codex-only legacy migration"
else
  fail "installer did not compose or isolate legacy migration"
fi
if HOME="$MIGRATION_HOME" CODEX_HOME="$MIGRATION_CODEX_HOME" \
  CLAUDE_HOME="$MIGRATION_CLAUDE_HOME" T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$MIGRATION_BIN_DIR" T_HUB_CAPTAIN_DIR="$MIGRATION_CAPTAIN_DIR" \
  bash "$SCRIPT" --migrate-legacy-registration --repair-skills >/dev/null 2>&1; then
  pass "installer accepts reverse migration and repair flag order"
else
  fail "installer rejected reverse migration and repair flag order"
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

ADOPTION_WORK="$WORK/adoption-races"
mkdir -p "$ADOPTION_WORK/wrapper-bin"
REAL_ATOMIC="$HERE/atomic-config.py"
cat > "$ADOPTION_WORK/wrapper-bin/atomic-config" <<'EOF'
#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
result = subprocess.run([sys.executable, os.environ["REAL_ATOMIC"], *sys.argv[1:]])
if result.returncode:
    raise SystemExit(result.returncode)
if sys.argv[1] == "publish" and pathlib.Path(sys.argv[3]).name == f'{os.environ["RACE_KIND"]}-state.json':
    config = pathlib.Path(os.environ["RACE_CONFIG"])
    if os.environ["RACE_KIND"] == "claude":
        value = json.loads(config.read_text())
        value["mcpServers"]["t-hub"]["command"] = "/concurrent-owner"
        config.write_text(json.dumps(value) + "\n")
    else:
        with config.open("a") as output:
            output.write("# concurrent-codex-owner\n")
EOF
chmod 700 "$ADOPTION_WORK/wrapper-bin/atomic-config"

CLAUDE_RACE="$ADOPTION_WORK/claude"
mkdir -p "$CLAUDE_RACE/home" "$CLAUDE_RACE/codex" "$CLAUDE_RACE/claude" "$CLAUDE_RACE/bin" "$CLAUDE_RACE/captain"
printf '{"mcpServers":{"t-hub":{"type":"stdio","command":"/prior","args":[],"env":{}}}}\n' > "$CLAUDE_RACE/home/.claude.json"
if HOME="$CLAUDE_RACE/home" CODEX_HOME="$CLAUDE_RACE/codex" CLAUDE_HOME="$CLAUDE_RACE/claude" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CLAUDE_RACE/bin" T_HUB_CAPTAIN_DIR="$CLAUDE_RACE/captain" \
  T_HUB_ATOMIC_CONFIG_HELPER="$ADOPTION_WORK/wrapper-bin/atomic-config" REAL_ATOMIC="$REAL_ATOMIC" \
  RACE_KIND=claude RACE_CONFIG="$CLAUDE_RACE/home/.claude.json" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "Claude post-helper ownership race unexpectedly succeeded"
elif jq -e '.mcpServers["t-hub"].command == "/concurrent-owner"' "$CLAUDE_RACE/home/.claude.json" >/dev/null; then
  pass "Claude adoption mismatch preserves the concurrent t-hub owner"
else
  fail "Claude adoption mismatch overwrote the concurrent t-hub owner"
fi

CODEX_RACE="$ADOPTION_WORK/codex-race"
mkdir -p "$CODEX_RACE/home" "$CODEX_RACE/codex" "$CODEX_RACE/claude" "$CODEX_RACE/bin" "$CODEX_RACE/captain"
cat > "$CODEX_RACE/codex/config.toml" <<EOF
[mcp_servers.t-hub]
command = "$CODEX_RACE/bin/t-hub-mcp"
args = []
env = {}
env_vars = ["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"]
EOF
if HOME="$CODEX_RACE/home" CODEX_HOME="$CODEX_RACE/codex" CLAUDE_HOME="$CODEX_RACE/claude" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CODEX_RACE/bin" T_HUB_CAPTAIN_DIR="$CODEX_RACE/captain" \
  T_HUB_ATOMIC_CONFIG_HELPER="$ADOPTION_WORK/wrapper-bin/atomic-config" REAL_ATOMIC="$REAL_ATOMIC" \
  RACE_KIND=codex RACE_CONFIG="$CODEX_RACE/codex/config.toml" \
  bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1; then
  fail "Codex post-helper ownership race unexpectedly succeeded"
elif grep -Fq 'env_vars = ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]' "$CODEX_RACE/codex/config.toml" \
  && grep -Fq '# concurrent-codex-owner' "$CODEX_RACE/codex/config.toml"; then
  pass "Codex adoption mismatch preserves the concurrent unrelated edit"
else
  fail "Codex adoption mismatch overwrote the concurrent edit"
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
  "$POST_SKILL_BIN_DIR" "$POST_SKILL_CAPTAIN_DIR" "$POST_SKILL_WORK/wrapper-bin"
printf 'old binary\n' > "$POST_SKILL_BIN_DIR/t-hub-mcp"
cat > "$POST_SKILL_CODEX_HOME/config.toml" <<EOF
[mcp_servers.t-hub]
command = "$POST_SKILL_BIN_DIR/t-hub-mcp"
args = []
env = {}
env_vars = ["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"]

[mcp_servers.t-hub.tools.list_terminals]
approval_mode = "approve"
EOF
cp -p "$POST_SKILL_CODEX_HOME/config.toml" "$POST_SKILL_WORK/codex-before.toml"
REAL_CODEX="$(command -v codex)"
cat > "$POST_SKILL_WORK/wrapper-bin/codex" <<'EOF'
#!/usr/bin/env bash
if [ ! -f "$CONCURRENT_CACHE_ONCE" ]; then
  : > "$CONCURRENT_CACHE_ONCE"
  jq '.cachedMetadata.concurrent = "preserved"' "$CLAUDE_CONFIG" > "$CLAUDE_CONFIG.update"
  mv "$CLAUDE_CONFIG.update" "$CLAUDE_CONFIG"
fi
exec "$REAL_CODEX" "$@"
EOF
chmod 700 "$POST_SKILL_WORK/wrapper-bin/codex"

if HOME="$POST_SKILL_HOME" \
  CODEX_HOME="$POST_SKILL_CODEX_HOME" \
  CLAUDE_HOME="$POST_SKILL_CLAUDE_HOME" \
  PATH="$POST_SKILL_WORK/wrapper-bin:$PATH" \
  REAL_CODEX="$REAL_CODEX" \
  CLAUDE_CONFIG="$POST_SKILL_HOME/.claude.json" \
  CONCURRENT_CACHE_ONCE="$POST_SKILL_WORK/concurrent-cache-once" \
  T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$POST_SKILL_BIN_DIR" \
  T_HUB_CAPTAIN_DIR="$POST_SKILL_CAPTAIN_DIR" \
  T_HUB_SKILL_FAIL_AFTER_INSTALL=1 \
  bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1; then
  fail "injected post-skill failure unexpectedly succeeded"
else
  pass "injected post-skill failure is reported"
fi
if [ "$(cat "$POST_SKILL_BIN_DIR/t-hub-mcp")" = "old binary" ] \
  && cmp -s "$POST_SKILL_WORK/codex-before.toml" "$POST_SKILL_CODEX_HOME/config.toml" \
  && jq -e '
    .mcpServers["t-hub"] == null and
    .cachedMetadata.concurrent == "preserved"
  ' "$POST_SKILL_HOME/.claude.json" >/dev/null \
  && [ ! -e "$POST_SKILL_CODEX_HOME/skills/captain" ] \
  && [ ! -e "$POST_SKILL_CLAUDE_HOME/skills/captain" ] \
  && [ ! -e "$POST_SKILL_CLAUDE_HOME/commands/handoff.md" ]; then
  pass "stage failure restores legacy Codex and t-hub node while preserving Claude cache"
else
  fail "post-skill failure left a partial installation"
fi

if [ "$FAILED" -eq 0 ]; then
  echo "install-thub-codex.test: PASS"
else
  echo "install-thub-codex.test: FAIL" >&2
fi
exit "$FAILED"
