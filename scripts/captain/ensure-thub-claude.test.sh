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
STATE_DIR="$WORK/helper-state"
mkdir -m 700 "$STATE_DIR"
cat > "$BIN" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = --list-tools ] && printf '[]\n' && exit 0
exit 1
EOF
chmod 700 "$BIN"

if T_HUB_MCP_BIN="$BIN" T_HUB_INSTALL_STATE_DIR="$STATE_DIR" "$SCRIPT" >/dev/null 2>&1; then
  pass "first run exits 0"
else
  fail "first run exited non-zero"
fi
if jq -e '
  .status == "committed" and
  .before.file_presence == "absent" and
  .before.parent == {presence:false,type:"absent"} and
  .before.key == {presence:false,type:"absent",digest:"absent"} and
  .post_structure.file_presence == "present" and
  .post_structure.parent == {presence:true,type:"object"} and
  .post_structure.key.presence == true and .post_structure.key.type == "object" and
  (.post_structure.key.digest | type) == "string" and
  (.before_file | has("value") | not) and
  (.before | has("value") | not) and
  (.post_structure | has("value") | not)
' "$STATE_DIR/claude-state.json" >/dev/null \
  && [ "$(stat -c %a "$STATE_DIR")" = 700 ] \
  && [ "$(stat -c %a "$STATE_DIR/claude-state.json")" = 600 ]; then
  pass "helper publishes presence-aware Claude boundaries without node values"
else
  fail "helper Claude ownership descriptor leaks values or loses presence semantics"
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
CUSTOM_SNAP="$(cat "$HOME/.claude.json")"
if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1 \
  && [ "$CUSTOM_SNAP" = "$(cat "$HOME/.claude.json")" ]; then
  pass "matching transport preserves Claude args and environment"
else
  fail "matching transport changed Claude args or environment"
fi

jq '.mcpServers["t-hub"].command="/stale"' "$HOME/.claude.json" > "$WORK/stale-command.json"
mv "$WORK/stale-command.json" "$HOME/.claude.json"
CUSTOM_STALE_SNAP="$(cat "$HOME/.claude.json")"
if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "customized stale Claude registration was replaced"
elif [ "$CUSTOM_STALE_SNAP" = "$(cat "$HOME/.claude.json")" ]; then
  pass "customized stale Claude registration is refused unchanged"
else
  fail "customized stale Claude registration changed on refusal"
fi

jq '.mcpServers["t-hub"].args=[] | .mcpServers["t-hub"].env={}' \
  "$HOME/.claude.json" > "$WORK/uncustomized-stale.json"
mv "$WORK/uncustomized-stale.json" "$HOME/.claude.json"
if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1 \
  && jq -e --arg bin "$BIN" '.mcpServers["t-hub"].command == $bin' "$HOME/.claude.json" >/dev/null; then
  pass "uncustomized stale Claude registration converges"
else
  fail "uncustomized stale Claude registration did not converge"
fi

mkdir -p "$WORK/fail-bin"
cat > "$WORK/fail-bin/claude" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = mcp ] && [ "\${2:-}" = remove ]; then exec "$REAL_CLAUDE" "\$@"; fi
if [ "\${1:-}" = mcp ] && [ "\${2:-}" = add ]; then exit 23; fi
exec "$REAL_CLAUDE" "\$@"
EOF
chmod 700 "$WORK/fail-bin/claude"
jq '.mcpServers["t-hub"].command="/stale" | .cachedMetadata.keep="yes"' "$HOME/.claude.json" > "$WORK/before-failure.json"
cp "$WORK/before-failure.json" "$HOME/.claude.json"
if PATH="$WORK/fail-bin:$PATH" T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "injected add failure unexpectedly succeeded"
elif jq -e '
  .mcpServers["t-hub"].command == "/stale" and
  .cachedMetadata.keep == "yes"
' "$HOME/.claude.json" >/dev/null; then
  pass "remove-mutated/add-no-write failure restores prior node and metadata"
else
  fail "injected add failure did not restore the prior config"
fi

mkdir -p "$WORK/concurrent-cache-bin"
cat > "$WORK/concurrent-cache-bin/claude" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = mcp ] && [ "${2:-}" = add ]; then
  "$REAL_CLAUDE" "$@" || exit $?
  jq '.cachedMetadata.concurrent = "preserved"' "$CLAUDE_CONFIG" > "$CLAUDE_CONFIG.update"
  mv "$CLAUDE_CONFIG.update" "$CLAUDE_CONFIG"
  exit 31
fi
exec "$REAL_CLAUDE" "$@"
EOF
chmod 700 "$WORK/concurrent-cache-bin/claude"
jq '.mcpServers["t-hub"] = {"type":"stdio","command":"/prior","args":[],"env":{}}' \
  "$HOME/.claude.json" > "$WORK/before-concurrent-cache.json"
