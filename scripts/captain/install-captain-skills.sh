#!/usr/bin/env bash
# Transactionally install the canonical Captain skill and Shipmate compatibility alias.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SOURCE_ROOT="${T_HUB_SKILLS_SOURCE:-$REPO_ROOT/skills}"
CODEX_SKILLS="${T_HUB_CODEX_SKILLS_DIR:-${CODEX_HOME:-${HOME}/.codex}/skills}"
CLAUDE_SKILLS="${T_HUB_CLAUDE_SKILLS_DIR:-${CLAUDE_HOME:-${HOME}/.claude}/skills}"
CHECK_ONLY=false
if [ "${1:-}" = "--check" ]; then
  CHECK_ONLY=true
elif [ "$#" -ne 0 ]; then
  echo "usage: install-captain-skills.sh [--check]" >&2
  exit 2
fi

SOURCES=(
  "$SOURCE_ROOT/captain"
  "$SOURCE_ROOT/shipmate"
  "$SOURCE_ROOT/captain"
  "$SOURCE_ROOT/shipmate"
)
TARGETS=(
  "$CODEX_SKILLS/captain"
  "$CODEX_SKILLS/shipmate"
  "$CLAUDE_SKILLS/captain"
  "$CLAUDE_SKILLS/shipmate"
)

for index in "${!SOURCES[@]}"; do
  source="${SOURCES[$index]}"
  target="${TARGETS[$index]}"
  if [ ! -f "$source/SKILL.md" ]; then
    echo "install-captain-skills: missing source skill: $source" >&2
    exit 1
  fi
  if [ -e "$target" ] || [ -L "$target" ]; then
    if [ -L "$target" ] && [ "$(readlink -f "$target")" = "$(readlink -f "$source")" ]; then
      continue
    fi
    if [ ! -f "$target/.t-hub-managed" ]; then
      echo "install-captain-skills: refusing to replace unmanaged skill: $target" >&2
      exit 1
    fi
  fi
done

if "$CHECK_ONLY"; then
  echo "install-captain-skills: preflight passed"
  exit 0
fi

TEMPS=()
BACKUPS=()
INSTALLED=0
rollback() {
  local index
  for ((index = INSTALLED - 1; index >= 0; index--)); do
    rm -rf "${TARGETS[$index]}"
    if [ -e "${BACKUPS[$index]}" ] || [ -L "${BACKUPS[$index]}" ]; then
      mv "${BACKUPS[$index]}" "${TARGETS[$index]}"
    fi
  done
  for temp in "${TEMPS[@]}"; do
    rm -rf "$temp"
  done
  for backup in "${BACKUPS[@]}"; do
    rm -rf "$backup"
  done
}
trap rollback EXIT

for index in "${!SOURCES[@]}"; do
  source="${SOURCES[$index]}"
  target="${TARGETS[$index]}"
  target_root="$(dirname "$target")"
  name="$(basename "$target")"
  install -d -m 700 "$target_root"
  temp="$(mktemp -d "$target_root/.$name.staged.XXXXXX")"
  cp -a "$source/." "$temp/"
  printf 'managed by T-Hub\n' > "$temp/.t-hub-managed"
  TEMPS[$index]="$temp"
  BACKUPS[$index]="$target_root/.$name.previous.$$"
done

for index in "${!TARGETS[@]}"; do
  target="${TARGETS[$index]}"
  backup="${BACKUPS[$index]}"
  if [ -e "$target" ] || [ -L "$target" ]; then
    mv "$target" "$backup"
  fi
  INSTALLED=$((index + 1))
  mv "${TEMPS[$index]}" "$target"
done

trap - EXIT
for backup in "${BACKUPS[@]}"; do
  rm -rf "$backup"
done
echo "install-captain-skills: installed Captain and Shipmate for Codex and Claude"
