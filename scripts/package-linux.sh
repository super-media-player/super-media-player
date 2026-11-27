#!/usr/bin/env bash
# Build self-contained Linux packages of the desktop app:
#
#   dist/super-media-player-<version>-linux-<arch>.tar.gz   portable folder, no root needed
#   dist/super-media-player_<version>_<debarch>.deb         installs to /opt/super-media-player
#   dist/super-media-player-<version>-<arch>.rpm            the same for Fedora / openSUSE
#                                                           (when rpmbuild is installed)
#   dist/super-media-player-<version>-<arch>.AppImage       one file, runs anywhere
#                                                           (when appimagetool is installed)
#
# They run on the distribution they were built on and newer ones (glibc only
# works forwards): the release packages are built in the Ubuntu 22.04
# container (scripts/in-container.sh).
#
# libmpv and every library it needs (FFmpeg, codecs, ...) are copied into the
# package, so nothing has to be downloaded or installed on the target machine.
# Only base libraries every desktop already has (glibc, OpenGL/EGL, X11 and
# Wayland, audio servers, GPU video-decode drivers, fontconfig) are taken from
# the system; for the .deb they become package dependencies, which apt finds
# already installed.
#
# Also bundled, because nothing links to them directly:
#   - the disc decryption libraries libbluray and libdvdread load at runtime
#     (libaacs, libbdplus, and libdvdcss when the build machine has it; set
#     SMP_NO_DVDCSS=1 to leave libdvdcss out where its use isn't allowed);
#   - yt-dlp's standalone build, checked against the release's SHA-256 list,
#     so YouTube and similar links work out of the box (SMP_NO_YTDLP=1 skips it).
#
# Build machine needs: Rust, patchelf, dpkg-deb, curl, and libmpv: the newer
# engine built by scripts/build-engine.sh (FFmpeg + libmpv from source, used
# whenever .devlibs/engine/ exists), or the system's (`sudo apt install libmpv2`,
# or unpacked by scripts/fetch-dev-libmpv.sh).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

for tool in cargo patchelf dpkg-deb ldd curl sha256sum; do
    command -v "$tool" >/dev/null || { echo "error: '$tool' is required" >&2; exit 1; }
done

version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
# A rebuild of the same code with newer libraries can add a suffix
# (SMP_PACKAGE_SUFFIX=+auto20261124), so apt sees the package as newer.
version="$version${SMP_PACKAGE_SUFFIX:-}"
arch=$(uname -m)
debarch=$(dpkg --print-architecture)
triplet=$(gcc -dumpmachine 2>/dev/null || echo "$arch-linux-gnu")

# ---- locate libmpv ----
devdir="$root/.devlibs/root/usr/lib/$triplet"
enginedir="$root/.devlibs/engine/lib"
if [[ -n "${SMP_LIBMPV:-}" ]]; then
    libmpv="$SMP_LIBMPV"
elif [[ -e "$enginedir/libmpv.so.2" ]]; then
    libmpv="$enginedir/libmpv.so.2"
elif [[ -e "$devdir/libmpv.so.2" ]]; then
    libmpv="$devdir/libmpv.so.2"
elif [[ -e "/usr/lib/$triplet/libmpv.so.2" ]]; then
    libmpv="/usr/lib/$triplet/libmpv.so.2"
else
    echo "error: libmpv not found. Run 'sudo apt install libmpv2' or scripts/fetch-dev-libmpv.sh" >&2
    exit 1
fi
echo "==> libmpv: $libmpv"
export LD_LIBRARY_PATH="$enginedir:$devdir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# ---- build ----
echo "==> building release binary"
touch apps/desktop/build.rs  # refresh the build month shown in About
cargo build --release -p super-media-player

# ---- stage the app folder ----
stage="$root/dist/build"
app="$stage/super-media-player-$version"
rm -rf "$stage"
mkdir -p "$app/lib" "$app/share/applications" "$app/share/icons/hicolor/scalable/apps"
cp target/release/super-media-player "$app/"
cp -L "$libmpv" "$app/lib/libmpv.so.2"
# Where each bundled library came from, so a later check can tell when a
# system update changed one of them (sha256sum -c dist/bundled-sources.sha256).
sources=("$libmpv")

