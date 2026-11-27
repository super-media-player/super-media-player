//! User settings, stored as `key = value` lines in
//! `$XDG_CONFIG_HOME/super-media-player/settings.conf`.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// One of `VIEW_MODES`: the full media player or a small skinned player.
    pub view_mode: String,
    /// The skin dressing that view: a skin id (skins.rs), or "" for the
    /// view's own colours.
    pub skin: String,
    /// The colour theme of the media player and compact views: a theme id
    /// (skins.rs, `layout = any`), or "" for none.
    pub theme: String,
    /// Music visual: one of `VISUALIZERS`.
    pub visualizer: String,
    /// Artist picture corner: "off", "top-left", "top-right".
    pub artist_image: String,
    /// Look up artist pictures, album covers and artist summaries online
    /// (sends artist and album names to Deezer and Wikipedia).
    pub artist_image_online: bool,
    /// The heartbeat-like line that scrolls across the music view.
    pub pulse_line: bool,
    /// Look up lyrics online (sends artist and title to LRCLIB).
    pub lyrics_online: bool,
    /// Show timed lyrics in the music view.
    pub show_lyrics: bool,
    /// Blu-ray decryption library: "auto", "libaacs", "makemkv". Applies at startup.
    pub bluray_decryption: String,
    /// Streaming quality: "best", "2160", "1080", "720", "480", "audio".
    pub stream_quality: String,
    pub hardware_decoding: bool,
    /// Linux: "auto" (X11 when available, for drag and drop), "x11", "wayland". Applies at startup.
    pub window_system: String,
    pub volume: f64,
    /// Keep yt-dlp and the Blu-ray key database up to date in the background.
    pub auto_update: bool,
    /// The classic deck's playlist sits in its own window.
    pub playlist_detached: bool,
    /// The classic deck shows its playlist (on at first, then as last left).
    pub classic_playlist: bool,
    /// Tone (silver deck knobs), -12..12 dB.
    pub bass: f64,
    pub treble: f64,
    /// No gap between songs of the same format (mpv `gapless-audio`).
    pub gapless: bool,
    /// Fade out the end of a song and fade in the next, seconds (0 = off).
    pub crossfade: f64,
    /// ReplayGain loudness levelling: one of `REPLAYGAIN`.
    pub replaygain: String,
    /// The 10-band equalizer is on, and its gains (-12..12 dB) for
    /// `EQ_BANDS`.
    pub eq_enabled: bool,
    pub eq_gains: [f64; 10],
    /// Pick up long videos (and long audio: podcasts, audiobooks) where
    /// they were left (resume.rs).
    pub resume: bool,
    /// Bring back the speed (and skipping silence) a file was played with
    /// (pace.rs).
    pub remember_pace: bool,
    /// Bit-perfect output: the sound card gets the file's samples untouched
    /// and to itself (no tone, equalizer, levels or fades).
    pub bit_perfect: bool,
    /// Night mode: compress loud and quiet parts (late-night films).
    pub night_mode: bool,
    /// Dialogue boost: voices over music and effects.
    pub dialogue_boost: bool,
    /// Preview pictures over the seek bar (a second, hidden decoder).
    pub seek_thumbnails: bool,
    /// Intros and credits of series episodes: one of `SKIP_INTRO`.
    pub skip_intro: String,
    /// Sponsors in YouTube videos (SponsorBlock): one of `SKIP_INTRO`, and
    /// what counts: one of `SPONSOR_KINDS`.
    pub sponsor_skip: String,
    pub sponsor_kinds: String,
    /// When a single episode was opened, queue the next one from its folder.
    pub next_episode: bool,
    /// Video enhancement shader: one of `ENHANCE`.
    pub video_enhance: String,
    /// Smoother motion: frames are blended to the screen's refresh rate.
    pub smooth_motion: bool,
    /// Leave out black bars that are part of the video (autocrop.rs).
    pub auto_crop: bool,
    /// How HDR video is fitted to an SDR screen: one of `TONE_MAPS`.
    pub hdr_tone_map: String,
    /// Measure each scene's brightness for HDR tone-mapping.
    pub hdr_peak_detect: bool,
    /// Look audio CDs up on MusicBrainz for song names and the cover.
    pub cd_online: bool,
    /// Speech recognition model for generated subtitles: one of `WHISPER_MODELS`.
    pub whisper_model: String,
    /// The MilkDrop visualizer changes preset every this many seconds.
    pub milkdrop_seconds: u32,
    /// Advanced navigation (navigation.rs) is on, and docked under the
    /// control bar rather than in its own window.
    pub nav_enabled: bool,
    pub nav_docked: bool,
    /// Its step, in milliseconds (-1: one frame), and the media time one
    /// turn of its jog wheel covers, in seconds.
    pub nav_step_ms: f64,
    pub nav_turn: f64,
    /// Keep the pitch at other speeds while it is on.
    pub nav_pitch: bool,
    /// Hear the sound while turning the wheel forwards / on each step.
    pub nav_jog_sound: bool,
    pub nav_step_sound: bool,
    /// mpv audio output device name ("auto": the system's choice).
    pub audio_device: String,
    /// Keep the window above others.
    pub always_on_top: bool,
    /// The media player's title bar: one of `TITLE_BARS`.
    pub title_bar: String,
    /// Text in the media player and the panels: one of `TEXT_SIZES`.
    pub text_size: String,
    /// Files opened while the player runs go to it (instance.rs).
    pub single_instance: bool,
    /// Text subtitles: one of `SUB_SIZES`, `SUB_POSITIONS`, `SUB_COLORS`,
    /// `SUB_STYLES`.
    pub sub_size: String,
    pub sub_pos: String,
    pub sub_color: String,
    pub sub_style: String,
    /// Compressed surround sent untouched to a receiver: one of `PASSTHROUGH`.
    pub passthrough: String,
    /// DVDs start with their own menu (dvdmenu.rs) instead of the main film.
    pub dvd_menus: bool,
    /// Mouse over the picture: one of `MOUSE_WHEEL`, `MOUSE_DOUBLE`,
    /// `MOUSE_MIDDLE`.
    pub mouse_wheel: String,
    pub mouse_double: String,
    pub mouse_middle: String,
    /// How files are cast: one of `CAST_QUALITY`.
    pub cast_quality: String,
    /// Draw subtitles into the picture that is cast.
    pub cast_burn_subs: bool,
    /// The address last typed for a device the search didn't find.
    pub cast_address: String,
    /// Subtitle languages to search for online, "en,ro".
    pub sub_languages: String,
    /// Line up downloaded subtitles with the speech (subsync.rs).
    pub sub_auto_sync: bool,
    /// Run the user's mpv scripts (scripts.rs), and the files of those that
    /// are switched off, with a / between them (no file name has one).
    pub scripts: bool,
    pub scripts_off: String,
    /// Country code for the radio directory ("MD"); "" = from the locale.
    pub radio_country: String,
}

