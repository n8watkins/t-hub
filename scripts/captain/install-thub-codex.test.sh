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
  printf '{"tools":[{"name":"list_terminals"},{"name":"claim_captain"},{"name":"cortana_bootstrap","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"t-hubTier":"read","confirmationRequired":false,"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}]}\n'
  exit 0
fi
exit 1
EOF
chmod 700 "$SOURCE"

BIN_DIR="$WORK/install/bin"
CAPTAIN_DIR="$WORK/install/captain"

for bad_args in \
  '--repair-skills --repair-skills' \
  '--migrate-legacy-registration --migrate-legacy-registration' \
  '--unknown-option'; do
  # shellcheck disable=SC2086
  T_HUB_MCP_SOURCE="$SOURCE" bash "$SCRIPT" $bad_args >/dev/null 2>&1
  bad_status=$?
  if [ "$bad_status" -eq 2 ]; then
    pass "installer refuses duplicate or unknown flags: $bad_args"
  else
    fail "installer accepted or misclassified flags: $bad_args"
  fi
done

NONREGULAR_WORK="$WORK/nonregular-source"
mkdir -p "$NONREGULAR_WORK/directory"
ln -s "$SOURCE" "$NONREGULAR_WORK/symlink"
mkfifo "$NONREGULAR_WORK/fifo"
chmod 700 "$NONREGULAR_WORK/fifo"
for nonregular in symlink directory fifo; do
  if T_HUB_MCP_SOURCE="$NONREGULAR_WORK/$nonregular" T_HUB_BIN_DIR="$BIN_DIR" \
    T_HUB_CAPTAIN_DIR="$CAPTAIN_DIR" bash "$SCRIPT" >/dev/null 2>&1; then
    fail "installer accepted nonregular source: $nonregular"
  else
    pass "installer refuses nonregular source without opening it: $nonregular"
  fi
done

mkdir -p "$BIN_DIR"
printf 'known-good binary\n' > "$BIN_DIR/t-hub-mcp"
chmod 700 "$BIN_DIR/t-hub-mcp"
STALE_SOURCE="$WORK/stale-source-t-hub-mcp"
cat > "$STALE_SOURCE" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--list-tools" ]; then
  printf '{"tools":[{"name":"list_terminals"}]}\n'
  exit 0
fi
exit 1
EOF
chmod 700 "$STALE_SOURCE"
if T_HUB_MCP_SOURCE="$STALE_SOURCE" T_HUB_BIN_DIR="$BIN_DIR" \
  T_HUB_CAPTAIN_DIR="$CAPTAIN_DIR" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "installer accepted a stale MCP catalog"
elif [ "$(cat "$BIN_DIR/t-hub-mcp")" = "known-good binary" ]; then
  pass "stale MCP catalog is refused before replacing the installed binary"
else
  fail "stale MCP catalog changed the installed binary"
fi
rm -f "$BIN_DIR/t-hub-mcp"

RACE_WORK="$WORK/source-race"
RACE_HOME="$RACE_WORK/home"
RACE_CODEX="$RACE_WORK/codex"
RACE_CLAUDE="$RACE_WORK/claude"
RACE_BIN="$RACE_WORK/install/bin"
RACE_CAPTAIN="$RACE_WORK/install/captain"
RACE_SOURCE="$RACE_WORK/source-t-hub-mcp"
RACE_PAUSE="$RACE_WORK/pause"
mkdir -p "$RACE_HOME" "$RACE_CODEX" "$RACE_CLAUDE" "$RACE_BIN" "$RACE_CAPTAIN" "$RACE_PAUSE"
cp "$SOURCE" "$RACE_SOURCE"
printf 'known-good binary\n' > "$RACE_BIN/t-hub-mcp"
chmod 700 "$RACE_BIN/t-hub-mcp"
printf 'operator config\n' > "$RACE_CODEX/config.toml"
race_config_before="$(sha256sum "$RACE_CODEX/config.toml" | awk '{print $1}')"
HOME="$RACE_HOME" CODEX_HOME="$RACE_CODEX" CLAUDE_HOME="$RACE_CLAUDE" \
  T_HUB_MCP_SOURCE="$RACE_SOURCE" T_HUB_BIN_DIR="$RACE_BIN" \
  T_HUB_CAPTAIN_DIR="$RACE_CAPTAIN" T_HUB_INSTALL_SOURCE_PAUSE_DIR="$RACE_PAUSE" \
  bash "$SCRIPT" >/dev/null 2>&1 &
race_pid=$!
race_wait=0
while [ ! -e "$RACE_PAUSE/discovered" ] && [ "$race_wait" -lt 1000 ]; do
  sleep 0.01
  race_wait=$((race_wait + 1))
done
RACE_REPLACEMENT="$RACE_WORK/replacement-t-hub-mcp"
cp "$SOURCE" "$RACE_REPLACEMENT"
printf '\n# different valid binary bytes\n' >> "$RACE_REPLACEMENT"
chmod 700 "$RACE_REPLACEMENT"
mv "$RACE_REPLACEMENT" "$RACE_SOURCE"
: > "$RACE_PAUSE/resume"
wait "$race_pid"
race_status=$?
if [ "$race_status" -ne 0 ] \
  && [ "$(cat "$RACE_BIN/t-hub-mcp")" = "known-good binary" ] \
  && [ "$(sha256sum "$RACE_CODEX/config.toml" | awk '{print $1}')" = "$race_config_before" ] \
  && [ ! -e "$RACE_HOME/.t-hub/transactions/install-current/manifest.json" ]; then
  pass "source inode swap after discovery preserves binary, config, and manifest"
else
  fail "source inode swap crossed the verified snapshot boundary"
fi

HARDLINK_WORK="$WORK/source-hardlink-race"
HARDLINK_HOME="$HARDLINK_WORK/home"
HARDLINK_CODEX="$HARDLINK_WORK/codex"
HARDLINK_CLAUDE="$HARDLINK_WORK/claude"
HARDLINK_BIN="$HARDLINK_WORK/install/bin"
HARDLINK_CAPTAIN="$HARDLINK_WORK/install/captain"
HARDLINK_SOURCE="$HARDLINK_WORK/source-t-hub-mcp"
HARDLINK_SIBLING="$HARDLINK_WORK/source-sibling"
HARDLINK_PAUSE="$HARDLINK_WORK/pause"
mkdir -p "$HARDLINK_HOME" "$HARDLINK_CODEX" "$HARDLINK_CLAUDE" \
  "$HARDLINK_BIN" "$HARDLINK_CAPTAIN" "$HARDLINK_PAUSE"