mv "$WORK/before-concurrent-cache.json" "$HOME/.claude.json"
if PATH="$WORK/concurrent-cache-bin:$PATH" REAL_CLAUDE="$REAL_CLAUDE" \
  CLAUDE_CONFIG="$HOME/.claude.json" T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "concurrent-cache add failure unexpectedly succeeded"
elif jq -e '
  .mcpServers["t-hub"].command == "/prior" and
  .cachedMetadata.concurrent == "preserved"
' "$HOME/.claude.json" >/dev/null; then
  pass "rollback restores only t-hub and preserves concurrent cached metadata"
else
  fail "rollback lost the prior t-hub node or concurrent cached metadata"
fi

mkdir -p "$WORK/concurrent-node-bin"
cat > "$WORK/concurrent-node-bin/claude" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = mcp ] && [ "${2:-}" = add ]; then
  "$REAL_CLAUDE" "$@" || exit $?
  jq '.mcpServers["t-hub"].command = "/concurrent-owner"' "$CLAUDE_CONFIG" > "$CLAUDE_CONFIG.update"
  mv "$CLAUDE_CONFIG.update" "$CLAUDE_CONFIG"
  exit 32
fi
exec "$REAL_CLAUDE" "$@"
EOF
chmod 700 "$WORK/concurrent-node-bin/claude"
jq '.mcpServers["t-hub"] = {"type":"stdio","command":"/prior","args":[],"env":{}}' \
  "$HOME/.claude.json" > "$WORK/before-concurrent-node.json"
mv "$WORK/before-concurrent-node.json" "$HOME/.claude.json"
if PATH="$WORK/concurrent-node-bin:$PATH" REAL_CLAUDE="$REAL_CLAUDE" \
  CLAUDE_CONFIG="$HOME/.claude.json" T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "concurrent-node add failure unexpectedly succeeded"
elif jq -e '.mcpServers["t-hub"].command == "/concurrent-owner"' \
  "$HOME/.claude.json" >/dev/null; then
  pass "rollback refuses to overwrite a concurrent t-hub node change"
else
  fail "rollback overwrote the concurrent t-hub node"
fi

mkdir -p "$WORK/add-write-fail-bin"
cat > "$WORK/add-write-fail-bin/claude" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = mcp ] && [ "${2:-}" = add ]; then
  "$REAL_CLAUDE" "$@" || exit $?
  exit 41
fi
exec "$REAL_CLAUDE" "$@"
EOF
chmod 700 "$WORK/add-write-fail-bin/claude"
printf '{}\n' > "$HOME/.claude.json"
if PATH="$WORK/add-write-fail-bin:$PATH" REAL_CLAUDE="$REAL_CLAUDE" \
  T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "absent-parent injected failure unexpectedly succeeded"
elif jq -e 'has("mcpServers") | not' "$HOME/.claude.json" >/dev/null; then
  pass "rollback restores absent mcpServers parent"
else
  fail "rollback did not restore absent mcpServers parent"
fi
printf '{"mcpServers":{}}\n' > "$HOME/.claude.json"
if PATH="$WORK/add-write-fail-bin:$PATH" REAL_CLAUDE="$REAL_CLAUDE" \
  T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
  fail "empty-parent injected failure unexpectedly succeeded"
elif jq -e '.mcpServers == {}' "$HOME/.claude.json" >/dev/null; then
  pass "rollback preserves an explicitly empty mcpServers parent"
else
  fail "rollback did not preserve empty mcpServers parent"
fi

for malformed in \
  '{"mcpServers":null}' \
  '{"mcpServers":false}' \
  '{"mcpServers":[]}' \
  '{"mcpServers":"bad"}' \
  '{"mcpServers":{"t-hub":null}}' \
  '{"mcpServers":{"t-hub":false}}' \
  '{"mcpServers":{"t-hub":[]}}' \
  '{"mcpServers":{"t-hub":"bad"}}'; do
  printf '%s\n' "$malformed" > "$HOME/.claude.json"
  cp "$HOME/.claude.json" "$WORK/malformed-snapshot.json"
  if T_HUB_MCP_BIN="$BIN" "$SCRIPT" >/dev/null 2>&1; then
    fail "malformed Claude structure was accepted: $malformed"
  elif ! cmp -s "$WORK/malformed-snapshot.json" "$HOME/.claude.json"; then
    fail "malformed Claude structure changed on refusal: $malformed"
  fi
done
pass "malformed parent and t-hub node structures are refused unchanged"

[ "$FAILED" -eq 0 ] && echo "ensure-thub-claude.test: PASS" || echo "ensure-thub-claude.test: FAIL" >&2
exit "$FAILED"
