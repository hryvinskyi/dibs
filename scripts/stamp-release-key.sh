#!/bin/sh
# Writes the release's install script with the public release key in it, so the script and the
# binaries trust one key, the one in keys/release.pub.
#
# Usage: scripts/stamp-release-key.sh <release.pub> <install.sh> <output>
#
# The key is the second line of the minisign public key file. Only base64 characters are copied,
# so nothing in the file can become shell syntax. The placeholder kept before a key is made copies
# as text that is no key, and the installed script then says it cannot verify signatures.
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <release.pub> <install.sh> <output>" >&2
  exit 2
fi

key="$(sed -n '2p' "$1" | tr -cd 'A-Za-z0-9+/=')"
if ! grep -q "@DIBS_RELEASE_KEY@" "$2"; then
  echo "$2 has no place for the release key." >&2
  exit 1
fi
sed "s|@DIBS_RELEASE_KEY@|$key|" "$2" >"$3"
chmod 755 "$3"
