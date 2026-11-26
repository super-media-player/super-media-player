#!/usr/bin/env bash
# Build the Windows packages of the desktop app (64-bit), cross-compiled on
# Linux in the smp-build container (scripts/in-container.sh):
#
#   dist/super-media-player-<version>-windows-x64-setup.exe   installer (NSIS)
#   dist/super-media-player-<version>-windows-x64.zip         portable folder
#
# Bundled: libmpv with its FFmpeg (shinchiro's mpv-winbuild, pinned below and
# checked against its SHA-256) and yt-dlp.exe (checked against the release's
# checksum list; SMP_NO_YTDLP=1 leaves it out).
#
# Windows has no copy-protected DVD / Blu-ray decryption here, and audio CDs
# and drive detection beyond drive letters are Linux-only so far.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# shinchiro/mpv-winbuild-cmake release 20261111 (mpv git bcde6b93cc), the
# x86-64 baseline build (the "v3" one needs AVX2). SHA-256 as GitHub lists it.
MPV_URL=https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261111/mpv-dev-x86_64-20261111-git-bcde6b93cc.7z
MPV_SHA256=273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452

for tool in cargo cargo-xwin makensis 7z rsvg-convert zip curl sha256sum python3; do
    command -v "$tool" >/dev/null || { echo "error: '$tool' is required (run in the smp-build container)" >&2; exit 1; }
done
version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
version="$version${SMP_PACKAGE_SUFFIX:-}"
target=x86_64-pc-windows-msvc
cache="$root/dist/cache"
mkdir -p "$cache"

echo "==> building the Windows binary"
cargo xwin build --release --target "$target" -p super-media-player

stage="$root/dist/build-windows/Super Media Player"
rm -rf "$root/dist/build-windows"
mkdir -p "$stage"
cp "target/$target/release/super-media-player.exe" "$stage/"

echo "==> libmpv"
mpv7z="$cache/$(basename "$MPV_URL")"
if ! { [[ -f "$mpv7z" ]] && echo "$MPV_SHA256  $mpv7z" | sha256sum -c --quiet >/dev/null 2>&1; }; then
    curl -fsSL -o "$mpv7z" "$MPV_URL"
    echo "$MPV_SHA256  $mpv7z" | sha256sum -c --quiet
fi
7z e -y -bso0 -o"$stage" "$mpv7z" libmpv-2.dll >/dev/null
[[ -f "$stage/libmpv-2.dll" ]] || { echo "error: no libmpv-2.dll in $mpv7z" >&2; exit 1; }

if [[ -z "${SMP_NO_YTDLP:-}" ]]; then
    echo "==> yt-dlp.exe"
    rel=https://github.com/yt-dlp/yt-dlp/releases/latest/download
    curl -fsSL -o "$cache/SHA2-256SUMS" "$rel/SHA2-256SUMS"
    want=$(awk '$2 == "yt-dlp.exe" {print $1}' "$cache/SHA2-256SUMS")
    [[ -n "$want" ]] || { echo "error: no checksum for yt-dlp.exe" >&2; exit 1; }
    if ! { [[ -f "$cache/yt-dlp.exe" ]] && echo "$want  $cache/yt-dlp.exe" | sha256sum -c --quiet >/dev/null 2>&1; }; then
        curl -fsSL -o "$cache/yt-dlp.exe" "$rel/yt-dlp.exe"
        echo "$want  $cache/yt-dlp.exe" | sha256sum -c --quiet
    fi
    cp "$cache/yt-dlp.exe" "$stage/"
fi

# The icon, from the SVG: PNGs of the usual sizes packed into one .ico.
icons="$root/dist/build-windows/icons"
mkdir -p "$icons"
for s in 16 24 32 48 64 128 256; do
    rsvg-convert -w "$s" -h "$s" packaging/linux/super-media-player.svg -o "$icons/$s.png"
done
python3 -I - "$icons" "$stage/smp.ico" <<'PY'
import struct, sys, pathlib
src, out = pathlib.Path(sys.argv[1]), sys.argv[2]
pngs = [(int(p.stem), p.read_bytes()) for p in sorted(src.glob("*.png"), key=lambda p: int(p.stem))]
head = struct.pack("<HHH", 0, 1, len(pngs))
offset = 6 + 16 * len(pngs)
entries, data = b"", b""
for size, png in pngs:
    dim = 0 if size >= 256 else size
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset + len(data))
    data += png
open(out, "wb").write(head + entries + data)
PY

# Text files with Windows line ends.
sed 's/$/\r/' LICENSE > "$stage/LICENSE.txt"
sed 's/$/\r/' README.md > "$stage/README.txt"

echo "==> installer"
setup="dist/super-media-player-$version-windows-x64-setup.exe"
nsis_version=$(sed 's/[^0-9.].*//' <<<"$version")
makensis -V2 -DVERSION="$nsis_version" -DSTAGE="$stage" -DOUT="$root/$setup" packaging/windows/installer.nsi
echo "==> $setup ($(du -h "$setup" | cut -f1))"

portable="dist/super-media-player-$version-windows-x64.zip"
rm -f "$portable"
(cd "$root/dist/build-windows" && zip -qr -9 "$root/$portable" "Super Media Player")
echo "==> $portable ($(du -h "$portable" | cut -f1))"