cp "$SOURCE" "$HARDLINK_SOURCE"
ln "$HARDLINK_SOURCE" "$HARDLINK_SIBLING"
printf 'known-good hardlink binary\n' > "$HARDLINK_BIN/t-hub-mcp"
chmod 700 "$HARDLINK_BIN/t-hub-mcp"
HOME="$HARDLINK_HOME" CODEX_HOME="$HARDLINK_CODEX" CLAUDE_HOME="$HARDLINK_CLAUDE" \
  T_HUB_MCP_SOURCE="$HARDLINK_SOURCE" T_HUB_BIN_DIR="$HARDLINK_BIN" \
  T_HUB_CAPTAIN_DIR="$HARDLINK_CAPTAIN" T_HUB_INSTALL_SOURCE_PAUSE_DIR="$HARDLINK_PAUSE" \
  bash "$SCRIPT" >/dev/null 2>&1 &
hardlink_pid=$!
hardlink_wait=0
while [ ! -e "$HARDLINK_PAUSE/discovered" ] && [ "$hardlink_wait" -lt 1000 ]; do
  sleep 0.01
  hardlink_wait=$((hardlink_wait + 1))
done
printf '#!/usr/bin/env bash\nexit 1\n' > "$HARDLINK_SIBLING"
: > "$HARDLINK_PAUSE/resume"
wait "$hardlink_pid"
hardlink_status=$?
if [ "$hardlink_status" -ne 0 ] \
  && [ "$(cat "$HARDLINK_BIN/t-hub-mcp")" = "known-good hardlink binary" ] \
  && [ ! -e "$HARDLINK_HOME/.t-hub/transactions/install-current/manifest.json" ]; then
  pass "hardlink content mutation cannot cross the verified snapshot boundary"
else
  fail "hardlink content mutation changed install state"
fi

ROLLBACK_WORK="$WORK/installed-catalog-rollback"
ROLLBACK_HOME="$ROLLBACK_WORK/home"
ROLLBACK_CODEX="$ROLLBACK_WORK/codex"
ROLLBACK_CLAUDE="$ROLLBACK_WORK/claude"
ROLLBACK_BIN="$ROLLBACK_WORK/install/bin"
ROLLBACK_CAPTAIN="$ROLLBACK_WORK/install/captain"
ROLLBACK_SOURCE="$ROLLBACK_WORK/source-t-hub-mcp"
ROLLBACK_COUNTER="$ROLLBACK_WORK/catalog-counter"
mkdir -p "$ROLLBACK_HOME" "$ROLLBACK_CODEX" "$ROLLBACK_CLAUDE" \
  "$ROLLBACK_BIN" "$ROLLBACK_CAPTAIN"
cat > "$ROLLBACK_SOURCE" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--list-tools" ]; then
  count=0
  [ ! -f "$CATALOG_COUNTER" ] || count="$(cat "$CATALOG_COUNTER")"
  count=$((count + 1))
  printf '%s\n' "$count" > "$CATALOG_COUNTER"
  if [ "$count" -eq 1 ]; then
    printf '{"tools":[{"name":"cortana_bootstrap","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"t-hubTier":"read","confirmationRequired":false,"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}]}\n'
  else
    printf '{"tools":[{"name":"list_terminals"}]}\n'
  fi
  exit 0
fi
exit 1
EOF
chmod 700 "$ROLLBACK_SOURCE"
printf 'known-good rollback binary\n' > "$ROLLBACK_BIN/t-hub-mcp"
chmod 700 "$ROLLBACK_BIN/t-hub-mcp"
printf 'model = "operator-model"\n' > "$ROLLBACK_CODEX/config.toml"
rollback_config_before="$(sha256sum "$ROLLBACK_CODEX/config.toml" | awk '{print $1}')"
if HOME="$ROLLBACK_HOME" CODEX_HOME="$ROLLBACK_CODEX" CLAUDE_HOME="$ROLLBACK_CLAUDE" \
  CATALOG_COUNTER="$ROLLBACK_COUNTER" T_HUB_MCP_SOURCE="$ROLLBACK_SOURCE" \
  T_HUB_BIN_DIR="$ROLLBACK_BIN" T_HUB_CAPTAIN_DIR="$ROLLBACK_CAPTAIN" \
  bash "$SCRIPT" >/dev/null 2>&1; then
  fail "installer accepted a stale post-install catalog"
elif [ "$(cat "$ROLLBACK_BIN/t-hub-mcp")" = "known-good rollback binary" ] \
  && [ "$(sha256sum "$ROLLBACK_CODEX/config.toml" | awk '{print $1}')" = "$rollback_config_before" ] \
  && [ ! -e "$ROLLBACK_HOME/.t-hub/transactions/install-current/manifest.json" ] \
  && ! find "$ROLLBACK_HOME/.t-hub/transactions" -name '.source-snapshot.*' -print -quit \
    | grep -q .; then
  pass "post-install catalog failure rolls back binary, config, manifest, and private snapshot"
else
  fail "post-install catalog failure left partial install state"
fi

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

BUSY_EXEC_WORK="$WORK/busy-executable"
BUSY_EXEC_HOME="$BUSY_EXEC_WORK/home"
BUSY_EXEC_CODEX="$BUSY_EXEC_WORK/codex"
BUSY_EXEC_CLAUDE="$BUSY_EXEC_WORK/claude"
BUSY_EXEC_BIN="$BUSY_EXEC_WORK/install/bin"
BUSY_EXEC_CAPTAIN="$BUSY_EXEC_WORK/install/captain"
mkdir -p "$BUSY_EXEC_HOME" "$BUSY_EXEC_CODEX" "$BUSY_EXEC_CLAUDE" \
  "$BUSY_EXEC_BIN" "$BUSY_EXEC_CAPTAIN"
cp -p /bin/sleep "$BUSY_EXEC_BIN/t-hub-mcp"
"$BUSY_EXEC_BIN/t-hub-mcp" 120 &
busy_exec_pid=$!
if HOME="$BUSY_EXEC_HOME" CODEX_HOME="$BUSY_EXEC_CODEX" CLAUDE_HOME="$BUSY_EXEC_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BUSY_EXEC_BIN" \
  T_HUB_CAPTAIN_DIR="$BUSY_EXEC_CAPTAIN" bash "$SCRIPT" \
  >"$BUSY_EXEC_WORK/install.log" 2>&1; then
  pass "installer replaces a running executable without truncating its displaced inode"
