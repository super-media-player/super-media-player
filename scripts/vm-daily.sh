#!/usr/bin/env bash
# Daily check ON the build VM (cron, installed by `scripts/vm-build.sh
# schedule`): rebuilds the packages when something they bundle has a newer
# version, then sends a phone notification (scripts/notify.sh). Every run
# sends one, so a missing morning note means the check did not run.
#
# Rebuilds by itself when:
#   - FFmpeg, mpv or libplacebo has a newer point release on the branch in
#     use (FFmpeg 9.1 → 9.1.1, mpv v0.42.0 → v0.42.1, libplacebo v7.x). The new
#     versions go into engine-versions.conf, which overrides the pins in
#     scripts/build-engine.sh; a failed engine build puts the old ones back.
#   - a system library bundled in the package changed on disk: Ubuntu's
#     unattended-upgrades installs security updates on its own, and this
#     notices them through dist/bundled-sources.sha256.
# Only reports (no rebuild) a newer branch, such as FFmpeg 9.2 or mpv 0.43:
# those can need build changes, so the pin is moved by hand.
#
# Rebuilt packages carry a "+autoYYYYMMDD" suffix, so apt sees them as newer.
# It first gets GitHub's newest code (scripts/vm-pull.sh) when the folder is a
# git checkout; otherwise it builds the source last copied over by
# vm-build.sh. Log: daily.log.
#
# Usage: vm-daily.sh [--force]    --force rebuilds even when nothing changed
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
export PATH="$HOME/.cargo/bin:$PATH:/usr/sbin:/sbin"

if pgrep -f "$root/scripts/[v]m-job.sh" >/dev/null; then
    exit 0 # a build started by hand is running; tomorrow's check catches up
fi
# Newest code first, then this script again from it (it may have changed).
if [[ -z "${SMP_NO_PULL:-}" ]]; then
    flock -n .daily.lock true || exit 0
    scripts/vm-pull.sh >>daily.log 2>&1
    case $? in
    0) export SMP_NO_PULL=1; exec "$0" "$@" ;;
    3) ;;
    *) echo "=== $(date -Is) could not get the code from GitHub" >>daily.log
       scripts/notify.sh "SMP daily check failed" "Could not get the code from GitHub; see daily.log on the build machine." high
       exit 1 ;;
    esac
fi
exec 9>.daily.lock
flock -n 9 || exit 0
exec >>daily.log 2>&1
echo "=== $(date -Is) daily check"

# ---- versions in use ----
eval "$(grep -E '^(FFMPEG_VERSION|LIBPLACEBO_TAG|MPV_TAG)=' scripts/build-engine.sh)"
# shellcheck source=/dev/null
[[ -f engine-versions.conf ]] && source engine-versions.conf
ff=$FFMPEG_VERSION pl=$LIBPLACEBO_TAG mpv=$MPV_TAG

problems=()
fetch() { curl -fsSL --max-time 30 "$@"; }
json_field() { python3 -I -c "import json,sys; [print($1) for r in json.load(sys.stdin) $2]"; }

ff_all=$(fetch https://ffmpeg.org/releases/ | grep -oE 'ffmpeg-[0-9]+(\.[0-9]+)+\.tar\.xz"' |
    sed -E 's/^ffmpeg-//; s/\.tar\.xz"$//' | sort -uV) || true
mpv_all=$(fetch "https://api.github.com/repos/mpv-player/mpv/releases?per_page=30" |
    json_field 'r["tag_name"]' 'if not r["draft"] and not r["prerelease"]' |
    grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -uV) || true
pl_all=$(fetch "https://code.videolan.org/api/v4/projects/videolan%2Flibplacebo/repository/tags?per_page=100" |
    json_field 'r["name"]' '' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -uV) || true

# newest <list> <regex>: the highest version in the list matching the regex.
newest() { grep -E "$2" <<<"$1" | tail -n 1; }
esc() { sed 's/\./\\./g' <<<"$1"; }

changes=() waiting=()
new_ff=$ff new_pl=$pl new_mpv=$mpv
if [[ -n "$ff_all" ]]; then
    branch=$(esc "$(cut -d. -f1-2 <<<"$ff")")
    best=$(newest "$ff_all" "^$branch(\.[0-9]+)?$")
    [[ -n "$best" && "$best" != "$ff" ]] && { new_ff=$best; changes+=("FFmpeg $ff → $best"); }
    top=$(newest "$ff_all" '.')
    [[ "$top" != "$new_ff" ]] && waiting+=("FFmpeg $top")
