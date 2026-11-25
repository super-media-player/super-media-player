# The scenes of capture.sh: each starts the app its own way and takes its
# pictures. Coordinates are for the 1100×680 window.

# Films start from the beginning again (an empty list of positions).
forget() { mkdir -p "$state/data/super-media-player" && : > "$state/data/super-media-player/resume.tsv"; }
# palette <letters>: run a command palette entry.
palette() { echo key:Ctrl+k sleep:0.6 "type:$1" sleep:0.5 key:Return; }
# The media library's folders, added once through the app itself.
need_library() {
    [[ -e "$state/library-ready" ]] && return
    app --
    sleep 5
    drive focus key:Ctrl+Shift+m sleep:2 click:545,380 sleep:12
    : > "$state/library-ready"
}

scene_start() {
    forget
    app --
    sleep 5
    drive move:900,520
    sleep 4
    shot start
}

scene_video() {
    forget
    good 6 -- "$FILM"
    drive focus key:Shift+Right key:Shift+Right key:Right sleep:5 move:520,300 sleep:0.3 move:540,330 sleep:0.8
    shot video
    drive key:space sleep:0.5
    gif seek-preview sleep:0.4 glide:300,582,0.8 sleep:0.9 glide:520,582,0.9 sleep:0.9 glide:760,582,0.9 sleep:0.9 click sleep:1.5
}

scene_palette() {
    forget
    app -- "$ALBUM/02 - Harbor Lights.flac"
    sleep 6
    drive focus move:900,500
    GIF_COLORS=96 gif palette sleep:0.3 key:Ctrl+k sleep:0.9 type:visual sleep:1.2 key:BackSpace key:BackSpace key:BackSpace key:BackSpace key:BackSpace key:BackSpace type:equal sleep:1 key:Return sleep:2
}

scene_menus() {
    app --
    sleep 5
    drive focus click:22,19 sleep:0.4 glide:60,154 sleep:1 click sleep:0.8
    shot menu-library 660:370:0:0
}

scene_medialib() {
    forget
    need_library
    app --
    sleep 5
    drive focus key:Ctrl+Shift+m sleep:4 move:700,600
    shot media-library
    GIF_COLORS=160 gif media-library sleep:0.6 click:56,140 sleep:1.3 click:420,250 sleep:1.6 click:48,176 sleep:1.3 click:60,250 sleep:1.3 click:700,64,0.7 sleep:0.4 type:harbor sleep:1.8
}

scene_radio() {
    app --
    sleep 5
    GIF_FPS=10 gif radio focus sleep:0.3 $(palette radio) sleep:3 click:390,182 sleep:0.2 type:somafm sleep:0.4 key:Return sleep:3.5 glide:650,250 sleep:1.2
    shot radio
}

scene_podcasts() {
    app --
    sleep 5
    drive focus $(palette podcasts) sleep:2 click:400,182 type:nasa key:Return sleep:4
    shot podcasts
}

scene_saved() {
    local links="$state/config/super-media-player/links.tsv"
    printf 'radio\tHarbor FM\thttps://radio.example/harbor\nradio\tNight Jazz\thttps://radio.example/jazz\nradio\tNews 24\thttps://radio.example/news\naudio\tOcean waves (2 hours)\thttps://audio.example/waves\nvideo\tAquarium live\thttps://video.example/aquarium\n' > "$links"
    app --
    sleep 5
    drive focus key:Ctrl+Shift+l sleep:1.5
    shot saved-links
    : > "$links"
}

scene_music() {
    forget
    app visualizer=disc -- "$ALBUM"
    sleep 8
    drive focus move:900,420 key:n sleep:8
    shot music
    GIF_COLORS=160 gif music sleep:4
}

scene_visualizers() {
    for v in bars particles milkdrop; do
        forget
        app visualizer=$v milkdrop_seconds=6 -- "$ALBUM/01 - First Signal.flac"
        sleep 10
        drive move:1000,480
        shotj visual-$v
        [[ $v == milkdrop ]] && GIF_WIDTH=520 GIF_FPS=8 GIF_COLORS=96 gif milkdrop sleep:3
    done
    return 0
}

