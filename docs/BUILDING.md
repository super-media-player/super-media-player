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

## Packaging

The release packages are built in a container, so the build machine needs only
podman (rootless) and none of the build packages:

```sh
scripts/in-container.sh --image                      # once: the smp-build image (Ubuntu 22.04)
scripts/in-container.sh scripts/build-engine.sh      # FFmpeg + libmpv, see below
scripts/in-container.sh scripts/package-linux.sh     # the Linux packages
scripts/in-container.sh scripts/package-windows.sh   # the Windows packages
```

Built on Ubuntu 22.04, the Linux packages run on 22.04 / Mint 21 / Debian 12 /
Fedora 36 and every newer distribution (glibc only works forwards).
`scripts/vm-job.sh` does all of this on the build machine by itself.

`scripts/package-linux.sh` writes to `dist/`. All of them bundle libmpv, FFmpeg
and the codec libraries, so the target machine needs nothing downloaded:

- `super-media-player-<version>-x86_64.AppImage`: one file; `chmod +x` and run it.
- `super-media-player_<version>_amd64.deb`: `sudo apt install ./dist/super-media-player_*.deb`
  installs it to `/opt/super-media-player`. Its dependencies are only base desktop
  libraries (OpenGL, X11/Wayland, audio), which every desktop already has.
- `super-media-player-<version>-1.x86_64.rpm`: the same for Fedora and openSUSE
  (`sudo dnf install ./…rpm`).
- `super-media-player-<version>-linux-x86_64.tar.gz`: unpack anywhere and run
  `./install.sh` to add it to the applications menu for the current user
  (`./install.sh --uninstall` removes it). No root needed.
- `SHA256SUMS-<version>`: checksums of all of them.

It also runs outside the container (`sudo apt install patchelf`, plus libmpv as
above): then the packages need that machine's distribution or a newer one, and
the .rpm and AppImage are made only when `rpmbuild` and `appimagetool` are installed.

`scripts/package-windows.sh` cross-compiles the 64-bit Windows program with
cargo-xwin and writes `super-media-player-<version>-windows-x64-setup.exe` (an
NSIS installer: Start menu, "Open with" for media files, an uninstaller) and a
portable `…-windows-x64.zip`. libmpv comes from shinchiro's mpv-winbuild (pinned
in the script, SHA-256 checked). On Windows, DVDs and Blu-rays play from drive
letters; audio CDs and disc decryption are Linux-only so far.

macOS packages need a Mac: `scripts/package-macos.sh` (Homebrew's mpv,
dylibbundler, librsvg) makes a `.dmg`, and `.github/workflows/release.yml` runs
it on GitHub's Apple silicon and Intel runners for every `v*` tag and adds
the `.dmg` files to that tag's GitHub release.

The Linux packages also bundle:

- [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) (standalone build, checked against
  the release's SHA-256 list) for YouTube and ~1800 other sites. The player
  keeps it current by itself (weekly check, Settings → Update automatically)
  and can update it from Settings; set `SMP_NO_YTDLP=1` to leave it out.
- The disc decryption libraries libbluray and libdvdread load at runtime:
  libaacs, libbdplus and libdvdcss, so discs play on machines that have none
  of them installed. The build machine needs libdvdcss (see above); the script
  stops if it is missing. Set `SMP_NO_DVDCSS=1` to build without it on purpose,
  where its use isn't allowed. The script ends with a list of what was bundled.
- libcdio-paranoia (reading audio CDs exactly), libprojectM with its presets
  (MilkDrop) and whisper.cpp (subtitles from speech), built once by
  `scripts/vm-job.sh`.

### Newer FFmpeg in the packages

By default the packages carry the distribution's FFmpeg. To ship a newer one,
build the engine from source first (the release packages always do):

```sh
scripts/build-engine.sh --deps       # prints the one-time `sudo apt install ...` line
scripts/build-engine.sh              # FFmpeg 9.1 + libplacebo + libmpv 0.42 → .devlibs/engine/
scripts/package-linux.sh             # now bundles that engine
```

Where the distribution's are too old (Ubuntu 22.04), dav1d and libdisplay-info
are built too and bundled, and a newer libdrm is built only to compile against
(the packages use the system's, which must match its GPU drivers). The engine
includes x264, LAME and Opus (saved clips, casting) and audio CD support; mpv's
PipeWire output only where libpipewire is 0.3.57 or newer (on 22.04 it plays
through the PulseAudio output, which PipeWire desktops serve too).

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
scripts/vm-build.sh connect super-media-player/super-media-player  # once
scripts/vm-build.sh           # start a build of GitHub's newest main
scripts/vm-build.sh start --copy  # or of this folder, unpushed changes too
scripts/vm-build.sh status    # end of the build log
scripts/vm-build.sh fetch     # copy the finished packages into dist/
```

After `connect`, the build machine's folder is a git checkout: every build
and every daily check first moves it to GitHub's newest commit
(`scripts/vm-pull.sh`), so what is built is always a known commit. Push, then
build. Changes copied over with `--copy` are dropped again by the next build
from GitHub.

For notifications, put `NTFY_URL=https://ntfy.sh/<topic>` in
`~/super-media-player/notify.conf` on the build machine. The engine is only
rebuilt when `scripts/build-engine.sh` changed since its last build.

### Releases

A build of a commit with a version tag publishes its packages as that tag's
GitHub release (`scripts/publish-github.sh`), next to the macOS `.dmg` files
from GitHub Actions:

```sh
git tag v0.59.0 && git push origin main v0.59.0   # macOS build starts
scripts/vm-build.sh                               # Linux + Windows, then publish
```

This needs `github.conf` in `~/super-media-player` on the build machine
(git-ignored): `GITHUB_TOKEN=` a fine-grained token for the repository only,
with "Contents: Read and write", and `GITHUB_REPO=super-media-player/super-media-player`.
`scripts/vm-build.sh publish` publishes the last build again by hand. Daily
rebuilds (`+auto`) are not published.

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
so it doesn't run twice. build-hub's runs get GitHub's newest code the same
way.

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
