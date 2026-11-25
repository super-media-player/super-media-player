#!/usr/bin/env bash
# Build libprojectM 4 (the MilkDrop visualizer engine) for OpenGL ES 3 —
# the kind of context the player's window uses — and fetch the "Cream of
# the Crop" preset collection, into .devlibs/projectm/ (no root needed).
#
# scripts/dev-run.sh and scripts/package-linux.sh pick them up from there.
# Needs: cmake, a C++ compiler, and the GLES/EGL headers
# (sudo apt install cmake build-essential libgles-dev libegl-dev).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version=4.1.4
sha256=38cc45296bfe2dfc1c053c26c8e37be1f8cb277f8adbd541dfea077a8ecdb3ae
presets_commit=0180df21f5e0bd39b9060cc5de420ed2f1f9e509
presets_sha256=77ef8e527fb00343afdfb267f5a2e8d3d00430c563ca9f5ab2104fc306f5c674
dest="$root/.devlibs/projectm"
work="$root/.devlibs/build-projectm"

for tool in cmake c++ curl; do
    command -v "$tool" >/dev/null || { echo "needs $tool" >&2; exit 1; }
done
[[ -e /usr/include/GLES3/gl3.h ]] || { echo "needs the GLES headers: sudo apt install libgles-dev libegl-dev" >&2; exit 1; }

rm -rf "$work" && mkdir -p "$work"
cd "$work"
curl -fsSL -o src.tar.gz "https://github.com/projectM-visualizer/projectm/releases/download/v$version/libprojectM-$version.tar.gz"
echo "$sha256  src.tar.gz" | sha256sum -c --quiet
curl -fsSL -o presets.tar.gz "https://github.com/projectM-visualizer/presets-cream-of-the-crop/archive/$presets_commit.tar.gz"
echo "$presets_sha256  presets.tar.gz" | sha256sum -c --quiet
tar xzf src.tar.gz
cmake -S "libprojectM-$version" -B build -DCMAKE_BUILD_TYPE=Release -DENABLE_GLES=ON \
    -DENABLE_SYSTEM_PROJECTM_EVAL=OFF -DENABLE_PLAYLIST=OFF -DBUILD_TESTING=OFF \
    -DCMAKE_INSTALL_PREFIX="$work/install" -DCMAKE_INSTALL_LIBDIR=lib >/dev/null
cmake --build build -j"$(nproc)" >/dev/null
cmake --install build >/dev/null
rm -rf "$dest" && mkdir -p "$dest/lib" "$dest/presets"
cp -P install/lib/libprojectM-4*.so* "$dest/lib/"
tar xzf presets.tar.gz -C "$dest/presets" --strip-components=1
rm -rf "$work"
echo "libprojectM $version installed to $dest ($(find "$dest/presets" -name '*.milk' | wc -l) presets)"
