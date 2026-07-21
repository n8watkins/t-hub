#!/usr/bin/env bash
# Provisioning test for ensure-thub-codex.sh (plan test bar §1.4).
#
# Runs the provisioner against a throwaway CODEX_HOME pre-seeded with a user
# [hooks] block and asserts:
#   1. [mcp_servers.t-hub] is added.
#   2. the pre-seeded [hooks] / [hooks.state] block is byte-preserved (MED-3).
#   3. a re-run is idempotent (config.toml unchanged, exit 0).
#   4. only the two capability variable names are inherited, never values.
#   5. an exact managed legacy empty-env registration migrates.
#   6. a stale t-hub command converges to the requested binary.
#   7. verification failure restores the original config bytes exactly.
#   8. same-command hidden policy is preserved during migration, while stale
#      hidden policy, disabled registrations, concurrent writes, and symlinked
#      configs are refused without destroying user state.
#
# Requires codex on PATH (the merge is codex-native); SKIPs cleanly if absent so
# it never fails a host without codex-cli. Run: bash ensure-thub-codex.test.sh
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/ensure-thub-codex.sh"
FAILED=0
EXPECTED_ENV_VARS='["T_HUB_CONTROL_FILE","T_HUB_SESSION_TOKEN"]'

pass() { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1" >&2; FAILED=1; }

if ! command -v codex >/dev/null 2>&1; then
  echo "SKIP: codex not on PATH (the merge is codex-native, nothing to test here)"
  exit 0
fi
REAL_CODEX="$(command -v codex)"

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
export T_HUB_CONTROL_ADDR="sentinel-address-one"
export T_HUB_CONTROL_TOKEN="sentinel-control-one"
export T_HUB_SESSION_TOKEN="sentinel-session-one"
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

if codex mcp get t-hub --json | jq -e --argjson expected "$EXPECTED_ENV_VARS" '
  .transport.env_vars == $expected and
  (.transport.env == null or .transport.env == {})
' >/dev/null; then
  pass "registration inherits the two stable T-Hub identity variables"
else
  fail "registration does not inherit the two stable T-Hub identity variables"
fi
if grep -Fq 'sentinel-' "$WORK/config.toml"; then
  fail "registration persisted a capability variable value"
else
  pass "registration persists no capability variable values"
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
export T_HUB_CONTROL_ADDR="sentinel-address-two"
export T_HUB_CONTROL_TOKEN="sentinel-control-two"
export T_HUB_SESSION_TOKEN="sentinel-session-two"
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
if grep -Fq 'sentinel-' "$WORK/config.toml"; then
  fail "rotated capability values leaked into config.toml"
else
  pass "rotating capability values is byte-idempotent and secret-free"
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
  .transport.env_vars == [
    "T_HUB_CONTROL_FILE",
    "T_HUB_SESSION_TOKEN"
  ] and .transport.cwd == null and
  .enabled_tools == null and .disabled_tools == null and
  .startup_timeout_sec == null and .tool_timeout_sec == null
' >/dev/null; then
  pass "stale args, env, disabled fields, cwd, tools, and timeouts are absent"
else
  fail "complete registration shape did not converge"
fi

# --- 6. exact legacy registration requires explicit migration ---------------
codex mcp remove t-hub >/dev/null
codex mcp add t-hub -- "$FAKE_BIN" >/dev/null
sed -i "\|^command = \"$FAKE_BIN\"$|a env = {}\nenv_vars = [\"T_HUB_CONTROL_ADDR\", \"T_HUB_CONTROL_TOKEN\", \"T_HUB_SESSION_TOKEN\"]" "$WORK/config.toml"
cat >> "$WORK/config.toml" <<'EOF'

[mcp_servers.t-hub.tools.list_terminals]
approval_mode = "approve"
EOF
LEGACY_SNAP="$WORK/config.legacy-snapshot.toml"
LEGACY_EXPECTED="$WORK/config.legacy-expected.toml"
cp -p "$WORK/config.toml" "$LEGACY_SNAP"
cp -p "$WORK/config.toml" "$LEGACY_EXPECTED"
sed -i 's/env_vars = \["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"\]/env_vars = ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]/' "$LEGACY_EXPECTED"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "legacy registration migrated without explicit authorization"
elif cmp -s "$LEGACY_SNAP" "$WORK/config.toml"; then
  pass "legacy registration is refused byte-for-byte without migration flag"
else
  fail "legacy registration changed during unflagged refusal"
fi
export T_HUB_CONTROL_ADDR="legacy-secret-address"
export T_HUB_CONTROL_TOKEN="legacy-secret-control"
export T_HUB_SESSION_TOKEN="legacy-secret-session"
MIGRATION_WRAPPER_DIR="$WORK/migration-wrapper-bin"
mkdir "$MIGRATION_WRAPPER_DIR"
cat > "$MIGRATION_WRAPPER_DIR/codex" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = mcp ] && { [ "${2:-}" = add ] || [ "${2:-}" = remove ]; }; then
  exit 97