/// How files are cast: converted where needed, always as they are, always
/// converted.
pub const CAST_QUALITY: &[&str] = &["auto", "original", "convert"];

/// "button": show a Skip button; "auto": skip by itself; "off".
pub const SKIP_INTRO: &[&str] = &["button", "auto", "off"];
/// Sponsors only; also self-promotion and "subscribe" reminders; also
/// intros, endings, previews and parts of music videos without music.
pub const SPONSOR_KINDS: &[&str] = &["sponsors", "promotion", "all"];
/// Video enhancement: off, sharpen (CAS), AMD FSR upscaling, Anime4K.
pub const ENHANCE: &[&str] = &["off", "sharpen", "fsr", "anime4k"];
/// HDR tone-mapping curves (mpv's `tone-mapping` names); "auto" is mpv's pick.
pub const TONE_MAPS: &[&str] = &["auto", "bt.2390", "spline", "hable", "mobius", "clip"];
pub const SUB_SIZES: &[&str] = &["small", "normal", "large", "huge"];
/// Text size names, and how much bigger than normal each is.
pub const TEXT_SIZES: &[&str] = &["small", "normal", "large", "larger"];
pub const TEXT_SCALES: &[f32] = &[0.9, 1.0, 1.15, 1.3];
pub const SUB_POSITIONS: &[&str] = &["bottom", "higher", "high", "top"];
pub const SUB_COLORS: &[&str] = &["white", "yellow", "grey", "cyan", "green"];
pub const SUB_STYLES: &[&str] = &["outline", "shadow", "box", "lightbox"];
/// Off; AC3 and DTS (S/PDIF); everything an HDMI receiver takes.
pub const PASSTHROUGH: &[&str] = &["off", "spdif", "hdmi"];
pub const MOUSE_WHEEL: &[&str] = &["volume", "seek", "off"];
pub const MOUSE_DOUBLE: &[&str] = &["fullscreen", "pause", "off"];
pub const MOUSE_MIDDLE: &[&str] = &["mute", "pause", "fullscreen", "next", "off"];
pub const WHISPER_MODELS: &[&str] = &["tiny", "base", "small", "medium", "large-v3-turbo"];

