//! Draw each player view offscreen to a PNG, to check how a skin looks
//! without running the player (and without screen capture, which GNOME on
//! Wayland doesn't allow):
//!
//!     cargo run -p super-media-player --example render-views -- /tmp/views
//!
//! Uses Slint's software renderer; the player engine isn't started, so the
//! views show sample data.

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, VecModel};

slint::include_modules!();

// The player's own skin loader, to draw the built-in skins.
#[allow(dead_code)]
#[path = "../src/settings.rs"]
mod settings;
#[allow(dead_code)]
#[path = "../src/skins.rs"]
mod skins;

/// The window, and a clock that only moves when told to (so each shot can
/// jump past its animations: a bar sliding in, a panel fading).
struct Offscreen(Rc<MinimalSoftwareWindow>, Rc<std::cell::Cell<std::time::Duration>>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> std::time::Duration {
        self.1.get()
    }
}

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out).unwrap();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let clock = Rc::new(std::cell::Cell::new(std::time::Duration::ZERO));
    slint::platform::set_platform(Box::new(Offscreen(window.clone(), clock.clone()))).unwrap();

    let ui = AppWindow::new().unwrap();
    ui.set_starting(false);
    ui.set_idle(false);
    ui.set_paused(false);
    ui.set_position(74.0);
    ui.set_duration(225.0);
    ui.set_volume(80.0);
    ui.set_media_title("Sample Artist - A Song With A Fairly Long Title".into());
    ui.set_audio_info("MP3 · 320 kbps · 44.1 kHz".into());
    let levels: Vec<f32> = (0..20).map(|i| (0.9 - i as f32 * 0.03) * (0.6 + 0.4 * ((i * 7 % 5) as f32 / 4.0))).collect();
    ui.set_spectrum(Rc::new(VecModel::from(levels)).into());
    let items: Vec<PlaylistItem> = (1..=6)
        .map(|i| PlaylistItem { title: format!("Track {i}").into(), detail: "Music".into(), current: i == 2 })
        .collect();
    ui.set_playlist(Rc::new(VecModel::from(items)).into());

    let check = |level, title: &str, detail: &str, action: &str, label: &str, command: &str| CheckItem {
        level,
        title: title.into(),
        detail: detail.into(),
        action: action.into(),
        action_label: label.into(),
        command: command.into(),
    };
    ui.set_check_items(
        Rc::new(VecModel::from(vec![
            check(3, "The drive won't read the disc in it", "/dev/sr1 reports a disc but refuses to read it. This is what an Ultra HD Blu-ray does in a drive that isn't UHD-friendly.", "", "", ""),
            check(2, "yt-dlp is not installed", "YouTube and ~1800 other sites need it.", "install-ytdlp", "Install yt-dlp", ""),
            check(1, "MakeMKV is installed but broken", "makemkvcon won't start (libavcodec.so.58 missing).", "", "", "sudo apt install build-essential pkg-config\ncd makemkv-oss-<version> && ./configure && make && sudo make install"),
            check(0, "Blu-ray key database", "~/.config/aacs/KEYDB.cfg (63 MB, 0 days old).", "", "", ""),
        ]))
        .into(),
    );
    ui.set_check_summary("1 problem, 1 warning".into());
    ui.set_check_report("report".into());

    // (file name, view index, size, playlist open). The skins are the whole
    // window; only the full player (view 0) has a menu bar on top.
    const MENU: u32 = 40;
    let shots = [
        ("music-pulse", 0, (1100, 640), false),
        ("music-particles", 0, (1100, 640), false),
        ("music-info", 0, (1100, 640), false),
        ("equalizer", 0, (1100, 640), false),
        ("settings", 0, (900, 760), false),
        ("settings-audio", 0, (1100, 680), false),
        ("settings-subtitles", 0, (1100, 680), false),
        ("settings-video", 0, (1100, 680), false),
        ("player-playlist", 0, (1100, 680), true),
        ("settings-online", 0, (1100, 680), false),
        ("settings-shortcuts", 0, (1100, 680), false),
        ("settings-scripts", 0, (1100, 680), false),
        ("settings-scripts-min", 0, (520, 420), false),
        ("library-radio", 0, (1100, 720), false),
        ("library-network", 0, (1100, 720), false),
        ("library-saved", 2, (600, 560), false),
        ("library-saved-min", 2, (470, 316), true),
        ("library-network-min", 2, (470, 560), true),
        ("library-radio-min", 0, (520, 420), false),
        ("settings-min", 2, (600, 560), false),
        ("settings-shortcuts-min", 0, (520, 420), false),
        ("settings-audio-min", 2, (600, 560), false),
        ("equalizer-min", 2, (600, 560), false),
        ("makemkv-min", 2, (600, 560), false),
        ("system-check-min", 2, (600, 560), false),
        ("cast-min", 2, (600, 560), false),
        ("clip-min", 2, (600, 560), false),
        ("remote-min", 2, (600, 560), false),
        ("party-min", 2, (600, 560), false),
        ("transcript-min", 2, (600, 560), false),
        ("signal-path-min", 2, (600, 560), false),
        ("media-info-min", 2, (600, 560), false),
        ("subsearch-min", 2, (600, 560), false),
        ("subsync", 0, (900, 600), false),
        ("subsync-min", 2, (600, 560), false),
        ("media-home", 0, (1100, 760), false),
        ("media-album", 0, (900, 640), false),
        ("media-empty", 0, (1100, 640), false),
        ("picture", 0, (1100, 680), false),
        ("media-info", 0, (1100, 680), false),
        ("classic-wide-playlist", 2, (820, 560), true),
        ("compact", 1, (500, 210), false),
        ("classic", 2, (550, 196), false),
        ("classic-playlist", 2, (550, 496), true),
        ("classic-detached", 2, (550, 196), true),
        ("silver", 3, (600, 340), false),
        ("silver-playlist", 3, (600, 640), true),
        ("system-check", 0, (900, 640), false),
        ("makemkv", 0, (900, 760), false),
        ("ripping", 0, (900, 560), false),
        ("ripping-compact", 1, (500, 210), false),
        ("rip-scanning", 0, (900, 560), false),
        ("ask", 0, (900, 560), false),
        ("palette", 0, (1100, 640), false),
        ("signal-path", 0, (1100, 640), false),
        ("karaoke", 0, (1100, 640), false),
        ("transcript", 0, (1100, 640), false),
        ("clip", 0, (1100, 640), false),
        ("remote", 0, (1100, 680), false),
        ("cast", 0, (1100, 680), false),
        ("cast-playing", 0, (1100, 640), false),
        ("party", 0, (1100, 640), false),
        ("skip-intro", 0, (1100, 640), false),
        ("skip-sponsor", 0, (1100, 640), false),
        ("navigation", 0, (1100, 640), false),
        // Skins: "skin-<id>" draws skin <id> on its layout.
        ("skin-aurora", 0, (1100, 640), false),
        ("skin-ember", 1, (500, 210), false),
        ("skin-amber", 2, (550, 396), true),
        ("skin-ice", 2, (550, 196), false),
        ("skin-midnight", 3, (600, 640), true),
        ("skin-rose-gold", 3, (600, 340), false),
    ];
    let (all_skins, _) = skins::load_all();
    // A sample pulse line (pulse.rs draws the real one): a small wobble with
    // a heartbeat spike now and then, in a viewbox shaped like the line.
    let pulse = |view_h: f32| -> String {
        let spike = [0.12f32, -0.18, 1.0, -0.62, 0.22, 0.08];
        (0..750)
            .map(|i| {
                let mut v = 0.06 * (i as f32 * 1.1).sin();
                let beat = i % 16;
                if beat < 6 {
                    v += spike[beat] * (0.35 + 0.4 * ((i / 16 * 7 % 5) as f32 / 4.0));
                }
                let mid = view_h / 2.0;
                format!("{}{} {:.1} ", if i == 0 { 'M' } else { 'L' }, i, mid - v * mid * 0.92)
            })
            .collect()
    };
    ui.set_beat(0.6);
    ui.set_energy(0.5);
    for (file, view, (w, h), list) in shots {
        // "-min": the same panel, at the size a skinned window grows to.
        let name = file.strip_suffix("-min").unwrap_or(file);
        ui.set_s_view_mode(view);
        let skin = name
            .strip_prefix("skin-")
            .and_then(|id| all_skins.iter().find(|s| s.id == id))
            .unwrap_or(&all_skins[view as usize]);
        skins::apply(&ui, None, skin);
        ui.set_s_visualizer(if name == "music-particles" { 1 } else { 0 });
        ui.set_info_open(name == "music-info");
        ui.set_eq_open(name == "equalizer");
        ui.set_settings_open(name.starts_with("settings"));
        ui.set_chapter_times(Rc::new(VecModel::from(vec![0.0f32, 60.0, 140.0])).into());
        ui.set_settings_tab(match name {
            "settings-audio" => 2,
            "settings-subtitles" => 5,
            "settings-video" => 4,
            "settings-online" => 7,
            "settings-shortcuts" => 6,
            "settings-scripts" | "settings-scripts-min" => 10,
            _ => 0,
        });
        ui.set_picture_open(name == "picture");
        ui.set_library_open(name.starts_with("library"));
        ui.set_media_open(name.starts_with("media-") && name != "media-info");
        media_library(&ui, name);
        if name == "library-network" {
            ui.set_library_tab(3);
            ui.set_library_status("1 media server found".into());
            let r = |t: &str, d: &str, a: &str, b: &str| LibraryRow { title: t.into(), detail: d.into(), primary: a.into(), secondary: b.into() };
            ui.set_library_rows(Rc::new(VecModel::from(vec![
                r("Living room NAS", "Media server (DLNA / UPnP) on your network", "Open", ""),
                r("smb://nas/music", "Saved share · dima", "Connect", "Forget"),
            ])).into());
        }
        ui.set_subsearch_open(name == "subsearch");
        ui.set_subsync_open(name == "subsync");
        ui.set_subsync_done(true);
        ui.set_subsync_status("Done: moved 2.3 s earlier, and fitted to the frame rate (-4.1 %).".into());
        if name == "library-saved" {
            ui.set_library_tab(0);
            ui.set_library_status("".into());
            let r = |t: &str, d: &str| LibraryRow { title: t.into(), detail: d.into(), primary: "Play".into(), secondary: "Remove".into() };
            ui.set_library_rows(Rc::new(VecModel::from(vec![
                r("Kiss FM Chișinău", "https://live.kissfm.ro/kissfm.chisinau"),
                r("Hit FM", "https://radio.example/hit_fm_hot_hits/hitfm_hothits128.aac"),
            ])).into());
        }
        if name == "library-radio" {
            ui.set_library_tab(1);
            ui.set_library_country("MD".into());
            ui.set_library_status("83 found".into());
            let r = |t: &str, d: &str, saved: bool| LibraryRow { title: t.into(), detail: d.into(), primary: "Play".into(), secondary: if saved { "★ Saved" } else { "☆ Save" }.into() };
            ui.set_library_rows(Rc::new(VecModel::from(vec![
                r("Kiss FM Chișinău", "Moldova · top hits, top40 · AAC 80 kbit/s", true),
                r("Radio Moldova", "Moldova · news, talk · MP3 128 kbit/s", false),
                r("Hit FM", "Moldova · pop · AAC 64 kbit/s", false),
            ])).into());
        }
        ui.set_shortcut_rows(Rc::new(VecModel::from(vec![
            ShortcutRow { keys: "Shift+Right".into(), action: "Forward 1 minute".into() },
            ShortcutRow { keys: "F".into(), action: "Nothing (key turned off)".into() },
        ])).into());
        ui.set_bookmark_times(Rc::new(VecModel::from(vec![95.0f32])).into());
        ui.set_media_info_open(name == "media-info");
        if name == "picture" {
            ui.set_picture(PictureState { brightness: 12.0, contrast: 5.0, zoom: 0.26, aspect: 1, ..Default::default() });
        }
        if name == "media-info" {
            let row = |l: &str, v: &str, h: bool| InfoRow { label: l.into(), value: v.into(), heading: h };
            ui.set_media_info_rows(Rc::new(VecModel::from(vec![
                row("FILE", "", true),
                row("Location", "/home/me/Videos/film.mkv", false),
                row("Format", "matroska,webm", false),
                row("Size", "4.21 GB", false),
                row("VIDEO", "", true),
                row("Codec", "hevc (H.265 / HEVC)", false),
                row("Resolution", "3840 × 2160", false),
                row("HDR", "HDR10 (PQ), peak 1000 nits", false),
                row("Hardware decoding", "Yes (vaapi)", false),
                row("Dropped frames", "0 by the display, 0 by the decoder", false),
                row("AUDIO", "", true),
                row("Codec", "eac3 (Dolby Digital Plus)", false),
                row("Channels", "5.1", false),
            ])).into());
        }
        ui.set_s_eq_enabled(true);
        ui.set_eq_gains(Rc::new(VecModel::from(vec![5.0f32, 4.0, 2.0, 0.0, -1.0, -1.0, 0.0, 2.0, 4.0, 5.0])).into());
        ui.set_eq_preset_names(Rc::new(VecModel::from(vec![slint::SharedString::from("Flat"), "Loudness".into()])).into());
        if name.starts_with("music") || name == "skin-aurora" {
            ui.set_media_artist("Sample Artist".into());
            ui.set_media_album("Sample Album".into());
            ui.set_media_year("1999".into());
            ui.set_media_genre("Electronic".into());
            ui.set_media_track("2".into());
            ui.set_about_text("Sample Artist is an electronic music duo formed in 1993. Their records mixed house with funk and disco, and they became one of the most influential acts of the genre.".into());
            ui.set_about_source("Wikipedia".into());
            ui.set_lyrics_synced(true);
            ui.set_lyrics_text("First line of the song\nThe line being sung right now\nAnd the one that comes next".into());
            ui.set_lyric_prev("First line of the song".into());
            ui.set_lyric_current("The line being sung right now".into());
            ui.set_lyric_next("And the one that comes next".into());
        }
        ui.set_playlist_open(list);
        ui.set_playlist_detached(name.ends_with("detached"));
        ui.set_check_open(name == "system-check");
        ui.set_makemkv_open(name == "makemkv");
        ui.set_rip_active(name.starts_with("ripping") || name == "rip-scanning");
        ui.set_rip_percent(if name == "rip-scanning" { -1.0 } else { 0.42 });
        ui.set_rip_title(if name == "rip-scanning" { "Reading the disc…" } else { "HOLIDAY FILM" }.into());
        ui.set_ask_open(name == "ask");
        ui.set_ask_title("Rip & play".into());
        ui.set_ask_text("Copy \"HOLIDAY FILM\" (2:35:31, 7.6 GB) from the DVD drive (TSST SH-216DB, /dev/sr0) to\n/home/alex/Videos/Super Media Player/Rip & play (temporary)/HOLIDAY FILM\n\nIt starts playing when the copy is done. A DVD takes 10 to 30 minutes, an Ultra HD movie 20 to 60; you can cancel at any time.\n\nThe copy is temporary: it is deleted when playback ends, when you play something else, or when you close the player.".into());
        ui.set_ask_ok_label("Rip & play".into());
        ui.set_ask_cancel_label("Cancel".into());
        ui.set_rip_drive("Blu-ray drive (LG WH14NS40, /dev/sr1)".into());
        ui.set_rip_stage("Saving to MKV file".into());
        ui.set_rip_eta("about 12 min left".into());
        ui.set_makemkv_status("MakeMKV (Flatpak) 2.0.0 is installed and registered: ready for Rip & play.".into());
        ui.set_makemkv_ready(true);
        ui.set_makemkv_installed(true);
        ui.set_makemkv_install_command("flatpak install -y flathub com.makemkv.MakeMKV".into());
        ui.set_makemkv_update_command("flatpak update -y com.makemkv.MakeMKV".into());
        if name == "rip-scanning" {
            ui.set_rip_heading("READING THE DISC".into());
            ui.set_rip_drive("".into());
            ui.set_rip_eta("".into());
            ui.set_rip_stage("Waiting for the Blu-ray drive (LG WH14NS40, /dev/sr1) to get ready (up to 25 s)…".into());
        }
        extras(&ui, name);
        let h = if view == 0 { h + MENU } else { h };
        window.set_size(slint::PhysicalSize::new(w, h));
        ui.show().unwrap();
        ui.set_pulse_path(pulse(750.0 * ui.get_pulse_aspect()).into());
        let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
        // Draw once so the changes start their animations, then jump ahead
        // until they are over and draw what is left.
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut buf, w as usize);
        });
        clock.set(clock.get() + std::time::Duration::from_secs(5));
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut buf, w as usize);
        });
        let rgba: Vec<u8> = buf.iter().flat_map(|p| [p.red, p.green, p.blue, 255]).collect();
        let path = out.join(format!("{file}.png"));
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        println!("{}", path.display());
    }
}