fi
exec "$REAL_CODEX" "$@"
EOF
chmod 700 "$MIGRATION_WRAPPER_DIR/codex"
if PATH="$MIGRATION_WRAPPER_DIR:$PATH" REAL_CODEX="$REAL_CODEX" \
  T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1 \
  && cmp -s "$LEGACY_EXPECTED" "$WORK/config.toml"; then
  pass "explicit migration changes only env_vars without Codex add/remove"
else
  fail "explicit migration changed unrelated TOML or invoked Codex add/remove"
fi
if codex mcp get t-hub --json | jq -e --argjson expected "$EXPECTED_ENV_VARS" '
  .transport.env_vars == $expected and
  .transport.env == {} and .transport.args == [] and .transport.cwd == null
' >/dev/null; then
  pass "explicit legacy migration has the canonical JSON transport"
else
  fail "explicit legacy migration JSON transport is not canonical"
fi
if grep -Fq 'approval_mode = "approve"' "$WORK/config.toml" \
  && ! grep -Fq 'legacy-secret-' "$WORK/config.toml"; then
  pass "legacy migration preserves nested policy and persists no secrets"
else
  fail "legacy migration changed nested policy or persisted a secret"
fi
MIGRATED_SNAP="$(cat "$WORK/config.toml")"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1 \
  && [ "$MIGRATED_SNAP" = "$(cat "$WORK/config.toml")" ]; then
  pass "explicit migration rerun is idempotent"
else
  fail "explicit migration rerun changed config.toml"
fi

# --- 7. verification failure restores exact original bytes ------------------
sed -i 's/env_vars = \["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"\]/env_vars = ["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"]/' "$WORK/config.toml"
ROLLBACK_SNAP="$WORK/config.rollback-snapshot.toml"
cp -p "$WORK/config.toml" "$ROLLBACK_SNAP"
WRAPPER_DIR="$WORK/wrapper-bin"
COUNT_FILE="$WORK/get-count"
mkdir "$WRAPPER_DIR"
cat > "$WRAPPER_DIR/codex" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "get" ] && [ "${3:-}" = "t-hub" ] && [ "${4:-}" = "--json" ]; then
  count=0
  [ ! -f "$CODEX_GET_COUNT_FILE" ] || count="$(cat "$CODEX_GET_COUNT_FILE")"
  count=$((count + 1))
  printf '%s\n' "$count" > "$CODEX_GET_COUNT_FILE"
  if [ "$count" -ge 2 ]; then
    printf '{}\n'
    exit 0
  fi
fi
exec "$REAL_CODEX" "$@"
EOF
chmod 700 "$WRAPPER_DIR/codex"
if PATH="$WRAPPER_DIR:$PATH" REAL_CODEX="$REAL_CODEX" CODEX_GET_COUNT_FILE="$COUNT_FILE" \
  T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1; then
  fail "injected verification failure exited 0"
elif cmp -s "$ROLLBACK_SNAP" "$WORK/config.toml"; then
  pass "verification failure restores config.toml byte-for-byte"
else
  fail "verification failure did not restore exact original bytes"
fi

# --- 8. hidden Codex policy prevents replacement ----------------------------
codex mcp remove t-hub >/dev/null
codex mcp add t-hub -- "$STALE_BIN" >/dev/null
sed -i "\|^command = \"$STALE_BIN\"$|a required = true\nsupports_parallel_tool_calls = true" "$WORK/config.toml"
HIDDEN_POLICY_SNAP="$WORK/config.hidden-policy-snapshot.toml"
cp -p "$WORK/config.toml" "$HIDDEN_POLICY_SNAP"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "stale registration with hidden Codex policy was replaced"
elif cmp -s "$HIDDEN_POLICY_SNAP" "$WORK/config.toml"; then
  pass "hidden Codex policy is refused byte-for-byte"