# Base libraries taken from the system (by soname). They are part of every
# desktop, or must match the running drivers/daemons, so bundling them would
# break more than it fixes.
system_re='^(ld-linux.*|linux-vdso.*|libc|libm|libdl|libpthread|librt|libresolv|libutil|libanl|libgcc_s|libstdc\+\+|libGL|libGLX|libGLdispatch|libOpenGL|libEGL|libGLESv2|libgbm|libdrm.*|libX.*|libxcb.*|libxkbcommon.*|libwayland-.*|libasound|libpulse.*|libpipewire-.*|libjack|libdbus-1|libsystemd|libudev|libfontconfig|libfreetype|libexpat|libz|libva.*|libvdpau|libvulkan|libselinux|libmount|libblkid|libcap|libuuid)\.so'

echo "==> collecting libmpv's dependencies"
system_paths=()
bundled=0
missing=$(ldd "$libmpv" | awk '/not found/ {print $1}')
if [[ -n "$missing" ]]; then
    echo "error: libmpv dependencies not found: $missing" >&2
    exit 1
fi
bundle_deps() {
    while read -r name path; do
        [[ -z "$path" || ! -e "$path" ]] && continue
        # Libraries from .devlibs are not installed system-wide: always bundle them.
        # libXpresent matches libX.* but isn't on every desktop (Ubuntu
        # doesn't install it by default), so it travels with the player.
        if [[ "$name" =~ $system_re && "$name" != libXpresent.so* && "$path" != "$root/.devlibs/"* ]]; then
            system_paths+=("$path")
        elif [[ ! -e "$app/lib/$name" ]]; then
            cp -L "$path" "$app/lib/$name"
            sources+=("$path")
            bundled=$((bundled + 1))
        fi
    done < <(ldd "$1" | awk '$2 == "=>" {print $1, $3}')
}
bundle_deps "$libmpv"

# Libraries loaded with dlopen() by name: libbluray → libaacs/libbdplus,
# libdvdread → libdvdcss. Found in .devlibs first, then on the system.
find_lib() {
    if [[ -e "$devdir/$1" ]]; then echo "$devdir/$1"; return; fi
    ldconfig -p | awk -v n="$1" '$1 == n {print $NF; exit}'
}
runtime_libs=(libaacs.so.0 libbdplus.so.0 libchromaprint.so.1)
[[ -z "${SMP_NO_DVDCSS:-}" ]] && runtime_libs+=(libdvdcss.so.2)
for soname in "${runtime_libs[@]}"; do
    path=$(find_lib "$soname")
    if [[ -z "$path" && "$soname" == libdvdcss.so.2 ]]; then
        # Without it, copy-protected DVDs fail on every machine the package
        # goes to, so don't build such a package by accident.
        echo "error: libdvdcss.so.2 not found, so the package could not play copy-protected DVDs." >&2
        echo "       Install it: sudo apt install libdvd-pkg && sudo dpkg-reconfigure libdvd-pkg" >&2
        echo "       (or without root: scripts/fetch-dev-dvdcss.sh)," >&2
        echo "       or build without it on purpose: SMP_NO_DVDCSS=1 scripts/package-linux.sh" >&2
        exit 1
    fi
    if [[ -z "$path" ]]; then
        echo "    note: $soname not found on this machine, not bundled"
        continue
    fi
    cp -L "$path" "$app/lib/$soname"
    sources+=("$path")
    bundled=$((bundled + 1))
    bundle_deps "$path"
    echo "    bundled $soname (runtime-loaded)"
done
for p in $(ldd "$app/super-media-player" | awk '$2 == "=>" {print $3}'); do system_paths+=("$p"); done

# MilkDrop (scripts/fetch-projectm.sh): libprojectM and its presets.
pm="$root/.devlibs/projectm"
if [[ -e "$pm/lib/libprojectM-4.so.4" ]]; then
    cp -L "$pm/lib/libprojectM-4.so.4" "$app/lib/"
    sources+=("$pm/lib/libprojectM-4.so.4")
    bundle_deps "$pm/lib/libprojectM-4.so.4"
    cp -r "$pm/presets" "$app/milkdrop"
    echo "    bundled libprojectM-4.so.4 and $(find "$app/milkdrop" -name '*.milk' | wc -l) MilkDrop presets"
