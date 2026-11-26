#!/usr/bin/env bash
# Build the macOS package on a Mac (GitHub Actions: .github/workflows/release.yml):
#
#   dist/super-media-player-<version>-macos-<arch>.dmg    the app, drag to Applications
#
# libmpv and everything it links to come from Homebrew (`brew install mpv
# dylibbundler librsvg`) and are copied into the app with dylibbundler, so the
# Mac it goes to needs nothing installed. yt-dlp's macOS build is bundled too
# (checked against the release's checksum list; SMP_NO_YTDLP=1 leaves it out).
#
# The app is not signed with an Apple Developer ID: the first start needs a
# right-click → Open (or System Settings → Privacy & Security → Open Anyway).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
for tool in cargo brew dylibbundler rsvg-convert iconutil hdiutil curl shasum; do
    command -v "$tool" >/dev/null || { echo "error: '$tool' is required (brew install mpv dylibbundler librsvg)" >&2; exit 1; }
done
version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
arch=$(uname -m)
libmpv="$(brew --prefix mpv)/lib/libmpv.2.dylib"
[[ -e "$libmpv" ]] || { echo "error: no $libmpv: brew install mpv" >&2; exit 1; }

echo "==> building"
cargo build --release -p super-media-player

app="dist/build-macos/Super Media Player.app"
rm -rf dist/build-macos
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
cp target/release/super-media-player "$app/Contents/MacOS/"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"

echo "==> libmpv and its libraries"
# The player looks for libmpv in lib/ next to itself: Contents/MacOS/lib.
cp -L "$libmpv" "$app/Contents/Frameworks/libmpv.2.dylib"
chmod u+w "$app/Contents/Frameworks/libmpv.2.dylib"
dylibbundler -of -b -cd -x "$app/Contents/Frameworks/libmpv.2.dylib" \
    -d "$app/Contents/Frameworks" -p @loader_path/ >/dev/null
install_name_tool -id @loader_path/libmpv.2.dylib "$app/Contents/Frameworks/libmpv.2.dylib"
ln -s ../Frameworks "$app/Contents/MacOS/lib"
# Changed libraries need a new (ad-hoc) signature on Apple silicon.
codesign --force --sign - "$app/Contents/Frameworks/"*.dylib >/dev/null 2>&1 || true

if [[ -z "${SMP_NO_YTDLP:-}" ]]; then
    echo "==> yt-dlp"
    rel=https://github.com/yt-dlp/yt-dlp/releases/latest/download
    curl -fsSL -o dist/build-macos/SUMS "$rel/SHA2-256SUMS"
    want=$(awk '$2 == "yt-dlp_macos" {print $1}' dist/build-macos/SUMS)
    curl -fsSL -o "$app/Contents/MacOS/yt-dlp" "$rel/yt-dlp_macos"
    echo "$want  $app/Contents/MacOS/yt-dlp" | shasum -a 256 -c --quiet
    chmod 755 "$app/Contents/MacOS/yt-dlp"
fi

echo "==> icon"
iconset=dist/build-macos/smp.iconset
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
    rsvg-convert -w $s -h $s packaging/linux/super-media-player.svg -o "$iconset/icon_${s}x${s}.png"
    rsvg-convert -w $((s * 2)) -h $((s * 2)) packaging/linux/super-media-player.svg -o "$iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/smp.icns"
cp LICENSE "$app/Contents/Resources/LICENSE.txt"
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || true

echo "==> disk image"
dmg="dist/super-media-player-$version-macos-$arch.dmg"
staging=dist/build-macos/dmg
mkdir -p "$staging"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "Super Media Player" -srcfolder "$staging" -ov -format UDZO "$dmg"
echo "==> $dmg ($(du -h "$dmg" | cut -f1))"
