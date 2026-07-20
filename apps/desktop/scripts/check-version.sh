#!/usr/bin/env bash
# Validate the desktop release version and optionally reject reuse of a version
# that has already been tagged for a different commit.
set -euo pipefail
cd "$(dirname "$0")/.."

package_version=$(grep -m1 '"version"' package.json | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
tauri_version=$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
cargo_version=$(grep -m1 '^version[[:space:]]*=' src-tauri/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
cargo_lock_version=$(awk '/^name = "t-hub"$/ { found=1; next } found && /^version = / { sub(/^[^"]*"/, ""); sub(/".*/, ""); print; exit }' src-tauri/Cargo.lock)

if [[ ! "$package_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "desktop version is not a stable semantic version: $package_version" >&2
  exit 1
fi

for entry in \
  "tauri.conf.json:$tauri_version" \
  "Cargo.toml:$cargo_version" \
  "Cargo.lock:$cargo_lock_version"
do
  file=${entry%%:*}
  version=${entry#*:}
  if [[ "$version" != "$package_version" ]]; then
    echo "desktop version mismatch: package.json=$package_version $file=$version" >&2
    exit 1
  fi
done

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      [[ $# -ge 2 ]] || { echo "--tag requires a value" >&2; exit 2; }
      tag=${2#v}
      if [[ "$tag" != "$package_version" ]]; then
        echo "release tag/version mismatch: tag=$tag desktop=$package_version" >&2
        exit 1
      fi
      shift 2
      ;;
    --history)
      version_tag="v$package_version"
      if git rev-parse --verify --quiet "refs/tags/$version_tag" >/dev/null && \
        [[ "$(git rev-list -n 1 "$version_tag")" != "$(git rev-parse HEAD)" ]]; then
        echo "desktop version $package_version is already tagged at $(git rev-list -n 1 --abbrev-commit "$version_tag"); run scripts/bump-version.sh" >&2
        exit 1
      fi
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

echo "desktop version $package_version is consistent"