else
    echo "    note: libprojectM not built (scripts/fetch-projectm.sh), MilkDrop not bundled"
fi

# Each bundled library finds its siblings in the same folder.
for f in "$app"/lib/*.so*; do
    chmod u+w "$f"
    patchelf --set-rpath '$ORIGIN' "$f"
done
echo "    bundled $bundled libraries ($(du -sh "$app/lib" | cut -f1))"
mkdir -p dist
for p in "${sources[@]}"; do readlink -f "$p"; done | sort -u | xargs sha256sum >dist/bundled-sources.sha256

# ---- whisper.cpp (scripts/fetch-whisper.sh): subtitles from speech ----
if [[ -x "$root/.devlibs/whisper/whisper-cli" ]]; then
    mkdir -p "$app/whisper"
    cp -P "$root/.devlibs/whisper/"* "$app/whisper/"
    echo "    bundled whisper-cli ($(du -sh "$app/whisper" | cut -f1); speech models are downloaded when first used)"
else
    echo "    note: whisper-cli not built (scripts/fetch-whisper.sh), subtitles from speech need it installed"
fi

# ---- yt-dlp ----
if [[ -z "${SMP_NO_YTDLP:-}" ]]; then
    case "$arch" in
        x86_64) asset=yt-dlp_linux ;;
        aarch64) asset=yt-dlp_linux_aarch64 ;;
        *) asset="" ;;
    esac
    if [[ -n "$asset" ]]; then
        echo "==> fetching yt-dlp ($asset)"
        cache="$root/dist/cache"
        mkdir -p "$cache"
        rel=https://github.com/yt-dlp/yt-dlp/releases/latest/download
        curl -fsSL -o "$cache/SHA2-256SUMS" "$rel/SHA2-256SUMS"
        want=$(awk -v a="$asset" '$2 == a {print $1}' "$cache/SHA2-256SUMS")
        [[ -n "$want" ]] || { echo "error: no checksum for $asset" >&2; exit 1; }
        if ! { [[ -f "$cache/$asset" ]] && echo "$want  $cache/$asset" | sha256sum -c --quiet >/dev/null 2>&1; }; then
            curl -fsSL -o "$cache/$asset" "$rel/$asset"
            echo "$want  $cache/$asset" | sha256sum -c --quiet
        fi
        install -m 755 "$cache/$asset" "$app/yt-dlp"
        echo "    yt-dlp $("$app/yt-dlp" --version 2>/dev/null || echo '?')"
    else
        echo "    note: no standalone yt-dlp for $arch, not bundled"
    fi
fi

cp packaging/linux/super-media-player.desktop "$app/share/applications/"
cp packaging/linux/super-media-player.svg "$app/share/icons/hicolor/scalable/apps/"
cp README.md "$app/"

# Self-check: every bundled library must resolve without LD_LIBRARY_PATH.
for f in "$app"/lib/*.so*; do
    if env -u LD_LIBRARY_PATH ldd "$f" | grep -q 'not found'; then
        echo "$f:" >&2
        env -u LD_LIBRARY_PATH ldd "$f" | grep 'not found' >&2
        echo "error: bundle is incomplete" >&2
        exit 1
    fi
done

# ---- portable tarball (with a per-user installer) ----
cat > "$app/install.sh" <<'EOF'
#!/usr/bin/env bash
# Install Super Media Player for the current user (no root needed): copies the app
# to ~/.local/opt/super-media-player and adds it to the applications menu.
# Run with --uninstall to remove it again.
set -euo pipefail
src="$(cd "$(dirname "$0")" && pwd)"
dest="$HOME/.local/opt/super-media-player"
apps="$HOME/.local/share/applications"
icons="$HOME/.local/share/icons/hicolor/scalable/apps"
bin="$HOME/.local/bin"
if [[ "${1:-}" == "--uninstall" ]]; then
    rm -rf "$dest" "$apps/super-media-player.desktop" "$icons/super-media-player.svg" "$bin/super-media-player"
    update-desktop-database "$apps" 2>/dev/null || true
    echo "Super Media Player removed."
    exit 0
fi
mkdir -p "$dest" "$apps" "$icons" "$bin"
rm -rf "$dest/milkdrop" "$dest/whisper"
cp -r "$src/super-media-player" "$src/lib" "$dest/"
[[ -e "$src/yt-dlp" ]] && cp "$src/yt-dlp" "$dest/"
for d in milkdrop whisper; do [[ -d "$src/$d" ]] && cp -r "$src/$d" "$dest/"; done
cp "$src/share/icons/hicolor/scalable/apps/super-media-player.svg" "$icons/"
sed "s|^Exec=super-media-player|Exec=$dest/super-media-player|" \
    "$src/share/applications/super-media-player.desktop" > "$apps/super-media-player.desktop"
ln -sf "$dest/super-media-player" "$bin/super-media-player"
update-desktop-database "$apps" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Super Media Player installed. Start it from the applications menu,"
echo "or right-click a video > Open With > Super Media Player."
EOF
chmod +x "$app/install.sh"

mkdir -p dist
tarball="dist/super-media-player-$version-linux-$arch.tar.gz"
tar -C "$stage" -czf "$tarball" "super-media-player-$version"
echo "==> $tarball ($(du -h "$tarball" | cut -f1))"

# ---- .deb ----
deb="$stage/deb"
mkdir -p "$deb/DEBIAN" "$deb/opt/super-media-player" "$deb/usr/bin" "$deb/usr/share"
cp -r "$app/super-media-player" "$app/lib" "$deb/opt/super-media-player/"
[[ -e "$app/yt-dlp" ]] && cp "$app/yt-dlp" "$deb/opt/super-media-player/"
for d in milkdrop whisper; do [[ -d "$app/$d" ]] && cp -r "$app/$d" "$deb/opt/super-media-player/"; done
cp -r "$app/share/applications" "$app/share/icons" "$deb/usr/share/"
ln -s /opt/super-media-player/super-media-player "$deb/usr/bin/super-media-player"

# Debian package names for the system libraries we rely on.
depends=$(for p in "${system_paths[@]}"; do
    for cand in "$p" "$(readlink -f "$p")" "/usr$p" "${p#/usr}"; do
        pkg=$(dpkg -S "$cand" 2>/dev/null | head -1 | cut -d: -f1) && [[ -n "$pkg" ]] && { echo "$pkg"; break; }
    done
# (AMD's driver repo renames some Mesa packages, e.g. libgbm1-amdgpu; depend on the standard names.)
done | sed "s/-amdgpu//" | sort -u | paste -sd, | sed "s/,/, /g")
# glibc only works forwards: at least the build machine's.
# (sed reads it all: `| head` would end ldd early, a failure under pipefail.)
glibc=$(ldd --version 2>&1 | sed -nE '1s/.* ([0-9]+\.[0-9]+)$/\1/p')
[[ -n "$glibc" ]] || { echo "error: couldn't tell the glibc version" >&2; exit 1; }
depends=$(sed -E "s/(^|, )libc6(,|$)/\1libc6 (>= $glibc)\2/" <<<"$depends")
installed_kb=$(du -sk "$deb" --exclude=DEBIAN | cut -f1)

cat > "$deb/DEBIAN/control" <<EOF
Package: super-media-player
Version: $version
Section: video
Priority: optional
Architecture: $debarch
Depends: $depends
Conflicts: media-player
Replaces: media-player
Installed-Size: $installed_kb
Maintainer: Dumitru Orlov <noreply@localhost>
Description: Super Media Player (SMP) for video, music, streams and discs
 A desktop media player built on libmpv and FFmpeg, with hardware video
 decoding. libmpv and its codec libraries are bundled in /opt/super-media-player.
EOF
debfile="dist/super-media-player_${version}_${debarch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$deb" "$debfile" >/dev/null
echo "==> $debfile ($(du -h "$debfile" | cut -f1))"
echo "    depends: $depends"

# ---- .rpm ----
if command -v rpmbuild >/dev/null; then
    rpmtop="$stage/rpm"
    mkdir -p "$rpmtop"/{BUILD,RPMS,SOURCES,SPECS,SRPMS,BUILDROOT}
    # The same system libraries as the .deb, by the names rpm packages provide.
    # Private libraries that come with a public one and carry the build
    # machine's version in their name (libpulsecommon-15.99.so comes with
    # libpulse.so.0) are left out: no other distribution has that name.
    requires=$(for p in "${system_paths[@]}"; do basename "$p"; done | sort -u |
        grep -vE '^(linux-vdso|libpulsecommon-)' | sed 's/$/()(64bit)/' | paste -sd' ')
    rpmversion=${version//-/_}
    cat > "$rpmtop/SPECS/smp.spec" <<EOF
Name: super-media-player
Version: $rpmversion
Release: 1
Summary: Super Media Player (SMP) for video, music, streams and discs
License: GPL-3.0-or-later
URL: https://github.com/
# Everything but base desktop libraries is in /opt/super-media-player.
AutoReqProv: no
Requires: $requires libc.so.6(GLIBC_$glibc)(64bit)
%description
A desktop media player built on libmpv and FFmpeg, with hardware video
decoding. libmpv and its codec libraries are bundled in /opt/super-media-player.
%install
cp -a "$deb/opt" "$deb/usr" %{buildroot}/
%files
/opt/super-media-player
/usr/bin/super-media-player
/usr/share/applications/super-media-player.desktop
/usr/share/icons/hicolor/scalable/apps/super-media-player.svg
EOF
    rpmbuild --quiet -bb --define "_topdir $rpmtop" --define "_build_id_links none" \
        --define "__strip /bin/true" --define "__brp_strip /bin/true" \
        --define "__os_install_post %{nil}" "$rpmtop/SPECS/smp.spec" >/dev/null
    rpmfile="dist/super-media-player-${rpmversion}-1.$arch.rpm"
    cp "$rpmtop"/RPMS/*/*.rpm "$rpmfile"
    echo "==> $rpmfile ($(du -h "$rpmfile" | cut -f1))"
