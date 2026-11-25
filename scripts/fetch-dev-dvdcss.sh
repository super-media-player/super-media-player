#!/usr/bin/env bash
# Build libdvdcss (DVD CSS decryption) from VideoLAN's source into .devlibs/,
# without root. libdvdread, used by libmpv for dvd://, loads it at runtime;
# without it copy-protected DVDs (most commercial ones) will not play.
# scripts/dev-run.sh and scripts/package-linux.sh pick it up from there.
#
# With root, `sudo apt install libdvd-pkg && sudo dpkg-reconfigure libdvd-pkg`
# installs it system-wide instead. Whether using libdvdcss is allowed depends
# on the laws where you live.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version=1.4.3
sha256=233cc92f5dc01c5d3a96f5b3582be7d5cee5a35a52d3a08158745d3d86070079
triplet=$(gcc -dumpmachine 2>/dev/null || echo "$(uname -m)-linux-gnu")
prefix="$root/.devlibs/root/usr"
work="$root/.devlibs/build-dvdcss"

rm -rf "$work" && mkdir -p "$work"
cd "$work"
curl -fsSL -o src.tar.bz2 "https://download.videolan.org/pub/libdvdcss/$version/libdvdcss-$version.tar.bz2"
echo "$sha256  src.tar.bz2" | sha256sum -c --quiet
tar xjf src.tar.bz2
cd "libdvdcss-$version"
./configure --quiet --prefix="$prefix" --libdir="$prefix/lib/$triplet" --disable-static --disable-doc
make -s -j"$(nproc)"
make -s install
rm -rf "$work"
echo "libdvdcss $version installed to $prefix/lib/$triplet"
