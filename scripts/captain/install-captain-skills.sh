#!/usr/bin/env bash
# Transactionally install T-Hub's cross-harness skills and Claude command wrappers.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SOURCE_ROOT="${T_HUB_SKILLS_SOURCE:-$REPO_ROOT/skills}"
CODEX_SKILLS="${T_HUB_CODEX_SKILLS_DIR:-${CODEX_HOME:-${HOME}/.codex}/skills}"
CLAUDE_SKILLS="${T_HUB_CLAUDE_SKILLS_DIR:-${CLAUDE_HOME:-${HOME}/.claude}/skills}"
CLAUDE_COMMANDS="${T_HUB_CLAUDE_COMMANDS_DIR:-${CLAUDE_HOME:-${HOME}/.claude}/commands}"
MODE=install
REPAIR=false
ATOMIC_HELPER="${T_HUB_ATOMIC_CONFIG_HELPER:-$HERE/atomic-config.py}"
SKILL_TXN="${T_HUB_SKILL_TRANSACTION_DIR:-}"
for arg in "$@"; do
  case "$arg" in
    --check) MODE=check ;;
    --verify) MODE=verify ;;
    --repair) REPAIR=true ;;
    *)
      echo "usage: install-captain-skills.sh [--check|--verify] [--repair]" >&2
      exit 2
      ;;
  esac
done
if [ "$MODE" = verify ] && "$REPAIR"; then
  echo "install-captain-skills: --verify and --repair cannot be combined" >&2
  exit 2
fi

tree_hash() {
  local root="$1"
  (
    cd "$root"
    find . -mindepth 1 ! -name .t-hub-managed -print0 \
      | sort -z \
      | while IFS= read -r -d '' entry; do
          if [ -L "$entry" ]; then
            printf 'l\0%s\0%s\0%s\0' "$entry" "$(stat -c '%a' "$entry")" "$(readlink "$entry")"
          elif [ -d "$entry" ]; then
            printf 'd\0%s\0%s\0' "$entry" "$(stat -c '%a' "$entry")"
          elif [ -f "$entry" ]; then
            printf 'f\0%s\0%s\0' "$entry" "$(stat -c '%a' "$entry")"
            sha256sum "$entry"
          else
            printf 'o\0%s\0%s\0' "$entry" "$(stat -c '%f' "$entry")"
          fi
        done
  ) | sha256sum | awk '{print $1}'
}

file_hash() {
  sha256sum "$1" | awk '{print $1}'
}

recorded_hash() {
  sed -n 's/^source-sha256=//p' "$1"
}

path_evidence() {
  local path="$1"
  if [ -L "$path" ]; then
    jq -cn --arg type symlink --arg device "$(stat -c %d "$path")" \
      --arg inode "$(stat -c %i "$path")" --arg mode "$(stat -c %a "$path")" \
      --arg digest "$(printf '%s' "$(readlink "$path")" | sha256sum | awk '{print $1}')" \
      '{presence:"present",type:$type,device:$device,inode:$inode,mode:$mode,digest:$digest}'
  elif [ -d "$path" ]; then
    jq -cn --arg type directory --arg device "$(stat -c %d "$path")" \
      --arg inode "$(stat -c %i "$path")" --arg mode "$(stat -c %a "$path")" \
      --arg digest "$(tree_hash "$path")" \
      '{presence:"present",type:$type,device:$device,inode:$inode,mode:$mode,digest:$digest}'
  elif [ -f "$path" ]; then
    jq -cn --arg type file --arg device "$(stat -c %d "$path")" \
      --arg inode "$(stat -c %i "$path")" --arg mode "$(stat -c %a "$path")" \
      --arg digest "$(file_hash "$path")" \
      '{presence:"present",type:$type,device:$device,inode:$inode,mode:$mode,digest:$digest}'
  elif [ -e "$path" ]; then
    echo "install-captain-skills: unsupported path type: $path" >&2
    return 1
  else
    printf '%s\n' '{"presence":"absent"}'
  fi
}

