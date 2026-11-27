#!/usr/bin/env bash
# The pictures of the user guide (docs/images), taken from the real app on a
# virtual display, in a sandbox with an invented home folder.
#
#   scripts/docs/demo-media.sh <media>          once
#   cargo build --release -p super-media-player
#   scripts/docs/capture.sh <media> [scene]...  all scenes, or some
#
# Needs Xvfb, bwrap, ffmpeg, xwd, python3. $SMP_XVFB: the Xvfb to use.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
media="$(cd "$1" && pwd)"
shift
images="$root/docs/images"
state="${SMP_CAPTURE_STATE:-$(mktemp -d)}"
mkdir -p "$images" "$state/config/super-media-player" "$state/data" "$state/cache"
W=1100 H=680
export DISPLAY="${SMP_CAPTURE_DISPLAY:-:78}"

xvfb() {
    stop_xvfb
    local kf6
    kf6="$(readlink -f /snap/kf6-core24/current 2> /dev/null || true)"
    if [[ -n "${SMP_XVFB:-}" ]] || command -v Xvfb > /dev/null; then
        "${SMP_XVFB:-Xvfb}" "$DISPLAY" -screen 0 "$1x$2x24" +extension GLX > "$state/xvfb.log" 2>&1 &
    else
        # A snap that happens to bring one.
        LD_LIBRARY_PATH="$kf6/usr/lib/x86_64-linux-gnu:$kf6/lib/x86_64-linux-gnu" \
            "$kf6/usr/bin/Xvfb" "$DISPLAY" -screen 0 "$1x$2x24" +extension GLX > "$state/xvfb.log" 2>&1 &
    fi
    xvfb_pid=$!
    size="$1x$2"
    sleep 2
}
xvfb_pid="" app_pid="" size=""
stop_app() { [[ -z "$app_pid" ]] || { kill -9 -- "-$app_pid" 2> /dev/null || true; wait "$app_pid" 2> /dev/null || true; app_pid=""; }; }
stop_xvfb() { stop_app; [[ -z "$xvfb_pid" ]] || { kill "$xvfb_pid" 2> /dev/null || true; wait "$xvfb_pid" 2> /dev/null || true; xvfb_pid=""; }; }
trap stop_xvfb EXIT

