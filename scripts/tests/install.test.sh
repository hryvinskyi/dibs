#!/usr/bin/env bash
# Runs the real scripts/install.sh, under every POSIX shell found among sh, dash and bash, against a
# release directory laid out the way GitHub lays out downloads, with the platform faked by shims of
# `uname` and `ldd` placed first on the PATH.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/../install.sh"
# shellcheck source=harness.sh
. "$here/harness.sh"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

targets=(
  aarch64-apple-darwin
  x86_64-apple-darwin
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
)

# make_release <release dir> <version> <download dir>: one archive per target, whose `dibs` prints
# the version and the target it was built for, and the SHA256SUMS that lists them.
make_release() {
  local root="$1" version="$2" dir="$3" target name pack
  mkdir -p "$dir"
  : >"$dir/SHA256SUMS"
  for target in "${targets[@]}"; do
    name="dibs-$version-$target"
    pack="$root/pack/$name"
    mkdir -p "$pack"
    printf '#!/bin/sh\necho "dibs %s (%s)"\n' "$version" "$target" >"$pack/dibs"
    chmod 755 "$pack/dibs"
    tar -czf "$dir/$name.tar.gz" -C "$root/pack" "$name"
    printf '%s  %s\n' "$(sha256 "$dir/$name.tar.gz")" "$name.tar.gz" >>"$dir/SHA256SUMS"
  done
}

# A release 0.1.0 and a newer 0.2.0, which is the latest.
release="$scratch/release"
make_release "$release/0.1.0" 0.1.0 "$release/download/v0.1.0"
make_release "$release/0.2.0" 0.2.0 "$release/download/v0.2.0"
mkdir -p "$release/latest"
cp -R "$release/download/v0.2.0" "$release/latest/download"

# The platform shims.
shims="$scratch/shims"
mkdir -p "$shims"
cat >"$shims/uname" <<'SHIM'
#!/bin/sh
case "$1" in
  -s) echo "$FAKE_OS" ;;
  -m) echo "$FAKE_MACHINE" ;;
  *) echo "$FAKE_OS" ;;
esac
SHIM
cat >"$shims/ldd" <<'SHIM'
#!/bin/sh
if [ "${FAKE_LIBC:-glibc}" = "musl" ]; then
  echo "musl libc (x86_64)" >&2
  echo "Version 1.2.5" >&2
  exit 1
fi
echo "ldd (GNU libc) 2.39"
SHIM
chmod 755 "$shims/uname" "$shims/ldd"

# Which release directory and which libc the next install sees.
root="$release"
libc=glibc

# install <shell> <os> <machine> [script arguments...]: runs the installer into $dest as that
# shell, on that platform, downloading from $root.
install() {
  local shell="$1" os="$2" machine="$3"
  shift 3
  FAKE_OS="$os" FAKE_MACHINE="$machine" FAKE_LIBC="$libc" \
    PATH="$shims:$PATH" DIBS_RELEASE_URL="file://$root" DIBS_INSTALL_DIR="$dest" \
    "$shell" "$script" "$@"
}

# copy_release <name>: a copy of the release directory that a test may damage.
copy_release() {
  cp -R "$release" "$scratch/$1"
  printf '%s' "$scratch/$1"
}

archive_name=dibs-0.1.0-x86_64-unknown-linux-gnu.tar.gz

# Only the shells this machine has.
shells=()
for candidate in sh dash bash; do
  if command -v "$candidate" >/dev/null 2>&1; then
    shells+=("$candidate")
  fi
done

