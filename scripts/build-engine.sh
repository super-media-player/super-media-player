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
# Built from source only where the distribution's is too old (Ubuntu 22.04,
# the base of the release packages); SHA-256 taken when pinned, 2026-11-25.
# libdrm: only to compile against (mpv needs >= 2.4.114 headers); the
# packages use the system's libdrm, which must match its GPU drivers.
LIBDRM_VERSION=2.4.124
LIBDRM_SHA256=ac36293f61ca4aafaf4b16a2a7afff312aa4f5c37c9fbd797de9e3c0863ca379
# dav1d (the AV1 decoder): FFmpeg 9 needs 1.0, 22.04 has 0.9; bundled.
DAV1D_VERSION=1.5.4
DAV1D_SHA256=686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5
# libdisplay-info: bundled with the packages (not part of 22.04).
DISPLAYINFO_VERSION=0.2.0
DISPLAYINFO_SHA256=5a2f002a16f42dd3540c8846f80a90b8f4bdcd067a94b9d2087bc2feae974176

root="$(cd "$(dirname "$0")/.." && pwd)"
# engine-versions.conf (git-ignored) overrides the pins above with the same
# variable names; a build machine's daily check can write it to move to a
# newer point release.
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
    libx264-dev libmp3lame-dev libopus-dev
    libdrm-dev libva-dev libegl-dev libgl-dev libgbm-dev hwdata
    libx11-dev libxext-dev libxrandr-dev libxinerama-dev libxss-dev libxpresent-dev
    libwayland-dev wayland-protocols libxkbcommon-dev
    libasound2-dev libpulse-dev
    libass-dev libbluray-dev libdvdnav-dev libdvdread-dev libcdio-paranoia-dev
    liblua5.2-dev libuchardet-dev liblcms2-dev libjpeg-dev
)
if [[ "${1:-}" == "--deps" ]]; then
    echo "sudo apt install ${apt_deps[*]}"
    exit 0
fi

# Used when the system has them (Ubuntu 24.04: libpipewire-0.3-dev,
# libdisplay-info-dev): the PipeWire output needs
# libpipewire 0.3.57 (22.04 has 0.3.48; then mpv uses the PulseAudio
# output, which PipeWire desktops serve too), libdisplay-info is built below.
missing=()
for p in "${apt_deps[@]}"; do
    # mpv needs meson 1.3; the container has a newer one from pip.
    if [[ "$p" == meson ]] && command -v meson >/dev/null &&
        [[ "$(printf '%s\n' 1.3.0 "$(meson --version)" | sort -V | head -n 1)" == 1.3.0 ]]; then
        continue
    fi
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
buildonly="$work/buildonly"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$buildonly/lib/pkgconfig"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# fetch <url> <sha256> <file>: download and check.
fetch() {
    curl -fsSL -o "$3" "$1"
    echo "$2  $3" | sha256sum -c --quiet || { echo "error: $3 doesn't match its pinned checksum" >&2; exit 1; }
}

# ---- libraries the distribution has too old (or not at all) ----
if ! pkg-config --atleast-version=2.4.114 libdrm; then
    echo "==> libdrm $LIBDRM_VERSION (to compile against only)"
    cd "$work"
    fetch "https://dri.freedesktop.org/libdrm/libdrm-$LIBDRM_VERSION.tar.xz" "$LIBDRM_SHA256" libdrm.tar.xz
    tar xJf libdrm.tar.xz
    meson setup libdrm-build "libdrm-$LIBDRM_VERSION" --prefix="$buildonly" --libdir=lib --buildtype=release \
        -Dintel=disabled -Dradeon=disabled -Damdgpu=disabled -Dnouveau=disabled -Dvmwgfx=disabled \
        -Dfreedreno=disabled -Dvc4=disabled -Detnaviv=disabled -Dtests=false -Dman-pages=disabled \
        -Dvalgrind=disabled -Dcairo-tests=disabled -Dudev=false
    ninja -C libdrm-build install >/dev/null