# app [setting=value]... -- [file]...   (paths as the sandbox sees them)
app() {
    stop_app
    local conf="$state/config/super-media-player/settings.conf"
    {
        echo "window_system = x11"
        echo "hardware_decoding = false"
        echo "volume = 70"
        echo "lyrics_online = false"
        echo "artist_image_online = false"
        echo "single_instance = false"
        echo "check_app_updates = false"
        while [[ $# -gt 0 && "$1" != "--" ]]; do
            echo "${1%%=*} = ${1#*=}"
            shift
        done
    } > "$conf"
    [[ "${1:-}" == "--" ]] && shift
    local dev=/home/you/app/.devlibs
    local libs="$dev/root/usr/lib/x86_64-linux-gnu"
    [[ -e "$root/.devlibs/engine/lib/libmpv.so.2" ]] && libs="$dev/engine/lib:$libs"
    # OWN_NET: a network of its own, so no real address is in the picture.
    local net=()
    [[ -z "${OWN_NET:-}" ]] || net=(unshare -rn sh -c 'ip link add d0 type dummy && ip addr add 192.168.1.20/24 dev d0 && ip link set d0 up && ip route add default via 192.168.1.1; exec "$@"' sh)
    setsid "${net[@]}" bwrap --dev-bind / / --tmpfs /home --bind "$root" /home/you/app \
        --bind "$media/Music" /home/you/Music --bind "$media/Videos" /home/you/Videos \
        --bind "$state" /home/you/.state --dir /home/you/.run --chdir /home/you --clearenv \
        --setenv HOME /home/you --setenv USER you --setenv PATH /usr/bin:/bin --setenv LANG en_US.UTF-8 \
        --setenv DISPLAY "$DISPLAY" --setenv XDG_CONFIG_HOME /home/you/.state/config \
        --setenv XDG_DATA_HOME /home/you/.state/data --setenv XDG_CACHE_HOME /home/you/.state/cache \
        --setenv XDG_RUNTIME_DIR /home/you/.run --setenv SDL_AUDIODRIVER dummy \
        --setenv SMP_MPV_OPTIONS "${MPV_OPTIONS:-scale=bilinear;cscale=bilinear;dscale=bilinear;correct-downscaling=no;sigmoid-upscaling=no;linear-downscaling=no;dither-depth=no}" \
        --setenv LIBGL_ALWAYS_SOFTWARE 1 --setenv LD_LIBRARY_PATH "$libs" \
        --setenv SMP_LIBMPV "${libs%%:*}/libmpv.so.2" --setenv SMP_EXTRA_LIBS "$dev/projectm/lib" \
        --setenv SMP_MILKDROP_PRESETS "$dev/projectm/presets" --setenv SMP_WHISPER "$dev/whisper/whisper-cli" \
        /home/you/app/target/release/super-media-player "$@" > "$state/app.log" 2>&1 < /dev/null &
    app_pid=$!
}

# The virtual display's OpenGL sometimes starts with a black picture (the
# real one doesn't): try again. good <secs> <app arguments>
good() {
    local secs="$1" y
    shift
    for _ in 1 2 3 4 5 6 7 8; do
        drive move:900,300
        app "$@"
        sleep "$secs"
        shot_to "$state/check.png"
        y="$(ffmpeg -v error -i "$state/check.png" -vf "crop=iw/2:ih/4:iw/4:ih/4,signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=-" -f null - | sed -n 's/.*YAVG=\([0-9]*\).*/\1/p' | tail -1)"
        [[ "${y:-0}" -gt 33 ]] && return 0
    done
    echo "the picture stayed black: $*" >&2
    return 1
}

drive() { python3 "$here/drive.py" "$@"; }
shot_to() { xwd -root -silent | ffmpeg -v error -y -f xwd_pipe -i - -frames:v 1 "$1"; }
# shot <name> [crop]
shot() {
    shot_to "$state/shot.png"
    ffmpeg -v error -y -i "$state/shot.png" -vf "${2:+crop=$2,}scale=in_range=full" "$images/$1.png"
    echo "  $1.png"
}

# The same as a JPEG, for pictures with video in them.
shotj() {
    shot_to "$state/shot.png"
    ffmpeg -v error -y -i "$state/shot.png" -vf "${2:+crop=$2,}format=yuvj444p" -q:v 3 "$images/$1.jpg"
    echo "  $1.jpg"
}

# gif <name> <actions>...: record while they run. $GIF_WIDTH, $GIF_FPS.
gif() {
    local name="$1" rec
    shift
    ffmpeg -v error -y -f x11grab -draw_mouse 1 -framerate "${GIF_FPS:-12}" -video_size "$size" -i "$DISPLAY" \
        -c:v libx264 -qp 0 -preset ultrafast "$state/rec.mkv" < /dev/null &
    rec=$!
    sleep 0.6
    drive "$@"
    kill -INT "$rec"
    wait "$rec" || true
    ffmpeg -v error -y -i "$state/rec.mkv" -vf "fps=${GIF_FPS:-12},scale=${GIF_WIDTH:-800}:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=${GIF_COLORS:-128}:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" "$images/$name.gif"
    echo "  $name.gif $(du -h "$images/$name.gif" | cut -f1)"
}

V="/home/you/Videos"
M="/home/you/Music"
FILM="$V/Big Buck Bunny (2008).mp4"
ALBUM="$M/Nova Harbor/Lighthouse Season"
source "$here/scenes.sh"

if [[ $# -eq 0 ]]; then
    mapfile -t list < <(declare -F | sed -n 's/^declare -f scene_//p')
else
    list=("$@")
fi
xvfb $W $H
for s in "${list[@]}"; do
    echo "$s"
    "scene_$s" || echo "  FAILED: $s" >&2
    [[ "$size" == "${W}x${H}" ]] || xvfb $W $H
done
