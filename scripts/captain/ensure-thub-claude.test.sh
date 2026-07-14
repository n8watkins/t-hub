#!/usr/bin/env bash
# Isolated provisioning and rollback tests for ensure-thub-claude.sh.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/ensure-thub-claude.sh"
FAILED=0
pass() { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1" >&2; FAILED=1; }

if ! command -v claude >/dev/null 2>&1; then
  echo "SKIP: claude not on PATH"
  exit 0
fi

REAL_CLAUDE="$(command -v claude)"
WORK="$(mktemp -d /tmp/thub-claude-provtest.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home"
mkdir -p "$HOME"
BIN="$WORK/t-hub-mcp"
cat > "$BIN" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = --list-tools ] && printf '[]\n' && exit 0
exit 1
EOF
chmod 700 "$BIN"

if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  pass "first run exits 0"
else
  fail "first run exited non-zero"
fi
if jq -e --arg bin "$BIN" '.mcpServers["t-hub"] == {"type":"stdio","command":$bin,"args":[],"env":{}}' "$HOME/.claude.json" >/dev/null; then
  pass "user registration has the complete expected shape"
else
  fail "user registration shape is incomplete"
fi
SNAP="$(cat "$HOME/.claude.json")"
if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1 && [ "$SNAP" = "$(cat "$HOME/.claude.json")" ]; then
  pass "repeat run is idempotent"
else
  fail "repeat run changed the registration"
fi

jq '.mcpServers["t-hub"].args=["--stale"] | .mcpServers["t-hub"].env={"BAD":"1"}' \
  "$HOME/.claude.json" > "$WORK/stale.json"
mv "$WORK/stale.json" "$HOME/.claude.json"
if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1 \
  && jq -e '.mcpServers["t-hub"].args == [] and .mcpServers["t-hub"].env == {}' "$HOME/.claude.json" >/dev/null; then
  pass "stale args and environment converge"
else
  fail "stale registration did not converge"
fi

mkdir -p "$WORK/fail-bin"
cat > "$WORK/fail-bin/claude" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = mcp ] && [ "\${2:-}" = remove ]; then exit 0; fi
if [ "\${1:-}" = mcp ] && [ "\${2:-}" = add ]; then exit 23; fi
exec "$REAL_CLAUDE" "\$@"
EOF
chmod 700 "$WORK/fail-bin/claude"
jq '.mcpServers["t-hub"].command="/stale"' "$HOME/.claude.json" > "$WORK/before-failure.json"
cp "$WORK/before-failure.json" "$HOME/.claude.json"
if PATH="$WORK/fail-bin:$PATH" T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "injected add failure unexpectedly succeeded"
elif cmp -s "$WORK/before-failure.json" "$HOME/.claude.json"; then
  pass "injected add failure restores the prior config"
else
  fail "injected add failure did not restore the prior config"
fi

[ "$FAILED" -eq 0 ] && echo "ensure-thub-claude.test: PASS" || echo "ensure-thub-claude.test: FAIL" >&2
exit "$FAILED"