else
  fail "hidden Codex policy changed on refusal"
fi
sed -i '/^required = true$/d; /^supports_parallel_tool_calls = true$/d' "$WORK/config.toml"
codex mcp remove t-hub >/dev/null
NESTED_POLICY_BASE="$WORK/config.nested-policy-base.toml"
cp -p "$WORK/config.toml" "$NESTED_POLICY_BASE"
codex mcp add t-hub -- "$FAKE_BIN" >/dev/null
sed -i "\|^command = \"$FAKE_BIN\"$|a required = true\nsupports_parallel_tool_calls = true" "$WORK/config.toml"
cat >> "$WORK/config.toml" <<'EOF'

[mcp_servers.t-hub.tools.my_capability]
approval_mode = "approve"

[mcp_servers.t-hub.tools.list_terminals]
approval_mode = "approve"

[mcp_servers.t-hub.tools.list_captains]
approval_mode = "approve"
EOF
HIDDEN_LEGACY_SNAP="$WORK/config.hidden-legacy-snapshot.toml"
HIDDEN_LEGACY_EXPECTED="$WORK/config.hidden-legacy-expected.toml"
cp -p "$WORK/config.toml" "$HIDDEN_LEGACY_SNAP"
cp -p "$WORK/config.toml" "$HIDDEN_LEGACY_EXPECTED"
sed -i "\|^command = \"$FAKE_BIN\"$|a env_vars = [\"T_HUB_CONTROL_FILE\", \"T_HUB_SESSION_TOKEN\"]" "$HIDDEN_LEGACY_EXPECTED"
if ! T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "same-command registration with hidden Codex policy was refused"
elif cmp -s "$HIDDEN_LEGACY_EXPECTED" "$WORK/config.toml"; then
  pass "same-command hidden and nested policy is preserved byte-for-byte"
else
  fail "same-command hidden or nested policy changed during migration"
fi
if codex mcp get t-hub --json | jq -e --argjson expected "$EXPECTED_ENV_VARS" '
  .transport.env_vars == $expected and
  (.transport.env == null or .transport.env == {})
' >/dev/null; then
  pass "hidden-policy migration declares only inherited variable names"
else
  fail "hidden-policy migration did not declare inherited variable names"
fi
cp -p "$NESTED_POLICY_BASE" "$WORK/config.toml"

# --- 9. disabled canonical registration is not reported ready ---------------
codex mcp add t-hub -- "$FAKE_BIN" >/dev/null
sed -i "\|^command = \"$FAKE_BIN\"$|a env_vars = [\"T_HUB_CONTROL_FILE\", \"T_HUB_SESSION_TOKEN\"]\nenabled = false" "$WORK/config.toml"
DISABLED_SNAP="$WORK/config.disabled-snapshot.toml"
cp -p "$WORK/config.toml" "$DISABLED_SNAP"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "disabled canonical registration was reported ready"
elif cmp -s "$DISABLED_SNAP" "$WORK/config.toml"; then
  pass "disabled canonical registration is refused unchanged"
else
  fail "disabled canonical registration changed on refusal"
fi

# --- 10. concurrent config change wins before atomic replacement ------------
codex mcp remove t-hub >/dev/null
codex mcp add t-hub -- "$FAKE_BIN" >/dev/null
sed -i "\|^command = \"$FAKE_BIN\"$|a env = {}\nenv_vars = [\"T_HUB_CONTROL_ADDR\", \"T_HUB_CONTROL_TOKEN\", \"T_HUB_SESSION_TOKEN\"]" "$WORK/config.toml"
CONCURRENT_SNAP="$WORK/config.concurrent-snapshot.toml"
CONCURRENT_EXPECTED="$WORK/config.concurrent-expected.toml"
cp -p "$WORK/config.toml" "$CONCURRENT_SNAP"
cp -p "$WORK/config.toml" "$CONCURRENT_EXPECTED"
printf '# concurrent-user-change\n' >> "$CONCURRENT_EXPECTED"
REAL_TAIL="$(command -v tail)"
TAIL_WRAPPER_DIR="$WORK/tail-wrapper-bin"
mkdir "$TAIL_WRAPPER_DIR"
cat > "$TAIL_WRAPPER_DIR/tail" <<'EOF'
#!/usr/bin/env bash
"$REAL_TAIL" "$@"
result=$?
if [ "$result" -eq 0 ] && [ ! -f "$CONCURRENT_ONCE" ]; then
  : > "$CONCURRENT_ONCE"
  printf '# concurrent-user-change\n' >> "$CONCURRENT_CONFIG"
