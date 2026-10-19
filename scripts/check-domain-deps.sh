#!/usr/bin/env bash
# Fails when dibs-domain has a direct normal dependency outside the allow-list.
#
# Usage: scripts/check-domain-deps.sh
# The manifest defaults to the workspace root; DIBS_MANIFEST_PATH points the
# check at another workspace that holds a `dibs-domain` package.
set -euo pipefail

allowed=(thiserror globset time serde)

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="${DIBS_MANIFEST_PATH:-$root/Cargo.toml}"

tree="$(cargo tree --manifest-path "$manifest" -p dibs-domain -e normal --prefix none --depth 1)"

violations=()
first=1
while IFS= read -r line; do
  if [ "$first" -eq 1 ]; then
    first=0
    continue
  fi
  name="${line%% *}"
  [ -n "$name" ] || continue
  ok=0
  for candidate in "${allowed[@]}"; do
    if [ "$name" = "$candidate" ]; then
      ok=1
      break
    fi
  done
  if [ "$ok" -eq 0 ]; then
    violations+=("$name")
  fi
done <<<"$tree"

if [ "${#violations[@]}" -gt 0 ]; then
  echo "dibs-domain depends directly on crates outside the allow-list (${allowed[*]}):" >&2
  printf '  %s\n' "${violations[@]}" >&2
  exit 1
fi

echo "dibs-domain direct dependencies are within the allow-list."
