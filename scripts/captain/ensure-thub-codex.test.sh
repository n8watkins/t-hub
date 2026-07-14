#!/usr/bin/env bash
# Provisioning test for ensure-thub-codex.sh (plan test bar §1.4).
#
# Runs the provisioner against a throwaway CODEX_HOME pre-seeded with a user
# [hooks] block and asserts:
#   1. [mcp_servers.t-hub] is added.
#   2. the pre-seeded [hooks] / [hooks.state] block is byte-preserved (MED-3).
#   3. a re-run is idempotent (config.toml unchanged, exit 0).
#   4. a stale t-hub command converges to the requested binary.
#
# Requires codex on PATH (the merge is codex-native); SKIPs cleanly if absent so
# it never fails a host without codex-cli. Run: bash ensure-thub-codex.test.sh
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/ensure-thub-codex.sh"
FAILED=0

pass() { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1" >&2; FAILED=1; }

if ! command -v codex >/dev/null 2>&1; then
  echo "SKIP: codex not on PATH (the merge is codex-native, nothing to test here)"
  exit 0
fi

# Isolated, non-/tmp CODEX_HOME (codex refuses to create PATH aliases under /tmp;
# a HOME-rooted dir avoids that noise and better mirrors the real ~/.codex).
WORK="$(mktemp -d "${HOME}/.thub-codex-provtest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
export CODEX_HOME="$WORK"
FAKE_BIN="$WORK/t-hub-mcp"

cat > "$FAKE_BIN" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--list-tools" ]; then
  printf '[{"name":"list_terminals"}]\n'
  exit 0
fi
exit 1
EOF
chmod 700 "$FAKE_BIN"

# Pre-seed a user config with [hooks] + [hooks.state] trust blocks (the clobber
# risk the provisioner must not touch).
cat > "$WORK/config.toml" <<'EOF'
# user-authored config
model = "gpt-5-codex"

[hooks]
[hooks.state]
"normalize" = { trusted = true }

[hooks.normalize]
command = ["echo", "normalize"]
EOF
HOOKS_BEFORE="$(sed -n '/^\[hooks\]/,/^\[mcp_servers/p' "$WORK/config.toml" | grep -v '^\[mcp_servers')"

# --- 1. first run registers the server --------------------------------------
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "first run exits 0"
else
  fail "first run exited non-zero"
fi

if grep -q '^\[mcp_servers.t-hub\]' "$WORK/config.toml"; then
  pass "[mcp_servers.t-hub] added"
else
  fail "[mcp_servers.t-hub] NOT added"
fi

if codex mcp get t-hub >/dev/null 2>&1; then
  pass "codex mcp get t-hub resolves"
else
  fail "codex mcp get t-hub does not resolve"
fi

# --- 2. hooks block byte-preserved ------------------------------------------
HOOKS_AFTER="$(sed -n '/^\[hooks\]/,/^\[mcp_servers/p' "$WORK/config.toml" | grep -v '^\[mcp_servers')"
if [ "$HOOKS_BEFORE" = "$HOOKS_AFTER" ]; then
  pass "[hooks]/[hooks.state] block byte-preserved"
else
  fail "hooks block changed by provisioning"
  echo "--- before ---"; echo "$HOOKS_BEFORE"
  echo "--- after ----"; echo "$HOOKS_AFTER"
fi

# --- 3. idempotent re-run ---------------------------------------------------
SNAP="$(cat "$WORK/config.toml")"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "re-run exits 0"
else
  fail "re-run exited non-zero"
fi
if [ "$SNAP" = "$(cat "$WORK/config.toml")" ]; then
  pass "re-run left config.toml unchanged (idempotent)"
else
  fail "re-run mutated config.toml"
fi

# --- 4. existing policy is preserved and blocks unsafe repointing ------------
STALE_BIN="$WORK/stale-t-hub-mcp"
cp "$FAKE_BIN" "$STALE_BIN"
sed -i '/^\[mcp_servers.t-hub\]/a enabled_tools = ["list_terminals"]\ntool_timeout_sec = 17' "$WORK/config.toml"
POLICY_SNAP="$(cat "$WORK/config.toml")"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1 \
  && [ "$POLICY_SNAP" = "$(cat "$WORK/config.toml")" ]; then
  pass "matching transport preserves Codex tool and timeout policy"
else
  fail "matching transport changed Codex policy"
fi
sed -i "s#command = \"$FAKE_BIN\"#command = \"$STALE_BIN\"#" "$WORK/config.toml"
STALE_POLICY_SNAP="$(cat "$WORK/config.toml")"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "customized stale registration was replaced"
elif [ "$STALE_POLICY_SNAP" = "$(cat "$WORK/config.toml")" ]; then
  pass "customized stale registration is refused unchanged"
else
  fail "customized stale registration changed on refusal"
fi

# --- 5. uncustomized stale registration converges ---------------------------
sed -i '/^enabled_tools = /d; /^tool_timeout_sec = /d' "$WORK/config.toml"

if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "stale registration update exits 0"
else
  fail "stale registration update exited non-zero"
fi

if codex mcp get t-hub --json | grep -Fq "\"command\": \"$FAKE_BIN\""; then
  pass "stale registration converged to requested binary"
else
  fail "stale registration did not converge"
fi
if codex mcp get t-hub --json | jq -e '
    .enabled == true and .disabled_reason == null and .transport.args == [] and
  (.transport.env == null or .transport.env == {}) and
  .transport.env_vars == [] and .transport.cwd == null and
  .enabled_tools == null and .disabled_tools == null and
  .startup_timeout_sec == null and .tool_timeout_sec == null
' >/dev/null; then
  pass "stale args, env, disabled fields, cwd, tools, and timeouts are absent"
else
  fail "complete registration shape did not converge"
fi

if [ "$FAILED" -eq 0 ]; then
  echo "ensure-thub-codex.test: PASS"
else
  echo "ensure-thub-codex.test: FAIL" >&2
fi
exit "$FAILED"
