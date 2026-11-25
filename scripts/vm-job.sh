#!/usr/bin/env bash
# Runs ON the build VM (started by scripts/vm-build.sh): builds the engine
# when its pinned versions changed, then the packages, then sends a phone
# notification (scripts/notify.sh) saying how it went. Output goes to
# build.log in the project folder (when started by vm-build.sh) or to
# daily.log (by vm-daily.sh, which sets SMP_BUILD_REASON and the package
# suffix).
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

# The engine takes the longest: rebuild it only when build-engine.sh (and so
# the pinned versions) changed since the last build.
stamp=.devlibs/engine/.build-engine.sha256
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

# Old compiled code goes when the version changed; only the newest build is
# kept on this machine (a problem here never fails a build).
scripts/clean-builds.sh --before-build "$base_version" || echo "WARNING: could not clean target/"

step="package"
echo "==> $step"
scripts/package-linux.sh || fail

echo "==> clean up older builds"
scripts/clean-builds.sh || echo "WARNING: could not clean older builds"

mins=$(( ($(date +%s) - started) / 60 ))
files=$(cd dist && ls -1 ./*"$version"* 2>/dev/null | sed 's#^\./##' | tr '\n' ' ')
echo "==> done in $mins min: $files"
scripts/notify.sh "SMP $version packages ready" "${reason}Built in $mins min: $files
Fetch them with scripts/vm-build.sh fetch"
