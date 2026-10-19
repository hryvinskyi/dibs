#!/usr/bin/env bash
# Fails when a word from a denylist appears, case-insensitively, in the tree, in a file name,
# in any commit's diff, in any commit message, or in any author or committer name or e-mail.
#
# The output names paths, commit ids and line numbers only; a listed word is never printed
# (except as a `::add-mask::` command, and only when GitHub Actions runs the script).
#
# The word list is one word per line and comes from, in order:
#   1. the file named by the first argument;
#   2. the DIBS_DENYLIST environment variable (the repository secret in CI);
#   3. ${XDG_CONFIG_HOME:-~/.config}/dibs-dev/denylist.
# With none of them available, CI passes with a notice (a fork's pull request cannot read the
# secret) and a local run fails.
#
# Exit status: 0 clean, 1 a word was found, 2 the script could not run.
set -euo pipefail
set +x
case "$-" in
  *x*)
    echo "check-clean.sh must not run under 'set -x'." >&2
    exit 2
    ;;
esac

in_ci=0
[ "${GITHUB_ACTIONS:-}" = "true" ] && in_ci=1

scratch="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/dibs-clean.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
umask 077

raw="$scratch/raw"
words="$scratch/words"

if [ "$#" -ge 1 ]; then
  if [ ! -r "$1" ]; then
    echo "The word list '$1' cannot be read." >&2
    exit 2
  fi
  cp "$1" "$raw"
elif [ -n "${DIBS_DENYLIST:-}" ]; then
  printf '%s\n' "$DIBS_DENYLIST" >"$raw"
else
  default_list="${XDG_CONFIG_HOME:-$HOME/.config}/dibs-dev/denylist"
  if [ -r "$default_list" ]; then
    cp "$default_list" "$raw"
  elif [ "$in_ci" -eq 1 ]; then
    echo "::notice::No denylist is available to this run; the clean check was skipped."
    exit 0
  else
    echo "No denylist found at '$default_list'." >&2
    exit 2
  fi
fi

# Carriage returns and blank lines go: an empty pattern would match everything.
tr -d '\r' <"$raw" | awk 'NF > 0' >"$words"
if [ ! -s "$words" ]; then
  echo "The word list holds no words." >&2
  exit 2
fi

if [ "$in_ci" -eq 1 ]; then
  while IFS= read -r word; do
    printf '::add-mask::%s\n' "$word"
  done <"$words"
fi

cd "$(git rev-parse --show-toplevel)"

failed=0

# Prints "<label> <n>" for each line of stdin that holds a listed word, never the line itself.
# A record that starts with a \001 byte opens a commit: the first field is its id.
scan='
  FNR == 1 { file++ }
  file == 1 { if ($0 != "") words[n++] = tolower($0); next }
  substr($0, 1, 1) == sprintf("%c", 1) { id = substr($1, 2); line = 0 }
  {
    line++
    lower = tolower($0)
    for (i = 0; i < n; i++) {
      if (index(lower, words[i]) > 0) {
        if (id != "") print "commit " id " line " line
        else print "file name #" line
        break
      }
    }
  }
'

report() { # heading; the lines to print arrive on stdin
  echo "$1"
  sed 's/^/  /'
}

# The working tree: tracked files and untracked files that are not ignored.
tree_hits="$(git grep --untracked -l -i -F -f "$words" -- . || true)"
if [ -n "$tree_hits" ]; then
  failed=1
  printf '%s\n' "$tree_hits" | report "A listed word appears in these files:"
fi

# File names.
name_hits="$(git ls-files --cached --others --exclude-standard |
  awk "$scan" "$words" - || true)"
if [ -n "$name_hits" ]; then
  failed=1
  printf '%s\n' "$name_hits" | report "A listed word appears in the name of these files (position in the file listing):"
fi

# Diffs of every commit on every ref.
diff_hits=""
while IFS= read -r word; do
  pattern="$(printf '%s' "$word" | sed 's/[][\\.*^$+?(){}|/]/\\&/g')"
  found="$(git log --all --format=%H -i -G"$pattern" 2>/dev/null || true)"
  [ -n "$found" ] && diff_hits="$diff_hits$found"$'\n'
done <"$words"
if [ -n "$diff_hits" ]; then
  failed=1
  printf '%s' "$diff_hits" | sort -u | sed '/^$/d; s/^/commit /' |
    report "A listed word appears in the changes of these commits:"
fi

# Commit messages and author/committer names and e-mails.
msg_hits="$(git log --all --format='%x01%H %an %ae %cn %ce%n%B' 2>/dev/null |
  awk "$scan" "$words" - || true)"
if [ -n "$msg_hits" ]; then
  failed=1
  printf '%s\n' "$msg_hits" | report "A listed word appears in a commit's identity line (line 1) or message:"
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi
echo "No listed word appears in the tree or the history."
