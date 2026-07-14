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

if ! "$SOURCE" --list-tools >/dev/null 2>&1; then
  echo "install-thub-codex: source binary failed its offline catalog probe: $SOURCE" >&2
  exit 1
fi

# Refuse every known skill conflict before replacing the MCP binary or changing
# Codex registration. The installer repeats validation inside its own
# transaction to cover races between preflight and commit.
bash "$HERE/install-captain-skills.sh" --check

install -d -m 700 "$BIN_DIR" "$CAPTAIN_DIR"
TXN="$(mktemp -d "$BIN_DIR/.t-hub-install.XXXXXX")"
STAGED_BIN="$TXN/t-hub-mcp"
STAGED_CODEX="$TXN/ensure-thub-codex.sh"
STAGED_CLAUDE="$TXN/ensure-thub-claude.sh"
install -m 700 "$SOURCE" "$STAGED_BIN"
install -m 700 "$HERE/ensure-thub-codex.sh" "$STAGED_CODEX"
install -m 700 "$HERE/ensure-thub-claude.sh" "$STAGED_CLAUDE"
"$STAGED_BIN" --list-tools >/dev/null

backup_file() {
  local source="$1" name="$2"
  if [ -f "$source" ]; then
    cp -p "$source" "$TXN/$name"
    printf 'present' > "$TXN/$name.state"
  else
    printf 'absent' > "$TXN/$name.state"
  fi
}
restore_file() {
  local target="$1" name="$2"
  if [ "$(cat "$TXN/$name.state")" = present ]; then
    install -d -m 700 "$(dirname "$target")"
    cp -p "$TXN/$name" "$target"
  else
    rm -f "$target"
  fi
}

backup_file "$DEST" previous-bin
backup_file "$CAPTAIN_DIR/ensure-thub-codex.sh" previous-codex
backup_file "$CAPTAIN_DIR/ensure-thub-claude.sh" previous-claude
backup_file "$CODEX_CONFIG" previous-codex-config
backup_file "$CLAUDE_CONFIG" previous-claude-config
rollback() {
  restore_file "$DEST" previous-bin
  restore_file "$CAPTAIN_DIR/ensure-thub-codex.sh" previous-codex
  restore_file "$CAPTAIN_DIR/ensure-thub-claude.sh" previous-claude
  restore_file "$CODEX_CONFIG" previous-codex-config
  restore_file "$CLAUDE_CONFIG" previous-claude-config
  rm -rf "$TXN"
}
trap rollback EXIT

install -m 700 "$STAGED_BIN" "$DEST"
install -m 700 "$STAGED_CODEX" "$CAPTAIN_DIR/ensure-thub-codex.sh"
install -m 700 "$STAGED_CLAUDE" "$CAPTAIN_DIR/ensure-thub-claude.sh"
T_HUB_MCP_BIN="$DEST" "$CAPTAIN_DIR/ensure-thub-claude.sh"
T_HUB_MCP_BIN="$DEST" "$CAPTAIN_DIR/ensure-thub-codex.sh"
bash "$HERE/install-captain-skills.sh"
bash "$HERE/install-captain-skills.sh" --verify

trap - EXIT
rm -rf "$TXN"

echo "install-thub-codex: installed $DEST"
echo "install-thub-codex: start new Codex and Claude sessions to load the updated integration"
