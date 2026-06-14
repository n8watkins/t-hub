#!/usr/bin/env bash
# Bump the app's PATCH version across the three manifests that must agree:
#   - package.json                 (npm/pnpm + About fallback)
#   - src-tauri/tauri.conf.json    (the version baked into the binary; About's
#                                   getVersion() reads this; NSIS installer too)
#   - src-tauri/Cargo.toml         ([package] version of the `termhub` crate)
#
# Run from anywhere; it locates the repo root via this script's path. Prints the
# new version. Standing policy: run this before EVERY deploy so each update ships
# a fresh, visible version (see About in Settings).
set -euo pipefail
cd "$(dirname "$0")/.."

cur=$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
IFS='.' read -r MA MI PA <<< "$cur"
new="$MA.$MI.$((PA + 1))"

# tauri.conf.json + package.json: first "version": "x.y.z"
sed -i -E "0,/\"version\"[[:space:]]*:[[:space:]]*\"[^\"]+\"/s//\"version\": \"$new\"/" src-tauri/tauri.conf.json
sed -i -E "0,/\"version\"[[:space:]]*:[[:space:]]*\"[^\"]+\"/s//\"version\": \"$new\"/" package.json
# Cargo.toml: first `version = "x.y.z"` (the [package] one)
sed -i -E "0,/^version[[:space:]]*=[[:space:]]*\"[^\"]+\"/s//version = \"$new\"/" src-tauri/Cargo.toml

echo "$new"