scene_karaoke() {
    forget
    app -- "$ALBUM/02 - Harbor Lights.flac"
    sleep 7
    drive focus $(palette karaoke) sleep:3 move:1000,500
    GIF_COLORS=64 gif karaoke sleep:6
}

scene_equalizer() {
    forget
    app -- "$ALBUM/03 - Long Way Round.flac"
    sleep 6
    drive focus key:e sleep:1.5
    shot equalizer
}

scene_dvd() {
    forget
    good 12 -- "$V/Bunny DVD.iso"
    drive focus move:700,300 sleep:1
    shotj dvd-menu
    GIF_WIDTH=680 GIF_FPS=6 GIF_COLORS=96 gif dvd-menu focus sleep:0.6 key:Down sleep:0.8 key:Down sleep:0.8 key:Up sleep:0.8 key:Return sleep:2.5
}

scene_skins() {
    forget
    app -- "$ALBUM/01 - First Signal.flac"
    sleep 7
    drive focus move:900,500
    GIF_FPS=6 GIF_COLORS=160 gif skins sleep:0.8 key:Ctrl+m sleep:1.4 key:Ctrl+m sleep:1.4 key:Ctrl+m sleep:1.4 key:Ctrl+m sleep:1.4 key:Ctrl+m sleep:1.4 key:Ctrl+m sleep:1.4
}

scene_navigation() {
    forget
    good 6 -- "$FILM"
    drive focus key:Shift+Right key:Shift+Right sleep:2 key:space key:Ctrl+j sleep:1.5
    shotj navigation
    GIF_FPS=10 gif navigation sleep:0.4 key:Right sleep:0.35 key:Right sleep:0.35 key:Right sleep:0.35 key:Ctrl+Right sleep:0.35 key:Ctrl+Right sleep:0.35 key:Left sleep:0.35 key:Left sleep:0.8
}

scene_enhance() {
    forget
    MPV_OPTIONS="scale=bilinear;dither-depth=no" good 6 video_enhance=sharpen -- "$FILM"
    drive focus key:Shift+Right key:Shift+Right key:Shift+Right sleep:3 key:space $(palette compare) sleep:2
    shotj compare
}

scene_subtitles() {
    forget
    good 6 -- "$FILM"
    drive focus key:Shift+Right key:Shift+Right key:Right sleep:3 move:500,300 sleep:5
    shotj subtitles
    drive $(palette whisper) sleep:2
    shot transcript
}

scene_playlist() {
    forget
    app -- "$M"
    sleep 8
    drive focus key:l sleep:1.5 move:500,500
    shot playlist
}

scene_settings() {
    # Invented mpv scripts, for their page.
    local dir="$state/config/super-media-player/scripts"
    mkdir -p "$dir/quick-menu"
    echo 'mp.add_key_binding("h", "name", function() mp.osd_message(mp.get_property("media-title", "")) end)' > "$dir/show-name.lua"
    echo 'mp.add_key_binding("Ctrl+r", "random", function() end) mp.add_key_binding("y", "mark", function() end)' > "$dir/random-scene.lua"
    echo 'mp.add_key_binding("Ctrl+w", "open", function() end)' > "$dir/quick-menu/main.lua"
    app --
    sleep 5
    drive focus key:Ctrl+p sleep:1.5
    shot settings
    drive click:250,355 sleep:1
    shot settings-shortcuts
    drive click:250,426 sleep:1
    shot settings-discs
    drive click:250,499 sleep:1
    shot settings-scripts
    rm -r "$dir"
}

scene_cast() {
    "$root/scripts/fake-cast-devices.py" --prefix "" > "$state/fake.log" 2>&1 < /dev/null &
    local fake=$!
    sleep 1.5
    forget
    good 6 -- "$FILM"
    drive focus key:space sleep:0.5
    GIF_FPS=8 gif cast move:600,300 sleep:0.5 click:1021,68 sleep:4 glide:500,315 sleep:0.6 click sleep:5
    shot cast-active
    kill "$fake" 2> /dev/null || true
}

scene_remote() {
    forget
    OWN_NET=1 good 6 -- "$FILM"
    drive focus key:space $(palette phone) sleep:1.5 click:550,594 sleep:2 move:700,650 sleep:0.5
    shot remote
}