else
  fail "running displaced executable caused ETXTBSY during install"
  sed 's/^/    /' "$BUSY_EXEC_WORK/install.log" >&2
fi
kill "$busy_exec_pid" 2>/dev/null || true
wait "$busy_exec_pid" 2>/dev/null || true

BUSY_CRASH_WORK="$WORK/busy-executable-crash"
BUSY_CRASH_HOME="$BUSY_CRASH_WORK/home"
BUSY_CRASH_CODEX="$BUSY_CRASH_WORK/codex"
BUSY_CRASH_CLAUDE="$BUSY_CRASH_WORK/claude"
BUSY_CRASH_BIN="$BUSY_CRASH_WORK/install/bin"
BUSY_CRASH_CAPTAIN="$BUSY_CRASH_WORK/install/captain"
mkdir -p "$BUSY_CRASH_HOME" "$BUSY_CRASH_CODEX" "$BUSY_CRASH_CLAUDE" \
  "$BUSY_CRASH_BIN" "$BUSY_CRASH_CAPTAIN"
cp -p /bin/sleep "$BUSY_CRASH_BIN/t-hub-mcp"
"$BUSY_CRASH_BIN/t-hub-mcp" 120 &
busy_crash_pid=$!
HOME="$BUSY_CRASH_HOME" CODEX_HOME="$BUSY_CRASH_CODEX" CLAUDE_HOME="$BUSY_CRASH_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BUSY_CRASH_BIN" \
  T_HUB_CAPTAIN_DIR="$BUSY_CRASH_CAPTAIN" T_HUB_INSTALL_CRASH_AFTER_STAGE=binary \
  bash "$SCRIPT" >/dev/null 2>&1 || true
if HOME="$BUSY_CRASH_HOME" CODEX_HOME="$BUSY_CRASH_CODEX" CLAUDE_HOME="$BUSY_CRASH_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BUSY_CRASH_BIN" \
  T_HUB_CAPTAIN_DIR="$BUSY_CRASH_CAPTAIN" bash "$SCRIPT" \
  >"$BUSY_CRASH_WORK/recovery.log" 2>&1 \
  && kill -0 "$busy_crash_pid" 2>/dev/null \
  && [ ! -e "$BUSY_CRASH_HOME/.t-hub/transactions/install-current" ] \
  && ! find "$BUSY_CRASH_BIN" -maxdepth 1 -name '.t-hub-stage.*' | grep -q .; then
  pass "SIGKILL recovery converges while the displaced executable inode remains active"
else
  fail "SIGKILL recovery leaked or truncated an active displaced executable"
  sed 's/^/    /' "$BUSY_CRASH_WORK/recovery.log" >&2
fi
kill "$busy_crash_pid" 2>/dev/null || true
wait "$busy_crash_pid" 2>/dev/null || true

BUSY_ATOMIC_WORK="$WORK/busy-executable-atomic-crash"
BUSY_ATOMIC_HOME="$BUSY_ATOMIC_WORK/home"
BUSY_ATOMIC_CODEX="$BUSY_ATOMIC_WORK/codex"
BUSY_ATOMIC_CLAUDE="$BUSY_ATOMIC_WORK/claude"
BUSY_ATOMIC_BIN="$BUSY_ATOMIC_WORK/install/bin"
BUSY_ATOMIC_CAPTAIN="$BUSY_ATOMIC_WORK/install/captain"
mkdir -p "$BUSY_ATOMIC_HOME" "$BUSY_ATOMIC_CODEX" "$BUSY_ATOMIC_CLAUDE" \
  "$BUSY_ATOMIC_BIN" "$BUSY_ATOMIC_CAPTAIN"
cp -p /bin/sleep "$BUSY_ATOMIC_BIN/t-hub-mcp"
busy_atomic_before="$(sha256sum "$BUSY_ATOMIC_BIN/t-hub-mcp" | awk '{print $1}')"
"$BUSY_ATOMIC_BIN/t-hub-mcp" 120 &
busy_atomic_pid=$!
if HOME="$BUSY_ATOMIC_HOME" CODEX_HOME="$BUSY_ATOMIC_CODEX" CLAUDE_HOME="$BUSY_ATOMIC_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BUSY_ATOMIC_BIN" \
  T_HUB_CAPTAIN_DIR="$BUSY_ATOMIC_CAPTAIN" T_HUB_ATOMIC_CRASH_AT=committed \
  T_HUB_ATOMIC_CRASH_ONCE_FILE="$BUSY_ATOMIC_WORK/crashed-once" \
  bash "$SCRIPT" >/dev/null 2>&1; then
  fail "atomic committed-phase crash unexpectedly succeeded"
elif kill -0 "$busy_atomic_pid" 2>/dev/null \
  && [ "$(sha256sum "$BUSY_ATOMIC_BIN/t-hub-mcp" | awk '{print $1}')" = "$busy_atomic_before" ] \
  && [ ! -e "$BUSY_ATOMIC_HOME/.t-hub/transactions/install-current" ] \
  && ! find "$BUSY_ATOMIC_BIN" -maxdepth 1 -name '.t-hub-stage.*' | grep -q .; then
  pass "committed-phase recovery releases a live displaced inode before journal cleanup"
else
  fail "committed-phase recovery lost evidence or leaked a live displaced stage"
fi
if HOME="$BUSY_ATOMIC_HOME" CODEX_HOME="$BUSY_ATOMIC_CODEX" CLAUDE_HOME="$BUSY_ATOMIC_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$BUSY_ATOMIC_BIN" \
  T_HUB_CAPTAIN_DIR="$BUSY_ATOMIC_CAPTAIN" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "rerun converges after active-inode atomic recovery"
else
  fail "rerun failed after active-inode atomic recovery"
fi
kill "$busy_atomic_pid" 2>/dev/null || true
wait "$busy_atomic_pid" 2>/dev/null || true

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

CLAUDE_KILL="$ADOPTION_WORK/claude-kill"
mkdir -p "$CLAUDE_KILL/home" "$CLAUDE_KILL/codex" "$CLAUDE_KILL/claude" \
  "$CLAUDE_KILL/bin" "$CLAUDE_KILL/captain"
cat > "$CLAUDE_KILL/atomic-kill-wrapper.py" <<'EOF'
#!/usr/bin/env python3
import json, os, pathlib, signal, subprocess, sys, time
published = json.loads(sys.argv[-1]) if sys.argv[1] == "publish" else {}
is_claude_state = any(pathlib.Path(value).name == "claude-state.json" for value in sys.argv[2:])
if sys.argv[1] == "publish" and is_claude_state and published.get("status") == "committed":
    os.kill(os.getppid(), signal.SIGKILL)
    time.sleep(0.2)
    raise SystemExit(137)
