#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow_dir="$repo_root/.github/workflows"
failed=0

while IFS=: read -r file line reference; do
  [[ -n "$file" ]] || continue
  reference="$(
    printf '%s' "$reference" |
      sed -E 's/[[:space:]]+#.*$//; s/^[[:space:]]+//; s/[[:space:]]+$//; s/^["'"'"']//; s/["'"'"']$//'
  )"
  if [[ "$reference" == ./* ]]; then
    continue
  fi
  if [[ ! "$reference" =~ ^[^[:space:]@]+@([0-9a-f]{40})$ ]]; then
    printf 'Mutable or invalid action reference: %s:%s: %s\n' \
      "${file#"$repo_root/"}" "$line" "$reference" >&2
    failed=1
  fi
done < <(
  grep -RHEn '^[[:space:]]*-?[[:space:]]*uses:[[:space:]]+' \
    "$workflow_dir" |
    sed -E 's|^([^:]+):([0-9]+):[[:space:]]*-?[[:space:]]*uses:[[:space:]]+|\1:\2:|'
)

if ((failed)); then
  exit 1
fi

echo "All external GitHub Actions references use immutable commit SHAs."
