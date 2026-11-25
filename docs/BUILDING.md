# Building and developing Super Media Player

For using the player, see the [user guide](guide/README.md). The roadmap and
design notes are in [PLAN.md](../PLAN.md), skins in [SKINS.md](../SKINS.md).

## Layout

| Path | What it is |
|---|---|
| `crates/mpv-sys` | FFI bindings to libmpv, loaded at runtime (no build-time link) |
| `crates/player-core` | Safe player API: commands, events, OpenGL render context |
| `apps/desktop` | Desktop app (Slint UI in `ui/`, Rust glue in `src/`) |
| `scripts/` | Development helpers |

## Building and running (Linux)

Requirements: Rust 1.88+, and libmpv at runtime.

```sh
sudo apt install libmpv-dev          # or, without root: scripts/fetch-dev-libmpv.sh
scripts/dev-run.sh [FILE|FOLDER|URL]...   # runs the app (uses .devlibs/ if present)
```

Copy-protected DVDs need libdvdcss: `sudo apt install libdvd-pkg && sudo
dpkg-reconfigure libdvd-pkg`, or without root `scripts/fetch-dev-dvdcss.sh`
(builds it into `.devlibs/`). Commercial Blu-rays need libaacs (`sudo apt
install libaacs0 libbdplus0`) and a key database (Tools → Download Blu-ray key
database). Whether decrypting discs is allowed depends on where you live.

The app looks for libmpv in this order: `$SMP_LIBMPV`, next to the
executable (and `lib/` beside it), then the system library path.

Optional helpers, each built into `.devlibs/` without root and picked up by
`dev-run.sh` and `package-linux.sh`. The player works without them; only that
feature is missing:

| Helper | Feature | Get it |
|---|---|---|
| libchromaprint | Finding series intros and credits by listening | `sudo apt install libchromaprint1` (usually already installed) |
| libprojectM 4 + presets | MilkDrop visualizer | `scripts/fetch-projectm.sh` (needs `libgles-dev libegl-dev cmake`) |
| whisper.cpp `whisper-cli` | Subtitles from speech | `scripts/fetch-whisper.sh` (GPU through Vulkan when `libvulkan-dev glslc` are installed) |

## Packaging (Linux)

```sh
sudo apt install patchelf            # once; plus libmpv as above
scripts/package-linux.sh
```

This writes two self-contained packages to `dist/`. Both bundle libmpv,
FFmpeg and the codec libraries, so the target machine needs nothing
downloaded:

- `super-media-player-<version>-linux-x86_64.tar.gz`: unpack anywhere and run
  `./install.sh` to add it to the applications menu for the current user
  (`./install.sh --uninstall` removes it). No root needed.
- `super-media-player_<version>_amd64.deb`: `sudo apt install ./dist/super-media-player_*.deb`
  installs it to `/opt/super-media-player`. Its dependencies are only base desktop
  libraries (OpenGL, X11/Wayland, audio), which Ubuntu already has.

The packages also bundle:

- [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) (standalone build, checked against
  the release's SHA-256 list) for YouTube and ~1800 other sites. The player
  keeps it current by itself (weekly check, Settings → Update automatically)
  and can update it from Settings; set `SMP_NO_YTDLP=1` to leave it out.
- The disc decryption libraries libbluray and libdvdread load at runtime:
  libaacs, libbdplus and libdvdcss, so discs play on machines that have none
  of them installed. The build machine needs libdvdcss (see above); the script
  stops if it is missing. Set `SMP_NO_DVDCSS=1` to build without it on purpose,
  where its use isn't allowed. The script ends with a list of what was bundled.

### Newer FFmpeg in the packages

By default the packages carry the distribution's FFmpeg (6.1 on Ubuntu
24.04). To ship a newer one, build the engine from source first:

```sh
scripts/build-engine.sh --deps       # prints the one-time `sudo apt install ...` line
scripts/build-engine.sh              # FFmpeg 9.1 + libplacebo + libmpv 0.42 → .devlibs/engine/
scripts/package-linux.sh             # now bundles that engine
```

FFmpeg and libmpv are built together as one matched set: libmpv only works
with the FFmpeg it was compiled against, so FFmpeg is never replaced on its
own. The versions are pinned at the top of `scripts/build-engine.sh`; bump
them there for the next release. `scripts/dev-run.sh` uses the same engine,
and deleting `.devlibs/engine/` goes back to the system's. In the app,
Tools → Update FFmpeg then reports that FFmpeg travels with the player.

### Building on a separate machine (VM)

`scripts/vm-build.sh` runs the whole build (engine, libdvdcss, packages) on
another machine over ssh and sends a phone notification through ntfy when it
is done. The build machine must run the same Ubuntu release as the target
machines, or an older one.

```sh
echo 'BUILD_HOST=user@host' > vm-build.conf   # git-ignored; ssh key login
scripts/vm-build.sh setup     # once: build packages (asks for sudo) + Rust
scripts/vm-build.sh           # copy the source over and start; returns at once
scripts/vm-build.sh status    # end of the build log
scripts/vm-build.sh fetch     # copy the finished packages into dist/
```

For notifications, put `NTFY_URL=https://ntfy.sh/<topic>` in
`~/super-media-player/notify.conf` on the build machine. The engine is only
rebuilt when `scripts/build-engine.sh` changed since its last build.

`scripts/vm-build.sh schedule` turns on a daily check there
(`scripts/vm-daily.sh`, from cron at 05:30). It rebuilds the packages by
itself when FFmpeg, mpv or libplacebo has a new point release on the branch
in use, or when a system library bundled in the package was updated (Ubuntu
installs security updates automatically). A newer branch (FFmpeg 9.2, mpv
0.43) is only reported: move the pin in `scripts/build-engine.sh` to try it.
Automatic builds are named `<version>+auto<date>`, so apt sees them as
newer, and a failed engine build keeps the last good one. Every run sends one
notification, so a missing morning note means the check did not run.
`scripts/vm-build.sh daily [--force]` runs it now; `unschedule` turns it off.

When the build machine runs build-hub (a web page that
schedules and records the builds of several projects), let build-hub
schedule `scripts/vm-daily.sh` instead: run `scripts/vm-build.sh unschedule`
so it doesn't run twice. Copying new code over (`scripts/vm-build.sh`) works
the same either way.

## Tests

```sh
SMP_LIBMPV=... cargo test --workspace
```

`player-core` tests drive a headless mpv with sample files generated by the
`ffmpeg` CLI. They are skipped when libmpv or ffmpeg is missing.

## The guide's pictures

The pictures in `docs/images` come from the real app on a virtual display, in a
sandbox with an invented home folder:

```sh
scripts/docs/demo-media.sh ~/smp-demo       # once: an open movie, made-up music, a DVD
cargo build --release -p super-media-player
scripts/docs/capture.sh ~/smp-demo          # or some: capture.sh ~/smp-demo radio dvd
```

The scenes are in `scripts/docs/scenes.sh`; they need Xvfb, bubblewrap, ffmpeg,
xwd and python3 (the DVD also dvdauthor, spumux, mkisofs and Pillow). The
`panel-*.png` files come from `cargo run -p super-media-player --example
render-views -- <folder>`.

## Dev aids

| | |
|---|---|
| `SMP_MPV_LOG=file` | mpv's own detailed log |
| `SMP_MPV_OPTIONS="scale=bilinear;dither=no"` | mpv options for this run |
| `SMP_TEST_DVD=/dev/sr0 cargo test walk_a_disc -- --ignored --nocapture` | walk a DVD's menus without a window |
| `scripts/fake-cast-devices.py` | a fake Chromecast and DLNA TV to cast to |
