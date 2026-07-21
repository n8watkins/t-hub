#!/usr/bin/env bash
# Build, transactionally install, and register the WSL-side T-Hub integration.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
MANIFEST="$REPO_ROOT/apps/desktop/src-tauri/Cargo.toml"
BIN_DIR="${T_HUB_BIN_DIR:-${HOME}/.t-hub/bin}"
CAPTAIN_DIR="${T_HUB_CAPTAIN_DIR:-${HOME}/.t-hub/captain}"
DEST="$BIN_DIR/t-hub-mcp"
CODEX_CONFIG="${CODEX_HOME:-${HOME}/.codex}/config.toml"
CLAUDE_CONFIG="${HOME}/.claude.json"
SKILL_ARGS=()
CODEX_ARGS=()
REPAIR_SEEN=false
MIGRATE_SEEN=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repair-skills)
      if "$REPAIR_SEEN"; then
        echo "install-thub-codex: duplicate --repair-skills" >&2
        exit 2
      fi
      REPAIR_SEEN=true
      SKILL_ARGS=(--repair)
      ;;
    --migrate-legacy-registration)
      if "$MIGRATE_SEEN"; then
        echo "install-thub-codex: duplicate --migrate-legacy-registration" >&2
        exit 2
      fi
      MIGRATE_SEEN=true
      CODEX_ARGS=(--migrate-legacy-registration)
      ;;
    *)
      echo "usage: install-thub-codex.sh [--repair-skills] [--migrate-legacy-registration]" >&2
      exit 2
      ;;
  esac
  shift
done

if [ -n "${T_HUB_MCP_SOURCE:-}" ]; then
  SOURCE="$T_HUB_MCP_SOURCE"
else
  if ! command -v cargo >/dev/null 2>&1; then
    echo "install-thub-codex: cargo is required to build t-hub-mcp" >&2
    exit 1
  fi
  cargo build --release -p t-hub-mcp --manifest-path "$MANIFEST"
  SOURCE="$REPO_ROOT/apps/desktop/src-tauri/target/release/t-hub-mcp"
fi

if [ ! -x "$SOURCE" ]; then
  echo "install-thub-codex: source binary is not executable: $SOURCE" >&2
  exit 1
fi
if ! command -v flock >/dev/null 2>&1; then
  echo "install-thub-codex: flock is required for safe installation" >&2
  exit 1
fi

if ! "$SOURCE" --list-tools >/dev/null 2>&1; then
  echo "install-thub-codex: source binary failed its offline catalog probe: $SOURCE" >&2
  exit 1
fi

# Refuse every known skill conflict before replacing the MCP binary or changing
# Codex registration. The installer repeats validation inside its own
# transaction to cover races between preflight and commit.
bash "$HERE/install-captain-skills.sh" --check "${SKILL_ARGS[@]}"

install -d -m 700 "$BIN_DIR" "$CAPTAIN_DIR"
exec 8>"$CAPTAIN_DIR/install.lock"
flock -x 8

ATOMIC_SOURCE="$HERE/atomic-config.py"
TRANSACTION_ROOT="${T_HUB_TRANSACTION_ROOT:-${HOME}/.t-hub/transactions}"
TXN="$TRANSACTION_ROOT/install-current"
install -d -m 700 "$TRANSACTION_ROOT"
if [ "$(stat -c %u "$TRANSACTION_ROOT")" != "$(id -u)" ] \
  || [ "$(stat -c %a "$TRANSACTION_ROOT")" != 700 ]; then
  echo "install-thub-codex: transaction root must be current-user owned with mode 0700" >&2
  exit 1
fi

describe_digest() {
  if [ -f "$1" ]; then
    python3 "$ATOMIC_SOURCE" describe --path "$1" | jq -r .digest
  else
    printf 'absent\n'
  fi
}

