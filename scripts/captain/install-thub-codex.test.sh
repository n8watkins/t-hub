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

for skill in \
  "$CODEX_HOME/skills/captain/SKILL.md" \
  "$CODEX_HOME/skills/shipmate/SKILL.md" \
  "$CLAUDE_HOME/skills/captain/SKILL.md" \
  "$CLAUDE_HOME/skills/shipmate/SKILL.md"; do
  if [ -f "$skill" ]; then
    pass "installed skill ${skill#"$WORK/"}"
  else
    fail "missing installed skill ${skill#"$WORK/"}"
  fi
done

if codex mcp get t-hub --json 2>/dev/null | grep -Fq "\"command\": \"$BIN_DIR/t-hub-mcp\""; then
  pass "Codex registration points at the installed binary"
else
  fail "Codex registration does not point at the installed binary"
fi

if T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BIN_DIR" T_HUB_CAPTAIN_DIR="$CAPTAIN_DIR" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "repeat install exits 0"
else
  fail "repeat install exited non-zero"
fi

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
  && [ ! -e "$CONFLICT_CLAUDE_HOME/skills/captain" ] \
  && [ "$(cat "$CONFLICT_CLAUDE_HOME/skills/shipmate/SKILL.md")" = "user-owned" ]; then
  pass "conflict leaves the binary and every skill target unchanged"
else
  fail "conflict left a partial installation"
fi

if CODEX_HOME="$CONFLICT_CODEX_HOME" codex mcp get t-hub >/dev/null 2>&1; then
  fail "conflict registered an MCP server"
else
  pass "conflict leaves Codex registration absent"
fi

if [ "$FAILED" -eq 0 ]; then
  echo "install-thub-codex.test: PASS"
else
  echo "install-thub-codex.test: FAIL" >&2
fi
exit "$FAILED"