raise SystemExit(subprocess.run([sys.executable, os.environ["REAL_ATOMIC"], *sys.argv[1:]]).returncode)
EOF
chmod 700 "$CLAUDE_KILL/atomic-kill-wrapper.py"
if HOME="$CLAUDE_KILL/home" CODEX_HOME="$CLAUDE_KILL/codex" CLAUDE_HOME="$CLAUDE_KILL/claude" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CLAUDE_KILL/bin" \
  T_HUB_CAPTAIN_DIR="$CLAUDE_KILL/captain" \
  T_HUB_ATOMIC_CONFIG_HELPER="$CLAUDE_KILL/atomic-kill-wrapper.py" \
  REAL_ATOMIC="$REAL_ATOMIC" bash "$SCRIPT" >"$CLAUDE_KILL/install.log" 2>&1; then
  fail "killed Claude helper unexpectedly succeeded"
elif [ ! -e "$CLAUDE_KILL/home/.t-hub/transactions/install-current" ] \
  && jq -e '
    (has("mcpServers") | not) or
    ((.mcpServers | type) == "object" and (.mcpServers | has("t-hub") | not))
  ' "$CLAUDE_KILL/home/.claude.json" >/dev/null; then
  pass "parent adopts and rolls back Claude before-only publication"
else
  fail "Claude before-only publication wedged or left partial state"
fi
if HOME="$CLAUDE_KILL/home" CODEX_HOME="$CLAUDE_KILL/codex" CLAUDE_HOME="$CLAUDE_KILL/claude" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CLAUDE_KILL/bin" \
  T_HUB_CAPTAIN_DIR="$CLAUDE_KILL/captain" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "rerun converges after killed Claude helper"
else
  fail "rerun remained wedged after killed Claude helper"
fi

CODEX_KILL="$ADOPTION_WORK/codex-kill"
mkdir -p "$CODEX_KILL/home" "$CODEX_KILL/codex" "$CODEX_KILL/claude" \
  "$CODEX_KILL/bin" "$CODEX_KILL/captain"
printf 'original binary\n' > "$CODEX_KILL/bin/t-hub-mcp"
cat > "$CODEX_KILL/codex/config.toml" <<EOF
[mcp_servers.t-hub]
command = "/stale"
EOF
cp -p "$CODEX_KILL/codex/config.toml" "$CODEX_KILL/codex-before.toml"
cat > "$CODEX_KILL/atomic-kill-wrapper.py" <<'EOF'
#!/usr/bin/env python3
import json, os, pathlib, signal, subprocess, sys, time
published = json.loads(sys.argv[-1]) if sys.argv[1] == "publish" else {}
is_codex_state = any(pathlib.Path(value).name == "codex-state.json" for value in sys.argv[2:])
if sys.argv[1] == "publish" and is_codex_state and published.get("status") == "committed":
    os.kill(os.getppid(), signal.SIGKILL)
    time.sleep(0.2)
    raise SystemExit(137)
raise SystemExit(subprocess.run([sys.executable, os.environ["REAL_ATOMIC"], *sys.argv[1:]]).returncode)
EOF
chmod 700 "$CODEX_KILL/atomic-kill-wrapper.py"
if HOME="$CODEX_KILL/home" CODEX_HOME="$CODEX_KILL/codex" CLAUDE_HOME="$CODEX_KILL/claude" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CODEX_KILL/bin" \
  T_HUB_CAPTAIN_DIR="$CODEX_KILL/captain" \
  T_HUB_ATOMIC_CONFIG_HELPER="$CODEX_KILL/atomic-kill-wrapper.py" \
  REAL_ATOMIC="$REAL_ATOMIC" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "killed Codex helper unexpectedly succeeded"
elif [ ! -e "$CODEX_KILL/home/.t-hub/transactions/install-current" ] \
  && [ "$(cat "$CODEX_KILL/bin/t-hub-mcp")" = "original binary" ] \
  && cmp -s "$CODEX_KILL/codex-before.toml" "$CODEX_KILL/codex/config.toml"; then
  pass "parent adopts and rolls back Codex before-only publication"
else
  fail "Codex before-only publication lost rollback evidence or left a missing binary"
fi
if HOME="$CODEX_KILL/home" CODEX_HOME="$CODEX_KILL/codex" CLAUDE_HOME="$CODEX_KILL/claude" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CODEX_KILL/bin" \
  T_HUB_CAPTAIN_DIR="$CODEX_KILL/captain" bash "$SCRIPT" >/dev/null 2>&1; then
  pass "rerun converges after killed Codex helper"
else
  fail "rerun remained wedged after killed Codex helper"
fi

FAILED_RECOVERY="$ADOPTION_WORK/failed-recovery"
mkdir -p "$FAILED_RECOVERY/home" "$FAILED_RECOVERY/codex" "$FAILED_RECOVERY/claude" \
  "$FAILED_RECOVERY/bin" "$FAILED_RECOVERY/captain" "$FAILED_RECOVERY/wrapper-bin"
printf 'original binary\n' > "$FAILED_RECOVERY/bin/t-hub-mcp"
printf '{"mcpServers":{"t-hub":{"type":"stdio","command":"/stale","args":[],"env":{}}}}\n' \
  > "$FAILED_RECOVERY/home/.claude.json"
cat > "$FAILED_RECOVERY/wrapper-bin/claude" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = mcp ] && [ "${2:-}" = remove ]; then
  jq 'del(.mcpServers["t-hub"])' "$CLAUDE_RACE_CONFIG" > "$CLAUDE_RACE_CONFIG.removed"
  mv "$CLAUDE_RACE_CONFIG.removed" "$CLAUDE_RACE_CONFIG"
  kill -KILL "$PPID"
  jq '.mcpServers["t-hub"] = {
    type:"stdio",command:"/concurrent-custom",args:["--owned"],env:{OWNER:"other"}
  }' "$CLAUDE_RACE_CONFIG" > "$CLAUDE_RACE_CONFIG.concurrent"
  mv "$CLAUDE_RACE_CONFIG.concurrent" "$CLAUDE_RACE_CONFIG"
  sleep 0.1
  exit 0
