# Super Media Player (SMP) — Implementation Plan

A cross-platform (Linux, Windows, macOS, Android) media player written in Rust.
Goals: play everything (local files, CD/DVD/Blu-ray, online streams), use the GPU
for video, ship as one self-contained package, and look modern with animations:
the skins and visualizers of Winamp, the media library of Windows Media Player,
and the "plays anything" engine of VLC.

---

## 1. Key technical decisions

### 1.1 Playback engine: embed **libmpv** (statically linked, with FFmpeg)

Writing a decoder/demuxer/renderer stack from scratch would take years. Pure-Rust
decoders (Symphonia etc.) cover only audio and a few formats. The practical way
to "play everything" is to embed an existing engine. Options:

| Option | Formats | Disc support | GPU decode | Embedding in a custom UI | Verdict |
|---|---|---|---|---|---|
| **libmpv** (+ FFmpeg, libplacebo, libass) | Everything FFmpeg supports | DVD (libdvdnav), Blu-ray (libbluray), Audio CD (libcdio) | VAAPI, VDPAU, NVDEC, D3D11VA, VideoToolbox, MediaCodec, Vulkan | Render API (OpenGL/SW) or native child window | **Chosen** |
| libVLC | Everything | Best DVD/BD menu support | Yes | Harder to embed, large plugin tree, weak Rust bindings | Fallback |
| GStreamer | Very wide | Weaker DVD, poor BD | Yes | Good Rust bindings, but hard to ship as one package | No |
| FFmpeg directly (`rsmpeg`/`ffmpeg-next`) | Everything | You build disc navigation yourself | You build it yourself | Full control | Too much work for v1 |

The Rust side wraps libmpv in our own safe API (`player-core` crate). If we ever
need to swap or add an engine (e.g. libVLC for Blu-ray menus), only that crate
changes.

### 1.2 UI framework: **Slint**

The UI has to animate, be skinnable, render on the GPU, and run on Android:

- **Slint**: declarative `.slint` markup, built-in animations/transitions/states,
  GPU renderers (FemtoVG/OpenGL, Skia), **Android support**, and it can draw an
  OpenGL "underlay" so mpv can render video into the same window. → **Chosen.**
- egui: easy, but immediate-mode and hard to make look polished with animations.
- Iced: nice, but no official Android support.
- Tauri: webview UI; embedding hardware-accelerated video under a webview is fragile.

Licensing: Slint is free under its royalty-free license for desktop apps and
GPLv3; embedded/commercial use needs a paid license. Check this before committing.

### 1.3 Video output and GPU usage

- **Hardware decode**: `hwdec=auto-safe` by default, with a user override.
  - Linux: VAAPI (Intel/AMD), NVDEC/VDPAU (NVIDIA), Vulkan
  - Windows: D3D11VA, NVDEC
  - macOS: VideoToolbox
  - Android: MediaCodec
- **Rendering**: the mpv render API draws into an OpenGL FBO that Slint composites,
  so the UI can float animated controls over the video. If GPU init fails,
  fall back to software decode and software rendering.
- **Native-window mode**: on Windows, and for HDR, there is a second mode where
  mpv draws into a native child window (`wid`). That gives full `gpu-next`
  (libplacebo) quality, HDR tone-mapping and the lowest latency. The UI then sits
  in a separate transparent overlay window. A spike in Phase 0 decides which
  mode is the default on each platform.
- Shaders and upscalers (FSR, Anime4K, …) are available through mpv's
  `glsl-shaders`.

### 1.4 Physical media

| Media | How it is played | Notes |
|---|---|---|
| Audio CD | `cdda://` (libcdio + libcdio-paranoia) | Track names from CD-Text, plus MusicBrainz/GnuDB lookup. Optional rip to FLAC. |
| DVD | `dvd://` (libdvdnav/libdvdread) | Titles, chapters, angles, audio/sub tracks. mpv has no DVD menu support: `dvdmenu/` runs libdvdnav itself (§8, item 26). |
| Blu-ray | `bd://` (libbluray) | Titles and chapters work. **BD-J (Java) menus are not supported.** Encrypted discs need libaacs + user-supplied keys (`KEYDB.cfg`). BD+ is not supported. |
| ISO / VIDEO_TS / BDMV folders | Same paths as the discs | Open disc images like real discs |

**Legal note:** libdvdcss (DVD CSS decryption) and AACS decryption are legal
grey areas in some countries (US DMCA, EU). Common practice: ship builds without
them, or load them at runtime only if the user has installed them. Decide per
distribution channel. They are **not** bundled on Android, Mac App Store or
Microsoft Store builds.

Drive detection and auto-play when a disc is inserted: udev (Linux), WM_DEVICECHANGE
(Windows), DiskArbitration (macOS). Android has no optical drives, so it only
gets ISO files.

### 1.5 Online media

- Native through FFmpeg: HTTP(S), HLS, DASH, RTSP, RTMP, SRT, UDP/RTP multicast,
  Icecast/Shoutcast radio (with ICY metadata).
- Site extraction (YouTube, Vimeo, Twitch, SoundCloud, ~1800 sites): **yt-dlp**.
  It is a separate binary that changes weekly, so we ship a standalone yt-dlp
  inside the package. The app updates it automatically into its data folder
  (signature/hash checked). On Android, yt-dlp needs an embedded Python runtime
  (the approach used by the `youtubedl-android` library), which adds ~30 MB.
  Without it, Android is limited to direct URLs. Validate this in a Phase 0 spike.
