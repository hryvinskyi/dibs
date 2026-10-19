#!/usr/bin/env bash
# Runs the real scripts/check-host-names.sh over temporary git repositories.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/../check-host-names.sh"
# shellcheck source=harness.sh
. "$here/harness.sh"

# The host names are assembled so that this file does not itself contain them.
hosts=("cla""ude" "co""dex" "open""code")

run() { (cd "$1" && "$script"); }

# put <repo> <relative path> <content>: writes a file and stages it.
put() {
  mkdir -p "$(dirname "$1/$2")"
  printf '%s\n' "$3" >"$1/$2"
  git -C "$1" add -A
}

repo="$(new_repo plain)"
put "$repo" crates/domain/src/lib.rs "pub struct Claim;"
check "a tree that names no host passes" 0 run "$repo"

allowed=(
  crates/host-adapter/src/lib.rs
  integrations/a/b.json
  ".${hosts[0]}-plugin/marketplace.json"
  docs/hosts.md
  README.md
  Cargo.toml
  Cargo.lock
  crates/cli/Cargo.toml
  crates/cli/src/hosts.rs
  scripts/check-host-names.sh
)
repo="$(new_repo allowed)"
for path in "${allowed[@]}"; do
  put "$repo" "$path" "uses ${hosts[0]} here"
done
check "every allowed path may name a host" 0 run "$repo"

for host in "${hosts[@]}"; do
  repo="$(new_repo "denied-$host")"
  put "$repo" crates/domain/src/lib.rs "// talks to $(printf '%s' "$host" | tr '[:lower:]' '[:upper:]')"
  check "an upper-case '$host' in the domain crate fails" 1 run "$repo"
  expect_output "the offending path is named for '$host'" "crates/domain/src/lib.rs"
done

repo="$(new_repo untracked)"
put "$repo" crates/domain/src/lib.rs "fine"
printf '%s\n' "${hosts[1]}" >"$repo/crates/domain/src/extra.rs"
check "an untracked file that is not ignored counts" 1 run "$repo"
expect_output "the untracked path is named" "crates/domain/src/extra.rs"

repo="$(new_repo ignored)"
put "$repo" .gitignore "target/"
mkdir -p "$repo/target"
printf '%s\n' "${hosts[2]}" >"$repo/target/out.txt"
check "an ignored file is outside the tree" 0 run "$repo"

repo="$(new_repo history)"
put "$repo" crates/domain/src/lib.rs "${hosts[0]}"
git -C "$repo" commit -q -m "add a file"
put "$repo" crates/domain/src/lib.rs "fine"
git -C "$repo" commit -q -m "remove it"
check "a name that only the history holds is not checked" 0 run "$repo"

repo="$(new_repo nested)"
put "$repo" crates/cli/src/main.rs "${hosts[0]}"
check "only the composition root's registry may name a host in the cli crate" 1 run "$repo"
expect_output "the cli entry point is named" "crates/cli/src/main.rs"

finish
