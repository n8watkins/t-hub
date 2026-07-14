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

for index in "${!TARGETS[@]}"; do
  target="${TARGETS[$index]}"
  backup="${BACKUPS[$index]}"
  # Mark the slot before its first mutation so every partial swap is restorable.
  INSTALLED=$((index + 1))
  if [ -e "$target" ] || [ -L "$target" ]; then
    mv "$target" "$backup"
  fi
  if [ "${KINDS[$index]}" = command ] && [ -e "$target.t-hub-managed" ]; then
    mv "$target.t-hub-managed" "${SIDECAR_BACKUPS[$index]}"
  fi
  mv "${TEMPS[$index]}" "$target"
  if [ "${KINDS[$index]}" = command ]; then
    printf 'managed by T-Hub\nhash-version=2\nsource-sha256=%s\n' \
      "$(file_hash "${SOURCES[$index]}")" > "$target.t-hub-managed"
  fi
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
echo "install-captain-skills: installed Captain, Shipmate, and Handoff for Codex and Claude"