- Built-in services: internet radio directory (radio-browser.info API), podcasts
  (RSS/Atom, with download for offline), online subtitles (OpenSubtitles API).
- Network shares and local servers: SMB, NFS, SFTP, WebDAV, and DLNA/UPnP browsing.
  Casting to Chromecast/DLNA comes later.

### 1.6 "Single full package"

Everything is statically linked or bundled inside the app:

| Platform | Package | Contents |
|---|---|---|
| Windows | Portable `.exe` + MSI/MSIX installer | One exe with libmpv/FFmpeg static (or one `libmpv-2.dll` next to it) |
| Linux | AppImage (single file), Flatpak, plus `.deb`/`.rpm` | Static libmpv/FFmpeg. GPU drivers (VAAPI/Vulkan) still come from the system, as they must. |
| macOS | Signed + notarized `.app` in a `.dmg`, universal (arm64 + x86_64) | dylibs embedded in the bundle |
| Android | APK / AAB | `libmpv.so` + FFmpeg `.so` inside the APK (based on the mpv-android build scripts) |

A `deps/` build system (meson/cross scripts run in CI) produces static
FFmpeg + libmpv + libplacebo + libass + libdvdnav + libbluray + libcdio for each
target triple. Dependency builds are cached by version hash.

**Licensing consequence:** mpv + FFmpeg built with GPL parts (libdvdnav, libcdio,
x264, …) make the shipped binary **GPL**. The simplest choice is to license the
project GPLv3. An LGPL-only build is possible, but it loses disc support.

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ app (binary)  – startup, config, single-instance, CLI args    │
├──────────────┬───────────────────────────────────────────────┤
│ ui (Slint)   │ skins, themes, animations, views:              │
│              │ Now Playing · Library · Playlist · Mini mode   │
│              │ Video (OSD overlay) · Visualizer · Settings    │
├──────────────┴───────────────────────────────────────────────┤
│ app-state  – central store (actions → reducer → state),       │
│              async runtime (tokio), event bus                 │
├────────────┬───────────┬───────────┬───────────┬─────────────┤
│ player-core│ library   │ online    │ disc      │ platform    │
│ libmpv     │ SQLite DB │ yt-dlp,   │ drive     │ media keys, │
│ wrapper,   │ scanner,  │ radio,    │ detection,│ MPRIS/SMTC/ │
│ playback   │ tags,     │ podcasts, │ CD lookup,│ NowPlaying, │
│ queue, EQ, │ art,      │ subtitles │ disc type │ tray, file  │
│ render API │ playlists │ updater   │ probing   │ associations│
├────────────┴───────────┴───────────┴───────────┴─────────────┤
│ mpv-sys (FFI) │ static libs: FFmpeg, libmpv, libplacebo, ...   │
└──────────────────────────────────────────────────────────────┘
```

### Cargo workspace layout

```
media_player/
├── Cargo.toml                # workspace
├── crates/
│   ├── mpv-sys/              # bindgen FFI + build.rs that links the static libs
│   ├── player-core/          # safe engine API: load, play, seek, tracks, events, render
│   ├── library/              # rusqlite (bundled), lofty tags, scanner (notify), art cache
│   ├── online/               # reqwest, yt-dlp manager, radio-browser, podcasts, OpenSubtitles
│   ├── disc/                 # drive enumeration + insert/eject events per OS
│   ├── platform/             # souvlaki (media keys, MPRIS/SMTC/macOS NowPlaying), tray
│   ├── visualizer/           # spectrum/oscilloscope/Milkdrop-style (wgpu/GL shaders)
│   ├── app-state/            # store, actions, persistence (serde + toml/json)
│   └── ui/                   # .slint files, skins, glue to app-state
├── apps/
│   ├── desktop/              # main binary for Linux/Windows/macOS
│   └── android/              # android-activity entry point + Gradle wrapper
├── deps/                     # scripts to build static native deps per target
├── skins/                    # built-in skins (themes as .slint + assets)
└── .github/workflows/        # CI: build deps, test, package for 4 OSes
```

### Main crates (dependencies)

`slint`, `libmpv2` or own `bindgen` FFI, `tokio`, `rusqlite` (bundled), `lofty`
(tags), `image`, `reqwest` (rustls, so no OpenSSL to ship), `serde`, `notify`,
`souvlaki`, `tray-icon`, `directories`, `tracing`, `rust-i18n` (translations),
`android-activity` + `jni` (Android).

---

## 3. UI / UX concept

The look combines the three players:

- **Winamp**: compact **mini-player mode** (always-on-top strip with spectrum),
  a 10-band EQ with presets, **skins**, spectrum/oscilloscope plus Milkdrop-style
  visualizers, a playlist editor.
- **WMP**: a **media library** with Artists / Albums / Genres / Years, album-art grid,
  smart playlists, "Now Playing" view with large blurred-art background.
- **VLC**: shows every stream detail: track selection (audio/sub/video), sync
  offsets, filters, codec info panel, "Open network stream", disc menu control,
  record/convert later.

Animation and visual style:
- Smooth view transitions (slide/fade), animated album-art flip, hover glow on
  controls, seek bar with thumbnail preview on hover, auto-hiding OSD over
  video with fade.
- Dynamic accent color taken from the album art / video frame.
- Glass/blur panels where the OS supports them (Mica on Windows 11, vibrancy on macOS).
- Dark and light themes. Skins are `.slint` theme packages (colors, fonts,
  layout variants, images) loaded at runtime.
- Android: the same Slint components with a touch layout (bottom sheet player,
  gestures for seek/volume/brightness, picture-in-picture).

---

## 4. Implementation phases

Time estimates assume 1–2 developers working full time. Each phase ends with a
working, tested build.

### Phase 0 — Spikes and foundations (2–3 weeks)
Prove the risky parts before building on them:
1. Build static libmpv + FFmpeg for Linux x86_64 and Windows x86_64 in CI.
2. Slint window + mpv render API drawing video into it with hardware decode;
   measure frame pacing and CPU use. Compare with native `wid` mode.
3. Same on Android (arm64) with MediaCodec.
4. yt-dlp bundling and update approach on desktop and Android.
5. Audio data access for visualizers (see risk R3).
6. Set up the workspace, CI (fmt, clippy, tests), logging, crash reporting.

**Exit:** a decision record for each spike; a "hello video" build on 3 OSes.

### Phase 1 — Core playback MVP (4–6 weeks)
- `player-core`: open file/URL, play/pause/stop, seek, volume, speed, events
  (position, duration, metadata, end-of-file, errors), track lists.
- Playback queue: next/prev, shuffle, repeat, gapless audio, crossfade option.
- Basic Slint UI: main window, transport controls, seek bar, volume,
  drag-and-drop, open file dialog, recent files, fullscreen, keyboard shortcuts.
- Video: hwdec on by default, aspect/zoom, subtitles (embedded + external),
  audio/subtitle track switching, screenshots.
- Settings saved to disk; single-instance (opening a file sends it to the running app).

**Exit:** plays common audio/video files on Linux, Windows, macOS with GPU decode.

### Phase 2 — Modern UI, skins, animations (4–5 weeks)
- Design system: tokens, components, themes; dark/light.
- Now Playing view, playlist panel, video OSD with auto-hide, animated transitions.
- Mini-player mode, always-on-top, tray icon.
- 10-band equalizer + presets, audio normalization (ReplayGain / loudnorm).
- Visualizers v1: spectrum bars, oscilloscope, blurred-art background.
- Skin loader + 3 built-in skins (Modern, Classic-retro, Minimal).
- OS integration: media keys, MPRIS (Linux), SMTC (Windows), Now Playing (macOS),
  file associations.

### Phase 3 — Media library (4–5 weeks)
- SQLite schema: tracks, albums, artists, videos, playlists, play stats.
- Background folder scanner with file watching; tag reading (lofty);
  embedded and folder album art; thumbnails for videos.
- Library views: albums grid, artists, genres, search with instant results,
  sorting, filters.
- Playlists: create/edit, import/export M3U/M3U8/PLS/XSPF/CUE; smart playlists
  (rules like "rating ≥ 4 and genre = Jazz").
- Online metadata and album art lookup (MusicBrainz / Cover Art Archive).

### Phase 4 — Physical media (3–4 weeks)
- Drive detection and auto-play prompt on insert, eject button.
- Audio CD: track list, CD-Text, MusicBrainz lookup, gapless playback;
  optional rip to FLAC/MP3 (later).
- DVD: titles, chapters, angles, tracks (works through mpv today).
- DVD menus: drive libdvdnav ourselves (menu VM, button highlights drawn by our UI), since mpv
  does not render DVD menus. Alternative: use libVLC only for menu playback.
- Blu-ray: title/chapter selection, the main title chosen automatically, tracks;
  AACS through user-provided libaacs + keys where legal.
- ISO and VIDEO_TS/BDMV folders open as discs.

### Phase 5 — Online media (3–4 weeks)
- "Open URL" with automatic detection (direct stream vs. yt-dlp site).
- yt-dlp manager: bundled binary, auto-update, quality selection, playlists.
- Internet radio browser (search, favorites, ICY now-playing titles).
- Podcasts: subscribe, episode list, download, resume position.
- Network: SMB/NFS/SFTP/WebDAV browsing, DLNA/UPnP server discovery.
- Online subtitle search and download.

### Phase 6 — Android (4–6 weeks; can overlap Phases 3–5)
- `apps/android`: Slint on android-activity, Gradle wrapper, signing.
- Touch layout, gestures, background playback through a foreground service,
  MediaSession and notification controls, audio focus, PiP.
- Storage Access Framework for files, scoped storage, Android TV layout (D-pad).

### Phase 7 — Packaging, polish, release (3–4 weeks)
- Installers: MSI/MSIX + portable exe, AppImage + Flatpak + deb/rpm,
  notarized universal DMG, signed APK/AAB.
- Auto-update for desktop (signed update manifests).
- Translations (i18n), accessibility (keyboard navigation, screen reader labels,
  high contrast).
- Performance work: startup time < 1 s, idle CPU near 0, 4K60 HDR playback test matrix.
- Documentation, crash reporting, beta program.

### Later (post 1.0)
Chromecast/DLNA casting, Milkdrop-compatible presets (projectM), recording and
converting streams, A-B repeat and frame stepping UI, lyrics (LRC + synced),
plugin API (WASM), remote control from phone.

**Rough total to 1.0:** about 7–9 months for 1–2 developers.

---

## 5. Testing strategy

- Unit tests on all pure-Rust crates (library, playlists, state reducer, parsers).
- Integration tests for `player-core` using a **headless mpv** (`vo=null`, `ao=null`)
  and a corpus of small sample files: many codecs and containers, broken files,
  subtitles, chapters.
- Disc tests using ISO images (DVD/BD samples, CUE/BIN for CD).
- Online tests against a local test server (HLS/DASH/ICY) to avoid flaky network tests.
- UI: Slint testing backend for component tests, plus screenshot comparison per skin.
- CI matrix: Linux x86_64/arm64, Windows x86_64/arm64, macOS universal, Android arm64/x86_64.
- Manual GPU test matrix: Intel, AMD, NVIDIA, Apple Silicon, Adreno/Mali.

---

## 6. Risks and how to handle them

| # | Risk | Mitigation |
|---|---|---|
| R1 | Static cross-builds of FFmpeg/mpv for 4 OSes are hard and slow | Start in Phase 0; reuse proven build scripts (mpv-winbuild-cmake, mpv-android buildscripts); cache in CI |
| R2 | mpv render API inside Slint may have frame pacing or HDR limits | Keep the native child-window mode as a fallback; decide per platform in Phase 0 |
| R3 | libmpv has no direct API for raw audio samples (needed by visualizers) | Options: mpv's lavfi visualization filters (`showcqt`, `showspectrum`, `avectorscope`) rendered by mpv on the GPU; or an `af=lavfi` tap; or OS audio loopback capture. Pick one in Phase 0. |
| R4 | Blu-ray: BD-J menus and BD+ are not supported by open-source libraries. DVD menus are not supported by mpv | Be clear in the UI: "title playback only"; auto-select the main title; own libdvdnav menu layer for DVD |
| R5 | DVD/BD decryption is legally restricted in some countries | Do not bundle libdvdcss/libaacs in store builds; load them only if the user installed them |
| R6 | yt-dlp breaks when sites change; it is hard to run on Android | Auto-update it; treat site support as "best effort"; direct URLs always work |
| R7 | GPL licensing from FFmpeg/mpv GPL parts | License the project GPLv3 from the start |
| R8 | Slint license terms for some distribution types | Check before release; GPLv3 project licensing is compatible |
| R9 | App store rules (codecs, downloader features) | Separate store build flavours without the restricted parts |

---

## 7. First steps (week 1)

1. `cargo new` workspace with the crates listed above (empty), CI with fmt/clippy/test.
2. Get a prebuilt libmpv working on Linux (`libmpv-dev`) to start coding
   `player-core` right away, while the static build in `deps/` is being made.
3. Minimal Slint window with play/pause and a seek bar driving libmpv (audio first).
4. Add the mpv render API → OpenGL underlay in Slint → first video on screen
   with `hwdec=auto-safe`.
5. Write decision records (`docs/adr/`) for the engine, UI framework, render mode and license.

---

## 8. Status and fix list (v0.3 work, November 2026)

### Done in this round

| # | Problem reported | Fix |
|---|---|---|
| F1 | A disc opened with "Blu-ray" failed with "Couldn't open Blu-ray device" (it was a DVD) | `disc.rs` detects the disc from its contents (CD table of contents; BDMV or VIDEO_TS after mounting through udisks; udev media type as a fallback). The menu choice is only a preference. A mismatch shows "This disc is a DVD, not a Blu-ray: playing it as a DVD". |
| F1b | A Blu-ray stopped playing until it was ejected and put back in | MakeMKV, merely by listing the drives, switches LibreDrive-capable drives to raw reads until the disc is ejected; libaacs then undoes a scrambling that isn't there. The player itself caused it: its MakeMKV check listed the drives (it no longer does, `ripper::CHECK_ARGS`). And `bluray.rs` (v0.50) tries one unit both ways before playing, opening the disc through a folder of links when the drive reads raw. |
| F2 | "Loading failed" when the drive was still spinning up, or empty | Reads the drive status ioctl, waits up to 25 s while the drive is "not ready", and reports "tray open" / "no disc" in plain words. Disc work runs on a worker thread with a busy indicator. |
| F3 | Opening VIDEO_TS/*.VOB gave "unrecognized format" | VOB/IFO/BUP files inside VIDEO_TS, index.bdmv, disc folders and ISO images all play through `dvd://` / `bd://`. On a mounted disc they read the drive itself, so CSS/AACS keys work. ISO type comes from the directory entries in the image, not the file size. |
| F4 | Each Blu-ray needed manual decryption setup | Packages bundle libaacs and libbdplus (and libdvdcss when built with it). Tools → "Download Blu-ray key database" installs KEYDB.cfg. Settings → Blu-ray decryption can switch to MakeMKV's libmmbd. A failed open names the missing piece (libdvdcss, libaacs, key database, BD+). Developers: `scripts/fetch-dev-dvdcss.sh`. |
| F5 | YouTube: "youtube-dl failed: not found" | The app finds yt-dlp (its own copy, the packaged one, or PATH). If a link needs it, it offers to download it (SHA-256 checked against the release list), then retries the link. Settings shows the version and updates it. Packages bundle it. Playback confirmed by the user, 2026-11-24. |
| F6 | H.264 "non-existing PPS" errors on a live stream | Harmless: joining a stream between keyframes. These lines are now logged at debug level. |
| F7 | No menu bar or settings | Menu bar (File, Playback, Audio & Subtitles, View, Tools, Help) with shortcuts, and a Settings panel saved to `~/.config/super-media-player/settings.conf`. |
| F8 | View modes | "Media player" and "Compact (classic)": a small player with time display, scrolling title, bitrate and sample rate, a visualizer window and an expandable playlist. Ctrl+M switches. |
| F9 | Music visual effects | Spinning disc (kept), spectrum bars, waveform, stereo scope, CQT spectrum and spectrogram. Real FFmpeg analysis filters on the playing audio, drawn by mpv on the GPU, in both views. |
| F10 | Artist picture | Top-left / top-right / off. Uses artist/folder/cover images next to the music, and optionally (off by default) looks the artist up on Deezer, with an on-disk cache. |
| F11 | Open a whole folder | File → Open folder (Ctrl+Shift+O). Scans recursively in natural order, skips playlists and disc structures, max 10,000 files. Folders also work on the command line and in "Open location". |
| F13 | Helpers go stale (yt-dlp breaks as sites change; new Blu-rays need new keys) | Background updates (Settings → Update automatically, on by default). yt-dlp is checked weekly against GitHub's latest release and downloaded only when newer (SHA-256 checked). The key database is refreshed monthly, but only once the user has downloaded it. System-managed yt-dlp copies are never touched, and first installs are always asked. |
| F12 | No drag and drop | Dropping files works where winit supports it (X11, Windows, macOS); on Wayland, Settings → Window system → X11. On every system, Ctrl+V plays a copied file, folder or link. |
| F14 | No word when a new version is out | When the player opens it asks GitHub for the latest release and, if it is newer, offers to open the download page (in-player card; "Not now" closes it). Nothing is downloaded by itself. Settings → Online → Tell me about new versions (on by default). v0.69. |
| F15 | Music visuals looked cheap; MilkDrop seemed to do nothing; empty artist corner | v0.70: "Spectrum bars" drawn by the player (meter.rs bands, peak caps that wait then fall, a reflection) instead of FFmpeg's showfreqs; "Spectrum" (CQT) shows only its bars, without the scrolling part; the spectrogram is a smooth heat map at 30 pictures a second (it made hundreds a second and dropped them). MilkDrop skips the presets of its "Waveform" folder (a thin line on black with the rebuilt signal it gets). The artist corner shows a music note when no picture or cover exists. |
| F16 | The pulse line only shows the beat (not voice, instruments, pitch) | v0.71: View → Music overlay (off by default) picks the pulse line or one of five new ones (overlay.rs): ribbons (bass / voice range / bright sounds / beats and tempo), melody trail (the tune's note and the bass note from a second meter with a semitone-wide band per note, C2–B6, run only while it is picked; pitch.rs scores each note with its overtones), heat map (the 20 bands scrolling, 10 s), ring (around the artist picture, colour from how bright the sound is) and part lamps (kick, bass, voice, snare, cymbals by range, and BPM). The old `pulse_line` setting is dropped. |

### Bugs and security issues found while reviewing

- The disc image dialog opened a second (folder) dialog when cancelled. It is now two separate menu items.
- "Screenshot saved" appeared even when the screenshot failed.
- Seeking while idle produced error toasts.
- Command-line file names starting with `-` could not be opened. `--` now ends option parsing.
- libaacs/libbdplus were missing from the packages: libbluray loads them by name at runtime, so the `ldd` scan never saw them. The package self-check now covers every bundled library.
- The CD TOC header is now validated (track range 1–99) before it is used.
- Long toasts were cut off. They now wrap and stay up longer.
- The player is shut down explicitly at exit, not in thread-local destructors.
- Downloads: yt-dlp is verified with SHA-256 over HTTPS. The key database comes over plain HTTP (the only way its site serves it), so it is size-limited and checked for the KEYDB format before being saved (mode 0600). Artist pictures are only fetched from Deezer's image hosts, size-limited and checked for JPEG/PNG magic bytes. All downloads have size caps and are written atomically.
- Settings values are validated against the known options; unknown values are ignored.
- Folder scans do not follow symlinked folders, so a symlink loop can't hang the player.
- Dependencies (v0.69.1): `scripts/check-deps.sh` lists newer versions (only releases 2+ weeks old count) and runs `cargo audit`. rusqlite 0.32 → 0.42 fixes the bundled SQLite CVE-2026-119375; lofty 0.25 drops the unmaintained `paste`; libloading, sha1/sha2, roxmltree, windows-sys and png moved to their current majors. Held back: flacenc 0.5.1 (newest) still uses heapless 0.8, but only `heapless::Vec` of numbers, which none of RUSTSEC-2026-0574…0582 touch; zbus stays 5 while Slint uses 5; the unmaintained crates under Slint wait for Slint.

### Beyond other players (v0.37, November 2026)

Features no mainstream player combines. Each optional native library is loaded
at runtime and only its feature is missing without it.

| Feature | How | Module |
|---|---|---|
| Seek bar previews | A second, hidden mpv (`vo=null`, software decode, 2 threads, keyframe seeks, `screenshot-raw`) started on first hover, closed after a minute idle; pictures cached per file | `thumbs.rs` |
| Command palette (Ctrl+K) | Fuzzy match over every action, setting and playlist item | `palette.rs` |
| Skip intro / credits, next episode | Chapter names, else Chromaprint fingerprints of the first 10 and last 7 minutes of two neighbouring episodes; longest stretch agreeing within 6 bits, gaps up to 3.5 s (the intro-skipper values). Cached on disk | `intro.rs`, `pcm.rs` |
| Night mode, dialogue boost, karaoke | Stages in the tone filter chain: `acompressor`; centre channel ×2 or `dialoguenhance`; stereo centre removed above 160 Hz | `meter.rs` |
| Signal path | mpv's audio params, filters, volume and output, PipeWire's clock (`pw-metadata`) and ALSA `hw_params`; can let PipeWire follow file rates | `signal.rs` |
| Subtitles from speech, search | Hidden mpv decodes 16 kHz WAV, `whisper-cli` writes SRT; models from Hugging Face, SHA-256 pinned | `transcribe.rs` |
| MilkDrop | libprojectM 4 (GLES 3) drawn in the window's GL context before the UI; fed a signal rebuilt from the meter's 20 bands | `milkdrop.rs`, `video.rs` |
| Video enhancement and before/after | CAS, FSR 1.0, Anime4K mode A as mpv user shaders; for the split each final pass is wrapped to pass its input through left of the line | `enhance.rs`, `shaders/` |
| Clips | Hidden mpv in encoding mode (`o=`): MP4 (x264), GIF (own palette), MP3 | `clip.rs` |
| Phone remote | HTTP on the LAN; every path carries a 128-bit secret; QR code | `remote.rs` |
| Watch together | TCP line protocol; host broadcasts state, guests' pause/seek go through the host | `party.rs` |

Still to check by eye on a real GPU (the screen was locked during this round;
the virtual X display used instead drops video frames): FSR and Anime4K
output, and the split line lining up with the picture when it has black bars.
MilkDrop was checked on the real GPU (Wayland, OpenGL ES 3.2).

### Next

The work still to do is tracked in [§10](#10-feature-backlog-compared-with-vlc-and-others).
Already done from the old list: equalizer and presets, ReplayGain, crossfade,
gapless, resume per file, A-B repeat, frame stepping, file associations
(`packaging/linux/super-media-player.desktop`), volume up to 150 %.

---

## 10. Feature backlog (compared with VLC and others)

Compared in November 2026 against VLC, mpv, PotPlayer/MPC-HC, IINA,
foobar2000/MusicBee and Kodi. Work through it from the top; tick items off
here as they land. Effort: S = a day or less, M = a few days, L = a week or more.

### Step 1: everyday basics

Items 1–12 landed in v0.39 (`basics.rs`, `mpris.rs`, `instance.rs`). Settings is now a tabbed window.

- [x] 1. MPRIS (media keys, GNOME's media controls, lock screen) — S
- [x] 2. Single instance: a file opened from the file manager goes to the running player — S
- [x] 3. Recent files (File → Open recent) — S
- [x] 4. Audio delay and subtitle delay, with keys — S
- [x] 5. Subtitle look: size, position, colour, background box — S
- [x] 6. Picture: aspect ratio, zoom and pan, rotate, deinterlace, brightness / contrast / saturation / gamma / hue — S
- [x] 7. Audio output device picker — S
- [x] 8. Chapters menu, chapter ticks on the seek bar, previous / next chapter — S
- [x] 9. Media information panel (codecs, bitrates, HDR, dropped frames, hardware decoding) — S
- [x] 10. Jump to time (Ctrl+T) — S
- [x] 11. Always on top — S
- [x] 12. Sleep timer, and pause after the current file — S
- [x] A. **Advanced navigation** (v0.38) (Tools → Advanced navigation): its own window that can sit beside the player, or docked under the control bar. Space plays / pauses; ←/→ step by a chosen amount (one frame, 1 ms … 5 min); a jog wheel that turns with the media and scrubs it (with sound going forwards); a spring-loaded shuttle and J/K/L keys; fine speed control and pitch correction; exact time and frame readout; go to time; A/B marks. Turning it off restores the normal keys and speed. — M

### Step 2: playlist and audio-player polish

- [x] 13. Save / load playlists (M3U, XSPF, PLS); search, sort, reorder, remove duplicates — M (v0.40, `playlists.rs`)
- [x] 14. Audio passthrough (AC3 / DTS / TrueHD over HDMI or S/PDIF) — S (v0.40, Settings → Audio)
- [x] 15. ~~Scrobbling (Last.fm, ListenBrainz)~~ — removed in v0.49: not used, and it needed accounts and keys.
- [x] 16. Bookmarks inside a file — S (v0.40: Ctrl+B, Playback → Bookmarks, marks on the seek bar)
- [x] 17. Custom keyboard shortcuts and mouse wheel / click actions — M (v0.41, `shortcuts.rs`, Settings → Shortcuts)

### Step 3: bigger features

- [x] 18. Online subtitle search (OpenSubtitles, by file hash), whisper as the fallback — M (v0.42, `opensubs.rs`; needs your free OpenSubtitles API key)
- [x] 19. Record a stream (`stream-record`); Convert / Save the whole file — S / M (v0.42: Tools → Record this stream; Save a clip or convert, now with Whole file, FLAC and Opus)
- [x] 20. Internet radio directory (radio-browser.info) and podcasts — M (v0.43, `library.rs`: File → Library, with saved Radio / Audio / Video links)
- [x] 21. Network sources: SMB, SFTP, WebDAV, DLNA/UPnP browsing — M–L (v0.44, `network.rs`: Library → Network; shares through gvfs, DLNA by SSDP + ContentDirectory)
- [x] 22. HDR tone-mapping choice — S (v0.45: View → HDR tone-mapping, Settings → Video; curve + scene brightness detection, `enhance::set_tone_mapping`)
  - [ ] HDR passthrough on Wayland — **blocked**: needs the compositor's colour-management protocol (GNOME 48+, KDE 6) and a 10-bit HDR surface for the video, which Slint's window doesn't offer yet. Revisit once the user's desktop has HDR.
- [x] 23. Audio CD: MusicBrainz / CD-Text names, rip to FLAC — M (v0.59: tried on a real CD, 2Pac *Greatest Hits* disc 1: names, artists, cover and tags right. Ripping now reads the sectors itself with libcdio-paranoia and encodes FLAC in Rust (`flacenc`): the files are bit-identical to the disc, checked against raw reads; the mpv way was off by up to 2,112 samples at song edges. Tags and cover are written as FLAC blocks before the audio (lofty broke the encoder's stream). The packaged engine had no CD support at all (mpv built without cdda): fixed. v0.46, `cd.rs`: names by MusicBrainz disc ID (cached), else CD-Text over SG_IO; tags ride in each song's EDL so the music view, MPRIS and scrobbling see them; cover from the Cover Art Archive. File → Play disc → Rip audio CD to FLAC: cdparanoia mode 2, tagged + cover, Music/Artist/Album/NN - Title.flac. Settings → Discs turns the online lookup off. Not yet tried on a real CD.)
- [x] 24. Media library (Phase 3): albums, artists, "continue watching", search — L (v0.47: `medialib.rs` index in SQLite with an incremental scan, `art.rs` pictures, `collection.rs` + `ui/media.slint` pages: Home, Albums, Artists, Genres, Songs, Videos, Folders, search. Ctrl+Shift+M. Files without tags get artist and title from "Artist - Title" names. An unplugged drive keeps its entries. 40,000 songs: every page under 0.1 s, off the UI thread.)
  - [ ] Later: AccurateRip check of rips (needs the drive's read offset); audio CDs on Windows (TOC through DeviceIoControl)
  - [ ] Later: smart playlists and ratings, "up next" for series, disc folders and ISOs in the library, watching folders live (it scans when opened)
- [x] 25. Casting to Chromecast / DLNA — L (v0.48, `cast/`: mDNS and SSDP discovery, own CASTV2 (rustls) and SOAP sessions, a media server with a secret in its links that runs only while casting. Sent as it is, or converted into a pipe by the ffmpeg program (picture copied where possible) or a hidden mpv; a refused file is tried again converted. Tried against `scripts/fake-cast-devices.py`, which speaks both protocols and decodes what it gets; **not yet on a real Chromecast or TV**.)
  - [ ] Try on a real Chromecast and TV (none on the user's network yet)
  - [ ] Later: hardware encoding, discs, Chromecast playback speed, several audio tracks without converting
- [x] 26. DVD menus (own libdvdnav layer) — L (v0.49, `dvdmenu/`: libdvdnav runs the disc's program; its menus and short clips reach mpv as our own stream (`smpdvd://`, `player-core/stream.rs`) with continuous times, the chosen button is laid over the picture (own subpicture decoder); titles over a minute are handed to mpv's `dvd://`, and afterwards the program goes on from the title's end. Keys, mouse, Disc menu (Ctrl+Shift+D), Back to the film, Main film. Settings → Discs turns it off. Tried on an authored DVD (motion, still and chapter menus) and a commercial, copy-protected series DVD.) Also: `dvd://` no longer runs on into the menu at a title's end.
  - [x] Rip a DVD or Blu-ray to named video files (v0.59, `discrip.rs`: File → Play disc → Rip disc to video files. MakeMKV copies the titles losslessly; series discs are recognised from the disc's name and volume label ("Reacher: Season Three Disc 2", "FRIENDS_S01_D2"), the episode titles picked by length ("play all" and extras left out), named from TVmaze (`Show - S03E05 - Name.mkv` in `Videos/Show/Season 03`), first episode adjustable. Tried on Reacher season 3 disc 2: episodes 5–8 named right, one ripped with both audio tracks, both subtitles and 8 chapters.)
  - [ ] Later: HDMV Blu-ray menus, choosing angles, buttons inside films

### Step 4: beyond VLC

- [x] 27. Automatic subtitle sync: line downloaded subtitles up with the speech — M (v0.50, `subsync.rs`: the loudness of the voice band is cross-correlated with when subtitles are on screen, at the usual frame-rate ratios, then part by part for cuts. No speech model needed, any language. On four TV episodes with known subtitles: within 0.1 s; another episode's subtitles are refused. Audio & Subtitles → Subtitles → Line up with the speech, and by itself after a download that isn't an exact match.)
  - [ ] Later: subtitles built into the film; a cut during a long silent or music scene can be placed a few lines off
- [x] 28. SponsorBlock for YouTube, through the skip button — S–M (v0.51, `sponsor.rs`: asked by a four-character hash prefix, so the service can't tell what is watched; marks on the seek bar; button / by itself / off and three levels in Settings → Online. Skipping by itself happens only when playback runs into a part, once. Also for yt-dlp downloads, by the ID in the name or the link in the tags. Segments marked on another cut of the video are left out, as in yt-dlp.)
- [x] B. **Title bar that matches the player** — M (v0.52, `frame.rs`, `ui/titlebar.slint`: GNOME reported no dark preference, so its frame stayed light grey. The window has no frame now; title, drag strip and window buttons share the menu bar's row, in `Theme` colours, so skins' `theme.*` reach it. Double-click maximizes, right-click opens the desktop's window menu (our own on X11), all edges and corners resize, corners are rounded by a small GL shader after drawing. Settings → Look → Title bar: player's own / desktop's. The desktop's is dark on Wayland; GNOME's X11 frames don't listen. Also fixed: after resizing a skin from its edge, the next press went to that edge again. Checked in a private headless GNOME 46 session on X11 and Wayland.)
  - [ ] Later: square corners and no edge on the tiled sides of a tiled window; a shadow (GNOME draws none without a frame); the detached playlist and Advanced navigation windows still have the desktop's bar; new guide pictures (start and settings are done)
- [x] 29. Auto-crop black bars — S–M (v0.56, `autocrop.rs`: C, View → Picture, or Settings → Video; mpv's `video-crop`, so hardware decoding stays. What is kept is every place a picture was ever seen. A file is looked through by a hidden player at 24 moments, in a second or two, and the result is cached. Discs and streams: a filter branch copies two frames a second back from the graphics card for `cropdetect`, and the crop comes after 10 s without news; it opens at once when a picture appears in the bars. About 1 % of a core at 1080p and 9 % at 4K 10-bit (VA-API), only while on. Logos, text and one-sided darkness aren't taken for the picture; picture subtitles stay where they were meant. Checked on made-up files and HTTP streams; **not yet on a real disc or on NVIDIA**.)
  - [ ] Check on a real disc and on an NVIDIA card (no NVIDIA here)
  - [ ] Later: cropping that changes with each scene; remembering a disc's bars
- [x] 30. Skip silence for podcasts and lectures; remember speed per file — S (v0.53, `silence.rs`, `pace.rs`: an FFmpeg graph looks 0.25 s ahead and 0.15 s back, and drops frames where all of that is quiet. Times stay true, so the position and subtitles jump along; for video mpv's `mc=1` brings the picture back within 0.2 s. Raising mpv's speed instead was tried: the position jumps about a second each way. The threshold is learned from the speech and the background, checked on speech with clean and noisy backgrounds. About 1.6 % of a core, only while on. Speed and skipping come back per file and per podcast show; files from the same folder go on as the last one.)
  - [ ] Later: live streams
- [x] 31. Load user mpv Lua scripts from a folder — S (v0.57, `scripts.rs`: `~/.config/super-media-player/scripts`, with `script-opts`, `script-modules` and `fonts` beside it. Settings → Scripts lists them with their keys, errors and a switch each. mpv's configuration folder is a private one with links to those four, since an `mpv.conf` there would win over the player's options (even `vo`). Keys a script bound are handed to mpv from `input-bindings`: forced ones first (menus, text input), ordinary ones only if the player doesn't use the key; the rest by command palette or Settings → Shortcuts. A script's `fullscreen` and `quit` are followed. A script that never returns gets a hint after 8 s; a stopped one is reported. Checked with mpv 0.37 and 0.42: libmpv 0.37 crashes on `keyup` without an argument.)
  - [ ] Later: mouse buttons and position; script messages while music plays; scripts' own drawings start under the menu bar in a window; reloading a changed script without a restart

### Step 5: reach

- [x] 32. Packages for Linux, Windows and macOS (v0.59, §9): .deb, .rpm, AppImage and tarball from one Ubuntu 22.04 container; the Windows installer and portable zip cross-built in it; the macOS .dmg from GitHub Actions.
  - [ ] Android: a port rather than a package (touch UI, libmpv for Android, Slint's Android backend)
  - [ ] Flatpak (needs flatpak-builder on the build machine)
  - [ ] Signed Windows installer and notarized macOS app (need paid certificates)
- [ ] 33. Translations and accessibility

Older ideas still open: Wayland drag and drop without XWayland (needs winit
0.31 or our own `wl_data_device`), auto-play prompt and eject for discs, light
theme and more compact looks.

---

## 9. Packaging

All Linux packages are built in the smp-build container (`packaging/linux/build.Containerfile`,
Ubuntu 22.04, rootless podman on the build machine; `scripts/in-container.sh`), so they run on
Ubuntu 22.04 / Mint 21 / Debian 12 / Fedora 36 and newer, and the build machine needs no build
packages. The daily check rebuilds the image with that day's Ubuntu updates.

- [x] **AppImage**: `scripts/package-linux.sh`, from the same staged folder (pinned appimagetool and runtime).
- [x] **.deb**: as before, now with `libc6 (>= 2.35)`.
- [x] **.rpm**: `rpmbuild` in the container; requires base libraries by soname, so it fits Fedora and openSUSE.
- [x] **Windows**: `scripts/package-windows.sh` in the container: cargo-xwin (MSVC target), libmpv from shinchiro's mpv-winbuild (pinned, SHA-256), yt-dlp.exe, NSIS installer (Start menu, Open with, uninstaller) and a portable zip. Drive letters for DVDs and Blu-rays; single instance over a localhost port with a secret. Not yet: audio CDs, disc decryption.
- [x] **macOS**: `scripts/package-macos.sh` on GitHub's macOS runners (`.github/workflows/release.yml`): Homebrew's libmpv bundled with dylibbundler, a .dmg per architecture. Unsigned, and untried until the code is on GitHub.
- Note, engine on 22.04: mpv's PipeWire output needs libpipewire 0.3.57 (22.04 has 0.3.48), so the packages use the PulseAudio output (PipeWire desktops serve it). dav1d, libdisplay-info are built from source and bundled; a newer libdrm only to compile against.
- [ ] arm64 Linux packages.