fi
exit 97
EOF
chmod 700 "$FAILED_RECOVERY/wrapper-bin/claude"
if HOME="$FAILED_RECOVERY/home" CODEX_HOME="$FAILED_RECOVERY/codex" \
  CLAUDE_HOME="$FAILED_RECOVERY/claude" PATH="$FAILED_RECOVERY/wrapper-bin:$PATH" \
  CLAUDE_RACE_CONFIG="$FAILED_RECOVERY/home/.claude.json" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$FAILED_RECOVERY/bin" \
  T_HUB_CAPTAIN_DIR="$FAILED_RECOVERY/captain" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "killed stale-Claude replacement unexpectedly succeeded"
fi
FAILED_RECOVERY_TXN="$FAILED_RECOVERY/home/.t-hub/transactions/install-current"
cp -a "$FAILED_RECOVERY_TXN" "$FAILED_RECOVERY/original-transaction"
cp -p "$FAILED_RECOVERY/bin/t-hub-mcp" "$FAILED_RECOVERY/binary-before-rerun"
cp -p "$FAILED_RECOVERY/home/.claude.json" "$FAILED_RECOVERY/claude-before-rerun.json"
if HOME="$FAILED_RECOVERY/home" CODEX_HOME="$FAILED_RECOVERY/codex" \
  CLAUDE_HOME="$FAILED_RECOVERY/claude" T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$FAILED_RECOVERY/bin" T_HUB_CAPTAIN_DIR="$FAILED_RECOVERY/captain" \
  bash "$SCRIPT" >"$FAILED_RECOVERY/recovery.log" 2>&1; then
  fail "failed ownership recovery unexpectedly started a new install"
elif diff -r "$FAILED_RECOVERY/original-transaction" "$FAILED_RECOVERY_TXN" >/dev/null \
  && cmp -s "$FAILED_RECOVERY/binary-before-rerun" "$FAILED_RECOVERY/bin/t-hub-mcp" \
  && cmp -s "$FAILED_RECOVERY/claude-before-rerun.json" "$FAILED_RECOVERY/home/.claude.json" \
  && grep -Fq 'original binary' "$FAILED_RECOVERY_TXN/recovery/binary.bin" \
  && ! grep -Fq 'rolled back interrupted transaction' "$FAILED_RECOVERY/recovery.log"; then
  pass "failed ownership recovery retains the original transaction and binary evidence"
else
  fail "failed ownership recovery destroyed evidence or began a partial new install"
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
  && [ ! -e "$ROLLBACK_CLAUDE_HOME/skills" ] \
  && ! find "$ROLLBACK_WORK" -name '*.t-hub-delete.*' | grep -q .; then
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

SKILL_KILL_WORK="$WORK/skill-kill"
SKILL_KILL_CODEX="$SKILL_KILL_WORK/codex"
SKILL_KILL_CLAUDE="$SKILL_KILL_WORK/claude"
SKILL_KILL_TXN="$SKILL_KILL_WORK/transaction"
mkdir -p "$SKILL_KILL_CODEX" "$SKILL_KILL_CLAUDE"
if T_HUB_CODEX_SKILLS_DIR="$SKILL_KILL_CODEX/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$SKILL_KILL_CLAUDE/skills" \
  T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_KILL_CLAUDE/commands" \
  T_HUB_SKILL_TRANSACTION_DIR="$SKILL_KILL_TXN" \
  T_HUB_ATOMIC_CONFIG_HELPER="$HERE/atomic-config.py" \
  T_HUB_SKILL_CRASH_AFTER_INDEX=3 \
  bash "$HERE/install-captain-skills.sh" >/dev/null 2>&1; then
  fail "mid-copy skill SIGKILL unexpectedly succeeded"
elif [ -d "$SKILL_KILL_TXN" ]; then
  pass "mid-copy skill SIGKILL leaves a durable recovery journal"
else
  fail "mid-copy skill SIGKILL lost its recovery journal"
fi
if T_HUB_CODEX_SKILLS_DIR="$SKILL_KILL_CODEX/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$SKILL_KILL_CLAUDE/skills" \
  T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_KILL_CLAUDE/commands" \
  T_HUB_SKILL_TRANSACTION_DIR="$SKILL_KILL_TXN" \
  T_HUB_ATOMIC_CONFIG_HELPER="$HERE/atomic-config.py" \
  bash "$HERE/install-captain-skills.sh" >/dev/null 2>&1 \
  && T_HUB_CODEX_SKILLS_DIR="$SKILL_KILL_CODEX/skills" \
    T_HUB_CLAUDE_SKILLS_DIR="$SKILL_KILL_CLAUDE/skills" \
    T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_KILL_CLAUDE/commands" \
    bash "$HERE/install-captain-skills.sh" --verify >/dev/null 2>&1 \
  && [ ! -e "$SKILL_KILL_TXN" ]; then
  pass "rerun recovers and completes every skill after mid-copy SIGKILL"
else
  fail "mid-copy skill recovery did not converge cleanly"
fi
if T_HUB_CODEX_SKILLS_DIR="$SKILL_KILL_CODEX/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$SKILL_KILL_CLAUDE/skills" \
  T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_KILL_CLAUDE/commands" \
  T_HUB_SKILL_TRANSACTION_DIR="$SKILL_KILL_TXN" \
  T_HUB_ATOMIC_CONFIG_HELPER="$HERE/atomic-config.py" \
  T_HUB_SKILL_CRASH_AFTER_INDEX=3 \
  bash "$HERE/install-captain-skills.sh" >/dev/null 2>&1; then
  fail "managed-target skill SIGKILL unexpectedly succeeded"
elif [ -f "$SKILL_KILL_CLAUDE/commands/handoff.md" ] \
  && [ -f "$SKILL_KILL_CLAUDE/commands/handoff.md.t-hub-managed" ]; then
  pass "SIGKILL does not delete untouched future managed targets"
else
  fail "SIGKILL damaged an untouched future managed target"
fi
if T_HUB_CODEX_SKILLS_DIR="$SKILL_KILL_CODEX/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$SKILL_KILL_CLAUDE/skills" \
  T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_KILL_CLAUDE/commands" \
  T_HUB_SKILL_TRANSACTION_DIR="$SKILL_KILL_TXN" \
  T_HUB_ATOMIC_CONFIG_HELPER="$HERE/atomic-config.py" \
  bash "$HERE/install-captain-skills.sh" >/dev/null 2>&1 \
  && [ ! -e "$SKILL_KILL_TXN" ]; then
  pass "pre-existing managed targets recover after mid-copy SIGKILL"
else
  fail "pre-existing managed target recovery wedged"
fi

