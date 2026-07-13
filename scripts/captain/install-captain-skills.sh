#!/usr/bin/env bash
# Atomically install the canonical Captain skill and Shipmate compatibility alias.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SOURCE_ROOT="${T_HUB_SKILLS_SOURCE:-$REPO_ROOT/skills}"
CODEX_SKILLS="${T_HUB_CODEX_SKILLS_DIR:-${CODEX_HOME:-${HOME}/.codex}/skills}"
CLAUDE_SKILLS="${T_HUB_CLAUDE_SKILLS_DIR:-${CLAUDE_HOME:-${HOME}/.claude}/skills}"

install_skill() {
  local source="$1"
  local target_root="$2"
  local name target temp backup
  name="$(basename "$source")"
  target="$target_root/$name"

  if [ ! -f "$source/SKILL.md" ]; then
    echo "install-captain-skills: missing source skill: $source" >&2
    exit 1
  fi
  install -d -m 700 "$target_root"
  if [ -e "$target" ] || [ -L "$target" ]; then
    if [ -L "$target" ] && [ "$(readlink -f "$target")" = "$(readlink -f "$source")" ]; then
      rm "$target"
    elif [ ! -f "$target/.t-hub-managed" ]; then
      echo "install-captain-skills: refusing to replace unmanaged skill: $target" >&2
      exit 1
    fi
  fi

  temp="$(mktemp -d "$target_root/.${name}.XXXXXX")"
  cp -a "$source/." "$temp/"
  printf 'managed by T-Hub\n' > "$temp/.t-hub-managed"
  backup="$target_root/.${name}.previous.$$"
  if [ -e "$target" ]; then
    mv "$target" "$backup"
  fi
  if mv "$temp" "$target"; then
    rm -rf "$backup"
  else
    rm -rf "$temp"
    if [ -e "$backup" ]; then
      mv "$backup" "$target"
    fi
    exit 1
  fi
}

for target_root in "$CODEX_SKILLS" "$CLAUDE_SKILLS"; do
  install_skill "$SOURCE_ROOT/captain" "$target_root"
  install_skill "$SOURCE_ROOT/shipmate" "$target_root"
done

echo "install-captain-skills: installed Captain and Shipmate for Codex and Claude"