for shell in "${shells[@]}"; do
  echo "# $shell"

  dest="$scratch/$shell-ok"
  check "a good checksum installs the requested release" 0 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the install is announced" "Installed $dest/dibs"
  check "the installed binary runs" 0 "$dest/dibs"
  expect_output "it is the requested version for this target" "dibs 0.1.0 (x86_64-unknown-linux-gnu)"

  dest="$scratch/$shell-v"
  check "a version written with a leading v installs the same release" 0 install "$shell" Linux x86_64 --version=v0.1.0
  check "and runs" 0 "$dest/dibs"
  expect_output "it is that release" "dibs 0.1.0"

  dest="$scratch/$shell-latest"
  check "with no version, the latest release installs" 0 install "$shell" Linux x86_64
  check "and runs" 0 "$dest/dibs"
  expect_output "it is the newest release" "dibs 0.2.0 (x86_64-unknown-linux-gnu)"

  platforms=(
    "Darwin arm64 aarch64-apple-darwin"
    "Darwin x86_64 x86_64-apple-darwin"
    "Linux aarch64 aarch64-unknown-linux-gnu"
    "Linux arm64 aarch64-unknown-linux-gnu"
    "Linux amd64 x86_64-unknown-linux-gnu"
  )
  for platform in "${platforms[@]}"; do
    read -r os machine expected <<<"$platform"
    dest="$scratch/$shell-$os-$machine"
    check "$os on $machine installs the $expected archive" 0 install "$shell" "$os" "$machine" --version 0.1.0
    check "and runs" 0 "$dest/dibs"
    expect_output "and is the $expected build" "($expected)"
  done

  # The archive was changed after the sums were made.
  root="$(copy_release "tampered-$shell")"
  printf 'not the archive that was released' >>"$root/download/v0.1.0/$archive_name"
  dest="$scratch/$shell-tampered"
  check "an archive that does not match SHA256SUMS is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the refusal names the checksum" "checksum"
  check "nothing was installed" 1 test -e "$dest/dibs"

  # The sums list another digest than the archive has.
  root="$(copy_release "wrong-sum-$shell")"
  sed 's/^[0-9a-f]\{8\}/00000000/' "$release/download/v0.1.0/SHA256SUMS" >"$root/download/v0.1.0/SHA256SUMS"
  dest="$scratch/$shell-wrong-sum"
  check "a SHA256SUMS that lists another digest is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the refusal names the checksum" "checksum"
  check "nothing was installed" 1 test -e "$dest/dibs"

  # An earlier installation survives a refused one.
  dest="$scratch/$shell-kept"
  root="$release"
  install "$shell" Linux x86_64 --version 0.2.0 >/dev/null 2>&1
  root="$scratch/tampered-$shell"
  check "a refused install over an installed one is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  check "the installed dibs still runs" 0 "$dest/dibs"
  expect_output "as the release it was" "dibs 0.2.0"

  # The release does not list the archive.
  root="$(copy_release "unlisted-$shell")"
  grep -v 'x86_64-unknown-linux-gnu' "$release/download/v0.1.0/SHA256SUMS" >"$root/download/v0.1.0/SHA256SUMS"
  dest="$scratch/$shell-unlisted"
  check "an archive the release does not list is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the refusal says so" "lists no archive"

  root="$release"
  dest="$scratch/$shell-missing-release"
  check "a release that does not exist is refused" 1 install "$shell" Linux x86_64 --version 9.9.9
  expect_output "the refusal says what could not be downloaded" "could not download"
  check "nothing was installed" 1 test -e "$dest/dibs"

  # Platforms there is no release for.
  for platform in "FreeBSD amd64" "Linux riscv64" "Darwin ppc" "SunOS i86pc" "Linux i686"; do
    read -r os machine <<<"$platform"
    dest="$scratch/$shell-$os-$machine"
    check "$os on $machine is refused" 1 install "$shell" "$os" "$machine" --version 0.1.0
    expect_output "the refusal says there is no release" "no dibs release for $os on $machine"
    check "nothing was installed" 1 test -e "$dest/dibs"
  done

  dest="$scratch/$shell-windows"
  check "a Windows shell is pointed to install.ps1" 1 install "$shell" MINGW64_NT-10.0 x86_64 --version 0.1.0
  expect_output "by name" "install.ps1"

  dest="$scratch/$shell-musl"
  libc=musl
  check "a Linux system with musl is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  libc=glibc
  expect_output "the refusal says why" "glibc"
  check "nothing was installed" 1 test -e "$dest/dibs"

  dest="$scratch/$shell-badversion"
  for version in '../etc' 'a/b' '1.0 ; touch x'; do
    check "the version '$version' is refused" 1 install "$shell" Linux x86_64 --version "$version"
  done
  check "an unknown option is refused" 1 install "$shell" Linux x86_64 --verbose
  check "--help prints the usage" 0 install "$shell" Linux x86_64 --help
  expect_output "the usage names the version option" "--version"
  check "--help installs nothing" 1 test -e "$dest/dibs"
done

finish
