#!/bin/sh
# Rebuilds the local ALSA prefix used when building on machines without
# root (no `apt install libasound2-dev`). Downloads the Debian 13
# libasound2/runtime + dev packages, extracts them under
# /home/z/my-project/.alsa-prefix and rewrites the embedded alsa.pc
# prefix so pkg-config resolves to the prefix.
#
# Normal Linux machines do NOT need this script: install libasound2-dev.
set -eu
BASE="http://deb.debian.org/debian/pool/main/a/alsa-lib"
VERSION="1.2.14-1+deb13u1"
DEBS="libasound2t64_${VERSION}_amd64.deb libasound2-dev_${VERSION}_amd64.deb"
PREFIX="/home/z/my-project/.alsa-prefix"
DL="$(dirname "$PREFIX")/.alsa-dl"

mkdir -p "$DL" "$PREFIX"
cd "$DL"
for d in $DEBS; do
    [ -f "$d" ] || curl -fsSO "$BASE/$d"
done
for d in $DEBS; do
    dpkg-deb -x "$d" "$PREFIX/"
done
# Rewrite the .pc so -I/-L point at the prefix, not /usr.
for pc in "$PREFIX"/usr/lib/*/pkgconfig/alsa.pc; do
    sed -i "s|^prefix=/usr\$|prefix=$PREFIX/usr|" "$pc"
done
echo "ALSA prefix ready at $PREFIX"