/// Equalizer band centres, Hz.
pub const EQ_BANDS: [f64; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
pub const REPLAYGAIN: &[&str] = &["off", "track", "album"];
pub const CROSSFADES: &[f64] = &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0];
/// Seconds of media in one turn of the navigation jog wheel.
pub const NAV_TURNS: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0];

impl Default for Settings {
    fn default() -> Self {
        Self {
            view_mode: "player".into(),
            skin: String::new(),
            theme: String::new(),
            visualizer: "disc".into(),
            artist_image: "top-right".into(),
            artist_image_online: false,
            pulse_line: true,
            lyrics_online: false,
            show_lyrics: true,
            bluray_decryption: "auto".into(),
            stream_quality: "1080".into(),
            hardware_decoding: true,
            window_system: "auto".into(),
            volume: 100.0,
            auto_update: true,
            playlist_detached: false,
            classic_playlist: true,
            bass: 0.0,
            treble: 0.0,
            gapless: true,
            crossfade: 0.0,
            replaygain: "off".into(),
            eq_enabled: false,
            eq_gains: [0.0; 10],
            resume: true,
            remember_pace: true,
            bit_perfect: false,
            night_mode: false,
            dialogue_boost: false,
            seek_thumbnails: true,
            skip_intro: "button".into(),
            sponsor_skip: "button".into(),
            sponsor_kinds: "promotion".into(),
            next_episode: true,
            video_enhance: "off".into(),
            smooth_motion: false,
            auto_crop: false,
            hdr_tone_map: "auto".into(),
            hdr_peak_detect: true,
            cast_quality: CAST_QUALITY[0].into(),
            cast_burn_subs: false,
            cast_address: String::new(),
            cd_online: true,
            whisper_model: "base".into(),
            milkdrop_seconds: 20,
            nav_enabled: false,
            nav_docked: true,
            nav_step_ms: 100.0,
            nav_turn: 2.0,
            nav_pitch: true,
            nav_jog_sound: true,
            nav_step_sound: false,
            audio_device: "auto".into(),
            always_on_top: false,
            title_bar: "player".into(),
            text_size: "normal".into(),
            single_instance: true,
            sub_size: "normal".into(),
            sub_pos: "bottom".into(),
            sub_color: "white".into(),
            sub_style: "shadow".into(),
            passthrough: "off".into(),
            dvd_menus: true,
            mouse_wheel: "volume".into(),
            mouse_double: "fullscreen".into(),
            mouse_middle: "mute".into(),
            sub_languages: "en".into(),
            sub_auto_sync: true,
            scripts: true,
            scripts_off: String::new(),
            radio_country: String::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let Some(text) = config_file().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            let choice = |allowed: &[&str], field: &mut String| {
                if allowed.contains(&v) {
                    *field = v.to_owned();
                }
            };
            match k {
                "view_mode" => choice(VIEW_MODES, &mut s.view_mode),
                // A file name: letters, digits, - _ . and spaces only.
                "theme" if v.chars().all(|c| c.is_alphanumeric() || "-_. ".contains(c)) => {
                    s.theme = v.to_owned();
                }
                "skin" if v.chars().all(|c| c.is_alphanumeric() || "-_. ".contains(c)) => {
                    s.skin = v.to_owned();
                }
                "visualizer" => choice(VISUALIZERS, &mut s.visualizer),
                "artist_image" => choice(&["off", "top-left", "top-right"], &mut s.artist_image),
                "artist_image_online" => s.artist_image_online = v == "true",
                "pulse_line" => s.pulse_line = v != "false",
                "lyrics_online" => s.lyrics_online = v == "true",
                "show_lyrics" => s.show_lyrics = v != "false",
                "auto_update" => s.auto_update = v != "false",
                "playlist_detached" => s.playlist_detached = v == "true",
                "classic_playlist" => s.classic_playlist = v != "false",
                "bass" | "treble" => {
                    if let Ok(x) = v.parse::<f64>() {
                        let x = x.clamp(-12.0, 12.0);
                        if k == "bass" {
                            s.bass = x;
                        } else {
                            s.treble = x;
                        }
                    }
                }
                "gapless" => s.gapless = v != "false",
                "crossfade" => {
                    if let Ok(x) = v.parse::<f64>() {
                        s.crossfade = x.clamp(0.0, 12.0);
                    }
                }
                "replaygain" => choice(REPLAYGAIN, &mut s.replaygain),
                "eq_enabled" => s.eq_enabled = v == "true",
                "eq_gains" => {
                    let g: Vec<f64> = v
                        .split(',')
                        .filter_map(|x| x.trim().parse::<f64>().ok())
                        .collect();
                    if g.len() == 10 {
                        for (dst, x) in s.eq_gains.iter_mut().zip(g) {
                            *dst = x.clamp(-12.0, 12.0);
                        }
                    }
                }
                "resume" => s.resume = v != "false",
                "remember_pace" => s.remember_pace = v != "false",
                "bit_perfect" => s.bit_perfect = v == "true",
                "night_mode" => s.night_mode = v == "true",
                "dialogue_boost" => s.dialogue_boost = v == "true",
                "seek_thumbnails" => s.seek_thumbnails = v != "false",
                "skip_intro" => choice(SKIP_INTRO, &mut s.skip_intro),
                "sponsor_skip" => choice(SKIP_INTRO, &mut s.sponsor_skip),
                "sponsor_kinds" => choice(SPONSOR_KINDS, &mut s.sponsor_kinds),
                "next_episode" => s.next_episode = v != "false",
                "video_enhance" => choice(ENHANCE, &mut s.video_enhance),
                "smooth_motion" => s.smooth_motion = v == "true",
                "auto_crop" => s.auto_crop = v == "true",
                "hdr_tone_map" => choice(TONE_MAPS, &mut s.hdr_tone_map),
                "hdr_peak_detect" => s.hdr_peak_detect = v != "false",
                "cast_quality" => choice(CAST_QUALITY, &mut s.cast_quality),
                "cast_burn_subs" => s.cast_burn_subs = v == "true",
                "cast_address" => {
                    s.cast_address = v.chars().filter(|c| !c.is_control()).take(200).collect()
                }
                "cd_online" => s.cd_online = v != "false",
                "whisper_model" => choice(WHISPER_MODELS, &mut s.whisper_model),
                "milkdrop_seconds" => {
                    if let Ok(x) = v.parse::<u32>() {
                        s.milkdrop_seconds = x.clamp(5, 600);
                    }
                }
                "nav_enabled" => s.nav_enabled = v == "true",
                "nav_docked" => s.nav_docked = v != "false",
                "nav_step_ms" => {
                    if let Ok(x) = v.parse::<f64>()
                        && (x == -1.0 || (x > 0.0 && x <= 3_600_000.0))
                    {
                        s.nav_step_ms = x;
                    }
                }
                "nav_turn" => {
                    if let Ok(x) = v.parse::<f64>()
                        && NAV_TURNS.contains(&x)
                    {
                        s.nav_turn = x;
                    }
                }
                "nav_pitch" => s.nav_pitch = v != "false",
                "nav_jog_sound" => s.nav_jog_sound = v != "false",
                "nav_step_sound" => s.nav_step_sound = v == "true",
                // A device name from mpv: no line breaks, nothing odd.
                "audio_device" if !v.is_empty() && v.len() < 300 && !v.contains(['\n', '\r']) => {
                    s.audio_device = v.to_owned();
                }
                "always_on_top" => s.always_on_top = v == "true",
                "title_bar" => choice(TITLE_BARS, &mut s.title_bar),
                "text_size" => choice(TEXT_SIZES, &mut s.text_size),
                "single_instance" => s.single_instance = v != "false",
                "sub_size" => choice(SUB_SIZES, &mut s.sub_size),
                "sub_pos" => choice(SUB_POSITIONS, &mut s.sub_pos),
                "sub_color" => choice(SUB_COLORS, &mut s.sub_color),
                "sub_style" => choice(SUB_STYLES, &mut s.sub_style),
                "passthrough" => choice(PASSTHROUGH, &mut s.passthrough),
                "sub_auto_sync" => s.sub_auto_sync = v != "false",
                "scripts" => s.scripts = v != "false",
                "scripts_off" => {
                    s.scripts_off = v.chars().filter(|c| !c.is_control()).take(8000).collect()
                }
                "dvd_menus" => s.dvd_menus = v != "false",
                "mouse_wheel" => choice(MOUSE_WHEEL, &mut s.mouse_wheel),
                "mouse_double" => choice(MOUSE_DOUBLE, &mut s.mouse_double),
                "mouse_middle" => choice(MOUSE_MIDDLE, &mut s.mouse_middle),
                "radio_country" if v.len() == 2 && v.chars().all(|c| c.is_ascii_alphabetic()) => {
                    s.radio_country = v.to_uppercase();
                }
                "sub_languages"
                    if v.len() < 100
                        && v.chars()
                            .all(|c| c.is_ascii_alphabetic() || ",- ".contains(c)) =>
                {
                    s.sub_languages = v.to_owned();
                }
                "bluray_decryption" => {
                    choice(&["auto", "libaacs", "makemkv"], &mut s.bluray_decryption)
                }
                "stream_quality" => choice(QUALITIES, &mut s.stream_quality),
                "hardware_decoding" => s.hardware_decoding = v != "false",
                "window_system" => choice(&["auto", "x11", "wayland"], &mut s.window_system),
                "volume" => {
                    if let Ok(x) = v.parse::<f64>() {
                        s.volume = x.clamp(0.0, 150.0);
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        format!(
            "# Super Media Player settings\n\
             view_mode = {}\nvisualizer = {}\nartist_image = {}\nartist_image_online = {}\npulse_line = {}\nlyrics_online = {}\nshow_lyrics = {}\n\
             bluray_decryption = {}\nstream_quality = {}\nhardware_decoding = {}\n\
             window_system = {}\nvolume = {}\nauto_update = {}\nplaylist_detached = {}\nclassic_playlist = {}\nbass = {}\ntreble = {}\n",
            self.view_mode,
            self.visualizer,
            self.artist_image,
            self.artist_image_online,
            self.pulse_line,
            self.lyrics_online,
            self.show_lyrics,
            self.bluray_decryption,
            self.stream_quality,
            self.hardware_decoding,
            self.window_system,
            self.volume.round(),
            self.auto_update,
            self.playlist_detached,
            self.classic_playlist,
            self.bass.round(),
            self.treble.round(),
        ) + &format!(
            "gapless = {}\ncrossfade = {}\nreplaygain = {}\neq_enabled = {}\neq_gains = {}\n\
             resume = {}\nremember_pace = {}\nbit_perfect = {}\nskin = {}\n",
            self.gapless,
            self.crossfade,
            self.replaygain,
            self.eq_enabled,
            self.eq_gains
                .iter()
                .map(|g| format!("{g:.1}"))
                .collect::<Vec<_>>()
                .join(","),
            self.resume,
            self.remember_pace,
            self.bit_perfect,
            self.skin,
        ) + &format!(
            "night_mode = {}\ndialogue_boost = {}\nseek_thumbnails = {}\nskip_intro = {}\n\
             next_episode = {}\nvideo_enhance = {}\nsmooth_motion = {}\nauto_crop = {}\nwhisper_model = {}\n\
             milkdrop_seconds = {}\nhdr_tone_map = {}\nhdr_peak_detect = {}\ncd_online = {}\n\
             cast_quality = {}\ncast_burn_subs = {}\ncast_address = {}\n",
            self.night_mode,
            self.dialogue_boost,
            self.seek_thumbnails,
            self.skip_intro,
            self.next_episode,
            self.video_enhance,
            self.smooth_motion,
            self.auto_crop,
            self.whisper_model,
            self.milkdrop_seconds,
            self.hdr_tone_map,
            self.hdr_peak_detect,
            self.cd_online,
            self.cast_quality,
            self.cast_burn_subs,
            self.cast_address,
        ) + &format!(
            "nav_enabled = {}\nnav_docked = {}\nnav_step_ms = {}\nnav_turn = {}\nnav_pitch = {}\n\
             nav_jog_sound = {}\nnav_step_sound = {}\n",
            self.nav_enabled,
            self.nav_docked,
            self.nav_step_ms,
            self.nav_turn,
            self.nav_pitch,
            self.nav_jog_sound,
            self.nav_step_sound,
        ) + &format!(
            "audio_device = {}\nalways_on_top = {}\nsingle_instance = {}\nsub_size = {}\nsub_pos = {}\n\
             sub_color = {}\nsub_style = {}\npassthrough = {}\ndvd_menus = {}\n\
             mouse_wheel = {}\nmouse_double = {}\nmouse_middle = {}\nsub_languages = {}\nradio_country = {}\nsub_auto_sync = {}\n\
             sponsor_skip = {}\nsponsor_kinds = {}\ntitle_bar = {}\nscripts = {}\nscripts_off = {}\ntext_size = {}\ntheme = {}\n",
            self.audio_device,
            self.always_on_top,
            self.single_instance,
            self.sub_size,
            self.sub_pos,
            self.sub_color,
            self.sub_style,
            self.passthrough,
            self.dvd_menus,
            self.mouse_wheel,
            self.mouse_double,
            self.mouse_middle,
            self.sub_languages,
            self.radio_country,
            self.sub_auto_sync,
            self.sponsor_skip,
            self.sponsor_kinds,
            self.title_bar,
            self.scripts,
            self.scripts_off,
            self.text_size,
            self.theme,
        )
    }

    pub fn save(&self) {
        let Some(path) = config_file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Write then rename, so a crash never leaves a half-written file.
        let tmp = path.with_extension("conf.tmp");
        if let Err(e) =
            std::fs::write(&tmp, self.to_text()).and_then(|()| std::fs::rename(&tmp, &path))
        {
            log::warn!("could not save settings to {}: {e}", path.display());
        }
    }

    /// mpv `ytdl-format` for the chosen streaming quality.
    pub fn ytdl_format(&self) -> String {
        match self.stream_quality.as_str() {
            "audio" => "bestaudio/best".into(),
            "best" => "bestvideo+bestaudio/best".into(),
            h => format!("bestvideo[height<=?{h}]+bestaudio/best[height<=?{h}]/best"),
        }
    }
}

/// Player views, in the order the UI lists them: full player, compact,
/// classic deck, silver deck.
pub const TITLE_BARS: &[&str] = &["player", "system"];
pub const VIEW_MODES: &[&str] = &["player", "compact", "classic", "silver"];

pub const VISUALIZERS: &[&str] = &[
    "disc",
    "particles",
    "spectrum",
    "waves",
    "scope",
    "cqt",
    "spectrogram",
    "milkdrop",
    "none",
];
pub const QUALITIES: &[&str] = &["best", "2160", "1080", "720", "480", "audio"];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(fallback)))
        .map(|p| p.join("super-media-player"))
}

pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("super-media-player"));
    }
    xdg("XDG_CONFIG_HOME", ".config")
}