fi
if ! pkg-config --atleast-version=0.1.1 libdisplay-info; then
    echo "==> libdisplay-info $DISPLAYINFO_VERSION"
    cd "$work"
    fetch "https://gitlab.freedesktop.org/emersion/libdisplay-info/-/releases/$DISPLAYINFO_VERSION/downloads/libdisplay-info-$DISPLAYINFO_VERSION.tar.xz" \
        "$DISPLAYINFO_SHA256" displayinfo.tar.xz
    tar xJf displayinfo.tar.xz
    meson setup displayinfo-build "libdisplay-info-$DISPLAYINFO_VERSION" --prefix="$prefix" --libdir=lib --buildtype=release
    ninja -C displayinfo-build install >/dev/null
fi
if ! pkg-config --atleast-version=1.0.0 dav1d; then
    echo "==> dav1d $DAV1D_VERSION"
    cd "$work"
    fetch "https://downloads.videolan.org/pub/videolan/dav1d/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.xz" "$DAV1D_SHA256" dav1d.tar.xz
    tar xJf dav1d.tar.xz
    meson setup dav1d-build "dav1d-$DAV1D_VERSION" --prefix="$prefix" --libdir=lib --buildtype=release \
        -Denable_tools=false -Denable_tests=false -Denable_examples=false
    ninja -C dav1d-build install >/dev/null
fi
pipewire=disabled
pkg-config --atleast-version=0.3.57 libpipewire-0.3 && pipewire=enabled
echo "    PipeWire output: $pipewire"

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
# (hardware decoding) are detected automatically. x264, LAME and Opus are
# the encoders of saved clips (MP4, MP3, Opus) and of casting.
./configure --prefix="$prefix" --libdir="$prefix/lib" \
    --enable-shared --disable-static --disable-programs --disable-doc \
    --enable-gpl --enable-version3 \
    --enable-gnutls --enable-libdav1d --enable-libxml2 --enable-libdrm \
    --enable-libx264 --enable-libmp3lame --enable-libopus
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
# cdda: audio CDs. Lazy binding: built against a newer libdrm than 22.04
# has, libmpv names a few functions only its own DRM video output calls
# (the player never uses it), so they must not be needed when it loads.
meson setup mpv/build mpv --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dc_link_args=-Wl,-z,lazy \
    -Dlibmpv=true -Dcplayer=false -Dvulkan=disabled \
    -Dlua=lua5.2 -Djavascript=disabled \
    -Dlibbluray=enabled -Ddvdnav=enabled -Dcdda=enabled \
    -Dpulse=enabled -Dpipewire=$pipewire -Dalsa=enabled \
    -Dvaapi=enabled -Dvaapi-drm=enabled -Dwayland=disabled -Dx11=enabled -Degl=enabled -Ddrm=enabled
ninja -C mpv/build
ninja -C mpv/build install

# Each library finds its siblings in the same folder, so libmpv loads with
# no LD_LIBRARY_PATH (the packager sets the same again).
for f in "$prefix"/lib/*.so.*; do
    [[ -L "$f" ]] && continue
    patchelf --set-rpath '$ORIGIN' "$f"
done

# Every encoder and input the player uses must be in this engine.
need_features=(cdda dvdnav libbluray libass lua5.2 vaapi)
features=$(strings "$prefix/lib/libmpv.so.2" | grep -m1 '^List of enabled features:' || true)
for f in "${need_features[@]}"; do
    grep -qw -- "$f" <<<"$features" || { echo "error: libmpv was built without $f" >&2; exit 1; }
done
# (Read whole first: grep -q stopping early would fail strings under pipefail.)
ffconfig=$(strings "$(readlink -f "$prefix/lib/libavcodec.so")" | grep -m1 -- '--prefix=' || true)
for lib in libx264 libmp3lame libopus; do
    grep -q -- "--enable-$lib" <<<"$ffconfig" || { echo "error: FFmpeg was built without $lib" >&2; exit 1; }
done

rm -rf "$work" "$final"
mv "$prefix" "$final"
echo
echo "Engine installed to $final:"
echo "  FFmpeg $FFMPEG_VERSION, libplacebo $LIBPLACEBO_TAG, mpv $MPV_TAG"
echo "scripts/dev-run.sh and scripts/package-linux.sh now use it."
