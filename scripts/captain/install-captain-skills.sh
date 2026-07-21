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

recover_skill_transaction() {
  local entry index target backup sidecar_backup original_target original_sidecar temp
  [ -n "$SKILL_TXN" ] && [ -d "$SKILL_TXN" ] || return 0
  for ((index = 6; index >= 0; index--)); do
    entry="$SKILL_TXN/entries/$index.json"
    [ -f "$entry" ] || continue
    target="$(jq -r .target "$entry")"
    backup="$(jq -r .backup "$entry")"
    sidecar_backup="$(jq -r .sidecar_backup "$entry")"
    temp="$(jq -r .temp "$entry")"
    original_target="$(jq -r .original_target "$entry")"
    original_sidecar="$(jq -r .original_sidecar "$entry")"
    if [ -e "$target" ] || [ -L "$target" ]; then
      python3 "$ATOMIC_HELPER" purge --path "$target"
    fi
    if [ "$original_target" = true ]; then
      [ -e "$backup" ] || [ -L "$backup" ] || {
        echo "install-captain-skills: missing durable target backup: $target" >&2
        return 1
      }
      mv "$backup" "$target"
      python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
    fi
    if [ "$(jq -r .kind "$entry")" = command ]; then
      if [ -e "$target.t-hub-managed" ]; then
        python3 "$ATOMIC_HELPER" purge --path "$target.t-hub-managed"
      fi
      if [ "$original_sidecar" = true ]; then
        [ -e "$sidecar_backup" ] || {
          echo "install-captain-skills: missing durable marker backup: $target" >&2
          return 1
        }
        mv "$sidecar_backup" "$target.t-hub-managed"
        python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
      fi
    fi
    if [ -e "$temp" ] || [ -L "$temp" ]; then
      python3 "$ATOMIC_HELPER" purge --path "$temp"
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
    original_target=false
    original_sidecar=false
    if [ -e "$target" ] || [ -L "$target" ]; then original_target=true; fi
    if [ "${KINDS[$index]}" = command ] && [ -e "$target.t-hub-managed" ]; then
      original_sidecar=true
    fi
    entry="$(jq -cn --arg target "$target" --arg temp "${TEMPS[$index]}" \
      --arg backup "${BACKUPS[$index]}" --arg sidecar_backup "${SIDECAR_BACKUPS[$index]}" \
      --arg kind "${KINDS[$index]}" --argjson original_target "$original_target" \
      --argjson original_sidecar "$original_sidecar" \
      '{target:$target,temp:$temp,backup:$backup,sidecar_backup:$sidecar_backup,
        kind:$kind,original_target:$original_target,original_sidecar:$original_sidecar}')"
    python3 "$ATOMIC_HELPER" publish --path "$SKILL_TXN/entries/$index.json" --value "$entry"
  done
fi

for index in "${!TARGETS[@]}"; do
  target="${TARGETS[$index]}"
  backup="${BACKUPS[$index]}"
  # Mark the slot before its first mutation so every partial swap is restorable.
  INSTALLED=$((index + 1))
  if [ -e "$target" ] || [ -L "$target" ]; then
    mv "$target" "$backup"
    python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
  fi
  if [ "${KINDS[$index]}" = command ] && [ -e "$target.t-hub-managed" ]; then
    mv "$target.t-hub-managed" "${SIDECAR_BACKUPS[$index]}"
    python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
  fi
  mv "${TEMPS[$index]}" "$target"
  python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
  if [ "${KINDS[$index]}" = command ]; then
    printf 'managed by T-Hub\nhash-version=2\nsource-sha256=%s\n' \
      "$(file_hash "${SOURCES[$index]}")" > "$target.t-hub-managed"
    python3 "$ATOMIC_HELPER" sync-directory --path "$(dirname "$target")"
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
if [ -n "$SKILL_TXN" ] && [ -d "$SKILL_TXN" ]; then
  python3 "$ATOMIC_HELPER" purge --path "$SKILL_TXN"
fi
echo "install-captain-skills: installed Captain, Shipmate, and Handoff for Codex and Claude"