SKILL_INTENT_WORK="$WORK/skill-intent-race"
SKILL_INTENT_CODEX="$SKILL_INTENT_WORK/codex"
SKILL_INTENT_CLAUDE="$SKILL_INTENT_WORK/claude"
SKILL_INTENT_TXN="$SKILL_INTENT_WORK/transaction"
mkdir -p "$SKILL_INTENT_CODEX" "$SKILL_INTENT_CLAUDE"
if T_HUB_CODEX_SKILLS_DIR="$SKILL_INTENT_CODEX/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$SKILL_INTENT_CLAUDE/skills" \
  T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_INTENT_CLAUDE/commands" \
  T_HUB_SKILL_TRANSACTION_DIR="$SKILL_INTENT_TXN" \
  T_HUB_ATOMIC_CONFIG_HELPER="$HERE/atomic-config.py" \
  T_HUB_SKILL_CRASH_AFTER_TARGET_INTENT_INDEX=0 \
  bash "$HERE/install-captain-skills.sh" >/dev/null 2>&1; then
  fail "pre-mutation skill SIGKILL unexpectedly succeeded"
fi
mkdir -p "$SKILL_INTENT_CODEX/skills/captain"
printf 'concurrent writer\n' > "$SKILL_INTENT_CODEX/skills/captain/OWNER"
if T_HUB_CODEX_SKILLS_DIR="$SKILL_INTENT_CODEX/skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$SKILL_INTENT_CLAUDE/skills" \
  T_HUB_CLAUDE_COMMANDS_DIR="$SKILL_INTENT_CLAUDE/commands" \
  T_HUB_SKILL_TRANSACTION_DIR="$SKILL_INTENT_TXN" \
  T_HUB_SKILL_RECOVER_ONLY=1 T_HUB_ATOMIC_CONFIG_HELPER="$HERE/atomic-config.py" \
  bash "$HERE/install-captain-skills.sh" >/dev/null 2>&1; then
  fail "skill recovery adopted a concurrent pre-mutation target"
elif [ "$(cat "$SKILL_INTENT_CODEX/skills/captain/OWNER")" = "concurrent writer" ] \
  && [ -d "$SKILL_INTENT_TXN" ]; then
  pass "skill intent recovery preserves an unowned concurrent target and journal"
else
  fail "skill intent recovery deleted a concurrent target or its evidence"
fi

for crash_stage in binary codex-helper claude-helper atomic-helper claude-config codex-config skills; do
  CRASH_WORK="$WORK/crash-$crash_stage"
  CRASH_HOME="$CRASH_WORK/home"
  CRASH_CODEX="$CRASH_WORK/codex"
  CRASH_CLAUDE="$CRASH_WORK/claude"
  CRASH_BIN="$CRASH_WORK/install/bin"
  CRASH_CAPTAIN="$CRASH_WORK/install/captain"
  mkdir -p "$CRASH_HOME" "$CRASH_CODEX" "$CRASH_CLAUDE"
  if [ "$crash_stage" = codex-config ]; then
    printf '{"cachedMetadata":{"secret":"journal-secret-value"}}\n' > "$CRASH_HOME/.claude.json"
  fi
  if HOME="$CRASH_HOME" CODEX_HOME="$CRASH_CODEX" CLAUDE_HOME="$CRASH_CLAUDE" \
    T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CRASH_BIN" \
    T_HUB_CAPTAIN_DIR="$CRASH_CAPTAIN" \
    T_HUB_INSTALL_CRASH_AFTER_STAGE="$crash_stage" \
    bash "$SCRIPT" >"$CRASH_WORK/crash.log" 2>&1; then
    fail "whole-installer SIGKILL unexpectedly succeeded at $crash_stage"
    continue
  fi
  if [ "$crash_stage" = codex-config ]; then
    CRASH_TXN="$CRASH_HOME/.t-hub/transactions/install-current"
    if [ "$(stat -c %a "$CRASH_TXN")" = 700 ] \
      && ! find "$CRASH_TXN" -type d -printf '%m\n' | grep -vx 700 >/dev/null \
      && ! find "$CRASH_TXN" -type f -printf '%m\n' | grep -vx 600 >/dev/null \
      && grep -Fq 'journal-secret-value' "$CRASH_TXN/helper-state/claude-before.bin" \
      && ! find "$CRASH_TXN" -type f ! -name '*.bin' -print0 \
        | xargs -0 grep -F 'journal-secret-value' >/dev/null 2>&1 \
      && ! grep -Fq 'journal-secret-value' "$CRASH_WORK/crash.log"; then
      pass "secret recovery is restricted and absent from descriptors and logs"
    else
      fail "secret recovery permissions, redaction, or placement is unsafe"
    fi
  fi
  if HOME="$CRASH_HOME" CODEX_HOME="$CRASH_CODEX" CLAUDE_HOME="$CRASH_CLAUDE" \
    T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$CRASH_BIN" \
    T_HUB_CAPTAIN_DIR="$CRASH_CAPTAIN" \
    bash "$SCRIPT" >/dev/null 2>&1 \
    && [ ! -e "$CRASH_HOME/.t-hub/transactions/install-current" ] \
    && CODEX_HOME="$CRASH_CODEX" codex mcp get t-hub --json 2>/dev/null \
      | jq -e --arg bin "$CRASH_BIN/t-hub-mcp" '.transport.command == $bin' >/dev/null \
    && jq -e --arg bin "$CRASH_BIN/t-hub-mcp" \
      '.mcpServers["t-hub"].command == $bin' "$CRASH_HOME/.claude.json" >/dev/null \
    && T_HUB_CODEX_SKILLS_DIR="$CRASH_CODEX/skills" \
      T_HUB_CLAUDE_SKILLS_DIR="$CRASH_CLAUDE/skills" \
      bash "$HERE/install-captain-skills.sh" --verify >/dev/null 2>&1; then
    pass "rerun recovers whole-installer SIGKILL after $crash_stage"
  else
    fail "whole-installer recovery failed after $crash_stage"
  fi
done

PROVENANCE_WORK="$WORK/provenance"
PROVENANCE_HOME="$PROVENANCE_WORK/home"
PROVENANCE_CODEX="$PROVENANCE_WORK/codex"
PROVENANCE_CLAUDE="$PROVENANCE_WORK/claude"
PROVENANCE_BIN="$PROVENANCE_WORK/install/bin"
PROVENANCE_CAPTAIN="$PROVENANCE_WORK/install/captain"
PROVENANCE_SOURCE="$PROVENANCE_WORK/source-t-hub-mcp"
mkdir -p "$PROVENANCE_HOME" "$PROVENANCE_CODEX" "$PROVENANCE_CLAUDE"
cp -p "$SOURCE" "$PROVENANCE_SOURCE"
HOME="$PROVENANCE_HOME" CODEX_HOME="$PROVENANCE_CODEX" CLAUDE_HOME="$PROVENANCE_CLAUDE" \
  T_HUB_MCP_SOURCE="$PROVENANCE_SOURCE" T_HUB_BIN_DIR="$PROVENANCE_BIN" \
  T_HUB_CAPTAIN_DIR="$PROVENANCE_CAPTAIN" T_HUB_INSTALL_CRASH_AFTER_STAGE=binary \
  bash "$SCRIPT" >/dev/null 2>&1 || true
