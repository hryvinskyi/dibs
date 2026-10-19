#!/usr/bin/env bash
# Runs the real scripts/check-domain-deps.sh over throwaway workspaces whose `dibs-domain`
# package depends on local stand-in crates. Nothing is fetched from a registry.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/../check-domain-deps.sh"
# shellcheck source=harness.sh
. "$here/harness.sh"

# make_workspace <name> <dependency crate names...>: a workspace with dibs-domain and the stand-ins.
make_workspace() {
  local dir="$scratch/$1"
  shift
  mkdir -p "$dir/domain/src"
  printf '[workspace]\nresolver = "3"\nmembers = ["domain"%s]\n' \
    "$(for dep in "$@"; do printf ', "%s"' "$dep"; done)" >"$dir/Cargo.toml"
  {
    printf '[package]\nname = "dibs-domain"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n'
    for dep in "$@"; do printf '%s = { path = "../%s" }\n' "$dep" "$dep"; done
  } >"$dir/domain/Cargo.toml"
  : >"$dir/domain/src/lib.rs"
  for dep in "$@"; do
    mkdir -p "$dir/$dep/src"
    printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2024"\n' "$dep" >"$dir/$dep/Cargo.toml"
    : >"$dir/$dep/src/lib.rs"
  done
  printf '%s' "$dir/Cargo.toml"
}

run() { DIBS_MANIFEST_PATH="$1" "$script"; }

manifest="$(make_workspace none)"
check "a domain crate without dependencies passes" 0 run "$manifest"

manifest="$(make_workspace allowed thiserror globset time serde)"
check "every allow-listed dependency passes" 0 run "$manifest"

manifest="$(make_workspace denied serde sqlx)"
check "a dependency outside the allow-list fails" 1 run "$manifest"
expect_output "the offending crate is named" "sqlx"

manifest="$(make_workspace deniedonly tokio reqwest)"
check "several offending dependencies fail" 1 run "$manifest"
expect_output "the first offender is named" "tokio"
expect_output "the second offender is named" "reqwest"

finish