/// Where downloaded helper programs (yt-dlp) live.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("super-media-player"));
    }
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return data_dir().map(|d| d.join("cache"));
    }
    xdg("XDG_CACHE_HOME", ".cache")
}

/// The user's libaacs key database, `~/.config/aacs/KEYDB.cfg`.
pub fn aacs_keydb() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("aacs").join("KEYDB.cfg"));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".config")))
        .map(|p| p.join("aacs").join("KEYDB.cfg"))
}

fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_values() {
        let s = Settings {
            view_mode: "compact".into(),
            skin: "ember".into(),
            theme: "ocean".into(),
            text_size: "larger".into(),
            visualizer: "cqt".into(),
            artist_image_online: true,
            volume: 77.0,
            auto_update: false,
            classic_playlist: false,
            crossfade: 6.0,
            replaygain: "album".into(),
            eq_enabled: true,
            eq_gains: [4.0, 3.5, 2.0, 0.0, -1.5, -1.0, 0.0, 1.5, 3.0, 4.0],
            remember_pace: false,
            bit_perfect: true,
            night_mode: true,
            dialogue_boost: true,
            seek_thumbnails: false,
            skip_intro: "auto".into(),
            sponsor_skip: "auto".into(),
            sponsor_kinds: "all".into(),
            next_episode: false,
            video_enhance: "fsr".into(),
            smooth_motion: true,
            auto_crop: true,
            hdr_tone_map: "spline".into(),
            hdr_peak_detect: false,
            cd_online: false,
            cast_quality: "convert".into(),
            cast_burn_subs: true,
            cast_address: "192.168.1.30:8009".into(),
            whisper_model: "small".into(),
            milkdrop_seconds: 45,
            nav_enabled: true,
            nav_docked: false,
            nav_step_ms: 41.708,
            nav_turn: 0.5,
            nav_pitch: false,
            nav_jog_sound: false,
            nav_step_sound: true,
            scripts: false,
            scripts_off: "one = two.lua/folder name".into(),
            ..Default::default()
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
        assert_eq!(Settings::parse("eq_gains = 1,2,3").eq_gains, [0.0; 10]);
        // Unknown keys and values are ignored, not trusted.
        let p = Settings::parse("view_mode = evil\nvolume = 9999\nfoo = bar\nvisualizer=waves");
        assert_eq!(p.view_mode, "player");
        assert_eq!(p.volume, 150.0);
        assert_eq!(p.visualizer, "waves");
    }

    #[test]
    fn stream_quality_formats() {
        let mut s = Settings {
            stream_quality: "720".into(),
            ..Default::default()
        };
        assert!(s.ytdl_format().contains("height<=?720"));
        s.stream_quality = "audio".into();
        assert_eq!(s.ytdl_format(), "bestaudio/best");
    }
}