path_matches_evidence() {
  local path="$1" expected="$2" actual
  actual="$(path_evidence "$path")" || return 1
  [ "$(printf '%s' "$actual" | jq -cS .)" = "$(printf '%s' "$expected" | jq -cS .)" ]
}

recover_owned_path() {
  local target="$1" backup="$2" original="$3" staged="$4"
  if [ "$(printf '%s' "$original" | jq -r .presence)" = present ]; then
    if [ -e "$backup" ] || [ -L "$backup" ]; then
      if ! path_matches_evidence "$backup" "$original"; then
        echo "install-captain-skills: durable backup ownership changed: $backup" >&2
        return 1
      fi
      if [ -e "$target" ] || [ -L "$target" ]; then
        if ! path_matches_evidence "$target" "$staged"; then
          echo "install-captain-skills: target left installer ownership: $target" >&2
          return 1
        fi
        python3 "$ATOMIC_HELPER" purge --path "$target"
      fi
      mv "$backup" "$target"
      python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
    elif ! path_matches_evidence "$target" "$original"; then
      echo "install-captain-skills: original target and durable backup are unavailable: $target" >&2
      return 1
    fi
  else
    if [ -e "$backup" ] || [ -L "$backup" ]; then
      echo "install-captain-skills: unexpected backup for absent target: $target" >&2
      return 1
    fi
    if [ -e "$target" ] || [ -L "$target" ]; then
      if ! path_matches_evidence "$target" "$staged"; then
        echo "install-captain-skills: absent target was created concurrently: $target" >&2
        return 1
      fi
      python3 "$ATOMIC_HELPER" purge --path "$target"
    fi
  fi
}

recover_skill_transaction() {
  local entry index target backup sidecar_backup temp sidecar_temp status original staged
  local original_sidecar staged_sidecar
  [ -n "$SKILL_TXN" ] && [ -d "$SKILL_TXN" ] || return 0
  for ((index = 6; index >= 0; index--)); do
    entry="$SKILL_TXN/entries/$index.json"
    [ -f "$entry" ] || continue
    target="$(jq -r .target "$entry")"
    backup="$(jq -r .backup "$entry")"
    sidecar_backup="$(jq -r .sidecar_backup "$entry")"
    temp="$(jq -r .temp "$entry")"
    sidecar_temp="$(jq -r .sidecar_temp "$entry")"
    status="$(jq -r .status "$entry")"
    original="$(jq -c .original "$entry")"
    staged="$(jq -c .staged "$entry")"
    original_sidecar="$(jq -c .original_sidecar "$entry")"
    staged_sidecar="$(jq -c .staged_sidecar "$entry")"
    if [ "$status" != prepared ]; then
      recover_owned_path "$target" "$backup" "$original" "$staged" || return 1
    fi
    if [ "$(jq -r .kind "$entry")" = command ]; then
      case "$status" in
        sidecar-intent|sidecar-acquired|sidecar-installed|applied)
          recover_owned_path "$target.t-hub-managed" "$sidecar_backup" \
            "$original_sidecar" "$staged_sidecar" || return 1
          ;;
      esac
    fi
    if [ -e "$temp" ] || [ -L "$temp" ]; then
      python3 "$ATOMIC_HELPER" purge --path "$temp"
    fi
    if [ "$sidecar_temp" != null ] && { [ -e "$sidecar_temp" ] || [ -L "$sidecar_temp" ]; }; then
      python3 "$ATOMIC_HELPER" purge --path "$sidecar_temp"
    fi
  done
  python3 "$ATOMIC_HELPER" purge --path "$SKILL_TXN"
}

# A killed prior installer leaves this restricted journal in place.  Restore
# every recorded target before checking ownership or beginning a fresh copy.
recover_skill_transaction
if [ "${T_HUB_SKILL_RECOVER_ONLY:-0}" = 1 ]; then exit 0; fi