/// Sample data for the panels in extras.slint.
fn extras(ui: &AppWindow, name: &str) {
    use slint::SharedString as S;
    ui.set_palette_open(name == "palette");
    ui.set_s_nav_enabled(name == "navigation");
    ui.set_s_nav_docked(true);
    if name == "navigation" {
        let labels = |v: &[&str]| -> slint::ModelRc<S> {
            Rc::new(VecModel::from(v.iter().map(|x| S::from(*x)).collect::<Vec<_>>())).into()
        };
        ui.set_nav_step_labels(labels(&["1 frame", "1 ms", "10 ms", "40 ms", "100 ms", "1 s", "5 s"]));
        ui.set_nav_turn_labels(labels(&["0.5 s", "1 s", "2 s", "5 s"]));
        ui.set_nav_state(NavState {
            position: 74.25,
            duration: 225.0,
            paused: true,
            idle: false,
            has_video: true,
            clock: "1:14.250".into(),
            total: "3:45.000".into(),
            frame: "Frame 1781 of 5400 · 23.98 fps".into(),
            chapter: "Chapter 2 of 6: The Chase".into(),
            speed: 1.0,
            pitch: true,
            jog_sound: true,
            step_sound: false,
            step_index: 3,
            step_text: "40 ms".into(),
            custom_step: S::default(),
            turn_index: 2,
            turn_seconds: 2.0,
            shuttle: 0.4,
            shuttle_label: "Forward 1.5×".into(),
            mark_a: "1:02.000".into(),
            mark_b: "1:20.500".into(),
        });
    }
    if name == "palette" {
        let items = vec![
            PaletteItem { id: "night".into(), title: "Night mode".into(), detail: "".into() },
            PaletteItem { id: "next".into(), title: "Next".into(), detail: "N".into() },
            PaletteItem { id: "play:1".into(), title: "Play: Track 2".into(), detail: "Playlist".into() },
        ];
        ui.set_palette_items(Rc::new(VecModel::from(items)).into());
    }
    ui.set_signal_open(name == "signal-path");
    let st = |t: &str, d: &str, l| SignalStage { title: t.into(), detail: d.into(), level: l };
    ui.set_signal_stages(
        Rc::new(VecModel::from(vec![
            st("Source", "FLAC · 16-bit integer · 44100 Hz · stereo · lossless", 0),
            st("Filters", "None: the decoded samples pass straight on", 0),
            st("Volume", "100%: untouched (use the sound card's or amplifier's volume)", 0),
            st("Player output", "pipewire · 16-bit integer · 44100 Hz · stereo", 0),
            st("Sound server", "PipeWire clock 48000 Hz; may switch to: 48000. It converts 44100 Hz to 48000 Hz.", 1),
            st("Sound card", "A50 · S16_LE · 48000 Hz", 1),
        ]))
        .into(),
    );
    ui.set_signal_verdict("Clean, but converted on the way (see the yellow stages)".into());
    ui.set_signal_level(1);
    ui.set_signal_can_fix(true);
    ui.set_karaoke_open(name == "karaoke");
    ui.set_karaoke(name == "karaoke");
    ui.set_lyric_progress(0.45);
    if name == "karaoke" {
        ui.set_lyrics_synced(true);
        ui.set_lyric_prev("First line of the song".into());
        ui.set_lyric_current("The line being sung right now".into());
        ui.set_lyric_next("And the one that comes next".into());
    }
    ui.set_transcript_open(name == "transcript");
    ui.set_has_transcript(true);
    ui.set_can_transcribe(true);
    ui.set_transcript_status("Subtitles are ready: search them below.".into());
    let tl = |t: f32, c: &str, x: &str| TranscriptLine { time: t, clock: c.into(), text: x.into() };
    ui.set_transcript_results(
        Rc::new(VecModel::from(vec![
            tl(62.0, "1:02", "The password is in the drawer."),
            tl(1514.0, "25:14", "Did you change the password again?"),
        ]))
        .into(),
    );
    ui.set_clip_open(name == "clip");
    ui.set_has_video(name == "clip" || name == "skip-intro");
    ui.set_clip_start(62.0);
    ui.set_clip_end(74.5);
    ui.set_clip_working(name == "clip");
    ui.set_clip_progress(0.4);
    ui.set_clip_status("Saving /home/you/Videos/Film clip 0-01-02.mp4…".into());
    ui.set_remote_open(name == "remote");
    ui.set_remote_running(true);
    ui.set_remote_address("http://192.168.1.20:47801/3f2a…/".into());
    ui.set_remote_status("Keep this window open while you use it.".into());
    ui.set_cast_open(name == "cast");
    ui.set_cast_active(name.starts_with("cast"));
    ui.set_cast_name("Living room TV".into());
    ui.set_cast_state("Playing, picture as it is, sound converted".into());
    let device = |name: &str, detail: &str, sound_only, current| CastDevice {
        name: name.into(),
        detail: detail.into(),
        sound_only,
        current,
    };
    ui.set_cast_devices(
        Rc::new(VecModel::from(vec![
            device("Bedroom TV", "DLNA · Samsung UE40", false, false),
            device("Kitchen speaker", "Chromecast, sound only · Nest Audio", true, false),
            device("Living room TV", "Chromecast", false, true),
        ]))
        .into(),
    );
    ui.set_party_open(name == "party");
    ui.set_party_mode(1);
    ui.set_party_invite("192.168.1.20:47800/ab12cd34".into());
    ui.set_party_name("Mia".into());
    ui.set_party_status("In step with everyone.".into());
    ui.set_party_people(Rc::new(VecModel::from(vec![S::from("Mia (host, you)"), "Ann".into(), "Bob".into()])).into());
    ui.set_skip_text(match name {
        "skip-intro" => "Skip intro",
        "skip-sponsor" => "Skip sponsor",
        _ => "",
    }.into());
    let mark = |start, end, main| SeekMark { start, end, main };
    let marks = if name == "skip-sponsor" { vec![mark(30.0, 75.0, true), mark(150.0, 151.0, true), mark(190.0, 220.0, false)] } else { vec![] };
    ui.set_sponsor_marks(Rc::new(VecModel::from(marks)).into());
    ui.set_intro_start(if name == "skip-intro" { 40.0 } else { -1.0 });
    ui.set_intro_end(if name == "skip-intro" { 95.0 } else { -1.0 });
}

