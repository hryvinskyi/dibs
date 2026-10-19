#!/usr/bin/env bash
# Runs the real scripts/check-clean.sh over temporary git repositories with a synthetic
# word list. The maintainer's own list is never read.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/../check-clean.sh"
# shellcheck source=harness.sh
. "$here/harness.sh"

word="zorblax"
list="$scratch/words.txt"
# A blank line and a CRLF ending must not turn into patterns that match everything.
printf '%s\r\n\n' "$word" >"$list"

run() { (cd "$1" && shift && "$script" "$@"); }

# seed <repo> <relative path> <content>: commits a file with a neutral message.
seed() {
  mkdir -p "$(dirname "$1/$2")"
  printf '%s\n' "$3" >"$1/$2"
  git -C "$1" add -A
  git -C "$1" commit -q -m "add $2"
}

repo="$(new_repo clean)"
seed "$repo" a.txt "nothing to see"
check "a clean repository passes" 0 run "$repo" "$list"

repo="$(new_repo empty)"
check "a repository without commits passes" 0 run "$repo" "$list"

repo="$(new_repo infile)"
seed "$repo" notes/a.txt "this mentions ZORBLAX in capitals"
check "a word in a file fails" 1 run "$repo" "$list"
expect_output "the path is named" "notes/a.txt"
expect_no_output "the word is not printed" "$word"

repo="$(new_repo untracked)"
seed "$repo" a.txt "fine"
printf '%s\n' "$word" >"$repo/new.txt"
check "a word in an untracked file fails" 1 run "$repo" "$list"
expect_output "the untracked path is named" "new.txt"

repo="$(new_repo ignored)"
seed "$repo" .gitignore "build/"
mkdir -p "$repo/build"
printf '%s\n' "$word" >"$repo/build/out.txt"
check "an ignored file is outside the tree" 0 run "$repo" "$list"

repo="$(new_repo filename)"
seed "$repo" "docs/$word.md" "fine"
check "a word in a file name fails" 1 run "$repo" "$list"
expect_output "the file name is reported by position" "file name #"
expect_no_output "the word is not printed for a file name" "$word"

repo="$(new_repo olddiff)"
seed "$repo" a.txt "$word"
old="$(git -C "$repo" rev-parse HEAD)"
seed "$repo" a.txt "removed again"
check "a word only in an old commit's diff fails" 1 run "$repo" "$list"
expect_output "the old commit is named" "$old"
expect_no_output "the word is not printed for a diff" "$word"

repo="$(new_repo message)"
seed "$repo" a.txt "fine"
git -C "$repo" commit -q --allow-empty -m "mentions $word in the subject"
msg="$(git -C "$repo" rev-parse HEAD)"
check "a word in a commit message fails" 1 run "$repo" "$list"
expect_output "the message's commit is named" "$msg"
expect_no_output "the word is not printed for a message" "$word"

repo="$(new_repo email)"
seed "$repo" a.txt "fine"
git -C "$repo" -c user.email="$word@example.com" commit -q --allow-empty -m "neutral message"
mail="$(git -C "$repo" rev-parse HEAD)"
check "a word in an author e-mail fails" 1 run "$repo" "$list"
expect_output "the author's commit is named" "$mail"
expect_output "the identity line is reported" "line 1"
expect_no_output "the word is not printed for an e-mail" "$word"

repo="$(new_repo committer)"
seed "$repo" a.txt "fine"
GIT_COMMITTER_NAME="Mr $word" git -C "$repo" commit -q --allow-empty -m "neutral message"
check "a word in a committer name fails" 1 run "$repo" "$list"
expect_no_output "the word is not printed for a committer" "$word"

repo="$(new_repo otherbranch)"
seed "$repo" a.txt "fine"
git -C "$repo" checkout -q -b topic
seed "$repo" b.txt "$word"
git -C "$repo" checkout -q main
check "a word on a ref other than the checked-out branch fails" 1 run "$repo" "$list"

repo="$(new_repo regex)"
seed "$repo" a.txt "qxz"
printf 'q.z\n' >"$scratch/regex-words.txt"
check "a word is matched literally, not as a pattern" 0 run "$repo" "$scratch/regex-words.txt"
seed "$repo" b.txt "q.z"
check "a literal word with pattern characters is found in a diff" 1 run "$repo" "$scratch/regex-words.txt"

repo="$(new_repo envlist)"
seed "$repo" a.txt "$word"
check "the list can come from DIBS_DENYLIST" 1 env DIBS_DENYLIST="$word" bash -c 'cd "$1" && "$2"' _ "$repo" "$script"

repo="$(new_repo defaultlist)"
seed "$repo" a.txt "$word"
mkdir -p "$XDG_CONFIG_HOME/dibs-dev"
printf '%s\n' "$word" >"$XDG_CONFIG_HOME/dibs-dev/denylist"
check "the list can come from the per-user config file" 1 run "$repo"
rm -rf "$XDG_CONFIG_HOME/dibs-dev"

repo="$(new_repo nolist)"
seed "$repo" a.txt "fine"
check "a local run without any list fails" 2 run "$repo"
check "a CI run without any list passes with a notice" 0 env GITHUB_ACTIONS=true bash -c 'cd "$1" && "$2"' _ "$repo" "$script"
expect_output "the CI skip is announced" "::notice::"

repo="$(new_repo masked)"
seed "$repo" a.txt "$word"
check "CI masks each word before reporting" 1 env GITHUB_ACTIONS=true bash -c 'cd "$1" && "$2" "$3"' _ "$repo" "$script" "$list"
expect_output "the mask command is emitted" "::add-mask::$word"
first_line="$(printf '%s\n' "$output" | head -n 1)"
if [ "$first_line" = "::add-mask::$word" ]; then
  passed=$((passed + 1)); echo "ok   - the mask command comes first"
else
  failed=$((failed + 1)); echo "FAIL - the mask command comes first"
fi
without_masks="$(printf '%s\n' "$output" | grep -v '^::add-mask::' || true)"
if printf '%s' "$without_masks" | grep -q -i -F -- "$word"; then
  failed=$((failed + 1)); echo "FAIL - nothing but the mask command carries the word"
else
  passed=$((passed + 1)); echo "ok   - nothing but the mask command carries the word"
fi

check "a word list without words is refused" 2 run "$repo" <(printf '\n\r\n')
check "an unreadable word list is refused" 2 run "$repo" "$scratch/missing.txt"

finish