SOURCES=(
  "$SOURCE_ROOT/captain"
  "$SOURCE_ROOT/shipmate"
  "$SOURCE_ROOT/handoff"
  "$SOURCE_ROOT/captain"
  "$SOURCE_ROOT/shipmate"
  "$SOURCE_ROOT/handoff"
  "$SOURCE_ROOT/handoff/assets/claude-command.md"
)
TARGETS=(
  "$CODEX_SKILLS/captain"
  "$CODEX_SKILLS/shipmate"
  "$CODEX_SKILLS/handoff"
  "$CLAUDE_SKILLS/captain"
  "$CLAUDE_SKILLS/shipmate"
  "$CLAUDE_SKILLS/handoff"
  "$CLAUDE_COMMANDS/handoff.md"
)
KINDS=(
  directory
  directory
  directory
  directory
  directory
  directory
  command
)

for index in "${!SOURCES[@]}"; do
  source="${SOURCES[$index]}"
  target="${TARGETS[$index]}"
  kind="${KINDS[$index]}"
  if [ "$kind" = directory ]; then
    if [ ! -f "$source/SKILL.md" ]; then
      echo "install-captain-skills: missing source skill: $source" >&2
      exit 1
    fi
  elif [ ! -f "$source" ] || [ "$(head -n 1 "$source")" != '---' ]; then
    echo "install-captain-skills: invalid Claude command source: $source" >&2
    exit 1
  fi
  if [ -e "$target" ] || [ -L "$target" ]; then
    if [ "$kind" = directory ] && [ -L "$target" ] && [ "$(readlink -f "$target")" = "$(readlink -f "$source")" ]; then
      continue
    fi
    marker="$target/.t-hub-managed"
    [ "$kind" = command ] && marker="$target.t-hub-managed"
    if [ ! -f "$marker" ]; then
      echo "install-captain-skills: refusing to replace unmanaged $kind: $target" >&2
      exit 1
    fi
    expected="$(recorded_hash "$marker")"
    if [ "$kind" = directory ]; then
      actual="$(tree_hash "$target")"
    else
      actual="$(file_hash "$target")"
    fi
    if [ -z "$expected" ] || [ "$actual" != "$expected" ]; then
      if ! "$REPAIR"; then
        echo "install-captain-skills: refusing modified or unverifiable $kind: $target" >&2
        echo "install-captain-skills: inspect it, then rerun with --repair to replace it intentionally" >&2
        exit 1
      fi
    fi
  fi
done

if [ "$MODE" = check ]; then
  echo "install-captain-skills: preflight passed"
  exit 0
fi

if [ "$MODE" = verify ]; then
  for index in "${!SOURCES[@]}"; do
    if [ "${KINDS[$index]}" = directory ]; then
      source_hash="$(tree_hash "${SOURCES[$index]}")"
      target_hash="$(tree_hash "${TARGETS[$index]}")"
      recorded_hash="$(recorded_hash "${TARGETS[$index]}/.t-hub-managed")"
      if [ "$recorded_hash" != "$source_hash" ] || [ "$target_hash" != "$source_hash" ]; then
        echo "install-captain-skills: stale or modified skill: ${TARGETS[$index]}" >&2
        exit 1
      fi
    else
      source_hash="$(file_hash "${SOURCES[$index]}")"
      target_hash="$(file_hash "${TARGETS[$index]}")"
      installed_hash="$(recorded_hash "${TARGETS[$index]}.t-hub-managed")"
      if [ "$installed_hash" != "$source_hash" ] || [ "$target_hash" != "$source_hash" ]; then
        echo "install-captain-skills: stale or modified command: ${TARGETS[$index]}" >&2
        exit 1
      fi
    fi
  done
  echo "install-captain-skills: installed skills and commands match source"
  exit 0
fi

