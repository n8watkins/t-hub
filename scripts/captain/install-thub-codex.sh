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

if ! command -v flock >/dev/null 2>&1; then
  echo "install-thub-codex: flock is required for safe installation" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "install-thub-codex: jq is required to verify the MCP catalog" >&2
  exit 1
fi

install -d -m 700 "$BIN_DIR" "$CAPTAIN_DIR"
exec 8>"$CAPTAIN_DIR/install.lock"
flock -x 8

ATOMIC_SOURCE="$HERE/atomic-config.py"
TRANSACTION_ROOT="${T_HUB_TRANSACTION_ROOT:-${HOME}/.t-hub/transactions}"
TXN="$TRANSACTION_ROOT/install-current"
SKILLS_SOURCE="${T_HUB_SKILLS_SOURCE:-$REPO_ROOT/skills}"
CODEX_SKILLS_DEST="${T_HUB_CODEX_SKILLS_DIR:-${CODEX_HOME:-${HOME}/.codex}/skills}"
CLAUDE_SKILLS_DEST="${T_HUB_CLAUDE_SKILLS_DIR:-${CLAUDE_HOME:-${HOME}/.claude}/skills}"
CLAUDE_COMMANDS_DEST="${T_HUB_CLAUDE_COMMANDS_DIR:-${CLAUDE_HOME:-${HOME}/.claude}/commands}"
install -d -m 700 "$TRANSACTION_ROOT"
if [ "$(stat -c %u "$TRANSACTION_ROOT")" != "$(id -u)" ] \
  || [ "$(stat -c %a "$TRANSACTION_ROOT")" != 700 ]; then
  echo "install-thub-codex: transaction root must be current-user owned with mode 0700" >&2
  exit 1
fi

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

