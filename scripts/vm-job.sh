#!/usr/bin/env bash
# Runs ON the build VM (started by scripts/vm-build.sh or build-hub): gets
# the newest code from GitHub (scripts/vm-pull.sh, when the folder is a git
# checkout), builds the engine when its pinned versions changed, then the
# Linux and Windows packages, tests the Linux ones in clean containers of the
# distributions, publishes them as the GitHub release of a version tag
# (scripts/publish-github.sh, when the commit has the tag and github.conf
# exists), then sends a phone notification (scripts/notify.sh) saying how it
# went. SMP_NO_PULL=1 builds the folder as it is (vm-build.sh start --copy).
# Output goes to build.log in the project folder (when started by
# vm-build.sh) or to daily.log (by vm-daily.sh, which sets SMP_BUILD_REASON
# and the package suffix).
#
# The packages are built in the smp-build container (Ubuntu 22.04, see
# packaging/linux/build.Containerfile), so they run on 22.04 and newer and
# the build machine needs no build packages installed; the tests start their
# own containers from the build machine. SMP_NO_CONTAINER=1 builds the Linux
# packages on the machine itself instead (no Windows packages, no tests).
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
# cron's PATH has no sbin (ldconfig) and no ~/.cargo/bin.
export PATH="$HOME/.cargo/bin:$PATH:/usr/sbin:/sbin"
version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
base_version="$version"
version="$version${SMP_PACKAGE_SUFFIX:-}"
log="${SMP_JOB_LOG:-build.log}"
reason="${SMP_BUILD_REASON:+$SMP_BUILD_REASON
}"
started=$(date +%s)
step=""

fail() {
    local tail
    tail=$(grep -v '^\s*$' "$log" | tail -n 8)
    echo "==> FAILED at: $step"
    scripts/notify.sh "SMP $version build failed" "${reason}Step: $step
$tail" high
    exit 1
}

# ---- the code: GitHub's newest, then this script again from it ----
if [[ -z "${SMP_NO_PULL:-}" && "${1:-}" != --linux-only ]]; then
    step="get the code from GitHub"
    scripts/vm-pull.sh
    case $? in
    0) export SMP_NO_PULL=1; exec "$0" "$@" ;;
    3) ;; # not a git checkout: the copy vm-build.sh made
    *) fail ;;
    esac
fi
echo "==> building $(git rev-parse --short HEAD 2>/dev/null || echo 'a copied folder')$( [[ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]] && echo ' with changes copied over')"

# ---- the Linux build (in the container, or here with SMP_NO_CONTAINER) ----
linux_build() {
    # The engine takes the longest: rebuild it only when build-engine.sh (and
    # so the pinned versions) changed since the last build.
    local stamp=.devlibs/engine/.build-engine.sha256 want
    want=$(cat scripts/build-engine.sh engine-versions.conf 2>/dev/null | sha256sum | cut -d' ' -f1)
    if [[ ! -e .devlibs/engine/lib/libmpv.so.2 || "$(cat "$stamp" 2>/dev/null)" != "$want" ]]; then
        step="build engine (FFmpeg + libmpv)"
        echo "==> $step"
        scripts/build-engine.sh || fail
        echo "$want" >"$stamp"
    else
        echo "==> engine is current, not rebuilt"
    fi

    if ! ldconfig -p | grep -q 'libdvdcss.so.2' && [[ ! -e .devlibs/root/usr/lib/x86_64-linux-gnu/libdvdcss.so.2 ]]; then
        step="build libdvdcss"
        echo "==> $step"
        scripts/fetch-dev-dvdcss.sh || fail
    fi

    # Old compiled code goes when the version changed; only the newest build
    # is kept on this machine (a problem here never fails a build).
    scripts/clean-builds.sh --before-build "$base_version" || echo "WARNING: could not clean target/"

    # MilkDrop and speech-to-subtitles travel with the packages; built once.
    if [[ ! -e .devlibs/projectm/lib/libprojectM-4.so.4 ]]; then
        step="build libprojectM (MilkDrop)"
        echo "==> $step"
        scripts/fetch-projectm.sh || fail
    fi
    if [[ ! -x .devlibs/whisper/whisper-cli ]]; then
        step="build whisper.cpp (subtitles from speech)"
        echo "==> $step"
        scripts/fetch-whisper.sh || fail
    fi

    step="Linux packages"
    echo "==> $step"
    scripts/package-linux.sh || fail
}

# Inside the container, called by the part below: only the Linux build.
if [[ "${1:-}" == --linux-only ]]; then
    linux_build
    exit 0
fi

if [[ -n "${SMP_NO_CONTAINER:-}" ]] || ! command -v podman >/dev/null; then
    linux_build
else
    if ! podman image exists localhost/smp-build; then
        step="build the smp-build container image"
        echo "==> $step"
        scripts/in-container.sh --image || fail
    fi
    # The container sends its own message when it fails.
    scripts/in-container.sh scripts/vm-job.sh --linux-only || exit 1

    step="test the Linux packages"
    echo "==> $step"
    scripts/test-packages.sh || fail

    step="Windows packages"
    echo "==> $step"
    scripts/in-container.sh scripts/package-windows.sh || fail
fi

# One checksum list for every package of this version (Windows ones too).
(cd dist && sha256sum ./*"$version"[-._]* 2>/dev/null | sed 's#  \./#  #' > "SHA256SUMS-$version") || true

# A version tag on this commit (git tag v0.59.0 && git push origin v0.59.0)
# makes its packages a GitHub release; daily rebuilds (+auto) are not published.
published=""
if [[ -z "${SMP_PACKAGE_SUFFIX:-}" && -f github.conf ]]; then
    step="publish on GitHub"
    echo "==> $step"
    scripts/publish-github.sh
    case $? in
    0) published="
Published as the GitHub release v$version" ;;
    4) published="
Not published: this commit has no v$version tag" ;;
    *) fail ;;
    esac
fi

echo "==> clean up older builds"
scripts/clean-builds.sh || echo "WARNING: could not clean older builds"

mins=$(( ($(date +%s) - started) / 60 ))
files=$(cd dist && ls -1 ./*"$version"[-._]* 2>/dev/null | sed 's#^\./##' | tr '\n' ' ')
echo "==> done in $mins min: $files"
scripts/notify.sh "SMP $version packages ready" "${reason}Built in $mins min: $files
Fetch them with scripts/vm-build.sh fetch$published"