else
    problems+=("ffmpeg.org")
fi
if [[ -n "$mpv_all" ]]; then
    branch=$(esc "$(cut -d. -f1-2 <<<"$mpv")")
    best=$(newest "$mpv_all" "^$branch\.[0-9]+$")
    [[ -n "$best" && "$best" != "$mpv" ]] && { new_mpv=$best; changes+=("mpv $mpv → $best"); }
    top=$(newest "$mpv_all" '.')
    [[ "$top" != "$new_mpv" ]] && waiting+=("mpv $top")
else
    problems+=("GitHub (mpv)")
fi
if [[ -n "$pl_all" ]]; then
    best=$(newest "$pl_all" "^$(cut -d. -f1 <<<"$pl")\.")
    [[ -n "$best" && "$best" != "$pl" ]] && { new_pl=$best; changes+=("libplacebo $pl → $best"); }
    top=$(newest "$pl_all" '.')
    [[ "$top" != "$new_pl" ]] && waiting+=("libplacebo $top")
else
    problems+=("code.videolan.org (libplacebo)")
fi

# ---- bundled system libraries changed by updates ----
# The packages are built in the smp-build container: rebuild its image with
# today's Ubuntu updates, then compare the libraries in it with the ones the
# last packages bundled.
check='sha256sum -c dist/bundled-sources.sha256 2>/dev/null'
if [[ -z "${SMP_NO_CONTAINER:-}" ]] && command -v podman >/dev/null; then
    scripts/in-container.sh "--image=$(date +%F)" >/dev/null || problems+=("the container image (Ubuntu updates)")
    run_check() { scripts/in-container.sh "$check"; }
else
    run_check() { bash -c "$check"; }
fi
libs=""
if [[ -f dist/bundled-sources.sha256 ]]; then
    libs=$(run_check | grep -v ': OK$' |
        sed 's/: [A-Z].*$//' | xargs -r -n1 basename | sort -u | paste -sd' ') || true
    [[ -n "$libs" ]] && changes+=("updated system libraries: $libs")
fi
# Old images (each day's refresh leaves the previous one untagged).
podman image prune -f >/dev/null 2>&1 || true
[[ "${1:-}" == "--force" ]] && changes+=("forced rebuild")

note=""
((${#waiting[@]})) && note+="
Newer branch out (move the pin in scripts/build-engine.sh to try it): ${waiting[*]}"
((${#problems[@]})) && note+="
Could not check: ${problems[*]}"

if ((${#changes[@]} == 0)); then
    echo "up to date"
    scripts/notify.sh "SMP daily check" "Up to date: FFmpeg $ff, libplacebo $pl, mpv $mpv; bundled libraries unchanged.$note" low
    exit 0
fi

# ---- rebuild ----
engine_changed=0
if [[ "$new_ff $new_pl $new_mpv" != "$ff $pl $mpv" ]]; then
    engine_changed=1
    [[ -f engine-versions.conf ]] && cp engine-versions.conf engine-versions.conf.prev
    printf 'FFMPEG_VERSION=%s\nLIBPLACEBO_TAG=%s\nMPV_TAG=%s\n' "$new_ff" "$new_pl" "$new_mpv" >engine-versions.conf
fi
why="Why: $(IFS=';'; echo "${changes[*]}" | sed 's/;/; /g')$note"
echo "$why"
SMP_PACKAGE_SUFFIX="+auto$(date +%Y%m%d)" SMP_BUILD_REASON="$why" SMP_JOB_LOG=daily.log \
    scripts/vm-job.sh
ok=$?

if ((ok != 0 && engine_changed)); then
    # Back to the last good versions unless the new engine did build (then
    # only packaging failed, and the next run retries with it).
    want=$(cat scripts/build-engine.sh engine-versions.conf 2>/dev/null | sha256sum | cut -d' ' -f1)
    if [[ "$(cat .devlibs/engine/.build-engine.sha256 2>/dev/null)" != "$want" ]]; then
        if [[ -f engine-versions.conf.prev ]]; then mv engine-versions.conf.prev engine-versions.conf
        else rm -f engine-versions.conf; fi
        echo "engine build failed: back to FFmpeg $ff, libplacebo $pl, mpv $mpv"
    fi
fi

# vm-job.sh has already removed older builds (scripts/clean-builds.sh).
exit "$ok"