fi
exit "$result"
EOF
chmod 700 "$TAIL_WRAPPER_DIR/tail"
if PATH="$TAIL_WRAPPER_DIR:$PATH" REAL_TAIL="$REAL_TAIL" \
  CONCURRENT_ONCE="$WORK/concurrent-once" CONCURRENT_CONFIG="$WORK/config.toml" \
  T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1; then
  fail "concurrent config modification was overwritten"
elif cmp -s "$CONCURRENT_EXPECTED" "$WORK/config.toml"; then
  pass "concurrent config modification is preserved and replacement refused"
else
  fail "concurrent config modification was not preserved exactly"
fi

# --- 11. concurrent change during verification is preserved -----------------
POST_VERIFY_EXPECTED="$WORK/config.post-verify-expected.toml"
cp -p "$WORK/config.toml" "$POST_VERIFY_EXPECTED"
sed -i 's/env_vars = \["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"\]/env_vars = ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]/' "$POST_VERIFY_EXPECTED"
printf '# concurrent-post-verification-change\n' >> "$POST_VERIFY_EXPECTED"
POST_VERIFY_WRAPPER_DIR="$WORK/post-verify-wrapper-bin"
POST_VERIFY_COUNT="$WORK/post-verify-count"
mkdir "$POST_VERIFY_WRAPPER_DIR"
cat > "$POST_VERIFY_WRAPPER_DIR/codex" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = mcp ] && [ "${2:-}" = get ] && [ "${3:-}" = t-hub ] && [ "${4:-}" = --json ]; then
  count=0
  [ ! -f "$POST_VERIFY_COUNT" ] || count="$(cat "$POST_VERIFY_COUNT")"
  count=$((count + 1))
  printf '%s\n' "$count" > "$POST_VERIFY_COUNT"
  if [ "$count" -eq 2 ]; then
    printf '# concurrent-post-verification-change\n' >> "$CONCURRENT_CONFIG"
  fi
fi
exec "$REAL_CODEX" "$@"
EOF
chmod 700 "$POST_VERIFY_WRAPPER_DIR/codex"
if PATH="$POST_VERIFY_WRAPPER_DIR:$PATH" REAL_CODEX="$REAL_CODEX" \
  POST_VERIFY_COUNT="$POST_VERIFY_COUNT" CONCURRENT_CONFIG="$WORK/config.toml" \
  T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1; then
  fail "concurrent verification modification was accepted"
elif cmp -s "$POST_VERIFY_EXPECTED" "$WORK/config.toml"; then
  pass "concurrent verification modification is preserved and migration refused"
else
  fail "concurrent verification modification was not preserved exactly"
fi

# --- 12. symlinked config is never replaced during migration ----------------
codex mcp remove t-hub >/dev/null
codex mcp add t-hub -- "$FAKE_BIN" >/dev/null
sed -i "\|^command = \"$FAKE_BIN\"$|a env = {}\nenv_vars = [\"T_HUB_CONTROL_ADDR\", \"T_HUB_CONTROL_TOKEN\", \"T_HUB_SESSION_TOKEN\"]" "$WORK/config.toml"
SYMLINK_TARGET="$WORK/config.symlink-target.toml"
SYMLINK_SNAP="$WORK/config.symlink-snapshot.toml"
mv "$WORK/config.toml" "$SYMLINK_TARGET"
cp -p "$SYMLINK_TARGET" "$SYMLINK_SNAP"
ln -s "$(basename "$SYMLINK_TARGET")" "$WORK/config.toml"
SYMLINK_VALUE="$(readlink "$WORK/config.toml")"
if T_HUB_MCP_BIN="$FAKE_BIN" bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1; then
  fail "symlinked legacy registration was migrated"
elif [ -L "$WORK/config.toml" ] \
  && [ "$(readlink "$WORK/config.toml")" = "$SYMLINK_VALUE" ] \
  && cmp -s "$SYMLINK_SNAP" "$SYMLINK_TARGET"; then
  pass "symlink identity and target bytes are preserved on refusal"
else
  fail "symlink identity or target bytes changed on refusal"
fi

if [ "$FAILED" -eq 0 ]; then
  echo "ensure-thub-codex.test: PASS"
else
  echo "ensure-thub-codex.test: FAIL" >&2
fi
exit "$FAILED"