printf '# changed source provenance\n' >> "$PROVENANCE_SOURCE"
if HOME="$PROVENANCE_HOME" CODEX_HOME="$PROVENANCE_CODEX" CLAUDE_HOME="$PROVENANCE_CLAUDE" \
  T_HUB_MCP_SOURCE="$PROVENANCE_SOURCE" T_HUB_BIN_DIR="$PROVENANCE_BIN" \
  T_HUB_CAPTAIN_DIR="$PROVENANCE_CAPTAIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "changed source adopted an interrupted transaction"
elif [ -d "$PROVENANCE_HOME/.t-hub/transactions/install-current" ]; then
  pass "interrupted transaction refuses changed source provenance"
else
  fail "provenance mismatch destroyed recovery state"
fi
cp -p "$SOURCE" "$PROVENANCE_SOURCE"
if HOME="$PROVENANCE_HOME" CODEX_HOME="$PROVENANCE_CODEX" CLAUDE_HOME="$PROVENANCE_CLAUDE" \
  T_HUB_MCP_SOURCE="$PROVENANCE_SOURCE" T_HUB_BIN_DIR="$PROVENANCE_BIN" \
  T_HUB_CAPTAIN_DIR="$PROVENANCE_CAPTAIN" bash "$SCRIPT" >/dev/null 2>&1 \
  && [ ! -e "$PROVENANCE_HOME/.t-hub/transactions/install-current" ]; then
  pass "matching provenance recovers the interrupted transaction"
else
  fail "matching provenance did not recover"
fi

INTEGRATION_PROVENANCE="$WORK/integration-provenance"
INTEGRATION_HOME="$INTEGRATION_PROVENANCE/home"
INTEGRATION_CODEX="$INTEGRATION_PROVENANCE/codex"
INTEGRATION_CLAUDE="$INTEGRATION_PROVENANCE/claude"
INTEGRATION_BIN="$INTEGRATION_PROVENANCE/install/bin"
INTEGRATION_CAPTAIN="$INTEGRATION_PROVENANCE/install/captain"
INTEGRATION_SKILLS="$INTEGRATION_PROVENANCE/source-skills"
INTEGRATION_CODEX_SKILLS="$INTEGRATION_PROVENANCE/dest/codex-skills"
INTEGRATION_CLAUDE_SKILLS="$INTEGRATION_PROVENANCE/dest/claude-skills"
INTEGRATION_CLAUDE_COMMANDS="$INTEGRATION_PROVENANCE/dest/claude-commands"
mkdir -p "$INTEGRATION_HOME" "$INTEGRATION_CODEX" "$INTEGRATION_CLAUDE"
cp -a "$HERE/../../skills" "$INTEGRATION_SKILLS"
ln -s SKILL.md "$INTEGRATION_SKILLS/captain/provenance-link"
HOME="$INTEGRATION_HOME" CODEX_HOME="$INTEGRATION_CODEX" CLAUDE_HOME="$INTEGRATION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$INTEGRATION_BIN" \
  T_HUB_CAPTAIN_DIR="$INTEGRATION_CAPTAIN" T_HUB_SKILLS_SOURCE="$INTEGRATION_SKILLS" \
  T_HUB_CODEX_SKILLS_DIR="$INTEGRATION_CODEX_SKILLS" \
  T_HUB_CLAUDE_SKILLS_DIR="$INTEGRATION_CLAUDE_SKILLS" \
  T_HUB_CLAUDE_COMMANDS_DIR="$INTEGRATION_CLAUDE_COMMANDS" \
  T_HUB_INSTALL_CRASH_AFTER_STAGE=binary bash "$SCRIPT" >/dev/null 2>&1 || true
ln -sfn ../shipmate/SKILL.md "$INTEGRATION_SKILLS/captain/provenance-link"
if HOME="$INTEGRATION_HOME" CODEX_HOME="$INTEGRATION_CODEX" CLAUDE_HOME="$INTEGRATION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$INTEGRATION_BIN" \
  T_HUB_CAPTAIN_DIR="$INTEGRATION_CAPTAIN" T_HUB_SKILLS_SOURCE="$INTEGRATION_SKILLS" \
  T_HUB_CODEX_SKILLS_DIR="$INTEGRATION_CODEX_SKILLS" \
  T_HUB_CLAUDE_SKILLS_DIR="$INTEGRATION_CLAUDE_SKILLS" \
  T_HUB_CLAUDE_COMMANDS_DIR="$INTEGRATION_CLAUDE_COMMANDS" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "changed skill-source symlink adopted an interrupted transaction"
elif [ -d "$INTEGRATION_HOME/.t-hub/transactions/install-current" ]; then
  pass "integration provenance binds the actual skill tree including symlinks"
else
  fail "skill-source provenance mismatch destroyed recovery state"
fi
ln -sfn SKILL.md "$INTEGRATION_SKILLS/captain/provenance-link"
if HOME="$INTEGRATION_HOME" CODEX_HOME="$INTEGRATION_CODEX" CLAUDE_HOME="$INTEGRATION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$INTEGRATION_BIN" \
  T_HUB_CAPTAIN_DIR="$INTEGRATION_CAPTAIN" T_HUB_SKILLS_SOURCE="$INTEGRATION_SKILLS" \
  T_HUB_CODEX_SKILLS_DIR="$INTEGRATION_PROVENANCE/dest/other-codex-skills" \
  T_HUB_CLAUDE_SKILLS_DIR="$INTEGRATION_CLAUDE_SKILLS" \
  T_HUB_CLAUDE_COMMANDS_DIR="$INTEGRATION_CLAUDE_COMMANDS" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "changed skill destination adopted an interrupted transaction"
elif [ -d "$INTEGRATION_HOME/.t-hub/transactions/install-current" ]; then
  pass "integration provenance binds overridden skill destinations"
else
  fail "destination provenance mismatch destroyed recovery state"