/// Sample pages of the media library.
fn media_library(ui: &AppWindow, name: &str) {
    use slint::SharedString as S;
    let card = |t: &str, s: &str, icon: i32, shape: i32, corner: &str, progress: f32| MediaCard {
        title: t.into(),
        subtitle: s.into(),
        corner: corner.into(),
        icon,
        shape,
        progress,
        removable: progress >= 0.0,
        ..Default::default()
    };
    let head = |t: &str| MediaLine { kind: 0, title: t.into(), ..Default::default() };
    let row = |first, count, columns, shape| MediaLine { kind: 1, first, count, columns, shape, ..Default::default() };
    let song = |i: i32, t: &str, d: &str, r: &str| MediaLine {
        kind: 2, title: t.into(), detail: d.into(), right: r.into(), number: (i + 1).to_string().into(),
        path: format!("/{i}").into(), first: i, ..Default::default()
    };
    ui.set_media_empty(name == "media-empty");
    ui.set_media_page(if name == "media-album" { 1 } else { 0 });
    ui.set_media_can_back(name == "media-album");
    ui.set_media_can_play(name == "media-album");
    ui.set_media_has_head(name == "media-album");
    ui.set_media_playing("/1".into());
    ui.set_media_status(if name == "media-home" { "Reading 1200 of 4312…" } else { "" }.into());
    if name == "media-album" {
        ui.set_media_heading("An Album With Quite A Long Name".into());
        ui.set_media_subheading("Some Artist · 2019 · 3 songs · 11 min".into());
        ui.set_media_head(card("", "", 0, 0, "", -1.0));
        ui.set_media_cards(Default::default());
        ui.set_media_lines(Rc::new(VecModel::from(vec![
            song(0, "First song", "Some Artist", "3:41"),
            song(1, "Second song", "Some Artist feat. Other", "4:05"),
            song(2, "Third song", "Some Artist", "3:12"),
        ])).into());
    } else {
        ui.set_media_heading("Home".into());
        ui.set_media_subheading("4312 songs · 87 videos".into());
        ui.set_media_sorts(Rc::new(VecModel::from(Vec::<S>::new())).into());
        ui.set_media_cards(Rc::new(VecModel::from(vec![
            card("A Film (2021)", "Stopped at 41:20", 2, 2, "1:02:11 left", 0.4),
            card("Series S01E03", "Stopped at 12:04", 2, 2, "31:00 left", 0.28),
            card("First Album", "Artist · 2020", 0, 0, "", -1.0),
            card("Second Album", "Other Artist · 1998", 0, 0, "", -1.0),
            card("Band", "2 albums · 24 songs", 1, 1, "", -1.0),
            card("Rock", "Music", 3, 0, "12 songs", -1.0),
        ])).into());
        ui.set_media_lines(if name == "media-empty" { Default::default() } else { Rc::new(VecModel::from(vec![
            head("Continue watching"), row(0, 2, 3, 2), head("New albums"), row(2, 4, 5, 0), head("New songs"),
            song(0, "A song", "Artist · Folder", "3:41"),
        ])).into() });
    }
}