TEMPS=()
SIDECAR_TEMPS=()
BACKUPS=()
SIDECAR_BACKUPS=()
INSTALLED=0
rollback() {
  local index
  if [ -n "$SKILL_TXN" ] && [ -d "$SKILL_TXN" ]; then
    recover_skill_transaction
    return
  fi
  for ((index = INSTALLED - 1; index >= 0; index--)); do
    rm -rf "${TARGETS[$index]}"
    if [ -e "${BACKUPS[$index]}" ] || [ -L "${BACKUPS[$index]}" ]; then
      mv "${BACKUPS[$index]}" "${TARGETS[$index]}"
    fi
    if [ "${KINDS[$index]}" = command ]; then
      rm -f "${TARGETS[$index]}.t-hub-managed"
      if [ -e "${SIDECAR_BACKUPS[$index]}" ]; then
        mv "${SIDECAR_BACKUPS[$index]}" "${TARGETS[$index]}.t-hub-managed"
      fi
    fi
  done
  for temp in "${TEMPS[@]}"; do
    rm -rf "$temp"
  done
  for temp in "${SIDECAR_TEMPS[@]}"; do
    rm -f "$temp"
  done
  for backup in "${BACKUPS[@]}"; do
    rm -rf "$backup"
  done
  for backup in "${SIDECAR_BACKUPS[@]}"; do
    rm -f "$backup"
  done
}
trap rollback EXIT

for index in "${!SOURCES[@]}"; do
  source="${SOURCES[$index]}"
  target="${TARGETS[$index]}"
  target_root="$(dirname "$target")"
  name="$(basename "$target")"
  install -d -m 700 "$target_root"
  if [ "${KINDS[$index]}" = directory ]; then
    temp="$(mktemp -d "$target_root/.$name.staged.XXXXXX")"
    cp -a "$source/." "$temp/"
    printf 'managed by T-Hub\nhash-version=2\nsource-sha256=%s\n' "$(tree_hash "$source")" > "$temp/.t-hub-managed"
  else
    temp="$(mktemp "$target_root/.$name.staged.XXXXXX")"
    install -m 600 "$source" "$temp"
    sidecar_temp="$(mktemp "$target_root/.$name.marker.staged.XXXXXX")"
    printf 'managed by T-Hub\nhash-version=2\nsource-sha256=%s\n' \
      "$(file_hash "$source")" > "$sidecar_temp"
    SIDECAR_TEMPS[$index]="$sidecar_temp"
  fi
  TEMPS[$index]="$temp"
  BACKUPS[$index]="$target_root/.$name.previous.$$"
  SIDECAR_BACKUPS[$index]="$target_root/.$name.marker.previous.$$"
done

if [ -n "$SKILL_TXN" ]; then
  install -d -m 700 "$SKILL_TXN" "$SKILL_TXN/entries"
  python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/state.json" \
    --value '{"version":1,"status":"running"}'
  for index in "${!TARGETS[@]}"; do
    target="${TARGETS[$index]}"
    original="$(path_evidence "$target")"
    staged="$(path_evidence "${TEMPS[$index]}")"
    original_sidecar='{"presence":"absent"}'
    staged_sidecar='{"presence":"absent"}'
    if [ "${KINDS[$index]}" = command ]; then
      original_sidecar="$(path_evidence "$target.t-hub-managed")"
      staged_sidecar="$(path_evidence "${SIDECAR_TEMPS[$index]}")"
    fi
    sidecar_temp=null
    if [ "${KINDS[$index]}" = command ]; then sidecar_temp="${SIDECAR_TEMPS[$index]}"; fi
    entry="$(jq -cn --arg target "$target" --arg temp "${TEMPS[$index]}" \
      --arg sidecar_temp "$sidecar_temp" \
      --arg backup "${BACKUPS[$index]}" --arg sidecar_backup "${SIDECAR_BACKUPS[$index]}" \
      --arg kind "${KINDS[$index]}" --argjson original "$original" \
      --argjson staged "$staged" --argjson original_sidecar "$original_sidecar" \
      --argjson staged_sidecar "$staged_sidecar" \
      '{status:"prepared",target:$target,temp:$temp,
        sidecar_temp:(if $sidecar_temp == "null" then null else $sidecar_temp end),
        backup:$backup,sidecar_backup:$sidecar_backup,
        kind:$kind,original:$original,staged:$staged,
        original_sidecar:$original_sidecar,staged_sidecar:$staged_sidecar}')"
    python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
  done
