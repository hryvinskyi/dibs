#!/bin/sh
# Installs the dibs command line from a release: picks the archive for this machine, checks it
# against the release's SHA256SUMS, and puts `dibs` in ~/.local/bin.
#
#   curl -fsSL https://github.com/hryvinskyi/dibs/releases/latest/download/install.sh | sh
#   curl -fsSL https://github.com/hryvinskyi/dibs/releases/latest/download/install.sh | sh -s -- --version 1.2.3
#
# Options:
#   --version <v>   the release to install (1.2.3 or v1.2.3); the latest when absent
#   -h, --help      this text
#
# Environment:
#   DIBS_INSTALL_DIR   where `dibs` goes (default: $HOME/.local/bin)
#   DIBS_RELEASE_URL   where releases are downloaded from, laid out as GitHub lays them out
#                      (default: https://github.com/hryvinskyi/dibs/releases)
#
# Nothing is installed unless the archive's checksum matches the release's SHA256SUMS.
set -eu

release_url="${DIBS_RELEASE_URL:-https://github.com/hryvinskyi/dibs/releases}"
version=""

say() { printf '%s\n' "$*"; }
die() {
  printf 'dibs install: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'TEXT'
Usage: install.sh [--version <v>]

Installs the dibs command line from a release into $DIBS_INSTALL_DIR, or ~/.local/bin.
The archive is checked against the release's SHA256SUMS first.

  --version <v>   the release to install (1.2.3 or v1.2.3); the latest when absent
  -h, --help      this text
TEXT
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version needs a value, such as --version 1.2.3."
      version="$2"
      shift 2
      ;;
    --version=*)
      version="${1#--version=}"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown option '$1'. Use --version <v> to pick a release."
      ;;
  esac
done

version="${version#v}"
case "$version" in
  *[!0-9A-Za-z.+-]*) die "'$version' is not a release version." ;;
esac

# The target triple of this machine's release archive.
detect_target() {
  os="$(uname -s)"
  machine="$(uname -m)"
  case "$os" in
    Darwin)
      case "$machine" in
        arm64 | aarch64) target="aarch64-apple-darwin" ;;
        x86_64) target="x86_64-apple-darwin" ;;
        *) die "there is no dibs release for $os on $machine." ;;
      esac
      ;;
    Linux)
      case "$machine" in
        x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
        aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
        *) die "there is no dibs release for $os on $machine." ;;
      esac
      if ldd --version 2>&1 | grep -qi musl; then
        die "the Linux releases need glibc, and this system uses musl."
      fi
      ;;
    MINGW* | MSYS* | CYGWIN*)
      die "on Windows, install with install.ps1 from the same release, in PowerShell."
      ;;
    *)
      die "there is no dibs release for $os on $machine."
      ;;
  esac
}

# fetch <url> <file>: downloads over https (or from a file, for a local release directory).
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https,file' --proto-redir '=https' --retry 2 -o "$2" "$1" ||
      die "could not download $1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1" || die "could not download $1"
  else
    die "curl or wget is needed to download the release."
  fi
}

# sha256_of <file>: the file's SHA-256 digest in lower-case hex.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/^.*= //'
  else
    die "sha256sum, shasum or openssl is needed to check the download."
  fi
}

# listed_hash <SHA256SUMS> <archive>: the digest the file lists for the archive, if it does.
listed_hash() {
  while read -r hash name; do
    name="${name#\*}"
    if [ "$name" = "$2" ]; then
      printf '%s\n' "$hash" | tr 'A-F' 'a-f'
      return 0
    fi
  done <"$1"
  return 1
}

# latest_version <SHA256SUMS>: the version of the archive listed for this machine's target.
latest_version() {
  while read -r _ name; do
    name="${name#\*}"
    case "$name" in
      dibs-*-"$target".tar.gz)
        found="${name#dibs-}"
        printf '%s\n' "${found%-"$target".tar.gz}"
        return 0
        ;;
    esac
  done <"$1"
  return 1
}

detect_target

install_dir="${DIBS_INSTALL_DIR:-${HOME:?HOME is not set; set DIBS_INSTALL_DIR to where dibs should go}/.local/bin}"
work="$(mktemp -d "${TMPDIR:-/tmp}/dibs-install.XXXXXX")" || die "could not make a temporary directory."
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

if [ -z "$version" ]; then
  fetch "$release_url/latest/download/SHA256SUMS" "$work/latest-sums"
  version="$(latest_version "$work/latest-sums")" ||
    die "the latest release lists no archive for $target."
fi

base="$release_url/download/v$version"
archive="dibs-$version-$target.tar.gz"

say "Installing dibs $version for $target."
fetch "$base/SHA256SUMS" "$work/SHA256SUMS"
expected="$(listed_hash "$work/SHA256SUMS" "$archive")" ||
  die "release $version lists no archive named $archive."
fetch "$base/$archive" "$work/$archive"
actual="$(sha256_of "$work/$archive")"
if [ "$actual" != "$expected" ]; then
  die "the checksum of $archive does not match the release's SHA256SUMS; nothing was installed."
fi

mkdir "$work/unpacked"
tar -xzf "$work/$archive" -C "$work/unpacked" || die "could not unpack $archive."
binary="$work/unpacked/dibs-$version-$target/dibs"
[ -f "$binary" ] || die "$archive holds no dibs binary."

mkdir -p "$install_dir" || die "could not create $install_dir."
cp "$binary" "$install_dir/.dibs.new.$$" || die "could not write into $install_dir."
chmod 755 "$install_dir/.dibs.new.$$"
mv -f "$install_dir/.dibs.new.$$" "$install_dir/dibs"

say "Installed $install_dir/dibs."
case ":${PATH:-}:" in
  *":$install_dir:"*) ;;
  *) say "$install_dir is not on your PATH; add it to your shell's profile." ;;
esac
say "Next: dibs login --server <url>, inside a checkout that has a .dibs.toml."
