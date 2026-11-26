#!/usr/bin/env bash
# Install this version's Linux packages into clean containers of the
# distributions they are for, and run the player's --self-test in each
# (libmpv loads, encoders, disc and CD support, bundled libraries). Runs on
# the build machine (rootless podman), after scripts/package-linux.sh.
#
#   scripts/test-packages.sh            all of them
#   scripts/test-packages.sh deb rpm    some: deb, deb22, debian, rpm, appimage, tar
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
version="$version${SMP_PACKAGE_SUFFIX:-}"
deb="super-media-player_${version}_amd64.deb"
rpm="super-media-player-${version//-/_}-1.x86_64.rpm"
appimage="super-media-player-$version-x86_64.AppImage"
tarball="super-media-player-$version-linux-x86_64.tar.gz"
# What a bare container lacks that every desktop has (for the AppImage and
# the tarball, which don't install dependencies).
desktop_libs="libgl1 libegl1 libgles2 libfontconfig1 libx11-6 libx11-xcb1 libxcb1 libxkbcommon0 libasound2 libpulse0 libva2 libva-drm2 libva-x11-2 libvdpau1 libgbm1 libdrm2 libxext6 libxrandr2 libxss1 libdbus-1-3"

run() { # run <name> <image> <script>
    local name=$1 image=$2 script=$3 log
    log=$(mktemp)
    if podman run --rm --security-opt label=disable -v "$root/dist:/dist:ro" "$image" bash -c "$script" >"$log" 2>&1; then
        echo "ok    $name ($image)"
    else
        echo "FAIL  $name ($image):"
        tail -n 25 "$log" | sed 's/^/        /'
        failed=1
    fi
    rm -f "$log"
}

apt_install='export DEBIAN_FRONTEND=noninteractive; apt-get update -qq >/dev/null && apt-get install -y -qq'
test_deb="$apt_install /dist/$deb >/dev/null && super-media-player --self-test"
test_rpm="dnf install -y -q /dist/$rpm >/dev/null && super-media-player --self-test"
# Ubuntu 24.04 renamed libasound2.
libs_for_release='libs="'"$desktop_libs"'"; grep -q noble /etc/os-release && libs=${libs/libasound2 /libasound2t64 }'
test_appimage="$libs_for_release; $apt_install \$libs >/dev/null && cp /dist/$appimage /tmp/a && APPIMAGE_EXTRACT_AND_RUN=1 /tmp/a --self-test"
test_tar="$libs_for_release; $apt_install \$libs >/dev/null && tar xzf /dist/$tarball -C /tmp && /tmp/super-media-player-$version/super-media-player --self-test"

want=("$@")
[[ ${#want[@]} -eq 0 ]] && want=(deb22 deb debian rpm appimage tar)
failed=0
for w in "${want[@]}"; do
    case "$w" in
        deb22) run ".deb on Ubuntu 22.04" docker.io/library/ubuntu:22.04 "$test_deb" ;;
        deb) run ".deb on Ubuntu 24.04" docker.io/library/ubuntu:24.04 "$test_deb" ;;
        debian) run ".deb on Debian 12" docker.io/library/debian:12 "$test_deb" ;;
        rpm) run ".rpm on Fedora" registry.fedoraproject.org/fedora:latest "$test_rpm" ;;
        appimage) run "AppImage on Ubuntu 24.04" docker.io/library/ubuntu:24.04 "$test_appimage" ;;
        tar) run "tarball on Ubuntu 22.04" docker.io/library/ubuntu:22.04 "$test_tar" ;;
        *) echo "unknown: $w" >&2; exit 2 ;;
    esac
done
exit $failed
