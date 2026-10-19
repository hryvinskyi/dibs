# Minimal test harness shared by the script tests. Source it; do not execute it.
#
# Provides: scratch (a fresh temp directory removed on exit), new_repo, check (expects an
# exit status), output (the last command's combined output), and finish (prints the summary).

scratch="$(mktemp -d "${TMPDIR:-/tmp}/dibs-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# The tests must never see the maintainer's own configuration or CI variables.
export HOME="$scratch/home"
export XDG_CONFIG_HOME="$scratch/home/.config"
mkdir -p "$XDG_CONFIG_HOME"
unset DIBS_DENYLIST GITHUB_ACTIONS RUNNER_TEMP DIBS_MANIFEST_PATH

export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null

passed=0
failed=0
output=""

# new_repo <name>: an empty git repository with a neutral identity; prints its path.
new_repo() {
  local dir="$scratch/$1"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  git -C "$dir" config user.name "Alice Example"
  git -C "$dir" config user.email "alice@example.com"
  git -C "$dir" config commit.gpgsign false
  printf '%s' "$dir"
}

# check <description> <expected status> <command...>: runs the command, records its output.
check() {
  local description="$1" expected="$2" status
  shift 2
  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e
  if [ "$status" -eq "$expected" ]; then
    passed=$((passed + 1))
    echo "ok   - $description"
  else
    failed=$((failed + 1))
    echo "FAIL - $description (exit $status, expected $expected)"
    printf '%s\n' "$output" | sed 's/^/       | /'
  fi
}

# expect_output <description> <fixed text>: the last output contains the text.
expect_output() {
  if printf '%s' "$output" | grep -q -F -- "$2"; then
    passed=$((passed + 1))
    echo "ok   - $1"
  else
    failed=$((failed + 1))
    echo "FAIL - $1"
    printf '%s\n' "$output" | sed 's/^/       | /'
  fi
}

# expect_no_output <description> <fixed text>: the last output lacks the text, ignoring case.
expect_no_output() {
  if printf '%s' "$output" | grep -q -i -F -- "$2"; then
    failed=$((failed + 1))
    echo "FAIL - $1"
  else
    passed=$((passed + 1))
    echo "ok   - $1"
  fi
}

finish() {
  echo "$passed passed, $failed failed"
  [ "$failed" -eq 0 ]
}
