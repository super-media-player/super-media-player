#!/usr/bin/env bash
# Build a newer playback engine (FFmpeg + libplacebo + libmpv) from source into
# .devlibs/engine/ (git-ignored), for packages that carry a newer FFmpeg than
# the distribution's. scripts/dev-run.sh and scripts/package-linux.sh use it
# whenever it is there; delete .devlibs/engine/ to go back to the system's.
#
# The three are built as one matched set: libmpv only works with the FFmpeg it
# was compiled against, so FFmpeg is never upgraded on its own.
#
#   scripts/build-engine.sh          build the pinned versions below
#   scripts/build-engine.sh --deps   only print the apt line for the build
#                                    dependencies, then exit
#
# Sources: FFmpeg's release tarball, checked against its GPG signature from
# the FFmpeg release key; libplacebo and mpv from their release tags (git,
# over HTTPS). Takes 10-20 minutes on a desktop machine. No root needed apart
# from installing the -dev packages once.
set -euo pipefail

FFMPEG_VERSION=9.1
LIBPLACEBO_TAG=v7.387.0
MPV_TAG=v0.42.0
# "FFmpeg release signing key <ffmpeg-devel@ffmpeg.org>", see ffmpeg.org/download.html
FFMPEG_KEY=FCF986EA15E6E293A5644F10B4322F04D67658D8

root="$(cd "$(dirname "$0")/.." && pwd)"
# engine-versions.conf (git-ignored) overrides the pins above with the same
# variable names; scripts/vm-daily.sh writes it when it moves to a newer
# point release.
# shellcheck source=/dev/null
[[ -f "$root/engine-versions.conf" ]] && source "$root/engine-versions.conf"
# Built beside the current engine and swapped in only when everything built,
# so a failed build leaves the last good engine in place. The libraries find
# each other through $ORIGIN, so the folder can move.
final="$root/.devlibs/engine"
prefix="$final.new"
work="$root/.devlibs/build-engine"

# Debian/Ubuntu packages the build needs (headers of the libraries FFmpeg and
# mpv link against, plus the tools).
apt_deps=(
    build-essential nasm meson ninja-build pkg-config git curl gpg patchelf
    python3-jinja2
    libgnutls28-dev libdav1d-dev libxml2-dev zlib1g-dev
    libdrm-dev libdisplay-info-dev libva-dev libegl-dev libgl-dev libgbm-dev
    libx11-dev libxext-dev libxrandr-dev libxinerama-dev libxss-dev libxpresent-dev
    libwayland-dev wayland-protocols libxkbcommon-dev
    libasound2-dev libpulse-dev libpipewire-0.3-dev
    libass-dev libbluray-dev libdvdnav-dev libdvdread-dev
    liblua5.2-dev libuchardet-dev liblcms2-dev libjpeg-dev
)
if [[ "${1:-}" == "--deps" ]]; then
    echo "sudo apt install ${apt_deps[*]}"
    exit 0
fi

missing=()
for p in "${apt_deps[@]}"; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed" || missing+=("$p")
done
if ((${#missing[@]})); then
    echo "error: build dependencies missing. Install them once with:" >&2
    echo "  sudo apt install ${missing[*]}" >&2
    exit 1
fi

jobs=$(nproc)
rm -rf "$work" "$prefix"
mkdir -p "$work"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# ---- FFmpeg ----
echo "==> FFmpeg $FFMPEG_VERSION"
cd "$work"
tarball="ffmpeg-$FFMPEG_VERSION.tar.xz"
curl -fsSL -o "$tarball" "https://ffmpeg.org/releases/$tarball"
curl -fsSL -o "$tarball.asc" "https://ffmpeg.org/releases/$tarball.asc"
export GNUPGHOME="$work/gnupg"
mkdir -m 700 "$GNUPGHOME"
gpg --quiet --keyserver hkps://keyserver.ubuntu.com --recv-keys "$FFMPEG_KEY"
# Only a good signature by exactly that key counts.
gpg --status-fd 1 --verify "$tarball.asc" "$tarball" 2>/dev/null \
    | grep -q "^\[GNUPG:\] VALIDSIG $FFMPEG_KEY " \
    || { echo "error: $tarball is not signed by the FFmpeg release key" >&2; exit 1; }
echo "    signature OK; sha256 $(sha256sum "$tarball" | cut -d' ' -f1)"
tar xJf "$tarball"
cd "ffmpeg-$FFMPEG_VERSION"
# Shared libraries only (no ffmpeg/ffprobe programs). FFmpeg's own decoders
# cover DVD, Blu-ray and nearly every file format; dav1d is the fast AV1
# decoder, gnutls gives https://, libxml2 DASH streams. VA-API and DRM
# (hardware decoding) are detected automatically.
./configure --prefix="$prefix" --libdir="$prefix/lib" \
    --enable-shared --disable-static --disable-programs --disable-doc \
    --enable-gpl --enable-version3 \
    --enable-gnutls --enable-libdav1d --enable-libxml2 --enable-libdrm
make -s -j"$jobs"
make -s install

# ---- libplacebo (mpv's renderer) ----
echo "==> libplacebo $LIBPLACEBO_TAG"
cd "$work"
git -c advice.detachedHead=false clone -q --depth 1 --branch "$LIBPLACEBO_TAG" \
    --recurse-submodules --shallow-submodules https://code.videolan.org/videolan/libplacebo.git
# The app draws through OpenGL; Vulkan would need newer headers than Ubuntu has.
meson setup libplacebo/build libplacebo --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dvulkan=disabled -Dopengl=enabled -Dglslang=disabled -Dshaderc=disabled \
    -Ddemos=false -Dtests=false
ninja -C libplacebo/build
ninja -C libplacebo/build install

# ---- mpv (libmpv only) ----
echo "==> mpv $MPV_TAG"
cd "$work"
git -c advice.detachedHead=false clone -q --depth 1 --branch "$MPV_TAG" https://github.com/mpv-player/mpv.git
# Wayland off: mpv 0.42 needs wayland-client 1.23 and Ubuntu 24.04 has 1.22
# (the packages take libwayland from the system, so a newer one can't be
# built here). The player draws the video itself through libmpv's render
# API, so mpv never opens a Wayland window; VA-API hardware decoding then
# opens the GPU through DRM (/dev/dri) instead of the Wayland display.
meson setup mpv/build mpv --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dlibmpv=true -Dcplayer=false -Dvulkan=disabled \
    -Dlua=lua5.2 -Djavascript=disabled \
    -Dlibbluray=enabled -Ddvdnav=enabled \
    -Dpulse=enabled -Dpipewire=enabled -Dalsa=enabled \
    -Dvaapi=enabled -Dvaapi-drm=enabled -Dwayland=disabled -Dx11=enabled -Degl=enabled -Ddrm=enabled
ninja -C mpv/build
ninja -C mpv/build install

# Each library finds its siblings in the same folder, so libmpv loads with
# no LD_LIBRARY_PATH (the packager sets the same again).
for f in "$prefix"/lib/*.so.*; do
    [[ -L "$f" ]] && continue
    patchelf --set-rpath '$ORIGIN' "$f"
done

rm -rf "$work" "$final"
mv "$prefix" "$final"
echo
echo "Engine installed to $final:"
echo "  FFmpeg $FFMPEG_VERSION, libplacebo $LIBPLACEBO_TAG, mpv $MPV_TAG"
echo "scripts/dev-run.sh and scripts/package-linux.sh now use it."