verify_cortana_catalog() {
  local binary="$1" catalog
  catalog="$("$binary" --list-tools 2>/dev/null)" || return 1
  printf '%s' "$catalog" | jq -e '
  [(.tools // .)[] | select(.name == "cortana_bootstrap")]
  | length == 1
    and .[0].inputSchema == {"type":"object","properties":{},"additionalProperties":false}
    and .[0].annotations["t-hubTier"] == "read"
    and .[0].annotations.confirmationRequired == false
    and .[0].annotations.readOnlyHint == true
    and .[0].annotations.destructiveHint == false
    and .[0].annotations.idempotentHint == true
    and .[0].annotations.openWorldHint == false
' >/dev/null
}

if [ ! -x "$SOURCE" ] || [ ! -f "$SOURCE" ] || [ -L "$SOURCE" ]; then
  echo "install-thub-codex: source binary must be an executable regular file, not a symlink: $SOURCE" >&2
  exit 1
fi
SOURCE_CANONICAL="$(readlink -f "$SOURCE")"
exec 5<"$SOURCE"
SOURCE_DEVICE="$(stat -Lc %d "/proc/$$/fd/5")"
SOURCE_INODE="$(stat -Lc %i "/proc/$$/fd/5")"
SOURCE_DIGEST="$(sha256sum "/proc/$$/fd/5" | awk '{print $1}')"
if [ "$(stat -Lc %d "$SOURCE")" != "$SOURCE_DEVICE" ] \
  || [ "$(stat -Lc %i "$SOURCE")" != "$SOURCE_INODE" ]; then
  echo "install-thub-codex: source binary changed while it was selected: $SOURCE" >&2
  exit 1
fi
SOURCE_SNAPSHOT_DIR="$(mktemp -d "$TRANSACTION_ROOT/.source-snapshot.XXXXXX")"
chmod 700 "$SOURCE_SNAPSHOT_DIR"
SOURCE_SNAPSHOT="$SOURCE_SNAPSHOT_DIR/t-hub-mcp"
cleanup_source_snapshot() {
  if [ -n "${SOURCE_SNAPSHOT:-}" ] && [ -f "$SOURCE_SNAPSHOT" ]; then
    rm -f -- "$SOURCE_SNAPSHOT"
  fi
  if [ -n "${SOURCE_SNAPSHOT_DIR:-}" ] && [ -d "$SOURCE_SNAPSHOT_DIR" ]; then
    rmdir -- "$SOURCE_SNAPSHOT_DIR"
  fi
  SOURCE_SNAPSHOT=
  SOURCE_SNAPSHOT_DIR=
}
trap cleanup_source_snapshot EXIT
if [ -n "${T_HUB_INSTALL_SOURCE_PAUSE_DIR:-}" ]; then
  printf 'selected\n' > "$T_HUB_INSTALL_SOURCE_PAUSE_DIR/discovered"
  source_wait_count=0
  while [ "$source_wait_count" -lt 1000 ]; do
    [ ! -e "$T_HUB_INSTALL_SOURCE_PAUSE_DIR/resume" ] || break
    sleep 0.01
    source_wait_count=$((source_wait_count + 1))
  done
  if [ ! -e "$T_HUB_INSTALL_SOURCE_PAUSE_DIR/resume" ]; then
    echo "install-thub-codex: timed out at the source-selection test boundary" >&2
    exit 1
  fi
fi
install -m 700 "/proc/$$/fd/5" "$SOURCE_SNAPSHOT"
source_matches_selection() {
  [ ! -L "$SOURCE" ] \
    && [ "$(readlink -f "$SOURCE")" = "$SOURCE_CANONICAL" ] \
    && [ "$(stat -Lc %d "$SOURCE")" = "$SOURCE_DEVICE" ] \
    && [ "$(stat -Lc %i "$SOURCE")" = "$SOURCE_INODE" ] \
    && [ "$(sha256sum "$SOURCE" | awk '{print $1}')" = "$SOURCE_DIGEST" ]
}
if [ "$(sha256sum "$SOURCE_SNAPSHOT" | awk '{print $1}')" != "$SOURCE_DIGEST" ] \
  || ! source_matches_selection; then
  echo "install-thub-codex: source binary changed before its private snapshot was verified: $SOURCE" >&2
  exit 1
fi
if ! verify_cortana_catalog "$SOURCE_SNAPSHOT"; then
  echo "install-thub-codex: source binary lacks the exact cortana_bootstrap catalog contract: $SOURCE" >&2
  exit 1
fi

# Refuse every known skill conflict before replacing the MCP binary or changing
# Codex registration. The installer repeats validation inside its own
# transaction to cover races between preflight and commit.
bash "$HERE/install-captain-skills.sh" --check "${SKILL_ARGS[@]}"

describe_digest() {
  if [ -f "$1" ]; then
    python3 "$ATOMIC_SOURCE" describe --path "$1" | jq -r .digest
  else
    printf 'absent\n'
  fi
}

integration_source_digest() {
  {
    for source in "$HERE/atomic-config.py" "$HERE/ensure-thub-codex.sh" \
      "$HERE/ensure-thub-claude.sh" "$HERE/install-captain-skills.sh" \
      "$HERE/install-thub-codex.sh"; do
      printf 'f\0%s\0%s\0' "$(basename "$source")" "$(stat -c %a "$source")"
      sha256sum "$source"
    done
    (
      cd "$SKILLS_SOURCE"
      printf 'd\0.\0%s\0' "$(stat -c %a .)"
      find . -mindepth 1 -print0 | sort -z | while IFS= read -r -d '' entry; do
        if [ -L "$entry" ]; then
          printf 'l\0%s\0%s\0%s\0' "$entry" "$(stat -c %a "$entry")" "$(readlink "$entry")"
        elif [ -d "$entry" ]; then
          printf 'd\0%s\0%s\0' "$entry" "$(stat -c %a "$entry")"
        elif [ -f "$entry" ]; then
          printf 'f\0%s\0%s\0' "$entry" "$(stat -c %a "$entry")"
          sha256sum "$entry"
        else
          echo "install-thub-codex: unsupported skill source path: $SKILLS_SOURCE/$entry" >&2
          exit 1
        fi
      done
    )
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
  local op intent operation candidate outcome candidate_digest candidate_device candidate_inode
  local expected_digest expected_device expected_inode cleanup_mode name
  for op in "$TXN"/ops/* "$CODEX_CONFIG".t-hub-*.journal "$CLAUDE_CONFIG".t-hub-*.journal; do
    [ -d "$op" ] || continue
    intent="$(cat "$op/intent.json")"
    operation="$(printf '%s' "$intent" | jq -r .operation)"
    candidate="$(printf '%s' "$intent" | jq -r .candidate)"
    outcome="$(python3 "$ATOMIC_SOURCE" recover --journal "$op" --keep-journal)" || return 1
    if [ "$operation" = exchange ] && [ -f "$candidate" ]; then
      candidate_digest="$(describe_digest "$candidate")"
      candidate_device="$(stat -c %d "$candidate")"
      candidate_inode="$(stat -c %i "$candidate")"
      if [ "$outcome" = committed ]; then
        expected_digest="$(printf '%s' "$intent" | jq -r .expected.digest)"
        expected_device="$(printf '%s' "$intent" | jq -r .recovery.target_identity.device)"
        expected_inode="$(printf '%s' "$intent" | jq -r .recovery.target_identity.inode)"
      else
        expected_digest="$(printf '%s' "$intent" | jq -r .desired.digest)"
        expected_device="$(printf '%s' "$intent" | jq -r .recovery.candidate_identity.device)"
        expected_inode="$(printf '%s' "$intent" | jq -r .recovery.candidate_identity.inode)"
      fi
      if [ "$candidate_digest" != "$expected_digest" ] \
        || [ "$candidate_device" != "$expected_device" ] \
        || [ "$candidate_inode" != "$expected_inode" ]; then
        echo "install-thub-codex: atomic recovery candidate ownership changed: $candidate" >&2
        return 1
      fi
      name="$(basename "$op")"
      cleanup_mode=scrub
      case "$name" in
        binary|codex-helper|claude-helper|atomic-helper|rollback-binary|rollback-codex-helper|rollback-claude-helper|rollback-atomic-helper)
          cleanup_mode=release
          ;;
      esac
      if [ "$cleanup_mode" = release ]; then
        python3 "$ATOMIC_SOURCE" release --path "$candidate" \
          --expected-digest "$expected_digest" --expected-device "$expected_device" \
          --expected-inode "$expected_inode" || return 1
      else
        python3 "$ATOMIC_SOURCE" discard --path "$candidate" || return 1
      fi
    fi
    python3 "$ATOMIC_SOURCE" recover --journal "$op" >/dev/null || return 1
  done
}

release_file_stage_candidate() {
  local name="$1" stage candidate live_digest live_device live_inode
  local before_digest before_device before_inode desired_digest desired_device desired_inode
  [ -f "$TXN/stages/$name.json" ] || return 0
  stage="$(cat "$TXN/stages/$name.json")"
  candidate="$(printf '%s' "$stage" | jq -r '.candidate // empty')"
  [ -n "$candidate" ] && [ -f "$candidate" ] || return 0
  live_digest="$(describe_digest "$candidate")"
  live_device="$(stat -c %d "$candidate")"
  live_inode="$(stat -c %i "$candidate")"
  before_digest="$(printf '%s' "$stage" | jq -r .before.digest)"
  before_device="$(printf '%s' "$stage" | jq -r '.before_identity.device // empty')"
  before_inode="$(printf '%s' "$stage" | jq -r '.before_identity.inode // empty')"
  desired_digest="$(printf '%s' "$stage" | jq -r .desired.digest)"
  desired_device="$(printf '%s' "$stage" | jq -r .candidate_identity.device)"
  desired_inode="$(printf '%s' "$stage" | jq -r .candidate_identity.inode)"
  if [ "$live_digest" = "$before_digest" ] && [ "$live_device" = "$before_device" ] \
    && [ "$live_inode" = "$before_inode" ]; then
    :
  elif [ "$live_digest" = "$desired_digest" ] && [ "$live_device" = "$desired_device" ] \
    && [ "$live_inode" = "$desired_inode" ]; then
    :
  else
    echo "install-thub-codex: executable stage cleanup ownership changed: $candidate" >&2
    return 1
  fi
  python3 "$ATOMIC_SOURCE" release --path "$candidate" --expected-digest "$live_digest" \
    --expected-device "$live_device" --expected-inode "$live_inode"
}

rollback_file_stage() {
  local name="$1" stage target live desired before_presence before_digest recovery candidate
  local candidate_digest candidate_device candidate_inode
  local -a delete_args
  [ -f "$TXN/stages/$name.json" ] || return 0
  stage="$(cat "$TXN/stages/$name.json")"
  target="$(printf '%s' "$stage" | jq -r .target)"
  desired="$(printf '%s' "$stage" | jq -r .desired.digest)"
  before_presence="$(printf '%s' "$stage" | jq -r .before.presence)"
  before_digest="$(printf '%s' "$stage" | jq -r .before.digest)"
  live="$(describe_digest "$target")"
  if [ "$live" != "$before_digest" ]; then
    if [ "$live" != "$desired" ]; then
      echo "install-thub-codex: $target left helper ownership; recovery refused" >&2
      return 1
    fi
    if [ "$before_presence" = absent ]; then
      delete_args=()
      case "$name" in
        binary|codex-helper|claude-helper|atomic-helper) delete_args=(--unlink-only) ;;
      esac
      python3 "$ATOMIC_SOURCE" delete --target "$target" --expected-digest "$live" \
        --journal "$TXN/ops/rollback-$name" "${delete_args[@]}" || return 1
    else
      recovery="$(printf '%s' "$stage" | jq -r --arg fallback "$TXN/recovery/$name.bin" '.recovery // $fallback')"
      candidate="$(mktemp "$(dirname "$target")/.t-hub-rollback.XXXXXX")"
      python3 "$ATOMIC_SOURCE" discard --path "$candidate" || return 1
      python3 "$ATOMIC_SOURCE" materialize --recovery "$recovery" \
        --candidate "$candidate" || return 1
      python3 "$ATOMIC_SOURCE" install --target "$target" --candidate "$candidate" \
        --expected-digest "$live" --preserve-candidate-metadata \
        --journal "$TXN/ops/rollback-$name" || return 1
      candidate_digest="$(describe_digest "$candidate")"
      candidate_device="$(stat -c %d "$candidate")"
      candidate_inode="$(stat -c %i "$candidate")"
      case "$name" in
        binary|codex-helper|claude-helper|atomic-helper)
          python3 "$ATOMIC_SOURCE" release --path "$candidate" \
            --expected-digest "$candidate_digest" --expected-device "$candidate_device" \
            --expected-inode "$candidate_inode" || return 1
          ;;
        *) python3 "$ATOMIC_SOURCE" discard --path "$candidate" || return 1 ;;
      esac
    fi
  fi
  release_file_stage_candidate "$name" || return 1
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

adopt_interrupted_codex_boundary() {
  local state live before current post
  [ -f "$TXN/helper-state/codex-state.json" ] || return 1
  state="$(cat "$TXN/helper-state/codex-state.json")"
  [ "$(printf '%s' "$state" | jq -r .status)" = before ] || return 0
  exec 6>"${CODEX_CONFIG}.t-hub.lock"
  flock -x 6
  live="$(describe_digest "$CODEX_CONFIG")"
  before="$(printf '%s' "$state" | jq -r .before.digest)"
  if [ "$live" != "$before" ]; then
    current="$(CODEX_HOME="$(dirname "$CODEX_CONFIG")" codex mcp get t-hub --json 2>/dev/null || true)"
    if [ -z "$current" ] || ! printf '%s' "$current" | jq -e --arg bin "$DEST" '
      .enabled == true and .disabled_reason == null and
      .transport.type == "stdio" and .transport.command == $bin and
      .transport.args == [] and (.transport.env == null or .transport.env == {}) and
      .transport.env_vars == ["T_HUB_CONTROL_FILE","T_HUB_SESSION_TOKEN"] and
      .transport.cwd == null and .enabled_tools == null and .disabled_tools == null and
      .startup_timeout_sec == null and .tool_timeout_sec == null
    ' >/dev/null; then
      flock -u 6
      echo "install-thub-codex: interrupted Codex helper has no adoptable owned poststate" >&2
      return 1
    fi
  fi
  if [ -f "$CODEX_CONFIG" ]; then
    post="$(python3 "$ATOMIC_SOURCE" describe --path "$CODEX_CONFIG")"
    post="$(printf '%s' "$post" | jq -c '{presence:"present",digest:.digest,description:.description}')"
  else
    post='{"presence":"absent","digest":"absent"}'
  fi
  state="$(printf '%s' "$state" | jq --argjson post "$post" '.status="committed" | .post=$post')"
  python3 "$ATOMIC_SOURCE" publish --path "$TXN/helper-state/codex-state.json" --value "$state"
  flock -u 6
}

rollback_transaction() {
  recover_atomic_ops || return 1
  if [ -d "$TXN/skills" ]; then
    T_HUB_SKILL_TRANSACTION_DIR="$TXN/skills" T_HUB_SKILL_RECOVER_ONLY=1 \
      T_HUB_ATOMIC_CONFIG_HELPER="$ATOMIC_SOURCE" \
      bash "$HERE/install-captain-skills.sh" || return 1
  fi
  if [ "$(jq -r .status "$TXN/manifest.json" 2>/dev/null)" = claude-running ]; then
    adopt_interrupted_claude_boundary || return 1
  fi
  if [ "$(jq -r .status "$TXN/manifest.json" 2>/dev/null)" = codex-running ]; then
    adopt_interrupted_codex_boundary || return 1
    codex_stage="$(jq --arg recovery "$TXN/helper-state/codex-before.bin" \
      '{status:"applied",target:.target,before:.before,desired:.post,recovery:$recovery}' \
      "$TXN/helper-state/codex-state.json")" || return 1
    write_stage codex-config "$codex_stage" || return 1
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
  if ! jq -e --arg source "$SOURCE_CANONICAL" \
    --arg source_digest "$SOURCE_DIGEST" \
    --arg integration_digest "$(integration_source_digest)" \
    --argjson repair "$( [ "${SKILL_ARGS[*]:-}" = --repair ] && printf true || printf false )" \
    --argjson migrate "$( [ "${CODEX_ARGS[*]:-}" = --migrate-legacy-registration ] && printf true || printf false )" \
    --arg dest "$DEST" --arg captain_dir "$CAPTAIN_DIR" \
    --arg skills_source "$(readlink -f "$SKILLS_SOURCE")" \
    --arg codex_skills_dest "$CODEX_SKILLS_DEST" \
    --arg claude_skills_dest "$CLAUDE_SKILLS_DEST" \
    --arg claude_commands_dest "$CLAUDE_COMMANDS_DEST" \
    --arg codex_config "$CODEX_CONFIG" --arg claude_config "$CLAUDE_CONFIG" '
      .source == $source and .source_digest == $source_digest and
      .integration_digest == $integration_digest and
      .repair_skills == $repair and .migrate_legacy == $migrate and
      .dest == $dest and .captain_dir == $captain_dir and
      .skills_source == $skills_source and .codex_skills_dest == $codex_skills_dest and
      .claude_skills_dest == $claude_skills_dest and
      .claude_commands_dest == $claude_commands_dest and
      .codex_config == $codex_config and .claude_config == $claude_config
    ' "$TXN/manifest.json" >/dev/null; then
    echo "install-thub-codex: interrupted transaction provenance does not match this invocation" >&2
    return 1
  fi
  recover_atomic_ops
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
  if ! rollback_transaction; then
    echo "install-thub-codex: interrupted transaction recovery safely refused; original journal retained" >&2
    return 1
  fi
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
  --arg source "$SOURCE_CANONICAL" \
  --arg source_digest "$SOURCE_DIGEST" \
  --arg dest "$DEST" --arg captain_dir "$CAPTAIN_DIR" \
  --arg skills_source "$(readlink -f "$SKILLS_SOURCE")" \
  --arg codex_skills_dest "$CODEX_SKILLS_DEST" \
  --arg claude_skills_dest "$CLAUDE_SKILLS_DEST" \
  --arg claude_commands_dest "$CLAUDE_COMMANDS_DEST" \
  --arg codex_config "$CODEX_CONFIG" --arg claude_config "$CLAUDE_CONFIG" \
  '{version:1,status:$status,repair_skills:$repair,migrate_legacy:$migrate,
    integration_digest:$integration_digest,source:$source,
    source_digest:$source_digest,dest:$dest,captain_dir:$captain_dir,
    skills_source:$skills_source,codex_skills_dest:$codex_skills_dest,
    claude_skills_dest:$claude_skills_dest,claude_commands_dest:$claude_commands_dest,
    codex_config:$codex_config,claude_config:$claude_config}')"
python3 "$ATOMIC_SOURCE" publish --path "$TXN/manifest.json" --value "$manifest"

install_file_stage() {
  local source="$1" target="$2" name="$3" before candidate desired stage
  local before_identity candidate_identity
  install -d -m 700 "$(dirname "$target")"
  before="$(python3 "$ATOMIC_SOURCE" capture --source "$target" \
    --recovery "$TXN/recovery/$name.bin")"
  candidate="$(mktemp "$(dirname "$target")/.t-hub-stage.XXXXXX")"
  install -m 700 "$source" "$candidate"
  desired="$(python3 "$ATOMIC_SOURCE" describe --path "$candidate")"
  before_identity=null
  if [ -f "$target" ]; then
    before_identity="$(jq -cn --arg device "$(stat -c %d "$target")" \
      --arg inode "$(stat -c %i "$target")" '{device:$device,inode:$inode}')"
  fi
  candidate_identity="$(jq -cn --arg device "$(stat -c %d "$candidate")" \
    --arg inode "$(stat -c %i "$candidate")" '{device:$device,inode:$inode}')"
  stage="$(jq -cn --arg target "$target" --argjson before "$before" \
    --arg candidate "$candidate" --argjson desired "$desired" \
    --argjson before_identity "$before_identity" --argjson candidate_identity "$candidate_identity" \
    '{status:"prepared",target:$target,candidate:$candidate,before:$before,desired:$desired,
      before_identity:$before_identity,candidate_identity:$candidate_identity}')"
  write_stage "$name" "$stage"
  python3 "$ATOMIC_SOURCE" install --target "$target" --candidate "$candidate" \
    --expected-digest "$(printf '%s' "$before" | jq -r .digest)" \
    --preserve-candidate-metadata \
    --journal "$TXN/ops/$name"
  release_file_stage_candidate "$name"
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
  cleanup_source_snapshot
  exit "$code"
}
trap rollback_on_exit EXIT

if ! source_matches_selection; then
  echo "install-thub-codex: source binary changed before atomic installation" >&2
  exit 1
fi
install_file_stage "$SOURCE_SNAPSHOT" "$DEST" binary
if [ "$(sha256sum "$DEST" | awk '{print $1}')" != "$SOURCE_DIGEST" ]; then
  echo "install-thub-codex: installed binary digest differs from the verified source snapshot" >&2
  exit 1
fi
if ! verify_cortana_catalog "$DEST"; then
  echo "install-thub-codex: installed binary lacks the exact cortana_bootstrap catalog contract: $DEST" >&2
  exit 1
fi
cleanup_source_snapshot
exec 5<&-
install_file_stage "$HERE/ensure-thub-codex.sh" "$CAPTAIN_DIR/ensure-thub-codex.sh" codex-helper
install_file_stage "$HERE/ensure-thub-claude.sh" "$CAPTAIN_DIR/ensure-thub-claude.sh" claude-helper
install_file_stage "$HERE/atomic-config.py" "$CAPTAIN_DIR/atomic-config.py" atomic-helper

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
