#!/usr/bin/env bash
# Fails when an agent host's name appears in a file outside the places that may name one.
# Prints the offending paths only. Untracked files that are not ignored count as part of
# the tree, so the check also holds before a change is staged.
#
# Usage: scripts/check-host-names.sh   (run from anywhere inside the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

hits="$(git grep --untracked -l -i -E 'claude|codex|opencode' -- . \
  ':!crates/host-*' \
  ':!integrations' \
  ':!.claude-plugin' \
  ':!docs/hosts.md' \
  ':!README.md' \
  ':!Cargo.toml' \
  ':!Cargo.lock' \
  ':!crates/cli/Cargo.toml' \
  ':!crates/cli/src/hosts.rs' \
  ':!scripts/check-host-names.sh' || true)"

if [ -n "$hits" ]; then
  echo "An agent host is named outside the adapter crates, integrations and docs:" >&2
  printf '  %s\n' "$hits" >&2
  exit 1
fi

echo "No agent host is named outside the places allowed to."