fi
if HOME="$INTEGRATION_HOME" CODEX_HOME="$INTEGRATION_CODEX" CLAUDE_HOME="$INTEGRATION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$INTEGRATION_BIN" \
  T_HUB_CAPTAIN_DIR="$INTEGRATION_CAPTAIN" T_HUB_SKILLS_SOURCE="$INTEGRATION_SKILLS" \
  T_HUB_CODEX_SKILLS_DIR="$INTEGRATION_CODEX_SKILLS" \
  T_HUB_CLAUDE_SKILLS_DIR="$INTEGRATION_CLAUDE_SKILLS" \
  T_HUB_CLAUDE_COMMANDS_DIR="$INTEGRATION_CLAUDE_COMMANDS" bash "$SCRIPT" >/dev/null 2>&1 \
  && [ ! -e "$INTEGRATION_HOME/.t-hub/transactions/install-current" ]; then
  pass "matching integration provenance recovers after strict refusals"
else
  fail "matching integration provenance did not recover"
fi

OPTION_WORK="$WORK/provenance-option"
OPTION_HOME="$OPTION_WORK/home"
OPTION_CODEX="$OPTION_WORK/codex"
OPTION_CLAUDE="$OPTION_WORK/claude"
OPTION_BIN="$OPTION_WORK/install/bin"
OPTION_CAPTAIN="$OPTION_WORK/install/captain"
mkdir -p "$OPTION_HOME" "$OPTION_CODEX" "$OPTION_CLAUDE"
cat > "$OPTION_CODEX/config.toml" <<EOF
[mcp_servers.t-hub]
command = "$OPTION_BIN/t-hub-mcp"
args = []
env = {}
env_vars = ["T_HUB_CONTROL_ADDR", "T_HUB_CONTROL_TOKEN", "T_HUB_SESSION_TOKEN"]
EOF
HOME="$OPTION_HOME" CODEX_HOME="$OPTION_CODEX" CLAUDE_HOME="$OPTION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$OPTION_BIN" \
  T_HUB_CAPTAIN_DIR="$OPTION_CAPTAIN" T_HUB_INSTALL_CRASH_AFTER_STAGE=binary \
  bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1 || true
if HOME="$OPTION_HOME" CODEX_HOME="$OPTION_CODEX" CLAUDE_HOME="$OPTION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$OPTION_BIN" \
  T_HUB_CAPTAIN_DIR="$OPTION_CAPTAIN" bash "$SCRIPT" >/dev/null 2>&1; then
  fail "changed migration option adopted an interrupted transaction"
elif [ -d "$OPTION_HOME/.t-hub/transactions/install-current" ]; then
  pass "interrupted transaction binds the migration option"
else
  fail "migration-option mismatch destroyed recovery state"
fi
if HOME="$OPTION_HOME" CODEX_HOME="$OPTION_CODEX" CLAUDE_HOME="$OPTION_CLAUDE" \
  T_HUB_MCP_SOURCE="$SOURCE" T_HUB_BIN_DIR="$OPTION_BIN" \
  T_HUB_CAPTAIN_DIR="$OPTION_CAPTAIN" \
  bash "$SCRIPT" --migrate-legacy-registration >/dev/null 2>&1 \
  && grep -Fq 'env_vars = ["T_HUB_CONTROL_FILE", "T_HUB_SESSION_TOKEN"]' \
    "$OPTION_CODEX/config.toml"; then
  pass "matching migration option recovers and converges"
else
  fail "matching migration option did not recover"
fi

SERIAL_WORK="$WORK/serialization"
SERIAL_HOME="$SERIAL_WORK/home"
SERIAL_CODEX="$SERIAL_WORK/codex"
SERIAL_CLAUDE="$SERIAL_WORK/claude"
SERIAL_BIN="$SERIAL_WORK/install/bin"
SERIAL_CAPTAIN="$SERIAL_WORK/install/captain"
SERIAL_WRAPPER="$SERIAL_WORK/wrapper"
mkdir -p "$SERIAL_HOME" "$SERIAL_CODEX" "$SERIAL_CLAUDE" "$SERIAL_WRAPPER"
cat > "$SERIAL_WRAPPER/claude" <<'EOF'
#!/usr/bin/env bash
if [ ! -e "$SERIAL_MARKER" ]; then
  : > "$SERIAL_MARKER"
  sleep 2
fi
exec "$REAL_CLAUDE" "$@"
EOF
chmod 700 "$SERIAL_WRAPPER/claude"
REAL_CLAUDE="$(command -v claude)"
HOME="$SERIAL_HOME" CODEX_HOME="$SERIAL_CODEX" CLAUDE_HOME="$SERIAL_CLAUDE" \
  PATH="$SERIAL_WRAPPER:$PATH" REAL_CLAUDE="$REAL_CLAUDE" \
  SERIAL_MARKER="$SERIAL_WORK/inside-lock" T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$SERIAL_BIN" T_HUB_CAPTAIN_DIR="$SERIAL_CAPTAIN" \
  bash "$SCRIPT" >/dev/null 2>&1 &
first_pid=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ -e "$SERIAL_WORK/inside-lock" ] && break
  sleep 0.1
done
HOME="$SERIAL_HOME" CODEX_HOME="$SERIAL_CODEX" CLAUDE_HOME="$SERIAL_CLAUDE" \
  PATH="$SERIAL_WRAPPER:$PATH" REAL_CLAUDE="$REAL_CLAUDE" \
  SERIAL_MARKER="$SERIAL_WORK/inside-lock" T_HUB_MCP_SOURCE="$SOURCE" \
  T_HUB_BIN_DIR="$SERIAL_BIN" T_HUB_CAPTAIN_DIR="$SERIAL_CAPTAIN" \
  bash "$SCRIPT" >/dev/null 2>&1 &
second_pid=$!
sleep 0.3
if kill -0 "$second_pid" 2>/dev/null; then
  pass "concurrent installer waits on the persistent install lock"
else
  fail "concurrent installer bypassed serialization"
fi
wait "$first_pid"
first_result=$?
wait "$second_pid"
second_result=$?
if [ "$first_result" -eq 0 ] && [ "$second_result" -eq 0 ] \
  && [ ! -e "$SERIAL_HOME/.t-hub/transactions/install-current" ]; then
  pass "serialized installers both converge without transaction residue"
else
  fail "serialized installers did not converge cleanly"
fi

if [ "$FAILED" -eq 0 ]; then
  echo "install-thub-codex.test: PASS"
else
  echo "install-thub-codex.test: FAIL" >&2
fi
exit "$FAILED"