integration_source_digest() {
  {
    sha256sum "$HERE/atomic-config.py" "$HERE/ensure-thub-codex.sh" \
      "$HERE/ensure-thub-claude.sh" "$HERE/install-captain-skills.sh" \
      "$HERE/install-thub-codex.sh"
    find "$REPO_ROOT/skills" -type f -print0 | sort -z | xargs -0 sha256sum
  } | sha256sum | awk '{print $1}'
}

publish_manifest_status() {
  local status="$1"
  manifest="$(jq --arg status "$status" '.status=$status' "$TXN/manifest.json")"
  python3 "$ATOMIC_SOURCE" publish --path "$TXN/manifest.json" --value "$manifest"
}

write_stage() {
  local name="$1" value="$2"
  python3 "$ATOMIC_SOURCE" publish --path "$TXN/stages/$name.json" --value "$value"
}

recover_atomic_ops() {
  local op
  for op in "$TXN"/ops/* "$CODEX_CONFIG".t-hub-*.journal "$CLAUDE_CONFIG".t-hub-*.journal; do
    [ -d "$op" ] || continue
    python3 "$ATOMIC_SOURCE" recover --journal "$op" >/dev/null
  done
}

rollback_file_stage() {
  local name="$1" stage target live desired before_presence before_digest recovery candidate
  [ -f "$TXN/stages/$name.json" ] || return 0
  stage="$(cat "$TXN/stages/$name.json")"
  target="$(printf '%s' "$stage" | jq -r .target)"
  desired="$(printf '%s' "$stage" | jq -r .desired.digest)"
  before_presence="$(printf '%s' "$stage" | jq -r .before.presence)"
  before_digest="$(printf '%s' "$stage" | jq -r .before.digest)"
  live="$(describe_digest "$target")"
  if [ "$live" = "$before_digest" ]; then return 0; fi
  if [ "$live" != "$desired" ]; then
    echo "install-thub-codex: $target left helper ownership; recovery refused" >&2
    return 1
  fi
  if [ "$before_presence" = absent ]; then
    python3 "$ATOMIC_SOURCE" delete --target "$target" --expected-digest "$live" \
      --journal "$TXN/ops/rollback-$name"
  else
    recovery="$(printf '%s' "$stage" | jq -r --arg fallback "$TXN/recovery/$name.bin" '.recovery // $fallback')"
    candidate="$(mktemp "$(dirname "$target")/.t-hub-rollback.XXXXXX")"
    python3 "$ATOMIC_SOURCE" discard --path "$candidate"
    python3 "$ATOMIC_SOURCE" materialize --recovery "$recovery" --candidate "$candidate"
    python3 "$ATOMIC_SOURCE" install --target "$target" --candidate "$candidate" \
      --expected-digest "$live" --preserve-candidate-metadata \
      --journal "$TXN/ops/rollback-$name"
    python3 "$ATOMIC_SOURCE" discard --path "$candidate"
  fi
}

adopt_interrupted_claude_boundary() {
  local state live before post structure node_digest
  [ -f "$TXN/helper-state/claude-state.json" ] || return 1
  state="$(cat "$TXN/helper-state/claude-state.json")"
  [ "$(printf '%s' "$state" | jq -r .status)" = before ] || return 0
  exec 7>"${CLAUDE_CONFIG}.t-hub.lock"
  flock -x 7
  live="$(describe_digest "$CLAUDE_CONFIG")"
  before="$(printf '%s' "$state" | jq -r .before_file.digest)"
  if [ "$live" != "$before" ]; then
    if [ ! -f "$CLAUDE_CONFIG" ] || ! jq -e --arg bin "$DEST" '
      (.mcpServers|type) == "object" and
      .mcpServers["t-hub"] == {type:"stdio",command:$bin,args:[],env:{}}
    ' "$CLAUDE_CONFIG" >/dev/null; then
      flock -u 7
      echo "install-thub-codex: interrupted Claude helper has no adoptable owned poststate" >&2
      return 1
    fi
  fi
  if [ -f "$CLAUDE_CONFIG" ]; then
    post="$(python3 "$ATOMIC_SOURCE" describe --path "$CLAUDE_CONFIG")"
    post="$(printf '%s' "$post" | jq -c '{presence:"present",digest:.digest,description:.description}')"
    node_digest=absent
    if jq -e '(.mcpServers|type)=="object" and (.mcpServers|has("t-hub"))' \
      "$CLAUDE_CONFIG" >/dev/null; then
      node_digest="$(jq -Sc '.mcpServers["t-hub"]' "$CLAUDE_CONFIG" | sha256sum | awk '{print $1}')"
    fi
    structure="$(jq -Sc --arg node_digest "$node_digest" '{
      file_presence:"present",
      parent:{presence:has("mcpServers"),type:(if has("mcpServers") then (.mcpServers|type) else "absent" end)},
      key:{presence:(if (.mcpServers|type)=="object" then (.mcpServers|has("t-hub")) else false end),
        type:(if (.mcpServers|type)=="object" and (.mcpServers|has("t-hub")) then (.mcpServers["t-hub"]|type) else "absent" end),digest:$node_digest}
    }' "$CLAUDE_CONFIG")"
  else
    post='{"presence":"absent","digest":"absent"}'
    structure='{"file_presence":"absent","parent":{"presence":false,"type":"absent"},"key":{"presence":false,"type":"absent","digest":"absent"}}'
  fi
  state="$(printf '%s' "$state" | jq --argjson post "$post" --argjson structure "$structure" \
    '.status="committed" | .post=$post | .post_structure=$structure')"
  python3 "$ATOMIC_SOURCE" publish --path "$TXN/helper-state/claude-state.json" --value "$state"
  flock -u 7
}

rollback_transaction() {
  set +e
  recover_atomic_ops || return 1
  if [ -d "$TXN/skills" ]; then
    T_HUB_SKILL_TRANSACTION_DIR="$TXN/skills" T_HUB_SKILL_RECOVER_ONLY=1 \
      T_HUB_ATOMIC_CONFIG_HELPER="$ATOMIC_SOURCE" \
      bash "$HERE/install-captain-skills.sh" || return 1
  fi
  if [ "$(jq -r .status "$TXN/manifest.json" 2>/dev/null)" = claude-running ]; then
    adopt_interrupted_claude_boundary || return 1
  fi
  if [ -f "$TXN/stages/codex-config.json" ]; then
    rollback_file_stage codex-config || return 1
  fi
  if [ -f "$TXN/helper-state/claude-state.json" ]; then
    live="$(describe_digest "$CLAUDE_CONFIG")"
    post="$(jq -r .post.digest "$TXN/helper-state/claude-state.json")"
    before="$(jq -r .before_file.digest "$TXN/helper-state/claude-state.json")"
    if [ "$live" != "$before" ]; then
      if ! python3 "$ATOMIC_SOURCE" claude-rollback --target "$CLAUDE_CONFIG" \
        --state "$TXN/helper-state/claude-state.json" \
        --recovery "$TXN/helper-state/claude-before.bin" \
        --journal "$TXN/ops/rollback-claude-config"; then
        return 1
      fi
    fi
  fi
  rollback_file_stage atomic-helper || return 1
  rollback_file_stage claude-helper || return 1
  rollback_file_stage codex-helper || return 1
  rollback_file_stage binary || return 1
  python3 "$ATOMIC_SOURCE" purge --path "$TXN"
}

recover_previous_transaction() {
  [ -d "$TXN" ] || return 0
  if [ ! -f "$TXN/manifest.json" ]; then
    echo "install-thub-codex: incomplete transaction has no valid manifest" >&2
    return 1
  fi
  status="$(jq -r .status "$TXN/manifest.json")"
  if ! jq -e --arg source "$(readlink -f "$SOURCE")" \
    --arg source_digest "$(sha256sum "$SOURCE" | awk '{print $1}')" \
    --arg integration_digest "$(integration_source_digest)" \
    --argjson repair "$( [ "${SKILL_ARGS[*]:-}" = --repair ] && printf true || printf false )" \
    --argjson migrate "$( [ "${CODEX_ARGS[*]:-}" = --migrate-legacy-registration ] && printf true || printf false )" \
    --arg dest "$DEST" --arg captain_dir "$CAPTAIN_DIR" \
    --arg codex_config "$CODEX_CONFIG" --arg claude_config "$CLAUDE_CONFIG" '
      .source == $source and .source_digest == $source_digest and
      .integration_digest == $integration_digest and
      .repair_skills == $repair and .migrate_legacy == $migrate and
      .dest == $dest and .captain_dir == $captain_dir and
      .codex_config == $codex_config and .claude_config == $claude_config
    ' "$TXN/manifest.json" >/dev/null; then
    echo "install-thub-codex: interrupted transaction provenance does not match this invocation" >&2
    return 1
  fi
  recover_atomic_ops
  if [ "$status" = codex-running ]; then
    echo "install-thub-codex: helper stopped before publishing its post boundary; recovery refused" >&2
    return 1
  fi
  if [ "$status" = skills-running ] || [ "$status" = skills-applied ]; then
    repair="$(jq -r .repair_skills "$TXN/manifest.json")"
    recovery_args=()
    [ "$repair" != true ] || recovery_args=(--repair)
    T_HUB_SKILL_TRANSACTION_DIR="$TXN/skills" \
      T_HUB_ATOMIC_CONFIG_HELPER="$ATOMIC_SOURCE" \
      bash "$HERE/install-captain-skills.sh" "${recovery_args[@]}"
    python3 "$ATOMIC_SOURCE" purge --path "$TXN"
    echo "install-thub-codex: completed interrupted skills stage"
    return 0
  fi
  rollback_transaction
  echo "install-thub-codex: rolled back interrupted transaction"
}

recover_previous_transaction

install -d -m 700 "$TXN" "$TXN/stages" "$TXN/recovery" "$TXN/ops" "$TXN/helper-state"
repair_skills=false
[ "${SKILL_ARGS[*]:-}" != --repair ] || repair_skills=true
migrate_legacy=false
[ "${CODEX_ARGS[*]:-}" != --migrate-legacy-registration ] || migrate_legacy=true
manifest="$(jq -cn --arg status active --argjson repair "$repair_skills" \
  --argjson migrate "$migrate_legacy" --arg integration_digest "$(integration_source_digest)" \
  --arg source "$(readlink -f "$SOURCE")" \
  --arg source_digest "$(sha256sum "$SOURCE" | awk '{print $1}')" \
  --arg dest "$DEST" --arg captain_dir "$CAPTAIN_DIR" \
  --arg codex_config "$CODEX_CONFIG" --arg claude_config "$CLAUDE_CONFIG" \
  '{version:1,status:$status,repair_skills:$repair,migrate_legacy:$migrate,
    integration_digest:$integration_digest,source:$source,
    source_digest:$source_digest,dest:$dest,captain_dir:$captain_dir,
    codex_config:$codex_config,claude_config:$claude_config}')"
python3 "$ATOMIC_SOURCE" publish --path "$TXN/manifest.json" --value "$manifest"

install_file_stage() {
  local source="$1" target="$2" name="$3" before candidate desired stage
  install -d -m 700 "$(dirname "$target")"
  before="$(python3 "$ATOMIC_SOURCE" capture --source "$target" \
    --recovery "$TXN/recovery/$name.bin")"
  candidate="$(mktemp "$(dirname "$target")/.t-hub-stage.XXXXXX")"
  install -m 700 "$source" "$candidate"
  desired="$(python3 "$ATOMIC_SOURCE" describe --path "$candidate")"
  stage="$(jq -cn --arg target "$target" --argjson before "$before" \
    --argjson desired "$desired" \
    '{status:"prepared",target:$target,before:$before,desired:$desired}')"
  write_stage "$name" "$stage"
  python3 "$ATOMIC_SOURCE" install --target "$target" --candidate "$candidate" \
    --expected-digest "$(printf '%s' "$before" | jq -r .digest)" \
    --preserve-candidate-metadata \
    --journal "$TXN/ops/$name"
  if [ -f "$candidate" ]; then python3 "$ATOMIC_SOURCE" discard --path "$candidate"; fi
  stage="$(printf '%s' "$stage" | jq '.status="applied"')"
  write_stage "$name" "$stage"
  if [ "${T_HUB_INSTALL_CRASH_AFTER_STAGE:-}" = "$name" ]; then kill -KILL "$$"; fi
}

rollback_on_exit() {
  code=$?
  trap - EXIT
  if [ "$code" -ne 0 ] && ! rollback_transaction; then
    echo "install-thub-codex: rollback safely refused; rerun for deterministic recovery" >&2
  fi
  exit "$code"
}
trap rollback_on_exit EXIT

install_file_stage "$SOURCE" "$DEST" binary
install_file_stage "$HERE/ensure-thub-codex.sh" "$CAPTAIN_DIR/ensure-thub-codex.sh" codex-helper
install_file_stage "$HERE/ensure-thub-claude.sh" "$CAPTAIN_DIR/ensure-thub-claude.sh" claude-helper
install_file_stage "$HERE/atomic-config.py" "$CAPTAIN_DIR/atomic-config.py" atomic-helper
"$DEST" --list-tools >/dev/null

CHILD_ATOMIC_HELPER="${T_HUB_ATOMIC_CONFIG_HELPER:-$CAPTAIN_DIR/atomic-config.py}"
publish_manifest_status claude-running
T_HUB_MCP_BIN="$DEST" T_HUB_INSTALL_STATE_DIR="$TXN/helper-state" \
  T_HUB_ATOMIC_CONFIG_HELPER="$CHILD_ATOMIC_HELPER" "$CAPTAIN_DIR/ensure-thub-claude.sh"
if [ "$(describe_digest "$CLAUDE_CONFIG")" != \
  "$(jq -r .post.digest "$TXN/helper-state/claude-state.json")" ]; then
  echo "install-thub-codex: Claude helper poststate changed before validation" >&2
  exit 1
fi
publish_manifest_status claude-applied
if [ "${T_HUB_INSTALL_CRASH_AFTER_STAGE:-}" = claude-config ]; then kill -KILL "$$"; fi

publish_manifest_status codex-running
T_HUB_MCP_BIN="$DEST" T_HUB_INSTALL_STATE_DIR="$TXN/helper-state" \
  T_HUB_ATOMIC_CONFIG_HELPER="$CHILD_ATOMIC_HELPER" \
  "$CAPTAIN_DIR/ensure-thub-codex.sh" "${CODEX_ARGS[@]}"
if [ "$(describe_digest "$CODEX_CONFIG")" != \
  "$(jq -r .post.digest "$TXN/helper-state/codex-state.json")" ]; then
  echo "install-thub-codex: Codex helper poststate changed before validation" >&2
  exit 1
fi
codex_stage="$(jq --arg recovery "$TXN/helper-state/codex-before.bin" \
  '{status:"applied",target:.target,before:.before,desired:.post,recovery:$recovery}' \
  "$TXN/helper-state/codex-state.json")"
write_stage codex-config "$codex_stage"
publish_manifest_status codex-applied
if [ "${T_HUB_INSTALL_CRASH_AFTER_STAGE:-}" = codex-config ]; then kill -KILL "$$"; fi

publish_manifest_status skills-running
T_HUB_SKILL_TRANSACTION_DIR="$TXN/skills" \
  T_HUB_ATOMIC_CONFIG_HELPER="$ATOMIC_SOURCE" \
  bash "$HERE/install-captain-skills.sh" "${SKILL_ARGS[@]}"
publish_manifest_status skills-applied
if [ "${T_HUB_INSTALL_CRASH_AFTER_STAGE:-}" = skills ]; then kill -KILL "$$"; fi

trap - EXIT
python3 "$ATOMIC_SOURCE" purge --path "$TXN"

echo "install-thub-codex: installed $DEST"
echo "install-thub-codex: start new Codex and Claude sessions to load the updated integration"
