#!/usr/bin/env bash
# Runs the real scripts/install.sh, under every POSIX shell found among sh, dash and bash, against a
# release directory laid out the way GitHub lays out downloads, with the platform faked by shims of
# `uname` and `ldd` placed first on the PATH. The releases' SHA256SUMS are signed with a minisign key
# made for the run, and the script is also run with that key written in by
# scripts/stamp-release-key.sh, as the release workflow writes in the project's key. minisign must be
# installed.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/../install.sh"
stamp="$here/../stamp-release-key.sh"
# shellcheck source=harness.sh
. "$here/harness.sh"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

if ! command -v minisign >/dev/null 2>&1; then
  echo "minisign is needed to sign the test releases; install it (brew install minisign, apt install minisign)." >&2
  exit 1
fi

# The key the test releases are signed with, and another one.
keys="$scratch/keys"
mkdir -p "$keys"
minisign -G -W -p "$keys/good.pub" -s "$keys/good.key" >/dev/null
minisign -G -W -p "$keys/other.pub" -s "$keys/other.key" >/dev/null

# sign <SHA256SUMS> <secret key>: writes the file's signature beside it.
sign() {
  rm -f "$1.minisig"
  minisign -S -s "$2" -m "$1" -x "$1.minisig" >/dev/null
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
  sign "$dir/SHA256SUMS" "$keys/good.key"
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

# Which release directory, libc and PATH after the shims the next install sees.
root="$release"
libc=glibc
path_tail="$PATH"

# The script with the run's key written in, and with the repository's own key file.
signed="$scratch/install-signed.sh"
"$stamp" "$keys/good.pub" "$script" "$signed"
as_released="$scratch/install-as-released.sh"
"$stamp" "$here/../../keys/release.pub" "$script" "$as_released"

# A PATH holding every tool the script runs but minisign.
toolbox="$scratch/toolbox"
mkdir -p "$toolbox"
for tool in sh dash bash curl wget sha256sum shasum openssl cut tr sed mktemp rm mkdir tar gzip cp chmod mv cat grep head perl env; do
  found="$(command -v "$tool" 2>/dev/null || true)"
  case "$found" in
    /*) ln -s "$found" "$toolbox/$tool" ;;
  esac
done

# install <shell> <os> <machine> [script arguments...]: runs the installer into $dest as that
# shell, on that platform, downloading from $root.
install() {
  local shell="$1" os="$2" machine="$3"
  shift 3
  FAKE_OS="$os" FAKE_MACHINE="$machine" FAKE_LIBC="$libc" \
    PATH="$shims:$path_tail" DIBS_RELEASE_URL="file://$root" DIBS_INSTALL_DIR="$dest" \
    "$shell" "${installer:-$script}" "$@"
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
  expect_output "a script with no key written in says it cannot verify signatures" "carries no release signing key"
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

  # The script as a release builds it: the run's key written in.
  installer="$signed"
  root="$release"
  dest="$scratch/$shell-signed"
  check "a release signed by the key written in installs" 0 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the signature was checked" "signature verifies"
  check "and runs" 0 "$dest/dibs"
  dest="$scratch/$shell-signed-latest"
  check "the latest release is checked the same way" 0 install "$shell" Linux x86_64
  expect_output "its signature too" "signature verifies"

  root="$(copy_release "other-key-$shell")"
  sign "$root/download/v0.1.0/SHA256SUMS" "$keys/other.key"
  dest="$scratch/$shell-other-key"
  check "a release signed by another key is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the refusal names the signature" "does not verify"
  check "nothing was installed" 1 test -e "$dest/dibs"

  root="$(copy_release "resummed-$shell")"
  printf '%s  %s\n' "$(printf 'x' >"$scratch/x" && sha256 "$scratch/x")" "extra.tar.gz" >>"$root/download/v0.1.0/SHA256SUMS"
  dest="$scratch/$shell-resummed"
  check "a SHA256SUMS changed after it was signed is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the refusal names the signature" "does not verify"
  check "nothing was installed" 1 test -e "$dest/dibs"

  root="$(copy_release "unsigned-$shell")"
  rm "$root/download/v0.1.0/SHA256SUMS.minisig"
  dest="$scratch/$shell-unsigned"
  check "a release without a signature is refused" 1 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "the refusal names the missing file" "SHA256SUMS.minisig"
  check "nothing was installed" 1 test -e "$dest/dibs"

  root="$release"
  path_tail="$toolbox"
  dest="$scratch/$shell-no-minisign"
  check "without minisign a good checksum still installs" 0 install "$shell" Linux x86_64 --version 0.1.0
  expect_output "and it says the signature was not checked" "minisign is not installed"
  check "and runs" 0 "$dest/dibs"
  path_tail="$PATH"

  installer="$as_released"
  dest="$scratch/$shell-as-released"
  case "$(sed -n 2p "$here/../../keys/release.pub")" in
    RW*)
      check "the script stamped with the repository's key refuses a release that key did not sign" 1 install "$shell" Linux x86_64 --version 0.1.0
      expect_output "it checked the signature against that key" "does not verify"
      ;;
    *)
      check "the script stamped with the placeholder key file installs" 0 install "$shell" Linux x86_64 --version 0.1.0
      expect_output "while that file holds the placeholder, it says it cannot verify" "carries no release signing key"
      ;;
  esac
  installer=""
done

# Stamping copies base64 characters only, so a key file cannot become shell syntax.
{
  echo "untrusted comment: not a key"
  echo "RW\$(touch $scratch/injected)';touch $scratch/injected;'"
} >"$keys/evil.pub"
"$stamp" "$keys/evil.pub" "$script" "$scratch/install-evil.sh"
dest="$scratch/evil"
installer="$scratch/install-evil.sh"
check "a stamped key file with shell syntax in it installs as a script without a key" 0 install sh Linux x86_64 --version 0.1.0
expect_output "and says so" "carries no release signing key"
check "and ran none of it" 1 test -e "$scratch/injected"
installer=""
check "a script with no place for the key is not stamped" 1 "$stamp" "$keys/good.pub" "$keys/good.pub" "$scratch/nowhere"

finish