fi

for index in "${!TARGETS[@]}"; do
  target="${TARGETS[$index]}"
  backup="${BACKUPS[$index]}"
  # Publish intent before mutation, then durable acquisition and installation
  # boundaries. Recovery still verifies exact inode and digest evidence because
  # a kill can occur between any rename and its following phase publication.
  INSTALLED=$((index + 1))
  if [ -n "$SKILL_TXN" ]; then
    entry="$(jq '.status="target-intent"' "$SKILL_TXN/entries/$index.json")"
    python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
  fi
  if [ "${T_HUB_SKILL_CRASH_AFTER_TARGET_INTENT_INDEX:-}" = "$index" ]; then kill -KILL "$$"; fi
  if [ -e "$target" ] || [ -L "$target" ]; then
    mv "$target" "$backup"
    python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
  fi
  if [ -n "$SKILL_TXN" ]; then
    entry="$(jq '.status="target-acquired"' "$SKILL_TXN/entries/$index.json")"
    python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
  fi
  mv "${TEMPS[$index]}" "$target"
  python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
  if [ -n "$SKILL_TXN" ]; then
    entry="$(jq '.status="target-installed"' "$SKILL_TXN/entries/$index.json")"
    python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
  fi
  if [ "${KINDS[$index]}" = command ]; then
    if [ -n "$SKILL_TXN" ]; then
      entry="$(jq '.status="sidecar-intent"' "$SKILL_TXN/entries/$index.json")"
      python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
    fi
    if [ -e "$target.t-hub-managed" ]; then
      mv "$target.t-hub-managed" "${SIDECAR_BACKUPS[$index]}"
      python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
    fi
    if [ -n "$SKILL_TXN" ]; then
      entry="$(jq '.status="sidecar-acquired"' "$SKILL_TXN/entries/$index.json")"
      python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
    fi
    mv "${SIDECAR_TEMPS[$index]}" "$target.t-hub-managed"
    python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
    if [ -n "$SKILL_TXN" ]; then
      entry="$(jq '.status="sidecar-installed"' "$SKILL_TXN/entries/$index.json")"
      python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
    fi
  fi
  if [ -n "$SKILL_TXN" ]; then
    entry="$(jq '.status="applied"' "$SKILL_TXN/entries/$index.json")"
    python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
  fi
  if [ "${T_HUB_SKILL_CRASH_AFTER_INDEX:-}" = "$index" ]; then kill -KILL "$$"; fi
done

if [ "${T_HUB_SKILL_FAIL_AFTER_INSTALL:-0}" = 1 ]; then
  echo "install-captain-skills: injected post-install failure" >&2
  exit 1
fi

# Verify while backups still exist. Any failure is handled by the active trap.
for index in "${!SOURCES[@]}"; do
  if [ "${KINDS[$index]}" = directory ]; then
    [ "$(tree_hash "${TARGETS[$index]}")" = "$(tree_hash "${SOURCES[$index]}")" ]
  else
    cmp -s "${SOURCES[$index]}" "${TARGETS[$index]}"
    [ "$(recorded_hash "${TARGETS[$index]}.t-hub-managed")" = "$(file_hash "${SOURCES[$index]}")" ]
  fi
done

trap - EXIT
for backup in "${BACKUPS[@]}"; do
  rm -rf "$backup"
done
for backup in "${SIDECAR_BACKUPS[@]}"; do
  rm -f "$backup"
done
for temp in "${SIDECAR_TEMPS[@]}"; do
  rm -f "$temp"
done
if [ -n "$SKILL_TXN" ] && [ -d "$SKILL_TXN" ]; then
  python3 "$ATOMIC_HELPER" purge --path "$SKILL_TXN"
fi
echo "install-captain-skills: installed Captain, Shipmate, and Handoff for Codex and Claude"