else
    echo "    note: rpmbuild not installed, no .rpm"
fi

# ---- AppImage ----
if command -v appimagetool >/dev/null; then
    appdir="$stage/AppDir"
    rm -rf "$appdir"
    cp -r "$app" "$appdir"
    rm -f "$appdir/install.sh" "$appdir/README.md"
    cp packaging/linux/super-media-player.desktop "$appdir/"
    cp packaging/linux/super-media-player.svg "$appdir/"
    mkdir -p "$appdir/usr/share/metainfo"
    cp packaging/linux/*.metainfo.xml "$appdir/usr/share/metainfo/"
    cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/super-media-player" "$@"
EOF
    chmod 755 "$appdir/AppRun"
    appimage="dist/super-media-player-$version-$arch.AppImage"
    runtime=/usr/local/share/appimage-runtime-$arch
    ARCH=$arch appimagetool --no-appstream ${SMP_APPIMAGE_RUNTIME:+--runtime-file "$SMP_APPIMAGE_RUNTIME"} \
        $([[ -z "${SMP_APPIMAGE_RUNTIME:-}" && -e "$runtime" ]] && echo --runtime-file "$runtime") \
        "$appdir" "$appimage" >/dev/null 2>"$stage/appimagetool.log" ||
        { cat "$stage/appimagetool.log" >&2; echo "error: appimagetool failed" >&2; exit 1; }
    echo "==> $appimage ($(du -h "$appimage" | cut -f1))"
else
    echo "    note: appimagetool not installed, no AppImage"
fi

# ---- checksums of everything this run made ----
(cd dist && sha256sum ./*"$version"[-._]* 2>/dev/null | sed 's#  \./#  #' | grep -v SHA256SUMS > "SHA256SUMS-$version") || true

# ---- summary: what other machines get without installing anything ----
has() { [[ -e "$app/$1" ]] && echo "included" || echo "NOT included"; }
echo "==> bundled for other machines:"
echo "    DVD decryption (libdvdcss):   $(has lib/libdvdcss.so.2)"
echo "    Blu-ray AACS (libaacs):       $(has lib/libaacs.so.0)"
echo "    Blu-ray BD+ (libbdplus):      $(has lib/libbdplus.so.0)"
echo "    Online video (yt-dlp):        $(has yt-dlp)"
echo "    Audio CD reading (cdparanoia): $(has lib/libcdio_paranoia.so.2)"
echo "    Intro detection (chromaprint): $(has lib/libchromaprint.so.1)"
echo "    MilkDrop (libprojectM):       $(has lib/libprojectM-4.so.4)"
echo "    Speech to subtitles (whisper): $(has whisper/whisper-cli)"
